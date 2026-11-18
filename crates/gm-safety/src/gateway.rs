use crate::blocklist::BlocklistManager;
use crate::tiers::ActionTier;
use gm_core::muse_paths::{MountAccess, ResolvedMusePath};
use gm_core::types::ComputerAction;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyOutcome {
    Permitted,
    NeedsApproval,
    Refused,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyDecision {
    pub outcome: PolicyOutcome,
    pub tier: ActionTier,
    pub rule_id: &'static str,
    pub citation: String,
}

/// Exact-action approval grant bound to action digest, arguments, workspace, destination, principal, job, and expiry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionApprovalGrant {
    pub grant_id: String,
    pub action_name: String,
    pub action_digest: String,
    pub workspace_id: String,
    pub destination: String,
    pub principal: String,
    pub job_id: String,
    pub irreversible_summary: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Copy)]
pub struct ActionExecutionContext<'a> {
    pub action_name: &'a str,
    pub canonical_payload: &'a str,
    pub workspace_id: &'a str,
    pub destination: &'a str,
    pub principal: &'a str,
    pub job_id: &'a str,
    pub now: chrono::DateTime<chrono::Utc>,
}

impl ActionApprovalGrant {
    pub fn compute_action_digest(action_name: &str, canonical_payload: &str) -> String {
        let material = format!("{action_name}\n{canonical_payload}");
        gm_core::hash::compute_sha256(material.as_bytes())
    }

    /// Re-verify grant immediately before execution; changed arguments/recipients/patches/tenants/expiry fail closed.
    pub fn verify_before_execution(
        &self,
        ctx: &ActionExecutionContext<'_>,
    ) -> std::result::Result<(), String> {
        if ctx.now >= self.expires_at {
            return Err("Approval grant has expired".into());
        }
        if self.action_name != ctx.action_name {
            return Err("Approval grant action mismatch".into());
        }
        let actual_digest = Self::compute_action_digest(ctx.action_name, ctx.canonical_payload);
        if self.action_digest != actual_digest {
            return Err(
                "Approval grant payload/patch digest mismatch; fresh approval required".into(),
            );
        }
        if self.workspace_id != ctx.workspace_id {
            return Err("Approval grant workspace mismatch".into());
        }
        if self.destination != ctx.destination {
            return Err(
                "Approval grant recipient/destination changed; fresh approval required".into(),
            );
        }
        if self.principal != ctx.principal {
            return Err("Approval grant principal mismatch".into());
        }
        if self.job_id != ctx.job_id {
            return Err("Approval grant job mismatch".into());
        }
        Ok(())
    }
}

/// Central pre-execution security gateway (OpenBot Gateway Security Pattern).
/// Every GUI action, workspace file access, shell command, and secret lease is evaluated
/// before execution and stamped with an explicit governance rule citation.
#[derive(Debug, Clone)]
pub struct SecurityGateway {
    blocklist: BlocklistManager,
}

impl SecurityGateway {
    pub fn new(blocklist: BlocklistManager) -> Self {
        Self { blocklist }
    }

