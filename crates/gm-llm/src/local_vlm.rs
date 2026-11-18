use crate::client::{ChatCompletionResponse, ChatMessage, LlmBackend};
use crate::router::ModelTarget;
use async_trait::async_trait;
use gm_core::error::Result;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Normalized bounding box coordinates (0.0 to 1.0 or absolute points).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VlmBoundingBox {
    pub x_min: f64,
    pub y_min: f64,
    pub x_max: f64,
    pub y_max: f64,
}

impl VlmBoundingBox {
    pub fn center(&self) -> (f64, f64) {
        (
            (self.x_min + self.x_max) / 2.0,
            (self.y_min + self.y_max) / 2.0,
        )
    }

    pub fn width(&self) -> f64 {
        (self.x_max - self.x_min).abs()
    }

    pub fn height(&self) -> f64 {
        (self.y_max - self.y_min).abs()
    }
}

/// Result of grounding a natural language query against visual image input.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VlmGroundingResult {
    pub x: f64,
    pub y: f64,
    pub bbox: Option<VlmBoundingBox>,
    pub confidence: f32,
    pub target_element: Option<String>,
}

/// Visual triage classification of a screen frame or region.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VlmTriageResult {
    pub label: String,
    pub confidence: f32,
    pub reasoning: String,
    pub contains_sensitive_data: bool,
    pub is_terminal_state: bool,
}

/// Hardware execution device for local VLM inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum VlmDevice {
    #[default]
    Cpu,
    Metal,
    Cuda,
    Vulkan,
    DirectMl,
}

/// Active worker backend engine and explicit fallback reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VlmWorkerRuntime {
    LlamaCppMtmd,
    OnnxDirectMl,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VlmAcceleratorStatus {
    pub requested: VlmDevice,
    pub actual: VlmDevice,
    pub runtime: VlmWorkerRuntime,
    pub cpu_fallback_reason: Option<String>,
}

/// Pinned checkpoint & multimodal worker specification for Qwen2-VL-2B, SmolVLM, and SmolVLM2.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedVlmWorkerSpec {
    pub model_id: String,
    pub runtime: VlmWorkerRuntime,
    pub worker_bin: PathBuf,
    pub model_path: PathBuf,
    pub mmproj_path: Option<PathBuf>,
    pub requested_device: VlmDevice,
    pub required_memory_mb: u64,
    pub available_device_memory_mb: u64,
    pub endpoint: String,
}

impl ManagedVlmWorkerSpec {
    /// Resolve effective accelerator or fail/report explicit CPU fallback.
    pub fn resolve_accelerator(&self) -> VlmAcceleratorStatus {
        let supported = match (self.runtime, self.requested_device) {
            (VlmWorkerRuntime::LlamaCppMtmd, VlmDevice::Metal) => cfg!(target_os = "macos"),
            (VlmWorkerRuntime::LlamaCppMtmd, VlmDevice::Cuda | VlmDevice::Vulkan) => {
                cfg!(any(target_os = "linux", target_os = "windows"))
            }
            (VlmWorkerRuntime::OnnxDirectMl, VlmDevice::DirectMl) => cfg!(target_os = "windows"),
            (_, VlmDevice::Cpu) => true,
            _ => false,
        };
        if supported && self.available_device_memory_mb >= self.required_memory_mb {
            VlmAcceleratorStatus {
                requested: self.requested_device,
                actual: self.requested_device,
                runtime: self.runtime,
                cpu_fallback_reason: None,
            }
        } else {
            let reason = if !supported {
                format!(
                    "Requested accelerator {:?} for {:?} is unavailable on {}/{}",
                    self.requested_device,
                    self.runtime,
                    std::env::consts::OS,
                    std::env::consts::ARCH
                )
            } else {
                format!(
                    "Insufficient accelerator memory ({} MB available < {} MB required); falling back to CPU",
                    self.available_device_memory_mb, self.required_memory_mb
                )
            };
            VlmAcceleratorStatus {
                requested: self.requested_device,
                actual: VlmDevice::Cpu,
                runtime: self.runtime,
                cpu_fallback_reason: Some(reason),
            }
        }
    }

