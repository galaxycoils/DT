use crate::a2a::A2aCoordinator;
use crate::channels::{ChannelInfo, ChannelMessageRecord, ChannelStore, MessageReactionRecord};
use crate::compaction::ContextCompactor;
use crate::connectors::ConnectorStore;
use crate::direct_chat::{DirectChatStore, DirectMessageRecord};
use crate::discretion::DiscretionStore;
use crate::library::{LibraryFilter, LibraryPage, LibraryStore};
use crate::queue::{DurableTaskQueue, QueuedTask};
use crate::roster::{BotProfile, BotRoster};
use crate::rules::ApprovalRulesStore;
use crate::sections::SectionStore;
use crate::spaces::{SpaceKind, SpacesManager};
use crate::vault::{KeychainSecretBackend, SecretBackend, VaultStore};
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

/// Structured result of a direct 1:1 DM turn (`chat` reply or `/do`/`queue` task handoff).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectChatDispatchOutcome {
    pub user_message: DirectMessageRecord,
    pub assistant_message: DirectMessageRecord,
    pub queued_task: Option<QueuedTask>,
}

/// Coordinates the multi-bot roster, durable SQLite FIFO task queues, shared channels,
/// A2A handoffs, `GuiSeatMutex` serialization, Muse Stores (`SpacesManager`, `DiscretionStore`,
/// `ProvenanceStore`, `FeedStore`), and AG-UI / DaemonEvent broadcasting.
#[derive(Clone)]
pub struct OpenBotSupervisor {
    pub(crate) config: GmConfig,
    ui_override: Arc<parking_lot::RwLock<gm_core::config::UiConfig>>,
    pub(crate) providers_override: Arc<parking_lot::RwLock<gm_core::config::ProvidersConfig>>,
    pub(crate) connections: Arc<crate::provider_connections::ProviderConnections>,
    host_desktop_override: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) runtime_demo: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) runtime_paused: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) runtime_active: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) roster: BotRoster,
    pub(crate) queue: DurableTaskQueue,
    channels: ChannelStore,
    sections: SectionStore,
    direct_chat: DirectChatStore,
    vault: VaultStore,
    rules: ApprovalRulesStore,
    connectors: ConnectorStore,
    marketplace: crate::marketplace::MarketplaceStore,
    billing: crate::billing::BillingStore,
    library: LibraryStore,
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
    pub(crate) daemon_events_tx: broadcast::Sender<DaemonEvent>,
    agui_events_tx: broadcast::Sender<AgUiEvent>,
    audit: Arc<AuditLogger>,
    scrubber: SecretScrubber,
}

impl OpenBotSupervisor {
    pub fn open(
        config: GmConfig,
        audit: Arc<AuditLogger>,
        daemon_events_tx: broadcast::Sender<DaemonEvent>,
    ) -> Result<Self> {
        Self::open_with_secret_backend(
            config,
            audit,
            daemon_events_tx,
            Arc::new(KeychainSecretBackend::default()),
        )
    }

