//! Pure-std virtual path resolution and mount policy definitions for the Muse
//! hybrid runtime layout (shared user/memory/skills storage + per-bot persona
//! and workspace roots). Contains no platform-specific or `libc` dependencies.

use crate::config::GmConfig;
use crate::error::{GmError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

/// Access policy attached to a resolved Muse runtime mount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MountAccess {
    /// Immutable runtime assets or inbound transfers (e.g. `skills/`, `docs/`, `inbox/`).
    ReadOnly,
    /// Mutable runtime data, memory, goals, spaces, or bot workspace/outbox.
    ReadWrite,
    /// Per-bot persona files requiring explicit `Irreversible` approval before mutation.
    PersonaApproval,
}

/// Declarative mount entry mapping a virtual prefix to a filesystem location and access policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MuseMount {
    pub label: String,
    pub virtual_prefix: String,
    pub root: PathBuf,
    pub access: MountAccess,
}

/// Hybrid runtime directory layout shared across bots plus optional active bot binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MuseRuntimePaths {
    pub storage: PathBuf,
    pub bot_root: Option<PathBuf>,
    pub bot_id: Option<String>,
}

/// Virtual path resolved to a canonical target, its owning directory mount root, and policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedMusePath {
    pub absolute: PathBuf,
    pub mount_label: String,
    pub mount_root: PathBuf,
    pub relative: PathBuf,
    pub access: MountAccess,
}

#[derive(Debug, Clone)]
struct MountSpec {
    label: &'static str,
    prefix: &'static [&'static str],
    root: PathBuf,
    mount_root: PathBuf,
    is_file: bool,
    access: MountAccess,
}

impl MuseRuntimePaths {
    /// Shared storage layout without an active bot workspace binding.
    pub fn shared(storage: impl AsRef<Path>) -> Self {
        Self {
            storage: storage.as_ref().to_path_buf(),
            bot_root: None,
            bot_id: None,
        }
    }

    /// Hybrid layout combining shared storage with a specific bot workspace root.
    pub fn for_bot(
        storage: impl AsRef<Path>,
        bot_root: impl AsRef<Path>,
        bot_id: impl Into<String>,
    ) -> Self {
        Self {
            storage: storage.as_ref().to_path_buf(),
            bot_root: Some(bot_root.as_ref().to_path_buf()),
            bot_id: Some(bot_id.into()),
        }
    }

    /// Bind or replace the active bot workspace on an existing shared layout.
    pub fn with_bot(mut self, bot_root: impl AsRef<Path>, bot_id: impl Into<String>) -> Self {
        self.bot_root = Some(bot_root.as_ref().to_path_buf());
        self.bot_id = Some(bot_id.into());
        self
    }

    /// Derive paths from `GmConfig`, optionally binding a bot ID under `config.openbot.bots_dir`.
    pub fn from_config(config: &GmConfig, bot_id: Option<&str>) -> Self {
        let storage = config.storage_dir.clone();
        match bot_id {
            Some(id) => Self {
                storage,
                bot_root: Some(config.openbot.bots_dir.join(id)),
                bot_id: Some(id.to_string()),
            },
            None => Self::shared(storage),
        }
    }

    pub fn storage_dir(&self) -> &Path {
        &self.storage
    }

    // ---- Shared storage accessors ----

    pub fn user_md(&self) -> PathBuf {
        self.storage.join("USER.md")
    }

    pub fn memory_md(&self) -> PathBuf {
        self.storage.join("MEMORY.md")
    }

    pub fn proactive_preferences_md(&self) -> PathBuf {
        self.storage.join("PROACTIVE_PREFERENCES.md")
    }

    pub fn feedback_md(&self) -> PathBuf {
        self.storage.join("FEEDBACK.md")
    }

    pub fn timezones_yaml(&self) -> PathBuf {
        self.storage.join("workspace/user/timezones.yaml")
    }

    pub fn system_prompt_md(&self) -> PathBuf {
        self.storage.join("workspace/system/system_prompt.md")
    }