    /// Evaluate a GUI / ComputerAction against blocklist and tier policies.
    pub fn evaluate_gui_action(
        &self,
        app_name: Option<&str>,
        action: &ComputerAction,
        target_label: Option<&str>,
    ) -> PolicyDecision {
        if let Some(app) = app_name
            && self.blocklist.is_blocked(app, target_label.unwrap_or(""))
        {
            return PolicyDecision {
                outcome: PolicyOutcome::Refused,
                tier: ActionTier::Irreversible,
                rule_id: "RULE-BLK-001",
                citation: format!(
                    "Target application '{app}' is in SafetyConfig.blocklisted_apps and cannot be accessed"
                ),
            };
        }

        match action {
            ComputerAction::FocusApp { app_name } if self.blocklist.is_blocked(app_name, "") => {
                return PolicyDecision {
                    outcome: PolicyOutcome::Refused,
                    tier: ActionTier::Irreversible,
                    rule_id: "RULE-BLK-001",
                    citation: format!(
                        "Focusing blocked application '{app_name}' is prohibited by RULE-BLK-001"
                    ),
                };
            }
            ComputerAction::LaunchApp { bundle_id } if self.blocklist.is_blocked(bundle_id, "") => {
                return PolicyDecision {
                    outcome: PolicyOutcome::Refused,
                    tier: ActionTier::Irreversible,
                    rule_id: "RULE-BLK-001",
                    citation: format!(
                        "Launching blocked bundle '{bundle_id}' is prohibited by RULE-BLK-001"
                    ),
                };
            }
            _ => {}
        }

        let tier = ActionTier::classify(action, target_label);
        if tier.requires_confirmation() {
            PolicyDecision {
                outcome: PolicyOutcome::NeedsApproval,
                tier,
                rule_id: "RULE-GUI-005",
                citation: format!(
                    "Destructive or external GUI action ({:?} on {:?}) requires explicit operator confirmation",
                    action, target_label
                ),
            }
        } else {
            PolicyDecision {
                outcome: PolicyOutcome::Permitted,
                tier,
                rule_id: "RULE-STD-000",
                citation: "Standard read or reversible GUI action permitted".into(),
            }
        }
    }

    /// Evaluate a workspace file path access against bot root confinement.
    pub fn evaluate_workspace_path(
        &self,
        bot_root: &Path,
        candidate_path: &Path,
        is_write: bool,
    ) -> PolicyDecision {
        let canonical_root = bot_root
            .canonicalize()
            .unwrap_or_else(|_| bot_root.to_path_buf());

        // Check lexical traversal (`..`) or absolute escape outside bot_root
        let mut normalized = std::path::PathBuf::new();
        let joined = if candidate_path.is_absolute() {
            candidate_path.to_path_buf()
        } else {
            canonical_root.join(candidate_path)
        };

        for comp in joined.components() {
            match comp {
                std::path::Component::ParentDir => {
                    normalized.pop();
                }
                std::path::Component::CurDir => {}
                other => normalized.push(other.as_os_str()),
            }
        }

        // Also check real canonical path if parent exists
        let effective = if normalized.exists() {
            normalized.canonicalize().unwrap_or(normalized)
        } else if let Some(parent) = normalized.parent()
            && parent.exists()
        {
            if let Ok(canon_parent) = parent.canonicalize() {
                if let Some(name) = normalized.file_name() {
                    canon_parent.join(name)
                } else {
                    canon_parent
                }
            } else {
                normalized
            }
        } else {
            normalized
        };

        if !effective.starts_with(&canonical_root) {
            return PolicyDecision {
                outcome: PolicyOutcome::Refused,
                tier: ActionTier::Irreversible,
                rule_id: "RULE-PATH-002",
                citation: format!(
                    "Path '{}' escapes confined bot root '{}'",
                    candidate_path.display(),
                    canonical_root.display()
                ),
            };
        }

        PolicyDecision {
            outcome: PolicyOutcome::Permitted,
            tier: if is_write {
                ActionTier::Reversible
            } else {
                ActionTier::Read
            },
            rule_id: "RULE-STD-000",
            citation: "Confined bot workspace file operation permitted".into(),
        }
    }

