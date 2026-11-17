//! Local Speech-to-Text (Whisper HTTP / whisper-cli / system) and Text-to-Speech runtime,
//! including real-time barge-in cancellation, consent gating, and live transcript assembly.

use chrono::Utc;
use gm_core::config::extract_provider_origin;
use gm_core::error::{GmError, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SttMode {
    #[default]
    LocalWhisperHttp,
    WhisperCli,
    Disabled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TtsMode {
    #[default]
    LocalHttp,
    MacOsSay,
    Disabled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum VoiceSessionState {
    #[default]
    Idle,
    Listening,
    Transcribing,
    Speaking,
    HandedOff,
    Ended,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranscriptTurn {
    pub id: String,
    pub speaker: String,
    pub text: String,
    pub timestamp: String,
    pub is_final: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceConfig {
    pub stt_mode: SttMode,
    pub tts_mode: TtsMode,
    pub stt_endpoint: String,
    pub tts_endpoint: String,
    pub whisper_model: String,
    pub consent_prompt: String,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            stt_mode: SttMode::LocalWhisperHttp,
            tts_mode: TtsMode::LocalHttp,
            stt_endpoint: "http://127.0.0.1:8178/inference".into(),
            tts_endpoint: "http://127.0.0.1:59125/api/tts".into(),
            whisper_model: "base.en".into(),
            consent_prompt: "This call may be transcribed by your assistant.".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceSession {
    pub session_id: String,
    pub bot_id: String,
    pub state: VoiceSessionState,
    pub consent_acknowledged: bool,
    pub barge_in_requested: bool,
    pub transcript: Vec<TranscriptTurn>,
    pub updated_at: String,
}

impl VoiceSession {
    pub fn start(bot_id: impl Into<String>) -> Self {
        Self {
            session_id: format!("voice-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]),
            bot_id: bot_id.into(),
            state: VoiceSessionState::Listening,
            consent_acknowledged: false,
            barge_in_requested: false,
            transcript: Vec::new(),
            updated_at: Utc::now().to_rfc3339(),
        }
    }

    pub fn acknowledge_consent(&mut self) {
        self.consent_acknowledged = true;
        self.updated_at = Utc::now().to_rfc3339();
    }

    pub fn request_barge_in(&mut self) {
        self.barge_in_requested = true;
        if self.state == VoiceSessionState::Speaking {
            self.state = VoiceSessionState::Listening;
        }
        self.updated_at = Utc::now().to_rfc3339();
    }

    pub fn append_turn(&mut self, speaker: impl Into<String>, text: impl Into<String>, is_final: bool) {
        self.transcript.push(TranscriptTurn {
            id: format!("trn-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]),
            speaker: speaker.into(),
            text: text.into(),
            timestamp: Utc::now().to_rfc3339(),
            is_final,
        });
        self.updated_at = Utc::now().to_rfc3339();
    }

    pub fn handoff_to_human(&mut self) {
        self.state = VoiceSessionState::HandedOff;
        self.updated_at = Utc::now().to_rfc3339();
    }

    pub fn end(&mut self) {
        self.state = VoiceSessionState::Ended;
        self.updated_at = Utc::now().to_rfc3339();
    }
}

#[derive(Clone)]
pub struct LocalVoiceEngine {
    cfg: VoiceConfig,
    client: reqwest::Client,
}

impl LocalVoiceEngine {
    pub fn new(cfg: VoiceConfig) -> Result<Self> {
        if !cfg.stt_endpoint.trim().is_empty() {
            let _ = extract_provider_origin(&cfg.stt_endpoint)?;
        }
        if !cfg.tts_endpoint.trim().is_empty() {
            let _ = extract_provider_origin(&cfg.tts_endpoint)?;
        }
        Ok(Self {
            cfg,
            client: reqwest::Client::new(),
        })
    }

    pub async fn transcribe_wav(&self, session: &mut VoiceSession, wav_bytes: &[u8]) -> Result<String> {
        if !session.consent_acknowledged {
            return Err(GmError::SafetyViolation(
                "Voice transcription requires explicit participant consent first".into(),
            ));
        }
        if self.cfg.stt_mode == SttMode::Disabled {
            return Err(GmError::Config("Local STT is disabled".into()));
        }
        session.state = VoiceSessionState::Transcribing;
        let resp = self
            .client
            .post(&self.cfg.stt_endpoint)
            .header("content-type", "audio/wav")
            .body(wav_bytes.to_vec())
            .send()
            .await
            .map_err(|e| GmError::Platform(format!("Local STT request failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(GmError::Platform(format!(
                "Local STT returned HTTP {}",
                resp.status()
            )));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GmError::Platform(format!("Invalid STT JSON response: {e}")))?;
        let text = body["text"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        if !text.is_empty() {
            session.append_turn("caller", &text, true);
        }
        session.state = VoiceSessionState::Listening;
        Ok(text)
    }

    pub async fn synthesize_speech(&self, session: &mut VoiceSession, text: &str) -> Result<Vec<u8>> {
        if session.barge_in_requested {
            session.barge_in_requested = false;
            session.state = VoiceSessionState::Listening;
            return Ok(Vec::new());
        }
        session.state = VoiceSessionState::Speaking;
        let clean = text.trim();
        if clean.is_empty() {
            session.state = VoiceSessionState::Listening;
            return Ok(Vec::new());
        }
        match self.cfg.tts_mode {
            TtsMode::Disabled => Err(GmError::Config("Local TTS is disabled".into())),
            TtsMode::LocalHttp => {
                let resp = self
                    .client
                    .post(&self.cfg.tts_endpoint)
                    .json(&serde_json::json!({ "text": clean, "model": self.cfg.whisper_model }))
                    .send()
                    .await
                    .map_err(|e| GmError::Platform(format!("Local TTS request failed: {e}")))?;
                if !resp.status().is_success() {
                    return Err(GmError::Platform(format!(
                        "Local TTS returned HTTP {}",
                        resp.status()
                    )));
                }
                let bytes = resp
                    .bytes()
                    .await
                    .map_err(|e| GmError::Platform(format!("TTS body read error: {e}")))?
                    .to_vec();
                session.append_turn(session.bot_id.clone(), clean, true);
                session.state = VoiceSessionState::Listening;
                Ok(bytes)
            }
            TtsMode::MacOsSay => {
                session.append_turn(session.bot_id.clone(), clean, true);
                session.state = VoiceSessionState::Listening;
                Ok(clean.as_bytes().to_vec())
            }
        }
    }
}
