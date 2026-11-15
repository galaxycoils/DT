use crate::a2a::A2aCoordinator;
use crate::channels::{ChannelInfo, ChannelMessageRecord, ChannelStore};
use crate::compaction::ContextCompactor;
use crate::discretion::DiscretionStore;
use crate::queue::{DurableTaskQueue, QueuedTask};
use crate::roster::{BotProfile, BotRoster};
use crate::spaces::{SpaceKind, SpacesManager};
use gm_agent::{
    AgentCapabilities, AgentLoop, AgentLoopConfig, DirectiveKind, GoalOpKind, InteractionHandler,
    MuseWorkflowDelegate, NonInteractiveHandler, SpaceOpKind,
};
use gm_core::config::GmConfig;
use gm_core::error::{GmError, Result};
use gm_core::ipc::{DaemonEvent, QueueControlAction};
use gm_core::muse_paths::MuseRuntimePaths;
use gm_llm::{HybridLlmClient, LlmRouter, TaskType, VaultSecretResolver};
use gm_memory::{ForgetExecutor, ForgetPlan, ForgetPlanner, ForgetResources, ProvenanceStore};
use gm_muse::prompt::{MusePromptBuilder, PromptInput, PromptSurface};
use gm_muse::shared_files::SharedFileStore;
use gm_muse::skills::SkillRegistry;
use gm_platform::Platform;
use gm_safety::{AuditCategory, AuditLogger, BlocklistManager, KillSwitch, SecretScrubber};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};
use tokio::sync::{Mutex as AsyncMutex, broadcast};

/// Standard AG-UI protocol event envelope (`CopilotKit/OpenBot` compatible).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgUiEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub sequence: u64,
    pub run_id: String,
    pub bot_id: String,
    pub timestamp: String,
    pub payload: serde_json::Value,
}

/// Coordinates the multi-bot roster, durable SQLite FIFO task queues, shared channels,
/// A2A handoffs, `GuiSeatMutex` serialization, Muse Stores (`SpacesManager`, `DiscretionStore`,
/// `ProvenanceStore`, `FeedStore`), and AG-UI / DaemonEvent broadcasting.
#[derive(Clone)]
pub struct OpenBotSupervisor {
    pub(crate) config: GmConfig,
    pub(crate) runtime_demo: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) runtime_paused: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) runtime_active: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) roster: BotRoster,
    pub(crate) queue: DurableTaskQueue,
    channels: ChannelStore,
    a2a: A2aCoordinator,
    compactor: ContextCompactor,
    feed_store: Arc<gm_goals::FeedStore>,
    quiet_gate: gm_goals::QuietGate,
    spaces: SpacesManager,
    discretion: DiscretionStore,
    provenance: ProvenanceStore,
    companion_port: Arc<AtomicU16>,
    spaces_port: Arc<AtomicU16>,
    gui_seat: Arc<AsyncMutex<()>>,
    daemon_events_tx: broadcast::Sender<DaemonEvent>,
    agui_events_tx: broadcast::Sender<AgUiEvent>,
    audit: Arc<AuditLogger>,
    scrubber: SecretScrubber,
}

impl OpenBotSupervisor {
    pub fn open(
        mut config: GmConfig,
        audit: Arc<AuditLogger>,
        daemon_events_tx: broadcast::Sender<DaemonEvent>,
    ) -> Result<Self> {
        let defaults = GmConfig::default();
        if config.storage_dir != defaults.storage_dir
            && config.openbot.bots_dir == defaults.openbot.bots_dir
        {
            config.openbot.bots_dir = config.storage_dir.join("bots");
        }
        gm_core::storage::ensure_private_dir(&config.storage_dir)?;
        let roster = BotRoster::open_with_storage(&config.openbot.bots_dir, &config.storage_dir)?;
        let db_path = config.storage_dir.join("openbot.sqlite");
        let queue = DurableTaskQueue::open(&db_path)?;
        let channels = ChannelStore::open(&db_path)?;
        let feed_store = Arc::new(gm_goals::FeedStore::open(
            config.storage_dir.join("feed.sqlite"),
        )?);
        let _ = feed_store.seed_getting_started_if_never_seeded();
        let quiet_gate = gm_goals::QuietGate::new(gm_goals::QuietGateConfig::default());
        let shared_paths = MuseRuntimePaths::shared(&config.storage_dir);
        let _ = gm_goals::ProactivePrefsStore::new(shared_paths.clone())
            .sync_with_quiet_gate(&quiet_gate);

        let comp_port_val = config
            .openbot
            .companion_bind_addr
            .parse::<SocketAddr>()
            .map(|a| a.port())
            .unwrap_or(7433);
        let spaces_port_val = config
            .muse
            .spaces_port
            .unwrap_or_else(|| comp_port_val.saturating_add(1));

        let provenance = ProvenanceStore::open(shared_paths.provenance_db_path())?;
        let spaces = SpacesManager::new(
            shared_paths.clone(),
            Arc::clone(&audit),
            config.muse.clone(),
            comp_port_val,
        );
        let discretion = DiscretionStore::new(shared_paths);

        let a2a = A2aCoordinator::new(
            roster.clone(),
            queue.clone(),
            channels.clone(),
            config.openbot.max_handoff_depth,
        );
        let compactor = ContextCompactor::new(
            config.muse.context_tokens,
            config.openbot.compaction_trigger_ratio,
            4,
        );
        let (agui_events_tx, _) = broadcast::channel(512);

        Ok(Self {
            config,
            runtime_demo: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            runtime_paused: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            runtime_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            roster,
            queue,
            channels,
            a2a,
            compactor,
            feed_store,
            quiet_gate,
            spaces,
            discretion,
            provenance,
            companion_port: Arc::new(AtomicU16::new(comp_port_val)),
            spaces_port: Arc::new(AtomicU16::new(spaces_port_val)),
            gui_seat: Arc::new(AsyncMutex::new(())),
            daemon_events_tx,
            agui_events_tx,
            audit,
            scrubber: SecretScrubber::new(),
        })
    }

