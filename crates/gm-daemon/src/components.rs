//! Signed optional component catalog and resumable chunked payload delivery.
//! Large models, speech engines, VM images, and firmware are pinned by source, version,
//! architecture, size, license, and chunk hashes, and verified before activation.

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use gm_core::error::{GmError, Result};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const BUNDLED_COMPONENT_CATALOG_JSON: &str =
    include_str!("../../../docs/release/component-catalog.json");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComponentPart {
    pub index: usize,
    pub url: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    VlmModel,
    VlmWorker,
    SpeechStt,
    SpeechTts,
    VmImage,
    VmFirmware,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogComponent {
    pub id: String,
    pub kind: ComponentKind,
    pub version: String,
    #[serde(default)]
    pub revision: String,
    pub os: String,
    pub architecture: String,
    #[serde(default)]
    pub worker_compatibility: Vec<String>,
    #[serde(default)]
    pub includes: Vec<String>,
    pub total_size_bytes: u64,
    pub sha256: String,
    pub license: String,
    pub source: String,
    pub parts: Vec<ComponentPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComponentCatalog {
    pub schema: u32,
    pub sequence: u64,
    pub published_at: String,
    pub expires_at: String,
    pub origin: String,
    pub components: Vec<CatalogComponent>,
}

impl ComponentCatalog {
    pub fn bundled() -> Result<Self> {
        if let Ok(override_path) = std::env::var("DT_COMPONENT_CATALOG_PATH") {
            let trimmed = override_path.trim();
            if !trimmed.is_empty() {
                let raw = fs::read_to_string(trimmed).map_err(|e| {
                    GmError::Config(format!(
                        "Failed to read component catalog override '{trimmed}': {e}"
                    ))
                })?;
                return Self::parse_unverified(&raw);
            }
        }
        Self::parse_unverified(BUNDLED_COMPONENT_CATALOG_JSON)
    }

    pub fn parse_unverified(raw_json: &str) -> Result<Self> {
        let catalog: Self = serde_json::from_str(raw_json)
            .map_err(|e| GmError::Config(format!("Invalid component catalog JSON: {e}")))?;
        if catalog.schema != 1 {
            return Err(GmError::Config(
                "Unsupported component catalog schema".into(),
            ));
        }
        Ok(catalog)
    }

    pub fn verify_signed(raw_json: &[u8], sig_hex: &str, public_key_hex: &str) -> Result<Self> {
        let key_bytes = hex::decode(public_key_hex.trim()).map_err(|e| {
            GmError::SafetyViolation(format!("Invalid component catalog public key: {e}"))
        })?;
        let key_arr: [u8; 32] = key_bytes.try_into().map_err(|_| {
            GmError::SafetyViolation("Component catalog public key must be 32 bytes".into())
        })?;
        let verifying_key = VerifyingKey::from_bytes(&key_arr)
            .map_err(|e| GmError::SafetyViolation(format!("Invalid Ed25519 key: {e}")))?;
        let sig_bytes = hex::decode(sig_hex.trim()).map_err(|e| {
            GmError::SafetyViolation(format!("Invalid component catalog signature: {e}"))
        })?;
        let sig_arr: [u8; 64] = sig_bytes.try_into().map_err(|_| {
            GmError::SafetyViolation("Component catalog signature must be 64 bytes".into())
        })?;
        verifying_key
            .verify(raw_json, &Signature::from_bytes(&sig_arr))
            .map_err(|_| {
                GmError::SafetyViolation("Component catalog signature verification failed".into())
            })?;

        let catalog: Self = serde_json::from_slice(raw_json)
            .map_err(|e| GmError::Config(format!("Invalid component catalog JSON: {e}")))?;
        if catalog.schema != 1 {
            return Err(GmError::Config(
                "Unsupported component catalog schema".into(),
            ));
        }
        let expires: DateTime<Utc> = DateTime::parse_from_rfc3339(&catalog.expires_at)
            .map_err(|e| GmError::Config(format!("Invalid component catalog expiry: {e}")))?
            .with_timezone(&Utc);
        if expires <= Utc::now() {
            return Err(GmError::SafetyViolation(
                "Component catalog has expired".into(),
            ));
        }
        Ok(catalog)
    }

    /// Finds the best matching component entry for `(id, os, arch)`, preferring exact
    /// OS and architecture matches over `"any"` wildcards.
    pub fn find_for_target(&self, id: &str, os: &str, arch: &str) -> Option<&CatalogComponent> {
        self.components
            .iter()
            .filter(|c| {
                c.id == id
                    && (c.os == os || c.os == "any")
                    && (c.architecture == arch || c.architecture == "any")
            })
            .max_by_key(|c| {
                let os_score = if c.os == os { 2 } else { 1 };
                let arch_score = if c.architecture == arch { 2 } else { 1 };
                (os_score, arch_score)
            })
    }

    /// Returns one resolved component per distinct ID matching the target `(os, arch)`.
    pub fn components_for_target(&self, os: &str, arch: &str) -> Vec<&CatalogComponent> {
        let mut seen = Vec::new();
        for c in &self.components {
            if !seen.contains(&c.id) {
                seen.push(c.id.clone());
            }
        }
        seen.into_iter()
            .filter_map(|id| self.find_for_target(&id, os, arch))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComponentInstallReceipt {
    pub id: String,
    pub version: String,
    pub sha256: String,
    pub total_size_bytes: u64,
    pub active_path: PathBuf,
    pub reused_cache: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ComponentInstallState {
    NotInstalled,
    Downloading,
    Installed,
    Verified,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentStatusSnapshot {
    pub id: String,
    pub kind: ComponentKind,
    pub version: String,
    pub revision: String,
    pub os: String,
    pub architecture: String,
    pub total_size_bytes: u64,
    pub bytes_downloaded: u64,
    pub license: String,
    pub source: String,
    pub includes: Vec<String>,
    pub status: ComponentInstallState,
    pub active_path: Option<String>,
    pub verified_at: Option<String>,
    pub check_summary: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct ComponentRuntimeProgress {
    status: Option<ComponentInstallState>,
    bytes_downloaded: u64,
    verified_at: Option<String>,
    check_summary: Option<String>,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VerificationReceipt {
    id: String,
    version: String,
    sha256: String,
    total_size_bytes: u64,
    verified_at: String,
    check_summary: String,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Streams a file in 256 KiB chunks so multi-hundred-MB GGUF/VM assets never spike RSS.
fn sha256_file_hex(path: &Path) -> Result<(String, u64)> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(256 * 1024, file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 256 * 1024];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hex::encode(hasher.finalize()), total))
}

fn available_disk_bytes(_path: &Path) -> u64 {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let probe = _path
            .ancestors()
            .find(|candidate| candidate.exists())
            .unwrap_or(Path::new("."));
        if let Ok(cpath) = CString::new(probe.as_os_str().as_bytes()) {
            let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
            if unsafe { libc::statvfs(cpath.as_ptr(), &mut stat) } == 0 {
                #[allow(clippy::unnecessary_cast)]
                return (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64);
            }
        }
    }
    // Conservative fallback on platforms without statvfs.
    20 * 1024 * 1024 * 1024
}

fn inspect_payload_header(component: &CatalogComponent, path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut magic = [0u8; 16];
    let n = file.read(&mut magic)?;
    let header = &magic[..n];
    match component.kind {
        ComponentKind::VlmModel => {
            if header.starts_with(b"GGUF") {
                Ok("Verified SHA-256 and GGUF model header".into())
            } else {
                Ok("Verified SHA-256 model checkpoint payload".into())
            }
        }
        ComponentKind::VlmWorker => {
            if header.starts_with(&[0x1f, 0x8b]) {
                Ok("Verified SHA-256 and gzip worker archive".into())
            } else if header.starts_with(b"PK\x03\x04") {
                Ok("Verified SHA-256 and zip worker archive".into())
            } else {
                Ok("Verified SHA-256 worker package".into())
            }
        }
        ComponentKind::SpeechStt => {
            if header.starts_with(b"lmgg") || header.starts_with(b"ggml") {
                Ok("Verified SHA-256 and GGML Whisper model header".into())
            } else {
                Ok("Verified SHA-256 Whisper STT payload".into())
            }
        }
        ComponentKind::SpeechTts => Ok("Verified SHA-256 Piper ONNX TTS voice model".into()),
        ComponentKind::VmImage => {
            if header.starts_with(b"QFI\xfb") {
                Ok("Verified SHA-256 and QCOW2 guest VM disk header".into())
            } else {
                Ok("Verified SHA-256 guest VM image payload".into())
            }
        }
        ComponentKind::VmFirmware => Ok("Verified SHA-256 EDK2 UEFI firmware image".into()),
    }
}

/// Assemble, verify, and atomically activate a chunked component from cached or downloaded parts.
/// Reuses verified cached final or part payloads and rejects any size or hash tampering.
pub fn assemble_and_verify_component<F>(
    component: &CatalogComponent,
    cache_dir: &Path,
    available_bytes: u64,
    mut fetch_part: F,
) -> Result<ComponentInstallReceipt>
where
    F: FnMut(&ComponentPart) -> Result<Vec<u8>>,
{
    if component.parts.is_empty() {
        return Err(GmError::Config(format!(
            "Component '{}' has no pinned parts",
            component.id
        )));
    }
    let parts_sum: u64 = component.parts.iter().map(|p| p.size_bytes).sum();
    if parts_sum != component.total_size_bytes {
        return Err(GmError::SafetyViolation(format!(
            "Component '{}' part sizes ({parts_sum}) do not match declared total ({})",
            component.id, component.total_size_bytes
        )));
    }

    let component_dir = cache_dir.join(&component.id).join(&component.version);
    let final_path = component_dir.join("payload.bin");
    if final_path.is_file()
        && let Ok((digest, size)) = sha256_file_hex(&final_path)
        && digest == component.sha256
        && size == component.total_size_bytes
    {
        return Ok(ComponentInstallReceipt {
            id: component.id.clone(),
            version: component.version.clone(),
            sha256: digest,
            total_size_bytes: size,
            active_path: final_path,
            reused_cache: true,
        });
    }

    // Require enough headroom for parts + assembled file.
    let needed = component.total_size_bytes.saturating_mul(2);
    if available_bytes < needed {
        return Err(GmError::Platform(format!(
            "Insufficient disk space for '{}': need {} bytes, have {} bytes",
            component.id, needed, available_bytes
        )));
    }

    gm_core::storage::ensure_private_dir(&component_dir)?;
    let staging_path = component_dir.join(format!("payload.staging.{}.tmp", uuid::Uuid::new_v4()));
    let mut staged_file = File::create(&staging_path)?;
    let mut whole_hasher = Sha256::new();
    let mut total_written = 0u64;

    let mut ordered_parts = component.parts.clone();
    ordered_parts.sort_by_key(|p| p.index);

    for part in &ordered_parts {
        let part_path = component_dir.join(format!("part-{:04}.bin", part.index));
        let part_bytes = if part_path.is_file() {
            let cached = fs::read(&part_path)?;
            if cached.len() as u64 == part.size_bytes && sha256_hex(&cached) == part.sha256 {
                cached
            } else {
                let fresh = fetch_part(part)?;
                if fresh.len() as u64 != part.size_bytes || sha256_hex(&fresh) != part.sha256 {
                    let _ = fs::remove_file(&staging_path);
                    return Err(GmError::SafetyViolation(format!(
                        "Component '{}' part {} failed size/SHA-256 verification",
                        component.id, part.index
                    )));
                }
                gm_core::storage::atomic_write_private(&part_path, &fresh)?;
                fresh
            }
        } else {
            let fresh = fetch_part(part)?;
            if fresh.len() as u64 != part.size_bytes || sha256_hex(&fresh) != part.sha256 {
                let _ = fs::remove_file(&staging_path);
                return Err(GmError::SafetyViolation(format!(
                    "Component '{}' part {} failed size/SHA-256 verification",
                    component.id, part.index
                )));
            }
            gm_core::storage::atomic_write_private(&part_path, &fresh)?;
            fresh
        };

        staged_file.write_all(&part_bytes)?;
        whole_hasher.update(&part_bytes);
        total_written += part_bytes.len() as u64;
    }
    staged_file.sync_all()?;
    drop(staged_file);

    let final_digest = hex::encode(whole_hasher.finalize());
    if total_written != component.total_size_bytes || final_digest != component.sha256 {
        let _ = fs::remove_file(&staging_path);
        return Err(GmError::SafetyViolation(format!(
            "Assembled component '{}' failed whole-payload SHA-256 verification",
            component.id
        )));
    }
    fs::rename(&staging_path, &final_path)?;

    Ok(ComponentInstallReceipt {
        id: component.id.clone(),
        version: component.version.clone(),
        sha256: final_digest,
        total_size_bytes: total_written,
        active_path: final_path,
        reused_cache: false,
    })
}

/// Manages local optional component discovery, resumable streaming download, SHA-256 verification,
/// and readiness reporting for Doctor, Desktop Settings, and Companion Web.
#[derive(Clone)]
pub struct ComponentManager {
    cache_dir: PathBuf,
    catalog: Arc<ComponentCatalog>,
    os: String,
    arch: String,
    progress: Arc<RwLock<BTreeMap<String, ComponentRuntimeProgress>>>,
    client: reqwest::Client,
}

impl ComponentManager {
    pub fn open(storage_dir: &Path) -> Result<Self> {
        let catalog = ComponentCatalog::bundled()?;
        Ok(Self::with_catalog(
            storage_dir.join("components"),
            catalog,
            std::env::consts::OS,
            std::env::consts::ARCH,
        ))
    }

    pub fn with_catalog(
        cache_dir: PathBuf,
        catalog: ComponentCatalog,
        os: impl Into<String>,
        arch: impl Into<String>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(format!("DT/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            cache_dir,
            catalog: Arc::new(catalog),
            os: os.into(),
            arch: arch.into(),
            progress: Arc::new(RwLock::new(BTreeMap::new())),
            client,
        }
    }

    fn component_dir(&self, component: &CatalogComponent) -> PathBuf {
        self.cache_dir.join(&component.id).join(&component.version)
    }

    fn receipt_path(&self, component: &CatalogComponent) -> PathBuf {
        self.component_dir(component).join("verified.json")
    }

    fn load_receipt(&self, component: &CatalogComponent) -> Option<VerificationReceipt> {
        let raw = fs::read_to_string(self.receipt_path(component)).ok()?;
        let receipt: VerificationReceipt = serde_json::from_str(&raw).ok()?;
        if receipt.sha256 == component.sha256
            && receipt.total_size_bytes == component.total_size_bytes
        {
            Some(receipt)
        } else {
            None
        }
    }

    pub fn list_statuses(&self) -> Vec<ComponentStatusSnapshot> {
        let prog_map = self.progress.read();
        self.catalog
            .components_for_target(&self.os, &self.arch)
            .into_iter()
            .map(|c| {
                let comp_dir = self.component_dir(c);
                let payload_path = comp_dir.join("payload.bin");
                let payload_exists = payload_path.is_file();
                let payload_size = payload_exists
                    .then(|| fs::metadata(&payload_path).ok().map(|m| m.len()))
                    .flatten()
                    .unwrap_or(0);
                let receipt = self.load_receipt(c);
                let runtime = prog_map.get(&c.id);

                let (status, bytes_downloaded, verified_at, check_summary, last_error) =
                    if let Some(rt) = runtime
                        && rt.status == Some(ComponentInstallState::Downloading)
                    {
                        (
                            ComponentInstallState::Downloading,
                            rt.bytes_downloaded,
                            rt.verified_at.clone(),
                            rt.check_summary.clone(),
                            rt.last_error.clone(),
                        )
                    } else if let Some(rec) = receipt
                        && payload_exists
                        && payload_size == c.total_size_bytes
                    {
                        (
                            ComponentInstallState::Verified,
                            c.total_size_bytes,
                            Some(rec.verified_at),
                            Some(rec.check_summary),
                            None,
                        )
                    } else if payload_exists && payload_size == c.total_size_bytes {
                        (
                            ComponentInstallState::Installed,
                            payload_size,
                            runtime.and_then(|r| r.verified_at.clone()),
                            runtime.and_then(|r| r.check_summary.clone()),
                            runtime.and_then(|r| r.last_error.clone()),
                        )
                    } else if let Some(rt) = runtime
                        && rt.status == Some(ComponentInstallState::Failed)
                    {
                        (
                            ComponentInstallState::Failed,
                            rt.bytes_downloaded,
                            None,
                            None,
                            rt.last_error.clone(),
                        )
                    } else {
                        // Sum any already-cached verified parts for partial progress display.
                        let cached_bytes: u64 = c
                            .parts
                            .iter()
                            .filter_map(|p| {
                                fs::metadata(comp_dir.join(format!("part-{:04}.bin", p.index)))
                                    .ok()
                                    .map(|m| m.len())
                            })
                            .sum();
                        (
                            ComponentInstallState::NotInstalled,
                            cached_bytes.min(c.total_size_bytes),
                            None,
                            None,
                            runtime.and_then(|r| r.last_error.clone()),
                        )
                    };

                ComponentStatusSnapshot {
                    id: c.id.clone(),
                    kind: c.kind,
                    version: c.version.clone(),
                    revision: c.revision.clone(),
                    os: c.os.clone(),
                    architecture: c.architecture.clone(),
                    total_size_bytes: c.total_size_bytes,
                    bytes_downloaded,
                    license: c.license.clone(),
                    source: c.source.clone(),
                    includes: c.includes.clone(),
                    status,
                    active_path: payload_exists.then(|| payload_path.display().to_string()),
                    verified_at,
                    check_summary,
                    last_error,
                }
            })
            .collect()
    }

    pub fn installed_components_for_kind(
        &self,
        kind: ComponentKind,
    ) -> Vec<ComponentStatusSnapshot> {
        self.list_statuses()
            .into_iter()
            .filter(|c| {
                c.kind == kind
                    && matches!(
                        c.status,
                        ComponentInstallState::Installed | ComponentInstallState::Verified
                    )
            })
            .collect()
    }

    /// Downloads missing parts via streaming HTTP (resuming from cached verified parts),
    /// verifies part and whole-payload SHA-256 digests, atomically activates `payload.bin`,
    /// and runs post-install verification.
    pub async fn install(&self, id: &str) -> Result<ComponentStatusSnapshot> {
        let component = self
            .catalog
            .find_for_target(id.trim(), &self.os, &self.arch)
            .cloned()
            .ok_or_else(|| {
                GmError::Config(format!(
                    "Component '{}' not found in catalog for {}/{}",
                    id.trim(),
                    self.os,
                    self.arch
                ))
            })?;

        if component.parts.is_empty() || component.total_size_bytes == 0 {
            return Err(GmError::Config(format!(
                "Component '{}' has no downloadable parts configured",
                component.id
            )));
        }
        let parts_sum: u64 = component.parts.iter().map(|p| p.size_bytes).sum();
        if parts_sum != component.total_size_bytes {
            return Err(GmError::SafetyViolation(format!(
                "Component '{}' part sizes ({parts_sum}) do not match total ({})",
                component.id, component.total_size_bytes
            )));
        }

        let comp_dir = self.component_dir(&component);
        let final_path = comp_dir.join("payload.bin");
        if final_path.is_file()
            && let Ok((digest, size)) = sha256_file_hex(&final_path)
            && digest == component.sha256
            && size == component.total_size_bytes
        {
            return self.verify(&component.id).await;
        }

        let avail = available_disk_bytes(&self.cache_dir);
        let needed = component.total_size_bytes.saturating_mul(2);
        if avail < needed {
            let msg = format!(
                "Insufficient disk space for '{}': need {} bytes, have {} bytes",
                component.id, needed, avail
            );
            self.record_failure(&component.id, 0, msg.clone());
            return Err(GmError::Platform(msg));
        }

        gm_core::storage::ensure_private_dir(&comp_dir)?;
        {
            let mut map = self.progress.write();
            map.insert(
                component.id.clone(),
                ComponentRuntimeProgress {
                    status: Some(ComponentInstallState::Downloading),
                    bytes_downloaded: 0,
                    verified_at: None,
                    check_summary: None,
                    last_error: None,
                },
            );
        }

        let mut ordered_parts = component.parts.clone();
        ordered_parts.sort_by_key(|p| p.index);

        let mut completed_bytes = 0u64;
        for part in &ordered_parts {
            let part_path = comp_dir.join(format!("part-{:04}.bin", part.index));
            if part_path.is_file()
                && let Ok((digest, size)) = sha256_file_hex(&part_path)
                && digest == part.sha256
                && size == part.size_bytes
            {
                completed_bytes = completed_bytes.saturating_add(size);
                self.update_bytes(&component.id, completed_bytes);
                continue;
            }

            let part_tmp = comp_dir.join(format!(
                "part-{:04}.{}.tmp",
                part.index,
                uuid::Uuid::new_v4().simple()
            ));
            let dl_res = async {
                let mut resp = self
                    .client
                    .get(&part.url)
                    .send()
                    .await
                    .map_err(|e| GmError::Platform(format!("Download failed for {}: {e}", part.url)))?;
                if !resp.status().is_success() {
                    return Err(GmError::Platform(format!(
                        "Download failed for {}: HTTP {}",
                        part.url,
                        resp.status()
                    )));
                }
                let mut out = File::create(&part_tmp)?;
                let mut hasher = Sha256::new();
                let mut part_written = 0u64;
                while let Some(chunk) = resp
                    .chunk()
                    .await
                    .map_err(|e| GmError::Platform(format!("Stream error for {}: {e}", part.url)))?
                {
                    out.write_all(&chunk)?;
                    hasher.update(&chunk);
                    part_written = part_written.saturating_add(chunk.len() as u64);
                    self.update_bytes(
                        &component.id,
                        completed_bytes.saturating_add(part_written),
                    );
                }
                out.sync_all()?;
                drop(out);
                let digest = hex::encode(hasher.finalize());
                if part_written != part.size_bytes || digest != part.sha256 {
                    let _ = fs::remove_file(&part_tmp);
                    return Err(GmError::SafetyViolation(format!(
                        "Component '{}' part {} failed size/SHA-256 verification (got {} bytes / {})",
                        component.id, part.index, part_written, digest
                    )));
                }
                fs::rename(&part_tmp, &part_path)?;
                Ok::<u64, GmError>(part_written)
            }
            .await;

            match dl_res {
                Ok(written) => {
                    completed_bytes = completed_bytes.saturating_add(written);
                    self.update_bytes(&component.id, completed_bytes);
                }
                Err(e) => {
                    let _ = fs::remove_file(&part_tmp);
                    self.record_failure(&component.id, completed_bytes, e.to_string());
                    return Err(e);
                }
            }
        }

        // Stream-assemble parts into staging file, verify whole-payload SHA-256, and atomically activate.
        let staging_path = comp_dir.join(format!(
            "payload.staging.{}.tmp",
            uuid::Uuid::new_v4().simple()
        ));
        let assemble_res = (|| {
            let mut staged = File::create(&staging_path)?;
            let mut whole_hasher = Sha256::new();
            let mut total_written = 0u64;
            let mut buf = [0u8; 256 * 1024];
            for part in &ordered_parts {
                let part_path = comp_dir.join(format!("part-{:04}.bin", part.index));
                let mut reader = BufReader::with_capacity(256 * 1024, File::open(&part_path)?);
                loop {
                    let n = reader.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    staged.write_all(&buf[..n])?;
                    whole_hasher.update(&buf[..n]);
                    total_written += n as u64;
                }
            }
            staged.sync_all()?;
            drop(staged);
            let final_digest = hex::encode(whole_hasher.finalize());
            if total_written != component.total_size_bytes || final_digest != component.sha256 {
                let _ = fs::remove_file(&staging_path);
                return Err(GmError::SafetyViolation(format!(
                    "Assembled component '{}' failed whole-payload SHA-256 verification",
                    component.id
                )));
            }
            fs::rename(&staging_path, &final_path)?;
            // Clean up redundant single-part file after verified atomic activation to save SSD space.
            for part in &ordered_parts {
                let _ = fs::remove_file(comp_dir.join(format!("part-{:04}.bin", part.index)));
            }
            Ok::<_, GmError>(())
        })();

        if let Err(e) = assemble_res {
            let _ = fs::remove_file(&staging_path);
            self.record_failure(&component.id, completed_bytes, e.to_string());
            return Err(e);
        }

        self.verify(&component.id).await
    }

    /// Re-hashes the active payload on disk and performs format/header readiness validation.
    pub async fn verify(&self, id: &str) -> Result<ComponentStatusSnapshot> {
        let component = self
            .catalog
            .find_for_target(id.trim(), &self.os, &self.arch)
            .cloned()
            .ok_or_else(|| {
                GmError::Config(format!(
                    "Component '{}' not found in catalog for {}/{}",
                    id.trim(),
                    self.os,
                    self.arch
                ))
            })?;
        let final_path = self.component_dir(&component).join("payload.bin");
        if !final_path.is_file() {
            let msg = format!("Component '{}' is not installed yet", component.id);
            self.record_failure(&component.id, 0, msg.clone());
            return Err(GmError::Config(msg));
        }
        let path_clone = final_path.clone();
        let comp_clone = component.clone();
        let (digest, size, check_summary) = tokio::task::spawn_blocking(move || {
            let (digest, size) = sha256_file_hex(&path_clone)?;
            let summary = inspect_payload_header(&comp_clone, &path_clone)?;
            Ok::<_, GmError>((digest, size, summary))
        })
        .await
        .map_err(|e| GmError::Platform(format!("Verification worker panicked: {e}")))??;

        if size != component.total_size_bytes || digest != component.sha256 {
            let msg = format!(
                "Verification failed for '{}': expected {} bytes ({}), found {} bytes ({})",
                component.id, component.total_size_bytes, component.sha256, size, digest
            );
            let _ = fs::remove_file(self.receipt_path(&component));
            self.record_failure(&component.id, size, msg.clone());
            return Err(GmError::SafetyViolation(msg));
        }

        let now = Utc::now().to_rfc3339();
        let receipt = VerificationReceipt {
            id: component.id.clone(),
            version: component.version.clone(),
            sha256: digest,
            total_size_bytes: size,
            verified_at: now.clone(),
            check_summary: check_summary.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&receipt)?;
        gm_core::storage::atomic_write_private(&self.receipt_path(&component), &bytes)?;

        {
            let mut map = self.progress.write();
            map.insert(
                component.id.clone(),
                ComponentRuntimeProgress {
                    status: Some(ComponentInstallState::Verified),
                    bytes_downloaded: size,
                    verified_at: Some(now),
                    check_summary: Some(check_summary),
                    last_error: None,
                },
            );
        }

        self.list_statuses()
            .into_iter()
            .find(|s| s.id == component.id)
            .ok_or_else(|| GmError::Platform("Verified component missing from catalog".into()))
    }

    fn update_bytes(&self, id: &str, bytes: u64) {
        let mut map = self.progress.write();
        if let Some(entry) = map.get_mut(id) {
            entry.bytes_downloaded = bytes;
        }
    }

    fn record_failure(&self, id: &str, bytes: u64, error: String) {
        let mut map = self.progress.write();
        map.insert(
            id.to_string(),
            ComponentRuntimeProgress {
                status: Some(ComponentInstallState::Failed),
                bytes_downloaded: bytes,
                verified_at: None,
                check_summary: None,
                last_error: Some(error),
            },
        );
    }
}

#[async_trait::async_trait]
impl gm_openbot::CompanionComponents for ComponentManager {
    async fn list(&self) -> Result<serde_json::Value> {
        Ok(serde_json::json!({
            "schema": self.catalog.schema,
            "sequence": self.catalog.sequence,
            "os": self.os,
            "architecture": self.arch,
            "components": self.list_statuses(),
        }))
    }

    async fn install(&self, id: &str) -> Result<serde_json::Value> {
        let status = ComponentManager::install(self, id).await?;
        Ok(serde_json::to_value(status)?)
    }

    async fn verify(&self, id: &str) -> Result<serde_json::Value> {
        let status = ComponentManager::verify(self, id).await?;
        Ok(serde_json::to_value(status)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn test_bundled_component_catalog_has_real_pinned_parts_for_all_targets() {
        let catalog = ComponentCatalog::parse_unverified(BUNDLED_COMPONENT_CATALOG_JSON).unwrap();
        let expected_ids = [
            "qwen2-vl-2b",
            "smolvlm",
            "smolvlm2",
            "vlm_worker",
            "whisper-stt-base-en",
            "speech_tts",
            "vm_image",
            "vm_firmware",
        ];
        let targets = [
            ("macos", "aarch64"),
            ("macos", "x86_64"),
            ("linux", "x86_64"),
            ("linux", "aarch64"),
            ("windows", "x86_64"),
            ("windows", "aarch64"),
        ];
        for (os, arch) in targets {
            for id in expected_ids {
                let comp = catalog
                    .find_for_target(id, os, arch)
                    .unwrap_or_else(|| panic!("missing catalog entry for {id} on {os}/{arch}"));
                assert!(
                    !comp.parts.is_empty(),
                    "component {id} on {os}/{arch} must have pinned parts"
                );
                assert!(
                    comp.total_size_bytes > 0,
                    "component {id} on {os}/{arch} must have non-zero size"
                );
                assert_eq!(
                    comp.sha256.len(),
                    64,
                    "component {id} on {os}/{arch} must have 64-hex sha256"
                );
                assert!(
                    comp.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
                    "component {id} sha256 must be valid hex"
                );
                let parts_sum: u64 = comp.parts.iter().map(|p| p.size_bytes).sum();
                assert_eq!(parts_sum, comp.total_size_bytes);
                for part in &comp.parts {
                    assert!(part.url.starts_with("https://"));
                    assert_eq!(part.sha256.len(), 64);
                }
            }
        }
    }

    #[test]
    fn test_component_catalog_chunked_verification_resume_and_tamper_rejection() {
        let p0 = b"hello-part-zero-".to_vec();
        let p1 = b"world-part-one!".to_vec();
        let combined: Vec<u8> = [p0.as_slice(), p1.as_slice()].concat();

        let component = CatalogComponent {
            id: "smolvlm2".into(),
            kind: ComponentKind::VlmModel,
            version: "1.0.1".into(),
            revision: "rev-1".into(),
            os: "any".into(),
            architecture: "any".into(),
            worker_compatibility: vec!["vlm_worker>=1.0.1".into()],
            includes: vec!["weights".into(), "mmproj".into(), "tokenizer".into()],
            total_size_bytes: combined.len() as u64,
            sha256: sha256_hex(&combined),
            license: "Apache-2.0".into(),
            source: "https://example.invalid/smolvlm2".into(),
            parts: vec![
                ComponentPart {
                    index: 0,
                    url: "https://example.invalid/part0".into(),
                    size_bytes: p0.len() as u64,
                    sha256: sha256_hex(&p0),
                },
                ComponentPart {
                    index: 1,
                    url: "https://example.invalid/part1".into(),
                    size_bytes: p1.len() as u64,
                    sha256: sha256_hex(&p1),
                },
            ],
        };

        // Signed catalog verification
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let catalog = ComponentCatalog {
            schema: 1,
            sequence: 1,
            published_at: Utc::now().to_rfc3339(),
            expires_at: (Utc::now() + chrono::Duration::days(7)).to_rfc3339(),
            origin: "https://github.com/galaxycoils/DT".into(),
            components: vec![component.clone()],
        };
        let raw = serde_json::to_vec(&catalog).unwrap();
        let sig_hex = hex::encode(signing_key.sign(&raw).to_bytes());
        let pub_hex = hex::encode(signing_key.verifying_key().to_bytes());
        let verified = ComponentCatalog::verify_signed(&raw, &sig_hex, &pub_hex).unwrap();
        assert_eq!(verified.components.len(), 1);

        let cache = std::env::temp_dir().join(format!("dt-comp-test-{}", uuid::Uuid::new_v4()));
        // Insufficient disk space blocks download before staging.
        assert!(assemble_and_verify_component(&component, &cache, 10, |_| Ok(vec![])).is_err());

        // Simulated interruption on part 1 preserves verified part 0.
        let mut attempts = 0;
        let interrupted = assemble_and_verify_component(&component, &cache, 100_000, |part| {
            attempts += 1;
            if part.index == 0 {
                Ok(p0.clone())
            } else {
                Err(GmError::Platform("Simulated network drop".into()))
            }
        });
        assert!(interrupted.is_err());

        // Tampered part 1 is rejected.
        let tampered = assemble_and_verify_component(&component, &cache, 100_000, |part| {
            if part.index == 0 {
                panic!("part 0 should be reused from verified cache");
            }
            Ok(b"tampered-bytes!".to_vec())
        });
        assert!(tampered.is_err());

        // Resume downloads only part 1 and activates.
        let mut resumed_fetches = 0;
        let ok = assemble_and_verify_component(&component, &cache, 100_000, |part| {
            resumed_fetches += 1;
            assert_eq!(part.index, 1);
            Ok(p1.clone())
        })
        .unwrap();
        assert_eq!(resumed_fetches, 1);
        assert!(!ok.reused_cache);

        // Next call reuses final cached payload with 0 fetches.
        let cached = assemble_and_verify_component(&component, &cache, 100_000, |_| {
            panic!("should reuse verified cache")
        })
        .unwrap();
        assert!(cached.reused_cache);

        let _ = fs::remove_dir_all(&cache);
    }

    #[test]
    fn test_component_catalog_chunked_verification() {
        test_component_catalog_chunked_verification_resume_and_tamper_rejection();
    }

    #[tokio::test]
    async fn test_component_manager_downloads_retries_and_verifies_over_http() {
        let p0 = b"GGUF-part-0-bytes".to_vec();
        let p1 = b"-part-1-tail!".to_vec();
        let combined: Vec<u8> = [p0.as_slice(), p1.as_slice()].concat();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let p0_srv = p0.clone();
        let p1_srv = p1.clone();
        let fail_first_p1 = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let fail_flag = Arc::clone(&fail_first_p1);

        let server = tokio::spawn(async move {
            for _ in 0..4 {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let mut req_buf = [0u8; 1024];
                let n = sock.read(&mut req_buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&req_buf[..n]);
                if req.starts_with("GET /p0 ") {
                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        p0_srv.len()
                    );
                    let _ = sock.write_all(hdr.as_bytes()).await;
                    let _ = sock.write_all(&p0_srv).await;
                } else if req.starts_with("GET /p1 ") {
                    if fail_flag.swap(false, std::sync::atomic::Ordering::SeqCst) {
                        let _ = sock
                            .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await;
                    } else {
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            p1_srv.len()
                        );
                        let _ = sock.write_all(hdr.as_bytes()).await;
                        let _ = sock.write_all(&p1_srv).await;
                    }
                }
            }
        });

        let catalog = ComponentCatalog {
            schema: 1,
            sequence: 1,
            published_at: Utc::now().to_rfc3339(),
            expires_at: (Utc::now() + chrono::Duration::days(7)).to_rfc3339(),
            origin: "http://127.0.0.1".into(),
            components: vec![CatalogComponent {
                id: "smolvlm".into(),
                kind: ComponentKind::VlmModel,
                version: "1.0.0".into(),
                revision: "test".into(),
                os: "any".into(),
                architecture: "any".into(),
                worker_compatibility: vec![],
                includes: vec!["smolvlm.gguf".into()],
                total_size_bytes: combined.len() as u64,
                sha256: sha256_hex(&combined),
                license: "Apache-2.0".into(),
                source: "http://127.0.0.1".into(),
                parts: vec![
                    ComponentPart {
                        index: 0,
                        url: format!("http://{addr}/p0"),
                        size_bytes: p0.len() as u64,
                        sha256: sha256_hex(&p0),
                    },
                    ComponentPart {
                        index: 1,
                        url: format!("http://{addr}/p1"),
                        size_bytes: p1.len() as u64,
                        sha256: sha256_hex(&p1),
                    },
                ],
            }],
        };

        let cache = std::env::temp_dir().join(format!("dt-cmgr-{}", uuid::Uuid::new_v4()));
        let mgr = ComponentManager::with_catalog(cache.clone(), catalog, "macos", "aarch64");
        assert_eq!(
            mgr.list_statuses()[0].status,
            ComponentInstallState::NotInstalled
        );

        // First attempt fails on part 1 (503) and records Failed status while keeping verified part 0.
        assert!(mgr.install("smolvlm").await.is_err());
        assert_eq!(mgr.list_statuses()[0].status, ComponentInstallState::Failed);

        // Retry resumes cleanly and verifies GGUF header + SHA-256.
        let installed = mgr.install("smolvlm").await.unwrap();
        assert_eq!(installed.status, ComponentInstallState::Verified);
        assert!(
            installed
                .check_summary
                .as_deref()
                .unwrap_or("")
                .contains("GGUF")
        );
        assert_eq!(
            mgr.installed_components_for_kind(ComponentKind::VlmModel)
                .len(),
            1
        );

        server.abort();
        let _ = fs::remove_dir_all(&cache);
    }
}
