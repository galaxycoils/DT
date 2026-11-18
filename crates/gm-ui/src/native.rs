//! Shared native desktop UI (`gm-ui` GrokBot Suite).
//! - Event-driven `0 Hz` idle repaint discipline (zero periodic polling loops when idle)
//! - Default `eframe::Renderer::Glow` (`35–55 MB` RSS) with opt-in `--features hardware-wgpu`
//! - Strictly bounded `16 MB` / `32`-entry `BoundedTextureCache`
//! - Adaptive `2–5 FPS` `CaptureComputerPreview` ONLY while the Live Computer viewer is visible
//! - Full GrokBot 4-column adaptive layout, 14 inline cards, Smart Composer, and 5-tier Escape stack.

use crate::ui::{
    cache::{BoundedTextureCache, MarkdownLayoutCache},
    chat::{TranscriptRow, draw_chat_canvas},
    details_drawer::draw_bot_details_drawer,
    hotkeys::handle_global_hotkeys,
    modals::draw_active_modal,
    sidebar::draw_adaptive_sidebar,
    split_page::draw_split_page_panel,
    state::{BotDetailsTab, GrokUiState, WorkspaceSection},
    theme::GrokPalette,
    workspace_views::draw_workspace_section_view,
};
use base64::Engine;
use eframe::egui;
mod session;
use gm_core::{
    config::GmConfig,
    error::{GmError, Result},
    ipc::*,
};
use serde_json::Value;
use session::{SessionData, SessionStore, switch_draft};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::io::{AsyncRead, AsyncWrite};

trait Connection: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Connection for T {}

