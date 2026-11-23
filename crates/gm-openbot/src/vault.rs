//! Non-secret SQLite metadata (`vault_items_v1`) paired with OS Keychain (`KeychainSecrets`) /
//! `1Password CLI` secret storage, plus streaming `< 5 MB` CSV importer for 1Password, Chrome,
//! Edge, Safari, Firefox, Bitwarden, and LastPass.
//!
//! `RULE-SECRET-001`: Plaintext passwords, CVVs, API keys, and tokens NEVER enter SQLite or logs.

use chrono::Utc;
use gm_core::database::Database;
use gm_core::error::{GmError, Result};
use gm_llm::KeychainSecrets;
use parking_lot::Mutex;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

pub const MAX_VAULT_CSV_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_VAULT_CSV_ROWS: usize = 10_000;

pub trait SecretBackend: Send + Sync {
    fn get_secret(&self, account: &str) -> Result<String>;
    fn set_secret(&self, account: &str, secret: &str) -> Result<()>;
    fn delete_secret(&self, account: &str) -> Result<()>;
}

#[derive(Clone, Default)]
pub struct KeychainSecretBackend {
    inner: KeychainSecrets,
}

impl KeychainSecretBackend {
    pub fn new(inner: KeychainSecrets) -> Self {
        Self { inner }
    }
}

impl SecretBackend for KeychainSecretBackend {
    fn get_secret(&self, account: &str) -> Result<String> {
        self.inner.get_secret(account)
    }

    fn set_secret(&self, account: &str, secret: &str) -> Result<()> {
        self.inner.set_secret(account, secret)
    }

    fn delete_secret(&self, account: &str) -> Result<()> {
        self.inner.delete_secret(account)
    }
}

/// Headless-safe in-memory secret backend for deterministic tests without OS dbus keyring.
#[derive(Clone, Default)]
pub struct InMemorySecretBackend {
    map: Arc<Mutex<BTreeMap<String, String>>>,
}

impl SecretBackend for InMemorySecretBackend {
    fn get_secret(&self, account: &str) -> Result<String> {
        self.map
            .lock()
            .get(account)
            .cloned()
            .ok_or_else(|| GmError::Config(format!("Secret '{account}' not found")))
    }

    fn set_secret(&self, account: &str, secret: &str) -> Result<()> {
        self.map
            .lock()
            .insert(account.to_string(), secret.to_string());
        Ok(())
    }

