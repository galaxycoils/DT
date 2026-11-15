use crate::approval::{ApprovalRequest, InteractionHandler};
use cap_std::fs::Dir;
use gm_core::error::{GmError, Result};
use gm_core::muse_paths::{MountAccess, MuseRuntimePaths, ResolvedMusePath};
use gm_core::types::ComputerAction;
use gm_safety::{
    AuditCategory, AuditLogger, BlocklistManager, KillSwitch, PolicyOutcome, SecretScrubber,
    SecurityGateway,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceToolCall {
    ReadFile {
        path: String,
    },
    WriteFile {
        path: String,
        content: String,
    },
    ListFiles {
        path: Option<String>,
    },
    Shell {
        command: String,
        timeout_ms: Option<u64>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceToolOutcome {
    pub summary: String,
    pub output: String,
    pub diff_preview: Option<String>,
}

/// Generates a line-level unified diff preview capped at `max_lines` (default 200),
/// matching Darius's write diff preview contract.
pub fn compute_unified_diff_preview(
    rel_path: &str,
    previous: Option<&str>,
    updated: &str,
    max_lines: usize,
) -> String {
    let Some(old) = previous else {
        let line_count = updated.lines().count();
        return format!(
            "+++ b/{rel_path} (new file, {line_count} lines, {} bytes)",
            updated.len()
        );
    };
    if old.is_empty() {
        let line_count = updated.lines().count();
        return format!(
            "+++ b/{rel_path} (populated empty file, {line_count} lines, {} bytes)",
            updated.len()
        );
    }

    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = updated.lines().collect();

    let mut out = Vec::new();
    out.push(format!("--- a/{rel_path}"));
    out.push(format!("+++ b/{rel_path}"));

    let max_idx = old_lines.len().max(new_lines.len());
    for i in 0..max_idx {
        if out.len() >= max_lines {
            out.push(format!("... (diff truncated at {max_lines} lines)"));
            break;
        }
        match (old_lines.get(i), new_lines.get(i)) {
            (Some(a), Some(b)) if a == b => {
                out.push(format!(" {a}"));
            }
            (Some(a), Some(b)) => {
                out.push(format!("-{a}"));
                if out.len() < max_lines {
                    out.push(format!("+{b}"));
                }
            }
            (Some(a), None) => {
                out.push(format!("-{a}"));
            }
            (None, Some(b)) => {
                out.push(format!("+{b}"));
            }
            (None, None) => {}
        }
    }

    out.join("\n")
}

#[derive(Debug, Clone)]
enum ResolvedWorkspacePath {
    Legacy(PathBuf),
    Muse(ResolvedMusePath),
    NeedsPersonaApproval {
        resolved: ResolvedMusePath,
        rule_id: &'static str,
        citation: String,
    },
}

/// Per-bot isolated workspace & macOS Seatbelt (`sandbox-exec`) execution environment.
#[derive(Debug, Clone)]
pub struct BotWorkspaceSandbox {
    bot_id: String,
    bot_root: PathBuf,
    workspace_dir: PathBuf,
    inbox_dir: PathBuf,
    outbox_dir: PathBuf,
    workspace_cap: Arc<Dir>,
    inbox_cap: Arc<Dir>,
    outbox_cap: Arc<Dir>,
    muse_paths: Option<MuseRuntimePaths>,
    gateway: SecurityGateway,
    scrubber: SecretScrubber,
    allow_network: bool,
    vm_config: gm_core::config::WorkspaceVmConfig,
}

impl BotWorkspaceSandbox {
    pub fn new(bot_id: impl Into<String>, bot_root: impl AsRef<Path>) -> Result<Self> {
        let bot_id = bot_id.into();
        let root = bot_root.as_ref().to_path_buf();
        let workspace_dir = root.join("workspace");
        let inbox_dir = root.join("inbox");
        let outbox_dir = root.join("outbox");

        std::fs::create_dir_all(&workspace_dir)?;
        std::fs::create_dir_all(&inbox_dir)?;
        std::fs::create_dir_all(&outbox_dir)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700));
        }

        let canonical_root = root.canonicalize().unwrap_or(root);
        let workspace_dir = canonical_root.join("workspace");
        let inbox_dir = canonical_root.join("inbox");
        let outbox_dir = canonical_root.join("outbox");

        let workspace_cap = Arc::new(Dir::open_ambient_dir(
            &workspace_dir,
            cap_std::ambient_authority(),
        )?);
        let inbox_cap = Arc::new(Dir::open_ambient_dir(
            &inbox_dir,
            cap_std::ambient_authority(),
        )?);
        let outbox_cap = Arc::new(Dir::open_ambient_dir(
            &outbox_dir,
            cap_std::ambient_authority(),
        )?);
        Ok(Self {
            workspace_cap,
            inbox_cap,
            outbox_cap,
            muse_paths: None,
            bot_id,
            bot_root: canonical_root,
            workspace_dir,
            inbox_dir,
            outbox_dir,
            gateway: SecurityGateway::new(BlocklistManager::new(&[])),
            scrubber: SecretScrubber::new(),
            allow_network: true,
            vm_config: gm_core::config::WorkspaceVmConfig::default(),
        })
    }

    pub fn with_shared_runtime_paths(mut self, mut paths: MuseRuntimePaths) -> Self {
        if paths.bot_root.is_none() {
            paths = paths.with_bot(&self.bot_root, &self.bot_id);
        }
        self.muse_paths = Some(paths);
        self
    }

    pub fn shared_runtime_paths(&self) -> Option<&MuseRuntimePaths> {
        self.muse_paths.as_ref()
    }

    pub fn with_scrubber(mut self, scrubber: SecretScrubber) -> Self {
        self.scrubber = scrubber;
        self
    }

    pub fn with_vm_config(mut self, config: gm_core::config::WorkspaceVmConfig) -> Self {
        self.vm_config = config;
        self
    }

    pub fn read_artifact(&self, name: &std::ffi::OsStr) -> Result<Vec<u8>> {
        let path = Path::new(name);
        if path.components().count() != 1 || path.file_name() != Some(name) {
            return Err(GmError::SafetyViolation(
                "Artifact must be a single filename".into(),
            ));
        }
        let handle = if self.outbox_cap.exists(path) {
            &self.outbox_cap
        } else {
            &self.workspace_cap
        };
        if !handle.metadata(path)?.is_file() || handle.metadata(path)?.len() > 8 * 1024 * 1024 {
            return Err(GmError::Config(
                "Artifact is not a regular file within the size limit".into(),
            ));
        }
        use std::io::Read;
        let mut bytes = Vec::new();
        handle
            .open(path)?
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(GmError::Config("Artifact exceeds transfer limit".into()));
        }
        Ok(bytes)
    }
    pub fn receive_artifact(&self, name: &std::ffi::OsStr, data: &[u8]) -> Result<PathBuf> {
        use std::io::Write;
        let path = Path::new(name);
        if path.components().count() != 1
            || path.file_name() != Some(name)
            || data.len() > 8 * 1024 * 1024
        {
            return Err(GmError::SafetyViolation("Invalid artifact transfer".into()));
        }
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = self.inbox_cap.open_with(path, &options)?;
        file.write_all(data)?;
        file.sync_all()?;
        Ok(self.inbox_dir.join(name))
    }

    /// Lists inert inbox/outbox entries; never follows links or opens executables.
    pub fn shared_files(&self) -> Result<Vec<(String, u64)>> {
        fn walk(
            dir: &Dir,
            prefix: &str,
            out: &mut Vec<(String, u64)>,
            visited: &mut usize,
            depth: usize,
        ) -> Result<()> {
            if depth > 16 {
                return Err(GmError::Config(
                    "Artifact directory nesting exceeds limit".into(),
                ));
            }
            for entry in dir.entries()? {
                *visited += 1;
                if *visited > 1000 {
                    return Err(GmError::Config(
                        "Artifact directory entry limit exceeded".into(),
                    ));
                }
                let entry = entry?;
                let kind = entry.file_type()?;
                if kind.is_symlink() {
                    continue;
                }
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let path = format!("{prefix}/{name}");
                if kind.is_dir() {
                    walk(&entry.open_dir()?, &path, out, visited, depth + 1)?;
                } else if kind.is_file() {
                    out.push((path, entry.metadata()?.len()));
                }
            }
            Ok(())
        }
        let mut out = vec![];
        let mut visited = 0;
        walk(&self.inbox_cap, "inbox", &mut out, &mut visited, 0)?;
        walk(&self.outbox_cap, "outbox", &mut out, &mut visited, 0)?;
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }
    pub fn preview_shared_file(&self, path: &str) -> Result<Vec<u8>> {
        use std::io::Read;
        let (area, relative) = path
            .split_once('/')
            .ok_or_else(|| GmError::Config("Expected inbox/ or outbox/ path".into()))?;
        if relative.is_empty()
            || relative.contains('\\')
            || relative
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == ".." || p.contains(':'))
        {
            return Err(GmError::SafetyViolation("Invalid artifact path".into()));
        }
        let directory = match area {
            "inbox" => &self.inbox_cap,
            "outbox" => &self.outbox_cap,
            _ => {
                return Err(GmError::SafetyViolation(
                    "Only inbox and outbox are exposed to previews".into(),
                ));
            }
        };
        let file = directory.open(relative)?;
        if !file.metadata()?.is_file() || file.metadata()?.len() > 1024 * 1024 {
            return Err(GmError::Config(
                "Preview requires a regular file under 1 MiB".into(),
            ));
        }
        let mut bytes = vec![];
        file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 1024 * 1024 {
            return Err(GmError::Config("Preview exceeds 1 MiB".into()));
        }
        Ok(bytes)
    }
    pub fn network_allowed(&self) -> bool {
        self.allow_network
    }

    pub fn with_network(mut self, allow_network: bool) -> Self {
        self.allow_network = allow_network;
        self
    }

    pub fn bot_id(&self) -> &str {
        &self.bot_id
    }

    pub fn bot_root(&self) -> &Path {
        &self.bot_root
    }

    pub fn workspace_dir(&self) -> &Path {
        &self.workspace_dir
    }

    pub fn inbox_dir(&self) -> &Path {
        &self.inbox_dir
    }

    pub fn outbox_dir(&self) -> &Path {
        &self.outbox_dir
    }

    /// Resolve a relative path inside the Muse runtime mounts (if attached and matched)
    /// or inside the bot root (`workspace/` by default unless prefixed with `inbox/` or `outbox/`).
    fn resolve_confined_path(
        &self,
        rel_or_abs: &str,
        is_write: bool,
        persona_write_approved: bool,
        audit: Option<&Arc<AuditLogger>>,
    ) -> Result<ResolvedWorkspacePath> {
        if let Some(ref muse) = self.muse_paths
            && muse.matches_explicit_mount(rel_or_abs)
        {
            let resolved = match muse.resolve_virtual_path(rel_or_abs) {
                Ok(r) => r,
                Err(err) => {
                    if let Some(logger) = audit {
                        logger.log_governance(
                            if is_write {
                                "workspace_write_refused"
                            } else {
                                "workspace_read_refused"
                            },
                            Some(&self.bot_id),
                            AuditCategory::Refused,
                            "RULE-PATH-002",
                            &err.to_string(),
                            format!("Refused virtual path '{rel_or_abs}'"),
                            serde_json::json!({ "path": rel_or_abs }),
                        );
                    }
                    return Err(err);
                }
            };

            let decision =
                self.gateway
                    .evaluate_scoped_path(&resolved, is_write, persona_write_approved);

            return match decision.outcome {
                PolicyOutcome::Refused => {
                    if let Some(logger) = audit {
                        logger.log_governance(
                            if is_write {
                                "workspace_write_refused"
                            } else {
                                "workspace_read_refused"
                            },
                            Some(&self.bot_id),
                            AuditCategory::Refused,
                            decision.rule_id,
                            &decision.citation,
                            format!(
                                "Refused scoped mount '{}' path '{}'",
                                resolved.mount_label, rel_or_abs
                            ),
                            serde_json::json!({
                                "path": rel_or_abs,
                                "mount": resolved.mount_label,
                            }),
                        );
                    }
                    Err(GmError::SafetyViolation(format!(
                        "[{}] {}",
                        decision.rule_id, decision.citation
                    )))
                }
                PolicyOutcome::NeedsApproval => Ok(ResolvedWorkspacePath::NeedsPersonaApproval {
                    resolved,
                    rule_id: decision.rule_id,
                    citation: decision.citation,
                }),
                PolicyOutcome::Permitted => Ok(ResolvedWorkspacePath::Muse(resolved)),
            };
        }

        let candidate = Path::new(rel_or_abs);
        let mapped = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else if rel_or_abs.starts_with("workspace/")
            || rel_or_abs.starts_with("inbox/")
            || rel_or_abs.starts_with("outbox/")
            || rel_or_abs == "workspace"
            || rel_or_abs == "inbox"
            || rel_or_abs == "outbox"
        {
            self.bot_root.join(candidate)
        } else {
            self.workspace_dir.join(candidate)
        };

        let decision = self
            .gateway
            .evaluate_workspace_path(&self.bot_root, &mapped, is_write);

        if decision.outcome == PolicyOutcome::Refused {
            if let Some(logger) = audit {
                logger.log_governance(
                    if is_write {
                        "workspace_write_refused"
                    } else {
                        "workspace_read_refused"
                    },
                    Some(&self.bot_id),
                    AuditCategory::Refused,
                    decision.rule_id,
                    &decision.citation,
                    format!("Refused path '{rel_or_abs}'"),
                    serde_json::json!({ "path": rel_or_abs }),
                );
            }
            return Err(GmError::SafetyViolation(format!(
                "[{}] {}",
                decision.rule_id, decision.citation
            )));
        }

        // Write operations additionally may not write into `inbox/` (reserved for incoming A2A artifacts)
        if is_write && mapped.starts_with(&self.inbox_dir) {
            let msg = format!(
                "[RULE-PATH-002] Bot '{}' cannot overwrite read-only inbox path '{}'",
                self.bot_id, rel_or_abs
            );
            if let Some(logger) = audit {
                logger.log_governance(
                    "workspace_write_refused",
                    Some(&self.bot_id),
                    AuditCategory::Refused,
                    "RULE-PATH-002",
                    &msg,
                    format!("Refused inbox write '{rel_or_abs}'"),
                    serde_json::json!({ "path": rel_or_abs }),
                );
            }
            return Err(GmError::SafetyViolation(msg));
        }

        Ok(ResolvedWorkspacePath::Legacy(mapped))
    }

    fn file_handle(
        &self,
        target: &ResolvedWorkspacePath,
        is_write: bool,
    ) -> Result<(Arc<Dir>, PathBuf)> {
        match target {
            ResolvedWorkspacePath::Legacy(path) => {
                for (root, handle) in [
                    (&self.workspace_dir, &self.workspace_cap),
                    (&self.inbox_dir, &self.inbox_cap),
                    (&self.outbox_dir, &self.outbox_cap),
                ] {
                    if let Ok(relative) = path.strip_prefix(root) {
                        if relative.components().any(|c| {
                            matches!(
                                c,
                                std::path::Component::ParentDir
                                    | std::path::Component::RootDir
                                    | std::path::Component::Prefix(_)
                            )
                        }) {
                            return Err(GmError::SafetyViolation(
                                "Parent traversal is not allowed in workspace paths".into(),
                            ));
                        }
                        return Ok((
                            handle.clone(),
                            if relative.as_os_str().is_empty() {
                                PathBuf::from(".")
                            } else {
                                relative.to_path_buf()
                            },
                        ));
                    }
                }
                Err(GmError::SafetyViolation(
                    "Path is outside workspace, inbox and outbox grants".into(),
                ))
            }
            ResolvedWorkspacePath::Muse(resolved) => {
                if resolved.relative.components().any(|c| {
                    matches!(
                        c,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                }) {
                    return Err(GmError::SafetyViolation(
                        "Parent traversal is not allowed in scoped mount paths".into(),
                    ));
                }
                if is_write {
                    gm_core::storage::ensure_private_dir(&resolved.mount_root)?;
                }
                let dir = Arc::new(Dir::open_ambient_dir(
                    &resolved.mount_root,
                    cap_std::ambient_authority(),
                )?);
                let relative = if resolved.relative.as_os_str().is_empty() {
                    PathBuf::from(".")
                } else {
                    resolved.relative.clone()
                };
                Ok((dir, relative))
            }
            ResolvedWorkspacePath::NeedsPersonaApproval {
                rule_id, citation, ..
            } => Err(GmError::SafetyViolation(format!(
                "[{rule_id}] {citation} (needs confirmation)"
            ))),
        }
    }

    pub async fn execute_tool(
        &self,
        task_id: &str,
        step: u32,
        call: &WorkspaceToolCall,
        interaction_handler: &Arc<dyn InteractionHandler>,
        kill_switch: &KillSwitch,
        audit: Option<&Arc<AuditLogger>>,
    ) -> Result<WorkspaceToolOutcome> {
        if kill_switch.is_triggered() {
            return Err(GmError::SafetyViolation(
                "Execution stopped by Kill Switch".into(),
            ));
        }
        match call {
            WorkspaceToolCall::ReadFile { path } => {
                let target = self.resolve_confined_path(path, false, false, audit)?;
                let (handle, relative) = self.file_handle(&target, false)?;
                if handle.metadata(&relative)?.len() > 1024 * 1024 {
                    return Err(GmError::Config("File exceeds the 1 MiB tool limit".into()));
                }
                use std::io::Read;
                let mut bytes = Vec::new();
                handle
                    .open(&relative)?
                    .take(1024 * 1024 + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() > 1024 * 1024 {
                    return Err(GmError::Config("File exceeds the 1 MiB tool limit".into()));
                }
                let content = String::from_utf8(bytes)
                    .map_err(|_| GmError::Config("File is not valid UTF-8 text".into()))?;
                let scrubbed = self.scrubber.scrub_str(&content);
                if let Some(logger) = audit {
                    logger.log_governance(
                        "workspace_read",
                        Some(&self.bot_id),
                        AuditCategory::Permitted,
                        "RULE-STD-000",
                        "Confined workspace file read permitted",
                        format!("ReadFile({path})"),
                        serde_json::json!({ "task_id": task_id, "step": step, "path": path, "bytes": scrubbed.len() }),
                    );
                }
                Ok(WorkspaceToolOutcome {
                    summary: format!("ReadFile({path}) -> {} bytes", scrubbed.len()),
                    output: scrubbed,
                    diff_preview: None,
                })
            }

            WorkspaceToolCall::WriteFile { path, content } => {
                if content.len() > 1024 * 1024 {
                    return Err(GmError::Config("File exceeds the 1 MiB tool limit".into()));
                }
                let mut target = self.resolve_confined_path(path, true, false, audit)?;
                if let ResolvedWorkspacePath::NeedsPersonaApproval {
                    resolved,
                    rule_id,
                    citation,
                } = target
                {
                    let req = ApprovalRequest {
                        task_id: task_id.to_string(),
                        step,
                        action: ComputerAction::Type {
                            text: format!("Write {} ({} bytes)", path, content.len()),
                        },
                        target_label: Some(format!(
                            "PersonaFile (bot={}, mount={})",
                            self.bot_id, resolved.mount_label
                        )),
                        reason: format!("[{rule_id}] {citation}"),
                    };
                    let approved = interaction_handler.request_approval(&req).await?;
                    if !approved {
                        if let Some(logger) = audit {
                            logger.log_governance(
                                "workspace_write_refused",
                                Some(&self.bot_id),
                                AuditCategory::Refused,
                                rule_id,
                                &citation,
                                format!("Persona write needs confirmation for '{path}'"),
                                serde_json::json!({
                                    "task_id": task_id,
                                    "step": step,
                                    "path": path,
                                    "mount": resolved.mount_label,
                                }),
                            );
                        }
                        return Err(GmError::SafetyViolation(format!(
                            "[{rule_id}] {citation} (needs confirmation)"
                        )));
                    }
                    target = self.resolve_confined_path(path, true, true, audit)?;
                }

                let is_persona = matches!(
                    &target,
                    ResolvedWorkspacePath::Muse(r) if r.access == MountAccess::PersonaApproval
                );
                let (handle, relative) = self.file_handle(&target, true)?;
                if let Some(parent) = relative.parent()
                    && !parent.as_os_str().is_empty()
                {
                    handle.create_dir_all(parent)?;
                }
                let previous = handle.read_to_string(&relative).ok();
                let diff = compute_unified_diff_preview(path, previous.as_deref(), content, 200);
                {
                    use std::io::Write;
                    let mut options = cap_std::fs::OpenOptions::new();
                    options.write(true).create(true).truncate(true);
                    #[cfg(unix)]
                    {
                        use cap_std::fs::OpenOptionsExt;
                        options.mode(0o600);
                    }
                    let mut file = handle.open_with(&relative, &options)?;
                    file.write_all(content.as_bytes())?;
                    file.sync_all()?;
                }
                let scrubbed_diff = self.scrubber.scrub_str(&diff);
                if let Some(logger) = audit {
                    let rule_id = if is_persona {
                        "RULE-PERSONA-007"
                    } else {
                        "RULE-STD-000"
                    };
                    logger.log_governance(
                        "workspace_write",
                        Some(&self.bot_id),
                        AuditCategory::Permitted,
                        rule_id,
                        if is_persona {
                            "Operator-confirmed persona file write permitted"
                        } else {
                            "Confined workspace file write permitted"
                        },
                        format!("WriteFile({path})"),
                        serde_json::json!({
                            "task_id": task_id,
                            "step": step,
                            "path": path,
                            "bytes": content.len(),
                            "diff_preview": scrubbed_diff,
                        }),
                    );
                }
                Ok(WorkspaceToolOutcome {
                    summary: format!("WriteFile({path}) -> {} bytes written", content.len()),
                    output: format!("Wrote {} bytes to {path}\n{scrubbed_diff}", content.len()),
                    diff_preview: Some(scrubbed_diff),
                })
            }

            WorkspaceToolCall::ListFiles { path } => {
                let rel = path.as_deref().unwrap_or(".");
                let target = self.resolve_confined_path(rel, false, false, audit)?;
                let mut names = Vec::new();
                let (handle, relative) = self.file_handle(&target, false)?;
                if handle.metadata(&relative)?.is_dir() {
                    for entry in handle.read_dir(&relative)?.take(1000) {
                        let entry = entry?;
                        let file_name = entry.file_name().to_string_lossy().to_string();
                        let suffix = if entry.file_type()?.is_dir() { "/" } else { "" };
                        names.push(format!("{file_name}{suffix}"));
                    }
                    names.sort();
                }
                let listing = names.join("\n");
                if let Some(logger) = audit {
                    logger.log_governance(
                        "workspace_list",
                        Some(&self.bot_id),
                        AuditCategory::Permitted,
                        "RULE-STD-000",
                        "Confined workspace directory listing permitted",
                        format!("ListFiles({rel})"),
                        serde_json::json!({ "task_id": task_id, "step": step, "count": names.len() }),
                    );
                }
                Ok(WorkspaceToolOutcome {
                    summary: format!("ListFiles({rel}) -> {} entries", names.len()),
                    output: listing,
                    diff_preview: None,
                })
            }

            WorkspaceToolCall::Shell {
                command,
                timeout_ms,
            } => {
                let decision = self.gateway.evaluate_shell_command(command);
                match decision.outcome {
                    PolicyOutcome::Refused => {
                        if let Some(logger) = audit {
                            logger.log_governance(
                                "workspace_shell_refused",
                                Some(&self.bot_id),
                                AuditCategory::Refused,
                                decision.rule_id,
                                &decision.citation,
                                format!(
                                    "Refused shell command: {}",
                                    self.scrubber.scrub_str(command)
                                ),
                                serde_json::json!({ "task_id": task_id, "step": step }),
                            );
                        }
                        return Err(GmError::SafetyViolation(format!(
                            "[{}] {}",
                            decision.rule_id, decision.citation
                        )));
                    }
                    PolicyOutcome::NeedsApproval => {
                        let req = ApprovalRequest {
                            task_id: task_id.to_string(),
                            step,
                            action: ComputerAction::Type {
                                text: self.scrubber.scrub_str(command),
                            },
                            target_label: Some(format!("WorkspaceShell (bot={})", self.bot_id)),
                            reason: format!("[{}] {}", decision.rule_id, decision.citation),
                        };
                        let approved = interaction_handler.request_approval(&req).await?;
                        if !approved {
                            if let Some(logger) = audit {
                                logger.log_governance(
                                    "workspace_shell_denied",
                                    Some(&self.bot_id),
                                    AuditCategory::Refused,
                                    decision.rule_id,
                                    &decision.citation,
                                    format!(
                                        "Operator denied shell command: {}",
                                        self.scrubber.scrub_str(command)
                                    ),
                                    serde_json::json!({ "task_id": task_id, "step": step }),
                                );
                            }
                            return Err(GmError::SafetyViolation(format!(
                                "[{}] Operator denied shell command",
                                decision.rule_id
                            )));
                        }
                    }
                    PolicyOutcome::Permitted => {}
                }

                let timeout =
                    Duration::from_millis(timeout_ms.unwrap_or(15_000).clamp(100, 120_000));
                let vm = self.vm_config.clone();
                let outcome = crate::vm::execute(
                    vm,
                    &self.workspace_dir,
                    command,
                    timeout.as_millis() as u64,
                )
                .await?;
                let mut output = outcome.output;
                if !outcome.artifacts.is_empty() {
                    use std::io::Write;
                    let slot = format!("vm-{}", uuid::Uuid::new_v4());
                    self.outbox_cap.create_dir(&slot)?;
                    let directory = self.outbox_cap.open_dir(&slot)?;
                    for artifact in outcome.artifacts {
                        let path = Path::new(&artifact.path);
                        if let Some(parent) = path.parent() {
                            directory.create_dir_all(parent)?;
                        }
                        let mut options = cap_std::fs::OpenOptions::new();
                        options.write(true).create_new(true);
                        #[cfg(unix)]
                        {
                            use cap_std::fs::OpenOptionsExt;
                            options.mode(0o600);
                        }
                        let mut file = directory.open_with(path, &options)?;
                        file.write_all(&artifact.bytes)?;
                        file.sync_all()?;
                        output.push_str(&format!(
                            "\nReview artifact: outbox/{slot}/{} ({} bytes)",
                            artifact.path,
                            artifact.bytes.len()
                        ));
                    }
                }
                Ok(WorkspaceToolOutcome {
                    summary: format!("Workspace VM command exited with {}", outcome.exit_code),
                    output: self.scrubber.scrub_str(&output),
                    diff_preview: None,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::NonInteractiveHandler;

    #[tokio::test]
    async fn test_workspace_read_write_diff_and_path_confinement() {
        let dir = std::env::temp_dir().join(format!("bot_ws_test_{}", uuid::Uuid::new_v4()));
        let sandbox = BotWorkspaceSandbox::new("coder", &dir).unwrap();
        let handler: Arc<dyn InteractionHandler> = Arc::new(NonInteractiveHandler);
        let ks = KillSwitch::new();
        let audit = Arc::new(AuditLogger::in_memory());

        // 1. First write generates new-file diff preview
        let w1 = sandbox
            .execute_tool(
                "t1",
                1,
                &WorkspaceToolCall::WriteFile {
                    path: "hello.txt".into(),
                    content: "line 1\nline 2\n".into(),
                },
                &handler,
                &ks,
                Some(&audit),
            )
            .await
            .unwrap();
        assert!(w1.diff_preview.as_deref().unwrap().contains("new file"));

        // 2. Second write generates unified line diff
        let w2 = sandbox
            .execute_tool(
                "t1",
                2,
                &WorkspaceToolCall::WriteFile {
                    path: "hello.txt".into(),
                    content: "line 1\nline 2 updated\nline 3\n".into(),
                },
                &handler,
                &ks,
                Some(&audit),
            )
            .await
            .unwrap();
        let diff = w2.diff_preview.unwrap();
        assert!(diff.contains("-line 2"));
        assert!(diff.contains("+line 2 updated"));

        // 3. Escape attempt (`../../outside.txt`) is refused with RULE-PATH-002
        let escape = sandbox
            .execute_tool(
                "t1",
                3,
                &WorkspaceToolCall::WriteFile {
                    path: "../../outside.txt".into(),
                    content: "pwned".into(),
                },
                &handler,
                &ks,
                Some(&audit),
            )
            .await;
        assert!(escape.is_err());
        assert!(escape.unwrap_err().to_string().contains("RULE-PATH-002"));

        let refused =
            audit.query_governance_entries(Some(AuditCategory::Refused), Some("coder"), 10);
        assert_eq!(refused.len(), 1);
        assert_eq!(refused[0].rule_id, "RULE-PATH-002");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn shell_without_a_verified_vm_never_uses_the_host() {
        let dir = std::env::temp_dir().join(format!("gm-no-host-shell-{}", uuid::Uuid::new_v4()));
        let sandbox = BotWorkspaceSandbox::new("coder", &dir).unwrap();
        let handler: Arc<dyn InteractionHandler> = Arc::new(NonInteractiveHandler);
        let error = sandbox
            .execute_tool(
                "t",
                1,
                &WorkspaceToolCall::Shell {
                    command: "printf never-executed".into(),
                    timeout_ms: Some(1000),
                },
                &handler,
                &KillSwitch::new(),
                None,
            )
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Host shell fallback is disabled")
        );
        drop(sandbox);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn muse_runtime_paths_enforce_readonly_shared_write_and_persona_approval() {
        use crate::approval::CallbackInteractionHandler;

        let base = std::env::temp_dir().join(format!("bot_muse_ws_{}", uuid::Uuid::new_v4()));
        let storage = base.join("storage");
        let bot_root = storage.join("bots/muse");
        gm_core::storage::ensure_private_dir(&storage).unwrap();

        let shared = MuseRuntimePaths::shared(&storage);
        let sandbox = BotWorkspaceSandbox::new("muse", &bot_root)
            .unwrap()
            .with_shared_runtime_paths(shared.clone());

        let non_interactive: Arc<dyn InteractionHandler> = Arc::new(NonInteractiveHandler);
        let ks = KillSwitch::new();
        let audit = Arc::new(AuditLogger::in_memory());

        // 1. Writing to ReadOnly ~/skills/x is refused with RULE-PATH-002
        let readonly_err = sandbox
            .execute_tool(
                "t-muse",
                1,
                &WorkspaceToolCall::WriteFile {
                    path: "~/skills/x/SKILL.md".into(),
                    content: "tampered".into(),
                },
                &non_interactive,
                &ks,
                Some(&audit),
            )
            .await
            .unwrap_err();
        assert!(readonly_err.to_string().contains("RULE-PATH-002"));

        // 2. Writing to ~/MEMORY.md succeeds and targets shared storage
        let mem_write = sandbox
            .execute_tool(
                "t-muse",
                2,
                &WorkspaceToolCall::WriteFile {
                    path: "~/MEMORY.md".into(),
                    content: "# MEMORY.md\nUser prefers Rust.\n".into(),
                },
                &non_interactive,
                &ks,
                Some(&audit),
            )
            .await
            .unwrap();
        assert!(mem_write.summary.contains("bytes written"));
        assert!(shared.memory_md().exists());

        // 3. Reading ~/MEMORY.md returns content
        let mem_read = sandbox
            .execute_tool(
                "t-muse",
                3,
                &WorkspaceToolCall::ReadFile {
                    path: "~/MEMORY.md".into(),
                },
                &non_interactive,
                &ks,
                Some(&audit),
            )
            .await
            .unwrap();
        assert!(mem_read.output.contains("User prefers Rust."));

        // 4. Writing to SOUL.md without interactive approval returns needs confirmation
        let soul_denied = sandbox
            .execute_tool(
                "t-muse",
                4,
                &WorkspaceToolCall::WriteFile {
                    path: "SOUL.md".into(),
                    content: "Edited soul".into(),
                },
                &non_interactive,
                &ks,
                Some(&audit),
            )
            .await
            .unwrap_err();
        let err_msg = soul_denied.to_string();
        assert!(err_msg.contains("RULE-PERSONA-007"));
        assert!(err_msg.contains("needs confirmation"));

        // 5. Writing to SOUL.md with explicit approval prompts once and succeeds
        let cb = Arc::new(CallbackInteractionHandler::with_approval_always(true));
        let approved_handler: Arc<dyn InteractionHandler> = cb.clone();
        let soul_ok = sandbox
            .execute_tool(
                "t-muse",
                5,
                &WorkspaceToolCall::WriteFile {
                    path: "SOUL.md".into(),
                    content: "Approved soul update\n".into(),
                },
                &approved_handler,
                &ks,
                Some(&audit),
            )
            .await
            .unwrap();
        assert!(soul_ok.summary.contains("bytes written"));
        assert_eq!(cb.requested_approvals().len(), 1);
        assert_eq!(
            std::fs::read_to_string(bot_root.join("SOUL.md")).unwrap(),
            "Approved soul update\n"
        );

        let refused =
            audit.query_governance_entries(Some(AuditCategory::Refused), Some("muse"), 20);
        assert_eq!(refused.len(), 2);

        let _ = std::fs::remove_dir_all(&base);
    }
}
