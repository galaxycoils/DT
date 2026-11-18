//! Shared coordinator for interactive approvals, secret prompts, multiple-choice questions,
//! Teach Task demonstrations, UI settings merging/persistence, custom connector probing, and
//! live computer preview capture shared across IPC (`DaemonControlServer`) and Web (`CompanionWebServer`).

use crate::connectors::ConnectorTransport;
use crate::supervisor::OpenBotSupervisor;
use chrono::Utc;
use gm_core::config::GmConfig;
use gm_core::error::{GmError, Result};
use gm_core::ipc::DaemonEvent;
use gm_core::muse_paths::MuseRuntimePaths;
use gm_muse::skills::{SkillRegistry, SkillYaml};
use gm_platform::Platform;
use gm_safety::SecretScrubber;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex as AsyncMutex, oneshot};

pub const TEACH_TASK_MAX_DURATION: Duration = Duration::from_secs(10 * 60);
pub const MAX_TEACH_STEPS: usize = 200;

fn scrub_teach_text(scrubber: &SecretScrubber, raw: &str) -> String {
    static TOKEN_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = TOKEN_RE.get_or_init(|| {
        regex::Regex::new(
            r"(?x)
            \b(?:
                ghp_[A-Za-z0-9]{10,}
                |github_pat_[A-Za-z0-9_]{10,}
                |glpat-[A-Za-z0-9_-]{10,}
                |sk-(?:ant-|proj-|live-|test-)?[A-Za-z0-9_-]{12,}
                |xai-[A-Za-z0-9_-]{12,}
                |AKIA[0-9A-Z]{16}
                |AIza[0-9A-Za-z_-]{20,}
                |xox[baprs]-[A-Za-z0-9-]{10,}
            )\b
            |-----BEGIN\s+[A-Z\s]*PRIVATE\s+KEY-----
            |\bBearer\s+[A-Za-z0-9._-]{12,}
            ",
        )
        .expect("valid teach scrubber regex")
    });
    let first_pass = scrubber.scrub_str(raw);
    re.replace_all(&first_pass, "secret://keychain/redacted")
        .into_owned()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeachTaskSession {
    pub bot_id: String,
    pub started_at_rfc3339: String,
    #[serde(skip, default = "Instant::now")]
    pub started_instant: Instant,
    pub steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeachTaskOutcome {
    pub bot_id: String,
    pub started_at: String,
    pub duration_secs: u64,
    pub step_count: usize,
    pub draft_markdown: String,
    pub saved_skill_dir: Option<String>,
}

type PendingSecrets =
    Arc<AsyncMutex<HashMap<String, oneshot::Sender<Option<Vec<(String, String)>>>>>>;

/// Shared synchronization coordinator for interactive UI requests and Teach Task recording.
#[derive(Clone, Default)]
pub struct SharedInteractionState {
    pending_approvals: Arc<AsyncMutex<HashMap<String, oneshot::Sender<bool>>>>,
    pending_secrets: PendingSecrets,
    pending_questions: Arc<AsyncMutex<HashMap<String, oneshot::Sender<Option<String>>>>>,
    teach_task: Arc<Mutex<Option<TeachTaskSession>>>,
}

impl SharedInteractionState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pending_approvals(&self) -> Arc<AsyncMutex<HashMap<String, oneshot::Sender<bool>>>> {
        Arc::clone(&self.pending_approvals)
    }

    pub fn pending_secrets(&self) -> PendingSecrets {
        Arc::clone(&self.pending_secrets)
    }

    pub fn pending_questions(
        &self,
    ) -> Arc<AsyncMutex<HashMap<String, oneshot::Sender<Option<String>>>>> {
        Arc::clone(&self.pending_questions)
    }

    pub async fn register_approval(&self, request_id: String, tx: oneshot::Sender<bool>) {
        self.pending_approvals.lock().await.insert(request_id, tx);
    }

    pub async fn resolve_approval(&self, request_id: &str, approved: bool) -> bool {
        let tx = self.pending_approvals.lock().await.remove(request_id);
        match tx {
            Some(sender) => {
                let _ = sender.send(approved);
                true
            }
            None => false,
        }
    }

    pub async fn register_secret(
        &self,
        request_id: String,
        tx: oneshot::Sender<Option<Vec<(String, String)>>>,
    ) {
        self.pending_secrets.lock().await.insert(request_id, tx);
    }

    pub async fn resolve_secret(
        &self,
        request_id: &str,
        fields: Option<Vec<(String, String)>>,
    ) -> bool {
        let tx = self.pending_secrets.lock().await.remove(request_id);
        match tx {
            Some(sender) => {
                let _ = sender.send(fields);
                true
            }
            None => false,
        }
    }

    pub async fn register_question(&self, request_id: String, tx: oneshot::Sender<Option<String>>) {
        self.pending_questions.lock().await.insert(request_id, tx);
    }

    pub async fn resolve_question(&self, request_id: &str, answer: Option<String>) -> bool {
        let tx = self.pending_questions.lock().await.remove(request_id);
        match tx {
            Some(sender) => {
                let _ = sender.send(answer);
                true
            }
            None => false,
        }
    }

    pub fn start_teach_task(&self, bot_id: &str) -> Result<TeachTaskSession> {
        let clean_bot = bot_id.trim().to_ascii_lowercase();
        if clean_bot.is_empty() {
            return Err(GmError::Config(
                "bot_id is required to start Teach Task".into(),
            ));
        }
        let mut guard = self.teach_task.lock();
        if let Some(ref existing) = *guard
            && existing.started_instant.elapsed() < TEACH_TASK_MAX_DURATION
        {
            return Err(GmError::Config(format!(
                "Teach Task already active for bot '{}'",
                existing.bot_id
            )));
        }
        let session = TeachTaskSession {
            bot_id: clean_bot,
            started_at_rfc3339: Utc::now().to_rfc3339(),
            started_instant: Instant::now(),
            steps: Vec::new(),
        };
        *guard = Some(session.clone());
        Ok(session)
    }

    pub fn record_teach_step(&self, raw_step: &str) -> Result<()> {
        let mut guard = self.teach_task.lock();
        let Some(ref mut session) = *guard else {
            return Ok(());
        };
        if session.started_instant.elapsed() >= TEACH_TASK_MAX_DURATION {
            *guard = None;
            return Ok(());
        }
        if session.steps.len() >= MAX_TEACH_STEPS {
            return Ok(());
        }
        let scrubbed = scrub_teach_text(&SecretScrubber::new(), raw_step.trim());
        if !scrubbed.is_empty() {
            session.steps.push(scrubbed);
        }
        Ok(())
    }

    pub fn active_teach_task(&self) -> Option<TeachTaskSession> {
        let mut guard = self.teach_task.lock();
        if let Some(ref session) = *guard
            && session.started_instant.elapsed() >= TEACH_TASK_MAX_DURATION
        {
            *guard = None;
            return None;
        }
        guard.clone()
    }

    pub fn stop_teach_task(
        &self,
        shared_paths: &MuseRuntimePaths,
        save_skill_name: Option<&str>,
    ) -> Result<TeachTaskOutcome> {
        let session = self
            .teach_task
            .lock()
            .take()
            .ok_or_else(|| GmError::Config("No active Teach Task recording session".into()))?;
        let duration_secs = session.started_instant.elapsed().as_secs();
        let scrubber = SecretScrubber::new();
        let mut body = String::new();
        body.push_str("# Demonstrated Workflow\n\n");
        body.push_str(&format!(
            "- **Bot**: `{}`\n- **Recorded**: `{}` ({}s)\n\n## Observed Steps\n",
            session.bot_id, session.started_at_rfc3339, duration_secs
        ));
        if session.steps.is_empty() {
            body.push_str("1. Open target workflow cleanly and verify outcome.\n");
        } else {
            for (idx, step) in session.steps.iter().enumerate() {
                body.push_str(&format!(
                    "{}. {}\n",
                    idx + 1,
                    scrub_teach_text(&scrubber, step)
                ));
            }
        }
        body.push_str("\n## Verification\n- Confirm expected output state before finishing.\n");

        let saved_skill_dir =
            if let Some(raw_name) = save_skill_name.map(str::trim).filter(|s| !s.is_empty()) {
                let clean_name: String = raw_name
                    .to_ascii_lowercase()
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '-' {
                            c
                        } else {
                            '-'
                        }
                    })
                    .collect();
                let clean_name = clean_name.trim_matches('-').to_string();
                let mut reg = SkillRegistry::open_with_scrubber(shared_paths, scrubber)?;
                let manifest = SkillYaml {
                    name: clean_name.clone(),
                    title: format!("Custom Task: {raw_name}"),
                    description: "Workflow recorded via Teach Task".into(),
                    category: "custom".into(),
                    include_in_prompt: true,
                    skill_improvement: None,
                    amends: None,
                };
                let dir = reg.scaffold_custom_skill(manifest, &body)?;
                Some(dir.to_string_lossy().to_string())
            } else {
                None
            };

        Ok(TeachTaskOutcome {
            bot_id: session.bot_id,
            started_at: session.started_at_rfc3339,
            duration_secs,
            step_count: session.steps.len(),
            draft_markdown: body,
            saved_skill_dir,
        })
    }
}