async fn connect(path: &Path) -> Result<Box<dyn Connection>> {
    #[cfg(unix)]
    {
        Ok(Box::new(tokio::net::UnixStream::connect(path).await?))
    }
    #[cfg(windows)]
    {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            match tokio::net::windows::named_pipe::ClientOptions::new().open(path) {
                Ok(pipe) => return Ok(Box::new(pipe)),
                Err(e)
                    if e.raw_os_error() == Some(231) && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

pub(crate) async fn is_running(config: &GmConfig) -> bool {
    connect(&config.daemon_socket_path).await.is_ok()
}

async fn rpc(config: &GmConfig, request: DaemonRequest) -> Result<DaemonResponse> {
    tokio::time::timeout(std::time::Duration::from_secs(45), async {
        let mut connection = connect(&config.daemon_socket_path).await?;
        write_ipc_message(
            &mut connection,
            &IpcMessage {
                id: 1,
                payload: request,
            },
        )
        .await?;
        let reply: IpcMessage<DaemonResponse> = read_ipc_message(&mut connection).await?;
        match reply.payload {
            DaemonResponse::Error(error) => Err(GmError::Ipc(error)),
            reply => Ok(reply),
        }
    })
    .await
    .map_err(|_| GmError::Timeout("The daemon did not respond".into()))?
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingKind {
    Other,
    Bootstrap,
    ToolResult(String),
    RevealWebhook(String),
    Avatar(String),
    ImportAttachment,
    ListBotFiles,
    PreviewBotFile,
    LibraryPage,
    ChannelHistory(String),
    DirectChatHistory(String),
    PostDirectChat(String),
    SearchMessages,
    ListSections,
    ListRules,
    ListVaultItems,
    RevealVaultItem(String),
    UsageBreakdown,
    ListConnectors,
    ListRoutines,
    /// Any write whose result is reflected by re-fetching the bootstrap snapshot.
    Mutation,
    CaptureComputerPreview,
    ListActivity,
    RateActivity,
    ListGoals,
    SaveGoal,
    ListSpaces,
    CreateSpace,
    SearchSkills,
    ReadRuntimeFile,
    SaveRuntimeFile,
    ImportMemory,
    ForgetPlan,
    ForgetConfirm,
    UpdateDiscretion,
}

fn pending_kind(request: &DaemonRequest) -> PendingKind {
    match request {
        DaemonRequest::GetBootstrapState => PendingKind::Bootstrap,
        DaemonRequest::ManageSystem { op, payload } if op == "get_bot_avatar" => {
            PendingKind::Avatar(payload["bot_id"].as_str().unwrap_or_default().into())
        }
        DaemonRequest::ListRoutineRuns { .. } => PendingKind::ToolResult("routine_runs".into()),
        DaemonRequest::QueryAudit { .. } => PendingKind::ToolResult("audit".into()),
        DaemonRequest::ExplainMemory { .. } => PendingKind::ToolResult("explain_memory".into()),
        DaemonRequest::GetDiscretion { .. } => PendingKind::ToolResult("discretion".into()),
        DaemonRequest::CheckUpdates => PendingKind::ToolResult("updates".into()),
        DaemonRequest::StartTeachTask { .. } | DaemonRequest::StopTeachTask { .. } => {
            PendingKind::ToolResult("teach".into())
        }
        DaemonRequest::ManageSpace { op, .. } if op == "read" => {
            PendingKind::ToolResult("space_file".into())
        }
        DaemonRequest::ManageSystem { op, payload } if op == "get_routine_webhook_key" => {
            PendingKind::RevealWebhook(payload["routine_id"].as_str().unwrap_or_default().into())
        }
        DaemonRequest::ManageSystem { op, .. }
            if matches!(
                op.as_str(),
                "get_bot_avatar" | "install_update" | "support_bundle" | "health" | "feedback"
            ) =>
        {
            PendingKind::ToolResult(op.clone())
        }
        DaemonRequest::ImportAttachment { .. } => PendingKind::ImportAttachment,
        DaemonRequest::ListBotFiles { .. } => PendingKind::ListBotFiles,
        DaemonRequest::PreviewBotFile { .. } => PendingKind::PreviewBotFile,
        DaemonRequest::ListLibraryPage { .. } => PendingKind::LibraryPage,
        DaemonRequest::ChannelHistory { channel_id } => {
            PendingKind::ChannelHistory(channel_id.clone())
        }
        DaemonRequest::GetDirectChatHistory { bot_id, .. } => {
            PendingKind::DirectChatHistory(bot_id.clone())
        }
        DaemonRequest::PostDirectChat { bot_id, .. } => PendingKind::PostDirectChat(bot_id.clone()),
        DaemonRequest::SearchMessages { .. } => PendingKind::SearchMessages,
        DaemonRequest::ManageBotSection { op, .. } if op == "list" => PendingKind::ListSections,
        DaemonRequest::ManageCustomRule { op, .. } if op == "list" => PendingKind::ListRules,
        DaemonRequest::ManageVault { op, .. } if op == "list" => PendingKind::ListVaultItems,
        DaemonRequest::ManageVault { op, id, .. } if op == "reveal" => {
            PendingKind::RevealVaultItem(id.clone().unwrap_or_default())
        }
        DaemonRequest::GetUsageBreakdown { .. } => PendingKind::UsageBreakdown,
        DaemonRequest::ListCustomConnectors => PendingKind::ListConnectors,
        DaemonRequest::ListRoutines => PendingKind::ListRoutines,
        DaemonRequest::ManageBotSection { .. }
        | DaemonRequest::ManageCustomRule { .. }
        | DaemonRequest::ManageVault { .. }
        | DaemonRequest::ImportVaultCsv { .. }
        | DaemonRequest::SaveCustomConnector { .. }
        | DaemonRequest::SetBotUiPrefs { .. }
        | DaemonRequest::SaveBot { .. }
        | DaemonRequest::MarkChatRead { .. }
        | DaemonRequest::ManageSystem { .. }
        | DaemonRequest::SaveUiSettings { .. }
        | DaemonRequest::CreateChannel { .. }
        | DaemonRequest::SaveRoutine { .. }
        | DaemonRequest::DeleteRoutine { .. }
        | DaemonRequest::TriggerRoutineTest { .. }
        | DaemonRequest::ManageFeed { .. }
        | DaemonRequest::ManageSkill { .. }
        | DaemonRequest::ManageSpace { .. } => PendingKind::Mutation,
        DaemonRequest::CaptureComputerPreview { .. } => PendingKind::CaptureComputerPreview,
        DaemonRequest::ListActivity => PendingKind::ListActivity,
        DaemonRequest::RateActivity { .. } => PendingKind::RateActivity,
        DaemonRequest::ListGoals => PendingKind::ListGoals,
        DaemonRequest::SaveGoal { .. } => PendingKind::SaveGoal,
        DaemonRequest::ListSpaces => PendingKind::ListSpaces,
        DaemonRequest::CreateSpace { .. } => PendingKind::CreateSpace,
        DaemonRequest::SearchSkills { .. } => PendingKind::SearchSkills,
        DaemonRequest::ReadRuntimeFile { .. } => PendingKind::ReadRuntimeFile,
        DaemonRequest::SaveRuntimeFile { .. } => PendingKind::SaveRuntimeFile,
        DaemonRequest::ImportMemory { .. } => PendingKind::ImportMemory,
        DaemonRequest::ForgetPlan { .. } => PendingKind::ForgetPlan,
        DaemonRequest::ForgetConfirm { .. } => PendingKind::ForgetConfirm,
        DaemonRequest::GetProviderSettings
        | DaemonRequest::SaveProviderProfile { .. }
        | DaemonRequest::DiscoverProviderModels { .. } => {
            PendingKind::ToolResult("providers".into())
        }
        DaemonRequest::ProviderOAuthAction { .. } => {
            PendingKind::ToolResult("provider_oauth".into())
        }
        DaemonRequest::UpdateDiscretion { .. } => PendingKind::UpdateDiscretion,
        _ => PendingKind::Other,
    }
}

#[derive(Default)]
struct ViewState {
    status: String,
    paused: bool,
    error: Option<String>,
    messages: Vec<(String, String)>,
    dm_messages: Vec<Value>,
    dm_bot_id: String,
    streaming: String,
    tasks: Vec<Value>,
    bots: Vec<Value>,
    sections: Vec<Value>,
    channels: Vec<Value>,
    unread_counts: Value,
    routines: Vec<Value>,
    rules: Vec<Value>,
    vault_items: Vec<Value>,
    usage_data: Value,
    connectors_data: Value,
    bootstrap_status: Value,
    ui_config: Option<gm_core::config::UiConfig>,
    bot_statuses: Value,
    connected_app_count: u64,
    version: String,
    files: Value,
    file_preview: Value,
    library_page: Value,
    search_hits: Vec<Value>,
    attachment_paths: Vec<(String, String)>,
    channel_messages: Vec<Value>,
    channel_history_id: String,
    approvals: Vec<(String, String, String)>,
    connected: bool,
    sending: bool,
    clear_drafts: Vec<(String, String)>,
    active_chat: Option<String>,
    completed_chat: Option<String>,
    activity_posts: Vec<Value>,
    brief_prompt: Option<String>,
    activity_notice: String,
    goals: Vec<Value>,
    goal_notice: String,
    spaces_data: Value,
    spaces_notice: String,
    skills_data: Value,
    runtime_file_bot: String,
    runtime_file_name: String,
    runtime_file_content: String,
    runtime_file_sha256: Option<String>,
    runtime_file_dirty: bool,
    identity_notice: String,
    memory_import_report: Option<Value>,
    forget_plan: Option<Value>,
    forget_report: Option<Value>,
    forget_notice: String,
    discretion_policy: Option<Value>,
    discretion_notice: String,
    computer_frame_meta: Option<(u32, u32, u64)>,
    pending_computer_frame_jpeg: Option<(u32, u32, u64, String)>,
    revealed_secret: Option<(String, String)>,
    pending_ui_snapshot: Option<Value>,
    tool_results: serde_json::Map<String, Value>,
}

impl ViewState {
    fn select_chat_request(&mut self, request: &DaemonRequest) {
        match request {
            DaemonRequest::GetDirectChatHistory { bot_id, .. } if self.dm_bot_id != *bot_id => {
                self.dm_bot_id = bot_id.clone();
                self.dm_messages.clear();
            }
            DaemonRequest::ChannelHistory { channel_id }
                if self.channel_history_id != *channel_id =>
            {
                self.channel_history_id = channel_id.clone();
                self.channel_messages.clear();
            }
            _ => {}
        }
    }

    fn merge_dm_history(&mut self, bot_id: &str, value: &Value) {
        if self.dm_bot_id != bot_id {
            return;
        }
        if let Some(messages) = value["messages"].as_array().or_else(|| value.as_array()) {
            for message in messages {
                if let Some(existing) = self
                    .dm_messages
                    .iter_mut()
                    .find(|m| m["id"] == message["id"])
                {
                    *existing = message.clone();
                } else {
                    self.dm_messages.push(message.clone());
                }
            }
            self.dm_messages
                .sort_by_key(|m| m["seq"].as_u64().unwrap_or(0));
        }
    }

    fn finish_chat(&mut self, id: String, text: String) {
        if self.active_chat.as_ref() != Some(&id) || self.completed_chat.as_ref() == Some(&id) {
            return;
        }
        self.streaming.clear();
        self.messages.push(("Muse".into(), text));
        self.completed_chat = Some(id);
    }

    fn apply_bootstrap(&mut self, value: Value) {
        self.pending_ui_snapshot = Some(value.clone());
        if let Some(arr) = value["bots"].as_array() {
            self.bots = arr.clone();
        }
        if let Some(arr) = value["sections"].as_array() {
            self.sections = arr.clone();
        }
        if let Some(arr) = value["channels"].as_array() {
            self.channels = arr.clone();
        }
        if !value["unread_counts"].is_null() {
            self.unread_counts = value["unread_counts"].clone();
        }
        if let Some(arr) = value["dm_messages"].as_array() {
            self.dm_messages = arr.clone();
        }
        if let Some(arr) = value["recent_tasks"].as_array() {
            self.tasks = arr.clone();
        }
        if let Some(arr) = value["routines"].as_array() {
            self.routines = arr.clone();
        }
        if let Some(arr) = value["custom_rules"].as_array() {
            self.rules = arr.clone();
        }
        if let Some(arr) = value["vault_items"].as_array() {
            self.vault_items = arr.clone();
        }
        if !value["usage"].is_null() {
            self.usage_data = value["usage"].clone();
        }
        self.connectors_data = serde_json::json!({
            "integrations": value["integrations"],
            "custom_connectors": value["custom_connectors"],
        });
        self.bootstrap_status = value["status"].clone();
        self.paused = value["status"]["paused"].as_bool().unwrap_or(false);
        self.ui_config = serde_json::from_value(value["ui"].clone()).ok();
        self.skills_data = value["skills"].clone();
        self.bot_statuses = value["bot_statuses"].clone();
        self.connected_app_count = value["connected_app_count"].as_u64().unwrap_or(0);
        self.version = value["version"].as_str().unwrap_or_default().to_string();
        if !value["providers"].is_null() {
            self.tool_results
                .insert("providers".into(), value["providers"].clone());
        }
        if let Some(arr) = value["goals"].as_array() {
            self.goals = arr.clone();
        }
        if !value["spaces"].is_null() {
            let entries = value["spaces"].as_array().cloned().unwrap_or_default();
            let mut urls = serde_json::Map::new();
            let spaces: Vec<Value> = entries
                .into_iter()
                .map(|entry| {
                    let meta = entry.get("meta").unwrap_or(&entry).clone();
                    if let Some(slug) = meta["slug"].as_str() {
                        urls.insert(slug.to_string(), entry["url"].clone());
                    }
                    meta
                })
                .collect();
            self.spaces_data = serde_json::json!({
                "spaces": spaces, "urls": urls,
                "spaces_port": value["status"]["spaces_port"], "enable_spaces": true,
            });
        }
        if let Some(arr) = value["feed"]["posts"].as_array() {
            self.activity_posts = arr.clone();
        }
        self.brief_prompt = value["feed"]["brief_prompt"].as_str().map(str::to_string);
    }

    fn event(&mut self, event: DaemonEvent) {
        match event {
            DaemonEvent::HudStateChanged { state, detail, .. } => {
                self.paused = state.eq_ignore_ascii_case("paused");
                self.status = format!("{state}: {detail}");
            }
            DaemonEvent::ChatTokenV1 {
                message_id,
                delta,
                done,
            } if self.active_chat.as_ref() == Some(&message_id)
                && self.completed_chat.as_ref() != Some(&message_id) =>
            {
                self.streaming.push_str(&delta);
                if done {
                    let text = std::mem::take(&mut self.streaming);
                    self.finish_chat(message_id, text);
                }
            }
            DaemonEvent::TaskFinished {
                task_id,
                ok,
                message,
            } => self.messages.push((
                if ok { "Completed" } else { "Task update" }.into(),
                format!("{task_id}\n{message}"),
            )),
            DaemonEvent::ApprovalResolved { request_id }
            | DaemonEvent::ApprovalExpired { request_id } => {
                self.approvals.retain(|(id, _, _)| id != &request_id)
            }
            DaemonEvent::ApprovalRequested {
                request_id,
                title,
                details,
            } => self.approvals.push((request_id, title, details)),
            DaemonEvent::WheelHandoffRequested {
                task_id,
                blocker,
                instructions,
                ..
            } => self.messages.push((
                "Human input needed".into(),
                format!("{task_id}\n{blocker}\n{instructions}"),
            )),
            DaemonEvent::ActivityUpdated { reason } => {
                self.activity_notice = format!("Updated ({reason})");
            }
            DaemonEvent::DirectMessageAdded { bot_id, message } => {
                if self.dm_bot_id.is_empty() || self.dm_bot_id == bot_id {
                    let msg_id = message["id"].as_str().unwrap_or("");
                    if !self
                        .dm_messages
                        .iter()
                        .any(|m| m["id"].as_str() == Some(msg_id))
                    {
                        self.dm_messages.push(message);
                    }
                }
            }
            DaemonEvent::MessageReactionUpdated {
                message_id,
                reactions,
                ..
            } => {
                for m in &mut self.dm_messages {
                    if m["id"].as_str() == Some(message_id.as_str()) {
                        m["reactions"] = reactions.clone();
                    }
                }
                for m in &mut self.channel_messages {
                    if m["id"].as_str() == Some(message_id.as_str()) {
                        m["reactions"] = reactions.clone();
                    }
                }
            }
            DaemonEvent::MessageCardUpdated {
                message_id, card, ..
            } => {
                for m in &mut self.dm_messages {
                    if m["id"].as_str() == Some(message_id.as_str()) {
                        m["card"] = card.clone();
                    }
                }
                for m in &mut self.channel_messages {
                    if m["id"].as_str() == Some(message_id.as_str()) {
                        m["card"] = card.clone();
                    }
                }
            }
            DaemonEvent::BotStatusPhrase {
                bot_id,
                phrase,
                needs_attention,
            } => {
                self.bot_statuses[&bot_id] =
                    serde_json::json!({"phrase":phrase,"needs_attention":needs_attention});
                for b in &mut self.bots {
                    if b["id"].as_str() == Some(bot_id.as_str()) {
                        b["status_phrase"] =
                            phrase.clone().map(Value::String).unwrap_or(Value::Null);
                        b["needs_attention"] = Value::Bool(needs_attention);
                    }
                }
            }
            DaemonEvent::ComputerFrameReady {
                width,
                height,
                jpeg_base64,
                timestamp_ms,
            } => {
                self.computer_frame_meta = Some((width, height, timestamp_ms));
                self.pending_computer_frame_jpeg = Some((width, height, timestamp_ms, jpeg_base64));
            }
            _ => {}
        }
        // Bound retained UI history; full task results remain in the daemon's durable store.
        if self.messages.len() > 500 {
            self.messages.drain(..100);
        }
        if self.dm_messages.len() > 1000 {
            let excess = self.dm_messages.len() - 1000;
            self.dm_messages.drain(..excess);
        }
    }
}

#[derive(Clone)]
struct Shared {
    state: Arc<Mutex<ViewState>>,
    context: egui::Context,
    session: Option<Arc<SessionStore>>,
}

impl Shared {
    fn update(&self, f: impl FnOnce(&mut ViewState)) {
        if let Ok(mut state) = self.state.lock() {
            let before = (
                state.messages.len(),
                state
                    .messages
                    .last()
                    .map(|(role, text)| (role.len(), text.len())),
            );
            f(&mut state);
            if before
                != (
                    state.messages.len(),
                    state
                        .messages
                        .last()
                        .map(|(role, text)| (role.len(), text.len())),
                )
                && let Some(store) = &self.session
            {
                store.edit(|d| d.messages = state.messages.clone());
            }
        }
        self.context.request_repaint();
    }
}

type OutboundRequest = (DaemonRequest, Option<(String, String)>);

struct NativeApp {
    shared: Shared,
    requests: tokio::sync::mpsc::Sender<OutboundRequest>,
    live_computer_active: Arc<AtomicBool>,
    live_computer_notify: Arc<tokio::sync::Notify>,
    ui_state: GrokUiState,
    textures: BoundedTextureCache,
    layout_cache: MarkdownLayoutCache,
    avatar_assets: std::collections::HashMap<String, String>,
    lightbox_requested: Option<String>,
    draft: String,
    draft_target: String,
    other_drafts: Vec<(String, String)>,
    bot: String,
    task_mode: bool,
    search: String,
    resume_note: String,
    goal_title: String,
    goal_description: String,
    goal_category: String,
    goal_parent: String,
    space_slug: String,
    space_title: String,
    space_html: String,
    identity_bot: String,
    identity_file: String,
    identity_text: String,
    identity_sha256: Option<String>,
    memory_import_text: String,
    forget_request: String,
    channel: String,
    new_channel: String,
    settings_config: GmConfig,
    config_path: std::path::PathBuf,
    config_original: Option<Vec<u8>>,
    settings_notice: String,
}

impl NativeApp {
    fn sync_draft_target(&mut self) {
        let target = if self.ui_state.active_chat_kind == "channel" {
            format!("channel:{}", self.channel)
        } else {
            format!("dm:{}", self.bot)
        };
        switch_draft(
            &mut self.draft_target,
            &mut self.draft,
            &mut self.other_drafts,
            target,
        );
    }

    fn save_draft(&self, data: &mut SessionData) {
        data.draft.clone_from(&self.draft);
        data.draft_target = Some(self.draft_target.clone());
        data.other_drafts.clone_from(&self.other_drafts);
        data.bot.clone_from(&self.bot);
        data.task_mode = self.task_mode;
    }

    /// Detached request sink (cloned channel handles) so panels can enqueue requests while
    /// `self` fields are mutably borrowed.
    fn sender(&self) -> impl FnMut(DaemonRequest) + 'static {
        let requests = self.requests.clone();
        let shared = self.shared.clone();
        move |request| {
            shared.update(|s| s.select_chat_request(&request));
            if let Err(error) = requests.try_send((request, None)) {
                shared.update(|s| s.error = Some(format!("Request queue is busy: {error}")));
            }
        }
    }

    fn send(&self, request: DaemonRequest) {
        self.shared.update(|s| s.select_chat_request(&request));
        if let Err(error) = self.requests.try_send((request, None)) {
            self.shared
                .update(|s| s.error = Some(format!("Request queue is busy: {error}")));
        }
    }

    fn enqueue_chat_message(
        &self,
        text: String,
        messages: &[(String, String)],
        reply_to_id: Option<String>,
        clear_draft: Option<String>,
    ) {
        let message_id = uuid::Uuid::new_v4().to_string();
        let active_bot = self.bot.clone();
        self.shared.update(|state| {
            state.active_chat = Some(message_id.clone());
            state.completed_chat = None;
            state.streaming.clear();
            state.dm_bot_id = active_bot.clone();
        });
        // Bot DMs are persisted and answered by the daemon (`PostDirectChat`); the legacy
        // streaming chat path is only used when no bot roster is available.
        let request = if active_bot.is_empty() {
            DaemonRequest::SendChatMessageV2 {
                message_id,
                text: text.clone(),
                history: conversation_context(messages),
            }
        } else {
            DaemonRequest::PostDirectChat {
                bot_id: active_bot,
                content: text.clone(),
                reply_to_id,
                thread_id: None,
            }
        };
        let clear_draft = clear_draft.map(|text| (self.draft_target.clone(), text));
        if self.requests.try_send((request, clear_draft)).is_ok() {
            self.shared.update(|s| {
                s.sending = true;
                s.error = None;
                s.messages.push(("You".into(), text));
            });
        } else {
            self.shared.update(|s| {
                s.error = Some("Request queue is busy; your draft is preserved".into())
            });
        }
    }

    fn build_transcript_rows(
        &self,
        dm_messages: &[Value],
        channel_messages: &[Value],
        local_messages: &[(String, String)],
    ) -> Vec<TranscriptRow> {
        if self.ui_state.active_chat_kind == "channel" {
            return channel_messages
                .iter()
                .enumerate()
                .map(|(idx, m)| {
                    let author = m["author"].as_str().unwrap_or("bot").to_string();
                    let role = if author == "human" || author == "You" {
                        "user".to_string()
                    } else {
                        "assistant".to_string()
                    };
                    TranscriptRow {
                        id: m["id"]
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("ch-{idx}")),
                        seq: m["seq"].as_i64().unwrap_or(idx as i64 + 1),
                        author,
                        role,
                        content: m["content"].as_str().unwrap_or("").to_string(),
                        created_at: m["created_at"].as_str().unwrap_or("").to_string(),
                        reply_to_id: m["reply_to_id"].as_str().map(str::to_string),
                        request_id: m["request_id"].as_str().map(str::to_string),
                        reactions: m["reactions"].as_array().cloned().unwrap_or_default(),
                        card: if m["card"].is_null() {
                            None
                        } else {
                            Some(m["card"].clone())
                        },
                    }
                })
                .collect();
        }

        if !self.bot.is_empty() {
            let rows: Vec<TranscriptRow> = dm_messages
                .iter()
                .filter(|m| m["bot_id"].as_str().is_none_or(|id| id == self.bot))
                .enumerate()
                .map(|(idx, m)| {
                    let role = m["role"].as_str().unwrap_or("assistant").to_string();
                    let author = m["author"].as_str().map(str::to_string).unwrap_or_else(|| {
                        if role == "user" {
                            "You".into()
                        } else {
                            self.bot.clone()
                        }
                    });
                    TranscriptRow {
                        id: m["id"]
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("dm-{idx}")),
                        seq: m["seq"].as_i64().unwrap_or(idx as i64 + 1),
                        author,
                        role,
                        content: m["content"].as_str().unwrap_or("").to_string(),
                        created_at: m["created_at"].as_str().unwrap_or("").to_string(),
                        reply_to_id: m["reply_to_id"].as_str().map(str::to_string),
                        request_id: m["request_id"].as_str().map(str::to_string),
                        reactions: m["reactions"].as_array().cloned().unwrap_or_default(),
                        card: if m["card"].is_null() {
                            None
                        } else {
                            Some(m["card"].clone())
                        },
                    }
                })
                .collect();
            return rows;
        }

        local_messages
            .iter()
            .enumerate()
            .map(|(idx, (role_label, content))| {
                let is_user = role_label == "You";
                TranscriptRow {
                    id: format!("local-{idx}"),
                    seq: idx as i64 + 1,
                    author: role_label.clone(),
                    role: if is_user {
                        "user".into()
                    } else {
                        "assistant".into()
                    },
                    content: content.clone(),
                    created_at: String::new(),
                    reply_to_id: None,
                    request_id: None,
                    reactions: Vec::new(),
                    card: None,
                }
            })
            .collect()
    }
}

impl eframe::App for NativeApp {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(store) = &self.shared.session {
            store.edit(|data| self.save_draft(data));
            if let Err(error) = store.flush() {
                tracing::error!(%error, "Could not save native conversation");
            }
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let previous_section = self.ui_state.section;
        let previous_modal = self.ui_state.active_modal.clone();
        // 1. Handle global GrokBot hotkeys & 5-tier Escape stack
        let hotkey_outcome = handle_global_hotkeys(ctx, &mut self.ui_state);

        if previous_section != self.ui_state.section {
            match self.ui_state.section {
                WorkspaceSection::Feed => self.send(DaemonRequest::ListActivity),
                WorkspaceSection::Goals => self.send(DaemonRequest::ListGoals),
                WorkspaceSection::Spaces => self.send(DaemonRequest::ListSpaces),
                WorkspaceSection::Audit => {
                    self.send(DaemonRequest::ListQueue { bot_id: None });
                    self.send(DaemonRequest::QueryAudit {
                        category: None,
                        bot_id: None,
                        limit: Some(100),
                    });
                }
                WorkspaceSection::Bots => {}
            }
        }
        if hotkey_outcome.escape_dismissed {
            if !matches!(
                self.ui_state.active_modal,
                Some(crate::ui::state::ActiveModal::Lightbox { .. })
            ) {
                self.textures.evict_prefix("lightbox:");
            }
            if !self.ui_state.is_live_computer_visible() {
                self.textures.evict_prefix("live_computer:");
            }
        }
        if let Some(ui) = self
            .shared
            .state
            .lock()
            .ok()
            .and_then(|mut s| s.ui_config.take())
        {
            self.ui_state.theme_pref = ui.theme.clone();
            self.ui_state.accent_mode = ui.accent_mode.clone();
            self.ui_state.execution_mode = ui.computer_execution_mode.clone();
            self.settings_config.ui = ui;
        }

        if let Ok(mut state) = self.shared.state.lock() {
            if let Some(snapshot) = state.pending_ui_snapshot.take() {
                self.ui_state.server_snapshot = snapshot;
            }
            self.ui_state.server_snapshot["bot_statuses"] = state.bot_statuses.clone();
            for (key, value) in std::mem::take(&mut state.tool_results) {
                if key.starts_with("avatar:") || key.starts_with("lightbox:") {
                    if let Some(encoded) = value["bytes_base64"].as_str()
                        && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded)
                    {
                        self.textures.insert_encoded(
                            ctx,
                            key.clone(),
                            &bytes,
                            if key.starts_with("avatar:") {
                                256
                            } else {
                                1920
                            },
                        );
                    }
                } else {
                    if key == "providers" || key == "provider_oauth" {
                        let candidate = value.get("providers").unwrap_or(&value);
                        if let Ok(parsed) = serde_json::from_value::<gm_core::config::ProvidersConfig>(
                            candidate.clone(),
                        ) {
                            self.ui_state.server_snapshot["providers"] =
                                serde_json::to_value(&parsed).unwrap_or_default();
                            self.settings_config.providers = parsed;
                        } else if let Some(prof_val) = value.get("profile")
                            && let Ok(prof) = serde_json::from_value::<
                                gm_core::config::ProviderProfileConfig,
                            >(prof_val.clone())
                        {
                            let _ = self.settings_config.providers.upsert_profile(prof, false);
                        }
                    }
                    self.ui_state.responses[key] = value;
                }
            }
        }
        let target = crate::ui::state::NavTarget {
            section: self.ui_state.section,
            chat_kind: self.ui_state.active_chat_kind.clone(),
            target_id: if self.ui_state.active_chat_kind == "channel" {
                self.channel.clone()
            } else {
                self.bot.clone()
            },
        };
        self.ui_state.nav_history.push(target);
        let target = match std::mem::take(&mut self.ui_state.nav_request) {
            -1 => self.ui_state.nav_history.back(),
            1 => self.ui_state.nav_history.forward(),
            _ => None,
        };
        if let Some(target) = target {
            self.ui_state.section = target.section;
            self.ui_state.active_chat_kind = target.chat_kind;
            if self.ui_state.active_chat_kind == "channel" {
                self.channel = target.target_id;
                self.send(DaemonRequest::ChannelHistory {
                    channel_id: self.channel.clone(),
                });
            } else {
                self.bot = target.target_id;
                self.send(DaemonRequest::GetDirectChatHistory {
                    bot_id: self.bot.clone(),
                    before_seq: None,
                    limit: Some(50),
                });
            }
            match self.ui_state.section {
                WorkspaceSection::Feed => self.send(DaemonRequest::ListActivity),
                WorkspaceSection::Goals => self.send(DaemonRequest::ListGoals),
                WorkspaceSection::Spaces => self.send(DaemonRequest::ListSpaces),
                WorkspaceSection::Audit => self.send(DaemonRequest::ListQueue { bot_id: None }),
                WorkspaceSection::Bots => {}
            }
        }
        self.sync_draft_target();
        if let Ok(mut state) = self.shared.state.lock()
            && let Some((id, mut secret)) = state.revealed_secret.take()
        {
            if matches!(
                self.ui_state.active_modal,
                Some(
                    crate::ui::state::ActiveModal::Settings
                        | crate::ui::state::ActiveModal::RoutineEditor
                )
            ) {
                self.ui_state.revealed_secrets.insert(id, secret);
            } else {
                use zeroize::Zeroize;
                secret.zeroize();
            }
        }
        // 2. Handle dropped file attachments
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for file in dropped {
            if let Some(path) = file.path {
                self.send(DaemonRequest::ImportAttachment {
                    bot_id: self.bot.clone(),
                    source_path: path,
                });
            }
        }

        // 3. Snapshot ViewState without holding mutex across egui layout
        let (
            status,
            _paused,
            error,
            connected,
            messages,
            dm_messages,
            channel_messages,
            streaming,
            tasks,
            bots,
            sections,
            channels,
            unread_counts,
            routines,
            rules,
            vault_items,
            usage_data,
            connectors_data,
            file_preview,
            library_page,
            search_hits,
            approvals,
            sending,
            activity_posts,
            brief_prompt,
            activity_notice,
            goals,
            goal_notice,
            spaces_data,
            spaces_notice,
            skills_data,
            identity_notice,
            memory_import_report,
            forget_plan,
            forget_notice,
            computer_frame_meta,
            pending_jpeg,
        ) = {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            let attachments = std::mem::take(&mut state.attachment_paths);
            for (bot, path) in attachments {
                if bot == self.bot {
                    if !self.draft.is_empty() {
                        self.draft.push('\n');
                    }
                    self.draft.push_str(&format!("Attached file: {path}"));
                } else {
                    state
                        .messages
                        .push(("File imported".into(), format!("@{bot}: {path}")));
                }
            }
            if let Some((id, secret)) = state.revealed_secret.take() {
                self.ui_state.revealed_secrets.insert(id, secret);
            }
            for (target, sent) in std::mem::take(&mut state.clear_drafts) {
                if target == self.draft_target && self.draft.trim() == sent {
                    self.draft.clear();
                }
                self.other_drafts
                    .retain(|(key, draft)| *key != target || draft.trim() != sent);
            }
            if state.runtime_file_dirty {
                self.identity_bot = state.runtime_file_bot.clone();
                self.identity_file = state.runtime_file_name.clone();
                self.identity_text = state.runtime_file_content.clone();
                self.identity_sha256 = state.runtime_file_sha256.clone();
                state.runtime_file_dirty = false;
            } else if state.runtime_file_bot == self.identity_bot
                && state.runtime_file_name == self.identity_file
                && state.runtime_file_sha256.is_some()
            {
                self.identity_sha256 = state.runtime_file_sha256.clone();
            }
            (
                state.status.clone(),
                state.paused,
                state.error.clone(),
                state.connected,
                state.messages.clone(),
                state.dm_messages.clone(),
                state.channel_messages.clone(),
                state.streaming.clone(),
                state.tasks.clone(),
                state.bots.clone(),
                state.sections.clone(),
                state.channels.clone(),
                state.unread_counts.clone(),
                state.routines.clone(),
                state.rules.clone(),
                state.vault_items.clone(),
                state.usage_data.clone(),
                state.connectors_data.clone(),
                state.file_preview.clone(),
                state.library_page.clone(),
                state.search_hits.clone(),
                state.approvals.clone(),
                state.sending,
                state.activity_posts.clone(),
                state.brief_prompt.clone(),
                state.activity_notice.clone(),
                state.goals.clone(),
                state.goal_notice.clone(),
                state.spaces_data.clone(),
                state.spaces_notice.clone(),
                state.skills_data.clone(),
                state.identity_notice.clone(),
                state.memory_import_report.clone(),
                state.forget_plan.clone(),
                state.forget_notice.clone(),
                state.computer_frame_meta,
                state.pending_computer_frame_jpeg.take(),
            )
        };

        // 4. Handle Cmd/Ctrl + 1..9 bot quick-switch
        if let Some(bot_idx) = hotkey_outcome.switch_to_bot_index {
            let visible_bots: Vec<&Value> = bots
                .iter()
                .filter(|b| !b["hidden_from_sidebar"].as_bool().unwrap_or(false))
                .collect();
            if let Some(target_bot) = visible_bots.get(bot_idx)
                && let Some(id) = target_bot["id"].as_str()
            {
                self.bot = id.to_string();
                self.ui_state.active_chat_kind = "dm".into();
                self.ui_state.section = WorkspaceSection::Bots;
                self.send(DaemonRequest::GetDirectChatHistory {
                    bot_id: self.bot.clone(),
                    before_seq: None,
                    limit: Some(50),
                });
            }
        }

        // 5. Decode Live Computer JPEG frame into bounded LRU texture cache ONLY when visible
        let live_computer_visible = self.ui_state.live_computer_open
            || (self.ui_state.details_drawer_open
                && self.ui_state.details_tab == BotDetailsTab::Computer);
        let prev_live = self
            .live_computer_active
            .swap(live_computer_visible, Ordering::Relaxed);
        if live_computer_visible && !prev_live {
            self.live_computer_notify.notify_one();
        } else if !live_computer_visible && prev_live {
            // Immediately free Live Computer GPU texture when closed
            self.textures.evict_prefix("live_computer:");
        }
        if live_computer_visible
            && let Some((_w, _h, _seq, jpeg_b64)) = pending_jpeg
            && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(jpeg_b64.trim())
        {
            self.textures
                .insert_encoded(ctx, "live_computer:latest", &bytes, 1280);
        }

        for bot in &bots {
            if let (Some(id), Some(asset)) = (bot["id"].as_str(), bot["avatar_asset"].as_str())
                && self.avatar_assets.get(id).map(String::as_str) != Some(asset)
            {
                self.avatar_assets.insert(id.into(), asset.into());
                self.send(DaemonRequest::ManageSystem {
                    op: "get_bot_avatar".into(),
                    payload: serde_json::json!({"bot_id":id}),
                });
            }
        }
        // 6. Resolve & apply GrokBot Theme Palette
        let active_bot_color_hex = bots
            .iter()
            .find(|b| b["id"].as_str() == Some(self.bot.as_str()))
            .and_then(|b| b["avatar_color"].as_str().or_else(|| b["color"].as_str()))
            .unwrap_or("#3B82F6");
        let system_prefers_dark = ctx
            .system_theme()
            .is_none_or(|theme| theme == egui::Theme::Dark);
        let palette = GrokPalette::resolve(
            &self.settings_config.ui.theme,
            system_prefers_dark,
            &self.settings_config.ui.accent_mode,
            active_bot_color_hex,
        );
        let style_key = format!(
            "{}:{}:{}:{}",
            self.settings_config.ui.theme,
            self.settings_config.ui.accent_mode,
            active_bot_color_hex,
            system_prefers_dark
        );
        if self.ui_state.applied_style_key != style_key {
            palette.apply_to_context(ctx);
            self.ui_state.applied_style_key = style_key;
        }

        let mut outbound = self.sender();
        let prev_bot = self.bot.clone();
        draw_adaptive_sidebar(
            ctx,
            &mut self.ui_state,
            &palette,
            &mut self.textures,
            &bots,
            &sections,
            &channels,
            &unread_counts,
            &mut self.bot,
            &mut self.channel,
            &mut outbound,
        );
        if self.bot != prev_bot {
            let next_bot = self.bot.clone();
            self.shared.update(|s| s.dm_bot_id = next_bot);
        }

        self.sync_draft_target();

        // 9. Right 340px 4-Tab `Bot Details` Drawer (`Settings`, `Computer`, `Memory`, `Library`)
        draw_bot_details_drawer(
            ctx,
            &mut self.ui_state,
            &palette,
            &mut self.textures,
            &bots,
            &self.bot,
            &mut self.identity_file,
            &mut self.identity_text,
            &self.identity_sha256,
            &identity_notice,
            &mut self.memory_import_text,
            &memory_import_report,
            &mut self.forget_request,
            &forget_plan,
            &forget_notice,
            &library_page,
            &computer_frame_meta,
            &mut outbound,
        );

        // 10. Side-by-Side Split Page & Static Space Canvas
        draw_split_page_panel(
            ctx,
            &mut self.ui_state,
            &palette,
            &mut self.draft,
            &mut self.identity_text,
            &self.identity_sha256,
            &file_preview,
            &mut self.space_html,
            &mut outbound,
        );

        // 11. Central Canvas (`Bots` Chat Canvas OR Full-Canvas `Feed` / `Goals` / `Spaces` / `Audit`)
        let rows = self.build_transcript_rows(&dm_messages, &channel_messages, &messages);
        let status_line = if let Some(err) = &error {
            format!("⚠ {err}")
        } else {
            status.clone()
        };

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(palette.bg_canvas))
            .show(ctx, |ui| {
                if self.ui_state.section == WorkspaceSection::Bots {
                    let mut pending_send: Option<(String, Option<String>)> = None;
                    let mut outbound_reqs: Vec<DaemonRequest> = Vec::new();
                    draw_chat_canvas(
                        ui,
                        ctx,
                        &mut self.ui_state,
                        &palette,
                        &mut self.textures,
                        &mut self.layout_cache,
                        &bots,
                        &mut self.bot,
                        &self.channel,
                        &rows,
                        &streaming,
                        &mut self.search,
                        &search_hits,
                        &mut self.draft,
                        connected,
                        sending,
                        &status_line,
                        |req| outbound_reqs.push(req),
                        |text, reply_to_id| pending_send = Some((text, reply_to_id)),
                    );
                    for req in outbound_reqs {
                        self.send(req);
                    }
                    if let Some((text, reply_to_id)) = pending_send {
                        let receipt = Some((self.draft_target.clone(), text.clone()));
                        if self.ui_state.active_chat_kind == "channel" {
                            let req = DaemonRequest::ChannelPost {
                                channel_id: self.channel.clone(),
                                author: "human".into(),
                                content: text.clone(),
                            };
                            let _ = self.requests.try_send((req, receipt));
                        } else if self.task_mode || text.starts_with("/do ") {
                            let instruction = text
                                .strip_prefix("/do ")
                                .unwrap_or(&text)
                                .trim()
                                .to_string();
                            let req = DaemonRequest::QueueTask {
                                bot_id: Some(self.bot.clone()),
                                channel_id: Some(self.channel.clone()),
                                instruction,
                                priority: 0,
                            };
                            if self.requests.try_send((req, receipt)).is_ok() {
                                self.shared.update(|s| {
                                    s.sending = true;
                                    s.error = None;
                                    s.messages.push(("You".into(), text));
                                });
                            }
                        } else {
                            self.enqueue_chat_message(
                                text.clone(),
                                &messages,
                                reply_to_id,
                                Some(text),
                            );
                        }
                    }
                } else {
                    draw_workspace_section_view(
                        ui,
                        ctx,
                        &mut self.ui_state,
                        &palette,
                        &activity_posts,
                        &brief_prompt,
                        &activity_notice,
                        &goals,
                        &mut self.goal_title,
                        &mut self.goal_description,
                        &mut self.goal_category,
                        &mut self.goal_parent,
                        &goal_notice,
                        &spaces_data,
                        &mut self.space_slug,
                        &mut self.space_title,
                        &mut self.space_html,
                        &spaces_notice,
                        &tasks,
                        &mut self.resume_note,
                        &mut self.draft,
                        &mut outbound,
                    );
                }
            });

        if let Some(crate::ui::state::ActiveModal::Lightbox { uri, .. }) =
            &self.ui_state.active_modal
        {
            if self.lightbox_requested.as_deref() != Some(uri) {
                self.lightbox_requested = Some(uri.clone());
                if let Some(path) = crate::muse_render::sandbox_uri_to_bot_relpath(uri) {
                    self.send(DaemonRequest::PreviewBotFile {
                        bot_id: self.bot.clone(),
                        path,
                    });
                }
            }
        } else if self.lightbox_requested.take().is_some() {
            self.textures.evict_prefix("lightbox:");
        }
        // 12. Global Modals (`CommandPalette`, 5-Pane `Settings`, `Studio`, `Shortcuts`, `ConnectApps`, `AvatarPicker`, `Lightbox`, `ExpandedTable`)
        let mut do_save_config = false;
        let mut modal_reqs: Vec<DaemonRequest> = Vec::new();
        draw_active_modal(
            ctx,
            &mut self.ui_state,
            &palette,
            &mut self.textures,
            &bots,
            &sections,
            &routines,
            &skills_data,
            &rules,
            &vault_items,
            &usage_data,
            &connectors_data,
            &mut self.bot,
            &mut self.new_channel,
            &mut self.settings_config,
            &mut self.settings_notice,
            || do_save_config = true,
            |req| modal_reqs.push(req),
        );
        for req in modal_reqs {
            self.send(req);
        }
        if do_save_config {
            let current = std::fs::read(&self.config_path).ok();
            let result = if current != self.config_original && self.config_original.is_some() {
                Err(GmError::Config(
                    "Configuration changed outside this window. Reopen before saving.".into(),
                ))
            } else {
                self.settings_config.save(&self.config_path)
            };
            match result {
                Ok(()) => {
                    self.config_original = std::fs::read(&self.config_path).ok();
                    self.settings_notice = "Saved preferences.".into();
                }
                Err(e) => self.settings_notice = e.to_string(),
            }
        }

        // 13. Floating Tier-3 Approval Windows (in addition to inline approval cards)
        for (id, title, details) in approvals {
            egui::Window::new(title)
                .id(egui::Id::new(&id))
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.add(egui::Label::new(details).selectable(true).wrap());
                    ui.horizontal(|ui| {
                        for (label, approved) in [("Approve this action", true), ("Deny", false)] {
                            if ui.button(label).clicked() {
                                self.send(DaemonRequest::RespondApproval {
                                    request_id: id.clone(),
                                    approved,
                                });
                                self.shared
                                    .update(|s| s.approvals.retain(|(key, _, _)| key != &id));
                            }
                        }
                    });
                });
        }

        if previous_modal.is_some() && previous_modal != self.ui_state.active_modal {
            use zeroize::Zeroize;
            self.ui_state.vault_secret_draft.zeroize();
            self.ui_state.connector_secret_draft.zeroize();
            self.ui_state.vault_csv_draft.zeroize();
            for secret in self.ui_state.revealed_secrets.values_mut() {
                secret.zeroize();
            }
            self.ui_state.revealed_secrets.clear();
        }
        self.sync_draft_target();
        // 14. Persist draft/bot/task_mode changes to private session file without triggering repaint
        if let Some(store) = &self.shared.session {
            let changed = {
                let d = store.data.lock().unwrap_or_else(|e| e.into_inner());
                d.draft != self.draft
                    || d.draft_target.as_deref() != Some(self.draft_target.as_str())
                    || d.other_drafts != self.other_drafts
                    || d.bot != self.bot
                    || d.task_mode != self.task_mode
            };
            if changed {
                store.edit(|data| self.save_draft(data));
            }
        }
    }
}