    pub fn bank_experience_md(&self) -> PathBuf {
        self.storage.join("memory/bank/experience.md")
    }

    pub fn bank_opinions_md(&self) -> PathBuf {
        self.storage.join("memory/bank/opinions.md")
    }

    pub fn bank_world_md(&self) -> PathBuf {
        self.storage.join("memory/bank/world.md")
    }

    pub fn people_dir(&self) -> PathBuf {
        self.storage.join("memory/people")
    }

    pub fn people_index_md(&self) -> PathBuf {
        self.storage.join("memory/people/INDEX.md")
    }

    pub fn groups_dir(&self) -> PathBuf {
        self.storage.join("memory/groups")
    }

    pub fn groups_index_md(&self) -> PathBuf {
        self.storage.join("memory/groups/INDEX.md")
    }

    pub fn shopping_profile_md(&self) -> PathBuf {
        self.storage.join("memory/shopping/PROFILE.md")
    }

    pub fn travel_profile_md(&self) -> PathBuf {
        self.storage.join("memory/travel/PROFILE.md")
    }

    pub fn alignment_synthesis_md(&self) -> PathBuf {
        self.storage
            .join("dreams/alignment/derived/ALIGNMENT_SYNTHESIS.md")
    }

    pub fn dreams_dir(&self) -> PathBuf {
        self.storage.join("dreams")
    }

    pub fn dreams_daily_dir(&self) -> PathBuf {
        self.storage.join("dreams/daily")
    }

    pub fn memory_dir(&self) -> PathBuf {
        self.storage.join("memory")
    }

    pub fn provenance_db_path(&self) -> PathBuf {
        self.storage.join("memory/provenance.sqlite")
    }

    pub fn self_improvement_migrations_dir(&self) -> PathBuf {
        self.storage.join("self_improvement/migrations")
    }

    pub fn skill_amendments_dir(&self) -> PathBuf {
        self.storage.join("workspace/skill-amendments")
    }

    pub fn connector_amendments_dir(&self) -> PathBuf {
        self.storage.join("workspace/connector-amendments")
    }

    pub fn workspace_goals_dir(&self) -> PathBuf {
        self.storage.join("workspace/goals")
    }

    pub fn forget_pending_path(&self) -> PathBuf {
        self.storage.join("workspace/memory/forget/pending.json")
    }

    pub fn goals_dir(&self) -> PathBuf {
        self.storage.join("goals")
    }

    pub fn side_chats_dir(&self) -> PathBuf {
        self.storage.join("side-chats")
    }

    pub fn custom_skills_dir(&self) -> PathBuf {
        self.storage.join("custom-skills")
    }

    pub fn custom_connectors_dir(&self) -> PathBuf {
        self.storage.join("custom-connectors")
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.storage.join("skills")
    }

    pub fn connectors_dir(&self) -> PathBuf {
        self.storage.join("connectors")
    }

    pub fn docs_dir(&self) -> PathBuf {
        self.storage.join("docs")
    }

    pub fn spaces_dir(&self) -> PathBuf {
        self.storage.join("spaces")
    }

    pub fn schema_version_path(&self) -> PathBuf {
        self.storage.join(".muse_schema_version")
    }

    // ---- Per-bot accessors ----

    pub fn soul_md(&self) -> Option<PathBuf> {
        self.bot_root.as_ref().map(|b| b.join("SOUL.md"))
    }

    pub fn identity_md(&self) -> Option<PathBuf> {
        self.bot_root.as_ref().map(|b| b.join("IDENTITY.md"))
    }

    pub fn agents_md(&self) -> Option<PathBuf> {
        self.bot_root.as_ref().map(|b| b.join("AGENTS.md"))
    }

    pub fn tools_md(&self) -> Option<PathBuf> {
        self.bot_root.as_ref().map(|b| b.join("TOOLS.md"))
    }

    pub fn heartbeat_md(&self) -> Option<PathBuf> {
        self.bot_root.as_ref().map(|b| b.join("HEARTBEAT.md"))
    }

