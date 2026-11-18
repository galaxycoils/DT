//! Asterisk ARI + External Media SIP Trunk adapter.
//! Enforces Tier-3 Irreversible carrier-cost gating before originating outbound PSTN calls,
//! supports RFC 6455 ARI event WebSocket streaming, live transcripts, human handoff, and recovery.

use crate::voice::{TranscriptTurn, VoiceSessionState};
use base64::Engine;
use chrono::Utc;
use gm_core::config::extract_provider_origin;
use gm_core::error::{GmError, Result};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsteriskAriConfig {
    pub ari_base_url: String,
    pub ws_events_url: String,
    pub app_name: String,
    pub trunk_endpoint: String,
    pub username: String,
    #[serde(skip_serializing)]
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SipCallSession {
    pub call_id: String,
    pub channel_id: String,
    pub external_media_channel_id: Option<String>,
    pub callee: String,
    pub bot_id: String,
    pub carrier_cost_approved: bool,
    pub consent_announced: bool,
    pub state: VoiceSessionState,
    pub transcripts: Vec<TranscriptTurn>,
    pub reconnect_attempts: u32,
    pub updated_at: String,
}

/// Minimal RFC 6455 client helper for loopback / plaintext WebSocket signaling in local adapters & tests.
pub(crate) async fn ws_connect_and_read_text_frames(
    ws_url: &str,
    max_frames: usize,
) -> Result<Vec<String>> {
    let trimmed = ws_url.trim();
    let rest = trimmed
        .strip_prefix("ws://")
        .ok_or_else(|| GmError::Config(format!("Expected ws:// URL, got '{trimmed}'")))?;
    let (host_port, path) = match rest.split_once('/') {
        Some((hp, p)) => (hp, format!("/{p}")),
        None => (rest, "/".to_string()),
    };
    let mut stream = TcpStream::connect(host_port)
        .await
        .map_err(|e| GmError::Platform(format!("WebSocket connect to {host_port} failed: {e}")))?;
    let key_bytes: [u8; 16] = *uuid::Uuid::new_v4().as_bytes();
    let ws_key = base64::engine::general_purpose::STANDARD.encode(key_bytes);
    let handshake = format!(
        "GET {path} HTTP/1.1\r\nHost: {host_port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {ws_key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    stream
        .write_all(handshake.as_bytes())
        .await
        .map_err(|e| GmError::Platform(format!("WebSocket handshake write error: {e}")))?;

    let mut hdr_buf = vec![0u8; 4096];
    let mut filled = 0usize;
    let hdr_end = loop {
        let n = stream
            .read(&mut hdr_buf[filled..])
            .await
            .map_err(|e| GmError::Platform(format!("WebSocket handshake read error: {e}")))?;
        if n == 0 {
            return Err(GmError::Platform(
                "WebSocket closed during handshake".into(),
            ));
        }
        filled += n;
        if let Some(pos) = hdr_buf[..filled].windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        if filled == hdr_buf.len() {
            return Err(GmError::Platform(
                "Oversized WebSocket response header".into(),
            ));
        }
    };
    let hdr_str = String::from_utf8_lossy(&hdr_buf[..hdr_end]);
    if !hdr_str.contains("101") {
        return Err(GmError::Platform(format!(
            "WebSocket upgrade rejected: {}",
            hdr_str.lines().next().unwrap_or("")
        )));
    }
    // Validate Sec-WebSocket-Accept if provided
    let mut expected_sha = Sha1::new();
    expected_sha.update(format!("{ws_key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes());
    let expected_accept = base64::engine::general_purpose::STANDARD.encode(expected_sha.finalize());
    for line in hdr_str.lines() {
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case("sec-websocket-accept")
            && v.trim() != expected_accept
        {
            return Err(GmError::SafetyViolation(
                "Invalid Sec-WebSocket-Accept header".into(),
            ));
        }
    }

    let mut leftover = hdr_buf[hdr_end..filled].to_vec();
    let mut frames = Vec::new();
    while frames.len() < max_frames {
        while leftover.len() < 2 {
            let mut tmp = [0u8; 2048];
            let n = stream.read(&mut tmp).await.unwrap_or(0);
            if n == 0 {
                return Ok(frames);
            }
            leftover.extend_from_slice(&tmp[..n]);
        }
        let opcode = leftover[0] & 0x0f;
        let len7 = leftover[1] & 0x7f;
        let mut idx = 2usize;
        let payload_len = match len7 {
            126 => {
                while leftover.len() < idx + 2 {
                    let mut tmp = [0u8; 2048];
                    let n = stream.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        return Ok(frames);
                    }
                    leftover.extend_from_slice(&tmp[..n]);
                }
                let l = u16::from_be_bytes([leftover[idx], leftover[idx + 1]]) as usize;
                idx += 2;
                l
            }
            127 => {
                while leftover.len() < idx + 8 {
                    let mut tmp = [0u8; 2048];
                    let n = stream.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        return Ok(frames);
                    }
                    leftover.extend_from_slice(&tmp[..n]);
                }
                let l = u64::from_be_bytes(leftover[idx..idx + 8].try_into().unwrap()) as usize;
                idx += 8;
                l
            }
            n => n as usize,
        };
        while leftover.len() < idx + payload_len {
            let mut tmp = [0u8; 4096];
            let n = stream.read(&mut tmp).await.unwrap_or(0);
            if n == 0 {
                return Ok(frames);
            }
            leftover.extend_from_slice(&tmp[..n]);
        }
        let payload = leftover[idx..idx + payload_len].to_vec();
        leftover.drain(..idx + payload_len);
        if opcode == 0x8 {
            break;
        }
        if opcode == 0x1
            && let Ok(text) = String::from_utf8(payload)
        {
            frames.push(text);
        }
    }
    Ok(frames)
}

#[derive(Clone)]
pub struct AsteriskSipClient {
    cfg: AsteriskAriConfig,
    client: reqwest::Client,
}

impl AsteriskSipClient {
    pub fn new(cfg: AsteriskAriConfig) -> Result<Self> {
        let _ = extract_provider_origin(&cfg.ari_base_url)?;
        Ok(Self {
            cfg,
            client: reqwest::Client::new(),
        })
    }

    pub async fn originate_call(
        &self,
        bot_id: &str,
        callee: &str,
        approved_carrier_cost: bool,
    ) -> Result<SipCallSession> {
        if !approved_carrier_cost {
            return Err(GmError::SafetyViolation(
                "Tier-3 Irreversible gate: explicit carrier-cost & consent approval required before outbound PSTN/SIP call"
                    .into(),
            ));
        }
        let clean_callee = callee.trim();
        if clean_callee.is_empty() {
            return Err(GmError::Config("SIP callee cannot be empty".into()));
        }
        let url = format!("{}/channels", self.cfg.ari_base_url.trim_end_matches('/'));
        let resp = self
            .client
            .post(&url)
            .basic_auth(&self.cfg.username, Some(&self.cfg.password))
            .json(&serde_json::json!({
                "endpoint": format!("{}/{}", self.cfg.trunk_endpoint, clean_callee),
                "app": self.cfg.app_name,
                "callerId": format!("GrokBot <{bot_id}>"),
            }))
            .send()
            .await
            .map_err(|e| GmError::Platform(format!("ARI originate failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(GmError::Platform(format!(
                "ARI originate HTTP {}",
                resp.status()
            )));
        }
        let created: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GmError::Platform(format!("Invalid ARI channel JSON: {e}")))?;
        let channel_id = created["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("chan-unknown")
            .to_string();

        // Start ExternalMedia leg so audio streams cleanly to local STT/TTS.
        let ext_url = format!(
            "{}/channels/externalMedia",
            self.cfg.ari_base_url.trim_end_matches('/')
        );
        let ext_resp = self
            .client
            .post(&ext_url)
            .basic_auth(&self.cfg.username, Some(&self.cfg.password))
            .json(&serde_json::json!({
                "app": self.cfg.app_name,
                "external_host": "127.0.0.1:4000",
                "format": "ulaw",
            }))
            .send()
            .await
            .ok();
        let ext_id = if let Some(r) = ext_resp
            && r.status().is_success()
        {
            r.json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v["id"].as_str().map(ToString::to_string))
        } else {
            None
        };

        Ok(SipCallSession {
            call_id: format!("sip-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]),
            channel_id,
            external_media_channel_id: ext_id,
            callee: clean_callee.to_string(),
            bot_id: bot_id.to_string(),
            carrier_cost_approved: true,
            consent_announced: false,
            state: VoiceSessionState::Listening,
            transcripts: Vec::new(),
            reconnect_attempts: 0,
            updated_at: Utc::now().to_rfc3339(),
        })
    }

    pub async fn announce_consent(&self, session: &mut SipCallSession, prompt: &str) -> Result<()> {
        let url = format!(
            "{}/channels/{}/play",
            self.cfg.ari_base_url.trim_end_matches('/'),
            session.channel_id
        );
        let resp = self
            .client
            .post(&url)
            .basic_auth(&self.cfg.username, Some(&self.cfg.password))
            .json(&serde_json::json!({ "media": format!("sound:{prompt}") }))
            .send()
            .await
            .map_err(|e| GmError::Platform(format!("ARI consent announcement failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(GmError::Platform(format!(
                "ARI play HTTP {}",
                resp.status()
            )));
        }
        session.consent_announced = true;
        session.transcripts.push(TranscriptTurn {
            id: format!("sip-t-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]),
            speaker: session.bot_id.clone(),
            text: prompt.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            is_final: true,
        });
        session.updated_at = Utc::now().to_rfc3339();
        Ok(())
    }

    pub async fn stream_ari_transcript_events(
        &self,
        session: &mut SipCallSession,
        max_frames: usize,
    ) -> Result<usize> {
        let frames = ws_connect_and_read_text_frames(&self.cfg.ws_events_url, max_frames).await?;
        let mut count = 0usize;
        for raw in frames {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw)
                && let Some(text) = val["transcript"].as_str().or_else(|| val["text"].as_str())
            {
                let speaker = val["speaker"].as_str().unwrap_or("caller");
                session.transcripts.push(TranscriptTurn {
                    id: format!("sip-t-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]),
                    speaker: speaker.to_string(),
                    text: text.to_string(),
                    timestamp: Utc::now().to_rfc3339(),
                    is_final: true,
                });
                count += 1;
            }
        }
        session.updated_at = Utc::now().to_rfc3339();
        Ok(count)
    }

    pub async fn transfer_to_human(
        &self,
        session: &mut SipCallSession,
        operator_extension: &str,
    ) -> Result<()> {
        let url = format!(
            "{}/channels/{}/redirect",
            self.cfg.ari_base_url.trim_end_matches('/'),
            session.channel_id
        );
        let resp = self
            .client
            .post(&url)
            .basic_auth(&self.cfg.username, Some(&self.cfg.password))
            .json(&serde_json::json!({ "endpoint": operator_extension }))
            .send()
            .await
            .map_err(|e| GmError::Platform(format!("ARI transfer failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(GmError::Platform(format!(
                "ARI transfer HTTP {}",
                resp.status()
            )));
        }
        session.state = VoiceSessionState::HandedOff;
        session.updated_at = Utc::now().to_rfc3339();
        Ok(())
    }

    pub async fn hangup_and_recover(&self, session: &mut SipCallSession) -> Result<()> {
        let external = if let Some(media_id) = session.external_media_channel_id.as_deref() {
            let result = self.hangup_channel(media_id).await;
            if result.is_ok() {
                session.external_media_channel_id = None;
            }
            result
        } else {
            Ok(())
        };
        let primary = self.hangup_channel(&session.channel_id).await;
        primary.and(external)?;
        session.state = VoiceSessionState::Ended;
        session.updated_at = Utc::now().to_rfc3339();
        Ok(())
    }

    async fn hangup_channel(&self, channel_id: &str) -> Result<()> {
        let url = format!(
            "{}/channels/{channel_id}",
            self.cfg.ari_base_url.trim_end_matches('/')
        );
        let resp = self
            .client
            .delete(&url)
            .basic_auth(&self.cfg.username, Some(&self.cfg.password))
            .send()
            .await
            .map_err(|e| GmError::Platform(format!("ARI hangup failed: {e}")))?;
        if !resp.status().is_success() && resp.status().as_u16() != 404 {
            return Err(GmError::Platform(format!(
                "ARI hangup HTTP {}",
                resp.status()
            )));
        }
        Ok(())
    }

    pub fn reconnect_session(&self, session: &mut SipCallSession) -> Result<()> {
        if session.state == VoiceSessionState::Ended {
            return Err(GmError::Config("Cannot reconnect an ended SIP call".into()));
        }
        session.reconnect_attempts += 1;
        session.state = VoiceSessionState::Listening;
        session.updated_at = Utc::now().to_rfc3339();
        Ok(())
    }
}
