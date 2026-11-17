use gm_core::config::{
    ProviderAuthMode, ProviderConnectionState, ProviderPreset, ProviderProfileConfig,
    ProvidersConfig, WireProtocol, extract_provider_origin,
};
use gm_core::error::{GmError, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskType {
    CuVisionStep,
    CuAxStep,
    Planning,
    ProactiveTriage,
    LiveWebSearch,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProviderKind {
    Meta,
    Xai,
    Claude,
    Codex,
    ChatGpt,
    KiloGateway,
    OpenRouter,
    NousPortal,
    Custom,
}

impl ProviderKind {
    pub fn from_preset(preset: ProviderPreset) -> Self {
        match preset {
            ProviderPreset::Meta => Self::Meta,
            ProviderPreset::Xai => Self::Xai,
            ProviderPreset::Claude => Self::Claude,
            ProviderPreset::Codex => Self::Codex,
            ProviderPreset::ChatGpt => Self::ChatGpt,
            ProviderPreset::KiloGateway => Self::KiloGateway,
            ProviderPreset::OpenRouter => Self::OpenRouter,
            ProviderPreset::NousPortal => Self::NousPortal,
            ProviderPreset::Custom => Self::Custom,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTarget {
    pub provider: ProviderKind,
    pub api_base: String,
    pub model: String,
    pub allow_vision: bool,
    #[serde(default)]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub protocol: WireProtocol,
    #[serde(default)]
    pub auth_mode: ProviderAuthMode,
    #[serde(default)]
    pub credential_ref: Option<String>,
    #[serde(default)]
    pub validated_origin: Option<String>,
}

impl ModelTarget {
    pub fn simple(
        provider: ProviderKind,
        api_base: impl Into<String>,
        model: impl Into<String>,
        allow_vision: bool,
    ) -> Self {
        let base = api_base.into();
        let validated_origin = extract_provider_origin(&base).ok();
        let protocol = match provider {
            ProviderKind::Claude | ProviderKind::Codex => WireProtocol::Cli,
            ProviderKind::ChatGpt => WireProtocol::Responses,
            _ => WireProtocol::ChatCompletions,
        };
        let auth_mode = match provider {
            ProviderKind::ChatGpt => ProviderAuthMode::ChatGptOAuth,
            ProviderKind::NousPortal => ProviderAuthMode::NousPortalOAuth,
            ProviderKind::Claude | ProviderKind::Codex => ProviderAuthMode::Cli,
            ProviderKind::Custom => ProviderAuthMode::None,
            _ => ProviderAuthMode::ApiKey,
        };
        Self {
            provider,
            api_base: base,
            model: model.into(),
            allow_vision,
            profile_id: None,
            protocol,
            auth_mode,
            credential_ref: None,
            validated_origin,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LlmRouter {
    config: ProvidersConfig,
    /// Tier 1: Explicit routine-level override (`provider_or_profile_id`, `optional_model`)
    routine_preference: Option<(String, Option<String>)>,
    /// Tier 2: Explicit bot-level override (`provider_or_profile_id`, `optional_model`)
    preference: Option<(String, Option<String>)>,
}

impl LlmRouter {
    pub fn new(config: ProvidersConfig) -> Self {
        Self {
            config,
            routine_preference: None,
            preference: None,
        }
    }

    pub fn config(&self) -> &ProvidersConfig {
        &self.config
    }

    fn validate_selector(&self, selector: &str) -> Result<String> {
        let trimmed = selector.trim();
        let lower = trimmed.to_ascii_lowercase();
        let builtin = matches!(
            lower.as_str(),
            "meta"
                | "muse"
                | "xai"
                | "grok"
                | "claude"
                | "anthropic"
                | "codex"
                | "openai"
                | "chatgpt"
                | "kilo"
                | "kilo_gateway"
                | "openrouter"
                | "nous"
                | "nous_portal"
                | "custom"
        );
        if builtin || self.config.profile_by_id(trimmed).is_some() {
            Ok(trimmed.to_string())
        } else {
            Err(GmError::Config(format!(
                "Unsupported bot provider or profile: {selector}"
            )))
        }
    }

    /// Tier 1 override: routine-level provider/model choice.
    pub fn with_routine_preference(
        mut self,
        provider: Option<&str>,
        model: Option<&str>,
    ) -> Result<Self> {
        self.routine_preference = match provider.map(str::trim).filter(|s| !s.is_empty()) {
            Some(sel) => Some((self.validate_selector(sel)?, model.map(str::to_string))),
            None if model.is_some() => {
                return Err(GmError::Config(
                    "A routine preferred model requires a preferred provider or profile".into(),
                ));
            }
            None => None,
        };
        Ok(self)
    }

    /// Tier 2 override: bot-level provider/model choice.
    pub fn with_preference(mut self, provider: Option<&str>, model: Option<&str>) -> Result<Self> {
        self.preference = match provider.map(str::trim).filter(|s| !s.is_empty()) {
            Some(sel) => Some((self.validate_selector(sel)?, model.map(str::to_string))),
            None if model.is_some() => {
                return Err(GmError::Config(
                    "A preferred model requires a preferred provider".into(),
                ));
            }
            None => None,
        };
        Ok(self)
    }

    fn profile_to_target(
        &self,
        profile: &ProviderProfileConfig,
        task: TaskType,
        model_override: Option<&str>,
    ) -> ModelTarget {
        let provider = ProviderKind::from_preset(profile.preset);
        let allow_vision = task == TaskType::CuVisionStep
            && match provider {
                ProviderKind::Meta => {
                    self.config.allow_screenshot_egress_meta || profile.allow_screenshot_egress
                }
                ProviderKind::Xai => {
                    self.config.allow_screenshot_egress_xai || profile.allow_screenshot_egress
                }
                _ => profile.allow_screenshot_egress,
            };
        let api_base = match provider {
            ProviderKind::Meta
                if !self.config.meta_api_base.is_empty() && profile.id == "meta-default" =>
            {
                self.config.meta_api_base.clone()
            }
            ProviderKind::Xai
                if !self.config.xai_api_base.is_empty() && profile.id == "xai-default" =>
            {
                self.config.xai_api_base.clone()
            }
            _ => profile.base_url.clone(),
        };
        let validated_origin =
            if profile.connection_state == ProviderConnectionState::NeedsRevalidation {
                None
            } else {
                profile.validated_origin.clone()
            };
        ModelTarget {
            provider,
            api_base,
            model: model_override
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| profile.selected_model.clone()),
            allow_vision,
            profile_id: Some(profile.id.clone()),
            protocol: profile.protocol,
            auth_mode: profile.auth_mode,
            credential_ref: profile.credential_ref.clone(),
            validated_origin,
        }
    }

    fn resolve_selector(
        &self,
        selector: &str,
        model_override: Option<&str>,
        task: TaskType,
    ) -> (ModelTarget, Option<ModelTarget>) {
        if let Some(profile) = self.config.profile_by_id(selector) {
            let primary = self.profile_to_target(profile, task, model_override);
            let fallback = profile
                .fallback_profile_id
                .as_deref()
                .and_then(|fid| self.config.profile_by_id(fid))
                .map(|fb| self.profile_to_target(fb, task, None));
            return (primary, fallback);
        }

        let lower = selector.to_ascii_lowercase();
        let mapped_profile_id = match lower.as_str() {
            "meta" | "muse" => Some("meta-default"),
            "xai" | "grok" => Some("xai-default"),
            "chatgpt" => Some("chatgpt-default"),
            "kilo" | "kilo_gateway" => Some("kilo-default"),
            "openrouter" => Some("openrouter-default"),
            "nous" | "nous_portal" => Some("nous-default"),
            "custom" => Some("custom-local"),
            _ => None,
        };

        if let Some(pid) = mapped_profile_id
            && let Some(profile) = self.config.profile_by_id(pid)
        {
            let mut primary = self.profile_to_target(profile, task, model_override);
            if model_override.is_none() {
                if pid == "meta-default" {
                    primary.model = self.config.meta_cu_model.clone();
                } else if pid == "xai-default" {
                    primary.model = self.config.xai_model.clone();
                }
            }
            // Explicit provider choice fails in place unless the user explicitly configured a fallback
            // on a non-legacy custom/preset profile (for legacy meta/xai aliases, keep explicit fail-in-place).
            let fallback = if matches!(pid, "meta-default" | "xai-default") {
                None
            } else {
                profile
                    .fallback_profile_id
                    .as_deref()
                    .and_then(|fid| self.config.profile_by_id(fid))
                    .map(|fb| self.profile_to_target(fb, task, None))
            };
            return (primary, fallback);
        }

        let mut primary = match lower.as_str() {
            "claude" | "anthropic" => ModelTarget {
                provider: ProviderKind::Claude,
                api_base: "cli://claude".into(),
                model: "claude-3-5-sonnet".into(),
                allow_vision: task == TaskType::CuVisionStep,
                profile_id: None,
                protocol: WireProtocol::Cli,
                auth_mode: ProviderAuthMode::Cli,
                credential_ref: None,
                validated_origin: Some("cli://claude".into()),
            },
            "codex" | "openai" => ModelTarget {
                provider: ProviderKind::Codex,
                api_base: "cli://codex".into(),
                model: "codex".into(),
                allow_vision: false,
                profile_id: None,
                protocol: WireProtocol::Cli,
                auth_mode: ProviderAuthMode::Cli,
                credential_ref: None,
                validated_origin: Some("cli://codex".into()),
            },
            "xai" | "grok" => ModelTarget::simple(
                ProviderKind::Xai,
                self.config.xai_api_base.clone(),
                self.config.xai_model.clone(),
                task == TaskType::CuVisionStep && self.config.allow_screenshot_egress_xai,
            ),
            _ => ModelTarget::simple(
                ProviderKind::Meta,
                self.config.meta_api_base.clone(),
                self.config.meta_cu_model.clone(),
                task == TaskType::CuVisionStep && self.config.allow_screenshot_egress_meta,
            ),
        };
        if let Some(m) = model_override {
            primary.model = m.to_string();
        }
        (primary, None)
    }

    /// Select primary target and optional fallback target using 5-tier precedence:
    /// 1. Routine override
    /// 2. Bot override
    /// 3. Workspace default profile
    /// 4. Account default profile
    /// 5. Legacy TaskType routing table
    ///
    /// Explicit choices (Tiers 1-4) fail in place (`fallback = None`) unless the
    /// selected profile configures an explicit `fallback_profile_id`.
    pub fn route(&self, task: TaskType) -> (ModelTarget, Option<ModelTarget>) {
        if let Some((selector, model)) = &self.routine_preference {
            return self.resolve_selector(selector, model.as_deref(), task);
        }
        if let Some((selector, model)) = &self.preference {
            return self.resolve_selector(selector, model.as_deref(), task);
        }
        if let Some(ws_pid) = self
            .config
            .workspace_default_profile_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            && self.config.profile_by_id(ws_pid).is_some()
        {
            return self.resolve_selector(ws_pid, None, task);
        }
        if let Some(acct_pid) = self
            .config
            .account_default_profile_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            && self.config.profile_by_id(acct_pid).is_some()
        {
            return self.resolve_selector(acct_pid, None, task);
        }

        // Tier 5: Legacy TaskType routing table
        match task {
            TaskType::CuVisionStep => (
                ModelTarget::simple(
                    ProviderKind::Meta,
                    self.config.meta_api_base.clone(),
                    self.config.meta_cu_model.clone(),
                    self.config.allow_screenshot_egress_meta,
                ),
                Some(ModelTarget::simple(
                    ProviderKind::Xai,
                    self.config.xai_api_base.clone(),
                    self.config.xai_model.clone(),
                    self.config.allow_screenshot_egress_xai,
                )),
            ),
            TaskType::CuAxStep => (
                ModelTarget::simple(
                    ProviderKind::Meta,
                    self.config.meta_api_base.clone(),
                    self.config.meta_cu_model.clone(),
                    false,
                ),
                Some(ModelTarget::simple(
                    ProviderKind::Xai,
                    self.config.xai_api_base.clone(),
                    self.config.xai_model.clone(),
                    false,
                )),
            ),
            TaskType::Planning => (
                ModelTarget::simple(
                    ProviderKind::Meta,
                    self.config.meta_api_base.clone(),
                    self.config.meta_reasoning_model.clone(),
                    false,
                ),
                Some(ModelTarget::simple(
                    ProviderKind::Xai,
                    self.config.xai_api_base.clone(),
                    self.config.xai_model.clone(),
                    false,
                )),
            ),
            TaskType::ProactiveTriage => (
                ModelTarget::simple(
                    ProviderKind::Meta,
                    self.config.meta_api_base.clone(),
                    self.config.meta_cu_model.clone(),
                    false,
                ),
                None,
            ),
            TaskType::LiveWebSearch => (
                ModelTarget::simple(
                    ProviderKind::Xai,
                    self.config.xai_api_base.clone(),
                    self.config.xai_model.clone(),
                    false,
                ),
                Some(ModelTarget::simple(
                    ProviderKind::Meta,
                    self.config.meta_api_base.clone(),
                    self.config.meta_reasoning_model.clone(),
                    false,
                )),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_router_rules() {
        let router = LlmRouter::new(ProvidersConfig::default());
        let (primary, fallback) = router.route(TaskType::CuVisionStep);
        assert_eq!(primary.provider, ProviderKind::Meta);
        assert_eq!(primary.model, "muse-spark");
        assert!(fallback.is_some());
        assert_eq!(fallback.unwrap().provider, ProviderKind::Xai);

        let (primary, fallback) = router.route(TaskType::LiveWebSearch);
        assert_eq!(primary.provider, ProviderKind::Xai);
        assert_eq!(primary.model, "grok-4");
        assert!(fallback.is_some());
    }

    #[test]
    fn test_five_tier_precedence_and_fail_in_place() {
        let mut cfg = ProvidersConfig {
            account_default_profile_id: Some("openrouter-default".into()),
            ..Default::default()
        };
        // Account tier wins over legacy
        let router = LlmRouter::new(cfg.clone());
        let (p, fb) = router.route(TaskType::Planning);
        assert_eq!(p.provider, ProviderKind::OpenRouter);
        assert!(
            fb.is_none(),
            "Explicit choice fails in place without fallback"
        );

        // Workspace wins over Account
        cfg.workspace_default_profile_id = Some("kilo-default".into());
        let router = LlmRouter::new(cfg.clone())
            .with_preference(None, None)
            .unwrap();
        let (p, _) = router.route(TaskType::Planning);
        assert_eq!(p.provider, ProviderKind::KiloGateway);

        // Bot wins over Workspace
        let router = LlmRouter::new(cfg.clone())
            .with_preference(Some("chatgpt-default"), Some("gpt-5-mini"))
            .unwrap();
        let (p, _) = router.route(TaskType::Planning);
        assert_eq!(p.provider, ProviderKind::ChatGpt);
        assert_eq!(p.model, "gpt-5-mini");
        assert_eq!(p.protocol, WireProtocol::Responses);

        // Routine wins over Bot
        let router = router
            .with_routine_preference(Some("nous-default"), Some("Hermes-3-Llama-3.1-405B"))
            .unwrap();
        let (p, _) = router.route(TaskType::Planning);
        assert_eq!(p.provider, ProviderKind::NousPortal);
        assert_eq!(p.model, "Hermes-3-Llama-3.1-405B");

        // Configured fallback on a profile is honored
        cfg.profile_by_id_mut("kilo-default")
            .unwrap()
            .fallback_profile_id = Some("openrouter-default".into());
        let router = LlmRouter::new(cfg);
        let (p, fb) = router.route(TaskType::Planning);
        assert_eq!(p.provider, ProviderKind::KiloGateway);
        assert_eq!(fb.unwrap().provider, ProviderKind::OpenRouter);
    }
}
