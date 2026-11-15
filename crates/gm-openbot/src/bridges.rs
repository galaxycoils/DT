use crate::supervisor::OpenBotSupervisor;
use gm_core::config::ChatBridgesConfig;
use gm_core::error::{GmError, Result};
use gm_core::ipc::QueueControlAction;
use gm_muse::directives::{RenderSurface, parse_directives, render_plain};
use gm_safety::{AuditCategory, format_external_content_fence};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeInboundMessage {
    pub platform: String,
    pub external_channel_id: String,
    pub sender_name: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeDispatchReply {
    pub accepted: bool,
    pub response_text: String,
    pub spawned_task_ids: Vec<String>,
}

/// Sanitize a bridge `(platform, external_channel_id)` pair into a safe `side-chats/<chat-id>` slug.
pub fn discretion_chat_id(platform: &str, external_channel_id: &str) -> String {
    let raw = format!(
        "{}-{}",
        platform.trim().to_ascii_lowercase(),
        external_channel_id.trim()
    );
    let mut clean: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if clean.is_empty() || clean.starts_with('.') {
        clean = "bridge_default".to_string();
    }
    clean
}

/// Verifies Slack webhook HMAC-SHA256 signature (`v0=<hex>`) over `v0:<timestamp>:<raw_body>`.
pub fn verify_slack_signature(
    signing_secret: &str,
    timestamp: &str,
    raw_body: &str,
    signature_header: &str,
) -> bool {
    let Ok(timestamp_value) = timestamp.parse::<i64>() else {
        return false;
    };
    if signing_secret.is_empty() || chrono::Utc::now().timestamp().abs_diff(timestamp_value) > 300 {
        return false;
    }
    let expected = compute_hmac_sha256_hex(
        signing_secret.as_bytes(),
        format!("v0:{timestamp}:{raw_body}").as_bytes(),
    );
    let expected_header = format!("v0={expected}");
    constant_time_eq(
        expected_header.as_bytes(),
        signature_header.trim().as_bytes(),
    )
}

/// RFC 2104 HMAC-SHA256 using `sha2::Sha256` and `hex`.
pub fn compute_hmac_sha256_hex(key: &[u8], message: &[u8]) -> String {
    const BLOCK_SIZE: usize = 64;
    let mut key_block = [0u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        let hashed = Sha256::digest(key);
        key_block[..hashed.len()].copy_from_slice(&hashed);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; BLOCK_SIZE];
    let mut opad = [0x5cu8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    hex::encode(outer.finalize())
}

pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Routes incoming messages from Telegram, Slack, and Discord into OpenBot channels and queues,
/// enforcing per-platform channel allowlists, per-chat `DISCRETION.md` policies,
/// untrusted external-content fencing, plain directive rendering, and remote slash controls.
#[derive(Clone)]
pub struct BridgeRouter {
    supervisor: OpenBotSupervisor,
    config: ChatBridgesConfig,
}

impl BridgeRouter {
    pub fn new(supervisor: OpenBotSupervisor, config: ChatBridgesConfig) -> Self {
        Self { supervisor, config }
    }

    pub fn is_channel_allowed(&self, platform: &str, external_channel_id: &str) -> bool {
        match platform.to_lowercase().as_str() {
            "telegram" => {
                self.config.telegram_enabled
                    && (!self.config.telegram_allowed_chat_ids.is_empty()
                        && self
                            .config
                            .telegram_allowed_chat_ids
                            .iter()
                            .any(|id| id == external_channel_id))
            }
            "slack" => {
                self.config.slack_enabled
                    && (!self.config.slack_allowed_channel_ids.is_empty()
                        && self
                            .config
                            .slack_allowed_channel_ids
                            .iter()
                            .any(|id| id == external_channel_id))
            }
            "discord" => {
                self.config.discord_enabled
                    && (!self.config.discord_allowed_channel_ids.is_empty()
                        && self
                            .config
                            .discord_allowed_channel_ids
                            .iter()
                            .any(|id| id == external_channel_id))
            }
            _ => false,
        }
    }

    pub fn handle_inbound(&self, msg: &BridgeInboundMessage) -> Result<BridgeDispatchReply> {
        if !self.is_channel_allowed(&msg.platform, &msg.external_channel_id) {
            return Err(GmError::SafetyViolation(format!(
                "External bridge '{}' channel '{}' is not enabled or not in allowlist",
                msg.platform, msg.external_channel_id
            )));
        }

        let trimmed = msg.text.trim();
        if trimmed.is_empty() {
            return Err(GmError::Config("Bridge message cannot be empty".into()));
        }

        // 1. Check slash commands: `/wheel <task_id> [note]`, `/pause <task_id>`, `/resume <task_id>`
        if let Some(rest) = trimmed.strip_prefix("/wheel ") {
            let mut parts = rest.trim().splitn(2, ' ');
            let task_id = parts.next().unwrap_or("").trim();
            let note = parts.next().map(|s| s.trim());
            if task_id.is_empty() {
                return Ok(BridgeDispatchReply {
                    accepted: false,
                    response_text: "Usage: /wheel <task_id> [note]".into(),
                    spawned_task_ids: vec![],
                });
            }
            let ok = self.supervisor.queue().resume_from_human(task_id, note)?;
            return Ok(BridgeDispatchReply {
                accepted: ok,
                response_text: if ok {
                    format!("🛞 Handed task `{task_id}` back to bot.")
                } else {
                    format!("Task `{task_id}` not found.")
                },
                spawned_task_ids: vec![],
            });
        }

        if let Some(task_id) = trimmed.strip_prefix("/pause ") {
            let clean = task_id.trim();
            let ok = self
                .supervisor
                .control_task(clean, &QueueControlAction::Pause)?;
            return Ok(BridgeDispatchReply {
                accepted: ok,
                response_text: format!("Paused task `{clean}`: {ok}"),
                spawned_task_ids: vec![],
            });
        }

        if let Some(task_id) = trimmed.strip_prefix("/resume ") {
            let clean = task_id.trim();
            let ok = self
                .supervisor
                .control_task(clean, &QueueControlAction::Resume)?;
            return Ok(BridgeDispatchReply {
                accepted: ok,
                response_text: format!("Resumed task `{clean}`: {ok}"),
                spawned_task_ids: vec![],
            });
        }

        // 2. Initialize side-chat `DISCRETION.md` policy if enabled and fence untrusted external text
        let chat_id = discretion_chat_id(&msg.platform, &msg.external_channel_id);
        let inbound_content = if self.supervisor.config.muse.enable_bridge_discretion {
            let _policy = self.supervisor.discretion().load_or_init(&chat_id)?;
            format_external_content_fence(trimmed, &format!("Sent from [{}]", msg.platform.trim()))
        } else {
            trimmed.to_string()
        };

        // 3. Post into OpenBot `#general` channel and trigger any `@bot` mentions or channel owner
        let author = format!("{}:{}", msg.platform, msg.sender_name);
        let (_, spawned) =
            self.supervisor
                .post_channel_message("general", &author, None, &inbound_content)?;
        let ids: Vec<String> = spawned.iter().map(|t| t.id.clone()).collect();

        Ok(BridgeDispatchReply {
            accepted: true,
            response_text: if ids.is_empty() {
                "Posted to #general".into()
            } else {
                format!("Queued task(s): {}", ids.join(", "))
            },
            spawned_task_ids: ids,
        })
    }

    /// Evaluate an outbound bridge response against `DISCRETION.md` (`RULE-PRIVACY-DISCRETION`,
    /// `ActionTier::Irreversible` for unrequested/out-of-rule replies) and render rich Muse
    /// directives into plain surface messages via `render_plain`.
    pub fn prepare_outbound_reply(
        &self,
        platform: &str,
        external_channel_id: &str,
        raw_response: &str,
        requested_reply: bool,
        approved_irreversible: bool,
    ) -> Result<Vec<String>> {
        let parts = parse_directives(raw_response);
        let plain_parts = render_plain(
            &parts,
            &RenderSurface::Bridge {
                platform: platform.trim().to_ascii_lowercase(),
            },
        );
        let non_empty: Vec<String> = plain_parts
            .into_iter()
            .filter(|p| !p.trim().is_empty())
            .collect();
        if non_empty.is_empty() {
            return Ok(Vec::new());
        }

        let chat_id = discretion_chat_id(platform, external_channel_id);
        if self.supervisor.config.muse.enable_bridge_discretion {
            let combined = non_empty.join("\n\n");
            let eval = self.supervisor.discretion().evaluate_outbound(
                &chat_id,
                &combined,
                requested_reply,
                approved_irreversible,
            )?;
            if !eval.allowed {
                let rule_id = eval
                    .rule_citation
                    .as_deref()
                    .unwrap_or("RULE-PRIVACY-DISCRETION");
                let reason = eval
                    .blocked_reason
                    .unwrap_or_else(|| "Bridge discretion refused outbound reply".into());
                self.supervisor.audit().log_governance(
                    "bridge_discretion_refused",
                    Some("muse"),
                    AuditCategory::Refused,
                    rule_id,
                    "External bridge reply blocked by DISCRETION.md",
                    &reason,
                    serde_json::json!({
                        "platform": platform,
                        "external_channel_id": external_channel_id,
                        "chat_id": chat_id,
                        "required_tier": format!("{:?}", eval.required_tier),
                    }),
                );
                return Err(GmError::SafetyViolation(reason));
            }
        }

        Ok(non_empty
            .into_iter()
            .map(|p| self.supervisor.scrubber().scrub_str(&p))
            .collect())
    }

    /// Dispatch multi-part outbound bridge replies with at least 1 second spacing between parts.
    pub async fn dispatch_outbound_parts<F, Fut>(
        &self,
        platform: &str,
        external_channel_id: &str,
        raw_response: &str,
        requested_reply: bool,
        approved_irreversible: bool,
        mut send_part: F,
    ) -> Result<Vec<String>>
    where
        F: FnMut(String) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let parts = self.prepare_outbound_reply(
            platform,
            external_channel_id,
            raw_response,
            requested_reply,
            approved_irreversible,
        )?;
        for (idx, part) in parts.iter().enumerate() {
            if idx > 0 {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            send_part(part.clone()).await?;
        }
        Ok(parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gm_core::config::GmConfig;
    use gm_core::muse_paths::MuseRuntimePaths;
    use gm_muse::shared_files::SharedFileStore;
    use gm_safety::AuditLogger;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    fn test_router() -> (tempfile::TempDir, OpenBotSupervisor, BridgeRouter) {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = GmConfig {
            storage_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        cfg.openbot.bots_dir = dir.path().join("bots");
        cfg.openbot.bridges = ChatBridgesConfig {
            telegram_enabled: false,
            telegram_allowed_chat_ids: vec![],
            slack_enabled: true,
            slack_allowed_channel_ids: vec!["C123".into()],
            discord_enabled: false,
            discord_allowed_channel_ids: vec![],
        };
        let (tx, _) = broadcast::channel(16);
        let sup =
            OpenBotSupervisor::open(cfg.clone(), Arc::new(AuditLogger::in_memory()), tx).unwrap();
        let router = BridgeRouter::new(sup.clone(), cfg.openbot.bridges);
        (dir, sup, router)
    }

    #[test]
    fn bridge_router_fences_inbound_blocks_private_leaks_and_renders_plain_directives() {
        let (dir, sup, router) = test_router();
        let paths = MuseRuntimePaths::shared(dir.path());
        SharedFileStore::global()
            .write_exact_sync(
                paths.user_md(),
                None,
                "# USER.md\n\n## Home\n- Address: 742 Evergreen Terrace, Springfield\n",
            )
            .unwrap();

        let inbound = BridgeInboundMessage {
            platform: "slack".into(),
            external_channel_id: "C123".into(),
            sender_name: "U99".into(),
            text: "@coder summarize sprint status".into(),
        };
        let reply = router.handle_inbound(&inbound).unwrap();
        assert!(reply.accepted);
        assert_eq!(reply.spawned_task_ids.len(), 1);

        // Verify posted channel message was wrapped in [BEGIN EXTERNAL CONTENT] fence
        let msgs = sup.channels().list_messages("general", 10).unwrap();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content.contains("[BEGIN EXTERNAL CONTENT]"));
        assert!(msgs[0].content.contains("source=\"Sent from [slack]\""));

        // Verify DISCRETION.md was initialized for slack-C123
        let policy = sup.discretion().load_or_init("slack-C123").unwrap();
        assert!(policy.raw_markdown.contains("# Default Policy"));

        // Private USER.md address leak MUST be refused and audited
        let leak_err = router
            .prepare_outbound_reply(
                "slack",
                "C123",
                "Sure! The address is 742 Evergreen Terrace, Springfield.",
                true,
                false,
            )
            .unwrap_err();
        assert!(leak_err.to_string().contains("RULE-PRIVACY-DISCRETION"));

        // Rich directives render into plain parts cleanly
        let parts = router
            .prepare_outbound_reply(
                "slack",
                "C123",
                "Status update 【source:daily_log】<message_break/>Next step <options><option>Approve</option></options>",
                true,
                false,
            )
            .unwrap();
        assert_eq!(parts.len(), 2);
        assert!(parts[0].contains("[1]") && parts[0].contains("Sources:"));
        assert!(parts[1].contains("1. Approve"));
    }
}
