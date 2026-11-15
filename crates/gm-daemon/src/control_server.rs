use async_trait::async_trait;
use gm_core::config::GmConfig;
use gm_core::error::{GmError, Result};
use gm_core::ipc::{
    DaemonEvent, DaemonRequest, DaemonResponse, IpcMessage, read_ipc_message, write_ipc_message,
};
use gm_llm::{BudgetManager, ChatMessage, HybridLlmClient, LlmRouter, TaskType};
use gm_platform::{IpcPlatformClient, Platform};
use gm_safety::{AuditLogger, BlocklistManager, KillSwitch};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncRead, AsyncWrite};
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::sync::{Mutex, broadcast, oneshot};
#[cfg(unix)]
use tracing::error;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ControlRequestEnvelope {
    Envelope(IpcMessage<DaemonRequest>),
    Raw(DaemonRequest),
}

/// Daemon interaction handler that routes approval requests to the UI / NSAlert sheet via IPC
#[derive(Clone)]
pub struct DaemonApprovalHandler {
    pending_approvals: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    event_tx: broadcast::Sender<DaemonEvent>,
    timeout: std::time::Duration,
}

impl DaemonApprovalHandler {
    pub fn new(
        pending_approvals: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
        event_tx: broadcast::Sender<DaemonEvent>,
        timeout: std::time::Duration,
    ) -> Self {
        Self {
            pending_approvals,
            event_tx,
            timeout,
        }
    }
}

struct PendingApprovalGuard {
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    request_id: String,
    events: broadcast::Sender<DaemonEvent>,
}
impl Drop for PendingApprovalGuard {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.pending.try_lock() {
            pending.remove(&self.request_id);
        } else if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let pending = self.pending.clone();
            let id = self.request_id.clone();
            runtime.spawn(async move {
                pending.lock().await.remove(&id);
            });
        }
        let _ = self.events.send(DaemonEvent::ApprovalResolved {
            request_id: self.request_id.clone(),
        });
    }
}

#[async_trait]
impl gm_agent::InteractionHandler for DaemonApprovalHandler {
    async fn request_approval(&self, req: &gm_agent::ApprovalRequest) -> Result<bool> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        let _guard = PendingApprovalGuard {
            pending: self.pending_approvals.clone(),
            request_id: request_id.clone(),
            events: self.event_tx.clone(),
        };
        self.pending_approvals
            .lock()
            .await
            .insert(request_id.clone(), tx);

        let title = format!("Action Approval Required for Task {}", req.task_id);
        let details = format!(
            "Step {}: Action '{:?}' targeting '{:?}' requires user confirmation. Reason: {}",
            req.step,
            req.action,
            req.target_label.as_deref().unwrap_or("<none>"),
            req.reason
        );

        let _ = self.event_tx.send(DaemonEvent::ApprovalRequested {
            request_id: request_id.clone(),
            title,
            details,
        });

        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(approved)) => Ok(approved),
            Ok(Err(_)) => Ok(false),
            Err(_) => {
                self.pending_approvals.lock().await.remove(&request_id);
                warn!(
                    "Approval request {} timed out after {:?}",
                    request_id, self.timeout
                );
                Ok(false)
            }
        }
    }

    async fn ask_user(&self, _question: &str) -> Result<Option<String>> {
        Ok(None)
    }

    async fn request_wheel_handoff(
        &self,
        req: &gm_agent::WheelHandoffRequest,
    ) -> Result<Option<String>> {
        let request_id = format!("wheel_{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let (tx, rx) = oneshot::channel();
        self.pending_approvals
            .lock()
            .await
            .insert(request_id.clone(), tx);

        let bot_id = req.bot_id.clone().unwrap_or_else(|| "muse".to_string());
        let _ = self.event_tx.send(DaemonEvent::WheelHandoffRequested {
            task_id: req.task_id.clone(),
            bot_id,
            blocker: req.blocker.clone(),
            instructions: req.instructions.clone(),
        });
        let _ = self.event_tx.send(DaemonEvent::ApprovalRequested {
            request_id: request_id.clone(),
            title: format!("🛞 Take the Wheel ({}) — Task {}", req.blocker, req.task_id),
            details: req.instructions.clone(),
        });

        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(true)) => Ok(Some("Resumed via DaemonApprovalHandler".to_string())),
            Ok(Ok(false)) | Ok(Err(_)) => Ok(None),
            Err(_) => {
                self.pending_approvals.lock().await.remove(&request_id);
                Ok(None)
            }
        }
    }
}

#[derive(Clone)]
pub struct DaemonControlServer {
    config: GmConfig,
    paused: Arc<AtomicBool>,
    active_task: Arc<Mutex<Option<String>>>,
    active_task_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    pending_approvals: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    event_tx: broadcast::Sender<DaemonEvent>,
    budget: Arc<BudgetManager>,
    llm_client: Arc<HybridLlmClient>,
    router: LlmRouter,
    kill_switch: KillSwitch,
    platform: Arc<IpcPlatformClient>,
    openbot: Option<gm_openbot::OpenBotSupervisor>,
}

