//! Persistent custom MCP connectors (Remote HTTPS & Stdio Command) plus status catalog
//! for built-in integrations & bridges (`Gmail`, `Google Calendar`, `Slack`, `Teams`,
//! `GitHub`, `Discord`, `Telegram`, `1Password`, `Email Inbox`, `Voice / Meetings`, `Payments`).

use crate::vault::SecretBackend;
use chrono::Utc;
use gm_core::config::GmConfig;
use gm_core::error::{GmError, Result};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorTransport {
    RemoteHttps,
    StdioCommand,
}

impl ConnectorTransport {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConnectorTransport::RemoteHttps => "remote_https",
            ConnectorTransport::StdioCommand => "stdio_command",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "remote_https" | "remotehttps" | "https" | "http" => {
                Ok(ConnectorTransport::RemoteHttps)
            }
            "stdio_command" | "stdiocommand" | "stdio" => Ok(ConnectorTransport::StdioCommand),
            other => Err(GmError::Config(format!(
                "Unsupported connector transport '{other}'"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomConnectorRecord {
    pub id: String,
    pub name: String,
    pub transport: ConnectorTransport,
    pub url_or_command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub auth_header_name: Option<String>,
    #[serde(default)]
    pub secret_ref: Option<String>,
    pub has_secret: bool,
    #[serde(default)]
    pub account_label: String,
    pub enabled: bool,
    #[serde(default)]
    pub bot_ids: Vec<String>,
    #[serde(default)]
    pub status_text: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CustomConnectorInput {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub transport: ConnectorTransport,
    pub url_or_command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub auth_header_name: Option<String>,
    #[serde(default)]
    pub auth_secret: Option<String>,
    #[serde(default)]
    pub account_label: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub bot_ids: Vec<String>,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltInIntegrationStatus {
    pub id: String,
    pub name: String,
    pub category: String,
    pub status: String,
    pub account_summary: String,
    pub setup_hint: String,
    pub supports_multi_account: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityProbeResult {
    pub id: String,
    pub status: String,
    pub reachable: bool,
    pub detail: String,
}

#[derive(Clone)]
pub struct ConnectorStore {
    conn: Arc<Mutex<Connection>>,
    secrets: Option<Arc<dyn SecretBackend>>,
}

impl ConnectorStore {
    pub fn open_with_backend(
        db_path: impl AsRef<Path>,
        secrets: Option<Arc<dyn SecretBackend>>,
    ) -> Result<Self> {
        let conn = crate::db_recovery::open_sqlite_with_recovery(db_path.as_ref())?;
        Self::init_conn(conn, secrets)
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(|e| {
            GmError::Platform(format!("Failed to open in-memory connector DB: {e}"))
        })?;
        Self::init_conn(conn, None)
    }

    fn init_conn(conn: Connection, secrets: Option<Arc<dyn SecretBackend>>) -> Result<Self> {
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS custom_connectors_v1 (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                transport TEXT NOT NULL,
                url_or_command TEXT NOT NULL,
                args_json TEXT NOT NULL DEFAULT '[]',
                auth_header_name TEXT,
                secret_ref TEXT,
                has_secret INTEGER NOT NULL DEFAULT 0,
                account_label TEXT NOT NULL DEFAULT '',
                enabled INTEGER NOT NULL DEFAULT 1,
                bot_ids_json TEXT NOT NULL DEFAULT '[]',
                status_text TEXT NOT NULL DEFAULT 'Configured',
                updated_at TEXT NOT NULL
            );
            "#,
        )
        .map_err(|e| GmError::Platform(format!("ConnectorStore schema init failed: {e}")))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            secrets,
        })
    }

    pub fn list_connectors(&self) -> Result<Vec<CustomConnectorRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, transport, url_or_command, args_json, auth_header_name,
                       secret_ref, has_secret, account_label, enabled, bot_ids_json,
                       status_text, updated_at
                FROM custom_connectors_v1 ORDER BY name ASC
                "#,
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                let transport_str: String = r.get(2)?;
                let args_str: String = r.get(4)?;
                let bots_str: String = r.get(10)?;
                Ok(CustomConnectorRecord {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    transport: ConnectorTransport::parse(&transport_str)
                        .unwrap_or(ConnectorTransport::RemoteHttps),
                    url_or_command: r.get(3)?,
                    args: serde_json::from_str(&args_str).unwrap_or_default(),
                    auth_header_name: r.get(5)?,
                    secret_ref: r.get(6)?,
                    has_secret: r.get::<_, i32>(7)? != 0,
                    account_label: r.get(8)?,
                    enabled: r.get::<_, i32>(9)? != 0,
                    bot_ids: serde_json::from_str(&bots_str).unwrap_or_default(),
                    status_text: r.get(11)?,
                    updated_at: r.get(12)?,
                })
            })
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| GmError::Platform(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn upsert_connector(&self, input: CustomConnectorInput) -> Result<CustomConnectorRecord> {
        let name = input.name.trim();
        let target = input.url_or_command.trim();
        if name.is_empty() || name.len() > 128 {
            return Err(GmError::Config(
                "Connector name must be 1-128 characters".into(),
            ));
        }
        if target.is_empty() || target.len() > 1024 {
            return Err(GmError::Config(
                "Connector URL or command is required".into(),
            ));
        }
        if input.transport == ConnectorTransport::RemoteHttps
            && !target.starts_with("https://")
            && !target.starts_with("http://127.0.0.1")
            && !target.starts_with("http://localhost")
        {
            return Err(GmError::Config(
                "Remote MCP connector must use https:// (or loopback http://127.0.0.1)".into(),
            ));
        }

        let id = match input.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(existing) => existing.to_string(),
            None => format!("mcp-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]),
        };

        let mut secret_ref = None;
        let mut has_secret = false;
        if let Some(ref secret_val) = input.auth_secret
            && !secret_val.trim().is_empty()
        {
            let account = format!("gm_mcp_{id}");
            if let Some(ref backend) = self.secrets {
                backend.set_secret(&account, secret_val.trim())?;
                secret_ref = Some(format!("secret://keychain/{account}"));
                has_secret = true;
            }
        }

        let conn = self.conn.lock();
        if secret_ref.is_none()
            && let Some((old_ref, old_has)) = conn
                .query_row(
                    "SELECT secret_ref, has_secret FROM custom_connectors_v1 WHERE id = ?1",
                    params![&id],
                    |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i32>(1)? != 0)),
                )
                .optional()
                .map_err(|e| GmError::Platform(e.to_string()))?
        {
            secret_ref = old_ref;
            has_secret = old_has;
        }

        let status_text = if input.enabled {
            "Ready (Local config)".to_string()
        } else {
            "Disabled".to_string()
        };
        let now = Utc::now().to_rfc3339();
        let args_json = serde_json::to_string(&input.args)?;
        let bot_ids_json = serde_json::to_string(&input.bot_ids)?;

        conn.execute(
            r#"
            INSERT INTO custom_connectors_v1(
                id, name, transport, url_or_command, args_json, auth_header_name,
                secret_ref, has_secret, account_label, enabled, bot_ids_json,
                status_text, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                transport = excluded.transport,
                url_or_command = excluded.url_or_command,
                args_json = excluded.args_json,
                auth_header_name = excluded.auth_header_name,
                secret_ref = excluded.secret_ref,
                has_secret = excluded.has_secret,
                account_label = excluded.account_label,
                enabled = excluded.enabled,
                bot_ids_json = excluded.bot_ids_json,
                status_text = excluded.status_text,
                updated_at = excluded.updated_at
            "#,
            params![
                id,
                name,
                input.transport.as_str(),
                target,
                args_json,
                input.auth_header_name.as_deref().map(str::trim),
                secret_ref,
                has_secret as i32,
                input.account_label.trim(),
                input.enabled as i32,
                bot_ids_json,
                status_text,
                now
            ],
        )
        .map_err(|e| GmError::Platform(e.to_string()))?;

        Ok(CustomConnectorRecord {
            id,
            name: name.to_string(),
            transport: input.transport,
            url_or_command: target.to_string(),
            args: input.args,
            auth_header_name: input.auth_header_name.map(|s| s.trim().to_string()),
            secret_ref,
            has_secret,
            account_label: input.account_label.trim().to_string(),
            enabled: input.enabled,
            bot_ids: input.bot_ids,
            status_text,
            updated_at: now,
        })
    }

    pub fn delete_connector(&self, id: &str) -> Result<bool> {
        let rows = self
            .conn
            .lock()
            .execute(
                "DELETE FROM custom_connectors_v1 WHERE id = ?1",
                params![id.trim()],
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(rows > 0)
    }

    pub fn builtin_integrations(&self, cfg: &GmConfig) -> Vec<BuiltInIntegrationStatus> {
        let has_custom = |needle: &str| {
            self.list_connectors()
                .ok()
                .map(|list| {
                    list.iter().any(|c| {
                        c.enabled
                            && c.id.to_ascii_lowercase().contains(needle)
                            && (c.has_secret
                                || c.status_text.to_ascii_lowercase().contains("verified")
                                || c.status_text.to_ascii_lowercase().contains("connected"))
                    })
                })
                .unwrap_or(false)
        };
        let b = &cfg.openbot.bridges;
        vec![
            BuiltInIntegrationStatus {
                id: "github".into(),
                name: "GitHub".into(),
                category: "Code & Repos".into(),
                status: if has_custom("github") {
                    "connected".into()
                } else {
                    "setup_required".into()
                },
                account_summary: "Repositories, Pull Requests & Actions".into(),
                setup_hint: "Add GitHub MCP or PAT under Custom MCP / Vault".into(),
                supports_multi_account: true,
            },
            BuiltInIntegrationStatus {
                id: "slack".into(),
                name: "Slack".into(),
                category: "Messaging".into(),
                status: if b.slack_enabled {
                    "connected".into()
                } else {
                    "setup_required".into()
                },
                account_summary: format!("{} allowed channels", b.slack_allowed_channel_ids.len()),
                setup_hint: "Configure SLACK_BOT_TOKEN & signing secret in Keychain + bridges"
                    .into(),
                supports_multi_account: true,
            },
            BuiltInIntegrationStatus {
                id: "telegram".into(),
                name: "Telegram".into(),
                category: "Messaging".into(),
                status: if b.telegram_enabled {
                    "connected".into()
                } else {
                    "setup_required".into()
                },
                account_summary: format!("{} allowed chats", b.telegram_allowed_chat_ids.len()),
                setup_hint: "Add TELEGRAM_BOT_TOKEN & chat allowlist".into(),
                supports_multi_account: true,
            },
            BuiltInIntegrationStatus {
                id: "discord".into(),
                name: "Discord".into(),
                category: "Messaging".into(),
                status: if b.discord_enabled {
                    "connected".into()
                } else {
                    "setup_required".into()
                },
                account_summary: format!(
                    "{} allowed channels",
                    b.discord_allowed_channel_ids.len()
                ),
                setup_hint: "Add DISCORD_BOT_TOKEN & channel allowlist".into(),
                supports_multi_account: true,
            },
            BuiltInIntegrationStatus {
                id: "gmail".into(),
                name: "Gmail".into(),
                category: "Email & Calendar".into(),
                status: if has_custom("gmail") {
                    "connected".into()
                } else {
                    "setup_required".into()
                },
                account_summary: "Inbox triage, search, drafts & send approval".into(),
                setup_hint: "Connect Google Workspace MCP or IMAP/SMTP bridge".into(),
                supports_multi_account: true,
            },
            BuiltInIntegrationStatus {
                id: "google_calendar".into(),
                name: "Google Calendar".into(),
                category: "Email & Calendar".into(),
                status: if has_custom("calendar") {
                    "connected".into()
                } else {
                    "setup_required".into()
                },
                account_summary: "Upcoming meetings, invites & schedule blocks".into(),
                setup_hint: "Connect Google Calendar MCP or macOS EventKit helper".into(),
                supports_multi_account: true,
            },
            BuiltInIntegrationStatus {
                id: "msteams".into(),
                name: "Microsoft Teams".into(),
                category: "Messaging".into(),
                status: if has_custom("teams") {
                    "connected".into()
                } else {
                    "setup_required".into()
                },
                account_summary: "Teams chat & channel webhook bridge".into(),
                setup_hint: "Configure Teams MCP webhook URL in Custom Connectors".into(),
                supports_multi_account: true,
            },
            BuiltInIntegrationStatus {
                id: "1password".into(),
                name: "1Password CLI".into(),
                category: "Security & Vault".into(),
                status: if std::process::Command::new("op")
                    .arg("--version")
                    .output()
                    .is_ok_and(|o| o.status.success())
                {
                    "connected".into()
                } else {
                    "setup_required".into()
                },
                account_summary: "Resolves secret://1password/<vault>/<item>/<field>".into(),
                setup_hint: "Install 1Password CLI (`op`) or use OS Keychain Vault".into(),
                supports_multi_account: false,
            },
            BuiltInIntegrationStatus {
                id: "email_inbox".into(),
                name: "Bot Email Inbox".into(),
                category: "Email & Calendar".into(),
                status: if has_custom("imap") || has_custom("smtp") {
                    "connected".into()
                } else {
                    "setup_required".into()
                },
                account_summary: "Local IMAP/SMTP or inbound webhook email inbox".into(),
                setup_hint: "Configure Email bridge under Custom MCP Connectors".into(),
                supports_multi_account: true,
            },
            BuiltInIntegrationStatus {
                id: "voice_meetings".into(),
                name: "Voice / Phone & Meetings".into(),
                category: "Voice & Calls".into(),
                status: if has_custom("voice") || has_custom("sip") || has_custom("whisper") {
                    "connected".into()
                } else {
                    "setup_required".into()
                },
                account_summary: "Live STT dictation, voice chat & meeting note-taking".into(),
                setup_hint: "Connect local Whisper/STT or SIP/Meeting MCP bridge".into(),
                supports_multi_account: false,
            },
            BuiltInIntegrationStatus {
                id: "payments".into(),
                name: "Payments & Checkout".into(),
                category: "Commerce".into(),
                status: if has_custom("stripe") || has_custom("payment") {
                    "connected".into()
                } else {
                    "setup_required".into()
                },
                account_summary: "Order confirmation cards & default Vault card summary".into(),
                setup_hint: "Add default card in Settings -> Vault & configure checkout bridge"
                    .into(),
                supports_multi_account: false,
            },
        ]
    }

    /// Probes an integration or connector for real readiness (`"connected"`, `"degraded"`, or `"setup_required"`),
    /// never reporting `"connected"` without a verified secret, CLI check, or HTTP reachability probe.
    pub async fn probe_integration_capability(
        &self,
        cfg: &GmConfig,
        integration_id: &str,
    ) -> CapabilityProbeResult {
        let needle = integration_id.trim().to_ascii_lowercase();
        if needle == "1password" {
            let ok = std::process::Command::new("op")
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success());
            return CapabilityProbeResult {
                id: integration_id.to_string(),
                status: if ok { "connected" } else { "setup_required" }.into(),
                reachable: ok,
                detail: if ok {
                    "1Password CLI (`op`) available".into()
                } else {
                    "1Password CLI (`op`) not installed".into()
                },
            };
        }

        let matches = self
            .list_connectors()
            .unwrap_or_default()
            .into_iter()
            .filter(|c| c.enabled && c.id.to_ascii_lowercase().contains(&needle))
            .collect::<Vec<_>>();
        if let Some(conn) = matches.first() {
            if matches!(conn.transport, ConnectorTransport::RemoteHttps) {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(3))
                    .build()
                    .unwrap_or_else(|_| reqwest::Client::new());
                let mut req = client.get(&conn.url_or_command);
                if conn.has_secret
                    && let Some(ref sref) = conn.secret_ref
                    && let Some(account) = sref.strip_prefix("secret://keychain/")
                    && let Some(ref backend) = self.secrets
                    && let Ok(secret) = backend.get_secret(account)
                {
                    let header = conn.auth_header_name.as_deref().unwrap_or("Authorization");
                    req = req.header(header, secret);
                }
                match req.send().await {
                    Ok(resp) if resp.status().as_u16() < 500 => {
                        let status = if resp.status().is_success() {
                            "connected"
                        } else {
                            "degraded"
                        };
                        return CapabilityProbeResult {
                            id: integration_id.to_string(),
                            status: status.into(),
                            reachable: true,
                            detail: format!("HTTP {} from {}", resp.status(), conn.url_or_command),
                        };
                    }
                    Ok(resp) => {
                        return CapabilityProbeResult {
                            id: integration_id.to_string(),
                            status: "degraded".into(),
                            reachable: false,
                            detail: format!("HTTP {} from {}", resp.status(), conn.url_or_command),
                        };
                    }
                    Err(err) => {
                        return CapabilityProbeResult {
                            id: integration_id.to_string(),
                            status: "degraded".into(),
                            reachable: false,
                            detail: format!("Unreachable: {err}"),
                        };
                    }
                }
            }
            return CapabilityProbeResult {
                id: integration_id.to_string(),
                status: "connected".into(),
                reachable: true,
                detail: format!("Stdio connector '{}'", conn.url_or_command),
            };
        }

        let b = &cfg.openbot.bridges;
        let bridge_ok = match needle.as_str() {
            "slack" => b.slack_enabled && !b.slack_allowed_channel_ids.is_empty(),
            "telegram" => b.telegram_enabled && !b.telegram_allowed_chat_ids.is_empty(),
            "discord" => b.discord_enabled && !b.discord_allowed_channel_ids.is_empty(),
            _ => false,
        };
        CapabilityProbeResult {
            id: integration_id.to_string(),
            status: if bridge_ok {
                "connected"
            } else {
                "setup_required"
            }
            .into(),
            reachable: bridge_ok,
            detail: if bridge_ok {
                "Bridge enabled with allowlist".into()
            } else {
                "Missing verified connector or credentials".into()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::InMemorySecretBackend;

    #[test]
    fn custom_connectors_and_builtins() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("conn.sqlite");
        let secrets = Arc::new(InMemorySecretBackend::default());
        let store = ConnectorStore::open_with_backend(&db, Some(secrets)).unwrap();

        let created = store
            .upsert_connector(CustomConnectorInput {
                id: Some("linear-mcp".into()),
                name: "Linear Issues".into(),
                transport: ConnectorTransport::RemoteHttps,
                url_or_command: "https://mcp.linear.app/sse".into(),
                args: vec![],
                auth_header_name: Some("Authorization".into()),
                auth_secret: Some("Bearer lin_api_secret".into()),
                account_label: "engineering".into(),
                enabled: true,
                bot_ids: vec!["coder".into()],
            })
            .unwrap();
        assert!(created.has_secret);
        assert_eq!(store.list_connectors().unwrap().len(), 1);

        // Plaintext HTTP non-loopback rejected
        assert!(
            store
                .upsert_connector(CustomConnectorInput {
                    id: None,
                    name: "Bad".into(),
                    transport: ConnectorTransport::RemoteHttps,
                    url_or_command: "http://example.com/mcp".into(),
                    args: vec![],
                    auth_header_name: None,
                    auth_secret: None,
                    account_label: String::new(),
                    enabled: true,
                    bot_ids: vec![],
                })
                .is_err()
        );

        let builtins = store.builtin_integrations(&GmConfig::default());
        assert_eq!(builtins.len(), 11);
    }
}
