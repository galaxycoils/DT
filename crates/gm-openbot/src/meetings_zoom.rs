//! Zoom RTMS (Realtime Media Streams) meeting client for consent gating, webhook HMAC
//! verification, live WebSocket transcript ingestion, handoff, leave, and reconnect recovery.

use crate::bridges::{compute_hmac_sha256_hex, constant_time_eq};
use crate::sip_asterisk::ws_connect_and_read_text_frames;
use crate::voice::{TranscriptTurn, VoiceSessionState};
use chrono::Utc;
use gm_core::config::extract_provider_origin;
use gm_core::error::{GmError, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoomRtmsConfig {
    pub rest_base_url: String,
    pub client_id: String,
    #[serde(skip_serializing)]
    pub client_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoomMeetingSession {
    pub meeting_uuid: String,
    pub rtms_stream_id: String,
    pub server_ws_url: String,
    pub bot_id: String,
    pub consent_confirmed: bool,
    pub state: VoiceSessionState,
    pub participants: Vec<String>,
    pub notes: Vec<TranscriptTurn>,
    pub reconnects: u32,
}

#[derive(Clone)]
pub struct ZoomRtmsClient {
    cfg: ZoomRtmsConfig,
    client: reqwest::Client,
}

impl ZoomRtmsClient {
    pub fn new(cfg: ZoomRtmsConfig) -> Result<Self> {
        let _ = extract_provider_origin(&cfg.rest_base_url)?;
        Ok(Self {
            cfg,
            client: reqwest::Client::new(),
        })
    }

    pub fn verify_webhook_signature(
        &self,
        timestamp: &str,
        raw_body: &str,
        signature_header: &str,
    ) -> bool {
        let msg = format!("v0:{timestamp}:{raw_body}");
        let expected = format!(
            "v0={}",
            compute_hmac_sha256_hex(self.cfg.client_secret.as_bytes(), msg.as_bytes())
        );
        constant_time_eq(expected.as_bytes(), signature_header.trim().as_bytes())
    }

    pub async fn join_stream(
        &self,
        bot_id: &str,
        meeting_uuid: &str,
        rtms_stream_id: &str,
        server_ws_url: &str,
        consent_confirmed: bool,
    ) -> Result<ZoomMeetingSession> {
        if !consent_confirmed {
            return Err(GmError::SafetyViolation(
                "Zoom RTMS requires explicit meeting participant consent notice before joining"
                    .into(),
            ));
        }
        let url = format!(
            "{}/live_meetings/{}/rtms/join",
            self.cfg.rest_base_url.trim_end_matches('/'),
            meeting_uuid.trim()
        );
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({
                "client_id": self.cfg.client_id,
                "rtms_stream_id": rtms_stream_id,
                "bot_id": bot_id,
            }))
            .send()
            .await
            .map_err(|e| GmError::Platform(format!("Zoom RTMS join failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(GmError::Platform(format!(
                "Zoom RTMS join HTTP {}",
                resp.status()
            )));
        }
        Ok(ZoomMeetingSession {
            meeting_uuid: meeting_uuid.to_string(),
            rtms_stream_id: rtms_stream_id.to_string(),
            server_ws_url: server_ws_url.to_string(),
            bot_id: bot_id.to_string(),
            consent_confirmed: true,
            state: VoiceSessionState::Listening,
            participants: Vec::new(),
            notes: Vec::new(),
            reconnects: 0,
        })
    }

    pub async fn ingest_ws_events(
        &self,
        session: &mut ZoomMeetingSession,
        max_frames: usize,
    ) -> Result<usize> {
        let frames = ws_connect_and_read_text_frames(&session.server_ws_url, max_frames).await?;
        let mut count = 0usize;
        for raw in frames {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
                let kind = val["event"].as_str().unwrap_or("");
                if kind == "participant.joined"
                    && let Some(name) = val["participant"].as_str()
                    && !session.participants.iter().any(|p| p == name)
                {
                    session.participants.push(name.to_string());
                } else if kind == "transcript.data"
                    && let Some(text) = val["data"].as_str().or_else(|| val["text"].as_str())
                {
                    let speaker = val["user_name"].as_str().unwrap_or("participant");
                    session.notes.push(TranscriptTurn {
                        id: format!("zm-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]),
                        speaker: speaker.to_string(),
                        text: text.to_string(),
                        timestamp: Utc::now().to_rfc3339(),
                        is_final: true,
                    });
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    pub fn handoff_host_control(&self, session: &mut ZoomMeetingSession) {
        session.state = VoiceSessionState::HandedOff;
    }

    pub async fn leave_and_summarize(&self, session: &mut ZoomMeetingSession) -> Result<String> {
        let url = format!(
            "{}/live_meetings/{}/rtms/leave",
            self.cfg.rest_base_url.trim_end_matches('/'),
            session.meeting_uuid
        );
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({ "rtms_stream_id": session.rtms_stream_id }))
            .send()
            .await
            .map_err(|e| GmError::Platform(format!("Zoom RTMS leave failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(GmError::Platform(format!(
                "Zoom RTMS leave HTTP {}",
                resp.status()
            )));
        }
        session.state = VoiceSessionState::Ended;
        let summary = session
            .notes
            .iter()
            .map(|t| format!("- {}: {}", t.speaker, t.text))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(summary)
    }

    pub fn recover_reconnect(&self, session: &mut ZoomMeetingSession) -> Result<()> {
        if session.state == VoiceSessionState::Ended {
            return Err(GmError::Config("Cannot reconnect an ended Zoom session".into()));
        }
        session.reconnects += 1;
        session.state = VoiceSessionState::Listening;
        Ok(())
    }
}