    /// Evaluate a resolved Muse virtual runtime path against mount confinement, symlink
    /// restrictions, read-only mounts, and persona write approval gates.
    pub fn evaluate_scoped_path(
        &self,
        resolved: &ResolvedMusePath,
        is_write: bool,
        persona_write_approved: bool,
    ) -> PolicyDecision {
        // 1. Canonical deepest existing ancestor must stay within canonical mount root.
        let canon_root = canonicalize_with_tail(&resolved.mount_root);
        let normalized_abs = normalize_lexical(&resolved.absolute);
        let effective = canonicalize_with_tail(&normalized_abs);
        if !effective.starts_with(&canon_root) {
            return PolicyDecision {
                outcome: PolicyOutcome::Refused,
                tier: ActionTier::Irreversible,
                rule_id: "RULE-PATH-002",
                citation: format!(
                    "Path '{}' escapes scoped mount '{}'",
                    resolved.absolute.display(),
                    resolved.mount_label
                ),
            };
        }

        // 2. Any symlink component from mount_root through relative path -> refuse RULE-PATH-002.
        if let Ok(meta) = std::fs::symlink_metadata(&resolved.mount_root)
            && meta.file_type().is_symlink()
        {
            return PolicyDecision {
                outcome: PolicyOutcome::Refused,
                tier: ActionTier::Irreversible,
                rule_id: "RULE-PATH-002",
                citation: format!(
                    "Mount root '{}' is a symlink, which is prohibited by RULE-PATH-002",
                    resolved.mount_label
                ),
            };
        }
        let mut cursor = resolved.mount_root.clone();
        for comp in resolved.relative.components() {
            match comp {
                Component::CurDir => continue,
                Component::Normal(seg) => {
                    cursor.push(seg);
                    if let Ok(meta) = std::fs::symlink_metadata(&cursor)
                        && meta.file_type().is_symlink()
                    {
                        return PolicyDecision {
                            outcome: PolicyOutcome::Refused,
                            tier: ActionTier::Irreversible,
                            rule_id: "RULE-PATH-002",
                            citation: format!(
                                "Symlink component '{}' inside mount '{}' is prohibited by RULE-PATH-002",
                                cursor.display(),
                                resolved.mount_label
                            ),
                        };
                    }
                }
                other => {
                    return PolicyDecision {
                        outcome: PolicyOutcome::Refused,
                        tier: ActionTier::Irreversible,
                        rule_id: "RULE-PATH-002",
                        citation: format!(
                            "Invalid relative path component {:?} in mount '{}'",
                            other, resolved.mount_label
                        ),
                    };
                }
            }
        }

        // 3. Writes to ReadOnly mounts are refused.
        if is_write && resolved.access == MountAccess::ReadOnly {
            return PolicyDecision {
                outcome: PolicyOutcome::Refused,
                tier: ActionTier::Irreversible,
                rule_id: "RULE-PATH-002",
                citation: format!(
                    "RULE-PATH-002: Cannot modify read-only mount '{}'",
                    resolved.mount_label
                ),
            };
        }

        // 4. Writes to PersonaApproval mounts require explicit Irreversible approval.
        if is_write && resolved.access == MountAccess::PersonaApproval && !persona_write_approved {
            return PolicyDecision {
                outcome: PolicyOutcome::NeedsApproval,
                tier: ActionTier::Irreversible,
                rule_id: "RULE-PERSONA-007",
                citation: format!(
                    "Modifying persona file '{}' requires explicit Irreversible confirmation",
                    resolved.mount_label
                ),
            };
        }

        // 5. Permitted otherwise.
        PolicyDecision {
            outcome: PolicyOutcome::Permitted,
            tier: if !is_write {
                ActionTier::Read
            } else if resolved.access == MountAccess::PersonaApproval {
                ActionTier::Irreversible
            } else {
                ActionTier::Reversible
            },
            rule_id: "RULE-STD-000",
            citation: format!(
                "Scoped mount '{}' file operation permitted",
                resolved.mount_label
            ),
        }
    }

