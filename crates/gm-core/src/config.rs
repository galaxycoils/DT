use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub fn default_daemon_socket_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        PathBuf::from(r"\\.\pipe\grok-muse-daemon")
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home)
            .join("Library/Application Support/GrokMuse")
            .join("daemon.sock")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let base = std::env::var("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let data = std::env::var("XDG_DATA_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| {
                        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
                            .join(".local/share")
                    });
                data.join("grok-muse")
            });
        base.join("grok-muse-daemon.sock")
    }
}

pub fn default_config_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join("Library/Application Support/GrokMuse/config.json")
    }
    #[cfg(target_os = "windows")]
    {
        let app_data = std::env::var("APPDATA")
            .or_else(|_| std::env::var("LOCALAPPDATA"))
            .unwrap_or_else(|_| ".".into());
        PathBuf::from(app_data).join("GrokMuse/config.json")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".config/grok-muse/config.json")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GmConfig {
    pub socket_path: PathBuf,
    #[serde(default = "default_daemon_socket_path")]
    pub daemon_socket_path: PathBuf,
    pub storage_dir: PathBuf,
    pub providers: ProvidersConfig,
    pub safety: SafetyConfig,
    pub budget: BudgetConfig,
    pub openbot: OpenBotConfig,
    pub workspace_vm: WorkspaceVmConfig,
    #[serde(default)]
    pub muse: MuseConfig,
    #[serde(default)]
    pub ui: UiConfig,
}

impl GmConfig {
    /// Missing default configuration uses defaults. Explicit paths and malformed files fail closed.
    pub fn load(custom_path: Option<&Path>) -> crate::error::Result<Self> {
        let path = custom_path
            .map(Path::to_path_buf)
            .unwrap_or_else(default_config_path);
        let mut config: Self = match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).map_err(|e| {
                crate::error::GmError::Config(format!(
                    "Invalid configuration {}: {e}",
                    path.display()
                ))
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && custom_path.is_none() => {
                Self::default()
            }
            Err(e) => {
                return Err(crate::error::GmError::Config(format!(
                    "Cannot read configuration {}: {e}",
                    path.display()
                )));
            }
        };

        if let Ok(socket) = std::env::var("GM_SOCKET_PATH")
            && !socket.trim().is_empty()
        {
            config.socket_path = PathBuf::from(socket);
        }
        if let Ok(daemon_socket) = std::env::var("GM_DAEMON_SOCKET_PATH")
            && !daemon_socket.trim().is_empty()
        {
            config.daemon_socket_path = PathBuf::from(daemon_socket);
        }

        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> crate::error::Result<()> {
        if !self.budget.daily_spend_cap_usd.is_finite()
            || self.budget.daily_spend_cap_usd < 0.0
            || !self.budget.per_job_spend_cap_usd.is_finite()
            || self.budget.per_job_spend_cap_usd < 0.0
        {
            return Err(crate::error::GmError::Config(
                "Budget limits must be finite and nonnegative".into(),
            ));
        }
        if !self.safety.user_active_pause_seconds.is_finite()
            || self.safety.user_active_pause_seconds < 0.0
        {
            return Err(crate::error::GmError::Config(
                "Idle threshold must be finite and nonnegative".into(),
            ));
        }
        if self.muse.nightly_dream_hour > 23 {
            return Err(crate::error::GmError::Config(
                "muse.nightly_dream_hour must be between 0 and 23".into(),
            ));
        }
        if self.muse.heartbeat_interval_secs < 60 {
            return Err(crate::error::GmError::Config(
                "muse.heartbeat_interval_secs must be at least 60".into(),
            ));
        }
        if self.muse.subagent_fanout_depth > 2 {
            return Err(crate::error::GmError::Config(
                "muse.subagent_fanout_depth must be at most 2".into(),
            ));
        }
        if self.muse.context_tokens < 8_000 {
            return Err(crate::error::GmError::Config(
                "muse.context_tokens must be at least 8000".into(),
            ));
        }
        if !matches!(self.ui.theme.as_str(), "system" | "dark" | "light") {
            return Err(crate::error::GmError::Config(
                "ui.theme must be one of: system, dark, light".into(),
            ));
        }
        if !matches!(self.ui.accent_mode.as_str(), "bot_color" | "black") {
            return Err(crate::error::GmError::Config(
                "ui.accent_mode must be one of: bot_color, black".into(),
            ));
        }
        if !matches!(
            self.ui.computer_execution_mode.as_str(),
            "ask" | "always" | "never"
        ) {
            return Err(crate::error::GmError::Config(
                "ui.computer_execution_mode must be one of: ask, always, never".into(),
            ));
        }
        Ok(())
    }
    pub fn save(&self, path: &Path) -> crate::error::Result<()> {
        self.validate()?;
        crate::storage::atomic_write_private(path, &serde_json::to_vec_pretty(self)?)
    }

    /// Atomically write configuration using write-to-temp and rename semantics.
    pub fn save_atomic(&self, path: impl AsRef<Path>) -> crate::error::Result<()> {
        self.save(path.as_ref())
    }

    pub fn load_or_default(custom_path: Option<&Path>) -> Self {
        Self::load(custom_path)
            .expect("Configuration could not be loaded; the original file has been preserved")
    }
}

impl Default for GmConfig {
    fn default() -> Self {
        #[cfg(target_os = "windows")]
        let (socket_path, storage_dir) = {
            let app_data = std::env::var("APPDATA")
                .or_else(|_| std::env::var("LOCALAPPDATA"))
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| ".".into());
            let storage = PathBuf::from(app_data).join("GrokMuse");
            let socket = PathBuf::from(r"\\.\pipe\grok-muse-cu-helper");
            (socket, storage)
        };

