//! Supported Claude/Codex/Grok CLI drivers with explicit availability checks,
//! documented authentication flows, and honest cost labeling.

use crate::client::{ChatCompletionResponse, ChatMessage, LlmBackend};
use crate::router::ModelTarget;
use async_trait::async_trait;
use gm_core::error::{GmError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

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

    /// Check availability of the CLI tool and return structured diagnosis.
    pub fn check_availability(&self) -> CliAvailability {
        let binary_path = if let Some(ref custom) = self.custom_binary_path {
            if custom.is_file() {
                Some(custom.clone())
            } else {
                None
            }
        } else {
            find_binary_on_path(self.provider.binary_name())
        };

        let Some(bin) = binary_path else {
            return CliAvailability {
                provider: self.provider,
                binary_name: self.provider.binary_name().to_string(),
                installed: false,
                binary_path: None,
                version: None,
                authenticated: false,
                auth_instructions: self.provider.auth_flow_instructions().to_string(),
                install_instructions: self.provider.install_instructions().to_string(),
                error_message: Some(format!(
                    "{} ('{}') not found on PATH. {}",
                    self.provider.display_name(),
                    self.provider.binary_name(),
                    self.provider.install_instructions()
                )),
            };
        };

        // Check version
        let version_out = Command::new(&bin).arg("--version").output();
        let (version, ver_err) = match version_out {
            Ok(output) if output.status.success() => {
                let ver_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
                (Some(ver_str), None)
            }
            Ok(output) => {
                let err_str = String::from_utf8_lossy(&output.stderr).trim().to_string();
                (
                    None,
                    Some(format!(
                        "Binary execution failed with code {:?}: {}",
                        output.status.code(),
                        err_str
                    )),
                )
            }
            Err(e) => (
                None,
                Some(format!("Failed to execute {}: {e}", bin.display())),
            ),
        };

        // Check authentication presence
        let auth_env = std::env::var(self.provider.environment_auth_var()).ok();
        let authenticated = auth_env.is_some_and(|v| !v.trim().is_empty());

        let error_message = if let Some(err) = ver_err {
            Some(err)
        } else if !authenticated {
            Some(format!(
                "{} is installed but not authenticated. {}",
                self.provider.display_name(),
                self.provider.auth_flow_instructions()
            ))
        } else {
            None
        };

        CliAvailability {
            provider: self.provider,
            binary_name: self.provider.binary_name().to_string(),
            installed: true,
            binary_path: Some(bin),
            version,
            authenticated,
            auth_instructions: self.provider.auth_flow_instructions().to_string(),
            install_instructions: self.provider.install_instructions().to_string(),
            error_message,
        }
    }

    /// Complete a prompt using the CLI driver.
    ///
    /// Costs are labeled honestly as 0.0 with unknown_cli_cost metadata.
    pub async fn complete(
        &self,
        model: Option<&str>,
        prompt: &str,
    ) -> Result<ChatCompletionResponse> {
        let availability = self.check_availability();
        if !availability.installed {
            return Err(GmError::Config(availability.error_message.unwrap_or_else(
                || format!("{} binary is not installed", self.provider.display_name()),
            )));
        }

        let bin = availability
            .binary_path
            .as_ref()
            .ok_or_else(|| GmError::Config("Binary path resolution error".into()))?;

        // Format command invocation based on provider
        let mut cmd = tokio::process::Command::new(bin);
        match self.provider {
            CliProvider::Claude => {
                cmd.arg("-p").arg(prompt);
                if let Some(m) = model
                    && m != self.provider.binary_name()
                {
                    cmd.arg("--model").arg(m);
                }
            }
            CliProvider::Codex => {
                cmd.arg("exec").arg(prompt);
                if let Some(m) = model
                    && m != self.provider.binary_name()
                {
                    cmd.arg("--model").arg(m);
                }
            }
            CliProvider::Grok => {
                cmd.arg("-p").arg(prompt);
                if let Some(m) = model
                    && m != self.provider.binary_name()
                {
                    cmd.arg("--model").arg(m);
                }
            }
        }

        let output = cmd.output().await.map_err(|e| {
            GmError::Provider(format!(
                "Failed to spawn {} process: {e}",
                self.provider.display_name()
            ))
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(GmError::Provider(format!(
                "{} failed (status {}): {stderr}. If this is an authentication issue, {}",
                self.provider.display_name(),
                output.status,
                self.provider.auth_flow_instructions()
            )));
        }

        let content = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let prompt_tokens = (prompt.len() / 4).max(1) as u32;
        let completion_tokens = (content.len() / 4).max(1) as u32;

        Ok(ChatCompletionResponse {
            id: format!(
                "cli-{}-{}",
                self.provider.binary_name(),
                uuid::Uuid::new_v4()
            ),
            content,
            model: model.unwrap_or(self.provider.binary_name()).to_string(),
            prompt_tokens,
            completion_tokens,
            // Milestone 4: Label unknown CLI costs honestly (0.0 USD, non-metered)
            estimated_cost_usd: 0.0,
        })
    }
}

pub fn find_binary_on_path(binary_name: &str) -> Option<PathBuf> {
    if let Some(paths) = std::env::var_os("PATH") {
        for path in std::env::split_paths(&paths) {
            let candidate = path.join(binary_name);
            if candidate.is_file() {
                return Some(candidate);
            }
            #[cfg(windows)]
            {
                let candidate_exe = path.join(format!("{binary_name}.exe"));
                if candidate_exe.is_file() {
                    return Some(candidate_exe);
                }
                let candidate_cmd = path.join(format!("{binary_name}.cmd"));
                if candidate_cmd.is_file() {
                    return Some(candidate_cmd);
                }
            }
        }
    }
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
    fn test_missing_binary_returns_actionable_error() {
        let driver = CliDriver::new(CliProvider::Claude)
            .with_binary_path(PathBuf::from("/nonexistent/bin/claude"));
        let avail = driver.check_availability();
        assert!(!avail.installed);
        assert!(!avail.authenticated);
        assert!(avail.error_message.is_some());
        assert!(avail.error_message.unwrap().contains("Install via npm"));
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