async fn background(
    config: GmConfig,
    shared: Shared,
    mut requests: tokio::sync::mpsc::Receiver<OutboundRequest>,
    live_computer_active: Arc<AtomicBool>,
    live_computer_notify: Arc<tokio::sync::Notify>,
) {
    let refresh_notify = Arc::new(tokio::sync::Notify::new());

    // 1. Event subscription stream (pushes DaemonEvents and triggers refresh ONLY on actual state changes)
    let events_config = config.clone();
    let events_shared = shared.clone();
    let events_notify = Arc::clone(&refresh_notify);
    tokio::spawn(async move {
        loop {
            if let Ok(mut connection) = connect(&events_config.daemon_socket_path).await {
                let result: Result<()> = async {
                    write_ipc_message(
                        &mut connection,
                        &IpcMessage {
                            id: 1,
                            payload: DaemonRequest::SubscribeEvents,
                        },
                    )
                    .await?;
                    let _: IpcMessage<DaemonResponse> = read_ipc_message(&mut connection).await?;
                    events_shared.update(|s| s.connected = true);
                    // Reconcile persisted history after subscription, including reconnects.
                    events_notify.notify_one();
                    let bot = events_shared
                        .state
                        .lock()
                        .map(|s| s.dm_bot_id.clone())
                        .unwrap_or_default();
                    if !bot.is_empty()
                        && let Ok(DaemonResponse::JsonPayload(value)) = rpc(
                            &events_config,
                            DaemonRequest::GetDirectChatHistory {
                                bot_id: bot.clone(),
                                before_seq: None,
                                limit: Some(50),
                            },
                        )
                        .await
                    {
                        events_shared.update(|s| {
                            s.merge_dm_history(&bot, &value);
                        });
                    }
                    loop {
                        let response: IpcMessage<DaemonResponse> =
                            read_ipc_message(&mut connection).await?;
                        if let DaemonResponse::Event(event) = response.payload {
                            let wake_refresh = matches!(
                                event,
                                DaemonEvent::ActivityUpdated { .. }
                                    | DaemonEvent::TaskFinished { .. }
                                    | DaemonEvent::BotUpdated { .. }
                                    | DaemonEvent::RoutineUpdated { .. }
                                    | DaemonEvent::RoutineRunRecorded { .. }
                                    | DaemonEvent::TaskUpdated { .. }
                                    | DaemonEvent::BudgetUpdated { .. }
                            );
                            events_shared.update(|s| s.event(event));
                            if wake_refresh {
                                events_notify.notify_one();
                            }
                        }
                    }
                }
                .await;
                if let Err(error) = result {
                    events_shared.update(|s| {
                        s.connected = false;
                        s.error = Some(error.to_string());
                    });
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });

    // 2. Event-driven Bootstrap & Refresh worker (ZERO periodic timer ticks when idle -> 0 Hz idle CPU)
    let refresh_shared = shared.clone();
    let refresh_config = config.clone();
    let poll_notify = Arc::clone(&refresh_notify);
    tokio::spawn(async move {
        loop {
            poll_notify.notified().await;
            if let Ok(DaemonResponse::JsonPayload(value)) =
                rpc(&refresh_config, DaemonRequest::GetBootstrapState).await
            {
                refresh_shared.update(|s| s.apply_bootstrap(value));
            }
        }
    });

    // 3. Adaptive 2–5 FPS Live Computer Preview worker (0% overhead when closed; runs ONLY while viewer is open)
    let preview_shared = shared.clone();
    let preview_config = config.clone();
    tokio::spawn(async move {
        loop {
            if !live_computer_active.load(Ordering::Relaxed) {
                live_computer_notify.notified().await;
                continue;
            }
            if let Ok(DaemonResponse::JsonPayload(value)) = rpc(
                &preview_config,
                DaemonRequest::CaptureComputerPreview {
                    max_long_edge: Some(1280),
                    quality: Some(75),
                },
            )
            .await
                && let (Some(w), Some(h), Some(seq), Some(b64)) = (
                    value["width"].as_u64(),
                    value["height"].as_u64(),
                    value["timestamp_ms"].as_u64(),
                    value["jpeg_base64"].as_str(),
                )
            {
                preview_shared.update(|s| {
                    s.computer_frame_meta = Some((w as u32, h as u32, seq));
                    s.pending_computer_frame_jpeg =
                        Some((w as u32, h as u32, seq, b64.to_string()));
                });
            }
            tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        }
    });

    // 4. Outbound RPC dispatcher (bounded 4-slot concurrency semaphore)
    let slots = Arc::new(tokio::sync::Semaphore::new(4));
    while let Some((request, draft)) = requests.recv().await {
        let Ok(permit) = slots.clone().acquire_owned().await else {
            break;
        };
        let request_shared = shared.clone();
        let request_config = config.clone();
        let request_notify = Arc::clone(&refresh_notify);
        tokio::spawn(async move {
            let _permit = permit;
            let kind = pending_kind(&request);
            match rpc(&request_config, request).await {
                Ok(response) => {
                    let mut refresh_after = false;
                    request_shared.update(|s| {
                        if let Some(draft) = draft {
                            s.sending = false;
                            if !matches!(&response, DaemonResponse::Error(_)) {
                                s.clear_drafts.push(draft);
                            }
                        }
                        s.error = None;
                        match (kind, response) {
                            (PendingKind::Avatar(id), DaemonResponse::JsonPayload(value)) => {
                                s.tool_results.insert(format!("avatar:{id}"), value);
                            }
                            (PendingKind::ToolResult(key), DaemonResponse::JsonPayload(value)) => {
                                s.tool_results.insert(key, value);
                            }
                            (
                                PendingKind::RevealWebhook(id),
                                DaemonResponse::JsonPayload(value),
                            ) => {
                                if let Some(secret) = value["webhook_key"].as_str() {
                                    s.revealed_secret =
                                        Some((format!("webhook:{id}"), secret.into()));
                                }
                            }
                            (PendingKind::Bootstrap, DaemonResponse::JsonPayload(value)) => {
                                s.apply_bootstrap(value);
                            }
                            (PendingKind::ImportAttachment, DaemonResponse::JsonPayload(value)) => {
                                if let (Some(bot), Some(path)) =
                                    (value["bot_id"].as_str(), value["attachment_path"].as_str())
                                {
                                    s.attachment_paths.push((bot.into(), path.into()));
                                }
                            }
                            (PendingKind::ListBotFiles, DaemonResponse::JsonPayload(value)) => {
                                s.files = value;
                            }
                            (PendingKind::LibraryPage, DaemonResponse::JsonPayload(value)) => {
                                s.library_page = value;
                            }
                            (
                                PendingKind::PreviewBotFile,
                                DaemonResponse::JsonPayload(mut value),
                            ) => {
                                if let Some(text) = value["text"].as_str()
                                    && text.len() > 64 * 1024
                                {
                                    let end = text.floor_char_boundary(64 * 1024);
                                    value["text"] = Value::String(format!(
                                        "{}\n[Preview limited to 64 KiB]",
                                        &text[..end]
                                    ));
                                }
                                if value["bytes_base64"].is_string() {
                                    if let Some(path) = value["path"].as_str() {
                                        s.tool_results
                                            .insert(format!("lightbox:sandbox://{path}"), value);
                                    }
                                } else {
                                    s.file_preview = value;
                                }
                            }
                            (
                                PendingKind::ChannelHistory(channel_id),
                                DaemonResponse::JsonPayload(value),
                            ) => {
                                if s.channel_history_id == channel_id {
                                    s.channel_messages =
                                        value.as_array().cloned().unwrap_or_default();
                                }
                            }
                            (
                                PendingKind::DirectChatHistory(bot_id),
                                DaemonResponse::JsonPayload(value),
                            ) => {
                                s.merge_dm_history(&bot_id, &value);
                            }
                            (
                                PendingKind::PostDirectChat(bot_id),
                                DaemonResponse::JsonPayload(value),
                            ) => {
                                for key in ["user_message", "assistant_message"] {
                                    let msg = value[key].clone();
                                    let id = msg["id"].as_str().unwrap_or("").to_string();
                                    if s.dm_bot_id == bot_id
                                        && !id.is_empty()
                                        && !s
                                            .dm_messages
                                            .iter()
                                            .any(|m| m["id"].as_str() == Some(id.as_str()))
                                    {
                                        s.dm_messages.push(msg);
                                    }
                                }
                                s.active_chat = None;
                                s.streaming.clear();
                                if value["queued_task"].is_object() {
                                    refresh_after = true;
                                }
                            }
                            (PendingKind::SearchMessages, DaemonResponse::JsonPayload(value)) => {
                                s.search_hits = value["hits"]
                                    .as_array()
                                    .or_else(|| value.as_array())
                                    .cloned()
                                    .unwrap_or_default();
                            }
                            (PendingKind::ListRoutines, DaemonResponse::JsonPayload(value)) => {
                                s.routines = value.as_array().cloned().unwrap_or_default();
                            }
                            (PendingKind::ListSections, DaemonResponse::JsonPayload(value)) => {
                                s.sections = value.as_array().cloned().unwrap_or_default();
                            }
                            (PendingKind::Mutation, _) => {
                                refresh_after = true;
                            }
                            (PendingKind::ListRules, DaemonResponse::JsonPayload(value)) => {
                                s.rules = value.as_array().cloned().unwrap_or_default();
                            }
                            (PendingKind::ListVaultItems, DaemonResponse::JsonPayload(value)) => {
                                s.vault_items = value.as_array().cloned().unwrap_or_default();
                            }
                            (
                                PendingKind::RevealVaultItem(id),
                                DaemonResponse::JsonPayload(value),
                            ) => {
                                if let Some(secret) = value["secret"].as_str() {
                                    s.revealed_secret = Some((id, secret.to_string()));
                                }
                            }
                            (PendingKind::UsageBreakdown, DaemonResponse::JsonPayload(value)) => {
                                s.usage_data = value;
                            }
                            (PendingKind::ListConnectors, DaemonResponse::JsonPayload(value)) => {
                                s.connectors_data = value;
                            }
                            (
                                PendingKind::CaptureComputerPreview,
                                DaemonResponse::JsonPayload(value),
                            ) => {
                                if let (Some(w), Some(h), Some(seq), Some(b64)) = (
                                    value["width"].as_u64(),
                                    value["height"].as_u64(),
                                    value["timestamp_ms"].as_u64(),
                                    value["jpeg_base64"].as_str(),
                                ) {
                                    s.computer_frame_meta = Some((w as u32, h as u32, seq));
                                    s.pending_computer_frame_jpeg =
                                        Some((w as u32, h as u32, seq, b64.to_string()));
                                }
                            }
                            (PendingKind::ListActivity, DaemonResponse::JsonPayload(value)) => {
                                s.activity_posts =
                                    value["posts"].as_array().cloned().unwrap_or_default();
                                s.brief_prompt = value["brief_prompt"].as_str().map(str::to_string);
                            }
                            (PendingKind::RateActivity, DaemonResponse::JsonPayload(_)) => {
                                s.activity_notice = "Feedback recorded".into();
                                refresh_after = true;
                            }
                            (PendingKind::ListGoals, DaemonResponse::JsonPayload(value)) => {
                                s.goals = value.as_array().cloned().unwrap_or_default();
                            }
                            (PendingKind::SaveGoal, DaemonResponse::JsonPayload(_)) => {
                                s.goal_notice = "Goal updated".into();
                                refresh_after = true;
                            }
                            (PendingKind::ListSpaces, DaemonResponse::JsonPayload(value)) => {
                                s.spaces_data = value;
                            }
                            (PendingKind::CreateSpace, DaemonResponse::JsonPayload(value)) => {
                                let slug = value["space"]["slug"].as_str().unwrap_or("space");
                                s.spaces_notice = format!("Created Space '{slug}'");
                                refresh_after = true;
                            }
                            (PendingKind::SearchSkills, DaemonResponse::JsonPayload(value)) => {
                                s.skills_data = value;
                            }
                            (PendingKind::ReadRuntimeFile, DaemonResponse::JsonPayload(value)) => {
                                let file_name =
                                    value["file"].as_str().unwrap_or("SOUL.md").to_string();
                                let exists = value["exists"].as_bool().unwrap_or(false);
                                s.runtime_file_bot =
                                    value["bot_id"].as_str().unwrap_or("muse").into();
                                s.runtime_file_name = file_name.clone();
                                s.runtime_file_content =
                                    value["content"].as_str().unwrap_or("").into();
                                s.runtime_file_sha256 =
                                    value["sha256"].as_str().map(str::to_string);
                                s.runtime_file_dirty = true;
                                s.identity_notice = if exists {
                                    format!("Loaded {file_name}")
                                } else {
                                    format!("{file_name} does not exist yet (created on save)")
                                };
                            }
                            (PendingKind::SaveRuntimeFile, DaemonResponse::JsonPayload(value)) => {
                                if let Some(sha) = value["sha256"].as_str() {
                                    s.runtime_file_sha256 = Some(sha.to_string());
                                }
                                let file_name = value["file"].as_str().unwrap_or("file");
                                s.identity_notice = format!("Saved {file_name}");
                            }
                            (PendingKind::ImportMemory, DaemonResponse::JsonPayload(value)) => {
                                let applied = value["applied"].as_bool().unwrap_or(false);
                                let additions =
                                    value["additions"].as_array().map_or(0, |arr| arr.len());
                                s.memory_import_report = Some(value);
                                s.identity_notice = if applied {
                                    refresh_after = true;
                                    format!("Imported {additions} facts into memory")
                                } else {
                                    format!("Dry-run ready ({additions} facts extracted)")
                                };
                            }
                            (PendingKind::ForgetPlan, DaemonResponse::JsonPayload(value)) => {
                                s.forget_plan = Some(value);
                                s.forget_report = None;
                                s.forget_notice =
                                    "Plan prepared. Review counts and confirm to erase.".into();
                            }
                            (PendingKind::ForgetConfirm, DaemonResponse::JsonPayload(value)) => {
                                let msg = value["message"]
                                    .as_str()
                                    .unwrap_or("Forget completed")
                                    .to_string();
                                s.forget_notice = msg;
                                s.tool_results.insert("forget_report".into(), value.clone());
                                s.forget_report = Some(value);
                                s.forget_plan = None;
                                refresh_after = true;
                            }
                            (PendingKind::UpdateDiscretion, DaemonResponse::JsonPayload(value)) => {
                                s.discretion_policy = Some(value);
                                s.discretion_notice = "Discretion rules saved.".into();
                            }
                            (_, DaemonResponse::Accepted { id }) => {
                                s.messages.push(("Queued".into(), id));
                                refresh_after = true;
                            }
                            (_, DaemonResponse::ChatReplyV1 { message_id, text })
                                if s.completed_chat.as_ref() != Some(&message_id) =>
                            {
                                s.finish_chat(message_id, text);
                            }
                            _ => {
                                refresh_after = true;
                            }
                        }
                    });
                    if refresh_after {
                        request_notify.notify_one();
                    }
                }
                Err(error) => {
                    let msg = error.to_string();
                    request_shared.update(|s| {
                        if let Some((_, draft)) = &draft
                            && s.completed_chat != s.active_chat
                            && let Some((role, _)) = s
                                .messages
                                .iter_mut()
                                .rev()
                                .find(|(role, text)| role == "You" && text == draft)
                        {
                            *role = "Unsent".into();
                        }
                        if draft.is_some() {
                            s.sending = false;
                            if !s.streaming.is_empty() {
                                let partial = std::mem::take(&mut s.streaming);
                                s.messages.push(("Interrupted response".into(), partial));
                            }
                        }
                        match kind {
                            PendingKind::ListActivity | PendingKind::RateActivity => {
                                s.activity_notice = msg.clone();
                            }
                            PendingKind::ListGoals | PendingKind::SaveGoal => {
                                s.goal_notice = msg.clone();
                            }
                            PendingKind::ListSpaces | PendingKind::CreateSpace => {
                                s.spaces_notice = msg.clone();
                            }
                            PendingKind::ReadRuntimeFile
                            | PendingKind::SaveRuntimeFile
                            | PendingKind::ImportMemory => {
                                s.identity_notice = msg.clone();
                            }
                            PendingKind::ForgetPlan | PendingKind::ForgetConfirm => {
                                s.forget_notice = msg.clone();
                            }
                            PendingKind::UpdateDiscretion => {
                                s.discretion_notice = msg.clone();
                            }
                            _ => {}
                        }
                        s.error = Some(msg);
                    });
                }
            }
        });
    }
}

pub(crate) fn take_composer_shortcut(ctx: &egui::Context) -> bool {
    ctx.memory(|m| m.has_focus(egui::Id::new("main-draft")))
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter))
}

fn conversation_context(messages: &[(String, String)]) -> Vec<ChatTurn> {
    let mut bytes = 0;
    let mut turns = Vec::new();
    for (role, text) in messages.iter().rev() {
        let role = match role.as_str() {
            "You" => "user",
            "Muse" => "assistant",
            _ => continue,
        };
        if turns.len() >= 24 || bytes + text.len() > 64 * 1024 {
            break;
        }
        bytes += text.len();
        turns.push(ChatTurn {
            role: role.into(),
            content: text.clone(),
        });
    }
    turns.reverse();
    turns
}

pub fn run(config: GmConfig, config_path: std::path::PathBuf) -> Result<()> {
    let runtime = tokio::runtime::Handle::current();
    eframe::run_native(
        "Grok Muse",
        eframe::NativeOptions {
            #[cfg(feature = "hardware-wgpu")]
            renderer: if config.ui.hardware_acceleration {
                eframe::Renderer::Wgpu
            } else {
                eframe::Renderer::Glow
            },
            #[cfg(not(feature = "hardware-wgpu"))]
            renderer: eframe::Renderer::Glow,
            hardware_acceleration: if config.ui.hardware_acceleration {
                eframe::HardwareAcceleration::Preferred
            } else {
                eframe::HardwareAcceleration::Off
            },
            vsync: true,
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1220.0, 800.0])
                .with_min_inner_size([680.0, 480.0]),
            ..Default::default()
        },
        Box::new(move |creation| {
            let system_dark = creation
                .egui_ctx
                .system_theme()
                .is_none_or(|theme| theme == egui::Theme::Dark);
            let initial_palette = GrokPalette::resolve(
                &config.ui.theme,
                system_dark,
                &config.ui.accent_mode,
                "#3B82F6",
            );
            initial_palette.apply_to_context(&creation.egui_ctx);

            let (session, mut receiver, session_error) =
                match SessionStore::open(&config.storage_dir) {
                    Ok((store, receiver)) => (Some(store), Some(receiver), None),
                    Err(e) => (None, None, Some(e.to_string())),
                };
            let restored = session
                .as_ref()
                .map(|s| s.data.lock().unwrap_or_else(|e| e.into_inner()).clone())
                .unwrap_or_else(SessionData::default);
            let shared = Shared {
                state: Arc::new(Mutex::new(ViewState {
                    messages: restored.messages.clone(),
                    dm_bot_id: restored.bot.clone(),
                    error: session_error,
                    ..Default::default()
                })),
                context: creation.egui_ctx.clone(),
                session: session.clone(),
            };
            if let (Some(store), Some(mut changes)) = (session, receiver.take()) {
                let writer_shared = shared.clone();
                runtime.spawn(async move {
                    while changes.changed().await.is_ok() {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        changes.borrow_and_update();
                        let next = store.clone();
                        match tokio::task::spawn_blocking(move || next.flush()).await {
                            Ok(Ok(())) => {}
                            other => writer_shared.update(|s| {
                                s.error =
                                    Some(format!("Conversation could not be saved: {other:?}"))
                            }),
                        }
                    }
                });
            }
            let settings_config = config.clone();
            let config_original = std::fs::read(&config_path).ok();
            let (sender, receiver) = tokio::sync::mpsc::channel(64);
            let live_computer_active = Arc::new(AtomicBool::new(false));
            let live_computer_notify = Arc::new(tokio::sync::Notify::new());
            runtime.spawn(background(
                config,
                shared.clone(),
                receiver,
                Arc::clone(&live_computer_active),
                Arc::clone(&live_computer_notify),
            ));
            let identity_bot = restored.bot.clone();
            let draft_target = restored
                .draft_target
                .unwrap_or_else(|| format!("dm:{}", restored.bot));
            crate::ui::theme::install_system_font(&creation.egui_ctx);
            let mut ui_state = GrokUiState::default();
            ui_state.sidebar_width = settings_config.ui.sidebar_width.clamp(240, 480) as f32;
            ui_state.details_width = settings_config.ui.details_width.clamp(320, 600) as f32;
            ui_state.sidebar_saved_width = ui_state.sidebar_width;
            ui_state.details_saved_width = ui_state.details_width;
            ui_state.sidebar_manual_override = Some(!settings_config.ui.sidebar_collapsed);
            if config_original.is_none() {
                ui_state.settings_pane = crate::ui::state::SettingsPane::Providers;
                ui_state.active_modal = Some(crate::ui::state::ActiveModal::Settings);
            }
            Ok(Box::new(NativeApp {
                shared,
                requests: sender,
                live_computer_active,
                live_computer_notify,
                ui_state,
                textures: BoundedTextureCache::new(),
                layout_cache: MarkdownLayoutCache::default(),
                avatar_assets: std::collections::HashMap::new(),
                lightbox_requested: None,
                draft: restored.draft,
                draft_target,
                other_drafts: restored.other_drafts,
                bot: restored.bot,
                task_mode: restored.task_mode,
                search: String::new(),
                resume_note: String::new(),
                goal_title: String::new(),
                goal_description: String::new(),
                goal_category: String::new(),
                goal_parent: String::new(),
                space_slug: String::new(),
                space_title: String::new(),
                space_html: String::new(),
                identity_bot,
                identity_file: "SOUL.md".into(),
                identity_text: String::new(),
                identity_sha256: None,
                memory_import_text: String::new(),
                forget_request: String::new(),
                channel: "general".into(),
                new_channel: String::new(),
                settings_config,
                config_path,
                config_original,
                settings_notice: String::new(),
            }))
        }),
    )
    .map_err(|e| GmError::Platform(format!("Native UI could not start: {e}")))
}