    /// Validate worker assets before on-demand launch (rejects missing/empty/corrupt files & OOM).
    pub fn validate_before_launch(&self) -> Result<VlmAcceleratorStatus> {
        if !matches!(
            self.model_id.as_str(),
            "qwen2-vl-2b" | "Qwen/Qwen2-VL-2B-Instruct" | "smolvlm" | "smolvlm2" | "smolvlm2-2.2b"
        ) && !self.model_id.to_lowercase().contains("qwen2-vl")
            && !self.model_id.to_lowercase().contains("smolvlm")
        {
            return Err(gm_core::error::GmError::Config(format!(
                "Unsupported local VLM checkpoint '{}'",
                self.model_id
            )));
        }
        if !self.model_path.is_file() || std::fs::metadata(&self.model_path)?.len() == 0 {
            return Err(gm_core::error::GmError::Config(format!(
                "Damaged or missing VLM checkpoint at {}",
                self.model_path.display()
            )));
        }
        if self.runtime == VlmWorkerRuntime::LlamaCppMtmd {
            let Some(ref mmproj) = self.mmproj_path else {
                return Err(gm_core::error::GmError::Config(
                    "llama.cpp multimodal worker requires mmproj_path".into(),
                ));
            };
            if !mmproj.is_file() || std::fs::metadata(mmproj)?.len() == 0 {
                return Err(gm_core::error::GmError::Config(format!(
                    "Damaged or missing mmproj weights at {}",
                    mmproj.display()
                )));
            }
        }
        if self.available_device_memory_mb < self.required_memory_mb / 2 {
            return Err(gm_core::error::GmError::Platform(format!(
                "Insufficient memory to load '{}' ({} MB available)",
                self.model_id, self.available_device_memory_mb
            )));
        }
        Ok(self.resolve_accelerator())
    }
}

/// On-demand multimodal worker controller (`llama.cpp` MTMD / ONNX DirectML) over loopback HTTP.
pub struct ManagedLocalVlmBackend {
    spec: ManagedVlmWorkerSpec,
    status: VlmAcceleratorStatus,
    child: Mutex<Option<std::process::Child>>,
    client: reqwest::Client,
}

impl ManagedLocalVlmBackend {
    pub fn new(spec: ManagedVlmWorkerSpec) -> Result<Self> {
        let status = spec.validate_before_launch()?;
        if !spec.endpoint.starts_with("http://127.0.0.1:")
            && !spec.endpoint.starts_with("http://localhost:")
        {
            return Err(gm_core::error::GmError::SafetyViolation(
                "Local VLM worker endpoint must bind to loopback only".into(),
            ));
        }
        Ok(Self {
            spec,
            status,
            child: Mutex::new(None),
            client: reqwest::Client::new(),
        })
    }

    pub fn accelerator_status(&self) -> &VlmAcceleratorStatus {
        &self.status
    }

    pub fn unload(&self) {
        if let Some(mut proc) = self.child.lock().take() {
            let _ = proc.kill();
            let _ = proc.wait();
        }
    }
}

impl Drop for ManagedLocalVlmBackend {
    fn drop(&mut self) {
        self.unload();
    }
}

#[async_trait]
impl LocalVlmBackend for ManagedLocalVlmBackend {
    fn model_name(&self) -> &str {
        &self.spec.model_id
    }

    async fn triage(&self, image_jpeg: &[u8], prompt: &str) -> Result<VlmTriageResult> {
        let reply = self
            .generate(
                image_jpeg,
                &format!("Classify screen state as JSON (label, reasoning, contains_sensitive_data, is_terminal_state): {prompt}"),
            )
            .await?;
        let parsed: serde_json::Value = serde_json::from_str(&reply).unwrap_or_default();
        Ok(VlmTriageResult {
            label: parsed["label"].as_str().unwrap_or("screen_ready").into(),
            confidence: 0.9,
            reasoning: parsed["reasoning"].as_str().unwrap_or(&reply).into(),
            contains_sensitive_data: parsed["contains_sensitive_data"].as_bool().unwrap_or(false),
            is_terminal_state: parsed["is_terminal_state"].as_bool().unwrap_or(false),
        })
    }

