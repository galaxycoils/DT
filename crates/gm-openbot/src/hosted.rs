//! Hosted multi-tenant control plane: SQLx PostgreSQL schema & versioned migrations,
//! membership-derived workspace authorization, isolated per-workspace worker directories,
//! AES-256-GCM + HKDF-SHA256 tenant secret encryption, expiring enrollment/sessions, and
//! dry-run import / backup / restore pipelines.

use crate::vault::SecretBackend;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use gm_core::error::{GmError, Result};
use hkdf::Hkdf;
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// External PostgreSQL 15+ versioned migrations executed by `PostgresHostedStore`.
pub const POSTGRES_MIGRATION_V1: &str = r#"
CREATE TABLE IF NOT EXISTS _dt_hosted_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
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
CREATE TABLE IF NOT EXISTS workspace_memberships (
    ws_id TEXT NOT NULL REFERENCES workspaces(ws_id) ON DELETE CASCADE,
    user_id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'member',
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (ws_id, user_id)
);
CREATE TABLE IF NOT EXISTS encrypted_secrets (
    ws_id TEXT NOT NULL REFERENCES workspaces(ws_id) ON DELETE CASCADE,
    account TEXT NOT NULL,
    nonce_b64 TEXT NOT NULL,
    ciphertext_b64 TEXT NOT NULL,
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

pub const POSTGRES_MIGRATION_V2: &str = r#"
CREATE TABLE IF NOT EXISTS hosted_enrollments (
    token_sha256 TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    ws_id TEXT NOT NULL REFERENCES workspaces(ws_id) ON DELETE CASCADE,
    tenant_id TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'member',
    expires_at TIMESTAMPTZ NOT NULL,
    redeemed_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE IF NOT EXISTS hosted_sessions (
    session_sha256 TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    ws_id TEXT NOT NULL REFERENCES workspaces(ws_id) ON DELETE CASCADE,
    tenant_id TEXT NOT NULL,
    role TEXT NOT NULL,
    csrf_token TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
"#;

pub const POSTGRES_MIGRATION_V3: &str = r#"
ALTER TABLE workspaces ENABLE ROW LEVEL SECURITY;
ALTER TABLE workspace_memberships ENABLE ROW LEVEL SECURITY;
ALTER TABLE encrypted_secrets ENABLE ROW LEVEL SECURITY;
ALTER TABLE workspace_backups ENABLE ROW LEVEL SECURITY;
ALTER TABLE hosted_enrollments ENABLE ROW LEVEL SECURITY;
ALTER TABLE hosted_sessions ENABLE ROW LEVEL SECURITY;

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE policyname = 'dt_ws_isolation_workspaces') THEN
        CREATE POLICY dt_ws_isolation_workspaces ON workspaces
            USING (ws_id = NULLIF(current_setting('app.current_ws_id', true), ''));
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE policyname = 'dt_ws_isolation_memberships') THEN
        CREATE POLICY dt_ws_isolation_memberships ON workspace_memberships
            USING (ws_id = NULLIF(current_setting('app.current_ws_id', true), ''));
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE policyname = 'dt_ws_isolation_secrets') THEN
        CREATE POLICY dt_ws_isolation_secrets ON encrypted_secrets
            USING (ws_id = NULLIF(current_setting('app.current_ws_id', true), ''));
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE policyname = 'dt_ws_isolation_backups') THEN
        CREATE POLICY dt_ws_isolation_backups ON workspace_backups
            USING (ws_id = NULLIF(current_setting('app.current_ws_id', true), ''));
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE policyname = 'dt_ws_isolation_enrollments') THEN
        CREATE POLICY dt_ws_isolation_enrollments ON hosted_enrollments
            USING (ws_id = NULLIF(current_setting('app.current_ws_id', true), ''));
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE policyname = 'dt_ws_isolation_sessions') THEN
        CREATE POLICY dt_ws_isolation_sessions ON hosted_sessions
            USING (ws_id = NULLIF(current_setting('app.current_ws_id', true), ''));
    END IF;
END
$$;
"#;

/// Combined PostgreSQL schema exported for reference and deployment bootstrap.
pub const POSTGRES_CONTROL_PLANE_SCHEMA: &str = const_str_concat_placeholder();