    pub fn bot_workspace_dir(&self) -> Option<PathBuf> {
        self.bot_root.as_ref().map(|b| b.join("workspace"))
    }

    pub fn bot_inbox_dir(&self) -> Option<PathBuf> {
        self.bot_root.as_ref().map(|b| b.join("inbox"))
    }

    pub fn bot_outbox_dir(&self) -> Option<PathBuf> {
        self.bot_root.as_ref().map(|b| b.join("outbox"))
    }

    pub fn heartbeat_state_path(&self) -> Option<PathBuf> {
        self.bot_root
            .as_ref()
            .map(|b| b.join("workspace/.heartbeat_state.json"))
    }

    fn mount_specs(&self) -> Vec<MountSpec> {
        let mut specs = vec![
            // 1. Most specific ReadOnly shared mounts
            MountSpec {
                label: "memory/bank",
                prefix: &["memory", "bank"],
                root: self.bank_experience_md().parent().unwrap().to_path_buf(),
                mount_root: self.storage.join("memory/bank"),
                is_file: false,
                access: MountAccess::ReadOnly,
            },
            MountSpec {
                label: "self_improvement/migrations",
                prefix: &["self_improvement", "migrations"],
                root: self.self_improvement_migrations_dir(),
                mount_root: self.self_improvement_migrations_dir(),
                is_file: false,
                access: MountAccess::ReadOnly,
            },
            MountSpec {
                label: "skills",
                prefix: &["skills"],
                root: self.skills_dir(),
                mount_root: self.skills_dir(),
                is_file: false,
                access: MountAccess::ReadOnly,
            },
            MountSpec {
                label: "connectors",
                prefix: &["connectors"],
                root: self.connectors_dir(),
                mount_root: self.connectors_dir(),
                is_file: false,
                access: MountAccess::ReadOnly,
            },
            MountSpec {
                label: "docs",
                prefix: &["docs"],
                root: self.docs_dir(),
                mount_root: self.docs_dir(),
                is_file: false,
                access: MountAccess::ReadOnly,
            },
        ];

        // 2. Per-bot ReadOnly and PersonaApproval mounts (if bound)
        if let Some(bot_root) = &self.bot_root {
            specs.push(MountSpec {
                label: "inbox",
                prefix: &["inbox"],
                root: bot_root.join("inbox"),
                mount_root: bot_root.join("inbox"),
                is_file: false,
                access: MountAccess::ReadOnly,
            });
            for (label, prefix) in [
                ("SOUL.md", &["SOUL.md"][..]),
                ("IDENTITY.md", &["IDENTITY.md"][..]),
                ("AGENTS.md", &["AGENTS.md"][..]),
                ("TOOLS.md", &["TOOLS.md"][..]),
                ("HEARTBEAT.md", &["HEARTBEAT.md"][..]),
            ] {
                specs.push(MountSpec {
                    label,
                    prefix,
                    root: bot_root.join(label),
                    mount_root: bot_root.clone(),
                    is_file: true,
                    access: MountAccess::PersonaApproval,
                });
            }
        }

        // 3. Shared root files (`ReadWrite`)
        for (label, prefix) in [
            ("USER.md", &["USER.md"][..]),
            ("MEMORY.md", &["MEMORY.md"][..]),
            (
                "PROACTIVE_PREFERENCES.md",
                &["PROACTIVE_PREFERENCES.md"][..],
            ),
            ("FEEDBACK.md", &["FEEDBACK.md"][..]),
        ] {
            specs.push(MountSpec {
                label,
                prefix,
                root: self.storage.join(label),
                mount_root: self.storage.clone(),
                is_file: true,
                access: MountAccess::ReadWrite,
            });
        }

        // 4. Shared `workspace/*` sub-mounts (must appear BEFORE bot `workspace/`)
        for (label, prefix, subdir) in [
            (
                "workspace/goals",
                &["workspace", "goals"][..],
                "workspace/goals",
            ),
            (
                "workspace/skill-amendments",
                &["workspace", "skill-amendments"][..],
                "workspace/skill-amendments",
            ),
            (
                "workspace/connector-amendments",
                &["workspace", "connector-amendments"][..],
                "workspace/connector-amendments",
            ),
            (
                "workspace/user",
                &["workspace", "user"][..],
                "workspace/user",
            ),
            (
                "workspace/system",
                &["workspace", "system"][..],
                "workspace/system",
            ),
            (
                "workspace/memory",
                &["workspace", "memory"][..],
                "workspace/memory",
            ),
        ] {
            let dir = self.storage.join(subdir);
            specs.push(MountSpec {
                label,
                prefix,
                root: dir.clone(),
                mount_root: dir,
                is_file: false,
                access: MountAccess::ReadWrite,
            });
        }

        // 5. Shared directory mounts (`ReadWrite`)
        for (label, prefix, dir) in [
            ("memory", &["memory"][..], self.memory_dir()),
            ("dreams", &["dreams"][..], self.dreams_dir()),
            ("goals", &["goals"][..], self.goals_dir()),
            ("side-chats", &["side-chats"][..], self.side_chats_dir()),
            (
                "custom-skills",
                &["custom-skills"][..],
                self.custom_skills_dir(),
            ),
            (
                "custom-connectors",
                &["custom-connectors"][..],
                self.custom_connectors_dir(),
            ),
            ("spaces", &["spaces"][..], self.spaces_dir()),
        ] {
            specs.push(MountSpec {
                label,
                prefix,
                root: dir.clone(),
                mount_root: dir,
                is_file: false,
                access: MountAccess::ReadWrite,
            });
        }

        // 6. Per-bot outbox and workspace mounts
        if let Some(bot_root) = &self.bot_root {
            specs.push(MountSpec {
                label: "outbox",
                prefix: &["outbox"],
                root: bot_root.join("outbox"),
                mount_root: bot_root.join("outbox"),
                is_file: false,
                access: MountAccess::ReadWrite,
            });
            specs.push(MountSpec {
                label: "workspace",
                prefix: &["workspace"],
                root: bot_root.join("workspace"),
                mount_root: bot_root.join("workspace"),
                is_file: false,
                access: MountAccess::ReadWrite,
            });
        }

        specs
    }