        #[cfg(target_os = "linux")]
        let (socket_path, storage_dir) = {
            let data_home = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                format!("{home}/.local/share")
            });
            let storage = PathBuf::from(data_home).join("grok-muse");
            let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| storage.clone());
            let socket = runtime_dir.join("grok-muse-cu-helper.sock");
            (socket, storage)
        };

        #[cfg(target_os = "macos")]
        let (socket_path, storage_dir) = {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            let app_support = PathBuf::from(home).join("Library/Application Support/GrokMuse");
            let socket = app_support.join("cu-helper.sock");
            (socket, app_support)
        };

        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        let (socket_path, storage_dir) = {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            let base = PathBuf::from(home).join(".grok-muse");
            (base.join("cu-helper.sock"), base)
        };

        let openbot = OpenBotConfig {
            bots_dir: storage_dir.join("bots"),
            ..OpenBotConfig::default()
        };

        Self {
            socket_path,
            daemon_socket_path: default_daemon_socket_path(),
            storage_dir,
            providers: ProvidersConfig::default(),
            safety: SafetyConfig::default(),
            budget: BudgetConfig::default(),
            openbot,
            workspace_vm: WorkspaceVmConfig::default(),
            muse: MuseConfig::default(),
            ui: UiConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProvidersConfig {
    pub meta_api_base: String,
    pub meta_cu_model: String,
    pub meta_reasoning_model: String,
    pub xai_api_base: String,
    pub xai_model: String,
    pub allow_screenshot_egress_meta: bool,
    pub allow_screenshot_egress_xai: bool,
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        Self {
            meta_api_base: "https://api.meta.ai/v1".into(),
            meta_cu_model: "muse-spark".into(),
            meta_reasoning_model: "muse-wonder".into(),
            xai_api_base: "https://api.x.ai/v1".into(),
            xai_model: "grok-4".into(),
            allow_screenshot_egress_meta: false,
            allow_screenshot_egress_xai: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SafetyConfig {
    /// Explicit opt-in for agents to observe or control the host desktop.
    pub allow_host_desktop: bool,
    /// Experimental credential injection requires separate opt-in and per-action approval.
    pub allow_secret_injection: bool,
    pub blocklisted_apps: Vec<String>,
    pub redact_secure_fields: bool,
    pub kill_switch_shortcut: String,
    pub user_active_pause_seconds: f64,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            allow_host_desktop: false,
            allow_secret_injection: false,
            blocklisted_apps: vec![
                "1Password".into(),
                "Keychain Access".into(),
                "Bitwarden".into(),
                "KeePassXC".into(),
                "Dashlane".into(),
                "LastPass".into(),
                "Authenticator".into(),
                "Passwords".into(),
                "com.apple.keychainaccess".into(),
                "com.apple.Passwords".into(),
            ],
            redact_secure_fields: true,
            kill_switch_shortcut: "Cmd+Alt+Escape".into(),
            user_active_pause_seconds: 1.5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BudgetConfig {
    pub daily_spend_cap_usd: f64,
    pub per_job_spend_cap_usd: f64,
    pub max_daily_notifications: u32,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            daily_spend_cap_usd: 5.0,
            per_job_spend_cap_usd: 0.50,
            max_daily_notifications: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenBotConfig {
    pub enabled: bool,
    pub bots_dir: PathBuf,
    pub default_bot_id: String,
    pub companion_enabled: bool,
    pub companion_bind_addr: String,
    pub max_context_tokens: usize,
    pub compaction_trigger_ratio: f32,
    pub max_handoff_depth: u8,
    pub bridges: ChatBridgesConfig,
}

impl Default for OpenBotConfig {
    fn default() -> Self {
        #[cfg(target_os = "macos")]
        let bots_dir = {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join("Library/Application Support/GrokMuse/bots")
        };
        #[cfg(not(target_os = "macos"))]
        let bots_dir = {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".grok-muse/bots")
        };

        Self {
            enabled: true,
            bots_dir,
            default_bot_id: "muse".into(),
            companion_enabled: true,
            companion_bind_addr: "127.0.0.1:7433".into(),
            max_context_tokens: 24_000,
            compaction_trigger_ratio: 0.80,
            max_handoff_depth: 4,
            bridges: ChatBridgesConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatBridgesConfig {
    pub telegram_enabled: bool,
    pub telegram_allowed_chat_ids: Vec<String>,
    pub slack_enabled: bool,
    pub slack_allowed_channel_ids: Vec<String>,
    pub discord_enabled: bool,
    pub discord_allowed_channel_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_default_config() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let config = GmConfig::default();
        assert!(!config.socket_path.as_os_str().is_empty());
        assert!(!config.daemon_socket_path.as_os_str().is_empty());
        assert!(
            config
                .safety
                .blocklisted_apps
                .contains(&"Passwords".to_string())
        );
        assert!(
            config
                .safety
                .blocklisted_apps
                .contains(&"com.apple.Passwords".to_string())
        );
        assert_eq!(config.safety.blocklisted_apps.len(), 10);
    }

    #[test]
    fn test_load_or_default_custom_file() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let path = std::env::temp_dir().join(format!("test_config_{}.json", uuid::Uuid::new_v4()));
        let json = r#"{
            "socket_path": "/tmp/custom-helper.sock",
            "daemon_socket_path": "/tmp/custom-daemon.sock",
            "storage_dir": "/tmp/custom-storage",
            "providers": {
                "meta_api_base": "https://api.meta.ai/v1",
                "meta_cu_model": "muse-spark",
                "meta_reasoning_model": "muse-wonder",
                "xai_api_base": "https://api.x.ai/v1",
                "xai_model": "grok-4",
                "allow_screenshot_egress_meta": false,
                "allow_screenshot_egress_xai": false
            },
            "safety": {
                "blocklisted_apps": ["CustomApp"],
                "redact_secure_fields": false,
                "kill_switch_shortcut": "Escape",
                "user_active_pause_seconds": 2.0
            },
            "budget": {
                "daily_spend_cap_usd": 10.0,
                "per_job_spend_cap_usd": 1.0,
                "max_daily_notifications": 5
            }
        }"#;
        std::fs::write(&path, json).unwrap();

        let loaded = GmConfig::load_or_default(Some(&path));
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded.socket_path, PathBuf::from("/tmp/custom-helper.sock"));
        assert_eq!(
            loaded.daemon_socket_path,
            PathBuf::from("/tmp/custom-daemon.sock")
        );
        assert_eq!(loaded.safety.blocklisted_apps, vec!["CustomApp"]);
    }

    #[test]
    fn test_load_or_default_partial_file() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let path =
            std::env::temp_dir().join(format!("test_partial_config_{}.json", uuid::Uuid::new_v4()));
        let json = r#"{
            "daemon_socket_path": "/tmp/custom-partial-daemon.sock",
            "budget": {
                "daily_spend_cap_usd": 25.0
            }
        }"#;
        std::fs::write(&path, json).unwrap();

        let loaded = GmConfig::load_or_default(Some(&path));
        let _ = std::fs::remove_file(&path);

        assert_eq!(
            loaded.daemon_socket_path,
            PathBuf::from("/tmp/custom-partial-daemon.sock")
        );
        assert_eq!(loaded.budget.daily_spend_cap_usd, 25.0);
        // Defaults preserved
        assert_eq!(loaded.budget.per_job_spend_cap_usd, 0.50);
        assert_eq!(loaded.safety.blocklisted_apps.len(), 10);
    }

    #[test]
    fn test_env_var_overrides() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("GM_SOCKET_PATH", "/tmp/env-helper.sock");
            std::env::set_var("GM_DAEMON_SOCKET_PATH", "/tmp/env-daemon.sock");
        }

        let config = GmConfig::load_or_default(None);
        assert_eq!(config.socket_path, PathBuf::from("/tmp/env-helper.sock"));
        assert_eq!(
            config.daemon_socket_path,
            PathBuf::from("/tmp/env-daemon.sock")
        );

        unsafe {
            std::env::remove_var("GM_SOCKET_PATH");
            std::env::remove_var("GM_DAEMON_SOCKET_PATH");
        }
    }

    #[test]
    fn test_openbot_config_defaults_and_overrides() {
        let cfg = GmConfig::default();
        assert!(cfg.openbot.enabled);
        assert_eq!(cfg.openbot.default_bot_id, "muse");
        assert_eq!(cfg.openbot.companion_bind_addr, "127.0.0.1:7433");
        assert_eq!(cfg.openbot.max_context_tokens, 24_000);
        assert!((cfg.openbot.compaction_trigger_ratio - 0.80).abs() < 1e-5);
        assert_eq!(cfg.openbot.max_handoff_depth, 4);
        assert!(!cfg.openbot.bridges.telegram_enabled);
        assert!(!cfg.openbot.bridges.slack_enabled);
        assert!(!cfg.openbot.bridges.discord_enabled);
    }

    #[test]
    fn test_muse_config_roundtrip_and_backwards_compat() {
        let cfg = GmConfig::default();
        assert!(cfg.muse.enable_muse_prompt);
        assert!(cfg.muse.enable_skills);
        assert!(!cfg.muse.enable_heartbeat);
        assert!(!cfg.muse.enable_dreams);
        assert!(cfg.muse.enable_spaces);
        assert!(cfg.muse.enable_bridge_discretion);
        assert!(cfg.muse.forget_redact_audit);
        assert_eq!(cfg.muse.heartbeat_interval_secs, 300);
        assert_eq!(cfg.muse.heartbeat_daily_token_cap, 200_000);
        assert_eq!(cfg.muse.nightly_dream_hour, 3);
        assert_eq!(cfg.muse.default_timezone, "UTC");
        assert_eq!(cfg.muse.context_tokens, 128_000);
        assert_eq!(cfg.muse.subagent_fanout_depth, 1);
        assert_eq!(cfg.muse.spaces_port, None);
        assert!(cfg.muse.artifact_require_publish_confirmation);
        assert!(cfg.validate().is_ok());

        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: GmConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.muse.context_tokens, 128_000);
        assert_eq!(
            parsed.muse.forget_redact_audit,
            cfg.muse.forget_redact_audit
        );

        // Old config JSON missing the `muse` key must parse seamlessly with defaults.
        let legacy_json = r#"{"storage_dir": "/tmp/legacy"}"#;
        let legacy: GmConfig = serde_json::from_str(legacy_json).unwrap();
        assert!(legacy.muse.enable_muse_prompt);
        assert_eq!(legacy.muse.heartbeat_interval_secs, 300);
        assert!(legacy.validate().is_ok());

        let mut bad_hour = GmConfig::default();
        bad_hour.muse.nightly_dream_hour = 24;
        assert!(bad_hour.validate().is_err());

        let mut bad_hb = GmConfig::default();
        bad_hb.muse.heartbeat_interval_secs = 59;
        assert!(bad_hb.validate().is_err());

        let mut bad_fanout = GmConfig::default();
        bad_fanout.muse.subagent_fanout_depth = 3;
        assert!(bad_fanout.validate().is_err());

        let mut bad_ctx = GmConfig::default();
        bad_ctx.muse.context_tokens = 7_999;
        assert!(bad_ctx.validate().is_err());
    }

    #[test]
    fn test_ui_config_roundtrip_and_validation() {
        let cfg = GmConfig::default();
        assert_eq!(cfg.ui.theme, "system");
        assert_eq!(cfg.ui.accent_mode, "bot_color");
        assert!(!cfg.ui.hardware_acceleration);
        assert!(!cfg.ui.sidebar_collapsed);
        assert_eq!(cfg.ui.language, "en-US");
        assert_eq!(cfg.ui.timezone, "UTC");
        assert!(cfg.ui.spellcheck);
        assert!(cfg.ui.notifications_enabled);
        assert!(cfg.ui.notification_sound);
        assert!(cfg.ui.suppress_notifications_when_active);
        assert!(!cfg.ui.auto_review_approvals);
        assert_eq!(cfg.ui.computer_execution_mode, "ask");
        assert!(!cfg.ui.route_traffic_through_computer);
        assert!(!cfg.ui.hardware_security_keys);
        assert!(cfg.ui.auto_updates);
        assert_eq!(cfg.ui.primary_bot_id, "muse");
        assert!(cfg.validate().is_ok());

        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: GmConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.ui.theme, "system");
        assert_eq!(parsed.ui.accent_mode, "bot_color");
        assert_eq!(parsed.ui.computer_execution_mode, "ask");

        // Legacy JSON missing `ui` must load cleanly with defaults.
        let legacy: GmConfig = serde_json::from_str(r#"{"storage_dir": "/tmp/legacy-ui"}"#).unwrap();
        assert_eq!(legacy.ui.theme, "system");
        assert!(legacy.validate().is_ok());

        let mut bad_theme = GmConfig::default();
        bad_theme.ui.theme = "neon".into();
        assert!(bad_theme.validate().is_err());

        let mut bad_accent = GmConfig::default();
        bad_accent.ui.accent_mode = "rainbow".into();
        assert!(bad_accent.validate().is_err());

        let mut bad_exec = GmConfig::default();
        bad_exec.ui.computer_execution_mode = "yolo".into();
        assert!(bad_exec.validate().is_err());
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub theme: String,
    pub accent_mode: String,
    pub hardware_acceleration: bool,
    pub sidebar_collapsed: bool,
    pub language: String,
    pub timezone: String,
    pub spellcheck: bool,
    pub notifications_enabled: bool,
    pub notification_sound: bool,
    pub suppress_notifications_when_active: bool,
    pub quiet_hours_start: Option<String>,
    pub quiet_hours_end: Option<String>,
    pub auto_review_approvals: bool,
    pub computer_execution_mode: String,
    pub route_traffic_through_computer: bool,
    pub hardware_security_keys: bool,
    pub auto_updates: bool,
    pub microphone_device: Option<String>,
    pub primary_bot_id: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            accent_mode: "bot_color".into(),
            hardware_acceleration: false,
            sidebar_collapsed: false,
            language: "en-US".into(),
            timezone: "UTC".into(),
            spellcheck: true,
            notifications_enabled: true,
            notification_sound: true,
            suppress_notifications_when_active: true,
            quiet_hours_start: None,
            quiet_hours_end: None,
            auto_review_approvals: false,
            computer_execution_mode: "ask".into(),
            route_traffic_through_computer: false,
            hardware_security_keys: false,
            auto_updates: true,
            microphone_device: None,
            primary_bot_id: "muse".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MuseConfig {
    pub enable_muse_prompt: bool,
    pub enable_skills: bool,
    pub enable_heartbeat: bool,
    pub enable_dreams: bool,
    pub enable_spaces: bool,
    pub enable_bridge_discretion: bool,
    pub forget_redact_audit: bool,
    pub heartbeat_interval_secs: u64,
    pub heartbeat_daily_token_cap: u64,
    pub nightly_dream_hour: u32,
    pub default_timezone: String,
    pub context_tokens: usize,
    pub subagent_fanout_depth: u8,
    pub spaces_port: Option<u16>,
    pub artifact_require_publish_confirmation: bool,
}

impl Default for MuseConfig {
    fn default() -> Self {
        Self {
            enable_muse_prompt: true,
            enable_skills: true,
            enable_heartbeat: false,
            enable_dreams: false,
            enable_spaces: true,
            enable_bridge_discretion: true,
            forget_redact_audit: true,
            heartbeat_interval_secs: 300,
            heartbeat_daily_token_cap: 200_000,
            nightly_dream_hour: 3,
            default_timezone: "UTC".into(),
            context_tokens: 128_000,
            subagent_fanout_depth: 1,
            spaces_port: None,
            artifact_require_publish_confirmation: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceVmConfig {
    pub enabled: bool,
    pub qemu_path: PathBuf,
    pub image_path: PathBuf,
    pub manifest_path: PathBuf,
    pub signature_path: PathBuf,
    pub trusted_public_key_hex: String,
    /// Read-only UEFI firmware, authenticated by the image manifest. Required on ARM.
    pub firmware_path: Option<PathBuf>,
}
impl Default for WorkspaceVmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            qemu_path: if cfg!(target_arch = "aarch64") {
                "qemu-system-aarch64"
            } else {
                "qemu-system-x86_64"
            }
            .into(),
            image_path: PathBuf::new(),
            manifest_path: PathBuf::new(),
            signature_path: PathBuf::new(),
            trusted_public_key_hex: String::new(),
            firmware_path: None,
        }
    }
}