    pub fn set_runtime_paused(&self, paused: bool) -> Result<()> {
        self.runtime_paused
            .store(paused, std::sync::atomic::Ordering::SeqCst);
        if paused {
            for task in self.queue.list_tasks(None)? {
                if task.status == crate::QueuedTaskStatus::Running {
                    self.queue.pause(&task.id)?;
                }
            }
        }
        Ok(())
    }

    pub fn runtime_status(&self) -> serde_json::Value {
        serde_json::json!({
            "demo_mode": self.runtime_demo.load(std::sync::atomic::Ordering::SeqCst),
            "protocol_version": 1,
            "worker_running": self.runtime_active.load(std::sync::atomic::Ordering::SeqCst),
            "host_desktop_opt_in": self.config.safety.allow_host_desktop,
            "shell_environment": if self.config.workspace_vm.enabled { "offline_vm_configured_unverified" } else { "unavailable" },
            "platform": std::env::consts::OS,
            "companion_port": self.companion_port(),
            "spaces_port": self.spaces_port(),
            "enable_spaces": self.config.muse.enable_spaces,
            "enable_heartbeat": self.config.muse.enable_heartbeat,
            "enable_dreams": self.config.muse.enable_dreams,
        })
    }

    pub fn config(&self) -> &GmConfig {
        &self.config
    }

    pub fn shared_paths(&self) -> MuseRuntimePaths {
        MuseRuntimePaths::shared(&self.config.storage_dir)
    }

    pub fn roster(&self) -> &BotRoster {
        &self.roster
    }

    pub fn queue(&self) -> &DurableTaskQueue {
        &self.queue
    }

    pub fn channels(&self) -> &ChannelStore {
        &self.channels
    }

    pub fn a2a(&self) -> &A2aCoordinator {
        &self.a2a
    }

    pub fn compactor(&self) -> &ContextCompactor {
        &self.compactor
    }

    pub fn feed_store(&self) -> &Arc<gm_goals::FeedStore> {
        &self.feed_store
    }

    pub fn quiet_gate(&self) -> &gm_goals::QuietGate {
        &self.quiet_gate
    }

    pub fn spaces(&self) -> &SpacesManager {
        &self.spaces
    }

    pub fn discretion(&self) -> &DiscretionStore {
        &self.discretion
    }

    pub fn provenance(&self) -> &ProvenanceStore {
        &self.provenance
    }

    pub fn goal_manager(&self) -> gm_goals::GoalWorkspaceManager {
        gm_goals::GoalWorkspaceManager::new(self.shared_paths())
    }

    pub fn prefs_store(&self) -> gm_goals::ProactivePrefsStore {
        gm_goals::ProactivePrefsStore::new(self.shared_paths())
    }

    pub fn skill_registry(&self) -> Result<SkillRegistry> {
        SkillRegistry::open_with_scrubber(&self.shared_paths(), self.scrubber.clone())
    }

    pub fn companion_port(&self) -> u16 {
        self.companion_port.load(Ordering::SeqCst)
    }

    pub fn spaces_port(&self) -> u16 {
        self.spaces_port.load(Ordering::SeqCst)
    }

    pub fn companion_port_atomic(&self) -> Arc<AtomicU16> {
        Arc::clone(&self.companion_port)
    }

    pub fn spaces_port_atomic(&self) -> Arc<AtomicU16> {
        Arc::clone(&self.spaces_port)
    }

    pub fn set_companion_port(&self, port: u16) {
        self.companion_port.store(port, Ordering::SeqCst);
        self.spaces.set_companion_port(port);
    }

    pub fn set_spaces_port(&self, port: u16) {
        self.spaces_port.store(port, Ordering::SeqCst);
    }

    pub fn set_ports(&self, companion: u16, spaces: u16) {
        self.set_companion_port(companion);
        self.set_spaces_port(spaces);
    }

    pub fn sync_quiet_gate_from_prefs(&self) -> Result<()> {
        gm_goals::ProactivePrefsStore::new(self.shared_paths())
            .sync_with_quiet_gate(&self.quiet_gate)?;
        Ok(())
    }

    pub fn audit(&self) -> &Arc<AuditLogger> {
        &self.audit
    }

    pub fn scrubber(&self) -> &SecretScrubber {
        &self.scrubber
    }

    pub fn agui_sender(&self) -> broadcast::Sender<AgUiEvent> {
        self.agui_events_tx.clone()
    }

    pub fn daemon_event_sender(&self) -> broadcast::Sender<DaemonEvent> {
        self.daemon_events_tx.clone()
    }

    pub fn emit_agui(
        &self,
        event_type: impl Into<String>,
        run_id: impl Into<String>,
        bot_id: impl Into<String>,
        payload: serde_json::Value,
    ) {
        let mut evt = AgUiEvent {
            sequence: 0,
            event_type: event_type.into(),
            run_id: run_id.into(),
            bot_id: bot_id.into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            payload,
        };
        match self
            .queue
            .append_event(&serde_json::to_value(&evt).unwrap_or_default())
        {
            Ok(sequence) => {
                evt.sequence = sequence;
                let _ = self.agui_events_tx.send(evt);
            }
            Err(error) => tracing::error!(%error, "Failed to persist runtime event"),
        }
    }