const fn const_str_concat_placeholder() -> &'static str {
    concat!(
        "CREATE TABLE IF NOT EXISTS _dt_hosted_migrations (version INTEGER PRIMARY KEY, applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW());\n",
        "CREATE TABLE IF NOT EXISTS tenants (tenant_id TEXT PRIMARY KEY, owner_email TEXT NOT NULL, billing_mode TEXT NOT NULL DEFAULT 'hosted', created_at TIMESTAMPTZ NOT NULL DEFAULT NOW());\n",
        "CREATE TABLE IF NOT EXISTS workspaces (ws_id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL REFERENCES tenants(tenant_id) ON DELETE CASCADE, name TEXT NOT NULL, worker_dir TEXT NOT NULL UNIQUE, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW());\n",
        "CREATE TABLE IF NOT EXISTS workspace_memberships (ws_id TEXT NOT NULL REFERENCES workspaces(ws_id) ON DELETE CASCADE, user_id TEXT NOT NULL, tenant_id TEXT NOT NULL, role TEXT NOT NULL DEFAULT 'member', revoked_at TIMESTAMPTZ, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), PRIMARY KEY (ws_id, user_id));\n",
        "CREATE TABLE IF NOT EXISTS encrypted_secrets (ws_id TEXT NOT NULL REFERENCES workspaces(ws_id) ON DELETE CASCADE, account TEXT NOT NULL, nonce_b64 TEXT NOT NULL, ciphertext_b64 TEXT NOT NULL, updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), PRIMARY KEY (ws_id, account));\n",
        "CREATE TABLE IF NOT EXISTS workspace_backups (backup_id TEXT PRIMARY KEY, ws_id TEXT NOT NULL REFERENCES workspaces(ws_id) ON DELETE CASCADE, sha256 TEXT NOT NULL, snapshot_json TEXT NOT NULL, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW());\n",
        "CREATE TABLE IF NOT EXISTS hosted_enrollments (token_sha256 TEXT PRIMARY KEY, user_id TEXT NOT NULL, ws_id TEXT NOT NULL REFERENCES workspaces(ws_id) ON DELETE CASCADE, tenant_id TEXT NOT NULL, role TEXT NOT NULL DEFAULT 'member', expires_at TIMESTAMPTZ NOT NULL, redeemed_at TIMESTAMPTZ, revoked_at TIMESTAMPTZ, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW());\n",
        "CREATE TABLE IF NOT EXISTS hosted_sessions (session_sha256 TEXT PRIMARY KEY, user_id TEXT NOT NULL, ws_id TEXT NOT NULL REFERENCES workspaces(ws_id) ON DELETE CASCADE, tenant_id TEXT NOT NULL, role TEXT NOT NULL, csrf_token TEXT NOT NULL, expires_at TIMESTAMPTZ NOT NULL, revoked_at TIMESTAMPTZ, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW());\n"
    )
}

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostedMembership {
    pub ws_id: String,
    pub user_id: String,
    pub tenant_id: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostedEnrollmentIssued {
    pub enrollment_token: String,
    pub user_id: String,
    pub ws_id: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostedSessionIssued {
    pub session_id: String,
    pub csrf_token: String,
    pub user_id: String,
    pub ws_id: String,
    pub tenant_id: String,
    pub role: String,
    pub expires_at: String,
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

/// Derive a 256-bit workspace key using HKDF-SHA256 (`salt = dt-hosted-hkdf-v1`, `info = ws_id:account`).
fn derive_hkdf_aes_gcm_key(master_key: &[u8], ws_id: &str, account: &str) -> Result<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(Some(b"dt-hosted-tenant-secret-hkdf-v1"), master_key);
    let info = format!("ws:{ws_id}:account:{account}");
    let mut okm = [0u8; 32];
    hk.expand(info.as_bytes(), &mut okm)
        .map_err(|_| GmError::Platform("HKDF key expansion failed".into()))?;
    Ok(okm)
}

fn encrypt_secret_aes_gcm(
    master_key: &[u8],
    ws_id: &str,
    account: &str,
    plaintext: &[u8],
) -> Result<(String, String)> {
    let key = derive_hkdf_aes_gcm_key(master_key, ws_id, account)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| GmError::Platform(format!("AES-256-GCM key init error: {e}")))?;
    let rand_bytes = *uuid::Uuid::new_v4().as_bytes();
    let nonce_bytes: [u8; 12] = rand_bytes[..12]
        .try_into()
        .map_err(|_| GmError::Platform("Nonce generation error".into()))?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let aad = format!("{ws_id}:{account}");
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| GmError::Platform("AES-256-GCM encryption failed".into()))?;
    Ok((
        base64::engine::general_purpose::STANDARD.encode(nonce_bytes),
        base64::engine::general_purpose::STANDARD.encode(ciphertext),
    ))
}