    fn delete_secret(&self, account: &str) -> Result<()> {
        self.map.lock().remove(account);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VaultItemKind {
    Login,
    Card,
    ContactInfo,
    Hint,
    PaymentMethod,
    PasswordManager,
}

impl VaultItemKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            VaultItemKind::Login => "login",
            VaultItemKind::Card => "card",
            VaultItemKind::ContactInfo => "contact_info",
            VaultItemKind::Hint => "hint",
            VaultItemKind::PaymentMethod => "payment_method",
            VaultItemKind::PasswordManager => "password_manager",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "login" | "logins" => Ok(VaultItemKind::Login),
            "card" | "cards" => Ok(VaultItemKind::Card),
            "contact_info" | "contact" | "contacts" => Ok(VaultItemKind::ContactInfo),
            "hint" | "hints" => Ok(VaultItemKind::Hint),
            "payment_method" | "payment" | "payments" => Ok(VaultItemKind::PaymentMethod),
            "password_manager" | "password_managers" => Ok(VaultItemKind::PasswordManager),
            other => Err(GmError::Config(format!("Unsupported vault kind '{other}'"))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultItemRecord {
    pub id: String,
    pub kind: VaultItemKind,
    pub label: String,
    #[serde(default)]
    pub subtitle_or_last4: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub hint: String,
    #[serde(default)]
    pub secret_ref: Option<String>,
    pub has_secret: bool,
    pub is_default_payment: bool,
    #[serde(default)]
    pub bot_scope: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VaultItemInput {
    #[serde(default)]
    pub id: Option<String>,
    pub kind: VaultItemKind,
    pub label: String,
    #[serde(default)]
    pub subtitle_or_last4: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub hint: String,
    #[serde(default)]
    pub secret_value: Option<String>,
    #[serde(default)]
    pub external_secret_ref: Option<String>,
    #[serde(default)]
    pub is_default_payment: bool,
    #[serde(default)]
    pub bot_scope: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultCsvImportReport {
    pub detected_format: String,
    pub imported_count: usize,
    pub skipped_count: usize,
    pub warnings: Vec<String>,
}

#[derive(Clone)]
pub struct VaultStore {
    conn: Arc<Mutex<Database>>,
    secrets: Arc<dyn SecretBackend>,
}

impl VaultStore {
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        let conn = gm_core::database::Database::open(db_path.as_ref())?;
        Self::init_conn(conn, Arc::new(KeychainSecretBackend::default()))
    }

    pub fn open_with_backend(
        db_path: impl AsRef<Path>,
        secrets: Arc<dyn SecretBackend>,
    ) -> Result<Self> {
        let conn = gm_core::database::Database::open(db_path.as_ref())?;
        Self::init_conn(conn, secrets)
    }

    pub fn in_memory_with_backend(secrets: Arc<dyn SecretBackend>) -> Result<Self> {
        let conn = Database::in_memory()
            .map_err(|e| GmError::Platform(format!("Failed to open in-memory vault DB: {e}")))?;
        Self::init_conn(conn, secrets)
    }

    fn init_conn(conn: Database, secrets: Arc<dyn SecretBackend>) -> Result<Self> {
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS vault_items_v1 (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                label TEXT NOT NULL,
                subtitle_or_last4 TEXT NOT NULL DEFAULT '',
                username TEXT NOT NULL DEFAULT '',
                domain TEXT NOT NULL DEFAULT '',
                hint TEXT NOT NULL DEFAULT '',
                secret_ref TEXT,
                has_secret INTEGER NOT NULL DEFAULT 0,
                is_default_payment INTEGER NOT NULL DEFAULT 0,
                bot_scope TEXT,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_vault_kind ON vault_items_v1(kind, label);
            "#,
        )
        .map_err(|e| GmError::Platform(format!("VaultStore schema init failed: {e}")))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            secrets,
        })
    }

    pub fn backend(&self) -> &Arc<dyn SecretBackend> {
        &self.secrets
    }

    pub fn list_items(&self, kind: Option<VaultItemKind>) -> Result<Vec<VaultItemRecord>> {
        let conn = self.conn.lock();
        let sql = if kind.is_some() {
            r#"
            SELECT id, kind, label, subtitle_or_last4, username, domain, hint, secret_ref, has_secret, is_default_payment, bot_scope, updated_at
            FROM vault_items_v1 WHERE kind = ?1 ORDER BY is_default_payment DESC, updated_at DESC
            "#
        } else {
            r#"
            SELECT id, kind, label, subtitle_or_last4, username, domain, hint, secret_ref, has_secret, is_default_payment, bot_scope, updated_at
            FROM vault_items_v1 ORDER BY kind ASC, is_default_payment DESC, updated_at DESC
            "#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let map_row = |r: &rusqlite::Row<'_>| -> rusqlite::Result<VaultItemRecord> {
            let kind_str: String = r.get(1)?;
            let kind = VaultItemKind::parse(&kind_str).unwrap_or(VaultItemKind::Login);
            Ok(VaultItemRecord {
                id: r.get(0)?,
                kind,
                label: r.get(2)?,
                subtitle_or_last4: r.get(3)?,
                username: r.get(4)?,
                domain: r.get(5)?,
                hint: r.get(6)?,
                secret_ref: r.get(7)?,
                has_secret: r.get::<_, i32>(8)? != 0,
                is_default_payment: r.get::<_, i32>(9)? != 0,
                bot_scope: r.get(10)?,
                updated_at: r.get(11)?,
            })
        };
        let rows = if let Some(k) = kind {
            stmt.query_map(params![k.as_str()], map_row)
        } else {
            stmt.query_map([], map_row)
        }
        .map_err(|e| GmError::Platform(e.to_string()))?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| GmError::Platform(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn upsert_item(&self, input: VaultItemInput) -> Result<VaultItemRecord> {
        let label = input.label.trim();
        if label.is_empty() || label.len() > 256 {
            return Err(GmError::Config(
                "Vault label must be 1-256 characters".into(),
            ));
        }
        let id = match input.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(existing) => existing.to_string(),
            None => format!("vi-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]),
        };

        let mut secret_ref = input
            .external_secret_ref
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let mut has_secret = secret_ref.is_some();

        // Store any raw secret ONLY via OS Keychain / SecretBackend (never SQLite!).
        if let Some(ref secret_val) = input.secret_value
            && !secret_val.is_empty()
        {
            let account = format!("gm_vault_{id}");
            self.secrets.set_secret(&account, secret_val)?;
            secret_ref = Some(format!("secret://keychain/{account}"));
            has_secret = true;
        }

        let clean_scope = input
            .bot_scope
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_lowercase());
        let now = Utc::now().to_rfc3339();

        let conn = self.conn.lock();
        if secret_ref.is_none()
            && let Some((old_ref, old_has)) = conn
                .query_row(
                    "SELECT secret_ref, has_secret FROM vault_items_v1 WHERE id = ?1",
                    params![&id],
                    |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i32>(1)? != 0)),
                )
                .optional()
                .map_err(|e| GmError::Platform(e.to_string()))?
        {
            secret_ref = old_ref;
            has_secret = old_has;
        }

        if input.is_default_payment
            && matches!(
                input.kind,
                VaultItemKind::Card | VaultItemKind::PaymentMethod
            )
        {
            let _ = conn.execute(
                "UPDATE vault_items_v1 SET is_default_payment = 0 WHERE kind IN ('card', 'payment_method')",
                [],
            );
        }

        conn.execute(
            r#"
            INSERT INTO vault_items_v1(
                id, kind, label, subtitle_or_last4, username, domain, hint,
                secret_ref, has_secret, is_default_payment, bot_scope, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(id) DO UPDATE SET
                kind = excluded.kind,
                label = excluded.label,
                subtitle_or_last4 = excluded.subtitle_or_last4,
                username = excluded.username,
                domain = excluded.domain,
                hint = excluded.hint,
                secret_ref = excluded.secret_ref,
                has_secret = excluded.has_secret,
                is_default_payment = excluded.is_default_payment,
                bot_scope = excluded.bot_scope,
                updated_at = excluded.updated_at
            "#,
            params![
                id,
                input.kind.as_str(),
                label,
                input.subtitle_or_last4.trim(),
                input.username.trim(),
                input.domain.trim(),
                input.hint.trim(),
                secret_ref,
                has_secret as i32,
                input.is_default_payment as i32,
                clean_scope,
                now
            ],
        )
        .map_err(|e| GmError::Platform(e.to_string()))?;

        Ok(VaultItemRecord {
            id,
            kind: input.kind,
            label: label.to_string(),
            subtitle_or_last4: input.subtitle_or_last4.trim().to_string(),
            username: input.username.trim().to_string(),
            domain: input.domain.trim().to_string(),
            hint: input.hint.trim().to_string(),
            secret_ref,
            has_secret,
            is_default_payment: input.is_default_payment,
            bot_scope: clean_scope,
            updated_at: now,
        })
    }

    pub fn reveal_secret(
        &self,
        id: &str,
        require_hw_confirm: bool,
        confirmed: bool,
    ) -> Result<String> {
        if require_hw_confirm && !confirmed {
            return Err(GmError::SafetyViolation(
                "Hardware security confirmation is required before revealing vault secrets".into(),
            ));
        }
        let secret_ref: Option<String> = self
            .conn
            .lock()
            .query_row(
                "SELECT secret_ref FROM vault_items_v1 WHERE id = ?1",
                params![id.trim()],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| GmError::Platform(e.to_string()))?
            .flatten();
        let Some(ref_uri) = secret_ref else {
            return Err(GmError::Config("Vault item has no stored secret".into()));
        };
        if let Some(account) = ref_uri.strip_prefix("secret://keychain/") {
            self.secrets.get_secret(account)
        } else {
            Err(GmError::Config(format!(
                "Reveal unavailable for external reference '{ref_uri}'"
            )))
        }
    }

    pub fn set_default_payment(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM vault_items_v1 WHERE id = ?1 AND kind IN ('card', 'payment_method')",
                params![id.trim()],
                |r| r.get(0),
            )
            .unwrap_or(false);
        if !exists {
            return Ok(false);
        }
        conn.execute(
            "UPDATE vault_items_v1 SET is_default_payment = 0 WHERE kind IN ('card', 'payment_method')",
            [],
        )
        .map_err(|e| GmError::Platform(e.to_string()))?;
        conn.execute(
            "UPDATE vault_items_v1 SET is_default_payment = 1 WHERE id = ?1",
            params![id.trim()],
        )
        .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(true)
    }

    pub fn delete_item(&self, id: &str) -> Result<bool> {
        let secret_ref: Option<String> = self
            .conn
            .lock()
            .query_row(
                "SELECT secret_ref FROM vault_items_v1 WHERE id = ?1",
                params![id.trim()],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| GmError::Platform(e.to_string()))?
            .flatten();
        if let Some(ref_uri) = secret_ref
            && let Some(account) = ref_uri.strip_prefix("secret://keychain/")
        {
            let _ = self.secrets.delete_secret(account);
        }
        let rows = self
            .conn
            .lock()
            .execute(
                "DELETE FROM vault_items_v1 WHERE id = ?1",
                params![id.trim()],
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(rows > 0)
    }

    /// Streams and imports CSV exports from 1Password, Chrome/Edge, Safari, Firefox, Bitwarden, LastPass.
    /// Strictly rejects > 5 MiB CSVs and stores passwords strictly via `SecretBackend` (never SQLite).
    pub fn import_csv(&self, csv: &str, source_hint: Option<&str>) -> Result<VaultCsvImportReport> {
        if csv.len() > MAX_VAULT_CSV_BYTES {
            return Err(GmError::Config(
                "CSV file exceeds the 5 MiB Vault import size limit".into(),
            ));
        }
        let stripped = csv.strip_prefix('\u{feff}').unwrap_or(csv);
        let rows = parse_rfc4180_csv(stripped, MAX_VAULT_CSV_ROWS + 1)?;
        if rows.len() < 2 {
            return Err(GmError::Config(
                "CSV must contain a header row and at least one credential row".into(),
            ));
        }

        let headers: Vec<String> = rows[0]
            .iter()
            .map(|h| h.trim().to_ascii_lowercase())
            .collect();
        let find_col = |candidates: &[&str]| -> Option<usize> {
            candidates
                .iter()
                .find_map(|c| headers.iter().position(|h| h == c))
        };

        let col_title = find_col(&["name", "title", "label", "login_title", "grouping"]);
        let col_url = find_col(&["url", "urls", "login_uri", "website", "hostname", "uri"]);
        let col_user = find_col(&["username", "login_username", "user", "email"]);
        let col_pass = find_col(&["password", "login_password", "pass", "secret"]);
        let col_notes = find_col(&["notes", "note", "extra", "hint"]);

        if col_user.is_none() && col_pass.is_none() {
            return Err(GmError::Config(
                "CSV headers not recognized (expected username/password or login_username/login_password)".into(),
            ));
        }

        let detected_format = source_hint
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| detect_csv_format(&headers));

        let mut imported_count = 0usize;
        let mut skipped_count = 0usize;
        let mut warnings = Vec::new();

        for (idx, row) in rows.iter().skip(1).enumerate() {
            if idx >= MAX_VAULT_CSV_ROWS {
                warnings.push(format!(
                    "Truncated CSV import after {MAX_VAULT_CSV_ROWS} rows"
                ));
                break;
            }
            let get = |col: Option<usize>| -> String {
                col.and_then(|i| row.get(i))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default()
            };
            let title = get(col_title);
            let url = get(col_url);
            let user = get(col_user);
            let pass = get(col_pass);
            let notes = get(col_notes);

            if title.is_empty() && url.is_empty() && user.is_empty() && pass.is_empty() {
                skipped_count += 1;
                continue;
            }

            let label = if !title.is_empty() {
                title
            } else if !url.is_empty() {
                extract_domain(&url)
            } else {
                user.clone()
            };
            let domain = extract_domain(&url);

            match self.upsert_item(VaultItemInput {
                id: None,
                kind: VaultItemKind::Login,
                label: label.chars().take(128).collect(),
                subtitle_or_last4: if user.is_empty() {
                    domain.clone()
                } else {
                    user.clone()
                },
                username: user,
                domain,
                hint: notes.chars().take(160).collect(),
                secret_value: if pass.is_empty() { None } else { Some(pass) },
                external_secret_ref: None,
                is_default_payment: false,
                bot_scope: None,
            }) {
                Ok(_) => imported_count += 1,
                Err(e) => {
                    skipped_count += 1;
                    if warnings.len() < 10 {
                        warnings.push(format!("Row {}: {e}", idx + 2));
                    }
                }
            }
        }

        Ok(VaultCsvImportReport {
            detected_format,
            imported_count,
            skipped_count,
            warnings,
        })
    }
}

fn detect_csv_format(headers: &[String]) -> String {
    let joined = headers.join(",");
    if joined.contains("login_uri") || joined.contains("login_username") {
        "Bitwarden".into()
    } else if joined.contains("httprealm") || joined.contains("formactionorigin") {
        "Firefox".into()
    } else if joined.contains("otpauth")
        || joined.contains("notes") && joined.contains("url") && joined.contains("title")
    {
        "Safari / Apple Passwords".into()
    } else if joined.contains("grouping") || joined.contains("extra") || joined.contains("totp") {
        "LastPass / 1Password".into()
    } else if joined.contains("name") && joined.contains("url") && joined.contains("username") {
        "Chrome / Edge / 1Password".into()
    } else {
        "Generic CSV".into()
    }
}

fn extract_domain(url_or_host: &str) -> String {
    let s = url_or_host.trim();
    let no_scheme = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    no_scheme
        .split('/')
        .next()
        .unwrap_or(no_scheme)
        .split(':')
        .next()
        .unwrap_or(no_scheme)
        .trim()
        .chars()
        .take(128)
        .collect()
}

fn parse_rfc4180_csv(input: &str, max_rows: usize) -> Result<Vec<Vec<String>>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else {
            match c {
                '"' if field.is_empty() => in_quotes = true,
                ',' => {
                    row.push(std::mem::take(&mut field));
                }
                '\r' => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                    if rows.len() >= max_rows {
                        return Ok(rows);
                    }
                }
                '\n' => {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                    if rows.len() >= max_rows {
                        return Ok(rows);
                    }
                }
                _ => field.push(c),
            }
        }
    }
    if in_quotes {
        return Err(GmError::Config(
            "Malformed CSV: unclosed quoted field".into(),
        ));
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_keeps_secrets_out_of_sqlite_and_imports_password_manager_csvs() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("vault.sqlite");
        let backend = Arc::new(InMemorySecretBackend::default());
        let vault = VaultStore::open_with_backend(&db, backend.clone()).unwrap();

        let item = vault
            .upsert_item(VaultItemInput {
                id: None,
                kind: VaultItemKind::Login,
                label: "GitHub Work".into(),
                subtitle_or_last4: "alice@acme.com".into(),
                username: "alice@acme.com".into(),
                domain: "github.com".into(),
                hint: "SSO".into(),
                secret_value: Some("ghp_UltraSecret999".into()),
                external_secret_ref: None,
                is_default_payment: false,
                bot_scope: None,
            })
            .unwrap();
        assert!(item.has_secret);
        assert!(
            item.secret_ref
                .as_deref()
                .unwrap()
                .starts_with("secret://keychain/")
        );

        // Verify hardware security key enforcement
        assert!(vault.reveal_secret(&item.id, true, false).is_err());
        assert_eq!(
            vault.reveal_secret(&item.id, true, true).unwrap(),
            "ghp_UltraSecret999"
        );

        // Verify plaintext secret NEVER appears in SQLite file or serialized item
        let sqlite_bytes = std::fs::read(&db).unwrap();
        let sqlite_lossy = String::from_utf8_lossy(&sqlite_bytes);
        assert!(!sqlite_lossy.contains("ghp_UltraSecret999"));
        assert!(
            !serde_json::to_string(&item)
                .unwrap()
                .contains("ghp_UltraSecret999")
        );

        // Test CSV import (1Password/Chrome mixed style with quoted comma notes)
        let csv = "\u{feff}name,url,username,password,note\n\
                   \"Stripe Live\",https://dashboard.stripe.com/login,ops@acme.com,\"sk_test_CsvSecret1\",\"Billing, main\"\n\
                   Jira,https://acme.atlassian.net,bob,PassCsv2,Work";
        let report = vault.import_csv(csv, None).unwrap();
        assert_eq!(report.imported_count, 2);
        assert_eq!(report.skipped_count, 0);

        let all = vault.list_items(Some(VaultItemKind::Login)).unwrap();
        assert_eq!(all.len(), 3);
        let stripe = all.iter().find(|i| i.label == "Stripe Live").unwrap();
        assert_eq!(stripe.domain, "dashboard.stripe.com");
        assert_eq!(
            vault.reveal_secret(&stripe.id, false, true).unwrap(),
            "sk_test_CsvSecret1"
        );

        // Verify > 5 MiB CSV rejected
        let huge = format!(
            "name,url,username,password\n{}",
            "a".repeat(MAX_VAULT_CSV_BYTES + 10)
        );
        assert!(vault.import_csv(&huge, None).is_err());
    }
}
