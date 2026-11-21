//! Signed optional component catalog and resumable chunked payload delivery.
//! Large models, speech engines, VM images, and firmware are pinned by source, version,
//! architecture, size, license, and chunk hashes, and verified before activation.

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use gm_core::error::{GmError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComponentPart {
    pub index: usize,
    pub url: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    pub os: String,
    pub architecture: String,
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

    pub fn find_for_target(&self, id: &str, os: &str, arch: &str) -> Option<&CatalogComponent> {
        self.components.iter().find(|c| {
            c.id == id
                && (c.os == os || c.os == "any")
                && (c.architecture == arch || c.architecture == "any")
        })
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

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn sha256_file_hex(path: &Path) -> Result<(String, u64)> {
    let bytes = fs::read(path)?;
    Ok((sha256_hex(&bytes), bytes.len() as u64))
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

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn test_component_catalog_chunked_verification_resume_and_tamper_rejection() {
        let p0 = b"hello-part-zero-".to_vec();
        let p1 = b"world-part-one!".to_vec();
        let combined: Vec<u8> = [p0.as_slice(), p1.as_slice()].concat();

        let component = CatalogComponent {
            id: "smolvlm2".into(),
            kind: ComponentKind::VlmModel,
            version: "1.0.0".into(),
            os: "any".into(),
            architecture: "any".into(),
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
}