fn decrypt_secret_aes_gcm(
    master_key: &[u8],
    ws_id: &str,
    account: &str,
    nonce_b64: &str,
    ciphertext_b64: &str,
) -> Result<String> {
    let nonce_vec = base64::engine::general_purpose::STANDARD
        .decode(nonce_b64)
        .map_err(|e| GmError::Platform(format!("Corrupt secret nonce: {e}")))?;
    let nonce_bytes: [u8; 12] = nonce_vec
        .try_into()
        .map_err(|_| GmError::Platform("Invalid AES-GCM nonce length".into()))?;
    let ct = base64::engine::general_purpose::STANDARD
        .decode(ciphertext_b64)
        .map_err(|e| GmError::Platform(format!("Corrupt ciphertext: {e}")))?;
    let key = derive_hkdf_aes_gcm_key(master_key, ws_id, account)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| GmError::Platform(format!("AES-256-GCM key init error: {e}")))?;
    let aad = format!("{ws_id}:{account}");
    let pt = cipher
        .decrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: &ct,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| {
            GmError::SafetyViolation("AES-256-GCM tenant secret authentication failed".into())
        })?;
    String::from_utf8(pt).map_err(|e| GmError::Platform(format!("Invalid UTF-8 secret: {e}")))
}

fn sha256_hex(input: &str) -> String {
    hex::encode(Sha256::digest(input.trim().as_bytes()))
}

