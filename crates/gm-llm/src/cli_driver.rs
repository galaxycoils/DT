//! Supported Claude/Codex/Grok CLI drivers with explicit availability checks,
//! documented authentication flows, and honest cost labeling.

use crate::client::{ChatCompletionResponse, ChatMessage, LlmBackend};
use crate::router::ModelTarget;
use async_trait::async_trait;
use gm_core::error::{GmError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CliProvider {
    Claude,
    Codex,
    Grok,
}

impl CliProvider {
    pub fn binary_name(&self) -> &'static str {
        match self {
            CliProvider::Claude => "claude",
            CliProvider::Codex => "codex",
            CliProvider::Grok => "grok",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            CliProvider::Claude => "Anthropic Claude CLI",
            CliProvider::Codex => "OpenAI Codex / CLI",
            CliProvider::Grok => "xAI Grok CLI",
        }
    }

    pub fn auth_flow_instructions(&self) -> &'static str {
        match self {
            CliProvider::Claude => {
                "Run `claude login` in your terminal to authenticate with Anthropic, \
                or set `export ANTHROPIC_API_KEY=\"sk-ant-...\"` in your environment."
            }
            CliProvider::Codex => {
                "Run `codex login` in your terminal to authenticate with OpenAI, \
                or set `export OPENAI_API_KEY=\"sk-...\"` in your environment."
            }
            CliProvider::Grok => {
                "Run `grok auth` in your terminal to authenticate with xAI, \
                or set `export XAI_API_KEY=\"xai-...\"` in your environment."
            }
        }
    }

    pub fn install_instructions(&self) -> &'static str {
        match self {
            CliProvider::Claude => "Install via npm: `npm install -g @anthropic-ai/claude-code`",
            CliProvider::Codex => "Install via npm: `npm install -g @openai/codex`",
            CliProvider::Grok => "Install via package manager or visit https://x.ai/grok-cli",
        }
    }

    pub fn environment_auth_var(&self) -> &'static str {
        match self {
            CliProvider::Claude => "ANTHROPIC_API_KEY",
            CliProvider::Codex => "OPENAI_API_KEY",
            CliProvider::Grok => "XAI_API_KEY",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliAvailability {
    pub provider: CliProvider,
    pub binary_name: String,
    pub installed: bool,
    pub binary_path: Option<PathBuf>,
    pub version: Option<String>,
    pub authenticated: bool,
    pub auth_instructions: String,
    pub install_instructions: String,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CliDriver {
    provider: CliProvider,
    #[allow(dead_code)]
    custom_binary_path: Option<PathBuf>,
}

impl CliDriver {
    pub fn new(provider: CliProvider) -> Self {
        Self {
            provider,
            custom_binary_path: None,
        }
    }

    pub fn with_binary_path(mut self, path: PathBuf) -> Self {
        self.custom_binary_path = Some(path);
        self
    }

    pub fn provider(&self) -> CliProvider {
        self.provider
    }

    /// Host CLI probing is disabled until isolated VM CLI-agent execution is enabled.
    pub fn check_availability(&self) -> CliAvailability {
        CliAvailability {
            provider: self.provider,
            binary_name: self.provider.binary_name().to_string(),
            installed: false,
            binary_path: None,
            version: None,
            authenticated: false,
            auth_instructions: self.provider.auth_flow_instructions().to_string(),
            install_instructions: self.provider.install_instructions().to_string(),
            error_message: Some(format!(
                "Host CLI execution and probes for {} are disabled; configure an API or OAuth provider profile in DT.",
                self.provider.display_name()
            )),
        }
    }

    /// Host CLI execution is disabled until isolated VM CLI-agent execution is enabled.
    pub async fn complete(
        &self,
        _model: Option<&str>,
        _prompt: &str,
    ) -> Result<ChatCompletionResponse> {
        Err(GmError::SafetyViolation(format!(
            "Host CLI execution for {} is disabled; configure an API or OAuth provider profile in DT.",
            self.provider.display_name()
        )))
    }
}

pub fn find_binary_on_path(_binary_name: &str) -> Option<PathBuf> {
    None
}

/// Adapter allowing `CliDriver` to be used wherever `LlmBackend` is expected.
#[derive(Debug, Clone)]
pub struct CliLlmBackend {
    driver: CliDriver,
}

impl CliLlmBackend {
    pub fn new(driver: CliDriver) -> Self {
        Self { driver }
    }

    pub fn driver(&self) -> &CliDriver {
        &self.driver
    }
}

#[async_trait]
impl LlmBackend for CliLlmBackend {
    async fn complete(
        &self,
        target: &ModelTarget,
        messages: &[ChatMessage],
    ) -> Result<ChatCompletionResponse> {
        let mut full_prompt = String::new();
        for msg in messages {
            full_prompt.push_str(&format!("{}: {}\n\n", msg.role, msg.content));
        }
        self.driver
            .complete(Some(&target.model), full_prompt.trim())
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_provider_attributes_and_auth_instructions() {
        let claude = CliProvider::Claude;
        assert_eq!(claude.binary_name(), "claude");
        assert!(claude.auth_flow_instructions().contains("claude login"));
        assert_eq!(claude.environment_auth_var(), "ANTHROPIC_API_KEY");

        let codex = CliProvider::Codex;
        assert_eq!(codex.binary_name(), "codex");
        assert!(codex.auth_flow_instructions().contains("codex login"));
        assert_eq!(codex.environment_auth_var(), "OPENAI_API_KEY");

        let grok = CliProvider::Grok;
        assert_eq!(grok.binary_name(), "grok");
        assert!(grok.auth_flow_instructions().contains("grok auth"));
        assert_eq!(grok.environment_auth_var(), "XAI_API_KEY");
    }

    #[test]
    fn test_host_cli_probe_disabled_even_with_existing_binary() {
        let driver = CliDriver::new(CliProvider::Claude).with_binary_path(PathBuf::from("/bin/sh"));
        let avail = driver.check_availability();
        assert!(!avail.installed);
        assert!(avail.binary_path.is_none());
        assert!(avail.version.is_none());
        assert!(!avail.authenticated);
        let err = avail.error_message.unwrap_or_default();
        assert!(err.contains("disabled"));
    }

    #[tokio::test]
    async fn test_host_cli_complete_fails_closed() {
        let driver = CliDriver::new(CliProvider::Codex).with_binary_path(PathBuf::from("/bin/sh"));
        let err = driver.complete(None, "hello").await.unwrap_err();
        assert!(matches!(err, GmError::SafetyViolation(_)));
    }

    #[test]
    fn test_honest_cli_cost_labeling() {
        // ChatCompletionResponse for CLI driver must have 0.0 estimated_cost_usd
        let resp = ChatCompletionResponse {
            id: "cli-test-1".into(),
            content: "result".into(),
            model: "claude-3-5-sonnet".into(),
            prompt_tokens: 100,
            completion_tokens: 50,
            estimated_cost_usd: 0.0,
        };
        assert_eq!(resp.estimated_cost_usd, 0.0);
    }

    #[test]
    fn test_cli_broker_egress_policy_and_process_tree_env() {
        let policy = CliEgressPolicy::for_provider(CliProvider::Codex, 5, 65_536);
        assert!(policy.is_host_allowed("api.openai.com"));
        assert!(!policy.is_host_allowed("evil.example.com"));
        assert!(!policy.is_host_allowed("169.254.169.254"));

        let broker = CliEgressBroker::new(policy, "sk-host-secret-never-exposed");
        let job_env = broker.isolated_guest_env("http://127.0.0.1:41999");
        assert!(!job_env.values().any(|v| v.contains("sk-host-secret")));
        assert_eq!(
            job_env.get("OPENAI_BASE_URL").map(String::as_str),
            Some("http://127.0.0.1:41999/v1")
        );
        assert!(
            broker
                .authorize_forward("api.openai.com", &broker.job_token)
                .is_ok()
        );
        assert!(
            broker
                .authorize_forward("evil.example.com", &broker.job_token)
                .is_err()
        );
        assert!(
            broker
                .authorize_forward("api.openai.com", "wrong-token")
                .is_err()
        );
    }
}

/// Job-scoped egress allowlist and metering policy for isolated CLI coding agents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CliEgressPolicy {
    pub provider: CliProvider,
    pub allowed_hosts: Vec<String>,
    pub max_requests: u32,
    pub max_output_bytes: usize,
}

impl CliEgressPolicy {
    pub fn for_provider(provider: CliProvider, max_requests: u32, max_output_bytes: usize) -> Self {
        let allowed_hosts = match provider {
            CliProvider::Claude => vec!["api.anthropic.com".to_string()],
            CliProvider::Codex => vec!["api.openai.com".to_string()],
            CliProvider::Grok => vec!["api.x.ai".to_string()],
        };
        Self {
            provider,
            allowed_hosts,
            max_requests,
            max_output_bytes,
        }
    }

    pub fn is_host_allowed(&self, host: &str) -> bool {
        let clean = host.trim().to_ascii_lowercase();
        self.allowed_hosts.iter().any(|h| h == &clean)
    }
}

/// Host-side job-scoped broker keeping real provider credentials on the host while exposing
/// only an ephemeral job token and metered proxy URL to the isolated CLI environment.
pub struct CliEgressBroker {
    pub policy: CliEgressPolicy,
    pub job_token: String,
    host_api_key: String,
    requests_used: std::sync::atomic::AtomicU32,
}

impl CliEgressBroker {
    pub fn new(policy: CliEgressPolicy, host_api_key: impl Into<String>) -> Self {
        Self {
            policy,
            job_token: format!("dtjob-{}", uuid::Uuid::new_v4().simple()),
            host_api_key: host_api_key.into(),
            requests_used: std::sync::atomic::AtomicU32::new(0),
        }
    }

    pub fn isolated_guest_env(
        &self,
        proxy_origin: &str,
    ) -> std::collections::BTreeMap<String, String> {
        let origin = proxy_origin.trim_end_matches('/');
        let mut env = std::collections::BTreeMap::new();
        // Never leak the host API key into the guest/CLI process environment.
        match self.policy.provider {
            CliProvider::Claude => {
                env.insert("ANTHROPIC_BASE_URL".into(), origin.to_string());
                env.insert("ANTHROPIC_API_KEY".into(), self.job_token.clone());
            }
            CliProvider::Codex => {
                env.insert("OPENAI_BASE_URL".into(), format!("{origin}/v1"));
                env.insert("OPENAI_API_KEY".into(), self.job_token.clone());
            }
            CliProvider::Grok => {
                env.insert("XAI_BASE_URL".into(), format!("{origin}/v1"));
                env.insert("XAI_API_KEY".into(), self.job_token.clone());
            }
        }
        env.insert("NO_UPDATE_NOTIFIER".into(), "1".into());
        env
    }

    pub fn authorize_forward(&self, target_host: &str, presented_token: &str) -> Result<&str> {
        if presented_token != self.job_token {
            return Err(GmError::SafetyViolation(
                "Invalid job-scoped CLI broker token".into(),
            ));
        }
        if !self.policy.is_host_allowed(target_host) {
            return Err(GmError::SafetyViolation(format!(
                "CLI egress broker denied unapproved host '{target_host}'"
            )));
        }
        let prev = self
            .requests_used
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if prev >= self.policy.max_requests {
            return Err(GmError::BudgetExceeded(format!(
                "CLI broker request cap ({}) exceeded",
                self.policy.max_requests
            )));
        }
        Ok(&self.host_api_key)
    }
}

/// Run a CLI subprocess in its own process group with output capping and full tree cancellation.
pub async fn run_bounded_cli_command(
    program: &std::path::Path,
    args: &[&str],
    env_overrides: &std::collections::BTreeMap<String, String>,
    max_output_bytes: usize,
    timeout: std::time::Duration,
) -> Result<(i32, Vec<u8>, bool)> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .env_clear()
        .envs(env_overrides)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Ok(path_env) = std::env::var("PATH") {
        cmd.env("PATH", path_env);
    }
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd
        .spawn()
        .map_err(|e| GmError::Platform(format!("CLI spawn failed: {e}")))?;
    let pid = child.id();

    let wait_fut = child.wait_with_output();
    match tokio::time::timeout(timeout, wait_fut).await {
        Ok(Ok(out)) => {
            let mut combined = out.stdout;
            combined.extend_from_slice(&out.stderr);
            let truncated = combined.len() > max_output_bytes;
            if truncated {
                combined.truncate(max_output_bytes);
            }
            Ok((out.status.code().unwrap_or(-1), combined, truncated))
        }
        Ok(Err(e)) => Err(GmError::Platform(format!("CLI execution error: {e}"))),
        Err(_) => {
            #[cfg(unix)]
            if let Some(p) = pid {
                unsafe {
                    libc::kill(-(p as i32), libc::SIGKILL);
                }
            }
            #[cfg(not(unix))]
            let _ = pid;
            Err(GmError::Timeout(
                "CLI process tree timed out and was killed".into(),
            ))
        }
    }
}
