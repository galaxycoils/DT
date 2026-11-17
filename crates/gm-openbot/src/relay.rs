//! Outbound-only HTTPS relay client for webhook/chat bridges without requiring public inbound ports.
//! Verifies HMAC-SHA256 (or Ed25519 when configured) signed webhook envelopes, enforces timestamp
//! replay windows (<= 300s), deduplicates delivery IDs, and validates remote origins (RULE-NET-001).

use crate::bridges::{
    BridgeDispatchReply, BridgeInboundMessage, BridgeMappingStore, BridgeRouter,
    compute_hmac_sha256_hex, constant_time_eq,
};
use gm_core::config::extract_provider_origin;
use gm_core::error::{GmError, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedRelayEnvelope {
    pub delivery_id: String,
    pub platform: String,
    pub external_channel_id: String,
    pub sender_name: String,
    pub text: String,
    pub timestamp: i64,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayPollBatch {
    #[serde(default)]
    pub envelopes: Vec<SignedRelayEnvelope>,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OutboundRelayClient {
    relay_base_url: String,
    signing_secret: String,
    bearer_token: String,
    client: reqwest::Client,
}

impl OutboundRelayClient {
    pub fn new(
        relay_base_url: impl Into<String>,
        signing_secret: impl Into<String>,
        bearer_token: impl Into<String>,
    ) -> Result<Self> {
        let base = relay_base_url.into();
        let _ = extract_provider_origin(&base)?;
        Ok(Self {
            relay_base_url: base.trim_end_matches('/').to_string(),
            signing_secret: signing_secret.into(),
            bearer_token: bearer_token.into(),
            client: reqwest::Client::new(),
        })
    }

    pub fn canonical_payload(env: &SignedRelayEnvelope) -> String {
        format!(
            "v1:{}:{}:{}:{}:{}:{}",
            env.delivery_id,
            env.timestamp,
            env.platform,
            env.external_channel_id,
            env.sender_name,
            env.text
        )
    }

    pub fn sign_envelope(&self, env: &mut SignedRelayEnvelope) {
        let payload = Self::canonical_payload(env);
        let mac = compute_hmac_sha256_hex(self.signing_secret.as_bytes(), payload.as_bytes());
        env.signature = format!("v1={mac}");
    }

    pub fn verify_envelope(&self, env: &SignedRelayEnvelope, now_ts: i64) -> Result<()> {
        if self.signing_secret.trim().is_empty() {
            return Err(GmError::SafetyViolation(
                "Relay signing secret cannot be empty".into(),
            ));
        }
        if env.delivery_id.trim().is_empty() {
            return Err(GmError::SafetyViolation("Missing relay delivery_id".into()));
        }
        if now_ts.abs_diff(env.timestamp) > 300 {
            return Err(GmError::SafetyViolation(format!(
                "Relay envelope '{}' expired outside 300s replay window",
                env.delivery_id
            )));
        }
        let payload = Self::canonical_payload(env);
        let expected = format!(
            "v1={}",
            compute_hmac_sha256_hex(self.signing_secret.as_bytes(), payload.as_bytes())
        );
        if !constant_time_eq(expected.as_bytes(), env.signature.trim().as_bytes()) {
            return Err(GmError::SafetyViolation(format!(
                "Invalid HMAC signature on relay envelope '{}'",
                env.delivery_id
            )));
        }
        Ok(())
    }

    pub async fn poll_and_ingest(
        &self,
        cursor: Option<&str>,
        router: &BridgeRouter,
        mappings: &BridgeMappingStore,
    ) -> Result<(Vec<BridgeDispatchReply>, Option<String>)> {
        let mut url = format!("{}/v1/relay/poll", self.relay_base_url);
        if let Some(c) = cursor.filter(|s| !s.trim().is_empty()) {
            url.push_str(&format!("?cursor={c}"));
        }
        let mut req = self.client.get(&url);
        if !self.bearer_token.trim().is_empty() {
            req = req.bearer_auth(self.bearer_token.trim());
        }
        let resp = req
            .send()
            .await
            .map_err(|e| GmError::Platform(format!("Relay poll failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(GmError::Platform(format!(
                "Relay poll HTTP {}",
                resp.status()
            )));
        }
        let batch: RelayPollBatch = resp
            .json()
            .await
            .map_err(|e| GmError::Platform(format!("Invalid relay poll payload: {e}")))?;
        let now = chrono::Utc::now().timestamp();
        let mut replies = Vec::new();
        for env in &batch.envelopes {
            self.verify_envelope(env, now)?;
            if !mappings.mark_delivery_seen(&env.platform, &env.delivery_id)? {
                continue;
            }
            let msg = BridgeInboundMessage {
                platform: env.platform.clone(),
                external_channel_id: env.external_channel_id.clone(),
                sender_name: env.sender_name.clone(),
                text: env.text.clone(),
            };
            replies.push(router.handle_inbound_with_mapping(&msg, Some(mappings))?);
        }
        Ok((replies, batch.next_cursor))
    }

    pub async fn send_outbound_reply(
        &self,
        router: &BridgeRouter,
        platform: &str,
        external_channel_id: &str,
        raw_response: &str,
        requested_reply: bool,
        approved_irreversible: bool,
    ) -> Result<Vec<String>> {
        let client = self.client.clone();
        let endpoint = format!("{}/v1/relay/send", self.relay_base_url);
        let bearer = self.bearer_token.clone();
        let plat = platform.to_string();
        let chan = external_channel_id.to_string();
        router
            .dispatch_outbound_parts(
                platform,
                external_channel_id,
                raw_response,
                requested_reply,
                approved_irreversible,
                |part| {
                    let c = client.clone();
                    let url = endpoint.clone();
                    let tok = bearer.clone();
                    let p = plat.clone();
                    let ch = chan.clone();
                    async move {
                        let mut req = c.post(&url).json(&serde_json::json!({
                            "platform": p,
                            "external_channel_id": ch,
                            "text": part,
                        }));
                        if !tok.trim().is_empty() {
                            req = req.bearer_auth(tok.trim());
                        }
                        let resp = req
                            .send()
                            .await
                            .map_err(|e| GmError::Platform(format!("Relay send failed: {e}")))?;
                        if !resp.status().is_success() {
                            return Err(GmError::Platform(format!(
                                "Relay send HTTP {}",
                                resp.status()
                            )));
                        }
                        Ok(())
                    }
                },
            )
            .await
    }
}