fn random_secret_token(prefix: &str) -> String {
    format!(
        "{prefix}_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// SQLx PostgreSQL-backed hosted control plane for multi-tenant production deployments.
#[derive(Clone)]
pub struct PostgresHostedStore {
    pool: PgPool,
    workers_root: PathBuf,
    master_key: [u8; 32],
}

impl PostgresHostedStore {
    pub async fn connect(
        database_url: &str,
        workers_root: impl AsRef<Path>,
        master_key: &[u8],
    ) -> Result<Self> {
        if master_key.len() < 16 {
            return Err(GmError::Config(
                "Hosted master encryption key must be at least 16 bytes".into(),
            ));
        }
        let pool = PgPool::connect(database_url)
            .await
            .map_err(|e| GmError::Platform(format!("PostgreSQL connect failed: {e}")))?;
        let key: [u8; 32] = Sha256::digest(master_key).into();
        let store = Self {
            pool,
            workers_root: workers_root.as_ref().to_path_buf(),
            master_key: key,
        };
        store.run_migrations().await?;
        Ok(store)
    }

    pub async fn run_migrations(&self) -> Result<()> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| GmError::Platform(format!("PostgreSQL acquire failed: {e}")))?;
        sqlx::query("SELECT pg_advisory_lock(492381920144)")
            .execute(&mut *conn)
            .await
            .map_err(|e| GmError::Platform(format!("PostgreSQL migration lock failed: {e}")))?;
        let res = async {
            for (version, sql) in [
                (1_i32, POSTGRES_MIGRATION_V1),
                (2_i32, POSTGRES_MIGRATION_V2),
                (3_i32, POSTGRES_MIGRATION_V3),
            ] {
                sqlx::raw_sql(sql).execute(&mut *conn).await.map_err(|e| {
                    GmError::Platform(format!("PostgreSQL migration v{version} failed: {e}"))
                })?;
                sqlx::query(
                    "INSERT INTO _dt_hosted_migrations (version) VALUES ($1) ON CONFLICT (version) DO NOTHING",
                )
                .bind(version)
                .execute(&mut *conn)
                .await
                .map_err(|e| {
                    GmError::Platform(format!("PostgreSQL migration record failed: {e}"))
                })?;
            }
            Ok::<(), GmError>(())
        }
        .await;
        let _ = sqlx::query("SELECT pg_advisory_unlock(492381920144)")
            .execute(&mut *conn)
            .await;
        res
    }

    pub async fn ready_check(&self) -> Result<()> {
        let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _dt_hosted_migrations")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| GmError::Platform(format!("PostgreSQL readiness check failed: {e}")))?;
        if applied < 3 {
            return Err(GmError::Platform(
                "PostgreSQL hosted migrations are incomplete".into(),
            ));
        }
        std::fs::create_dir_all(&self.workers_root)?;
        let probe = self
            .workers_root
            .join(format!(".ready-probe-{}", uuid::Uuid::new_v4()));
        std::fs::write(&probe, b"ok")?;
        let _ = std::fs::remove_file(&probe);
        Ok(())
    }

    pub async fn provision_workspace(
        &self,
        ws_id: &str,
        tenant_id: &str,
        owner_email: &str,
        name: &str,
        owner_user_id: &str,
    ) -> Result<WorkspaceRecord> {
        let clean_ws = validate_workspace_slug(ws_id)?;
        let dir = workspace_worker_dir(&self.workers_root, &clean_ws)?;
        let dir_str = dir.display().to_string();
        sqlx::query(
            "INSERT INTO tenants (tenant_id, owner_email) VALUES ($1, $2) ON CONFLICT (tenant_id) DO NOTHING",
        )
        .bind(tenant_id)
        .bind(owner_email)
        .execute(&self.pool)
        .await
        .map_err(|e| GmError::Platform(e.to_string()))?;

        sqlx::query(
            "INSERT INTO workspaces (ws_id, tenant_id, name, worker_dir) VALUES ($1, $2, $3, $4)
             ON CONFLICT (ws_id) DO UPDATE SET name = EXCLUDED.name, worker_dir = EXCLUDED.worker_dir",
        )
        .bind(&clean_ws)
        .bind(tenant_id)
        .bind(name)
        .bind(&dir_str)
        .execute(&self.pool)
        .await
        .map_err(|e| GmError::Platform(e.to_string()))?;

        sqlx::query(
            "INSERT INTO workspace_memberships (ws_id, user_id, tenant_id, role, revoked_at)
             VALUES ($1, $2, $3, 'owner', NULL)
             ON CONFLICT (ws_id, user_id) DO UPDATE SET revoked_at = NULL",
        )
        .bind(&clean_ws)
        .bind(owner_user_id)
        .bind(tenant_id)
        .execute(&self.pool)
        .await
        .map_err(|e| GmError::Platform(e.to_string()))?;

        Ok(WorkspaceRecord {
            ws_id: clean_ws,
            tenant_id: tenant_id.to_string(),
            name: name.to_string(),
            worker_dir: dir_str,
            created_at: Utc::now().to_rfc3339(),
        })
    }

    pub async fn authorize_membership(
        &self,
        ws_id: &str,
        user_id: &str,
    ) -> Result<HostedMembership> {
        let clean_ws = validate_workspace_slug(ws_id)?;
        let row = sqlx::query(
            "SELECT ws_id, user_id, tenant_id, role
             FROM workspace_memberships
             WHERE ws_id = $1 AND user_id = $2 AND revoked_at IS NULL",
        )
        .bind(&clean_ws)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| GmError::Platform(e.to_string()))?;
        let Some(row) = row else {
            return Err(GmError::SafetyViolation(format!(
                "Workspace membership denied for user '{user_id}' on '{clean_ws}'"
            )));
        };
        Ok(HostedMembership {
            ws_id: row.get("ws_id"),
            user_id: row.get("user_id"),
            tenant_id: row.get("tenant_id"),
            role: row.get("role"),
        })
    }

    pub async fn set_secret(&self, ws_id: &str, account: &str, secret: &str) -> Result<()> {
        let clean_ws = validate_workspace_slug(ws_id)?;
        let (nonce_b64, ct_b64) =
            encrypt_secret_aes_gcm(&self.master_key, &clean_ws, account, secret.as_bytes())?;
        sqlx::query(
            "INSERT INTO encrypted_secrets (ws_id, account, nonce_b64, ciphertext_b64, updated_at)
             VALUES ($1, $2, $3, $4, NOW())
             ON CONFLICT (ws_id, account) DO UPDATE SET
               nonce_b64 = EXCLUDED.nonce_b64,
               ciphertext_b64 = EXCLUDED.ciphertext_b64,
               updated_at = NOW()",
        )
        .bind(&clean_ws)
        .bind(account)
        .bind(&nonce_b64)
        .bind(&ct_b64)
        .execute(&self.pool)
        .await
        .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(())
    }

    pub async fn get_secret(&self, ws_id: &str, account: &str) -> Result<String> {
        let clean_ws = validate_workspace_slug(ws_id)?;
        let row = sqlx::query(
            "SELECT nonce_b64, ciphertext_b64 FROM encrypted_secrets WHERE ws_id = $1 AND account = $2",
        )
        .bind(&clean_ws)
        .bind(account)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| GmError::Platform(e.to_string()))?;
        let Some(row) = row else {
            return Err(GmError::Config(format!(
                "Tenant secret '{account}' not found in workspace '{clean_ws}'"
            )));
        };
        let nonce_b64: String = row.get("nonce_b64");
        let ct_b64: String = row.get("ciphertext_b64");
        decrypt_secret_aes_gcm(&self.master_key, &clean_ws, account, &nonce_b64, &ct_b64)
    }
}