#[cfg(test)]
mod bootstrap_regression_tests {
    use super::*;
    #[test]
    fn chat_history_reconciles_without_losing_events_or_switching_bots() {
        let mut state = ViewState::default();
        let select = |bot: &str| DaemonRequest::GetDirectChatHistory {
            bot_id: bot.into(),
            before_seq: None,
            limit: Some(50),
        };
        state.select_chat_request(&select("muse"));
        state
            .dm_messages
            .push(serde_json::json!({"id":"new", "seq":2}));
        let page = serde_json::json!({"messages":[{"id":"old", "seq":1},{"id":"new", "seq":2}]});
        state.merge_dm_history("muse", &page);
        state.merge_dm_history("muse", &page);
        assert_eq!(state.dm_messages.len(), 2);
        assert_eq!(state.dm_messages[0]["id"], "old");
        state.select_chat_request(&select("other"));
        state.merge_dm_history("muse", &page);
        assert_eq!(state.dm_bot_id, "other");
        assert!(state.dm_messages.is_empty());
    }

    #[test]
    fn bootstrap_uses_store_contract_and_normalizes_spaces() {
        let mut state = ViewState::default();
        state.apply_bootstrap(serde_json::json!({
            "recent_tasks": [{"id":"task"}], "custom_rules": [{"id":"rule"}],
            "integrations": [{"id":"github"}], "custom_connectors": [{"id":"mcp"}],
            "feed": {"posts":[{"id":"post"}], "brief_prompt":"daily"},
            "status": {"paused":true, "spaces_port":1234},
            "spaces": [{"meta":{"slug":"demo"}, "url":"http://127.0.0.1:1234/spaces/v2/demo/?token=abc"}],
            "goals": [{"slug":"goal"}], "skills": [{"name":"skill"}],
            "ui": gm_core::config::UiConfig::default(), "version":"test",
            "unread_counts": {"dm:muse":3}, "connected_app_count":2
        }));
        assert_eq!(state.tasks[0]["id"], "task");
        assert_eq!(state.rules[0]["id"], "rule");
        assert_eq!(state.activity_posts[0]["id"], "post");
        assert_eq!(state.brief_prompt.as_deref(), Some("daily"));
        assert_eq!(state.connectors_data["integrations"][0]["id"], "github");
        assert_eq!(state.spaces_data["spaces"][0]["slug"], "demo");
        assert_eq!(state.spaces_data["spaces_port"], 1234);
        assert!(state.spaces_data["urls"]["demo"].is_string());
        assert_eq!(state.unread_counts["dm:muse"], 3);
        assert!(state.paused);
        assert!(state.ui_config.is_some());
        assert_eq!(state.skills_data[0]["name"], "skill");
        assert_eq!(state.connected_app_count, 2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chat_reply_races_and_other_clients_never_duplicate_or_mix_answers() {
        for response_first in [true, false] {
            let mut state = ViewState {
                active_chat: Some("mine".into()),
                ..Default::default()
            };
            state.event(DaemonEvent::ChatTokenV1 {
                message_id: "another-client".into(),
                delta: "unrelated".into(),
                done: true,
            });
            assert!(state.messages.is_empty());
            if response_first {
                state.finish_chat("mine".into(), "one answer".into());
            }
            state.event(DaemonEvent::ChatTokenV1 {
                message_id: "mine".into(),
                delta: "one answer".into(),
                done: false,
            });
            state.event(DaemonEvent::ChatTokenV1 {
                message_id: "mine".into(),
                delta: String::new(),
                done: true,
            });
            if !response_first {
                state.finish_chat("mine".into(), "one answer".into());
            }
            assert_eq!(state.messages, vec![("Muse".into(), "one answer".into())]);
            assert!(state.streaming.is_empty());
        }
    }
    #[test]
    fn expired_or_cancelled_approvals_disappear() {
        let mut state = ViewState::default();
        state.event(DaemonEvent::ApprovalRequested {
            request_id: "one".into(),
            title: "Review".into(),
            details: "one action".into(),
        });
        state.event(DaemonEvent::ApprovalResolved {
            request_id: "one".into(),
        });
        assert!(state.approvals.is_empty());
    }
    #[test]
    fn activity_updated_event_sets_notice() {
        let mut state = ViewState::default();
        state.event(DaemonEvent::ActivityUpdated {
            reason: "feed_post_added".into(),
        });
        assert_eq!(state.activity_notice, "Updated (feed_post_added)");
    }
    #[test]
    fn pending_kind_maps_muse_and_grokbot_ipc_requests() {
        assert_eq!(
            pending_kind(&DaemonRequest::ListActivity),
            PendingKind::ListActivity
        );
        assert_eq!(
            pending_kind(&DaemonRequest::ListGoals),
            PendingKind::ListGoals
        );
        assert_eq!(
            pending_kind(&DaemonRequest::ListSpaces),
            PendingKind::ListSpaces
        );
        assert_eq!(
            pending_kind(&DaemonRequest::SearchSkills {
                query: "goals".into()
            }),
            PendingKind::SearchSkills
        );
        assert_eq!(
            pending_kind(&DaemonRequest::ChannelHistory {
                channel_id: "general".into()
            }),
            PendingKind::ChannelHistory("general".into())
        );
        assert_eq!(
            pending_kind(&DaemonRequest::GetBootstrapState),
            PendingKind::Bootstrap
        );
        assert_eq!(
            pending_kind(&DaemonRequest::GetDirectChatHistory {
                bot_id: "muse".into(),
                before_seq: None,
                limit: Some(50)
            }),
            PendingKind::DirectChatHistory("muse".into())
        );
        assert_eq!(
            pending_kind(&DaemonRequest::ManageVault {
                op: "reveal".into(),
                item: None,
                id: Some("v1".into()),
                reveal_confirmed: true
            }),
            PendingKind::RevealVaultItem("v1".into())
        );
        assert_eq!(
            pending_kind(&DaemonRequest::ManageSystem {
                op: "pin_library_item".into(),
                payload: serde_json::json!({})
            }),
            PendingKind::Mutation
        );
    }
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;
    #[test]
    fn composer_consumes_send_shortcut_before_multiline_editor() {
        let ctx = egui::Context::default();
        let mut text = "preserve draft".to_string();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.add(egui::TextEdit::multiline(&mut text).id(egui::Id::new("main-draft")))
                    .request_focus();
            });
        });
        let modifiers = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let raw = egui::RawInput {
            modifiers,
            events: vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
            ..Default::default()
        };
        let mut submitted = false;
        let _ = ctx.run(raw, |ctx| {
            submitted = take_composer_shortcut(ctx);
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.add(egui::TextEdit::multiline(&mut text).id(egui::Id::new("main-draft")));
            });
        });
        assert!(submitted);
        assert_eq!(text, "preserve draft");
        let _ = ctx.run(Default::default(), |ctx| {
            ctx.memory_mut(|m| m.surrender_focus(egui::Id::new("main-draft")));
            assert!(!take_composer_shortcut(ctx));
        });
    }
}