    /// Evaluate a workspace shell command before execution.
    pub fn evaluate_shell_command(&self, command: &str) -> PolicyDecision {
        let lower = command.to_lowercase();

        // Hard refusal on privilege escalation, keychain dumping, or sandbox bypass
        let forbidden = [
            "sudo ",
            "sudo\t",
            "su -",
            "security dump-keychain",
            "security find-generic-password",
            "launchctl ",
            "csrutil ",
            "dscl ",
        ];
        for pat in forbidden {
            if lower.contains(pat) || lower == "sudo" {
                return PolicyDecision {
                    outcome: PolicyOutcome::Refused,
                    tier: ActionTier::Irreversible,
                    rule_id: "RULE-SH-003",
                    citation: format!(
                        "Privilege escalation or credential-dumping pattern '{pat}' is refused by RULE-SH-003"
                    ),
                };
            }
        }

        // Mutating / destructive / outbound push patterns require interactive confirmation
        let needs_approval = [
            "rm -rf",
            "rm -r ",
            "git push",
            "curl -x post",
            "curl -d ",
            "curl --data",
            "dd if=",
            "mkfs",
            "kill -9",
            "pkill ",
        ];
        for pat in needs_approval {
            if lower.contains(pat) {
                return PolicyDecision {
                    outcome: PolicyOutcome::NeedsApproval,
                    tier: ActionTier::Irreversible,
                    rule_id: "RULE-SH-004",
                    citation: format!(
                        "Potentially destructive or external shell command matching '{pat}' requires confirmation"
                    ),
                };
            }
        }

        PolicyDecision {
            outcome: PolicyOutcome::Permitted,
            tier: ActionTier::Reversible,
            rule_id: "RULE-STD-000",
            citation: "Seatbelt-confined workspace shell command permitted".into(),
        }
    }

