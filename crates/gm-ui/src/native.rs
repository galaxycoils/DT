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
    rail::draw_workspace_rail,
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
use session::{SessionData, SessionStore};
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
        | DaemonRequest::CreateChannel { .. } => PendingKind::Mutation,
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
        DaemonRequest::UpdateDiscretion { .. } => PendingKind::UpdateDiscretion,
        _ => PendingKind::Other,
    }
}

#[derive(Default)]
struct ViewState {
    status: String,
    paused: bool,
    credentials_notice: String,
    credentials_saving: bool,
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
    clear_draft: Option<String>,
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
}

impl ViewState {
    fn finish_chat(&mut self, id: String, text: String) {
        if self.active_chat.as_ref() != Some(&id) || self.completed_chat.as_ref() == Some(&id) {
            return;
        }
        self.streaming.clear();
        self.messages.push(("Muse".into(), text));
        self.completed_chat = Some(id);
    }

    fn apply_bootstrap(&mut self, value: Value) {
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
        if let Some(arr) = value["tasks"].as_array() {
            self.tasks = arr.clone();
        }
        if let Some(arr) = value["routines"].as_array() {
            self.routines = arr.clone();
        }
        if let Some(arr) = value["rules"].as_array() {
            self.rules = arr.clone();
        }
        if let Some(arr) = value["vault_items"].as_array() {
            self.vault_items = arr.clone();
        }
        if !value["usage"].is_null() {
            self.usage_data = value["usage"].clone();
        }
        if !value["connectors"].is_null() {
            self.connectors_data = value["connectors"].clone();
        }
        if let Some(arr) = value["goals"].as_array() {
            self.goals = arr.clone();
        }
        if !value["spaces"].is_null() {
            self.spaces_data = value["spaces"].clone();
        }
        if let Some(arr) = value["activity"]["posts"].as_array() {
            self.activity_posts = arr.clone();
        }
        self.brief_prompt = value["activity"]["brief_prompt"]
            .as_str()
            .map(str::to_string);
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
            DaemonEvent::ApprovalResolved { request_id } => {
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
                for b in &mut self.bots {
                    if b["id"].as_str() == Some(bot_id.as_str()) {
                        b["status_phrase"] = phrase.clone().map(Value::String).unwrap_or(Value::Null);
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

struct NativeApp {
    shared: Shared,
    requests: tokio::sync::mpsc::Sender<(DaemonRequest, Option<String>)>,
    live_computer_active: Arc<AtomicBool>,
    live_computer_notify: Arc<tokio::sync::Notify>,
    ui_state: GrokUiState,
    textures: BoundedTextureCache,
    layout_cache: MarkdownLayoutCache,
    draft: String,
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
    meta_key: String,
    xai_key: String,
}

impl NativeApp {
    /// Detached request sink (cloned channel handles) so panels can enqueue requests while
    /// `self` fields are mutably borrowed.
    fn sender(&self) -> impl FnMut(DaemonRequest) + 'static {
        let requests = self.requests.clone();
        let shared = self.shared.clone();
        move |request| {
            if let Err(error) = requests.try_send((request, None)) {
                shared.update(|s| s.error = Some(format!("Request queue is busy: {error}")));
            }
        }
    }

    fn send(&self, request: DaemonRequest) {
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

        if !dm_messages.is_empty() {
            let mut rows: Vec<TranscriptRow> = dm_messages
                .iter()
                .enumerate()
                .map(|(idx, m)| {
                    let role = m["role"].as_str().unwrap_or("assistant").to_string();
                    let author = m["author"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| {
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
            // Append any local assistant replies from SendChatMessageV2 not yet in dm_messages
            for (idx, (role_label, content)) in local_messages.iter().enumerate() {
                if role_label != "You"
                    && !rows.iter().any(|r| r.content == *content)
                {
                    let seq = rows.len() as i64 + 1;
                    rows.push(TranscriptRow {
                        id: format!("local-{idx}"),
                        seq,
                        author: role_label.clone(),
                        role: "assistant".into(),
                        content: content.clone(),
                        created_at: String::new(),
                        reply_to_id: None,
                        request_id: None,
                        reactions: Vec::new(),
                        card: None,
                    });
                }
            }
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

impl Drop for NativeApp {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.meta_key.zeroize();
        self.xai_key.zeroize();
    }
}

impl eframe::App for NativeApp {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(store) = &self.shared.session {
            store.edit(|d| {
                d.draft = self.draft.clone();
                d.bot = self.bot.clone();
                d.task_mode = self.task_mode;
            });
            if let Err(error) = store.flush() {
                tracing::error!(%error, "Could not save native conversation");
            }
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 1. Handle global GrokBot hotkeys & 5-tier Escape stack
        let hotkey_outcome = handle_global_hotkeys(ctx, &mut self.ui_state);

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
            paused,
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
            credentials_notice,
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
            if let Some(sent) = state.clear_draft.take()
                && self.draft.trim() == sent
            {
                self.draft.clear();
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
                state.credentials_notice.clone(),
                state.computer_frame_meta,
                state.pending_computer_frame_jpeg.take(),
            )
        };

        // 4. Handle Cmd/Ctrl + 1..9 bot quick-switch
        if let Some(bot_idx) = hotkey_outcome.switch_to_bot_index {
            let visible_bots: Vec<&Value> = bots
                .iter()
                .filter(|b| !b["hidden"].as_bool().unwrap_or(false))
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
            && let Ok(decoded) = image::load_from_memory(&bytes)
        {
            let rgba = decoded.to_rgba8();
            let (width, height) = rgba.dimensions();
            self.textures.insert_rgba(
                ctx,
                "live_computer:latest".to_string(),
                width as usize,
                height as usize,
                rgba.into_raw(),
            );
        }

        // 6. Resolve & apply GrokBot Theme Palette
        let active_bot_color_hex = bots
            .iter()
            .find(|b| b["id"].as_str() == Some(self.bot.as_str()))
            .and_then(|b| b["color"].as_str())
            .unwrap_or("#3B82F6");
        let system_prefers_dark = ctx.style().visuals.dark_mode;
        let palette = GrokPalette::resolve(
            &self.settings_config.ui.theme,
            system_prefers_dark,
            &self.settings_config.ui.accent_mode,
            active_bot_color_hex,
        );
        palette.apply_to_context(ctx);

        let mut outbound = self.sender();
        // 7. Left 48px Workspace Rail (or bottom bar on < 768px)
        let unread_feed = activity_posts
            .iter()
            .filter(|p| p["rating"].is_null())
            .count();
        draw_workspace_rail(
            ctx,
            &mut self.ui_state,
            &palette,
            unread_feed,
            paused,
            &mut outbound,
        );

        // 8. Adaptive Bot / Group / Section Sidebar (`AutoCollapsed` <=1, `CompactRail` 2..=4, `FullList` 5+)
        if self.ui_state.section == WorkspaceSection::Bots {
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
        }

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
                        if self.ui_state.active_chat_kind == "channel" {
                            let req = DaemonRequest::ChannelPost {
                                channel_id: self.channel.clone(),
                                author: "human".into(),
                                content: text.clone(),
                            };
                            let _ = self.requests.try_send((req, Some(text)));
                        } else if self.task_mode || text.starts_with("/do ") {
                            let instruction =
                                text.strip_prefix("/do ").unwrap_or(&text).trim().to_string();
                            let req = DaemonRequest::QueueTask {
                                bot_id: Some(self.bot.clone()),
                                channel_id: Some(self.channel.clone()),
                                instruction,
                                priority: 0,
                            };
                            if self.requests.try_send((req, Some(text.clone()))).is_ok() {
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

        // 12. Global Modals (`CommandPalette`, 5-Pane `Settings`, `Studio`, `Shortcuts`, `ConnectApps`, `AvatarPicker`, `Lightbox`, `ExpandedTable`)
        let mut do_save_keys = false;
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
            &mut self.meta_key,
            &mut self.xai_key,
            &credentials_notice,
            &mut self.settings_notice,
            || do_save_keys = true,
            || do_save_config = true,
            |req| modal_reqs.push(req),
        );
        for req in modal_reqs {
            self.send(req);
        }
        if do_save_keys {
            let meta = zeroize::Zeroizing::new(std::mem::take(&mut self.meta_key));
            let xai = zeroize::Zeroizing::new(std::mem::take(&mut self.xai_key));
            let shared = self.shared.clone();
            shared.update(|s| {
                s.credentials_saving = true;
                s.credentials_notice = "Saving to the OS credential store…".into();
            });
            tokio::task::spawn_blocking(move || {
                let keys = gm_llm::KeychainSecrets::default();
                let result = (|| {
                    if !meta.is_empty() {
                        keys.set_meta_key(&meta)?;
                    }
                    if !xai.is_empty() {
                        keys.set_xai_key(&xai)?;
                    }
                    Ok::<_, GmError>(())
                })();
                shared.update(|s| {
                    s.credentials_saving = false;
                    s.credentials_notice = match result {
                        Ok(()) => {
                            "API keys saved in OS Keychain. Restart daemon to apply.".into()
                        }
                        Err(e) => e.to_string(),
                    };
                });
            });
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

        // 14. Persist draft/bot/task_mode changes to private session file without triggering repaint
        if let Some(store) = &self.shared.session {
            let changed = {
                let d = store.data.lock().unwrap_or_else(|e| e.into_inner());
                d.draft != self.draft || d.bot != self.bot || d.task_mode != self.task_mode
            };
            if changed {
                store.edit(|d| {
                    d.draft = self.draft.clone();
                    d.bot = self.bot.clone();
                    d.task_mode = self.task_mode;
                });
            }
        }
    }
}

async fn background(
    config: GmConfig,
    shared: Shared,
    mut requests: tokio::sync::mpsc::Receiver<(DaemonRequest, Option<String>)>,
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
                    // Trigger single bootstrap load on connection
                    events_notify.notify_one();
                    loop {
                        let response: IpcMessage<DaemonResponse> =
                            read_ipc_message(&mut connection).await?;
                        if let DaemonResponse::Event(event) = response.payload {
                            let wake_refresh = matches!(
                                event,
                                DaemonEvent::ActivityUpdated { .. }
                                    | DaemonEvent::TaskFinished { .. }
                                    | DaemonEvent::BotUpdated { .. }
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
            if let Ok(DaemonResponse::JsonPayload(value)) = rpc(
                &refresh_config,
                DaemonRequest::GetBootstrapState,
            )
            .await
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
                        if draft.is_some() {
                            s.sending = false;
                            s.clear_draft = draft.clone();
                        }
                        s.error = None;
                        match (kind, response) {
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
                                s.file_preview = value;
                            }
                            (
                                PendingKind::ChannelHistory(channel_id),
                                DaemonResponse::JsonPayload(value),
                            ) => {
                                s.channel_history_id = channel_id;
                                s.channel_messages = value.as_array().cloned().unwrap_or_default();
                            }
                            (
                                PendingKind::DirectChatHistory(bot_id),
                                DaemonResponse::JsonPayload(value),
                            ) => {
                                s.dm_bot_id = bot_id;
                                s.dm_messages = value["messages"]
                                    .as_array()
                                    .or_else(|| value.as_array())
                                    .cloned()
                                    .unwrap_or_default();
                            }
                            (
                                PendingKind::PostDirectChat(bot_id),
                                DaemonResponse::JsonPayload(value),
                            ) => {
                                s.dm_bot_id = bot_id;
                                for key in ["user_message", "assistant_message"] {
                                    let msg = value[key].clone();
                                    let id = msg["id"].as_str().unwrap_or("").to_string();
                                    if !id.is_empty()
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
                            (
                                PendingKind::SearchMessages,
                                DaemonResponse::JsonPayload(value),
                            ) => {
                                s.search_hits = value["hits"]
                                    .as_array()
                                    .or_else(|| value.as_array())
                                    .cloned()
                                    .unwrap_or_default();
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
                        if let Some(draft) = &draft
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

#[allow(dead_code)]
fn take_composer_shortcut(ctx: &egui::Context) -> bool {
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
            renderer: eframe::Renderer::Wgpu,
            #[cfg(not(feature = "hardware-wgpu"))]
            renderer: eframe::Renderer::Glow,
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1220.0, 800.0])
                .with_min_inner_size([680.0, 480.0]),
            ..Default::default()
        },
        Box::new(move |creation| {
            let system_dark = creation.egui_ctx.style().visuals.dark_mode;
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
            let mut ui_state = GrokUiState::default();
            if config_original.is_none() {
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
                draft: restored.draft,
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
                meta_key: String::new(),
                xai_key: String::new(),
            }))
        }),
    )
    .map_err(|e| GmError::Platform(format!("Native UI could not start: {e}")))
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