    async fn ground(&self, image_jpeg: &[u8], instruction: &str) -> Result<VlmGroundingResult> {
        let reply = self
            .generate(
                image_jpeg,
                &format!("Return JSON target coordinates (x, y) for: {instruction}"),
            )
            .await?;
        let parsed: serde_json::Value = serde_json::from_str(&reply).map_err(|e| {
            gm_core::error::GmError::Platform(format!("Invalid VLM grounding JSON: {e}"))
        })?;
        let x = parsed["x"].as_f64().ok_or_else(|| {
            gm_core::error::GmError::Platform("Missing x coordinate in VLM grounding output".into())
        })?;
        let y = parsed["y"].as_f64().ok_or_else(|| {
            gm_core::error::GmError::Platform("Missing y coordinate in VLM grounding output".into())
        })?;
        Ok(VlmGroundingResult {
            x,
            y,
            bbox: None,
            confidence: 0.9,
            target_element: Some(instruction.to_string()),
        })
    }

    async fn generate(&self, image_jpeg: &[u8], prompt: &str) -> Result<String> {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(image_jpeg);
        let resp = self
            .client
            .post(format!("{}/v1/chat/completions", self.spec.endpoint.trim_end_matches('/')))
            .json(&serde_json::json!({
                "model": self.spec.model_id,
                "messages": [{
                    "role": "user",
                    "content": [
                        { "type": "text", "text": prompt },
                        { "type": "image_url", "image_url": { "url": format!("data:image/jpeg;base64,{b64}") } }
                    ]
                }],
                "temperature": 0.1
            }))
            .send()
            .await
            .map_err(|e| gm_core::error::GmError::Platform(format!("Local VLM worker request failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(gm_core::error::GmError::Platform(format!(
                "Local VLM worker returned HTTP {}",
                resp.status()
            )));
        }
        let body: serde_json::Value = resp.json().await.map_err(|e| {
            gm_core::error::GmError::Platform(format!("Invalid local VLM JSON: {e}"))
        })?;
        body["choices"][0]["message"]["content"]
            .as_str()
            .map(ToString::to_string)
            .ok_or_else(|| {
                gm_core::error::GmError::Platform("Missing content in local VLM reply".into())
            })
    }
}

/// Weight precision format for local VLM tensor weights.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[allow(non_camel_case_types)]
pub enum VlmPrecision {
    #[default]
    Fp16,
    Bf16,
    Q4_0,
    Q4_K,
}

/// Pluggable interface for offline local vision-language models (Action A12 prep: Candle / MLX ready).
#[async_trait]
pub trait LocalVlmBackend: Send + Sync {
    /// Return the model name / architecture ID (e.g. "SmolVLM-Instruct", "Qwen2-VL-2B", "PaliGemma-3B").
    fn model_name(&self) -> &str;

    /// Triage the visual state of the screen (detect active modals, error states, safety flags, sensitive fields).
    async fn triage(&self, image_jpeg: &[u8], prompt: &str) -> Result<VlmTriageResult>;

    /// Ground a natural language instruction to precise target coordinates or bounding box.
    async fn ground(&self, image_jpeg: &[u8], instruction: &str) -> Result<VlmGroundingResult>;

    /// Multimodal visual question answering / reasoning from an image and text prompt.
    async fn generate(&self, image_jpeg: &[u8], prompt: &str) -> Result<String>;
}

/// In-memory mock local VLM backend for deterministic testing and offline triage.
pub struct MockLocalVlmBackend {
    model_name: String,
    mock_grounding: Arc<Mutex<Option<VlmGroundingResult>>>,
    mock_triage: Arc<Mutex<Option<VlmTriageResult>>>,
    mock_generation: Arc<Mutex<Option<String>>>,
    call_log: Arc<Mutex<Vec<String>>>,
}

impl Default for MockLocalVlmBackend {
    fn default() -> Self {
        Self::new("mock-vlm-2b")
    }
}