    /// Submit a task into a bot's durable FIFO queue and emit `QueueUpdated`.
    pub fn submit_task(
        &self,
        bot_id: Option<&str>,
        channel_id: Option<&str>,
        instruction: &str,
        priority: i32,
    ) -> Result<QueuedTask> {
        let target_bot = bot_id.unwrap_or(&self.config.openbot.default_bot_id);
        if self.roster.get_bot(target_bot).is_none() {
            return Err(GmError::Config(format!("Unknown bot '{target_bot}'")));
        }
        if instruction.trim().is_empty() {
            return Err(GmError::Config("Instruction cannot be empty".into()));
        }
        let channel = channel_id
            .map(|id| {
                self.channels
                    .get_channel(id)?
                    .ok_or_else(|| GmError::Config(format!("Unknown channel '{id}'")))
            })
            .transpose()?;
        if channel.as_ref().is_some_and(|c| c.archived || c.paused) {
            return Err(GmError::Config("Channel is archived or paused".into()));
        }
        let task = self.queue.enqueue(
            target_bot,
            channel.as_ref().map(|c| c.id.as_str()),
            instruction,
            priority,
        )?;
        self.broadcast_queue_state(target_bot);
        Ok(task)
    }

    pub fn create_channel(
        &self,
        id: &str,
        topic: &str,
        owner_bot_id: Option<&str>,
        participants: &[String],
    ) -> Result<ChannelInfo> {
        for bot_id in participants.iter().map(String::as_str).chain(owner_bot_id) {
            if self.roster.get_bot(bot_id).is_none() {
                return Err(GmError::Config(format!("Unknown bot '{bot_id}'")));
            }
        }
        let mut members = participants.to_vec();
        if let Some(owner) = owner_bot_id
            && !members.iter().any(|id| id == owner)
        {
            members.push(owner.to_string());
        }
        members.sort();
        members.dedup();
        self.channels
            .create_channel(id, topic, owner_bot_id, &members)
    }

    /// Post a message in a channel and automatically enqueue tasks for any `@bot` mentions
    /// (or the channel's `owner_bot_id` if a human posts without explicit `@bot` mentions).
    pub fn post_channel_message(
        &self,
        channel_id: &str,
        author: &str,
        bot_id: Option<&str>,
        content: &str,
    ) -> Result<(ChannelMessageRecord, Vec<QueuedTask>)> {
        let chan_info = self
            .channels
            .get_channel(channel_id)?
            .ok_or_else(|| GmError::Config(format!("Unknown channel '{channel_id}'")))?;
        let valid_ids: Vec<String> = self.roster.list_bots().into_iter().map(|b| b.id).collect();
        // Validate the owner before persisting a human message that needs its responder.
        if bot_id.is_none()
            && !chan_info.paused
            && crate::channels::extract_bot_mentions(content, &valid_ids).is_empty()
            && let Some(owner) = &chan_info.owner_bot_id
            && self.roster.get_bot(owner).is_none()
        {
            return Err(GmError::Config(format!("Unknown channel owner '{owner}'")));
        }
        let msg = self
            .channels
            .post_message(channel_id, author, bot_id, content, &valid_ids)?;

        let _ = self.daemon_events_tx.send(DaemonEvent::ChannelMessage {
            channel_id: msg.channel_id.clone(),
            author: msg.author.clone(),
            bot_id: msg.bot_id.clone(),
            content: msg.content.clone(),
            timestamp: msg.timestamp.clone(),
        });

        let mut spawned = Vec::new();
        if !chan_info.paused {
            if !msg.mentioned_bots.is_empty() {
                for target_bot in &msg.mentioned_bots {
                    // Prevent a bot from self-triggering in a loop
                    if bot_id != Some(target_bot.as_str()) {
                        let t =
                            self.submit_task(Some(target_bot), Some(&msg.channel_id), content, 5)?;
                        spawned.push(t);
                    }
                }
            } else if bot_id.is_none()
                && let Some(owner) = chan_info.owner_bot_id
            {
                let t = self.submit_task(Some(&owner), Some(&msg.channel_id), content, 0)?;
                spawned.push(t);
            }
        }

        Ok((msg, spawned))
    }

    pub fn control_task(&self, task_id: &str, action: &QueueControlAction) -> Result<bool> {
        let existing = self.queue.get_task(task_id)?;
        let res = match action {
            QueueControlAction::Pause => self.queue.pause(task_id)?,
            QueueControlAction::Resume => self.queue.resume(task_id)?,
            QueueControlAction::Retry => self.queue.retry(task_id)?,
            QueueControlAction::Cancel => self.queue.cancel(task_id)?,
            QueueControlAction::Reassign { target_bot_id } => {
                if self.roster.get_bot(target_bot_id).is_none() {
                    return Err(GmError::Config(format!("Unknown bot '{target_bot_id}'")));
                }
                let ok = self.queue.reassign(task_id, target_bot_id)?;
                self.broadcast_queue_state(target_bot_id);
                ok
            }
        };
        if let Some(t) = existing {
            self.broadcast_queue_state(&t.bot_id);
        }
        Ok(res)
    }

    pub fn broadcast_queue_state(&self, bot_id: &str) {
        if let Ok(tasks) = self.queue.list_tasks(Some(bot_id)) {
            let queued = tasks
                .iter()
                .filter(|t| t.status == crate::queue::QueuedTaskStatus::Queued)
                .count();
            let running_task_id = tasks
                .iter()
                .find(|t| t.status == crate::queue::QueuedTaskStatus::Running)
                .map(|t| t.id.clone());
            let _ = self.daemon_events_tx.send(DaemonEvent::QueueUpdated {
                bot_id: bot_id.to_string(),
                queued,
                running_task_id,
            });
        }
    }

