//! Hosted multi-tenant control plane: PostgreSQL ANSI schema, isolated per-workspace worker
//! directories, encrypted tenant secret storage, cross-tenant authorization enforcement, and
//! dry-run import / backup / restore pipelines.

use crate::vault::SecretBackend;
use aes::Aes256;
use aes::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};
use base64::Engine;
use chrono::Utc;
use gm_core::error::{GmError, Result};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// External PostgreSQL 15+ DDL executed when configuring a hosted PostgreSQL control plane (`DATABASE_URL`).
pub const POSTGRES_CONTROL_PLANE_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS tenants (
    tenant_id TEXT PRIMARY KEY,
    owner_email TEXT NOT NULL,
    billing_mode TEXT NOT NULL DEFAULT 'hosted',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE IF NOT EXISTS workspaces (
    ws_id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants(tenant_id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    worker_dir TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE IF NOT EXISTS workspace_tokens (
    token_hash TEXT PRIMARY KEY,
    ws_id TEXT NOT NULL REFERENCES workspaces(ws_id) ON DELETE CASCADE,
    tenant_id TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'owner',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE IF NOT EXISTS encrypted_secrets (
    ws_id TEXT NOT NULL REFERENCES workspaces(ws_id) ON DELETE CASCADE,
    account TEXT NOT NULL,
    nonce_b64 TEXT NOT NULL,
    ciphertext_b64 TEXT NOT NULL,
    tag_hex TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (ws_id, account)
);
CREATE TABLE IF NOT EXISTS workspace_backups (
    backup_id TEXT PRIMARY KEY,
    ws_id TEXT NOT NULL REFERENCES workspaces(ws_id) ON DELETE CASCADE,
    sha256 TEXT NOT NULL,
    snapshot_json TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
"#;

pub fn validate_workspace_slug(ws_id: &str) -> Result<String> {
    let trimmed = ws_id.trim();
    if trimmed.is_empty()
        || trimmed.len() > 64
        || trimmed.starts_with('.')
        || trimmed.contains("..")
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains('\0')
        || !trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(GmError::SafetyViolation(format!(
            "RULE-PATH-002: Invalid workspace identifier '{ws_id}'"
        )));
    }
    Ok(trimmed.to_string())
}

pub fn workspace_worker_dir(workers_root: &Path, ws_id: &str) -> Result<PathBuf> {
    let clean = validate_workspace_slug(ws_id)?;
    let root_canon = if workers_root.exists() {
        workers_root
            .canonicalize()
            .unwrap_or_else(|_| workers_root.to_path_buf())
    } else {
        std::fs::create_dir_all(workers_root)?;
        workers_root
            .canonicalize()
            .unwrap_or_else(|_| workers_root.to_path_buf())
    };
    let dir = root_canon.join(&clean);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRecord {
    pub ws_id: String,
    pub tenant_id: String,
    pub name: String,
    pub worker_dir: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceImportDryRun {
    pub ws_id: String,
    pub target_worker_dir: String,
    pub files_planned: usize,
    pub secrets_planned: usize,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceBackupBundle {
    pub backup_id: String,
    pub ws_id: String,
    pub created_at: String,
    pub files: BTreeMap<String, String>,
    pub encrypted_secret_accounts: Vec<String>,
    pub sha256: String,
}

fn derive_aes_key(master_key: &[u8], ws_id: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"gm-hosted-tenant-secret-v1:");
    h.update(master_key);
    h.update(b":");
    h.update(ws_id.as_bytes());
    h.finalize().into()
}

fn aes_ctr_xor(key: &[u8; 32], nonce: &[u8; 16], data: &[u8]) -> Vec<u8> {
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut out = Vec::with_capacity(data.len());
    for (block_idx, chunk) in data.chunks(16).enumerate() {
        let mut ctr = *nonce;
        let idx_bytes = (block_idx as u64).to_be_bytes();
        for i in 0..8 {
            ctr[8 + i] ^= idx_bytes[i];
        }
        let mut block = GenericArray::clone_from_slice(&ctr);
        cipher.encrypt_block(&mut block);
        for (b, k) in chunk.iter().zip(block.iter()) {
            out.push(b ^ k);
        }
    }
    out
}

fn compute_auth_tag(key: &[u8; 32], ws_id: &str, account: &str, nonce: &[u8], ct: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(key);
    h.update(ws_id.as_bytes());
    h.update(b":");
    h.update(account.as_bytes());
    h.update(nonce);
    h.update(ct);
    hex::encode(h.finalize())
}

#[derive(Clone)]
pub struct HostedControlPlane {
    conn: Arc<Mutex<Connection>>,
    workers_root: PathBuf,
    master_key: [u8; 32],
}

impl HostedControlPlane {
    pub fn open(
        db_path: impl AsRef<Path>,
        workers_root: impl AsRef<Path>,
        master_key: &[u8],
    ) -> Result<Self> {
        if master_key.len() < 16 {
            return Err(GmError::Config(
                "Hosted master encryption key must be at least 16 bytes".into(),
            ));
        }
        let conn = crate::db_recovery::open_sqlite_with_recovery(db_path.as_ref())?;
        Self::init(conn, workers_root.as_ref().to_path_buf(), master_key)
    }

    pub fn in_memory(workers_root: impl AsRef<Path>, master_key: &[u8]) -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| GmError::Platform(format!("Hosted control plane DB error: {e}")))?;
        Self::init(conn, workers_root.as_ref().to_path_buf(), master_key)
    }

    fn init(conn: Connection, workers_root: PathBuf, master_key: &[u8]) -> Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS hosted_workspaces_v1 (
                ws_id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                name TEXT NOT NULL,
                worker_dir TEXT NOT NULL,
                token_sha256 TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS hosted_encrypted_secrets_v1 (
                ws_id TEXT NOT NULL,
                account TEXT NOT NULL,
                nonce_b64 TEXT NOT NULL,
                ciphertext_b64 TEXT NOT NULL,
                tag_hex TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (ws_id, account)
            );",
        )
        .map_err(|e| GmError::Platform(format!("Hosted schema error: {e}")))?;
        let key: [u8; 32] = Sha256::digest(master_key).into();
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            workers_root,
            master_key: key,
        })
    }

    pub fn postgres_schema_sql(&self) -> &'static str {
        POSTGRES_CONTROL_PLANE_SCHEMA
    }

    pub fn provision_workspace(
        &self,
        ws_id: &str,
        tenant_id: &str,
        name: &str,
        bearer_token: &str,
    ) -> Result<WorkspaceRecord> {
        let clean_ws = validate_workspace_slug(ws_id)?;
        if tenant_id.trim().is_empty() || bearer_token.trim().is_empty() {
            return Err(GmError::Config(
                "tenant_id and bearer_token cannot be empty".into(),
            ));
        }
        let dir = workspace_worker_dir(&self.workers_root, &clean_ws)?;
        let token_sha = hex::encode(Sha256::digest(bearer_token.trim().as_bytes()));
        let now = Utc::now().to_rfc3339();
        let rec = WorkspaceRecord {
            ws_id: clean_ws.clone(),
            tenant_id: tenant_id.trim().to_string(),
            name: name.trim().to_string(),
            worker_dir: dir.display().to_string(),
            created_at: now.clone(),
        };
        self.conn
            .lock()
            .execute(
                "INSERT INTO hosted_workspaces_v1 (ws_id, tenant_id, name, worker_dir, token_sha256, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(ws_id) DO UPDATE SET
                   name = excluded.name,
                   worker_dir = excluded.worker_dir,
                   token_sha256 = excluded.token_sha256",
                params![rec.ws_id, rec.tenant_id, rec.name, rec.worker_dir, token_sha, now],
            )
            .map_err(|e| GmError::Platform(format!("Provision workspace failed: {e}")))?;
        Ok(rec)
    }

    pub fn authorize_workspace(&self, ws_id: &str, bearer_token: &str) -> Result<WorkspaceRecord> {
        let clean_ws = validate_workspace_slug(ws_id)?;
        let candidate_hash = hex::encode(Sha256::digest(bearer_token.trim().as_bytes()));
        let conn = self.conn.lock();
        let row: Option<(String, String, String, String, String)> = conn
            .query_row(
                "SELECT ws_id, tenant_id, name, worker_dir, token_sha256 FROM hosted_workspaces_v1 WHERE ws_id = ?1",
                params![clean_ws],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let Some((ws, tenant, name, dir, expected_hash)) = row else {
            return Err(GmError::SafetyViolation(format!(
                "Workspace '{ws_id}' not found or access denied"
            )));
        };
        if !crate::bridges::constant_time_eq(candidate_hash.as_bytes(), expected_hash.as_bytes()) {
            return Err(GmError::SafetyViolation(format!(
                "Cross-tenant access denied for workspace '{ws_id}'"
            )));
        }
        Ok(WorkspaceRecord {
            ws_id: ws,
            tenant_id: tenant,
            name,
            worker_dir: dir,
            created_at: String::new(),
        })
    }

    pub fn tenant_secret_backend(&self, ws_id: &str) -> Result<TenantSecretVault> {
        let clean = validate_workspace_slug(ws_id)?;
        Ok(TenantSecretVault {
            plane: self.clone(),
            ws_id: clean,
        })
    }

    pub fn dry_run_import(
        &self,
        ws_id: &str,
        files: &BTreeMap<String, String>,
        secrets_count: usize,
    ) -> Result<WorkspaceImportDryRun> {
        let dir = workspace_worker_dir(&self.workers_root, ws_id)?;
        let json = serde_json::to_string(files)?;
        let sha256 = hex::encode(Sha256::digest(json.as_bytes()));
        Ok(WorkspaceImportDryRun {
            ws_id: validate_workspace_slug(ws_id)?,
            target_worker_dir: dir.display().to_string(),
            files_planned: files.len(),
            secrets_planned: secrets_count,
            sha256,
        })
    }

    pub fn backup_workspace(&self, ws_id: &str) -> Result<WorkspaceBackupBundle> {
        let clean = validate_workspace_slug(ws_id)?;
        let dir = workspace_worker_dir(&self.workers_root, &clean)?;
        let mut files = BTreeMap::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for ent in entries.flatten() {
                if let Ok(meta) = ent.metadata()
                    && meta.is_file()
                    && let Some(name) = ent.file_name().to_str()
                    && let Ok(text) = std::fs::read_to_string(ent.path())
                {
                    files.insert(name.to_string(), text);
                }
            }
        }
        let mut accounts = Vec::new();
        {
            let conn = self.conn.lock();
            let mut stmt = conn
                .prepare("SELECT account FROM hosted_encrypted_secrets_v1 WHERE ws_id = ?1 ORDER BY account")
                .map_err(|e| GmError::Platform(e.to_string()))?;
            let rows = stmt
                .query_map(params![clean], |r| r.get::<_, String>(0))
                .map_err(|e| GmError::Platform(e.to_string()))?;
            for acc in rows.flatten() {
                accounts.push(acc);
            }
        }
        let payload_json = serde_json::to_string(&(&clean, &files, &accounts))?;
        let sha256 = hex::encode(Sha256::digest(payload_json.as_bytes()));
        Ok(WorkspaceBackupBundle {
            backup_id: format!("bak-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]),
            ws_id: clean,
            created_at: Utc::now().to_rfc3339(),
            files,
            encrypted_secret_accounts: accounts,
            sha256,
        })
    }

    pub fn restore_workspace(
        &self,
        target_ws_id: &str,
        bundle: &WorkspaceBackupBundle,
    ) -> Result<usize> {
        let clean = validate_workspace_slug(target_ws_id)?;
        let payload_json = serde_json::to_string(&(
            &bundle.ws_id,
            &bundle.files,
            &bundle.encrypted_secret_accounts,
        ))?;
        let actual = hex::encode(Sha256::digest(payload_json.as_bytes()));
        if actual != bundle.sha256 {
            return Err(GmError::SafetyViolation(
                "Workspace backup SHA-256 verification failed".into(),
            ));
        }
        if bundle.ws_id != clean {
            return Err(GmError::SafetyViolation(format!(
                "Cross-tenant restore blocked: backup belongs to '{}' not '{}'",
                bundle.ws_id, clean
            )));
        }
        let dir = workspace_worker_dir(&self.workers_root, &clean)?;
        for (name, content) in &bundle.files {
            if name.contains("..") || name.contains('/') || name.contains('\\') || name.is_empty() {
                return Err(GmError::SafetyViolation(format!(
                    "Unsafe backup filename '{name}'"
                )));
            }
            std::fs::write(dir.join(name), content)?;
        }
        Ok(bundle.files.len())
    }
}

#[derive(Clone)]
pub struct TenantSecretVault {
    plane: HostedControlPlane,
    ws_id: String,
}

impl SecretBackend for TenantSecretVault {
    fn get_secret(&self, account: &str) -> Result<String> {
        let conn = self.plane.conn.lock();
        let row: Option<(String, String, String)> = conn
            .query_row(
                "SELECT nonce_b64, ciphertext_b64, tag_hex FROM hosted_encrypted_secrets_v1
                 WHERE ws_id = ?1 AND account = ?2",
                params![self.ws_id, account],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let Some((nonce_b64, ct_b64, tag_hex)) = row else {
            return Err(GmError::Config(format!(
                "Tenant secret '{account}' not found in workspace '{}'",
                self.ws_id
            )));
        };
        let nonce_vec = base64::engine::general_purpose::STANDARD
            .decode(&nonce_b64)
            .map_err(|e| GmError::Platform(format!("Corrupt secret nonce: {e}")))?;
        let nonce: [u8; 16] = nonce_vec
            .try_into()
            .map_err(|_| GmError::Platform("Invalid nonce length".into()))?;
        let ct = base64::engine::general_purpose::STANDARD
            .decode(&ct_b64)
            .map_err(|e| GmError::Platform(format!("Corrupt ciphertext: {e}")))?;
        let key = derive_aes_key(&self.plane.master_key, &self.ws_id);
        let expected_tag = compute_auth_tag(&key, &self.ws_id, account, &nonce, &ct);
        if !crate::bridges::constant_time_eq(expected_tag.as_bytes(), tag_hex.as_bytes()) {
            return Err(GmError::SafetyViolation(
                "Encrypted tenant secret authentication tag mismatch".into(),
            ));
        }
        let pt = aes_ctr_xor(&key, &nonce, &ct);
        String::from_utf8(pt).map_err(|e| GmError::Platform(format!("Invalid UTF-8 secret: {e}")))
    }

    fn set_secret(&self, account: &str, secret: &str) -> Result<()> {
        let key = derive_aes_key(&self.plane.master_key, &self.ws_id);
        let nonce: [u8; 16] = *uuid::Uuid::new_v4().as_bytes();
        let ct = aes_ctr_xor(&key, &nonce, secret.as_bytes());
        let tag_hex = compute_auth_tag(&key, &self.ws_id, account, &nonce, &ct);
        let nonce_b64 = base64::engine::general_purpose::STANDARD.encode(nonce);
        let ct_b64 = base64::engine::general_purpose::STANDARD.encode(&ct);
        self.plane
            .conn
            .lock()
            .execute(
                "INSERT INTO hosted_encrypted_secrets_v1 (ws_id, account, nonce_b64, ciphertext_b64, tag_hex, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(ws_id, account) DO UPDATE SET
                   nonce_b64 = excluded.nonce_b64,
                   ciphertext_b64 = excluded.ciphertext_b64,
                   tag_hex = excluded.tag_hex,
                   updated_at = excluded.updated_at",
                params![self.ws_id, account, nonce_b64, ct_b64, tag_hex, Utc::now().to_rfc3339()],
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(())
    }

    fn delete_secret(&self, account: &str) -> Result<()> {
        self.plane
            .conn
            .lock()
            .execute(
                "DELETE FROM hosted_encrypted_secrets_v1 WHERE ws_id = ?1 AND account = ?2",
                params![self.ws_id, account],
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosted_control_plane_enforces_tenant_isolation_encryption_and_backup_restore() {
        let dir = tempfile::tempdir().unwrap();
        let workers = dir.path().join("workers");
        let plane =
            HostedControlPlane::in_memory(&workers, b"super-strong-master-key-32bytes!").unwrap();
        assert!(
            plane
                .postgres_schema_sql()
                .contains("CREATE TABLE IF NOT EXISTS workspaces")
        );

        let ws_a = plane
            .provision_workspace("ws_alpha", "t1", "Alpha", "tok-alpha")
            .unwrap();
        let ws_b = plane
            .provision_workspace("ws_beta", "t2", "Beta", "tok-beta")
            .unwrap();
        assert_ne!(ws_a.worker_dir, ws_b.worker_dir);

        // Cross-tenant auth denied
        assert!(plane.authorize_workspace("ws_alpha", "tok-alpha").is_ok());
        assert!(plane.authorize_workspace("ws_alpha", "tok-beta").is_err());

        // Tenant-isolated encrypted secret storage
        let vault_a = plane.tenant_secret_backend("ws_alpha").unwrap();
        let vault_b = plane.tenant_secret_backend("ws_beta").unwrap();
        vault_a.set_secret("openai_key", "sk-alpha-only").unwrap();
        assert_eq!(vault_a.get_secret("openai_key").unwrap(), "sk-alpha-only");
        assert!(vault_b.get_secret("openai_key").is_err());

        // Dry run import + backup + restore + cross-tenant restore block
        std::fs::write(Path::new(&ws_a.worker_dir).join("SOUL.md"), "# Alpha Bot").unwrap();
        let dry = plane
            .dry_run_import(
                "ws_alpha",
                &BTreeMap::from([("USER.md".into(), "Hi".into())]),
                1,
            )
            .unwrap();
        assert_eq!(dry.files_planned, 1);
        let bak = plane.backup_workspace("ws_alpha").unwrap();
        assert_eq!(
            bak.encrypted_secret_accounts,
            vec!["openai_key".to_string()]
        );
        assert!(plane.restore_workspace("ws_beta", &bak).is_err());
        std::fs::remove_file(Path::new(&ws_a.worker_dir).join("SOUL.md")).unwrap();
        assert_eq!(plane.restore_workspace("ws_alpha", &bak).unwrap(), 1);
        assert_eq!(
            std::fs::read_to_string(Path::new(&ws_a.worker_dir).join("SOUL.md")).unwrap(),
            "# Alpha Bot"
        );
    }
}