    /// Return the configured virtual mounts ordered from most specific to least specific.
    pub fn mounts(&self) -> Vec<MuseMount> {
        self.mount_specs()
            .into_iter()
            .map(|s| MuseMount {
                label: s.label.to_string(),
                virtual_prefix: s.prefix.join("/"),
                root: s.root,
                access: s.access,
            })
            .collect()
    }

    /// Returns true if `raw` explicitly addresses a Muse virtual scheme (`~/`, `sandbox://`)
    /// or starts with a declared mount prefix.
    pub fn matches_explicit_mount(&self, raw: &str) -> bool {
        let trimmed = raw.trim();
        if trimmed.starts_with("~/") || trimmed.starts_with("sandbox://") {
            return true;
        }
        let Ok(parts) = normalize_virtual_components(trimmed) else {
            return false;
        };
        let str_parts: Vec<&str> = parts.iter().filter_map(|p| p.to_str()).collect();
        self.mount_specs()
            .iter()
            .any(|spec| starts_with_components(&str_parts, spec.prefix))
    }

    /// Resolve a virtual Muse path (`~/USER.md`, `sandbox://outbox/r.pdf`, `SOUL.md`, `skills/x`, `notes/a.md`).
    pub fn resolve_virtual_path(&self, raw: &str) -> Result<ResolvedMusePath> {
        if raw.contains('\0') {
            return Err(GmError::SafetyViolation(
                "RULE-PATH-002: Path contains NUL byte".into(),
            ));
        }

        let stripped = if let Some(rest) = raw.strip_prefix("sandbox://") {
            rest
        } else if let Some(rest) = raw.strip_prefix("~/") {
            rest
        } else {
            raw
        };

        // Reject Windows drive letters (`C:\...`, `C:/...`), backslashes, and leading slashes across all OSes.
        if stripped.is_empty()
            || raw.starts_with('/')
            || stripped.starts_with('/')
            || stripped.contains('\\')
            || has_windows_drive_prefix(raw)
            || has_windows_drive_prefix(stripped)
        {
            return Err(GmError::SafetyViolation(format!(
                "RULE-PATH-002: Absolute, backslash, or drive-prefixed path '{raw}' is prohibited"
            )));
        }

        let parts = normalize_virtual_components(stripped)?;
        if parts.is_empty() {
            return Err(GmError::SafetyViolation(format!(
                "RULE-PATH-002: Empty virtual path '{raw}'"
            )));
        }

        // Guard against a literal `~` segment surviving `sandbox://~/...`.
        if parts.first().and_then(|s| s.to_str()) == Some("~") {
            return Err(GmError::SafetyViolation(format!(
                "RULE-PATH-002: Invalid tilde segment in '{raw}'"
            )));
        }

        let str_parts: Vec<Option<&str>> = parts.iter().map(|s| s.to_str()).collect();
        for spec in self.mount_specs() {
            if str_parts.len() >= spec.prefix.len()
                && spec
                    .prefix
                    .iter()
                    .enumerate()
                    .all(|(idx, expected)| str_parts[idx] == Some(*expected))
            {
                let rest = &parts[spec.prefix.len()..];
                if spec.is_file {
                    if !rest.is_empty() {
                        return Err(GmError::SafetyViolation(format!(
                            "RULE-PATH-002: Cannot traverse beneath file mount '{}'",
                            spec.label
                        )));
                    }
                    let file_name = PathBuf::from(spec.label);
                    return Ok(ResolvedMusePath {
                        absolute: spec.mount_root.join(&file_name),
                        mount_label: spec.label.to_string(),
                        mount_root: spec.mount_root,
                        relative: file_name,
                        access: spec.access,
                    });
                }

                let mut rel = PathBuf::new();
                for seg in rest {
                    rel.push(seg);
                }
                let absolute = if rest.is_empty() {
                    spec.mount_root.clone()
                } else {
                    spec.mount_root.join(&rel)
                };
                let relative = if rest.is_empty() {
                    PathBuf::from(".")
                } else {
                    rel
                };
                return Ok(ResolvedMusePath {
                    absolute,
                    mount_label: spec.label.to_string(),
                    mount_root: spec.mount_root,
                    relative,
                    access: spec.access,
                });
            }
        }

        // Check whether a per-bot file was requested without an active bot workspace.
        if self.bot_root.is_none()
            && let Some(first) = parts.first().and_then(|s| s.to_str())
            && matches!(
                first,
                "SOUL.md"
                    | "IDENTITY.md"
                    | "AGENTS.md"
                    | "TOOLS.md"
                    | "HEARTBEAT.md"
                    | "inbox"
                    | "outbox"
                    | "workspace"
            )
        {
            return Err(GmError::SafetyViolation(format!(
                "RULE-PATH-002: Path '{raw}' requires an active bot workspace"
            )));
        }

        let bot_workspace = self.bot_workspace_dir().ok_or_else(|| {
            GmError::SafetyViolation(format!(
                "RULE-PATH-002: No bot workspace bound to resolve relative path '{raw}'"
            ))
        })?;
        let mut rel = PathBuf::new();
        for seg in &parts {
            rel.push(seg);
        }
        Ok(ResolvedMusePath {
            absolute: bot_workspace.join(&rel),
            mount_label: "workspace".to_string(),
            mount_root: bot_workspace,
            relative: rel,
            access: MountAccess::ReadWrite,
        })
    }
}