#[derive(Clone)]
pub struct HostedControlPlane {
    conn: Arc<Mutex<Connection>>,
    workers_root: PathBuf,
    master_key: [u8; 32],
    revocation_tx: tokio::sync::broadcast::Sender<(String, String)>,
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
            CREATE TABLE IF NOT EXISTS hosted_memberships_v1 (
                ws_id TEXT NOT NULL,
                user_id TEXT NOT NULL,
                tenant_id TEXT NOT NULL,
                role TEXT NOT NULL DEFAULT 'member',
                revoked_at TEXT,
                created_at TEXT NOT NULL,
                PRIMARY KEY (ws_id, user_id)
            );
            CREATE TABLE IF NOT EXISTS hosted_enrollments_v1 (
                token_sha256 TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                ws_id TEXT NOT NULL,
                tenant_id TEXT NOT NULL,
                role TEXT NOT NULL DEFAULT 'member',
                expires_at TEXT NOT NULL,
                redeemed_at TEXT,
                revoked_at TEXT,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS hosted_sessions_v1 (
                session_sha256 TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                ws_id TEXT NOT NULL,
                tenant_id TEXT NOT NULL,
                role TEXT NOT NULL,
                csrf_token TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                revoked_at TEXT,
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
        let (revocation_tx, _) = tokio::sync::broadcast::channel(64);
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            workers_root,
            master_key: key,
            revocation_tx,
        })
    }

    pub fn subscribe_revocations(&self) -> tokio::sync::broadcast::Receiver<(String, String)> {
        self.revocation_tx.subscribe()
    }

    pub fn ready_check(&self) -> Result<()> {
        let count: i64 = self
            .conn
            .lock()
            .query_row("SELECT COUNT(*) FROM hosted_workspaces_v1", [], |r| {
                r.get(0)
            })
            .map_err(|e| GmError::Platform(format!("Hosted control plane DB not ready: {e}")))?;
        let _ = count;
        std::fs::create_dir_all(&self.workers_root)?;
        let probe = self
            .workers_root
            .join(format!(".ready-probe-{}", uuid::Uuid::new_v4()));
        std::fs::write(&probe, b"ok")?;
        let _ = std::fs::remove_file(&probe);
        Ok(())
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
        let token_sha = sha256_hex(bearer_token);
        let now = Utc::now().to_rfc3339();
        let rec = WorkspaceRecord {
            ws_id: clean_ws.clone(),
            tenant_id: tenant_id.trim().to_string(),
            name: name.trim().to_string(),
            worker_dir: dir.display().to_string(),
            created_at: now.clone(),
        };
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO hosted_workspaces_v1 (ws_id, tenant_id, name, worker_dir, token_sha256, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(ws_id) DO UPDATE SET
               name = excluded.name,
               worker_dir = excluded.worker_dir,
               token_sha256 = excluded.token_sha256",
            params![rec.ws_id, rec.tenant_id, rec.name, rec.worker_dir, token_sha, now],
        )
        .map_err(|e| GmError::Platform(format!("Provision workspace failed: {e}")))?;
        conn.execute(
            "INSERT INTO hosted_memberships_v1 (ws_id, user_id, tenant_id, role, revoked_at, created_at)
             VALUES (?1, ?2, ?3, 'owner', NULL, ?4)
             ON CONFLICT(ws_id, user_id) DO UPDATE SET revoked_at = NULL",
            params![rec.ws_id, format!("owner:{}", rec.tenant_id), rec.tenant_id, now],
        )
        .map_err(|e| GmError::Platform(format!("Provision membership failed: {e}")))?;
        Ok(rec)
    }

    pub fn grant_membership(
        &self,
        ws_id: &str,
        user_id: &str,
        role: &str,
    ) -> Result<HostedMembership> {
        let clean_ws = validate_workspace_slug(ws_id)?;
        let conn = self.conn.lock();
        let tenant_id: String = conn
            .query_row(
                "SELECT tenant_id FROM hosted_workspaces_v1 WHERE ws_id = ?1",
                params![clean_ws],
                |r| r.get(0),
            )
            .map_err(|_| GmError::Config(format!("Unknown workspace '{ws_id}'")))?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO hosted_memberships_v1 (ws_id, user_id, tenant_id, role, revoked_at, created_at)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5)
             ON CONFLICT(ws_id, user_id) DO UPDATE SET role = excluded.role, revoked_at = NULL",
            params![clean_ws, user_id, tenant_id, role, now],
        )
        .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(HostedMembership {
            ws_id: clean_ws,
            user_id: user_id.to_string(),
            tenant_id,
            role: role.to_string(),
        })
    }

    pub fn authorize_membership(&self, ws_id: &str, user_id: &str) -> Result<HostedMembership> {
        let clean_ws = validate_workspace_slug(ws_id)?;
        let conn = self.conn.lock();
        let row: Option<(String, String, String, String)> = conn
            .query_row(
                "SELECT ws_id, user_id, tenant_id, role
                 FROM hosted_memberships_v1
                 WHERE ws_id = ?1 AND user_id = ?2 AND revoked_at IS NULL",
                params![clean_ws, user_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let Some((ws_id, user_id, tenant_id, role)) = row else {
            return Err(GmError::SafetyViolation(format!(
                "Workspace membership denied for user '{user_id}' on '{clean_ws}'"
            )));
        };
        Ok(HostedMembership {
            ws_id,
            user_id,
            tenant_id,
            role,
        })
    }

    pub fn revoke_membership(&self, ws_id: &str, user_id: &str) -> Result<()> {
        let clean_ws = validate_workspace_slug(ws_id)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE hosted_memberships_v1 SET revoked_at = ?1 WHERE ws_id = ?2 AND user_id = ?3",
            params![now, clean_ws, user_id],
        )
        .map_err(|e| GmError::Platform(e.to_string()))?;
        conn.execute(
            "UPDATE hosted_sessions_v1 SET revoked_at = ?1 WHERE ws_id = ?2 AND user_id = ?3",
            params![now, clean_ws, user_id],
        )
        .map_err(|e| GmError::Platform(e.to_string()))?;
        let _ = self.revocation_tx.send((clean_ws, user_id.to_string()));
        Ok(())
    }

    pub fn issue_enrollment(
        &self,
        ws_id: &str,
        user_id: &str,
        role: &str,
        ttl_seconds: i64,
    ) -> Result<HostedEnrollmentIssued> {
        let membership = self.grant_membership(ws_id, user_id, role)?;
        let token = random_secret_token("dtenroll");
        let token_sha = sha256_hex(&token);
        let now = Utc::now();
        let expires_at = (now + Duration::seconds(ttl_seconds.max(60))).to_rfc3339();
        self.conn
            .lock()
            .execute(
                "INSERT INTO hosted_enrollments_v1
                 (token_sha256, user_id, ws_id, tenant_id, role, expires_at, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    token_sha,
                    membership.user_id,
                    membership.ws_id,
                    membership.tenant_id,
                    membership.role,
                    expires_at,
                    now.to_rfc3339()
                ],
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(HostedEnrollmentIssued {
            enrollment_token: token,
            user_id: membership.user_id,
            ws_id: membership.ws_id,
            expires_at,
        })
    }

    pub fn redeem_enrollment(
        &self,
        enrollment_token: &str,
        session_ttl_seconds: i64,
    ) -> Result<HostedSessionIssued> {
        struct EnrollmentRow {
            user_id: String,
            ws_id: String,
            tenant_id: String,
            role: String,
            expires_raw: String,
            redeemed_at: Option<String>,
            revoked_at: Option<String>,
        }
        let token_sha = sha256_hex(enrollment_token);
        let now = Utc::now();
        let conn = self.conn.lock();
        let row: Option<EnrollmentRow> = conn
            .query_row(
                "SELECT user_id, ws_id, tenant_id, role, expires_at, redeemed_at, revoked_at
                 FROM hosted_enrollments_v1 WHERE token_sha256 = ?1",
                params![token_sha],
                |r| {
                    Ok(EnrollmentRow {
                        user_id: r.get(0)?,
                        ws_id: r.get(1)?,
                        tenant_id: r.get(2)?,
                        role: r.get(3)?,
                        expires_raw: r.get(4)?,
                        redeemed_at: r.get(5)?,
                        revoked_at: r.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let Some(EnrollmentRow {
            user_id,
            ws_id,
            tenant_id,
            role,
            expires_raw,
            redeemed_at,
            revoked_at,
        }) = row
        else {
            return Err(GmError::SafetyViolation("Invalid enrollment token".into()));
        };
        if revoked_at.is_some() || redeemed_at.is_some() {
            return Err(GmError::SafetyViolation(
                "Enrollment token has already been used or revoked".into(),
            ));
        }
        let expires = DateTime::parse_from_rfc3339(&expires_raw)
            .map_err(|_| GmError::SafetyViolation("Corrupt enrollment expiry".into()))?
            .with_timezone(&Utc);
        if expires <= now {
            return Err(GmError::SafetyViolation(
                "Enrollment token has expired".into(),
            ));
        }
        // Verify active membership before issuing session.
        let active_member: Option<String> = conn
            .query_row(
                "SELECT role FROM hosted_memberships_v1 WHERE ws_id = ?1 AND user_id = ?2 AND revoked_at IS NULL",
                params![ws_id, user_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| GmError::Platform(e.to_string()))?;
        if active_member.is_none() {
            return Err(GmError::SafetyViolation(
                "Workspace membership is revoked".into(),
            ));
        }

        conn.execute(
            "UPDATE hosted_enrollments_v1 SET redeemed_at = ?1 WHERE token_sha256 = ?2",
            params![now.to_rfc3339(), token_sha],
        )
        .map_err(|e| GmError::Platform(e.to_string()))?;

        let session_id = random_secret_token("dtsess");
        let csrf_token = random_secret_token("dtcsrf");
        let session_sha = sha256_hex(&session_id);
        let session_expires = (now + Duration::seconds(session_ttl_seconds.max(300))).to_rfc3339();
        conn.execute(
            "INSERT INTO hosted_sessions_v1
             (session_sha256, user_id, ws_id, tenant_id, role, csrf_token, expires_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                session_sha,
                user_id,
                ws_id,
                tenant_id,
                role,
                csrf_token,
                session_expires,
                now.to_rfc3339()
            ],
        )
        .map_err(|e| GmError::Platform(e.to_string()))?;

        Ok(HostedSessionIssued {
            session_id,
            csrf_token,
            user_id,
            ws_id,
            tenant_id,
            role,
            expires_at: session_expires,
        })
    }

    pub fn authorize_session(
        &self,
        session_id: &str,
        ws_id: Option<&str>,
    ) -> Result<HostedSessionIssued> {
        struct SessionRow {
            user_id: String,
            sess_ws: String,
            tenant_id: String,
            role: String,
            csrf_token: String,
            expires_raw: String,
            revoked_at: Option<String>,
        }
        let session_sha = sha256_hex(session_id);
        let conn = self.conn.lock();
        let row: Option<SessionRow> = conn
            .query_row(
                "SELECT s.user_id, s.ws_id, s.tenant_id, m.role, s.csrf_token, s.expires_at, s.revoked_at
                 FROM hosted_sessions_v1 s
                 JOIN hosted_memberships_v1 m ON m.ws_id = s.ws_id AND m.user_id = s.user_id
                 WHERE s.session_sha256 = ?1 AND m.revoked_at IS NULL",
                params![session_sha],
                |r| {
                    Ok(SessionRow {
                        user_id: r.get(0)?,
                        sess_ws: r.get(1)?,
                        tenant_id: r.get(2)?,
                        role: r.get(3)?,
                        csrf_token: r.get(4)?,
                        expires_raw: r.get(5)?,
                        revoked_at: r.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let Some(SessionRow {
            user_id,
            sess_ws,
            tenant_id,
            role,
            csrf_token,
            expires_raw,
            revoked_at,
        }) = row
        else {
            return Err(GmError::SafetyViolation(
                "Session is invalid or workspace membership was revoked".into(),
            ));
        };
        if revoked_at.is_some() {
            return Err(GmError::SafetyViolation("Session has been revoked".into()));
        }
        let expires = DateTime::parse_from_rfc3339(&expires_raw)
            .map_err(|_| GmError::SafetyViolation("Corrupt session expiry".into()))?
            .with_timezone(&Utc);
        if expires <= Utc::now() {
            return Err(GmError::SafetyViolation("Session has expired".into()));
        }
        if let Some(requested_ws) = ws_id {
            let clean = validate_workspace_slug(requested_ws)?;
            // If user holds active membership in requested_ws, allow switching; otherwise deny!
            let member: Option<(String, String)> = conn
                .query_row(
                    "SELECT tenant_id, role FROM hosted_memberships_v1
                     WHERE ws_id = ?1 AND user_id = ?2 AND revoked_at IS NULL",
                    params![clean, user_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(|e| GmError::Platform(e.to_string()))?;
            let Some((target_tenant, target_role)) = member else {
                return Err(GmError::SafetyViolation(format!(
                    "Cross-tenant access denied for workspace '{clean}'"
                )));
            };
            return Ok(HostedSessionIssued {
                session_id: session_id.to_string(),
                csrf_token,
                user_id,
                ws_id: clean,
                tenant_id: target_tenant,
                role: target_role,
                expires_at: expires_raw,
            });
        }
        Ok(HostedSessionIssued {
            session_id: session_id.to_string(),
            csrf_token,
            user_id,
            ws_id: sess_ws,
            tenant_id,
            role,
            expires_at: expires_raw,
        })
    }

    pub fn list_user_workspaces(&self, user_id: &str) -> Result<Vec<WorkspaceRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT w.ws_id, w.tenant_id, w.name, w.worker_dir, w.created_at
                 FROM hosted_workspaces_v1 w
                 JOIN hosted_memberships_v1 m ON m.ws_id = w.ws_id
                 WHERE m.user_id = ?1 AND m.revoked_at IS NULL
                 ORDER BY w.ws_id",
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let rows = stmt
            .query_map(params![user_id], |r| {
                Ok(WorkspaceRecord {
                    ws_id: r.get(0)?,
                    tenant_id: r.get(1)?,
                    name: r.get(2)?,
                    worker_dir: r.get(3)?,
                    created_at: r.get(4)?,
                })
            })
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| GmError::Platform(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn revoke_session(&self, session_id: &str) -> Result<()> {
        let session_sha = sha256_hex(session_id);
        let now = Utc::now().to_rfc3339();
        self.conn
            .lock()
            .execute(
                "UPDATE hosted_sessions_v1 SET revoked_at = ?1 WHERE session_sha256 = ?2",
                params![now, session_sha],
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(())
    }

    pub fn authorize_workspace(&self, ws_id: &str, bearer_token: &str) -> Result<WorkspaceRecord> {
        let clean_ws = validate_workspace_slug(ws_id)?;
        let candidate_hash = sha256_hex(bearer_token);
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
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT nonce_b64, ciphertext_b64 FROM hosted_encrypted_secrets_v1
                 WHERE ws_id = ?1 AND account = ?2",
                params![self.ws_id, account],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let Some((nonce_b64, ct_b64)) = row else {
            return Err(GmError::Config(format!(
                "Tenant secret '{account}' not found in workspace '{}'",
                self.ws_id
            )));
        };
        decrypt_secret_aes_gcm(
            &self.plane.master_key,
            &self.ws_id,
            account,
            &nonce_b64,
            &ct_b64,
        )
    }

    fn set_secret(&self, account: &str, secret: &str) -> Result<()> {
        let (nonce_b64, ct_b64) = encrypt_secret_aes_gcm(
            &self.plane.master_key,
            &self.ws_id,
            account,
            secret.as_bytes(),
        )?;
        self.plane
            .conn
            .lock()
            .execute(
                "INSERT INTO hosted_encrypted_secrets_v1 (ws_id, account, nonce_b64, ciphertext_b64, tag_hex, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'aes-256-gcm', ?5)
                 ON CONFLICT(ws_id, account) DO UPDATE SET
                   nonce_b64 = excluded.nonce_b64,
                   ciphertext_b64 = excluded.ciphertext_b64,
                   tag_hex = 'aes-256-gcm',
                   updated_at = excluded.updated_at",
                params![self.ws_id, account, nonce_b64, ct_b64, Utc::now().to_rfc3339()],
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
                .contains("CREATE TABLE IF NOT EXISTS workspace_memberships")
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

        // Enrollment, session, workspace switching, and revocation
        let enroll = plane
            .issue_enrollment("ws_alpha", "alice", "owner", 600)
            .unwrap();
        let sess = plane
            .redeem_enrollment(&enroll.enrollment_token, 3600)
            .unwrap();
        assert!(
            plane
                .redeem_enrollment(&enroll.enrollment_token, 3600)
                .is_err()
        );
        assert!(
            plane
                .authorize_session(&sess.session_id, Some("ws_alpha"))
                .is_ok()
        );
        assert!(
            plane
                .authorize_session(&sess.session_id, Some("ws_beta"))
                .is_err()
        );
        plane.revoke_membership("ws_alpha", "alice").unwrap();
        assert!(
            plane
                .authorize_session(&sess.session_id, Some("ws_alpha"))
                .is_err()
        );

        // AES-256-GCM + HKDF tenant-isolated encrypted secret storage + tampering rejection
        let vault_a = plane.tenant_secret_backend("ws_alpha").unwrap();
        let vault_b = plane.tenant_secret_backend("ws_beta").unwrap();
        vault_a.set_secret("openai_key", "sk-alpha-only").unwrap();
        assert_eq!(vault_a.get_secret("openai_key").unwrap(), "sk-alpha-only");
        assert!(vault_b.get_secret("openai_key").is_err());

        // Tamper ciphertext in DB -> AES-GCM authentication must fail.
        {
            let conn = plane.conn.lock();
            let bad_ct = base64::engine::general_purpose::STANDARD.encode(b"tampered-ciphertext");
            conn.execute(
                "UPDATE hosted_encrypted_secrets_v1 SET ciphertext_b64 = ?1 WHERE ws_id = 'ws_alpha'",
                params![bad_ct],
            )
            .unwrap();
        }
        assert!(vault_a.get_secret("openai_key").is_err());

        // Restore valid secret for backup check
        vault_a.set_secret("openai_key", "sk-alpha-only").unwrap();

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