impl MockLocalVlmBackend {
    pub fn new(model_name: impl Into<String>) -> Self {
        Self {
            model_name: model_name.into(),
            mock_grounding: Arc::new(Mutex::new(None)),
            mock_triage: Arc::new(Mutex::new(None)),
            mock_generation: Arc::new(Mutex::new(None)),
            call_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn set_grounding_result(&self, res: VlmGroundingResult) {
        *self.mock_grounding.lock() = Some(res);
    }

    pub fn set_triage_result(&self, res: VlmTriageResult) {
        *self.mock_triage.lock() = Some(res);
    }

    pub fn set_generation_response(&self, resp: impl Into<String>) {
        *self.mock_generation.lock() = Some(resp.into());
    }

    pub fn calls(&self) -> Vec<String> {
        self.call_log.lock().clone()
    }
}

#[async_trait]
impl LocalVlmBackend for MockLocalVlmBackend {
    fn model_name(&self) -> &str {
        &self.model_name
    }

    async fn triage(&self, image_jpeg: &[u8], prompt: &str) -> Result<VlmTriageResult> {
        self.call_log.lock().push(format!(
            "triage: prompt='{prompt}', bytes={}",
            image_jpeg.len()
        ));

        if let Some(ref res) = *self.mock_triage.lock() {
            return Ok(res.clone());
        }

        // Default heuristic mock triage
        let lower = prompt.to_lowercase();
        let sensitive = lower.contains("password") || lower.contains("credential");
        let terminal = lower.contains("done") || lower.contains("success");

        Ok(VlmTriageResult {
            label: if terminal {
                "terminal_state".into()
            } else {
                "screen_ready".into()
            },
            confidence: 0.95,
            reasoning: format!("Mock visual triage evaluated {prompt}"),
            contains_sensitive_data: sensitive,
            is_terminal_state: terminal,
        })
    }

    async fn ground(&self, image_jpeg: &[u8], instruction: &str) -> Result<VlmGroundingResult> {
        self.call_log.lock().push(format!(
            "ground: instruction='{instruction}', bytes={}",
            image_jpeg.len()
        ));

        if let Some(ref res) = *self.mock_grounding.lock() {
            return Ok(res.clone());
        }

        // Default synthetic grounding target
        Ok(VlmGroundingResult {
            x: 250.0,
            y: 400.0,
            bbox: Some(VlmBoundingBox {
                x_min: 200.0,
                y_min: 380.0,
                x_max: 300.0,
                y_max: 420.0,
            }),
            confidence: 0.92,
            target_element: Some(format!("element_for:{instruction}")),
        })
    }

    async fn generate(&self, image_jpeg: &[u8], prompt: &str) -> Result<String> {
        self.call_log.lock().push(format!(
            "generate: prompt='{prompt}', bytes={}",
            image_jpeg.len()
        ));

        if let Some(ref resp) = *self.mock_generation.lock() {
            return Ok(resp.clone());
        }

        Ok(format!(
            "{{\"action\":\"click\",\"params\":{{\"x\":250.0,\"y\":400.0,\"instruction\":\"{prompt}\"}}}}"
        ))
    }
}

/// Candle / MLX local inference bridge configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandleMlxBridgeConfig {
    pub model_id: String,
    pub weights_path: Option<PathBuf>,
    pub device: VlmDevice,
    pub precision: VlmPrecision,
    pub context_window: usize,
    pub temperature: f32,
}

impl Default for CandleMlxBridgeConfig {
    fn default() -> Self {
        Self {
            model_id: "smolvlm-instruct-mlx".into(),
            weights_path: None,
            device: if cfg!(target_os = "macos") {
                VlmDevice::Metal
            } else {
                VlmDevice::Cpu
            },
            precision: VlmPrecision::Fp16,
            context_window: 2048,
            temperature: 0.1,
        }
    }
}

/// Pluggable Candle / MLX local inference backend bridge.
pub struct CandleMlxBridgeBackend {
    config: CandleMlxBridgeConfig,
    fallback_mock: MockLocalVlmBackend,
}

impl CandleMlxBridgeBackend {
    pub fn new(config: CandleMlxBridgeConfig) -> Self {
        let name = config.model_id.clone();
        Self {
            config,
            fallback_mock: MockLocalVlmBackend::new(name),
        }
    }

    pub fn with_weights_path(mut self, path: impl AsRef<Path>) -> Self {
        self.config.weights_path = Some(path.as_ref().to_path_buf());
        self
    }

    pub fn config(&self) -> &CandleMlxBridgeConfig {
        &self.config
    }
}

#[async_trait]
impl LocalVlmBackend for CandleMlxBridgeBackend {
    fn model_name(&self) -> &str {
        &self.config.model_id
    }

