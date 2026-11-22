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

/// Shared daemon/companion controller for interactive Voice & Dictation sessions.
/// Enforces explicit microphone consent, bounded audio buffers, local-only endpoints,
/// and immediate child-process teardown on stop, barge-in, or consent revocation.
#[derive(Clone)]
pub struct VoiceSessionManager {
    storage_dir: std::path::PathBuf,
    cfg: VoiceConfig,
    session: std::sync::Arc<parking_lot::Mutex<Option<VoiceSession>>>,
    mode: std::sync::Arc<parking_lot::Mutex<String>>,
    last_error: std::sync::Arc<parking_lot::Mutex<Option<String>>>,
    playback_child: std::sync::Arc<parking_lot::Mutex<Option<std::process::Child>>>,
}

impl VoiceSessionManager {
    pub fn new(storage_dir: std::path::PathBuf) -> Self {
        Self::with_config(storage_dir, VoiceConfig::default())
    }

    pub fn with_config(storage_dir: std::path::PathBuf, cfg: VoiceConfig) -> Self {
        Self {
            storage_dir,
            cfg,
            session: std::sync::Arc::new(parking_lot::Mutex::new(None)),
            mode: std::sync::Arc::new(parking_lot::Mutex::new("idle".into())),
            last_error: std::sync::Arc::new(parking_lot::Mutex::new(None)),
            playback_child: std::sync::Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    fn find_installed_component(&self, id: &str) -> Option<std::path::PathBuf> {
        let dir = self.storage_dir.join("components").join(id);
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let candidate = entry.path().join("payload.bin");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }

    fn kill_playback(&self) {
        if let Some(mut child) = self.playback_child.lock().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    pub fn status(&self) -> serde_json::Value {
        let sess = self.session.lock().clone();
        let mode = self.mode.lock().clone();
        let last_error = self.last_error.lock().clone();
        let stt_model = self.find_installed_component("whisper-stt-base-en");
        let tts_model = self.find_installed_component("speech_tts");
        let active = sess.as_ref().is_some_and(|s| {
            !matches!(s.state, VoiceSessionState::Idle | VoiceSessionState::Ended)
        });
        serde_json::json!({
            "active": active,
            "mode": if active { mode } else { "idle".into() },
            "state": sess.as_ref().map(|s| s.state).unwrap_or(VoiceSessionState::Idle),
            "session_id": sess.as_ref().map(|s| s.session_id.clone()),
            "bot_id": sess.as_ref().map(|s| s.bot_id.clone()),
            "consent_acknowledged": sess.as_ref().is_some_and(|s| s.consent_acknowledged),
            "input_device": sess.as_ref().map(|s| s.input_device.clone()).unwrap_or_else(|| "default".into()),
            "transcript": sess.as_ref().map(|s| s.transcript.clone()).unwrap_or_default(),
            "last_error": last_error,
            "stt_installed": stt_model.is_some(),
            "tts_installed": tts_model.is_some(),
            "stt_model_path": stt_model.as_ref().map(|p| p.display().to_string()),
            "tts_model_path": tts_model.as_ref().map(|p| p.display().to_string()),
            "stt_endpoint": self.cfg.stt_endpoint,
            "tts_endpoint": self.cfg.tts_endpoint,
            "consent_prompt": self.cfg.consent_prompt,
            "install_component": if stt_model.is_none() { Some("whisper-stt-base-en") } else { None },
        })
    }

    pub fn start(&self, bot_id: &str, mode: &str) -> serde_json::Value {
        self.kill_playback();
        let clean_mode = if mode.eq_ignore_ascii_case("dictate") {
            "dictate"
        } else {
            "voice"
        };
        let bot = if bot_id.trim().is_empty() {
            "muse"
        } else {
            bot_id.trim()
        };
        let mut sess = VoiceSession::start(bot);
        // Preserve explicit consent requirement on every fresh session.
        sess.consent_acknowledged = false;
        *self.session.lock() = Some(sess);
        *self.mode.lock() = clean_mode.into();
        *self.last_error.lock() = None;
        self.status()
    }

    pub fn set_consent(&self, granted: bool, bot_id: Option<&str>) -> serde_json::Value {
        if !granted {
            self.kill_playback();
            if let Some(ref mut sess) = *self.session.lock() {
                sess.revoke_consent();
            }
            *self.mode.lock() = "idle".into();
            *self.last_error.lock() = None;
            return self.status();
        }
        let mut guard = self.session.lock();
        if guard.is_none()
            || matches!(
                guard.as_ref().map(|s| s.state),
                Some(VoiceSessionState::Ended | VoiceSessionState::Idle)
            )
        {
            *guard = Some(VoiceSession::start(bot_id.unwrap_or("muse")));
            *self.mode.lock() = "voice".into();
        }
        if let Some(ref mut sess) = *guard {
            sess.acknowledge_consent();
            sess.resume();
        }
        drop(guard);
        *self.last_error.lock() = None;
        self.status()
    }

    pub fn set_input_device(&self, device: &str) -> serde_json::Value {
        if let Some(ref mut sess) = *self.session.lock() {
            sess.set_input_device(device.trim());
        }
        self.status()
    }

    pub fn barge_in(&self) -> serde_json::Value {
        self.kill_playback();
        if let Some(ref mut sess) = *self.session.lock() {
            sess.request_barge_in();
        }
        self.status()
    }

    pub fn stop(&self) -> serde_json::Value {
        self.kill_playback();
        if let Some(ref mut sess) = *self.session.lock() {
            sess.end();
        }
        *self.mode.lock() = "idle".into();
        self.status()
    }

    pub async fn transcribe_wav(&self, wav_bytes: &[u8]) -> Result<serde_json::Value> {
        let mut sess = self.session.lock().clone().ok_or_else(|| {
            GmError::SafetyViolation("Start a voice or dictation session first".into())
        })?;
        if !sess.consent_acknowledged {
            let msg = "Microphone consent is required before recording or transcribing audio";
            *self.last_error.lock() = Some(msg.into());
            return Err(GmError::SafetyViolation(msg.into()));
        }
        if wav_bytes.is_empty() {
            let msg = "No audio captured";
            *self.last_error.lock() = Some(msg.into());
            return Err(GmError::Config(msg.into()));
        }
        if wav_bytes.len() > MAX_AUDIO_BUFFER_BYTES {
            let msg = format!(
                "Audio buffer ({} bytes) exceeds bounded limit ({MAX_AUDIO_BUFFER_BYTES} bytes)",
                wav_bytes.len()
            );
            *self.last_error.lock() = Some(msg.clone());
            return Err(GmError::SafetyViolation(msg));
        }

        let engine = LocalVoiceEngine::new(self.cfg.clone())?;
        match engine.transcribe_wav(&mut sess, wav_bytes).await {
            Ok(text) => {
                *self.session.lock() = Some(sess);
                *self.last_error.lock() = None;
                Ok(serde_json::json!({
                    "text": text,
                    "engine": "local_whisper_http",
                    "status": self.status(),
                }))
            }
            Err(http_err) => {
                // Fallback to installed `whisper-cli` + verified `whisper-stt-base-en` payload if available.
                if let Some(model_path) = self.find_installed_component("whisper-stt-base-en") {
                    let tmp_wav = std::env::temp_dir()
                        .join(format!("dt-voice-{}.wav", uuid::Uuid::new_v4().simple()));
                    std::fs::write(&tmp_wav, wav_bytes)?;
                    let output = std::process::Command::new("whisper-cli")
                        .arg("-m")
                        .arg(&model_path)
                        .arg("-f")
                        .arg(&tmp_wav)
                        .arg("-nt")
                        .output();
                    let _ = std::fs::remove_file(&tmp_wav);
                    if let Ok(out) = output
                        && out.status.success()
                    {
                        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
                        if !text.is_empty() {
                            sess.append_turn("caller", &text, true);
                        }
                        sess.state = VoiceSessionState::Listening;
                        *self.session.lock() = Some(sess);
                        *self.last_error.lock() = None;
                        return Ok(serde_json::json!({
                            "text": text,
                            "engine": "whisper_cli",
                            "status": self.status(),
                        }));
                    }
                }
                let hint = if self
                    .find_installed_component("whisper-stt-base-en")
                    .is_none()
                {
                    format!(
                        "Local speech runtime is not installed or running ({http_err}). Install 'whisper-stt-base-en' in Settings -> Local Components."
                    )
                } else {
                    format!(
                        "Whisper STT server at {} is not reachable ({http_err}). Start local Whisper or use browser speech recognition.",
                        self.cfg.stt_endpoint
                    )
                };
                sess.state = VoiceSessionState::Listening;
                *self.session.lock() = Some(sess);
                *self.last_error.lock() = Some(hint.clone());
                Err(GmError::Platform(hint))
            }
        }
    }

    pub async fn speak(&self, text: &str) -> Result<serde_json::Value> {
        let clean = text.trim();
        if clean.is_empty() {
            return Err(GmError::Config("Nothing to speak".into()));
        }
        let mut sess = self
            .session
            .lock()
            .clone()
            .unwrap_or_else(|| VoiceSession::start("muse"));
        self.kill_playback();
        let engine = LocalVoiceEngine::new(self.cfg.clone())?;
        if let Ok(audio_bytes) = engine.synthesize_speech(&mut sess, clean).await {
            *self.session.lock() = Some(sess);
            *self.last_error.lock() = None;
            return Ok(serde_json::json!({
                "spoken": true,
                "engine": "local_http_tts",
                "bytes": audio_bytes.len(),
                "status": self.status(),
            }));
        }

        #[cfg(target_os = "macos")]
        {
            if std::path::Path::new("/usr/bin/say").is_file() {
                let child = std::process::Command::new("/usr/bin/say")
                    .arg(clean)
                    .spawn()?;
                *self.playback_child.lock() = Some(child);
                sess.append_turn(sess.bot_id.clone(), clean, true);
                sess.state = VoiceSessionState::Speaking;
                *self.session.lock() = Some(sess);
                *self.last_error.lock() = None;
                return Ok(serde_json::json!({
                    "spoken": true,
                    "engine": "macos_say",
                    "status": self.status(),
                }));
            }
        }

        let hint = "Local TTS runtime is not running. Install 'speech_tts' in Settings -> Local Components or start the local TTS server.";
        *self.last_error.lock() = Some(hint.into());
        Err(GmError::Platform(hint.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_voice_session_manager_consent_and_stop() {
        let dir = std::env::temp_dir().join(format!("dt-voice-mgr-{}", uuid::Uuid::new_v4()));
        let mgr = VoiceSessionManager::new(dir.clone());
        let s0 = mgr.status();
        assert_eq!(s0["active"], false);
        assert_eq!(s0["consent_acknowledged"], false);

        let s1 = mgr.start("muse", "dictate");
        assert_eq!(s1["active"], true);
        assert_eq!(s1["mode"], "dictate");
        assert_eq!(s1["consent_acknowledged"], false);
        // Must fail without consent.
        assert!(mgr.transcribe_wav(b"RIFFtest").await.is_err());

        let s2 = mgr.set_consent(true, Some("muse"));
        assert_eq!(s2["consent_acknowledged"], true);
        // Oversized buffer rejected cleanly.
        let oversized = vec![0u8; MAX_AUDIO_BUFFER_BYTES + 1];
        assert!(mgr.transcribe_wav(&oversized).await.is_err());

        // Revoking consent ends session immediately.
        let s3 = mgr.set_consent(false, None);
        assert_eq!(s3["active"], false);
        assert_eq!(s3["consent_acknowledged"], false);
        let _ = std::fs::remove_dir_all(&dir);
    }

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