pub fn get_ui_settings_snapshot(cfg: &GmConfig, sup: &OpenBotSupervisor) -> serde_json::Value {
    let prov = sup.providers_config();
    serde_json::json!({
        "ui": sup.ui_config(),
        "allow_host_desktop": sup.allow_host_desktop(),
        "allow_secret_injection": cfg.safety.allow_secret_injection,
        "providers": prov,
        "budget": {
            "daily_spend_cap_usd": cfg.budget.daily_spend_cap_usd,
            "per_job_spend_cap_usd": cfg.budget.per_job_spend_cap_usd,
            "max_daily_notifications": cfg.budget.max_daily_notifications,
        },
    })
}

/// Validates and atomically persists `sup.providers_config()` into `shared_config` and `config_path`.
pub fn persist_providers_config(
    shared_config: &Arc<RwLock<GmConfig>>,
    config_path: Option<&Path>,
    sup: &OpenBotSupervisor,
) -> Result<gm_core::config::ProvidersConfig> {
    sup.bind_provider_persistence(shared_config, config_path);
    let mut shared = shared_config.write();
    let providers = sup.providers_config();
    let mut next = shared.clone();
    next.providers = providers.clone();
    next.validate()?;
    if let Some(path) = config_path {
        next.save(path)?;
    }
    *shared = next;
    Ok(providers)
}

