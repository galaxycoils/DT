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

pub const MAX_AUDIO_BUFFER_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum VoiceSessionState {
    #[default]
    Idle,
    Listening,
    Paused,
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
    pub input_device: String,
    pub audio_buffer_bytes: usize,
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
            input_device: "default".into(),
            audio_buffer_bytes: 0,
            transcript: Vec::new(),
            updated_at: Utc::now().to_rfc3339(),
        }
    }

    pub fn acknowledge_consent(&mut self) {
        self.consent_acknowledged = true;
        self.updated_at = Utc::now().to_rfc3339();
    }

    pub fn revoke_consent(&mut self) {
        self.consent_acknowledged = false;
        self.audio_buffer_bytes = 0;
        self.state = VoiceSessionState::Ended;
        self.updated_at = Utc::now().to_rfc3339();
    }

    pub fn pause(&mut self) {
        self.state = VoiceSessionState::Paused;
        self.updated_at = Utc::now().to_rfc3339();
    }

    pub fn resume(&mut self) {
        if self.consent_acknowledged {
            self.state = VoiceSessionState::Listening;
        }
        self.updated_at = Utc::now().to_rfc3339();
    }

    pub fn set_input_device(&mut self, device: impl Into<String>) {
        self.input_device = device.into();
        self.audio_buffer_bytes = 0;
        self.updated_at = Utc::now().to_rfc3339();
    }

    pub fn request_barge_in(&mut self) {
        self.barge_in_requested = true;
        if self.state == VoiceSessionState::Speaking {
            self.state = VoiceSessionState::Listening;
        }
        self.updated_at = Utc::now().to_rfc3339();
    }

    pub fn append_turn(
        &mut self,
        speaker: impl Into<String>,
        text: impl Into<String>,
        is_final: bool,
    ) {
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
    ipc_token: Option<String>,
}

impl LocalVoiceEngine {
    pub fn new(cfg: VoiceConfig) -> Result<Self> {
        let check_loopback = |url: &str| -> Result<()> {
            if url.trim().is_empty() {
                return Ok(());
            }
            let _ = extract_provider_origin(url)?;
            let lower = url.trim().to_ascii_lowercase();
            if !lower.starts_with("http://127.0.0.1:")
                && !lower.starts_with("http://localhost:")
                && !lower.starts_with("http://[::1]:")
            {
                return Err(GmError::SafetyViolation(format!(
                    "Managed voice runtime endpoint must be loopback-only: {url}"
                )));
            }
            Ok(())
        };
        check_loopback(&cfg.stt_endpoint)?;
        check_loopback(&cfg.tts_endpoint)?;
        Ok(Self {
            cfg,
            client: reqwest::Client::new(),
            ipc_token: None,
        })
    }

    pub fn with_ipc_token(mut self, token: impl Into<String>) -> Self {
        self.ipc_token = Some(token.into());
        self
    }

    pub async fn transcribe_wav(
        &self,
        session: &mut VoiceSession,
        wav_bytes: &[u8],
    ) -> Result<String> {
        if !session.consent_acknowledged {
            return Err(GmError::SafetyViolation(
                "Voice transcription requires explicit participant consent first".into(),
            ));
        }
        if session.state == VoiceSessionState::Paused || session.state == VoiceSessionState::Ended {
            return Err(GmError::SafetyViolation(
                "Voice session is paused or ended; cannot capture audio".into(),
            ));
        }
        if wav_bytes.len() > MAX_AUDIO_BUFFER_BYTES {
            return Err(GmError::SafetyViolation(format!(
                "Audio buffer ({} bytes) exceeds bounded limit ({MAX_AUDIO_BUFFER_BYTES} bytes)",
                wav_bytes.len()
            )));
        }
        session.audio_buffer_bytes = wav_bytes.len();
        if self.cfg.stt_mode == SttMode::Disabled {
            return Err(GmError::Config("Local STT is disabled".into()));
        }
        session.state = VoiceSessionState::Transcribing;
        let mut req = self
            .client
            .post(&self.cfg.stt_endpoint)
            .header("content-type", "audio/wav")
            .body(wav_bytes.to_vec());
        if let Some(ref tok) = self.ipc_token {
            req = req.bearer_auth(tok);
        }
        let resp = req
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
        let text = body["text"].as_str().unwrap_or("").trim().to_string();
        if !text.is_empty() {
            session.append_turn("caller", &text, true);
        }
        session.state = VoiceSessionState::Listening;
        Ok(text)
    }