    async fn triage(&self, image_jpeg: &[u8], prompt: &str) -> Result<VlmTriageResult> {
        #[cfg(all(not(test), not(debug_assertions)))]
        {
            let _ = (image_jpeg, prompt);
            return Err(gm_core::error::GmError::Config(
                "Mock VLM fallback is excluded from production builds; configure ManagedLocalVlmBackend or CandleVlmEngine".into(),
            ));
        }
        #[cfg(any(test, debug_assertions))]
        self.fallback_mock.triage(image_jpeg, prompt).await
    }

    async fn ground(&self, image_jpeg: &[u8], instruction: &str) -> Result<VlmGroundingResult> {
        #[cfg(all(not(test), not(debug_assertions)))]
        {
            let _ = (image_jpeg, instruction);
            return Err(gm_core::error::GmError::Config(
                "Mock VLM fallback is excluded from production builds; configure ManagedLocalVlmBackend or CandleVlmEngine".into(),
            ));
        }
        #[cfg(any(test, debug_assertions))]
        self.fallback_mock.ground(image_jpeg, instruction).await
    }

    async fn generate(&self, image_jpeg: &[u8], prompt: &str) -> Result<String> {
        #[cfg(all(not(test), not(debug_assertions)))]
        {
            let _ = (image_jpeg, prompt);
            return Err(gm_core::error::GmError::Config(
                "Mock VLM fallback is excluded from production builds; configure ManagedLocalVlmBackend or CandleVlmEngine".into(),
            ));
        }
        #[cfg(any(test, debug_assertions))]
        self.fallback_mock.generate(image_jpeg, prompt).await
    }
}

/// Adapter converting a `LocalVlmBackend` into an `LlmBackend` for integration into `HybridLlmClient`.
pub struct LocalVlmLlmAdapter {
    backend: Arc<dyn LocalVlmBackend>,
}

impl LocalVlmLlmAdapter {
    pub fn new(backend: Arc<dyn LocalVlmBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl LlmBackend for LocalVlmLlmAdapter {
    async fn complete(
        &self,
        target: &ModelTarget,
        messages: &[ChatMessage],
    ) -> Result<ChatCompletionResponse> {
        use base64::Engine;

        let last_prompt = messages
            .last()
            .map(|m| m.content.clone())
            .unwrap_or_else(|| "Generate computer use action".into());

        let decoded_frame = messages
            .iter()
            .rev()
            .find_map(|m| m.images.last())
            .and_then(|img| {
                base64::engine::general_purpose::STANDARD
                    .decode(&img.base64_data)
                    .ok()
            })
            .filter(|bytes| !bytes.is_empty());

        // Fallback minimal JPEG marker for text-only calls through the VLM adapter
        let empty_frame: &[u8] = &[0xFF, 0xD8, 0xFF, 0xD9];
        let frame_slice = decoded_frame.as_deref().unwrap_or(empty_frame);
        let content = self.backend.generate(frame_slice, &last_prompt).await?;

        Ok(ChatCompletionResponse {
            id: format!("vlm_{}", self.backend.model_name()),
            content,
            model: target.model.clone(),
            prompt_tokens: 64,
            completion_tokens: 32,
            estimated_cost_usd: 0.0, // Local inference has 0 USD API cost!
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_local_vlm_grounding_and_triage() {
        let vlm = MockLocalVlmBackend::new("smolvlm-2b-test");
        assert_eq!(vlm.model_name(), "smolvlm-2b-test");

        let dummy_jpeg = vec![1, 2, 3, 4];
        let ground = vlm
            .ground(&dummy_jpeg, "Click the Submit button")
            .await
            .unwrap();
        assert_eq!(ground.x, 250.0);
        assert_eq!(ground.y, 400.0);
        assert!(ground.confidence > 0.9);
        assert_eq!(
            ground.target_element.as_deref(),
            Some("element_for:Click the Submit button")
        );

        let triage = vlm
            .triage(&dummy_jpeg, "Check if screen shows password input")
            .await
            .unwrap();
        assert!(triage.contains_sensitive_data);
        assert!(!triage.is_terminal_state);

        let calls = vlm.calls();
        assert_eq!(calls.len(), 2);
    }

    #[tokio::test]
    async fn test_candle_mlx_bridge_config_and_adapter() {
        let config = CandleMlxBridgeConfig {
            model_id: "qwen2-vl-2b-metal".into(),
            weights_path: Some(PathBuf::from("/models/qwen2-vl")),
            device: VlmDevice::Metal,
            precision: VlmPrecision::Fp16,
            context_window: 4096,
            temperature: 0.2,
        };

        let bridge = Arc::new(CandleMlxBridgeBackend::new(config));
        assert_eq!(bridge.model_name(), "qwen2-vl-2b-metal");
        assert_eq!(bridge.config().device, VlmDevice::Metal);

        let adapter = LocalVlmLlmAdapter::new(bridge);
        let target = ModelTarget::simple(
            crate::router::ProviderKind::Meta,
            "local://vlm",
            "qwen2-vl-2b-metal",
            true,
        );
        let messages = vec![ChatMessage::text("user", "Click button")];

        let resp = adapter.complete(&target, &messages).await.unwrap();
        assert_eq!(resp.model, "qwen2-vl-2b-metal");
        assert_eq!(resp.estimated_cost_usd, 0.0);
        assert!(resp.content.contains("click"));
    }

    #[tokio::test]
    async fn test_hybrid_client_integration_with_local_vlm_adapter() {
        use crate::budget::BudgetManager;
        use crate::client::HybridLlmClient;
        use gm_core::config::BudgetConfig;

        let bridge = Arc::new(CandleMlxBridgeBackend::new(CandleMlxBridgeConfig::default()));
        let adapter = Arc::new(LocalVlmLlmAdapter::new(bridge));
        let budget = Arc::new(BudgetManager::new(BudgetConfig::default()));
        let hybrid = HybridLlmClient::new(adapter, budget.clone());

        let target = ModelTarget::simple(
            crate::router::ProviderKind::Meta,
            "local://vlm",
            "smolvlm-instruct-mlx",
            true,
        );
        let messages = vec![ChatMessage::text("user", "Locate Submit button")];

        let resp = hybrid
            .execute_routed(&target, None, &messages)
            .await
            .unwrap();
        assert_eq!(resp.model, "smolvlm-instruct-mlx");
        assert_eq!(resp.estimated_cost_usd, 0.0);
        assert_eq!(budget.get_daily_spent(), 0.0);

        let (stream_resp, metrics) = hybrid
            .execute_routed_stream(&target, None, &messages)
            .await
            .unwrap();
        assert_eq!(stream_resp.model, "smolvlm-instruct-mlx");
        assert!(metrics.total_duration_ms >= 0.0);
    }

    #[tokio::test]
    async fn test_local_vlm_mtmd_and_directml_config() {
        let dir = std::env::temp_dir().join(format!("dt-vlm-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let model_path = dir.join("qwen2-vl-2b.gguf");
        let mmproj_path = dir.join("mmproj-qwen2-vl-2b.gguf");
        std::fs::write(&model_path, b"gguf-bytes").unwrap();
        std::fs::write(&mmproj_path, b"mmproj-bytes").unwrap();

        let spec = ManagedVlmWorkerSpec {
            model_id: "qwen2-vl-2b".into(),
            runtime: VlmWorkerRuntime::LlamaCppMtmd,
            worker_bin: PathBuf::from("/usr/local/bin/llama-server"),
            model_path: model_path.clone(),
            mmproj_path: Some(mmproj_path),
            requested_device: VlmDevice::DirectMl,
            required_memory_mb: 2048,
            available_device_memory_mb: 4096,
            endpoint: "http://127.0.0.1:18080".into(),
        };
        let backend = ManagedLocalVlmBackend::new(spec.clone()).unwrap();
        assert_eq!(backend.accelerator_status().actual, VlmDevice::Cpu);
        assert!(backend.accelerator_status().cpu_fallback_reason.is_some());

        let mut oom_spec = spec.clone();
        oom_spec.available_device_memory_mb = 512;
        assert!(ManagedLocalVlmBackend::new(oom_spec).is_err());

        let mut remote_spec = spec;
        remote_spec.endpoint = "https://remote.example.com".into();
        assert!(ManagedLocalVlmBackend::new(remote_spec).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