/// Validates, merges, and persists UI + non-secret Settings updates atomically to `config_path`
/// (when provided) and propagates them immediately to `OpenBotSupervisor`.
pub fn apply_and_save_ui_settings(
    shared_config: &Arc<RwLock<GmConfig>>,
    config_path: Option<&Path>,
    sup: &OpenBotSupervisor,
    patch: &serde_json::Value,
) -> Result<serde_json::Value> {
    let mut guard = shared_config.write();
    let mut next = guard.clone();
    next.providers = sup.providers_config();
    next.ui = sup.ui_config();
    next.safety.allow_host_desktop = sup.allow_host_desktop();

    let ui_src = patch.get("ui").unwrap_or(patch);
    if let Some(obj) = ui_src.as_object() {
        let mut merged_val = serde_json::to_value(&next.ui)?;
        if let Some(dst) = merged_val.as_object_mut() {
            for (k, v) in obj {
                if dst.contains_key(k) {
                    dst.insert(k.clone(), v.clone());
                }
            }
        }
        next.ui = serde_json::from_value(merged_val)?;
    }

    if let Some(v) = patch.get("allow_host_desktop").and_then(|v| v.as_bool()) {
        next.safety.allow_host_desktop = v;
    }
    if let Some(v) = patch
        .get("allow_secret_injection")
        .and_then(|v| v.as_bool())
    {
        next.safety.allow_secret_injection = v;
    }
    if let Some(b) = patch.get("budget").and_then(|v| v.as_object()) {
        if let Some(v) = b.get("daily_spend_cap_usd").and_then(|v| v.as_f64()) {
            next.budget.daily_spend_cap_usd = v;
        }
        if let Some(v) = b.get("per_job_spend_cap_usd").and_then(|v| v.as_f64()) {
            next.budget.per_job_spend_cap_usd = v;
        }
        if let Some(v) = b.get("max_daily_notifications").and_then(|v| v.as_u64()) {
            next.budget.max_daily_notifications = v.min(u32::MAX as u64) as u32;
        }
    }
    if let Some(p) = patch.get("providers").and_then(|v| v.as_object()) {
        if let Some(v) = p.get("meta_api_base").and_then(|v| v.as_str()) {
            next.providers.meta_api_base = v.trim().to_string();
        }
        if let Some(v) = p.get("meta_cu_model").and_then(|v| v.as_str()) {
            next.providers.meta_cu_model = v.trim().to_string();
        }
        if let Some(v) = p.get("meta_reasoning_model").and_then(|v| v.as_str()) {
            next.providers.meta_reasoning_model = v.trim().to_string();
        }
        if let Some(v) = p.get("xai_api_base").and_then(|v| v.as_str()) {
            next.providers.xai_api_base = v.trim().to_string();
        }
        if let Some(v) = p.get("xai_model").and_then(|v| v.as_str()) {
            next.providers.xai_model = v.trim().to_string();
        }
        if let Some(v) = p
            .get("allow_screenshot_egress_meta")
            .and_then(|v| v.as_bool())
        {
            next.providers.allow_screenshot_egress_meta = v;
        }
        if let Some(v) = p
            .get("allow_screenshot_egress_xai")
            .and_then(|v| v.as_bool())
        {
            next.providers.allow_screenshot_egress_xai = v;
        }
        if let Some(v) = p.get("workspace_default_profile_id") {
            next.providers.workspace_default_profile_id = v
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string);
        }
        if let Some(v) = p.get("account_default_profile_id") {
            next.providers.account_default_profile_id = v
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string);
        }
    }

    next.validate()?;
    sup.set_providers_config(next.providers.clone());
    if let Some(providers) = patch.get("providers") {
        let workspace = providers
            .get("workspace_default_profile_id")
            .map(|v| v.as_str().unwrap_or_default().to_string());
        let account = providers
            .get("account_default_profile_id")
            .map(|v| v.as_str().unwrap_or_default().to_string());
        if workspace.is_some() || account.is_some() {
            sup.set_default_provider_profiles(workspace, account)?;
        }
    }
    next.providers = sup.providers_config();
    if let Some(path) = config_path {
        next.save(path)?;
    }
    sup.set_ui_config(next.ui.clone());
    sup.set_allow_host_desktop(next.safety.allow_host_desktop);
    *guard = next.clone();

    let _ = sup
        .daemon_event_sender()
        .send(DaemonEvent::ActivityUpdated {
            reason: "ui_settings_updated".into(),
        });
    sup.emit_agui(
        "UI_SETTINGS_UPDATED",
        "ui",
        "",
        serde_json::json!({
            "theme": next.ui.theme,
            "accent_mode": next.ui.accent_mode,
        }),
    );

    Ok(get_ui_settings_snapshot(&next, sup))
}