impl DaemonControlServer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: GmConfig,
        budget: Arc<BudgetManager>,
        llm_client: Arc<HybridLlmClient>,
        router: LlmRouter,
        kill_switch: KillSwitch,
        _blocklist: BlocklistManager,
        audit_log: Arc<AuditLogger>,
        platform: Arc<IpcPlatformClient>,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        let openbot = gm_openbot::OpenBotSupervisor::open(
            config.clone(),
            Arc::clone(&audit_log),
            event_tx.clone(),
        )
        .ok();
        Self {
            config,
            paused: Arc::new(AtomicBool::new(false)),
            active_task: Arc::new(Mutex::new(None)),
            active_task_handle: Arc::new(Mutex::new(None)),
            pending_approvals: Arc::new(Mutex::new(HashMap::new())),
            event_tx,
            budget,
            llm_client,
            router,
            kill_switch,
            platform,
            openbot,
        }
    }

    pub fn start_runtime(&self) -> Result<tokio::task::JoinHandle<()>> {
        self.openbot
            .as_ref()
            .ok_or_else(|| GmError::Config("Task runtime could not initialize".into()))?
            .start_runtime(
                self.platform.clone(),
                self.llm_client.clone(),
                self.router.clone(),
                self.kill_switch.clone(),
                BlocklistManager::new(&self.config.safety.blocklisted_apps),
                Some(Arc::new(DaemonApprovalHandler::new(
                    self.pending_approvals.clone(),
                    self.event_tx.clone(),
                    std::time::Duration::from_secs(60),
                ))),
            )
    }

    pub fn openbot(&self) -> Option<&gm_openbot::OpenBotSupervisor> {
        self.openbot.as_ref()
    }

    pub fn event_sender(&self) -> broadcast::Sender<DaemonEvent> {
        self.event_tx.clone()
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    pub fn pending_approvals(&self) -> Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>> {
        Arc::clone(&self.pending_approvals)
    }

    pub async fn register_approval_request(&self, request_id: String, tx: oneshot::Sender<bool>) {
        self.pending_approvals.lock().await.insert(request_id, tx);
    }

    pub async fn run(&self) -> Result<()> {
        let path = &self.config.daemon_socket_path;
        #[cfg(unix)]
        {
            if path.exists() {
                let _ = std::fs::remove_file(path);
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            let listener = UnixListener::bind(path)?;
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = std::fs::Permissions::from_mode(0o600);
                if let Err(e) = std::fs::set_permissions(path, perms) {
                    warn!(
                        "Failed to lock daemon socket permissions to 0o600 on {:?}: {e}",
                        path
                    );
                }
            }

            let my_euid = unsafe { libc::geteuid() };
            info!(
                "Daemon control server listening on {:?} (UID: {my_euid}, mode 0600)",
                path
            );

            loop {
                match listener.accept().await {
                    Ok((stream, _addr)) => {
                        match stream.peer_cred() {
                            Ok(cred) => {
                                if !gm_core::ipc::is_peer_authorized(cred.uid(), my_euid) {
                                    warn!(
                                        "Rejecting daemon socket peer with mismatched UID (peer: {}, expected: {})",
                                        cred.uid(),
                                        my_euid
                                    );
                                    continue;
                                }
                            }
                            Err(e) => {
                                warn!("Failed to inspect peer credentials on daemon socket: {e}");
                                continue;
                            }
                        }

                        let server = self.clone_server_state();
                        tokio::spawn(async move {
                            if let Err(e) = server.handle_connection(stream).await {
                                warn!("Daemon connection ended: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        error!("Daemon listener error: {e}");
                        break;
                    }
                }
            }
        }

        #[cfg(windows)]
        {
            let pipe_name = path.to_string_lossy();
            info!(
                "Daemon control server listening on named pipe {}",
                pipe_name
            );

            let mut is_first = true;
            loop {
                let mut options = tokio::net::windows::named_pipe::ServerOptions::new();
                if is_first {
                    options.first_pipe_instance(true);
                    is_first = false;
                }
                let server = options.create(path.as_os_str()).map_err(|e| {
                    GmError::Platform(format!(
                        "Failed to create daemon named pipe {pipe_name}: {e}"
                    ))
                })?;

                server.connect().await.map_err(|e| {
                    GmError::Platform(format!(
                        "Failed to connect client to daemon named pipe: {e}"
                    ))
                })?;

                let state = self.clone_server_state();
                tokio::spawn(async move {
                    if let Err(e) = state.handle_connection(server).await {
                        warn!("Daemon named pipe connection error: {e}");
                    }
                });
            }
        }

        #[allow(unreachable_code)]
        Ok(())
    }

    fn clone_server_state(&self) -> DaemonControlServerState {
        DaemonControlServerState {
            config: self.config.clone(),
            paused: Arc::clone(&self.paused),
            active_task: Arc::clone(&self.active_task),
            active_task_handle: Arc::clone(&self.active_task_handle),
            pending_approvals: Arc::clone(&self.pending_approvals),
            event_tx: self.event_tx.clone(),
            budget: Arc::clone(&self.budget),
            llm_client: Arc::clone(&self.llm_client),
            router: self.router.clone(),
            kill_switch: self.kill_switch.clone(),
            platform: Arc::clone(&self.platform),
            openbot: self.openbot.clone(),
        }
    }
}

#[derive(Clone)]
struct DaemonControlServerState {
    config: GmConfig,
    paused: Arc<AtomicBool>,
    active_task: Arc<Mutex<Option<String>>>,
    active_task_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    pending_approvals: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    event_tx: broadcast::Sender<DaemonEvent>,
    budget: Arc<BudgetManager>,
    llm_client: Arc<HybridLlmClient>,
    router: LlmRouter,
    kill_switch: KillSwitch,
    platform: Arc<IpcPlatformClient>,
    openbot: Option<gm_openbot::OpenBotSupervisor>,
}

pub const LEGACY_CHAT_SYSTEM: &str = "You are Grok Muse, a helpful personal assistant. Use the supplied conversation for continuity. Explain clearly and do not claim you read files, searched sources or executed actions without tool results. The chat interface has no execution tools; the user can choose Run task for workspace or desktop actions.";

fn build_chat_system_messages(config: &GmConfig, model_display_name: &str) -> Vec<ChatMessage> {
    if !config.muse.enable_muse_prompt {
        return vec![ChatMessage::text("system", LEGACY_CHAT_SYSTEM)];
    }
    let paths = gm_core::muse_paths::MuseRuntimePaths::from_config(config, Some("muse"));
    let mut input = gm_muse::prompt::PromptInput::new(
        gm_muse::prompt::PromptSurface::MainChat,
        &paths,
        &config.muse,
        model_display_name,
    );
    input.allow_host_desktop = config.safety.allow_host_desktop;
    input.workspace_vm_enabled = config.workspace_vm.enabled;
    match gm_muse::prompt::MusePromptBuilder::build_messages(&input) {
        Ok(msgs) if !msgs.is_empty() => msgs,
        Ok(_) => vec![ChatMessage::text("system", LEGACY_CHAT_SYSTEM)],
        Err(err) => {
            warn!("Failed to build Muse MainChat prompt; using legacy fallback: {err}");
            vec![ChatMessage::text("system", LEGACY_CHAT_SYSTEM)]
        }
    }
}

fn assemble_chat_messages(
    text: &str,
    history: &[gm_core::ipc::ChatTurn],
    system_msgs: Vec<ChatMessage>,
) -> Result<Vec<ChatMessage>> {
    if text.trim().is_empty()
        || text.len() > 64 * 1024
        || history.len() > 24
        || history.iter().map(|t| t.content.len()).sum::<usize>() > 64 * 1024
        || history
            .iter()
            .any(|t| !matches!(t.role.as_str(), "user" | "assistant"))
    {
        return Err(GmError::Config(
            "Chat context is invalid or exceeds its size limit".into(),
        ));
    }
    let mut messages = system_msgs;
    messages.extend(
        history
            .iter()
            .map(|turn| ChatMessage::text(&turn.role, &turn.content)),
    );
    messages.push(ChatMessage::text("user", text));
    Ok(messages)
}

fn chat_event(id: &Option<String>, delta: String, done: bool) -> DaemonEvent {
    match id {
        Some(id) => DaemonEvent::ChatTokenV1 {
            message_id: id.clone(),
            delta,
            done,
        },
        None => DaemonEvent::ChatToken { delta, done },
    }
}

fn resolve_allowed_runtime_file(
    sup: &gm_openbot::OpenBotSupervisor,
    bot_id: Option<&str>,
    file: &str,
) -> Result<(String, std::path::PathBuf)> {
    let target_bot = bot_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&sup.config().openbot.default_bot_id)
        .to_string();
    if sup.roster().get_bot(&target_bot).is_none() {
        return Err(GmError::Config(format!("Unknown bot '{target_bot}'")));
    }
    let bot_paths = sup.roster().runtime_paths_for_bot(&target_bot);
    let shared = sup.shared_paths();
    let path = match file {
        "SOUL.md" => bot_paths
            .soul_md()
            .ok_or_else(|| GmError::Config("Missing bot SOUL.md".into()))?,
        "IDENTITY.md" => bot_paths
            .identity_md()
            .ok_or_else(|| GmError::Config("Missing bot IDENTITY.md".into()))?,
        "USER.md" => shared.user_md(),
        "MEMORY.md" => shared.memory_md(),
        "PROACTIVE_PREFERENCES.md" => shared.proactive_preferences_md(),
        _ => {
            return Err(GmError::Config(format!(
                "Unsupported runtime file '{file}'; allowed: SOUL.md, IDENTITY.md, USER.md, MEMORY.md, PROACTIVE_PREFERENCES.md"
            )));
        }
    };
    Ok((target_bot, path))
}

impl DaemonControlServerState {
    async fn handle_connection<S: AsyncRead + AsyncWrite + Unpin>(
        &self,
        mut stream: S,
    ) -> Result<()> {
        loop {
            let envelope: ControlRequestEnvelope = match read_ipc_message(&mut stream).await {
                Ok(msg) => msg,
                Err(GmError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    break;
                }
                Err(e) => return Err(e),
            };

            let (request_id, request) = match envelope {
                ControlRequestEnvelope::Envelope(msg) => (msg.id, msg.payload),
                ControlRequestEnvelope::Raw(req) => (0, req),
            };

            let routed_chat = matches!(
                request,
                DaemonRequest::SendChatMessage { .. }
                    | DaemonRequest::SendChatMessageV1 { .. }
                    | DaemonRequest::SendChatMessageV2 { .. }
            )
            .then(|| self.router.route(TaskType::Planning));

            let chat_id = match &request {
                DaemonRequest::SendChatMessageV1 { message_id, .. }
                | DaemonRequest::SendChatMessageV2 { message_id, .. } => Some(message_id.clone()),
                _ => None,
            };
            let history = match &request {
                DaemonRequest::SendChatMessageV2 { history, .. } => history.as_slice(),
                _ => &[],
            };
            let chat_messages = match &request {
                DaemonRequest::SendChatMessage { text }
                | DaemonRequest::SendChatMessageV1 { text, .. }
                | DaemonRequest::SendChatMessageV2 { text, .. } => {
                    let model_name = routed_chat
                        .as_ref()
                        .map(|(primary, _)| primary.model.as_str())
                        .unwrap_or("unknown");
                    let system_msgs = build_chat_system_messages(&self.config, model_name);
                    Some(assemble_chat_messages(text, history, system_msgs))
                }
                _ => None,
            };
            match request {
                DaemonRequest::Ping => {
                    let res = IpcMessage {
                        id: request_id,
                        payload: DaemonResponse::Pong,
                    };
                    write_ipc_message(&mut stream, &res).await?;
                }
                DaemonRequest::GetStatus => {
                    let helper_connected = tokio::time::timeout(
                        tokio::time::Duration::from_millis(150),
                        self.platform.ping(),
                    )
                    .await
                    .map(|r| r.is_ok())
                    .unwrap_or(false);

                    let paused = self.paused.load(Ordering::SeqCst);
                    let active_task = self.active_task.lock().await.clone();
                    let daily_spend_usd = self.budget.get_daily_spent();

                    let res = IpcMessage {
                        id: request_id,
                        payload: DaemonResponse::Status {
                            helper_connected,
                            paused,
                            active_task,
                            daily_spend_usd,
                        },
                    };
                    write_ipc_message(&mut stream, &res).await?;
                }
                DaemonRequest::SetPaused { paused } => {
                    self.paused.store(paused, Ordering::SeqCst);
                    if let Some(sup) = &self.openbot {
                        sup.set_runtime_paused(paused)?;
                    }
                    let _ = self.event_tx.send(DaemonEvent::HudStateChanged {
                        state: if paused {
                            "Paused".into()
                        } else {
                            "Idle".into()
                        },
                        detail: if paused {
                            "Paused by user coexistence toggle".into()
                        } else {
                            "Resumed by user coexistence toggle".into()
                        },
                        daily_spend_usd: self.budget.get_daily_spent(),
                    });
                    let res = IpcMessage {
                        id: request_id,
                        payload: DaemonResponse::Ack,
                    };
                    write_ipc_message(&mut stream, &res).await?;
                }
                DaemonRequest::EmergencyKill => {
                    info!("DaemonControlServer: EmergencyKill requested");
                    self.kill_switch.trigger();

                    // Abort active task handle if running
                    if let Some(handle) = self.active_task_handle.lock().await.take() {
                        handle.abort();
                    }
                    *self.active_task.lock().await = None;

                    // Send CancelActions to helper concurrently (<50ms budget)
                    let platform = Arc::clone(&self.platform);
                    tokio::spawn(async move {
                        let _ = tokio::time::timeout(
                            tokio::time::Duration::from_millis(40),
                            platform.cancel(),
                        )
                        .await;
                    });

                    let _ = self.event_tx.send(DaemonEvent::HudStateChanged {
                        state: "Stopped".into(),
                        detail: "Emergency kill switch triggered".into(),
                        daily_spend_usd: self.budget.get_daily_spent(),
                    });

                    let res = IpcMessage {
                        id: request_id,
                        payload: DaemonResponse::Killed,
                    };
                    write_ipc_message(&mut stream, &res).await?;
                }
                DaemonRequest::RespondApproval {
                    request_id: app_id,
                    approved,
                } => {
                    let mut approvals = self.pending_approvals.lock().await;
                    let resp = if let Some(tx) = approvals.remove(&app_id) {
                        let _ = tx.send(approved);
                        DaemonResponse::Ack
                    } else {
                        DaemonResponse::Error(format!(
                            "No pending approval found for request_id: {app_id}"
                        ))
                    };
                    let res = IpcMessage {
                        id: request_id,
                        payload: resp,
                    };
                    write_ipc_message(&mut stream, &res).await?;
                }
                DaemonRequest::SendChatMessage { .. }
                | DaemonRequest::SendChatMessageV1 { .. }
                | DaemonRequest::SendChatMessageV2 { .. } => {
                    let _ = self.event_tx.send(DaemonEvent::HudStateChanged {
                        state: "Thinking".into(),
                        detail: "Thinking...".into(),
                        daily_spend_usd: self.budget.get_daily_spent(),
                    });

                    let messages = match chat_messages
                        .unwrap_or_else(|| Err(GmError::Config("Chat request is missing".into())))
                    {
                        Ok(messages) => messages,
                        Err(error) => {
                            write_ipc_message(
                                &mut stream,
                                &IpcMessage {
                                    id: request_id,
                                    payload: DaemonResponse::Error(error.to_string()),
                                },
                            )
                            .await?;
                            continue;
                        }
                    };
                    let (primary, fallback) =
                        routed_chat.unwrap_or_else(|| self.router.route(TaskType::Planning));

                    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<String>(64);
                    let event_tx_delta = self.event_tx.clone();
                    let forward_id = chat_id.clone();
                    let forward_task = tokio::spawn(async move {
                        while let Some(delta) = delta_rx.recv().await {
                            let _ = event_tx_delta.send(chat_event(&forward_id, delta, false));
                        }
                    });

                    let mut kill_events = self.kill_switch.subscribe();
                    let chat_client = self.llm_client.scoped_job();
                    let result = if self.kill_switch.is_triggered() {
                        drop(delta_tx);
                        Err(GmError::SafetyViolation("Emergency stop is active".into()))
                    } else {
                        tokio::select! {biased; _=kill_events.recv()=>Err(GmError::SafetyViolation("Chat stopped by emergency stop".into())),
                        result=chat_client.complete_stream(&primary,fallback.as_ref(),&messages,1024,0.2,delta_tx)=>result }
                    };
                    match result {
                        Ok(completion) => {
                            let _ = forward_task.await;
                            let reply = completion.content;
                            let _ = self
                                .event_tx
                                .send(chat_event(&chat_id, String::new(), true));
                            let _ = self.event_tx.send(DaemonEvent::HudStateChanged {
                                state: "Idle".into(),
                                detail: "Ready".into(),
                                daily_spend_usd: self.budget.get_daily_spent(),
                            });
                            let res = IpcMessage {
                                id: request_id,
                                payload: match &chat_id {
                                    Some(id) => DaemonResponse::ChatReplyV1 {
                                        message_id: id.clone(),
                                        text: reply,
                                    },
                                    None => DaemonResponse::ChatReply { text: reply },
                                },
                            };
                            write_ipc_message(&mut stream, &res).await?;
                        }
                        Err(e) => {
                            let _ = forward_task.await;
                            let _ = self.event_tx.send(DaemonEvent::HudStateChanged {
                                state: "Idle".into(),
                                detail: format!("Error: {e}"),
                                daily_spend_usd: self.budget.get_daily_spent(),
                            });
                            let res = IpcMessage {
                                id: request_id,
                                payload: DaemonResponse::Error(e.to_string()),
                            };
                            write_ipc_message(&mut stream, &res).await?;
                        }
                    }
                }
                DaemonRequest::SubmitTask { instruction } => {
                    let payload = match self.openbot.as_ref() {
                        Some(sup) if !self.kill_switch.is_triggered() => {
                            match sup.submit_task(None, Some("general"), &instruction, 0) {
                                Ok(task) => DaemonResponse::Accepted { id: task.id },
                                Err(error) => DaemonResponse::Error(error.to_string()),
                            }
                        }
                        Some(_) => DaemonResponse::Error(
                            "Emergency stop is active; restart the runtime to resume".into(),
                        ),
                        None => DaemonResponse::Error("Task runtime is unavailable".into()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::ListBots => {
                    let payload = if let Some(ref sup) = self.openbot {
                        let bots = sup.roster().list_bots();
                        DaemonResponse::JsonPayload(serde_json::to_value(bots).unwrap_or_default())
                    } else {
                        DaemonResponse::Error("OpenBot supervisor not initialized".into())
                    };
                    let res = IpcMessage {
                        id: request_id,
                        payload,
                    };
                    write_ipc_message(&mut stream, &res).await?;
                }
                DaemonRequest::ImportAttachment {
                    bot_id,
                    source_path,
                } => {
                    let attachment_bot = bot_id.clone();
                    let result = if let Some(supervisor) = self.openbot.clone() {
                        tokio::task::spawn_blocking(move || {
                            supervisor.import_attachment(&bot_id, &source_path)
                        })
                        .await
                        .map_err(|e| GmError::Platform(e.to_string()))
                        .and_then(|r| r)
                    } else {
                        Err(GmError::Config("Task runtime is unavailable".into()))
                    };
                    let payload = match result {
                        Ok(path) => DaemonResponse::JsonPayload(
                            serde_json::json!({"attachment_path":path,"bot_id":attachment_bot}),
                        ),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::ListBotFiles { bot_id } => {
                    let result = (|| {
                        let sup = self
                            .openbot
                            .as_ref()
                            .ok_or_else(|| GmError::Config("Task runtime is unavailable".into()))?;
                        let files = sup.roster().workspace_sandbox(&bot_id)?.shared_files()?;
                        Ok::<_, GmError>(
                            serde_json::json!({"bot_id":bot_id,"files":files.into_iter().map(|(path,size)|serde_json::json!({"path":path,"size":size})).collect::<Vec<_>>()}),
                        )
                    })();
                    let payload = match result {
                        Ok(value) => DaemonResponse::JsonPayload(value),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::PreviewBotFile { bot_id, path } => {
                    let result = (|| {
                        let sup = self
                            .openbot
                            .as_ref()
                            .ok_or_else(|| GmError::Config("Task runtime is unavailable".into()))?;
                        let bytes = sup
                            .roster()
                            .workspace_sandbox(&bot_id)?
                            .preview_shared_file(&path)?;
                        let text = String::from_utf8(bytes).map_err(|_| {
                            GmError::Config("This binary file has no text preview".into())
                        })?;
                        Ok::<_, GmError>(
                            serde_json::json!({"bot_id":bot_id,"path":path,"text":text}),
                        )
                    })();
                    let payload = match result {
                        Ok(value) => DaemonResponse::JsonPayload(value),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::ListRoutines => {
                    let result = self
                        .openbot
                        .as_ref()
                        .ok_or_else(|| GmError::Config("Task runtime is unavailable".into()))
                        .and_then(|sup| sup.queue().list_routines())
                        .and_then(|value| serde_json::to_value(value).map_err(Into::into));
                    let payload = match result {
                        Ok(value) => DaemonResponse::JsonPayload(value),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::SaveRoutine { routine } => {
                    let result = (|| {
                        let sup = self
                            .openbot
                            .as_ref()
                            .ok_or_else(|| GmError::Config("Task runtime is unavailable".into()))?;
                        let routine: gm_openbot::Routine = serde_json::from_value(routine)?;
                        sup.save_routine(&routine)?;
                        Ok::<_, GmError>(())
                    })();
                    let payload = match result {
                        Ok(()) => DaemonResponse::Ack,
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::SaveBot { profile } => {
                    let result = (|| {
                        let sup = self
                            .openbot
                            .as_ref()
                            .ok_or_else(|| GmError::Config("Task runtime is unavailable".into()))?;
                        let profile: gm_openbot::BotProfile = serde_json::from_value(profile)?;
                        Ok::<_, GmError>(serde_json::to_value(sup.roster().upsert_bot(profile)?)?)
                    })();
                    let payload = match result {
                        Ok(value) => DaemonResponse::JsonPayload(value),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::ListChannels => {
                    let result = self
                        .openbot
                        .as_ref()
                        .ok_or_else(|| GmError::Config("Task runtime is unavailable".into()))
                        .and_then(|sup| sup.channels().list_channels(false))
                        .and_then(|value| serde_json::to_value(value).map_err(Into::into));
                    let payload = match result {
                        Ok(value) => DaemonResponse::JsonPayload(value),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::ChannelHistory { channel_id } => {
                    let result = self
                        .openbot
                        .as_ref()
                        .ok_or_else(|| GmError::Config("Task runtime is unavailable".into()))
                        .and_then(|sup| sup.channels().list_messages(&channel_id, 100))
                        .and_then(|value| serde_json::to_value(value).map_err(Into::into));
                    let payload = match result {
                        Ok(value) => DaemonResponse::JsonPayload(value),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::CreateChannel {
                    channel_id,
                    topic,
                    owner_bot_id,
                    participants,
                } => {
                    let result = self
                        .openbot
                        .as_ref()
                        .ok_or_else(|| GmError::Config("Task runtime is unavailable".into()))
                        .and_then(|sup| {
                            sup.create_channel(
                                &channel_id,
                                &topic,
                                owner_bot_id.as_deref(),
                                &participants,
                            )
                        })
                        .and_then(|value| serde_json::to_value(value).map_err(Into::into));
                    let payload = match result {
                        Ok(value) => DaemonResponse::JsonPayload(value),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::QueueTask {
                    bot_id,
                    channel_id,
                    instruction,
                    priority,
                } => {
                    let payload = if let Some(ref sup) = self.openbot {
                        match sup.submit_task(
                            bot_id.as_deref(),
                            channel_id.as_deref(),
                            &instruction,
                            priority,
                        ) {
                            Ok(task) => DaemonResponse::Accepted { id: task.id },
                            Err(e) => DaemonResponse::Error(e.to_string()),
                        }
                    } else {
                        DaemonResponse::Error("OpenBot supervisor not initialized".into())
                    };
                    let res = IpcMessage {
                        id: request_id,
                        payload,
                    };
                    write_ipc_message(&mut stream, &res).await?;
                }
                DaemonRequest::QueueControl { task_id, action } => {
                    let payload = if let Some(ref sup) = self.openbot {
                        match sup.control_task(&task_id, &action) {
                            Ok(_) => DaemonResponse::Ack,
                            Err(e) => DaemonResponse::Error(e.to_string()),
                        }
                    } else {
                        DaemonResponse::Error("OpenBot supervisor not initialized".into())
                    };
                    let res = IpcMessage {
                        id: request_id,
                        payload,
                    };
                    write_ipc_message(&mut stream, &res).await?;
                }
                DaemonRequest::ListQueue { bot_id } => {
                    let payload = if let Some(ref sup) = self.openbot {
                        match sup.queue().list_tasks(bot_id.as_deref()) {
                            Ok(tasks) => DaemonResponse::JsonPayload(
                                serde_json::to_value(tasks).unwrap_or_default(),
                            ),
                            Err(e) => DaemonResponse::Error(e.to_string()),
                        }
                    } else {
                        DaemonResponse::Error("OpenBot supervisor not initialized".into())
                    };
                    let res = IpcMessage {
                        id: request_id,
                        payload,
                    };
                    write_ipc_message(&mut stream, &res).await?;
                }
                DaemonRequest::ChannelPost {
                    channel_id,
                    author,
                    content,
                } => {
                    let payload = if let Some(ref sup) = self.openbot {
                        match sup.post_channel_message(&channel_id, &author, None, &content) {
                            Ok((msg, _)) => DaemonResponse::JsonPayload(
                                serde_json::to_value(msg).unwrap_or_default(),
                            ),
                            Err(e) => DaemonResponse::Error(e.to_string()),
                        }
                    } else {
                        DaemonResponse::Error("OpenBot supervisor not initialized".into())
                    };
                    let res = IpcMessage {
                        id: request_id,
                        payload,
                    };
                    write_ipc_message(&mut stream, &res).await?;
                }
                DaemonRequest::TakeTheWheelResume { task_id, note } => {
                    let payload = if let Some(ref sup) = self.openbot {
                        match sup.queue().resume_from_human(&task_id, note.as_deref()) {
                            Ok(_) => {
                                let _ = self.event_tx.send(DaemonEvent::WheelHandoffResumed {
                                    task_id: task_id.clone(),
                                    bot_id: "muse".into(),
                                });
                                DaemonResponse::Ack
                            }
                            Err(e) => DaemonResponse::Error(e.to_string()),
                        }
                    } else {
                        DaemonResponse::Error("OpenBot supervisor not initialized".into())
                    };
                    let res = IpcMessage {
                        id: request_id,
                        payload,
                    };
                    write_ipc_message(&mut stream, &res).await?;
                }
                DaemonRequest::ListActivity => {
                    let result = (|| {
                        let sup = self
                            .openbot
                            .as_ref()
                            .ok_or_else(|| GmError::Config("Task runtime is unavailable".into()))?;
                        let posts = sup.feed_store().list_posts(50)?;
                        let brief_prompt = sup.feed_store().get_brief_prompt()?;
                        Ok::<_, GmError>(serde_json::json!({
                            "posts": posts,
                            "brief_prompt": brief_prompt,
                        }))
                    })();
                    let payload = match result {
                        Ok(value) => DaemonResponse::JsonPayload(value),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::RateActivity { id, rating } => {
                    let result = (|| {
                        let sup = self
                            .openbot
                            .as_ref()
                            .ok_or_else(|| GmError::Config("Task runtime is unavailable".into()))?;
                        let ok = sup.feed_store().rate_post(&id, rating)?;
                        if !ok {
                            return Err(GmError::Config(format!("Feed post '{id}' not found")));
                        }
                        let _ = self.event_tx.send(DaemonEvent::ActivityUpdated {
                            reason: "feed_rated".into(),
                        });
                        Ok::<_, GmError>(serde_json::json!({
                            "ok": true,
                            "id": id,
                            "rating": rating.clamp(-1, 5),
                        }))
                    })();
                    let payload = match result {
                        Ok(value) => DaemonResponse::JsonPayload(value),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::ListGoals => {
                    let result = (|| {
                        let sup = self
                            .openbot
                            .as_ref()
                            .ok_or_else(|| GmError::Config("Task runtime is unavailable".into()))?;
                        let mgr = sup.goal_manager();
                        let goals = mgr.list_goals()?;
                        let enriched: Vec<serde_json::Value> = goals
                            .into_iter()
                            .map(|goal| {
                                let files = mgr.list_goal_files(&goal.slug).unwrap_or_default();
                                serde_json::json!({
                                    "goal": goal,
                                    "files": files,
                                })
                            })
                            .collect();
                        Ok::<_, GmError>(serde_json::Value::Array(enriched))
                    })();
                    let payload = match result {
                        Ok(value) => DaemonResponse::JsonPayload(value),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::SaveGoal { op, slug, payload } => {
                    let result = (|| {
                        let sup = self
                            .openbot
                            .as_ref()
                            .ok_or_else(|| GmError::Config("Task runtime is unavailable".into()))?;
                        let mgr = sup.goal_manager();
                        let target_slug = || -> Result<String> {
                            slug.as_deref()
                                .or_else(|| payload.get("slug").and_then(|v| v.as_str()))
                                .or_else(|| payload.get("id").and_then(|v| v.as_str()))
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .map(str::to_string)
                                .ok_or_else(|| {
                                    GmError::Config("Goal slug or id is required".into())
                                })
                        };
                        let extract_stops = |val: &serde_json::Value| -> Vec<String> {
                            val.get("stop_conditions")
                                .and_then(|v| v.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|x| x.as_str().map(str::to_string))
                                        .collect()
                                })
                                .unwrap_or_default()
                        };
                        let out = match op.trim().to_ascii_lowercase().as_str() {
                            "create" => {
                                let title = payload
                                    .get("title")
                                    .and_then(|v| v.as_str())
                                    .or(slug.as_deref())
                                    .unwrap_or("")
                                    .trim();
                                let desc = payload
                                    .get("description")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let cat = payload.get("category").and_then(|v| v.as_str());
                                let stops = extract_stops(&payload);
                                serde_json::to_value(mgr.create_goal(title, desc, cat, stops)?)?
                            }
                            "create_subgoal" | "subgoal" => {
                                let parent = payload
                                    .get("parent_slug")
                                    .or_else(|| payload.get("parent_goal_id"))
                                    .and_then(|v| v.as_str())
                                    .or(slug.as_deref())
                                    .map(str::trim)
                                    .filter(|s| !s.is_empty())
                                    .ok_or_else(|| {
                                        GmError::Config("Parent goal slug/id is required".into())
                                    })?;
                                let title = payload
                                    .get("title")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .trim();
                                let desc = payload
                                    .get("description")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let stops = extract_stops(&payload);
                                serde_json::to_value(
                                    mgr.create_subgoal(parent, title, desc, stops)?,
                                )?
                            }
                            "update" => {
                                let target = target_slug()?;
                                let title = payload.get("title").and_then(|v| v.as_str());
                                let desc = payload.get("description").and_then(|v| v.as_str());
                                let stops = payload
                                    .get("stop_conditions")
                                    .and_then(|v| v.as_array())
                                    .map(|a| {
                                        a.iter()
                                            .filter_map(|x| x.as_str().map(str::to_string))
                                            .collect()
                                    });
                                let sched = payload.get("track_brief_schedule").map(|v| v.as_str());
                                let cat = payload.get("category").map(|v| v.as_str());
                                serde_json::to_value(
                                    mgr.update_goal(&target, title, desc, stops, sched, cat)?,
                                )?
                            }
                            "complete" => {
                                let target = target_slug()?;
                                let verified = payload
                                    .get("verified")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(true);
                                serde_json::to_value(mgr.complete_goal(&target, verified)?)?
                            }
                            "abandon" => {
                                let target = target_slug()?;
                                serde_json::to_value(mgr.abandon_goal(&target)?)?
                            }
                            "delete" => {
                                let target = target_slug()?;
                                serde_json::json!({ "deleted_slugs": mgr.delete_goal(&target)? })
                            }
                            other => {
                                return Err(GmError::Config(format!(
                                    "Unsupported goal op '{other}'"
                                )));
                            }
                        };
                        let _ = self.event_tx.send(DaemonEvent::ActivityUpdated {
                            reason: "goal_updated".into(),
                        });
                        Ok::<_, GmError>(out)
                    })();
                    let payload = match result {
                        Ok(value) => DaemonResponse::JsonPayload(value),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::ListSpaces => {
                    let result = (|| {
                        let sup = self
                            .openbot
                            .as_ref()
                            .ok_or_else(|| GmError::Config("Task runtime is unavailable".into()))?;
                        let spaces = sup.spaces().list_spaces()?;
                        let sp_port = sup.spaces_port();
                        let mut urls = serde_json::Map::new();
                        if sup.config().muse.enable_spaces {
                            for sp in &spaces {
                                if let Ok(url) = sup.spaces().mint_url_for_slug(&sp.slug, sp_port) {
                                    urls.insert(sp.slug.clone(), serde_json::Value::String(url));
                                }
                            }
                        }
                        Ok::<_, GmError>(serde_json::json!({
                            "spaces": spaces,
                            "urls": urls,
                            "spaces_port": sp_port,
                            "enable_spaces": sup.config().muse.enable_spaces,
                        }))
                    })();
                    let payload = match result {
                        Ok(value) => DaemonResponse::JsonPayload(value),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::CreateSpace {
                    slug,
                    title,
                    kind,
                    html,
                } => {
                    let result = (|| {
                        let sup = self
                            .openbot
                            .as_ref()
                            .ok_or_else(|| GmError::Config("Task runtime is unavailable".into()))?;
                        if !sup.config().muse.enable_spaces {
                            return Err(GmError::Config(
                                "Spaces feature is disabled in MuseConfig".into(),
                            ));
                        }
                        let kind_enum = match kind
                            .as_deref()
                            .unwrap_or("static")
                            .trim()
                            .to_ascii_lowercase()
                            .as_str()
                        {
                            "static" => gm_openbot::SpaceKind::Static,
                            "ts" | "typescript" => gm_openbot::SpaceKind::Ts,
                            other => {
                                return Err(GmError::Config(format!(
                                    "Unknown Space kind '{other}'"
                                )));
                            }
                        };
                        let meta = sup.spaces().create_space(
                            &slug,
                            title.as_deref(),
                            kind_enum,
                            html.as_deref(),
                        )?;
                        let sp_port = sup.spaces_port();
                        let url = sup.spaces().mint_url_for_slug(&meta.slug, sp_port)?;
                        let _ = self.event_tx.send(DaemonEvent::ActivityUpdated {
                            reason: "space_created".into(),
                        });
                        Ok::<_, GmError>(serde_json::json!({
                            "space": meta,
                            "url": url,
                            "spaces_port": sp_port,
                        }))
                    })();
                    let payload = match result {
                        Ok(value) => DaemonResponse::JsonPayload(value),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::SearchSkills { query } => {
                    let result = (|| {
                        let sup = self
                            .openbot
                            .as_ref()
                            .ok_or_else(|| GmError::Config("Task runtime is unavailable".into()))?;
                        let reg = sup.skill_registry()?;
                        Ok::<_, GmError>(serde_json::json!({
                            "skills": reg.search_skills(&query),
                            "connectors": reg.search_connectors(&query),
                        }))
                    })();
                    let payload = match result {
                        Ok(value) => DaemonResponse::JsonPayload(value),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::ReadRuntimeFile { bot_id, file } => {
                    let result = (|| {
                        let sup = self
                            .openbot
                            .as_ref()
                            .ok_or_else(|| GmError::Config("Task runtime is unavailable".into()))?;
                        let (target_bot, path) =
                            resolve_allowed_runtime_file(sup, bot_id.as_deref(), &file)?;
                        let pair = gm_muse::shared_files::SharedFileStore::global()
                            .read_text_with_sha256(&path)?;
                        let (exists, content, sha256) = match pair {
                            Some((c, s)) => (true, c, Some(s)),
                            None => (false, String::new(), None),
                        };
                        Ok::<_, GmError>(serde_json::json!({
                            "bot_id": target_bot,
                            "file": file,
                            "exists": exists,
                            "content": content,
                            "sha256": sha256,
                        }))
                    })();
                    let payload = match result {
                        Ok(value) => DaemonResponse::JsonPayload(value),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::SaveRuntimeFile {
                    bot_id,
                    file,
                    content,
                    expected_sha256,
                } => {
                    let result = (|| {
                        let sup = self
                            .openbot
                            .as_ref()
                            .ok_or_else(|| GmError::Config("Task runtime is unavailable".into()))?;
                        let (target_bot, path) =
                            resolve_allowed_runtime_file(sup, bot_id.as_deref(), &file)?;
                        if sup.scrubber().scrub_str(&content) != content {
                            return Err(GmError::SafetyViolation(
                                "Refused plaintext secret in runtime file".into(),
                            ));
                        }
                        let store = gm_muse::shared_files::SharedFileStore::global();
                        let existing = store.read_text_with_sha256(&path)?;
                        if existing.is_some() && expected_sha256.is_none() {
                            return Err(GmError::Config(
                                "expected_sha256 is required when overwriting an existing runtime file".into(),
                            ));
                        }
                        match store.write_exact_sync(&path, expected_sha256.as_deref(), &content)? {
                            gm_muse::shared_files::SharedUpdateOutcome::Conflict {
                                current_sha256,
                            } => Err(GmError::Config(format!(
                                "Optimistic concurrency conflict: expected_sha256 does not match (current_sha256={})",
                                current_sha256.as_deref().unwrap_or("<missing>")
                            ))),
                            gm_muse::shared_files::SharedUpdateOutcome::Updated {
                                sha256,
                                bytes,
                            } => {
                                if file == "PROACTIVE_PREFERENCES.md" {
                                    let _ = sup.sync_quiet_gate_from_prefs();
                                }
                                let _ = self.event_tx.send(DaemonEvent::ActivityUpdated {
                                    reason: "identity_updated".into(),
                                });
                                Ok::<_, GmError>(serde_json::json!({
                                    "ok": true,
                                    "bot_id": target_bot,
                                    "file": file,
                                    "sha256": sha256,
                                    "bytes": bytes,
                                }))
                            }
                        }
                    })();
                    let payload = match result {
                        Ok(value) => DaemonResponse::JsonPayload(value),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::ImportMemory {
                    input,
                    apply,
                    report_sha256,
                } => {
                    let result = (|| {
                        let sup = self
                            .openbot
                            .as_ref()
                            .ok_or_else(|| GmError::Config("Task runtime is unavailable".into()))?;
                        if input.len() > gm_memory::MAX_IMPORT_BYTES {
                            return Err(GmError::Config(format!(
                                "Import payload exceeds {} byte limit",
                                gm_memory::MAX_IMPORT_BYTES
                            )));
                        }
                        let bot_id = &sup.config().openbot.default_bot_id;
                        let paths = sup.shared_paths();
                        let memory = sup.roster().memory_store(bot_id)?;
                        let report = gm_memory::import_memory_portable_with_stores(
                            &input,
                            &paths,
                            Some(sup.provenance()),
                            Some(&memory),
                            apply,
                            report_sha256.as_deref(),
                        )?;
                        if report.applied {
                            let _ = self.event_tx.send(DaemonEvent::ActivityUpdated {
                                reason: "memory_imported".into(),
                            });
                        }
                        Ok::<_, GmError>(serde_json::to_value(report)?)
                    })();
                    let payload = match result {
                        Ok(value) => DaemonResponse::JsonPayload(value),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::ForgetPlan { bot_id, request } => {
                    let result = (|| {
                        let sup = self
                            .openbot
                            .as_ref()
                            .ok_or_else(|| GmError::Config("Task runtime is unavailable".into()))?;
                        let plan = sup.plan_forget(bot_id.as_deref(), &request)?;
                        Ok::<_, GmError>(serde_json::to_value(plan)?)
                    })();
                    let payload = match result {
                        Ok(value) => DaemonResponse::JsonPayload(value),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::ForgetConfirm {
                    bot_id,
                    plan_id,
                    verification_query,
                    confirmed,
                } => {
                    let result = (|| {
                        let sup = self
                            .openbot
                            .as_ref()
                            .ok_or_else(|| GmError::Config("Task runtime is unavailable".into()))?;
                        let report = sup.confirm_forget(
                            bot_id.as_deref(),
                            &plan_id,
                            verification_query.as_deref(),
                            confirmed,
                        )?;
                        Ok::<_, GmError>(serde_json::to_value(report)?)
                    })();
                    let payload = match result {
                        Ok(value) => DaemonResponse::JsonPayload(value),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::UpdateDiscretion {
                    chat_id,
                    rules,
                    expected_sha256,
                } => {
                    let result = (|| {
                        let sup = self
                            .openbot
                            .as_ref()
                            .ok_or_else(|| GmError::Config("Task runtime is unavailable".into()))?;
                        let clean_rules: Vec<String> = rules
                            .into_iter()
                            .map(|r| r.trim().to_string())
                            .filter(|r| !r.is_empty())
                            .collect();
                        if clean_rules.is_empty() {
                            return Err(GmError::Config(
                                "At least one discretion rule is required".into(),
                            ));
                        }
                        let policy = sup.discretion().update_user_rules(
                            &chat_id,
                            &clean_rules,
                            expected_sha256.as_deref(),
                        )?;
                        sup.audit().log_governance(
                            "discretion_updated",
                            Some("muse"),
                            gm_safety::AuditCategory::Permitted,
                            "RULE-POLICY-001",
                            "Updated bridge discretion user rules via IPC",
                            &format!("Updated DISCRETION.md for chat '{chat_id}'"),
                            serde_json::json!({ "chat_id": chat_id }),
                        );
                        Ok::<_, GmError>(serde_json::to_value(policy)?)
                    })();
                    let payload = match result {
                        Ok(value) => DaemonResponse::JsonPayload(value),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    };
                    write_ipc_message(
                        &mut stream,
                        &IpcMessage {
                            id: request_id,
                            payload,
                        },
                    )
                    .await?;
                }
                DaemonRequest::SubscribeEvents => {
                    let mut rx = self.event_tx.subscribe();
                    let ack = IpcMessage {
                        id: request_id,
                        payload: DaemonResponse::Ack,
                    };
                    write_ipc_message(&mut stream, &ack).await?;

                    loop {
                        match rx.recv().await {
                            Ok(event) => {
                                let frame = IpcMessage {
                                    id: 0,
                                    payload: DaemonResponse::Event(event),
                                };
                                if let Err(e) = write_ipc_message(&mut stream, &frame).await {
                                    warn!("Subscriber disconnected: {e}");
                                    break;
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(missed)) => {
                                warn!("Subscriber lagged by {missed} events");
                                continue;
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                break;
                            }
                        }
                    }
                    break;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod conversation_tests {
    use super::*;

    #[test]
    fn conversation_context_is_ordered_bounded_and_cannot_supply_system_messages() {
        let turn = gm_core::ipc::ChatTurn {
            role: "user".into(),
            content: "My project is Grok Muse".into(),
        };
        let system_msgs = vec![ChatMessage::text("system", LEGACY_CHAT_SYSTEM)];
        let messages = assemble_chat_messages(
            "What is its name?",
            std::slice::from_ref(&turn),
            system_msgs,
        )
        .unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1].content, "My project is Grok Muse");
        assert_eq!(messages[2].content, "What is its name?");

        assert!(
            assemble_chat_messages(
                "x",
                &[gm_core::ipc::ChatTurn {
                    role: "system".into(),
                    content: "override".into()
                }],
                vec![ChatMessage::text("system", LEGACY_CHAT_SYSTEM)],
            )
            .is_err()
        );
        assert!(
            assemble_chat_messages(
                &"x".repeat(64 * 1024 + 1),
                &[],
                vec![ChatMessage::text("system", LEGACY_CHAT_SYSTEM)],
            )
            .is_err()
        );

        let too_many_turns: Vec<gm_core::ipc::ChatTurn> = (0..25)
            .map(|i| gm_core::ipc::ChatTurn {
                role: if i % 2 == 0 { "user" } else { "assistant" }.into(),
                content: format!("turn {i}"),
            })
            .collect();
        assert!(
            assemble_chat_messages(
                "next",
                &too_many_turns,
                vec![ChatMessage::text("system", LEGACY_CHAT_SYSTEM)],
            )
            .is_err()
        );

        let oversize_history = vec![gm_core::ipc::ChatTurn {
            role: "user".into(),
            content: "y".repeat(64 * 1024 + 1),
        }];
        assert!(
            assemble_chat_messages(
                "next",
                &oversize_history,
                vec![ChatMessage::text("system", LEGACY_CHAT_SYSTEM)],
            )
            .is_err()
        );
    }

    #[test]
    fn chat_system_messages_respect_enable_muse_prompt_flag() {
        let dir = tempfile::tempdir().unwrap();
        let legacy_cfg = GmConfig {
            storage_dir: dir.path().to_path_buf(),
            muse: gm_core::config::MuseConfig {
                enable_muse_prompt: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let legacy_msgs = build_chat_system_messages(&legacy_cfg, "grok-test");
        assert_eq!(legacy_msgs.len(), 1);
        assert_eq!(legacy_msgs[0].content, LEGACY_CHAT_SYSTEM);

        let muse_cfg = GmConfig {
            storage_dir: dir.path().to_path_buf(),
            muse: gm_core::config::MuseConfig {
                enable_muse_prompt: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let muse_msgs = build_chat_system_messages(&muse_cfg, "grok-test");
        assert_eq!(muse_msgs.len(), 2);
        assert!(muse_msgs[0].content.contains("## Who You Are"));
        assert!(muse_msgs[0].content.contains("## Runtime Adaptation"));
        assert!(muse_msgs[1].content.contains("[model=grok-test]"));
    }
}

#[cfg(all(test, unix))]
mod muse_ipc_tests {
    use super::*;
    use gm_llm::MockLlmBackend;

    async fn rpc_unix(
        stream: &mut tokio::net::UnixStream,
        id: u64,
        req: DaemonRequest,
    ) -> DaemonResponse {
        write_ipc_message(stream, &IpcMessage { id, payload: req })
            .await
            .unwrap();
        let resp: IpcMessage<DaemonResponse> = read_ipc_message(stream).await.unwrap();
        assert_eq!(resp.id, id);
        resp.payload
    }

    #[tokio::test]
    async fn muse_ipc_handlers_cover_runtime_files_goals_spaces_skills_import_discretion_and_forget()
     {
        let dir = tempfile::tempdir().unwrap();
        let short_id = &uuid::Uuid::new_v4().simple().to_string()[..8];
        let daemon_socket = std::path::PathBuf::from(format!("/tmp/gmmuse_{short_id}.sock"));
        let helper_socket = std::path::PathBuf::from(format!("/tmp/gmmuseh_{short_id}.sock"));
        let storage = dir.path().join("storage");
        let config = GmConfig {
            storage_dir: storage.clone(),
            openbot: gm_core::config::OpenBotConfig {
                bots_dir: storage.join("bots"),
                ..Default::default()
            },
            daemon_socket_path: daemon_socket.clone(),
            socket_path: helper_socket.clone(),
            ..Default::default()
        };

        let budget = Arc::new(BudgetManager::new(config.budget.clone()));
        let llm_client = Arc::new(HybridLlmClient::new(
            Arc::new(MockLlmBackend::new()),
            Arc::clone(&budget),
        ));
        let router = LlmRouter::new(config.providers.clone());
        let server = DaemonControlServer::new(
            config,
            budget,
            llm_client,
            router,
            KillSwitch::new(),
            BlocklistManager::default(),
            Arc::new(AuditLogger::in_memory()),
            Arc::new(IpcPlatformClient::new(&helper_socket)),
        );
        let srv_handle = tokio::spawn(async move {
            let _ = server.run().await;
        });

        for _ in 0..40 {
            if daemon_socket.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        }
        let mut stream = tokio::net::UnixStream::connect(&daemon_socket)
            .await
            .unwrap();

        // 1. ReadRuntimeFile allowlist rejection & valid read + SaveRuntimeFile CAS
        let bad_file = rpc_unix(
            &mut stream,
            1,
            DaemonRequest::ReadRuntimeFile {
                bot_id: Some("muse".into()),
                file: "../../etc/passwd".into(),
            },
        )
        .await;
        assert!(matches!(bad_file, DaemonResponse::Error(_)));

        let user_read = rpc_unix(
            &mut stream,
            2,
            DaemonRequest::ReadRuntimeFile {
                bot_id: Some("muse".into()),
                file: "USER.md".into(),
            },
        )
        .await;
        let user_val = match user_read {
            DaemonResponse::JsonPayload(v) => v,
            other => panic!("Expected JsonPayload, got {other:?}"),
        };
        let sha = user_val["sha256"].as_str().map(str::to_string);

        // Wrong CAS sha256 fails
        let conflict = rpc_unix(
            &mut stream,
            3,
            DaemonRequest::SaveRuntimeFile {
                bot_id: Some("muse".into()),
                file: "USER.md".into(),
                content: "# USER\n- Prefers concise briefs\n".into(),
                expected_sha256: Some("0".repeat(64)),
            },
        )
        .await;
        assert!(matches!(conflict, DaemonResponse::Error(msg) if msg.contains("conflict")));

        let saved = rpc_unix(
            &mut stream,
            4,
            DaemonRequest::SaveRuntimeFile {
                bot_id: Some("muse".into()),
                file: "USER.md".into(),
                content: "# USER\n- Prefers concise briefs\n".into(),
                expected_sha256: sha,
            },
        )
        .await;
        assert!(matches!(saved, DaemonResponse::JsonPayload(v) if v["ok"] == true));

        // 2. Goals: create -> subgoal -> depth-2 rejected -> list
        let g1 = rpc_unix(
            &mut stream,
            5,
            DaemonRequest::SaveGoal {
                op: "create".into(),
                slug: None,
                payload: serde_json::json!({"title": "Ship release", "description": "Milestone"}),
            },
        )
        .await;
        let g1_val = match g1 {
            DaemonResponse::JsonPayload(v) => v,
            other => panic!("Expected goal JsonPayload, got {other:?}"),
        };
        let parent_slug = g1_val["slug"].as_str().unwrap().to_string();

        let sub = rpc_unix(
            &mut stream,
            6,
            DaemonRequest::SaveGoal {
                op: "create_subgoal".into(),
                slug: Some(parent_slug.clone()),
                payload: serde_json::json!({"title": "Write docs"}),
            },
        )
        .await;
        let sub_slug = match sub {
            DaemonResponse::JsonPayload(v) => v["slug"].as_str().unwrap().to_string(),
            other => panic!("Expected subgoal JsonPayload, got {other:?}"),
        };

        let depth2 = rpc_unix(
            &mut stream,
            7,
            DaemonRequest::SaveGoal {
                op: "create_subgoal".into(),
                slug: Some(sub_slug),
                payload: serde_json::json!({"title": "Too deep"}),
            },
        )
        .await;
        assert!(matches!(depth2, DaemonResponse::Error(msg) if msg.contains("one subgoal")));

        // 3. Spaces: Static succeeds, Ts rejected
        let ts_err = rpc_unix(
            &mut stream,
            8,
            DaemonRequest::CreateSpace {
                slug: "ts-denied".into(),
                title: Some("TS".into()),
                kind: Some("ts".into()),
                html: None,
            },
        )
        .await;
        assert!(
            matches!(ts_err, DaemonResponse::Error(msg) if msg.contains("TS Spaces are not supported yet"))
        );

        let sp_ok = rpc_unix(
            &mut stream,
            9,
            DaemonRequest::CreateSpace {
                slug: "budget-view".into(),
                title: Some("Budget View".into()),
                kind: Some("static".into()),
                html: None,
            },
        )
        .await;
        assert!(
            matches!(sp_ok, DaemonResponse::JsonPayload(v) if v["url"].as_str().unwrap().contains("/spaces/v2/budget-view/"))
        );

        // 4. Activity, Skills, ImportMemory, Discretion, ForgetPlan -> ForgetConfirm
        let act = rpc_unix(&mut stream, 10, DaemonRequest::ListActivity).await;
        assert!(matches!(act, DaemonResponse::JsonPayload(v) if v["posts"].is_array()));

        let skills = rpc_unix(
            &mut stream,
            11,
            DaemonRequest::SearchSkills {
                query: "goals".into(),
            },
        )
        .await;
        assert!(matches!(skills, DaemonResponse::JsonPayload(v) if v["skills"].is_array()));

        let dry = rpc_unix(
            &mut stream,
            12,
            DaemonRequest::ImportMemory {
                input: "- Enjoys espresso every morning\n".into(),
                apply: false,
                report_sha256: None,
            },
        )
        .await;
        let report_sha = match dry {
            DaemonResponse::JsonPayload(v) => v["report_sha256"].as_str().unwrap().to_string(),
            other => panic!("Expected import dry-run report, got {other:?}"),
        };
        let applied = rpc_unix(
            &mut stream,
            13,
            DaemonRequest::ImportMemory {
                input: "- Enjoys espresso every morning\n".into(),
                apply: true,
                report_sha256: Some(report_sha),
            },
        )
        .await;
        assert!(matches!(applied, DaemonResponse::JsonPayload(v) if v["applied"] == true));

        let disc = rpc_unix(
            &mut stream,
            14,
            DaemonRequest::UpdateDiscretion {
                chat_id: "slack-team".into(),
                rules: vec!["Only summarize public sprint tickets".into()],
                expected_sha256: None,
            },
        )
        .await;
        assert!(
            matches!(disc, DaemonResponse::JsonPayload(v) if v["user_rules"].as_array().unwrap().len() == 1)
        );

        let plan_resp = rpc_unix(
            &mut stream,
            15,
            DaemonRequest::ForgetPlan {
                bot_id: Some("muse".into()),
                request: "espresso".into(),
            },
        )
        .await;
        let plan_id = match plan_resp {
            DaemonResponse::JsonPayload(v) => v["plan_id"].as_str().unwrap().to_string(),
            other => panic!("Expected ForgetPlan, got {other:?}"),
        };

        let unconfirmed = rpc_unix(
            &mut stream,
            16,
            DaemonRequest::ForgetConfirm {
                bot_id: Some("muse".into()),
                plan_id: plan_id.clone(),
                verification_query: Some("espresso".into()),
                confirmed: false,
            },
        )
        .await;
        assert!(matches!(unconfirmed, DaemonResponse::Error(_)));

        let confirmed = rpc_unix(
            &mut stream,
            17,
            DaemonRequest::ForgetConfirm {
                bot_id: Some("muse".into()),
                plan_id,
                verification_query: Some("espresso".into()),
                confirmed: true,
            },
        )
        .await;
        assert!(matches!(confirmed, DaemonResponse::JsonPayload(v) if v["status"] == "forgotten"));

        srv_handle.abort();
        let _ = std::fs::remove_file(&daemon_socket);
    }
}