    pub async fn synthesize_speech(
        &self,
        session: &mut VoiceSession,
        text: &str,
    ) -> Result<Vec<u8>> {
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

/// Managed Whisper STT & local TTS runtime supervisor with start/health/stop lifecycle.
pub struct ManagedVoiceRuntime {
    pub kind: String,
    pub endpoint: String,
    pub auth_token: String,
    child: parking_lot::Mutex<Option<std::process::Child>>,
}

impl ManagedVoiceRuntime {
    pub fn new(kind: impl Into<String>, endpoint: impl Into<String>) -> Result<Self> {
        let ep = endpoint.into();
        let lower = ep.to_ascii_lowercase();
        if !lower.starts_with("http://127.0.0.1:") && !lower.starts_with("http://localhost:") {
            return Err(GmError::SafetyViolation(
                "Managed voice runtime must bind to loopback only".into(),
            ));
        }
        Ok(Self {
            kind: kind.into(),
            endpoint: ep,
            auth_token: format!("dtvoice-{}", uuid::Uuid::new_v4().simple()),
            child: parking_lot::Mutex::new(None),
        })
    }

    pub fn start_command(&self, bin: &std::path::Path, args: &[&str]) -> Result<()> {
        if !bin.is_file() {
            return Err(GmError::Config(format!(
                "Voice runtime binary not found at {}",
                bin.display()
            )));
        }
        let child = std::process::Command::new(bin)
            .args(args)
            .env("DT_VOICE_IPC_TOKEN", &self.auth_token)
            .spawn()?;
        *self.child.lock() = Some(child);
        Ok(())
    }

    pub async fn health_check(&self, client: &reqwest::Client) -> bool {
        let url = format!("{}/health", self.endpoint.trim_end_matches('/'));
        client
            .get(url)
            .bearer_auth(&self.auth_token)
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }

    pub fn stop(&self) {
        if let Some(mut proc) = self.child.lock().take() {
            let _ = proc.kill();
            let _ = proc.wait();
        }
    }
}

impl Drop for ManagedVoiceRuntime {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_managed_voice_lifecycle_and_bounded_buffers() {
        assert!(ManagedVoiceRuntime::new("whisper", "https://remote.example.com").is_err());
        let rt = ManagedVoiceRuntime::new("whisper", "http://127.0.0.1:18178").unwrap();
        assert!(rt.auth_token.starts_with("dtvoice-"));
        rt.stop();

        let engine = LocalVoiceEngine::new(VoiceConfig::default())
            .unwrap()
            .with_ipc_token(&rt.auth_token);
        let mut session = VoiceSession::start("muse");
        // Without consent -> blocked.
        assert!(engine.transcribe_wav(&mut session, b"RIFF").await.is_err());
        session.acknowledge_consent();
        // Oversized audio buffer -> blocked.
        let oversized = vec![0u8; MAX_AUDIO_BUFFER_BYTES + 1];
        assert!(
            engine
                .transcribe_wav(&mut session, &oversized)
                .await
                .is_err()
        );
        // Device change resets buffer; revoke_consent ends session immediately.
        session.set_input_device("USB Mic");
        assert_eq!(session.input_device, "USB Mic");
        session.pause();
        assert_eq!(session.state, VoiceSessionState::Paused);
        session.revoke_consent();
        assert!(!session.consent_acknowledged);
        assert_eq!(session.state, VoiceSessionState::Ended);
    }
}