    /// Claim and execute the next FIFO task for `bot_id` using `AgentLoop`.
    /// Acquires `GuiSeatMutex` only if the bot profile has `allow_gui == true`, allowing
    /// non-GUI bots (`researcher`, `coder`, `guardian`) to run concurrently in parallel.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_next_for_bot<P: Platform>(
        &self,
        bot_id: &str,
        platform: Arc<P>,
        llm: Arc<HybridLlmClient>,
        router: LlmRouter,
        kill_switch: KillSwitch,
        blocklist: BlocklistManager,
        interaction_handler: Option<Arc<dyn InteractionHandler>>,
    ) -> Result<Option<QueuedTask>> {
        let profile = self
            .roster
            .get_bot(bot_id)
            .ok_or_else(|| GmError::Config(format!("Unknown bot '{bot_id}'")))?;

        let sandbox = self
            .roster
            .workspace_sandbox(bot_id)?
            .with_scrubber(self.scrubber.clone())
            .with_vm_config(self.config.workspace_vm.clone());

        let router = router.with_preference(
            profile.preferred_provider.as_deref(),
            profile.preferred_model.as_deref(),
        )?;

        let Some(task) = self.queue.claim_next_fifo(bot_id)? else {
            return Ok(None);
        };
        self.broadcast_queue_state(bot_id);

        self.emit_agui(
            "RUN_STARTED",
            &task.id,
            bot_id,
            serde_json::json!({
                "instruction": task.instruction,
                "channel_id": task.channel_id,
            }),
        );

        let a2a = self.a2a.clone();
        let from_bot = bot_id.to_string();
        let task_chan = task.channel_id.clone();
        let current_depth = task.handoff_depth;

        let handler = interaction_handler.unwrap_or_else(|| Arc::new(NonInteractiveHandler));
        let sup_for_progress = self.clone();
        let run_id_for_progress = task.id.clone();
        let bot_for_progress = bot_id.to_string();

        let history = self.queue.load_checkpoint(&task.id)?;
        let checkpoint_queue = self.queue.clone();
        let checkpoint_task = task.id.clone();
        let queue_for_mutation = self.queue.clone();
        let mutation_id = task.id.clone();
        let allow_gui = profile.allow_gui && self.config.safety.allow_host_desktop;
        let (primary_target, _) = router.route(if allow_gui {
            TaskType::CuAxStep
        } else {
            TaskType::Planning
        });
        let (muse_messages, legacy_prompt) = self.build_task_system_messages(
            &profile,
            task.channel_id.as_deref(),
            allow_gui,
            &primary_target.model,
        )?;

        let mut agent = AgentLoop::new(
            platform.clone(),
            Arc::new(llm.scoped_job()),
            router,
            kill_switch,
            blocklist,
            AgentLoopConfig {
                trajectory_dir: Some(self.config.storage_dir.join("trajectories")),
                ..Default::default()
            },
        )
        .with_checkpoint(history, move |records| {
            checkpoint_queue.store_checkpoint(&checkpoint_task, &serde_json::to_value(records)?)
        })
        .with_capabilities(AgentCapabilities {
            allow_gui,
            allow_shell: profile.allow_shell,
            allow_network: profile.allow_network,
        })
        .with_secret_injection(self.config.safety.allow_secret_injection)
        .with_system_prompt(legacy_prompt);
        if let Some(sys_msgs) = muse_messages {
            agent = agent.with_system_messages(sys_msgs);
        }
        let agent = agent
            .with_mutation_callback(move |in_flight| {
                queue_for_mutation.set_mutating_in_flight(&mutation_id, in_flight)
            })
            .with_workspace_sandbox(sandbox)
            .with_vault_resolver(VaultSecretResolver::default())
            .with_secret_scrubber(self.scrubber.clone())
            .with_audit(Arc::clone(&self.audit))
            .with_interaction_handler(handler)
            .with_fanout_depth(task.handoff_depth, self.config.muse.subagent_fanout_depth)
            .with_delegation_callback(move |target_bot, goal, artifacts| {
                let (_, child_task) = a2a.handoff(
                    &from_bot,
                    target_bot,
                    task_chan.as_deref(),
                    goal,
                    "Delegated from active agent loop",
                    artifacts,
                    current_depth,
                )?;
                Ok(format!(
                    "Delegated to @{target_bot} as task {}",
                    child_task.id
                ))
            })
            .with_progress_callback(move |step, action_summary| {
                sup_for_progress.emit_agui(
                    "TOOL_CALL_START",
                    &run_id_for_progress,
                    &bot_for_progress,
                    serde_json::json!({
                        "step": step,
                        "action": action_summary,
                    }),
                );
                let _ = sup_for_progress
                    .daemon_events_tx
                    .send(DaemonEvent::TaskProgress {
                        task_id: run_id_for_progress.clone(),
                        step,
                        action_summary: action_summary.to_string(),
                    });
            })
            .with_workflow_delegate(Arc::new(OpenBotWorkflowDelegate {
                supervisor: self.clone(),
                bot_id: bot_id.to_string(),
                task_id: task.id.clone(),
            }));

        let memory = self.roster.memory_store(bot_id)?;
        let memories = memory.search(&task.instruction, 5)?;
        let memory_context = memories
            .iter()
            .map(|record| {
                format!(
                    "[memory {} source={:?}] {}",
                    record.id,
                    record.source,
                    record.body.chars().take(2000).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let contextual_instruction = if memory_context.is_empty() {
            task.instruction.clone()
        } else {
            format!(
                "{}\n\n{}",
                task.instruction,
                gm_safety::format_untrusted_context(&memory_context, "bot_workspace_memory")
            )
        };

        let effective_instruction = if let Some(ref note) = task.human_resume_note {
            format!(
                "[Bot Persona: {} ({})]\n{}\n[Operator Wheel Resume Note]: {}",
                profile.display_name, profile.role_title, contextual_instruction, note
            )
        } else {
            format!(
                "[Bot Persona: {} ({})]\n{}",
                profile.display_name, profile.role_title, contextual_instruction
            )
        };

        let _seat = if allow_gui {
            let mut waiting = tokio::time::interval(std::time::Duration::from_millis(20));
            let seat = loop {
                tokio::select! {biased;
                    _=waiting.tick()=>{
                        if self.queue.get_task(&task.id)?.is_some_and(|task|matches!(task.status,crate::QueuedTaskStatus::PauseRequested|crate::QueuedTaskStatus::CancelRequested)) {
                            self.queue.acknowledge_control(&task.id)?;self.broadcast_queue_state(bot_id);return self.queue.get_task(&task.id);
                        }
                    }
                    seat=self.gui_seat.lock()=>break seat,
                }
            };
            Some(seat)
        } else {
            None
        };
        let mut run = Box::pin(agent.run_task(&task.id, &effective_instruction));
        let mut control_tick = tokio::time::interval(std::time::Duration::from_millis(20));
        let result = loop {
            tokio::select! {
                biased;
                _ = control_tick.tick() => {
                    let current = self.queue.get_task(&task.id)?.ok_or_else(|| GmError::Config("Active task disappeared".into()))?;
                    if matches!(current.status, crate::QueuedTaskStatus::PauseRequested | crate::QueuedTaskStatus::CancelRequested) {
                        // Dropping the future stops model/tool requests; acknowledge only after drop.
                        break None;
                    }
                }
                result = &mut run => break Some(result),
            }
        };
        drop(run);
        let Some(result) = result else {
            if allow_gui {
                tokio::time::timeout(
                    std::time::Duration::from_millis(100),
                    platform.cancel_task(&task.id),
                )
                .await
                .map_err(|_| {
                    GmError::Timeout("Desktop cancellation was not acknowledged".into())
                })??;
            }
            self.queue.acknowledge_control(&task.id)?;
            self.emit_agui(
                "RUN_STOPPED",
                &task.id,
                bot_id,
                serde_json::json!({"reason":"operator_control"}),
            );
            self.broadcast_queue_state(bot_id);
            return self.queue.get_task(&task.id);
        };

        match result {
            Ok(summary) => {
                if let Some(rest) = summary.strip_prefix("[WAITING_FOR_HUMAN] ") {
                    let _ = self.queue.mark_waiting_for_human(&task.id, rest);
                    let _ = self
                        .daemon_events_tx
                        .send(DaemonEvent::WheelHandoffRequested {
                            task_id: task.id.clone(),
                            bot_id: bot_id.to_string(),
                            blocker: rest.to_string(),
                            instructions: "Resolve blocker and click Hand Back to Bot".into(),
                        });
                    self.emit_agui(
                        "CUSTOM",
                        &task.id,
                        bot_id,
                        serde_json::json!({
                            "name": "TAKE_THE_WHEEL",
                            "blocker": rest,
                        }),
                    );
                } else {
                    if !self.queue.complete(&task.id, &summary)? {
                        self.queue.acknowledge_control(&task.id)?;
                        return self.queue.get_task(&task.id);
                    }
                    if let Some(ref chan) = task.channel_id {
                        let valid_ids: Vec<String> =
                            self.roster.list_bots().into_iter().map(|b| b.id).collect();
                        let _ = self.channels.post_message(
                            chan,
                            &profile.display_name,
                            Some(bot_id),
                            &summary,
                            &valid_ids,
                        );
                    }
                    self.emit_agui(
                        "RUN_FINISHED",
                        &task.id,
                        bot_id,
                        serde_json::json!({ "summary": summary }),
                    );
                }
            }
            Err(e) => {
                let err_str = e.to_string();
                if !self.queue.fail(&task.id, &err_str)? {
                    self.queue.acknowledge_control(&task.id)?;
                }
                self.emit_agui(
                    "RUN_ERROR",
                    &task.id,
                    bot_id,
                    serde_json::json!({ "error": err_str }),
                );
            }
        }

        self.broadcast_queue_state(bot_id);
        let updated = self.queue.get_task(&task.id)?;
        if let Some(current) = &updated {
            let _ = self.daemon_events_tx.send(DaemonEvent::TaskFinished {
                task_id: task.id.clone(),
                ok: current.status == crate::QueuedTaskStatus::Completed,
                message: current
                    .result_summary
                    .clone()
                    .or_else(|| current.blocker_note.clone())
                    .unwrap_or_else(|| current.status.as_str().into()),
            });
        }
        Ok(updated)
    }

    pub fn update_channel_instructions(
        &self,
        channel_id: &str,
        instructions: &str,
    ) -> Result<bool> {
        self.channels
            .update_channel_instructions(channel_id, instructions)
    }

    pub fn build_task_system_messages(
        &self,
        profile: &BotProfile,
        channel_id: Option<&str>,
        allow_gui: bool,
        model_display_name: &str,
    ) -> Result<(Option<Vec<gm_llm::ChatMessage>>, String)> {
        let channel_block = channel_id.and_then(|chan_id| {
            self.channels
                .get_channel(chan_id)
                .ok()
                .flatten()
                .and_then(|chan| {
                    let trimmed = chan.instructions.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(format!(
                            "## Channel Instructions (#{})\n{}",
                            chan.id, trimmed
                        ))
                    }
                })
        });

        if !self.config.muse.enable_muse_prompt {
            let legacy = match channel_block {
                Some(chan) => format!("{}\n\n{chan}", profile.system_prompt),
                None => profile.system_prompt.clone(),
            };
            return Ok((None, legacy));
        }

        let bot_paths = self.roster.runtime_paths_for_bot(&profile.id);
        let identity_existing = bot_paths
            .identity_md()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        let profile_trimmed = profile.system_prompt.trim();
        let combined_channel = match (
            !profile_trimmed.is_empty() && !identity_existing.contains(profile_trimmed),
            channel_block,
        ) {
            (true, Some(chan)) => Some(format!(
                "## Bot Profile Instructions ({})\n{}\n\n{}",
                profile.id, profile_trimmed, chan
            )),
            (true, None) => Some(format!(
                "## Bot Profile Instructions ({})\n{}",
                profile.id, profile_trimmed
            )),
            (false, chan) => chan,
        };

        let mut input = PromptInput::new(
            PromptSurface::OpenBotTask,
            &bot_paths,
            &self.config.muse,
            model_display_name,
        );
        input.channel_instructions = combined_channel.as_deref();
        input.allow_host_desktop = allow_gui;
        input.workspace_vm_enabled = self.config.workspace_vm.enabled;
        Ok((
            Some(MusePromptBuilder::build_messages(&input)?),
            String::new(),
        ))
    }

    pub fn plan_forget(&self, bot_id: Option<&str>, request: &str) -> Result<ForgetPlan> {
        let target_bot = bot_id.unwrap_or(&self.config.openbot.default_bot_id);
        let paths = self.shared_paths();
        let memory = self.roster.memory_store(target_bot)?;
        let mut res = ForgetResources::new(paths, memory, self.provenance.clone());
        res.audit_logger = Some(Arc::clone(&self.audit));
        res.forget_redact_audit = self.config.muse.forget_redact_audit;
        ForgetPlanner::plan(&res, request)
    }

    pub fn confirm_forget(
        &self,
        bot_id: Option<&str>,
        plan_id: &str,
        verification_query: Option<&str>,
        confirmed: bool,
    ) -> Result<gm_memory::ForgetReport> {
        let target_bot = bot_id.unwrap_or(&self.config.openbot.default_bot_id);
        let paths = self.shared_paths();
        let pending_path = paths.forget_pending_path();
        let (raw, _) = SharedFileStore::global()
            .read_text_with_sha256(&pending_path)?
            .ok_or_else(|| {
                GmError::Config("No pending Forget plan found; run ForgetPlan first".into())
            })?;
        let plan: ForgetPlan = serde_json::from_str(&raw)?;
        if plan.plan_id != plan_id {
            return Err(GmError::Config(format!(
                "Forget plan ID mismatch (pending={}, requested={plan_id})",
                plan.plan_id
            )));
        }
        let memory = self.roster.memory_store(target_bot)?;
        let mut res = ForgetResources::new(paths, memory, self.provenance.clone());
        res.audit_logger = Some(Arc::clone(&self.audit));
        res.forget_redact_audit = self.config.muse.forget_redact_audit;
        let feed_store = Arc::clone(&self.feed_store);
        res.feed_reset_fence = Some(Arc::new(move |_| {
            feed_store.reset_feed_signals()?;
            Ok(())
        }));
        let report = ForgetExecutor::confirm(&plan, &res, confirmed, verification_query)?;
        let _ = self.daemon_events_tx.send(DaemonEvent::ActivityUpdated {
            reason: "forget_confirmed".into(),
        });
        Ok(report)
    }
}

struct OpenBotWorkflowDelegate {
    supervisor: OpenBotSupervisor,
    bot_id: String,
    task_id: String,
}

#[async_trait::async_trait]
impl MuseWorkflowDelegate for OpenBotWorkflowDelegate {
    async fn memory_search(&self, query: &str, limit: Option<u32>) -> Result<String> {
        let store = self.supervisor.roster.memory_store(&self.bot_id)?;
        let cap = limit.unwrap_or(8).clamp(1, 50) as usize;
        let hits = store.search(query, cap)?;
        Ok(serde_json::to_string_pretty(&hits)?)
    }

    async fn memory_explain(&self, target: &str) -> Result<String> {
        let explanations = self.supervisor.provenance.explain_memory(target)?;
        Ok(serde_json::to_string_pretty(&explanations)?)
    }

    async fn forget_plan(&self, request: &str) -> Result<String> {
        let plan = self.supervisor.plan_forget(Some(&self.bot_id), request)?;
        Ok(serde_json::to_string_pretty(&plan)?)
    }

    async fn forget_confirm(&self, plan_id: &str) -> Result<String> {
        // `AgentLoop` enforces `ActionTier::Irreversible` interaction approval before reaching here.
        let report = self
            .supervisor
            .confirm_forget(Some(&self.bot_id), plan_id, None, true)?;
        Ok(serde_json::to_string_pretty(&report)?)
    }

    async fn goal_op(
        &self,
        op: GoalOpKind,
        slug: Option<&str>,
        payload: &serde_json::Value,
    ) -> Result<String> {
        let mgr = self.supervisor.goal_manager();
        let target_slug = || -> Result<String> {
            slug.or_else(|| payload.get("slug").and_then(|v| v.as_str()))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .ok_or_else(|| GmError::Config("Goal slug or id is required".into()))
        };
        let extract_stop_conditions = |val: &serde_json::Value| -> Vec<String> {
            val.get("stop_conditions")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };

        let val = match op {
            GoalOpKind::Create => {
                let title = payload
                    .get("title")
                    .and_then(|v| v.as_str())
                    .or(slug)
                    .unwrap_or("")
                    .trim();
                let desc = payload
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let cat = payload.get("category").and_then(|v| v.as_str());
                let stops = extract_stop_conditions(payload);
                let goal = mgr.create_goal(title, desc, cat, stops)?;
                serde_json::to_value(goal)?
            }
            GoalOpKind::CreateSubgoal => {
                let parent = payload
                    .get("parent_slug")
                    .or_else(|| payload.get("parent_goal_id"))
                    .and_then(|v| v.as_str())
                    .or(slug)
                    .ok_or_else(|| GmError::Config("Parent goal slug/id is required".into()))?;
                let title = payload
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                let desc = payload
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let stops = extract_stop_conditions(payload);
                let sub = mgr.create_subgoal(parent, title, desc, stops)?;
                serde_json::to_value(sub)?
            }
            GoalOpKind::Update => {
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
                let updated = mgr.update_goal(&target, title, desc, stops, sched, cat)?;
                serde_json::to_value(updated)?
            }
            GoalOpKind::Complete => {
                let target = target_slug()?;
                let verified = payload
                    .get("verified")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                serde_json::to_value(mgr.complete_goal(&target, verified)?)?
            }
            GoalOpKind::Abandon => {
                let target = target_slug()?;
                serde_json::to_value(mgr.abandon_goal(&target)?)?
            }
            GoalOpKind::Delete => {
                let target = target_slug()?;
                serde_json::json!({ "deleted_slugs": mgr.delete_goal(&target)? })
            }
        };
        let _ = self
            .supervisor
            .daemon_events_tx
            .send(DaemonEvent::ActivityUpdated {
                reason: format!("goal_{op:?}").to_lowercase(),
            });
        Ok(serde_json::to_string_pretty(&val)?)
    }

    async fn space_op(
        &self,
        op: SpaceOpKind,
        slug: &str,
        payload: &serde_json::Value,
    ) -> Result<String> {
        if !self.supervisor.config.muse.enable_spaces {
            return Err(GmError::Config(
                "Spaces feature is disabled in MuseConfig".into(),
            ));
        }
        let spaces = self.supervisor.spaces();
        let val = match op {
            SpaceOpKind::Create => {
                let kind = match payload
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("static")
                    .trim()
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "static" => SpaceKind::Static,
                    "ts" | "typescript" => SpaceKind::Ts,
                    other => {
                        return Err(GmError::Config(format!("Unknown Space kind '{other}'")));
                    }
                };
                let title = payload.get("title").and_then(|v| v.as_str());
                let html = payload
                    .get("html")
                    .or_else(|| payload.get("content"))
                    .and_then(|v| v.as_str());
                let meta = spaces.create_space(slug, title, kind, html)?;
                let url = spaces.mint_url_for_slug(&meta.slug, self.supervisor.spaces_port())?;
                serde_json::json!({ "space": meta, "url": url })
            }
            SpaceOpKind::Edit => {
                let rel_path = payload
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("index.html");
                let content = payload
                    .get("content")
                    .or_else(|| payload.get("html"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| GmError::Config("Space edit requires content string".into()))?;
                let abs = spaces.update_space_file(slug, rel_path, content)?;
                serde_json::json!({ "slug": slug, "updated_path": abs })
            }
            SpaceOpKind::Publish => {
                let confirmed = payload
                    .get("confirmed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                serde_json::to_value(spaces.publish_space(slug, confirmed)?)?
            }
            SpaceOpKind::Unpublish => serde_json::to_value(spaces.unpublish_space(slug)?)?,
            SpaceOpKind::Delete => {
                serde_json::json!({ "slug": slug, "deleted": spaces.delete_space(slug, true)? })
            }
        };
        Ok(serde_json::to_string_pretty(&val)?)
    }

    async fn update_discretion(&self, chat_id: &str, rules: &str, loosens: bool) -> Result<String> {
        let lines: Vec<String> = rules
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        if lines.is_empty() {
            return Err(GmError::Config("At least one rule line is required".into()));
        }
        let policy = self
            .supervisor
            .discretion
            .update_user_rules(chat_id, &lines, None)?;
        self.supervisor.audit.log_governance(
            "discretion_updated",
            Some(&self.bot_id),
            AuditCategory::Permitted,
            if loosens {
                "RULE-PRIVACY-DISCRETION"
            } else {
                "RULE-POLICY-001"
            },
            if loosens {
                "Approved discretion rule loosening"
            } else {
                "Added external bridge discretion user rule"
            },
            &format!("Updated DISCRETION.md for chat '{chat_id}'"),
            serde_json::json!({ "chat_id": chat_id, "loosens": loosens }),
        );
        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "chat_id": policy.chat_id,
            "sha256": policy.sha256,
            "user_rules": policy.user_rules,
        }))?)
    }

    async fn emit_directive(&self, directive: &DirectiveKind) -> Result<String> {
        self.supervisor.emit_agui(
            "MUSE_DIRECTIVE",
            &self.task_id,
            &self.bot_id,
            serde_json::json!({ "directive": directive }),
        );
        Ok(serde_json::to_string(&serde_json::json!({
            "emitted": true,
            "directive": directive,
        }))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openbot_task_prompt_uses_compact_muse_with_channel_info_and_respects_flag() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("storage");
        let bots = storage.join("bots");
        let (tx, _) = broadcast::channel(8);

        let cfg = GmConfig {
            storage_dir: storage.clone(),
            openbot: gm_core::config::OpenBotConfig {
                bots_dir: bots.clone(),
                ..Default::default()
            },
            ..Default::default()
        };
        let sup =
            OpenBotSupervisor::open(cfg, Arc::new(AuditLogger::in_memory()), tx.clone()).unwrap();
        sup.update_channel_instructions("general", "Keep diffs small and verified.")
            .unwrap();
        let profile = sup.roster().get_bot("coder").unwrap();

        let (muse_msgs, legacy) = sup
            .build_task_system_messages(&profile, Some("general"), false, "muse-spark-1.3")
            .unwrap();
        assert!(legacy.is_empty());
        let msgs = muse_msgs.expect("Muse messages built");
        assert_eq!(msgs.len(), 2);
        assert!(msgs[0].content.contains("## Who You Are"));
        assert!(
            msgs[0]
                .content
                .contains("## Channel Instructions (#general)")
        );
        assert!(msgs[0].content.contains("Keep diffs small and verified."));
        assert!(msgs[1].content.contains("[model=muse-spark-1.3]"));

        let legacy_cfg = GmConfig {
            storage_dir: storage,
            openbot: gm_core::config::OpenBotConfig {
                bots_dir: bots,
                ..Default::default()
            },
            muse: gm_core::config::MuseConfig {
                enable_muse_prompt: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let legacy_sup =
            OpenBotSupervisor::open(legacy_cfg, Arc::new(AuditLogger::in_memory()), tx).unwrap();
        let (muse_off, legacy_str) = legacy_sup
            .build_task_system_messages(&profile, Some("general"), false, "muse-spark-1.3")
            .unwrap();
        assert!(muse_off.is_none());
        assert!(legacy_str.contains("## Channel Instructions (#general)"));
        assert!(legacy_str.contains(&profile.system_prompt));
    }

    #[tokio::test]
    async fn supervisor_fanout_depth_allows_0_to_1_refuses_1_to_2_and_preserves_a2a_depth() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("storage");
        let bots = storage.join("bots");
        let (tx, _) = broadcast::channel(16);

        let cfg = GmConfig {
            storage_dir: storage,
            openbot: gm_core::config::OpenBotConfig {
                bots_dir: bots,
                ..Default::default()
            },
            ..Default::default()
        };
        let audit = Arc::new(AuditLogger::in_memory());
        let sup = OpenBotSupervisor::open(cfg, Arc::clone(&audit), tx).unwrap();

        let platform = Arc::new(gm_platform::MockPlatform::new());
        let backend = Arc::new(gm_llm::MockLlmBackend::new());
        // Step 1-2 (depth 0 coder): DelegateToBot -> researcher, then Done.
        backend.queue_response(
            r#"{"action":"DelegateToBot","params":{"target_bot_id":"researcher","goal":"Gather facts"}}"#,
        );
        backend.queue_response(r#"{"action":"Done","params":{"summary":"Coder finished"}}"#);
        // Step 3-4 (depth 1 researcher): DelegateToBot -> coder (refused), then Done.
        backend.queue_response(
            r#"{"action":"DelegateToBot","params":{"target_bot_id":"coder","goal":"Should not spawn"}}"#,
        );
        backend
            .queue_response(r#"{"action":"Done","params":{"summary":"Researcher handled limit"}}"#);

        let llm = Arc::new(HybridLlmClient::new(
            Arc::clone(&backend),
            Arc::new(gm_llm::BudgetManager::new(
                gm_core::config::BudgetConfig::default(),
            )),
        ));
        let router = LlmRouter::new(gm_core::config::ProvidersConfig::default());

        let parent = sup
            .submit_task(Some("coder"), Some("general"), "Analyze system", 0)
            .unwrap();
        assert_eq!(parent.handoff_depth, 0);

        let done_coder = sup
            .run_next_for_bot(
                "coder",
                Arc::clone(&platform),
                Arc::clone(&llm),
                router.clone(),
                KillSwitch::new(),
                BlocklistManager::default(),
                None,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done_coder.status, crate::QueuedTaskStatus::Completed);

        let researcher_tasks = sup.queue().list_tasks(Some("researcher")).unwrap();
        assert_eq!(researcher_tasks.len(), 1);
        assert_eq!(researcher_tasks[0].handoff_depth, 1);

        let done_researcher = sup
            .run_next_for_bot(
                "researcher",
                platform,
                llm,
                router,
                KillSwitch::new(),
                BlocklistManager::default(),
                None,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done_researcher.status, crate::QueuedTaskStatus::Completed);

        // No child task was enqueued back onto `coder` at depth 2.
        let coder_tasks = sup.queue().list_tasks(Some("coder")).unwrap();
        assert_eq!(coder_tasks.len(), 1);

        // Existing A2A max_handoff_depth (4) remains unchanged.
        let (_, depth4_task) = sup
            .a2a()
            .handoff(
                "coder",
                "researcher",
                None,
                "Depth 4 allowed",
                "ctx",
                &[],
                3,
            )
            .unwrap();
        assert_eq!(depth4_task.handoff_depth, 4);
        assert!(
            sup.a2a()
                .handoff(
                    "coder",
                    "researcher",
                    None,
                    "Depth 5 blocked",
                    "ctx",
                    &[],
                    4
                )
                .is_err()
        );
    }
}