    /// Evaluate an opaque vault secret lease (`secret://...`).
    pub fn evaluate_secret_lease(
        &self,
        secret_ref: &str,
        approved_in_session: bool,
    ) -> PolicyDecision {
        if !secret_ref.starts_with("secret://keychain/")
            && !secret_ref.starts_with("secret://1password/")
        {
            return PolicyDecision {
                outcome: PolicyOutcome::Refused,
                tier: ActionTier::Irreversible,
                rule_id: "RULE-SEC-006",
                citation: format!(
                    "Invalid or unsupported secret URI '{secret_ref}' refused by RULE-SEC-006"
                ),
            };
        }

        if approved_in_session {
            PolicyDecision {
                outcome: PolicyOutcome::Permitted,
                tier: ActionTier::Reversible,
                rule_id: "RULE-SEC-006",
                citation: format!("Opaque secret lease '{secret_ref}' permitted within session"),
            }
        } else {
            PolicyDecision {
                outcome: PolicyOutcome::NeedsApproval,
                tier: ActionTier::Irreversible,
                rule_id: "RULE-SEC-006",
                citation: format!(
                    "First-use lease of opaque vault credential '{secret_ref}' requires operator approval"
                ),
            }
        }
    }
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn canonicalize_with_tail(path: &Path) -> PathBuf {
    let mut remainder: Vec<std::ffi::OsString> = Vec::new();
    let mut cursor = path;
    loop {
        if let Ok(canon) = cursor.canonicalize() {
            let mut full = canon;
            for seg in remainder.into_iter().rev() {
                full.push(seg);
            }
            return full;
        }
        match (cursor.parent(), cursor.file_name()) {
            (Some(parent), Some(name)) => {
                remainder.push(name.to_os_string());
                cursor = parent;
            }
            _ => return normalize_lexical(path),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gateway_refuses_blocklisted_app_and_cites_rule() {
        let gw = SecurityGateway::new(BlocklistManager::new(&["1Password".into()]));
        let action = ComputerAction::Click {
            token: Some("btn_1".into()),
            coords: None,
            button: None,
            click_count: None,
        };
        let dec = gw.evaluate_gui_action(Some("1Password"), &action, Some("Unlock"));
        assert_eq!(dec.outcome, PolicyOutcome::Refused);
        assert_eq!(dec.rule_id, "RULE-BLK-001");
    }

    #[test]
    fn test_gateway_workspace_path_confinement() {
        let gw = SecurityGateway::new(BlocklistManager::new(&[]));
        let root = std::env::temp_dir().join(format!("gw_bot_test_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        let ok_dec = gw.evaluate_workspace_path(&root, Path::new("workspace/hello.txt"), true);
        assert_eq!(ok_dec.outcome, PolicyOutcome::Permitted);
        assert_eq!(ok_dec.rule_id, "RULE-STD-000");

        let escape_dec = gw.evaluate_workspace_path(&root, Path::new("../../etc/passwd"), false);
        assert_eq!(escape_dec.outcome, PolicyOutcome::Refused);
        assert_eq!(escape_dec.rule_id, "RULE-PATH-002");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_gateway_shell_rules() {
        let gw = SecurityGateway::new(BlocklistManager::new(&[]));

        let r1 = gw.evaluate_shell_command("sudo rm -rf /");
        assert_eq!(r1.outcome, PolicyOutcome::Refused);
        assert_eq!(r1.rule_id, "RULE-SH-003");

        let r2 = gw.evaluate_shell_command("git push origin main");
        assert_eq!(r2.outcome, PolicyOutcome::NeedsApproval);
        assert_eq!(r2.rule_id, "RULE-SH-004");

        let r3 = gw.evaluate_shell_command("cargo test --quiet");
        assert_eq!(r3.outcome, PolicyOutcome::Permitted);
        assert_eq!(r3.rule_id, "RULE-STD-000");
    }

    #[test]
    fn test_scoped_path_confinement_readonly_and_persona_approval() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = tmp.path().join("storage");
        let bot_root = storage.join("bots/muse");
        std::fs::create_dir_all(storage.join("skills")).unwrap();
        std::fs::create_dir_all(bot_root.join("workspace")).unwrap();

        let paths = gm_core::muse_paths::MuseRuntimePaths::for_bot(&storage, &bot_root, "muse");
        let gw = SecurityGateway::new(BlocklistManager::new(&[]));

        // 1. Escaping canonical mount root is refused
        let escaped = ResolvedMusePath {
            absolute: storage.join("skills/../MEMORY.md"),
            mount_label: "skills".into(),
            mount_root: storage.join("skills"),
            relative: PathBuf::from("../MEMORY.md"),
            access: MountAccess::ReadOnly,
        };
        let dec = gw.evaluate_scoped_path(&escaped, false, false);
        assert_eq!(dec.outcome, PolicyOutcome::Refused);
        assert_eq!(dec.rule_id, "RULE-PATH-002");

        // 2. ReadOnly mount permits read, refuses write
        let skill_path = paths.resolve_virtual_path("skills/goals/SKILL.md").unwrap();
        let read_dec = gw.evaluate_scoped_path(&skill_path, false, false);
        assert_eq!(read_dec.outcome, PolicyOutcome::Permitted);
        assert_eq!(read_dec.tier, ActionTier::Read);

        let write_ro = gw.evaluate_scoped_path(&skill_path, true, false);
        assert_eq!(write_ro.outcome, PolicyOutcome::Refused);
        assert_eq!(write_ro.rule_id, "RULE-PATH-002");
        assert!(
            write_ro
                .citation
                .contains("Cannot modify read-only mount 'skills'")
        );

        // 3. PersonaApproval mount permits read, asks approval on write unless approved
        let soul = paths.resolve_virtual_path("SOUL.md").unwrap();
        assert_eq!(
            gw.evaluate_scoped_path(&soul, false, false).outcome,
            PolicyOutcome::Permitted
        );
        let soul_unapproved = gw.evaluate_scoped_path(&soul, true, false);
        assert_eq!(soul_unapproved.outcome, PolicyOutcome::NeedsApproval);
        assert_eq!(soul_unapproved.tier, ActionTier::Irreversible);

        let soul_approved = gw.evaluate_scoped_path(&soul, true, true);
        assert_eq!(soul_approved.outcome, PolicyOutcome::Permitted);
        assert_eq!(soul_approved.tier, ActionTier::Irreversible);

        // 4. ReadWrite mount permits write as Reversible
        let mem = paths.resolve_virtual_path("~/MEMORY.md").unwrap();
        let mem_write = gw.evaluate_scoped_path(&mem, true, false);
        assert_eq!(mem_write.outcome, PolicyOutcome::Permitted);
        assert_eq!(mem_write.tier, ActionTier::Reversible);
    }

    #[cfg(unix)]
    #[test]
    fn test_scoped_path_refuses_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = tmp.path().join("storage");
        let bot_root = storage.join("bots/muse");
        std::fs::create_dir_all(&bot_root).unwrap();
        std::fs::create_dir_all(bot_root.join("workspace")).unwrap();
        let outside = tmp.path().join("outside.txt");
        std::fs::write(&outside, "secret").unwrap();

        std::os::unix::fs::symlink(&outside, bot_root.join("workspace/link.txt")).unwrap();
        let paths = gm_core::muse_paths::MuseRuntimePaths::for_bot(&storage, &bot_root, "muse");
        let resolved = paths.resolve_virtual_path("link.txt").unwrap();

        let gw = SecurityGateway::new(BlocklistManager::new(&[]));
        let dec = gw.evaluate_scoped_path(&resolved, false, false);
        assert_eq!(dec.outcome, PolicyOutcome::Refused);
        assert_eq!(dec.rule_id, "RULE-PATH-002");

        // Also catch an intra-mount broken symlink via symlink_metadata
        std::os::unix::fs::symlink(
            bot_root.join("workspace/missing_target.txt"),
            bot_root.join("workspace/dangling.txt"),
        )
        .unwrap();
        let dangling = paths.resolve_virtual_path("dangling.txt").unwrap();
        let dec_dangling = gw.evaluate_scoped_path(&dangling, true, false);
        assert_eq!(dec_dangling.outcome, PolicyOutcome::Refused);
        assert_eq!(dec_dangling.rule_id, "RULE-PATH-002");
    }

    #[test]
    fn approval_grant_binds_exact_action_recipient_patch_tenant_and_expiry() {
        let now = chrono::Utc::now();
        let payload = "{\"patch_sha256\":\"abc123\",\"body\":\"hello\"}";
        let grant = ActionApprovalGrant {
            grant_id: "g-1".into(),
            action_name: "send_email".into(),
            action_digest: ActionApprovalGrant::compute_action_digest("send_email", payload),
            workspace_id: "ws-alpha".into(),
            destination: "alice@example.com".into(),
            principal: "user-1".into(),
            job_id: "job-1".into(),
            irreversible_summary: "Send email to alice@example.com".into(),
            expires_at: now + chrono::Duration::minutes(5),
        };
        let base = ActionExecutionContext {
            action_name: "send_email",
            canonical_payload: payload,
            workspace_id: "ws-alpha",
            destination: "alice@example.com",
            principal: "user-1",
            job_id: "job-1",
            now,
        };
        assert!(grant.verify_before_execution(&base).is_ok());
        // Changed recipient fails
        assert!(
            grant
                .verify_before_execution(&ActionExecutionContext {
                    destination: "mallory@example.com",
                    ..base
                })
                .is_err()
        );
        // Modified patch/payload fails
        assert!(
            grant
                .verify_before_execution(&ActionExecutionContext {
                    canonical_payload: "{\"patch_sha256\":\"evil\",\"body\":\"hello\"}",
                    ..base
                })
                .is_err()
        );
        // Changed workspace/tenant fails
        assert!(
            grant
                .verify_before_execution(&ActionExecutionContext {
                    workspace_id: "ws-beta",
                    ..base
                })
                .is_err()
        );
        // Expired grant fails
        assert!(
            grant
                .verify_before_execution(&ActionExecutionContext {
                    now: now + chrono::Duration::minutes(10),
                    ..base
                })
                .is_err()
        );
    }
}