    pub fn open_with_secret_backend(
        mut config: GmConfig,
        audit: Arc<AuditLogger>,
        daemon_events_tx: broadcast::Sender<DaemonEvent>,
        secret_backend: Arc<dyn SecretBackend>,
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
        let sections = SectionStore::open(&db_path)?;
        let direct_chat = DirectChatStore::open(&db_path)?;
        let vault = VaultStore::open_with_backend(&db_path, Arc::clone(&secret_backend))?;
        let rules = ApprovalRulesStore::open(&db_path)?;
        let connectors = ConnectorStore::open_with_backend(&db_path, Some(secret_backend))?;
        let library = LibraryStore::open(&db_path)?;
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
        let marketplace = crate::marketplace::MarketplaceStore::open(
            config.storage_dir.join("marketplace.sqlite"),
        )?;
        let billing =
            crate::billing::BillingStore::open(config.storage_dir.join("billing.sqlite"), true)?;

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
        let ui_override = Arc::new(parking_lot::RwLock::new(config.ui.clone()));
        let mut providers = config.providers.clone();
        for default in gm_core::config::default_provider_profiles() {
            if !providers.profiles.iter().any(|p| p.id == default.id) {
                providers.profiles.push(default);
            }
        }
        for profile in &mut providers.profiles {
            if profile.preset == gm_core::config::ProviderPreset::KiloGateway
                && matches!(
                    profile.base_url.as_str(),
                    "https://api.kilo.ai/api/gateway/v1"
                        | "https://api.kilo.ai/api/gateway"
                        | "https://ai-gateway.kilo.ai/api/v1/v1"
                )
            {
                profile.base_url = "https://ai-gateway.kilo.ai/api/v1".into();
                profile.connection_state =
                    gm_core::config::ProviderConnectionState::NeedsRevalidation;
                profile.health = Default::default();
            }
            if profile.connection_state == gm_core::config::ProviderConnectionState::Connected
                && profile.health.verification_status != "verified"
            {
                profile.connection_state = gm_core::config::ProviderConnectionState::Authenticated;
            }
        }
        let providers_override = Arc::new(parking_lot::RwLock::new(providers));
        let host_desktop_override = Arc::new(std::sync::atomic::AtomicBool::new(
            config.safety.allow_host_desktop,
        ));

        Ok(Self {
            config,
            ui_override,
            providers_override,
            connections: Arc::new(Default::default()),
            host_desktop_override,
            runtime_demo: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            runtime_paused: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            runtime_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            roster,
            queue,
            channels,
            sections,
            direct_chat,
            vault,
            rules,
            connectors,
            marketplace,
            billing,
            library,
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
            "host_desktop_opt_in": self.allow_host_desktop(),
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

    pub fn ui_config(&self) -> gm_core::config::UiConfig {
        self.ui_override.read().clone()
    }

    pub fn set_ui_config(&self, ui: gm_core::config::UiConfig) {
        *self.ui_override.write() = ui;
    }

    pub fn providers_config(&self) -> gm_core::config::ProvidersConfig {
        self.providers_override.read().clone()
    }

    pub fn set_providers_config(&self, mut providers: gm_core::config::ProvidersConfig) {
        let mut live = self.providers_override.write();
        providers.profiles = live.profiles.clone();
        providers.workspace_default_profile_id = live.workspace_default_profile_id.clone();
        providers.account_default_profile_id = live.account_default_profile_id.clone();
        *live = providers;
    }

    pub fn upsert_provider_profile(
        &self,
        profile: gm_core::config::ProviderProfileConfig,
        api_key: Option<&str>,
        revalidate_credentials: bool,
    ) -> Result<gm_core::config::ProviderProfileConfig> {
        let mut cfg = self.providers_override.write();
        let mut incoming = profile;
        let origin = gm_core::config::extract_provider_origin(&incoming.base_url)?;
        gm_core::config::validate_oauth_destination(
            incoming.preset,
            incoming.auth_mode,
            incoming.protocol,
            &incoming.base_url,
        )?;
        if incoming.id.trim().is_empty() {
            return Err(GmError::Config(
                "Provider profile ID cannot be empty".into(),
            ));
        }
        // Secret references and account identity come from the daemon, never a submitted form.
        incoming.credential_ref = cfg
            .profile_by_id(&incoming.id)
            .and_then(|p| p.credential_ref.clone());
        incoming.account_id = cfg
            .profile_by_id(&incoming.id)
            .and_then(|p| p.account_id.clone());
        incoming.account_label = cfg
            .profile_by_id(&incoming.id)
            .and_then(|p| p.account_label.clone());
        if let Some(current) = cfg.profile_by_id(&incoming.id) {
            if incoming.revision != current.revision {
                return Err(GmError::Config(
                    "Provider settings changed. Reload and try again.".into(),
                ));
            }
            incoming.revision = current.revision.saturating_add(1);
        }
        if let Some(current) = cfg.profile_by_id(&incoming.id) {
            if current.credential_ref.is_some()
                && current.auth_mode != incoming.auth_mode
                && api_key.is_none()
            {
                return Err(GmError::Config(
                    "Disconnect before changing authentication type".into(),
                ));
            }
            incoming.model_catalog = current.model_catalog.clone();
            incoming.discovered_models = current.discovered_models.clone();
        }
        self.cancel_provider_sign_in(&incoming.id);
        incoming.connection_state = cfg
            .profile_by_id(&incoming.id)
            .map(|p| match p.connection_state {
                gm_core::config::ProviderConnectionState::Connected
                | gm_core::config::ProviderConnectionState::Authenticated => {
                    gm_core::config::ProviderConnectionState::Authenticated
                }
                gm_core::config::ProviderConnectionState::CredentialsSaved => {
                    gm_core::config::ProviderConnectionState::CredentialsSaved
                }
                _ => gm_core::config::ProviderConnectionState::Unconfigured,
            })
            .unwrap_or_default();
        incoming.health = cfg
            .profile_by_id(&incoming.id)
            .map(|p| p.health.clone())
            .unwrap_or_default();
        if cfg.profile_by_id(&incoming.id).is_some_and(|current| {
            incoming.base_url != current.base_url || incoming.protocol != current.protocol
        }) {
            incoming.health.catalog_status = "stale".into();
        }
        incoming.health.verification_status = "not_tested".into();
        incoming.health.verified_model = None;
        incoming.health.verified_at = None;
        incoming.last_error = None;
        if let Some(secret) = api_key.map(str::trim).filter(|s| !s.is_empty()) {
            use sha2::{Digest, Sha256};
            let account = format!("gm_provider_{:x}", Sha256::digest(incoming.id.as_bytes()));
            self.vault.backend().set_secret(&account, secret)?;
            incoming.credential_ref = Some(format!("secret://keychain/{account}"));
            incoming.connection_state = gm_core::config::ProviderConnectionState::CredentialsSaved;
            incoming.validated_origin = Some(origin);
            incoming.last_error = None;
            incoming.model_catalog.clear();
            incoming.discovered_models.clear();
            incoming.health = Default::default();
        }
        let saved = cfg.upsert_profile(
            incoming,
            revalidate_credentials || api_key.is_some_and(|value| !value.trim().is_empty()),
        )?;
        drop(cfg);
        let _ = self.daemon_events_tx.send(DaemonEvent::ActivityUpdated {
            reason: "providers_updated".into(),
        });
        Ok(saved)
    }

    pub fn set_default_provider_profiles(
        &self,
        workspace_default: Option<String>,
        account_default: Option<String>,
    ) -> Result<gm_core::config::ProvidersConfig> {
        let mut cfg = self.providers_override.write();
        if let Some(ws_id) = workspace_default {
            cfg.workspace_default_profile_id = if ws_id.trim().is_empty() {
                None
            } else {
                Some(ws_id.trim().to_string())
            };
        }
        if let Some(acc_id) = account_default {
            cfg.account_default_profile_id = if acc_id.trim().is_empty() {
                None
            } else {
                Some(acc_id.trim().to_string())
            };
        }
        let cfg = cfg.clone();
        let _ = self.daemon_events_tx.send(DaemonEvent::ActivityUpdated {
            reason: "providers_updated".into(),
        });
        Ok(cfg)
    }

    pub fn allow_host_desktop(&self) -> bool {
        self.host_desktop_override.load(Ordering::SeqCst)
    }

    pub fn set_allow_host_desktop(&self, enabled: bool) {
        self.host_desktop_override.store(enabled, Ordering::SeqCst);
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

    pub fn sections(&self) -> &SectionStore {
        &self.sections
    }

    pub fn direct_chat(&self) -> &DirectChatStore {
        &self.direct_chat
    }

    pub fn vault(&self) -> &VaultStore {
        &self.vault
    }

    pub fn rules(&self) -> &ApprovalRulesStore {
        &self.rules
    }

    pub fn connectors(&self) -> &ConnectorStore {
        &self.connectors
    }

    pub fn marketplace(&self) -> &crate::marketplace::MarketplaceStore {
        &self.marketplace
    }

    pub fn billing(&self) -> &crate::billing::BillingStore {
        &self.billing
    }

    pub fn library(&self) -> &LibraryStore {
        &self.library
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
        if !self.config.muse.enable_skills {
            return Err(GmError::Config(
                "Skills are disabled by runtime configuration (muse.enable_skills = false)".into(),
            ));
        }
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

    #[allow(clippy::too_many_arguments)]
    pub fn post_direct_chat(
        &self,
        bot_id: &str,
        role: &str,
        author: &str,
        content: &str,
        reply_to_id: Option<&str>,
        thread_id: Option<&str>,
        card: Option<&serde_json::Value>,
        request_id: Option<&str>,
    ) -> Result<DirectMessageRecord> {
        if self.roster.get_bot(bot_id).is_none() {
            return Err(GmError::Config(format!("Unknown bot '{bot_id}'")));
        }
        let rec = self.direct_chat.append_message(
            bot_id,
            role,
            author,
            content,
            reply_to_id,
            thread_id,
            card,
            request_id,
        )?;
        let val = serde_json::to_value(&rec).unwrap_or_default();
        let _ = self.daemon_events_tx.send(DaemonEvent::DirectMessageAdded {
            bot_id: rec.bot_id.clone(),
            message: val.clone(),
        });
        self.emit_agui("DIRECT_MESSAGE_ADDED", &rec.id, &rec.bot_id, val);
        Ok(rec)
    }

    pub fn toggle_message_reaction(
        &self,
        chat_kind: &str,
        message_id: &str,
        emoji: &str,
        actor: &str,
    ) -> Result<(String, Vec<MessageReactionRecord>)> {
        let kind = chat_kind.trim().to_ascii_lowercase();
        let (chat_id, reactions) = if kind == "dm" {
            self.direct_chat.toggle_reaction(message_id, emoji, actor)?
        } else {
            self.channels.toggle_reaction(message_id, emoji, actor)?
        };
        let rx_val = serde_json::to_value(&reactions).unwrap_or_default();
        let _ = self
            .daemon_events_tx
            .send(DaemonEvent::MessageReactionUpdated {
                chat_kind: kind.clone(),
                chat_id: chat_id.clone(),
                message_id: message_id.to_string(),
                reactions: rx_val.clone(),
            });
        self.emit_agui(
            "REACTION_UPDATED",
            message_id,
            &chat_id,
            serde_json::json!({
                "chat_kind": kind,
                "chat_id": chat_id,
                "message_id": message_id,
                "reactions": rx_val,
            }),
        );
        Ok((chat_id, reactions))
    }

    pub fn list_library_page(
        &self,
        bot_id: &str,
        filter: &str,
        cursor: usize,
        limit: usize,
    ) -> Result<LibraryPage> {
        self.library.query(
            bot_id,
            LibraryFilter::parse(filter),
            cursor,
            limit,
            &self.roster,
            &self.direct_chat,
            &self.spaces,
            &self.connectors,
            &self.config,
        )
    }

    pub fn bootstrap_snapshot(
        &self,
        daily_spent_usd: f64,
        paused: bool,
        helper_connected: bool,
    ) -> serde_json::Value {
        let bots = self.roster.list_bots();
        let sections = self.sections.list_sections().unwrap_or_default();
        let channels = self.channels.list_channels(true).unwrap_or_default();
        let tasks = self
            .queue
            .list_tasks(None)
            .unwrap_or_default()
            .into_iter()
            .take(100)
            .collect::<Vec<_>>();
        let routines = self.queue.list_routines().unwrap_or_default();
        let unread_counts = self.direct_chat.unread_counts().unwrap_or_default();
        let bot_statuses = self.direct_chat.list_bot_statuses().unwrap_or_default();
        let integrations = self.connectors.builtin_integrations(&self.config);
        let connected_app_count = integrations
            .iter()
            .filter(|i| i.status == "connected")
            .count()
            + self
                .connectors
                .list_connectors()
                .unwrap_or_default()
                .iter()
                .filter(|c| c.enabled)
                .count();
        let feed_posts = self.feed_store.list_posts(30).unwrap_or_default();
        let feed_ideas: Vec<gm_goals::ProactiveIdea> = {
            let custom_path = self
                .shared_paths()
                .storage_dir()
                .join("workspace/custom_ideas.json");
            SharedFileStore::global()
                .read_text_with_sha256(&custom_path)
                .ok()
                .flatten()
                .and_then(|(raw, _)| {
                    serde_json::from_str::<Vec<gm_goals::ProactiveIdea>>(&raw).ok()
                })
                .unwrap_or_default()
        };
        let goals = self.goal_manager().list_goals().unwrap_or_default();
        let spaces = self
            .spaces
            .list_spaces()
            .unwrap_or_default()
            .into_iter()
            .map(|s| {
                let url = self
                    .spaces
                    .mint_url_for_slug(&s.slug, self.spaces_port())
                    .unwrap_or_default();
                serde_json::json!({ "meta": s, "url": url })
            })
            .collect::<Vec<_>>();
        let skills = self
            .skill_registry()
            .ok()
            .map(|reg| reg.skills().to_vec())
            .unwrap_or_default();

        let custom_connectors = self.connectors.list_connectors().unwrap_or_default();

        let brief_prompt = self
            .feed_store
            .get_brief_prompt()
            .ok()
            .filter(|s| !s.is_empty());

        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "status": {
                "helper_connected": helper_connected,
                "paused": paused,
                "daily_spend_usd": daily_spent_usd,
                "daily_cap_usd": self.config.budget.daily_spend_cap_usd,
                "per_job_cap_usd": self.config.budget.per_job_spend_cap_usd,
                "companion_port": self.companion_port(),
                "spaces_port": self.spaces_port(),
                "allow_host_desktop": self.allow_host_desktop(),
            },
            "ui": self.ui_config(),
            "providers": self.providers_config(),
            "bots": bots,
            "sections": sections,
            "channels": channels,
            "recent_tasks": tasks,
            "routines": routines,
            "unread_counts": unread_counts,
            "bot_statuses": bot_statuses,
            "conversation_previews": self.direct_chat.conversation_previews().unwrap_or_default(),
            "channel_previews": self.channels.conversation_previews().unwrap_or_default(),
            "connected_app_count": connected_app_count,
            "integrations": integrations,
            "custom_connectors": custom_connectors,
            "feed": { "posts": feed_posts, "ideas": feed_ideas, "brief_prompt": brief_prompt },
            "goals": goals,
            "spaces": spaces,
            "skills": skills,
            "custom_rules": self.rules.list_rules().unwrap_or_default(),
            "vault_items": self.vault.list_items(None).unwrap_or_default(),
        })
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

    /// Broadcast an ephemeral high-frequency AG-UI delta (e.g. token stream) without hitting SQLite per token.
    pub fn emit_agui_ephemeral(
        &self,
        event_type: impl Into<String>,
        run_id: impl Into<String>,
        bot_id: impl Into<String>,
        payload: serde_json::Value,
    ) {
        let evt = AgUiEvent {
            sequence: 0,
            event_type: event_type.into(),
            run_id: run_id.into(),
            bot_id: bot_id.into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            payload,
        };
        let _ = self.agui_events_tx.send(evt);
    }

    /// Delete a sidebar section folder and clear `section_id` on any bots previously assigned to it.
    pub fn delete_section_and_clear_bots(&self, section_id: &str) -> Result<bool> {
        let clean = section_id.trim();
        let deleted = self.sections.delete_section(clean)?;
        if deleted {
            for bot in self.roster.list_bots() {
                if bot.section_id.as_deref() == Some(clean) {
                    let _ = self
                        .roster
                        .save_bot_ui_prefs(&bot.id, &serde_json::json!({ "section_id": null }));
                    let _ = self.daemon_events_tx.send(DaemonEvent::BotUpdated {
                        bot_id: bot.id.clone(),
                    });
                }
            }
            let _ = self.daemon_events_tx.send(DaemonEvent::ActivityUpdated {
                reason: "sections_updated".into(),
            });
            self.emit_agui(
                "SECTION_DELETED",
                clean,
                "",
                serde_json::json!({ "id": clean }),
            );
        }
        Ok(deleted)
    }

    /// Unified 1:1 DM dispatcher shared by IPC (`DaemonControlServer`) and Web (`CompanionWebServer`).
    /// Routes `/do <cmd>` / `/task <cmd>` (or bots configured with `task_mode_default == "queue"`) to
    /// the durable FIFO task queue; otherwise streams a contextual Muse DM turn and records the reply.
    #[allow(clippy::too_many_arguments)]
    pub async fn dispatch_direct_chat(
        &self,
        bot_id: &str,
        content: &str,
        reply_to_id: Option<&str>,
        thread_id: Option<&str>,
        llm_client: &HybridLlmClient,
        router: &LlmRouter,
        budget: Option<&gm_llm::BudgetManager>,
        kill_switch: Option<&KillSwitch>,
        shared_cfg: Option<&GmConfig>,
    ) -> Result<DirectChatDispatchOutcome> {
        let clean_bot = bot_id.trim().to_ascii_lowercase();
        let profile = self
            .roster
            .get_bot(&clean_bot)
            .ok_or_else(|| GmError::Config(format!("Unknown bot '{bot_id}'")))?;
        let trimmed = content.trim();
        if trimmed.is_empty() || content.len() > 64 * 1024 {
            return Err(GmError::Config(
                "Direct message cannot be empty or exceed 64 KiB".into(),
            ));
        }
        if kill_switch.is_some_and(|ks| ks.is_triggered()) {
            return Err(GmError::SafetyViolation(
                "Emergency stop is active; restart the runtime to resume".into(),
            ));
        }

        let user_msg = self.post_direct_chat(
            &clean_bot,
            "user",
            "You",
            content,
            reply_to_id,
            thread_id,
            None,
            None,
        )?;
        let _ = self.direct_chat.mark_chat_read("dm", &clean_bot);

        let explicit_task = trimmed
            .strip_prefix("/do ")
            .or_else(|| trimmed.strip_prefix("/task "))
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let explicit_chat = trimmed
            .strip_prefix("/chat ")
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let task_instruction = if let Some(inst) = explicit_task {
            Some(inst.to_string())
        } else if explicit_chat.is_none() && profile.task_mode_default == "queue" {
            Some(trimmed.to_string())
        } else {
            None
        };

        if let Some(instruction) = task_instruction {
            let task = self.submit_task(Some(&clean_bot), None, &instruction, 5)?;
            let card = serde_json::json!({
                "kind": "task_status",
                "status": "queued",
                "task_id": task.id,
                "bot_id": clean_bot,
                "instruction": instruction,
            });
            let assistant_msg = self.post_direct_chat(
                &clean_bot,
                "assistant",
                &profile.display_name,
                &format!("Queued task `{}`: {}", task.id, instruction),
                Some(&user_msg.id),
                thread_id,
                Some(&card),
                Some(&task.id),
            )?;
            return Ok(DirectChatDispatchOutcome {
                user_message: user_msg,
                assistant_message: assistant_msg,
                queued_task: Some(task),
            });
        }

        let prompt_text = explicit_chat.unwrap_or(trimmed);
        let cfg = shared_cfg.unwrap_or(&self.config);
        let _ = router;
        let routed_router = LlmRouter::new(self.providers_config()).with_preference(
            profile.preferred_provider.as_deref(),
            profile.preferred_model.as_deref(),
        )?;
        let (primary, fallback) = routed_router.route(TaskType::Planning);

        let bot_paths = self.roster.runtime_paths_for_bot(&clean_bot);
        let sys_msgs = if cfg.muse.enable_muse_prompt {
            let identity_existing = bot_paths
                .identity_md()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .unwrap_or_default();
            let profile_trimmed = profile.system_prompt.trim();
            let persona_note = (!profile_trimmed.is_empty()
                && !identity_existing.contains(profile_trimmed))
            .then(|| {
                format!(
                    "## Bot Profile ({})\n{}",
                    profile.display_name, profile_trimmed
                )
            });
            let mut input = PromptInput::new(
                PromptSurface::MainChat,
                &bot_paths,
                &cfg.muse,
                &primary.model,
            );
            input.allow_host_desktop = self.allow_host_desktop();
            input.workspace_vm_enabled = cfg.workspace_vm.enabled;
            input.channel_instructions = persona_note.as_deref();
            MusePromptBuilder::build_messages(&input).unwrap_or_else(|_| {
                vec![gm_llm::ChatMessage::text("system", &profile.system_prompt)]
            })
        } else {
            vec![gm_llm::ChatMessage::text("system", &profile.system_prompt)]
        };

        let mut messages = sys_msgs;
        let prior = self
            .direct_chat
            .list_messages(&clean_bot, Some(user_msg.seq), 16)?;
        let mut hist_bytes = 0usize;
        for m in prior {
            if !matches!(m.role.as_str(), "user" | "assistant") || m.content.trim().is_empty() {
                continue;
            }
            if hist_bytes + m.content.len() > 48 * 1024 {
                break;
            }
            hist_bytes += m.content.len();
            messages.push(gm_llm::ChatMessage::text(&m.role, &m.content));
        }
        messages.push(gm_llm::ChatMessage::text("user", prompt_text));

        let _ = self
            .direct_chat
            .set_bot_status(&clean_bot, Some("Thinking..."), false);
        let _ = self.daemon_events_tx.send(DaemonEvent::BotStatusPhrase {
            bot_id: clean_bot.clone(),
            phrase: Some("Thinking...".into()),
            needs_attention: false,
        });
        self.emit_agui(
            "BOT_STATUS_PHRASE",
            &clean_bot,
            &clean_bot,
            serde_json::json!({"bot_id":clean_bot,"phrase":"Thinking...","needs_attention":false}),
        );

        let stream_msg_id = format!(
            "dmstream-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        );
        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<String>(64);
        let sup_stream = self.clone();
        let stream_bot = clean_bot.clone();
        let stream_id = stream_msg_id.clone();
        let forward_task = tokio::spawn(async move {
            while let Some(delta) = delta_rx.recv().await {
                let _ = sup_stream.daemon_events_tx.send(DaemonEvent::ChatTokenV1 {
                    message_id: stream_id.clone(),
                    delta: delta.clone(),
                    done: false,
                });
                sup_stream.emit_agui_ephemeral(
                    "TEXT_MESSAGE_CONTENT",
                    &stream_id,
                    &stream_bot,
                    serde_json::json!({ "delta": delta }),
                );
            }
        });

        let scoped = llm_client.scoped_job();
        let stream_res = if let Some(ks) = kill_switch {
            let mut kill_events = ks.subscribe();
            tokio::select! {
                biased;
                _ = kill_events.recv() => Err(GmError::SafetyViolation("DM chat stopped by emergency stop".into())),
                res = scoped.complete_stream_attributed(&primary, fallback.as_ref(), &messages, 1024, 0.2, delta_tx, &clean_bot, "dm") => res,
            }
        } else {
            scoped
                .complete_stream_attributed(
                    &primary,
                    fallback.as_ref(),
                    &messages,
                    1024,
                    0.2,
                    delta_tx,
                    &clean_bot,
                    "dm",
                )
                .await
        };

        let _ = forward_task.await;
        let _ = self.direct_chat.set_bot_status(&clean_bot, None, false);
        let _ = self.daemon_events_tx.send(DaemonEvent::BotStatusPhrase {
            bot_id: clean_bot.clone(),
            phrase: None,
            needs_attention: false,
        });
        self.emit_agui(
            "BOT_STATUS_PHRASE",
            &clean_bot,
            &clean_bot,
            serde_json::json!({"bot_id":clean_bot,"phrase":null,"needs_attention":false}),
        );

        let completion = stream_res?;
        let _ = self.daemon_events_tx.send(DaemonEvent::ChatTokenV1 {
            message_id: stream_msg_id,
            delta: String::new(),
            done: true,
        });

        let assistant_msg = self.post_direct_chat(
            &clean_bot,
            "assistant",
            &profile.display_name,
            &completion.content,
            Some(&user_msg.id),
            thread_id,
            None,
            None,
        )?;
        if let Some(b) = budget {
            let _ = self.daemon_events_tx.send(DaemonEvent::BudgetUpdated {
                daily_spent_usd: b.get_daily_spent(),
                daily_cap_usd: cfg.budget.daily_spend_cap_usd,
            });
        }

        Ok(DirectChatDispatchOutcome {
            user_message: user_msg,
            assistant_message: assistant_msg,
            queued_task: None,
        })
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

        let Some(task) = self.queue.claim_next_fifo(bot_id)? else {
            return Ok(None);
        };
        let _ = router;
        let mut router = LlmRouter::new(self.providers_config()).with_preference(
            profile.preferred_provider.as_deref(),
            profile.preferred_model.as_deref(),
        )?;
        if let Ok(Some((r_prov, r_model))) = self.routine_preference_for_task(&task.id) {
            router = router.with_routine_preference(r_prov.as_deref(), r_model.as_deref())?;
        }
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
        let allow_gui = profile.allow_gui && self.allow_host_desktop();
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
                enable_skills: self.config.muse.enable_skills,
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

    pub fn update_channel_metadata(
        &self,
        channel_id: &str,
        topic: Option<&str>,
        instructions: Option<&str>,
        paused: Option<bool>,
        archived: Option<bool>,
    ) -> Result<Option<ChannelInfo>> {
        let updated = self.channels.update_channel_metadata(
            channel_id,
            topic,
            instructions,
            paused,
            archived,
        )?;
        if let Some(ref info) = updated {
            let _ = self.daemon_events_tx.send(DaemonEvent::ActivityUpdated {
                reason: "channel_updated".into(),
            });
            self.emit_agui(
                "CHANNEL_UPDATED",
                &info.id,
                "",
                serde_json::to_value(info).unwrap_or_default(),
            );
        }
        Ok(updated)
    }

    pub fn delete_custom_skill(&self, name: &str) -> Result<bool> {
        let clean = name.trim();
        if !gm_muse::skills::is_valid_custom_skill_name(clean) {
            return Err(GmError::Config(format!(
                "Invalid custom skill name '{clean}': must match ^[a-z0-9][a-z0-9-]{{0,63}}$"
            )));
        }
        let reg = self.skill_registry()?;
        if reg
            .skills()
            .iter()
            .any(|s| s.bundled && (s.dir_name == clean || s.manifest.name == clean))
        {
            return Err(GmError::Config(format!(
                "Cannot delete bundled system skill '{clean}'"
            )));
        }
        let base_dir = self.shared_paths().custom_skills_dir();
        let dir = base_dir.join(clean);
        let meta = match std::fs::symlink_metadata(&dir) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(GmError::SafetyViolation(
                "RULE-PATH-002: Refusing symlink custom skill directory".into(),
            ));
        }
        std::fs::remove_dir_all(&dir)?;
        let _ = self.daemon_events_tx.send(DaemonEvent::ActivityUpdated {
            reason: "skill_deleted".into(),
        });
        self.emit_agui(
            "SKILL_DELETED",
            clean,
            "",
            serde_json::json!({ "name": clean }),
        );
        Ok(true)
    }

    fn custom_ideas_path(&self) -> std::path::PathBuf {
        self.shared_paths()
            .storage_dir()
            .join("workspace/custom_ideas.json")
    }

    pub fn load_custom_ideas(&self) -> Result<Vec<gm_goals::ProactiveIdea>> {
        let path = self.custom_ideas_path();
        let Some((raw, _)) = SharedFileStore::global().read_text_with_sha256(&path)? else {
            return Ok(Vec::new());
        };
        if raw.trim().is_empty() {
            return Ok(Vec::new());
        }
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn create_custom_idea(
        &self,
        title: &str,
        description: Option<&str>,
        action_prompt: Option<&str>,
    ) -> Result<gm_goals::ProactiveIdea> {
        let clean_title = title.trim();
        if clean_title.is_empty() {
            return Err(GmError::Config("Idea title cannot be empty".into()));
        }
        let idea = gm_goals::ProactiveIdea {
            id: format!("idea-{}", uuid::Uuid::new_v4()),
            title: clean_title.to_string(),
            description: description
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| "Custom proactive idea added by operator".into()),
            source: gm_goals::IdeaSource::Hybrid,
            action_prompt: action_prompt
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| Some(clean_title.to_string())),
            confidence: 0.90,
            created_at: chrono::Utc::now(),
        };
        let path = self.custom_ideas_path();
        let idea_for_save = idea.clone();
        SharedFileStore::global().update_sync(&path, None, move |existing| {
            let mut list: Vec<gm_goals::ProactiveIdea> = if existing.trim().is_empty() {
                Vec::new()
            } else {
                serde_json::from_str(existing).unwrap_or_default()
            };
            list.push(idea_for_save);
            Ok(serde_json::to_string_pretty(&list)?)
        })?;
        let _ = self.daemon_events_tx.send(DaemonEvent::ActivityUpdated {
            reason: "idea_created".into(),
        });
        self.emit_agui(
            "IDEA_CREATED",
            &idea.id,
            "",
            serde_json::to_value(&idea).unwrap_or_default(),
        );
        Ok(idea)
    }

    pub async fn collect_effective_ideas(&self) -> Result<Vec<gm_goals::ProactiveIdea>> {
        let default_bot = self.config.openbot.default_bot_id.clone();
        let memory = self.roster.memory_store(&default_bot)?;
        let mut ideas = gm_goals::generate_ideas_with_llm(
            Option::<&gm_platform::MockPlatform>::None,
            &memory,
            None,
            chrono::Utc::now(),
        )
        .await?;

        let mut seen_ids: std::collections::BTreeSet<String> =
            ideas.iter().map(|i| i.id.clone()).collect();
        for custom in self.load_custom_ideas()? {
            if seen_ids.insert(custom.id.clone()) {
                ideas.push(custom);
            }
        }

        if ideas.is_empty() {
            let now = chrono::Utc::now();
            ideas.push(gm_goals::ProactiveIdea {
                id: "idea-starter-goal-review".into(),
                title: "Review active goals & morning brief".into(),
                description:
                    "Check due milestones across your Goals workspace and summarize next steps."
                        .into(),
                source: gm_goals::IdeaSource::Hybrid,
                action_prompt: Some("Review my active goals and suggest the top next step.".into()),
                confidence: 0.72,
                created_at: now,
            });
            ideas.push(gm_goals::ProactiveIdea {
                id: "idea-starter-memory-sync".into(),
                title: "Import portable context into MEMORY.md".into(),
                description: "Bring over recurring preferences or background notes using the two-step safe importer.".into(),
                source: gm_goals::IdeaSource::Memory,
                action_prompt: Some("Help me organize key preferences into MEMORY.md.".into()),
                confidence: 0.68,
                created_at: now,
            });
        }

        let prefs = self.prefs_store().load()?;
        let dismissed: std::collections::BTreeSet<String> = prefs
            .one_time_dismissals
            .iter()
            .map(|s| s.trim().to_lowercase())
            .collect();
        let opted_out: Vec<String> = prefs
            .never_tell_me_about
            .iter()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();

        ideas.retain(|idea| {
            let lower_id = idea.id.to_lowercase();
            if dismissed.contains(&lower_id) || dismissed.contains(&format!("idea:{lower_id}")) {
                return false;
            }
            let haystack = format!("{} {}", idea.title, idea.description).to_lowercase();
            !opted_out.iter().any(|topic| haystack.contains(topic))
        });

        Ok(ideas)
    }

    pub async fn run_idea(
        &self,
        id: &str,
        bot_id: Option<&str>,
        channel_id: Option<&str>,
        instruction_override: Option<&str>,
    ) -> Result<QueuedTask> {
        let ideas = self.collect_effective_ideas().await?;
        let matched = ideas.into_iter().find(|i| i.id == id.trim());
        let override_instr = instruction_override
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let instruction = match (override_instr, matched) {
            (Some(instr), _) => instr.to_string(),
            (None, Some(idea)) => idea
                .action_prompt
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(idea.title),
            (None, None) => {
                return Err(GmError::Config(format!("Idea '{id}' not found")));
            }
        };
        let chan = channel_id.or(Some("general"));
        self.submit_task(bot_id, chan, &instruction, 5)
    }

    pub fn dismiss_idea(&self, id: &str) -> Result<bool> {
        let clean = id.trim();
        if clean.is_empty() {
            return Err(GmError::Config("Idea ID is required".into()));
        }
        // One-time dismissal MUST write to FEEDBACK.md without adding a permanent opt-out.
        self.prefs_store()
            .add_one_time_dismissal(&format!("idea:{clean}"))?;
        let _ = self.daemon_events_tx.send(DaemonEvent::ActivityUpdated {
            reason: "idea_dismissed".into(),
        });
        Ok(true)
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
        ForgetPlanner::plan(request, &res)
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
            format!("Updated DISCRETION.md for chat '{chat_id}'"),
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
            backend,
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