pub async fn capture_preview_payload<P: Platform + ?Sized>(
    platform: &P,
    max_long_edge: Option<u32>,
    quality: Option<u8>,
    sup: Option<&OpenBotSupervisor>,
) -> Result<serde_json::Value> {
    if let Some(s) = sup
        && (!s.allow_host_desktop() || s.ui_config().computer_execution_mode == "never")
    {
        return Err(GmError::PermissionDenied(
            "Live Computer preview requires Host Desktop opt-in and execution mode != 'never'"
                .into(),
        ));
    }
    let q = quality.unwrap_or(65).clamp(15, 95);
    let edge = Some(max_long_edge.unwrap_or(960).clamp(160, 1920));
    let frame = platform.capture(edge, Some(q)).await?;
    let raw = gm_platform::shm::resolve_frame_bytes(&frame)?;
    let jpg_bytes = if raw.starts_with(&[0xFF, 0xD8]) {
        raw
    } else {
        let img = image::load_from_memory(&raw)
            .map_err(|e| GmError::Platform(format!("Frame decode failed: {e}")))?;
        let mut out = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, q);
        img.write_with_encoder(encoder)
            .map_err(|e| GmError::Platform(format!("JPEG encode failed: {e}")))?;
        out
    };
    use base64::Engine as _;
    let jpeg_base64 = base64::engine::general_purpose::STANDARD.encode(&jpg_bytes);
    if let Some(s) = sup {
        let _ = s
            .daemon_event_sender()
            .send(DaemonEvent::ComputerFrameReady {
                width: frame.width,
                height: frame.height,
                jpeg_base64: jpeg_base64.clone(),
                timestamp_ms: frame.timestamp_ms,
            });
    }
    Ok(serde_json::json!({
        "width": frame.width,
        "height": frame.height,
        "jpeg_base64": jpeg_base64,
        "timestamp_ms": frame.timestamp_ms,
    }))
}