fn has_windows_drive_prefix(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn starts_with_components(parts: &[&str], prefix: &[&str]) -> bool {
    parts.len() >= prefix.len() && parts[..prefix.len()] == *prefix
}

fn normalize_virtual_components(stripped: &str) -> Result<Vec<std::ffi::OsString>> {
    let mut parts = Vec::new();
    for comp in Path::new(stripped).components() {
        match comp {
            Component::Normal(seg) => {
                if seg.to_str().is_some_and(|s| s.contains(':')) {
                    return Err(GmError::SafetyViolation(format!(
                        "RULE-PATH-002: Forbidden colon in path segment '{}'",
                        stripped
                    )));
                }
                parts.push(seg.to_os_string());
            }
            other => {
                return Err(GmError::SafetyViolation(format!(
                    "RULE-PATH-002: Non-normal path component {:?} in '{}'",
                    other, stripped
                )));
            }
        }
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_paths() -> MuseRuntimePaths {
        MuseRuntimePaths::for_bot("/tmp/gm-storage", "/tmp/gm-storage/bots/muse", "muse")
    }

    #[test]
    fn resolves_shared_user_md_from_tilde() {
        let paths = fixture_paths();
        let resolved = paths.resolve_virtual_path("~/USER.md").unwrap();
        assert_eq!(resolved.absolute, PathBuf::from("/tmp/gm-storage/USER.md"));
        assert_eq!(resolved.mount_label, "USER.md");
        assert_eq!(resolved.access, MountAccess::ReadWrite);
    }

    #[test]
    fn resolves_bot_persona_soul_with_approval_access() {
        let paths = fixture_paths();
        let resolved = paths.resolve_virtual_path("SOUL.md").unwrap();
        assert_eq!(
            resolved.absolute,
            PathBuf::from("/tmp/gm-storage/bots/muse/SOUL.md")
        );
        assert_eq!(resolved.mount_label, "SOUL.md");
        assert_eq!(
            resolved.mount_root,
            PathBuf::from("/tmp/gm-storage/bots/muse")
        );
        assert_eq!(resolved.relative, PathBuf::from("SOUL.md"));
        assert_eq!(resolved.access, MountAccess::PersonaApproval);
    }

    #[test]
    fn resolves_skills_and_docs_and_migrations_as_readonly() {
        let paths = fixture_paths();
        for p in [
            "skills/x",
            "~/docs/muse.md",
            "memory/bank/world.md",
            "self_improvement/migrations/001.sql",
            "inbox/brief.md",
        ] {
            let resolved = paths.resolve_virtual_path(p).unwrap();
            assert_eq!(
                resolved.access,
                MountAccess::ReadOnly,
                "expected ReadOnly for {p}"
            );
        }
    }

    #[test]
    fn rejects_traversal_absolute_drive_and_nul_paths() {
        let paths = fixture_paths();
        for bad in [
            "../etc/passwd",
            "/etc/passwd",
            "~/../x",
            r"C:\x",
            "C:/x",
            "a\0b",
            "sandbox:///etc/passwd",
            "skills/../MEMORY.md",
            "./USER.md",
            "SOUL.md/child",
            "",
        ] {
            let err = paths.resolve_virtual_path(bad).unwrap_err();
            assert!(
                err.to_string().contains("RULE-PATH-002"),
                "expected RULE-PATH-002 for {bad:?}, got {err}"
            );
        }
    }

    #[test]
    fn resolves_unmatched_relative_path_to_bot_workspace() {
        let paths = fixture_paths();
        let resolved = paths.resolve_virtual_path("notes/a.md").unwrap();
        assert_eq!(
            resolved.absolute,
            PathBuf::from("/tmp/gm-storage/bots/muse/workspace/notes/a.md")
        );
        assert_eq!(resolved.mount_label, "workspace");
        assert_eq!(resolved.access, MountAccess::ReadWrite);
    }

    #[test]
    fn resolves_sandbox_scheme_outbox_path() {
        let paths = fixture_paths();
        let resolved = paths
            .resolve_virtual_path("sandbox://outbox/r.pdf")
            .unwrap();
        assert_eq!(
            resolved.absolute,
            PathBuf::from("/tmp/gm-storage/bots/muse/outbox/r.pdf")
        );
        assert_eq!(resolved.mount_label, "outbox");
        assert_eq!(resolved.access, MountAccess::ReadWrite);
    }

    #[test]
    fn distinguishes_shared_workspace_submounts_from_bot_workspace() {
        let paths = fixture_paths();
        let goal_doc = paths
            .resolve_virtual_path("~/workspace/goals/launch/GOAL.md")
            .unwrap();
        assert_eq!(
            goal_doc.absolute,
            PathBuf::from("/tmp/gm-storage/workspace/goals/launch/GOAL.md")
        );
        assert_eq!(goal_doc.mount_label, "workspace/goals");

        let bot_note = paths.resolve_virtual_path("workspace/draft.md").unwrap();
        assert_eq!(
            bot_note.absolute,
            PathBuf::from("/tmp/gm-storage/bots/muse/workspace/draft.md")
        );
        assert_eq!(bot_note.mount_label, "workspace");
    }

    #[test]
    fn shared_only_paths_reject_bot_only_mounts() {
        let paths = MuseRuntimePaths::shared("/tmp/gm-storage");
        assert!(paths.resolve_virtual_path("USER.md").is_ok());
        assert!(paths.resolve_virtual_path("SOUL.md").is_err());
        assert!(paths.resolve_virtual_path("notes/a.md").is_err());
    }

    #[test]
    fn mounts_list_orders_most_specific_first() {
        let paths = fixture_paths();
        let mounts = paths.mounts();
        let bank_idx = mounts
            .iter()
            .position(|m| m.label == "memory/bank")
            .unwrap();
        let mem_idx = mounts.iter().position(|m| m.label == "memory").unwrap();
        let goals_ws_idx = mounts
            .iter()
            .position(|m| m.label == "workspace/goals")
            .unwrap();
        let bot_ws_idx = mounts.iter().position(|m| m.label == "workspace").unwrap();
        assert!(bank_idx < mem_idx);
        assert!(goals_ws_idx < bot_ws_idx);
    }

    #[test]
    fn accessors_and_from_config_match_expected_layout() {
        let cfg = GmConfig {
            storage_dir: PathBuf::from("/tmp/s"),
            openbot: crate::config::OpenBotConfig {
                bots_dir: PathBuf::from("/tmp/s/bots"),
                ..Default::default()
            },
            ..Default::default()
        };
        let p = MuseRuntimePaths::from_config(&cfg, Some("coder"));
        assert_eq!(p.user_md(), PathBuf::from("/tmp/s/USER.md"));
        assert_eq!(p.memory_md(), PathBuf::from("/tmp/s/MEMORY.md"));
        assert_eq!(
            p.proactive_preferences_md(),
            PathBuf::from("/tmp/s/PROACTIVE_PREFERENCES.md")
        );
        assert_eq!(p.feedback_md(), PathBuf::from("/tmp/s/FEEDBACK.md"));
        assert_eq!(
            p.timezones_yaml(),
            PathBuf::from("/tmp/s/workspace/user/timezones.yaml")
        );
        assert_eq!(
            p.system_prompt_md(),
            PathBuf::from("/tmp/s/workspace/system/system_prompt.md")
        );
        assert_eq!(
            p.alignment_synthesis_md(),
            PathBuf::from("/tmp/s/dreams/alignment/derived/ALIGNMENT_SYNTHESIS.md")
        );
        assert_eq!(
            p.shopping_profile_md(),
            PathBuf::from("/tmp/s/memory/shopping/PROFILE.md")
        );
        assert_eq!(
            p.travel_profile_md(),
            PathBuf::from("/tmp/s/memory/travel/PROFILE.md")
        );
        assert_eq!(
            p.identity_md(),
            Some(PathBuf::from("/tmp/s/bots/coder/IDENTITY.md"))
        );
        assert!(p.matches_explicit_mount("~/USER.md"));
        assert!(p.matches_explicit_mount("sandbox://outbox/a.pdf"));
        assert!(p.matches_explicit_mount("skills/goals/SKILL.md"));
        assert!(!p.matches_explicit_mount("notes/a.md"));
    }
}