pub async fn probe_custom_connector(
    sup: &OpenBotSupervisor,
    connector_id: &str,
) -> Result<serde_json::Value> {
    let conn = sup
        .connectors()
        .list_connectors()?
        .into_iter()
        .find(|c| c.id == connector_id.trim())
        .ok_or_else(|| GmError::Config(format!("Unknown connector '{connector_id}'")))?;

    match conn.transport {
        ConnectorTransport::RemoteHttps => {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(3))
                .build()
                .map_err(|e| GmError::Platform(e.to_string()))?;
            match client.head(&conn.url_or_command).send().await {
                Ok(resp) => Ok(serde_json::json!({
                    "ok": true,
                    "id": conn.id,
                    "status_code": resp.status().as_u16(),
                    "status_text": format!("Reachable (HTTP {})", resp.status().as_u16()),
                })),
                Err(err) => Ok(serde_json::json!({
                    "ok": false,
                    "id": conn.id,
                    "status_text": format!("Unreachable: {err}"),
                })),
            }
        }
        ConnectorTransport::StdioCommand => {
            let cmd = conn.url_or_command.trim();
            let exists = if cmd.contains('/') || cmd.contains('\\') {
                Path::new(cmd).is_file()
            } else {
                std::env::var_os("PATH").is_some_and(|paths| {
                    std::env::split_paths(&paths).any(|p| p.join(cmd).is_file())
                })
            };
            Ok(serde_json::json!({
                "ok": exists,
                "id": conn.id,
                "status_text": if exists {
                    format!("Executable '{cmd}' found")
                } else {
                    format!("Executable '{cmd}' not found in PATH")
                },
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shared_interactions_resolve_and_teach_task_scrubs_secrets() {
        let state = SharedInteractionState::new();
        let (tx_a, rx_a) = oneshot::channel();
        state.register_approval("app-1".into(), tx_a).await;
        assert!(state.resolve_approval("app-1", true).await);
        assert!(rx_a.await.unwrap());

        let (tx_s, rx_s) = oneshot::channel();
        state.register_secret("sec-1".into(), tx_s).await;
        assert!(
            state
                .resolve_secret("sec-1", Some(vec![("api_key".into(), "secret-val".into())]))
                .await
        );
        assert_eq!(rx_s.await.unwrap().unwrap()[0].1, "secret-val");

        let (tx_q, rx_q) = oneshot::channel();
        state.register_question("q-1".into(), tx_q).await;
        assert!(state.resolve_question("q-1", Some("Option A".into())).await);
        assert_eq!(rx_q.await.unwrap().unwrap(), "Option A");

        let dir = tempfile::tempdir().unwrap();
        let paths = MuseRuntimePaths::shared(dir.path());
        state.start_teach_task("muse").unwrap();
        state
            .record_teach_step("Entered token ghp_123456789012345678901234567890123456")
            .unwrap();
        let out = state
            .stop_teach_task(&paths, Some("my-recorded-flow"))
            .unwrap();
        assert!(!out.draft_markdown.contains("ghp_1234567890"));
        assert!(out.saved_skill_dir.is_some());
    }
}
