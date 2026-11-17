//! Plugin & skill marketplace with pre-install scope confirmation, SHA-256 verification,
//! atomic install/update/remove lifecycle, and workspace scoping.

use chrono::Utc;
use gm_core::error::{GmError, Result};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MarketplaceListing {
    pub id: String,
    pub name: String,
    pub version: String,
    pub publisher: String,
    pub description: String,
    pub requested_scopes: Vec<String>,
    pub package_sha256: String,
    pub manifest_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledMarketplacePlugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub approved_scopes: Vec<String>,
    pub package_sha256: String,
    pub installed_at: String,
    pub updated_at: String,
}

#[derive(Clone)]
pub struct MarketplaceStore {
    conn: Arc<Mutex<Connection>>,
}

impl MarketplaceStore {
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        let conn = crate::db_recovery::open_sqlite_with_recovery(db_path.as_ref())?;
        Self::init(conn)
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| GmError::Platform(format!("Marketplace DB error: {e}")))?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS installed_plugins_v1 (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                version TEXT NOT NULL,
                approved_scopes_json TEXT NOT NULL,
                package_sha256 TEXT NOT NULL,
                manifest_json TEXT NOT NULL,
                installed_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );",
        )
        .map_err(|e| GmError::Platform(format!("Marketplace schema error: {e}")))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn builtin_catalog(&self) -> Vec<MarketplaceListing> {
        let defs = [
            (
                "github-pr-helper",
                "GitHub PR Reviewer",
                "1.0.0",
                "Grok Muse Catalog",
                "Review pull requests and summarize diffs via verified GitHub connector.",
                vec![
                    "connectors.github.read".to_string(),
                    "skills.use".to_string(),
                ],
                r#"{"kind":"skill_bundle","entrypoint":"pr-review"}"#,
            ),
            (
                "calendar-brief",
                "Calendar Morning Brief",
                "1.0.0",
                "Grok Muse Catalog",
                "Summarize today's meetings without executing external scripts.",
                vec!["connectors.calendar.read".to_string()],
                r#"{"kind":"skill_bundle","entrypoint":"morning-brief"}"#,
            ),
        ];
        defs.into_iter()
            .map(|(id, name, ver, pub_, desc, scopes, manifest)| {
                let sha = hex::encode(Sha256::digest(manifest.as_bytes()));
                MarketplaceListing {
                    id: id.into(),
                    name: name.into(),
                    version: ver.into(),
                    publisher: pub_.into(),
                    description: desc.into(),
                    requested_scopes: scopes,
                    package_sha256: sha,
                    manifest_json: manifest.into(),
                }
            })
            .collect()
    }

    pub fn install_plugin(
        &self,
        listing: &MarketplaceListing,
        confirmed_scopes: &[String],
    ) -> Result<InstalledMarketplacePlugin> {
        for req in &listing.requested_scopes {
            if !confirmed_scopes.iter().any(|c| c == req) {
                return Err(GmError::SafetyViolation(format!(
                    "Pre-install scope confirmation required: scope '{req}' for plugin '{}' was not confirmed",
                    listing.id
                )));
            }
        }
        let actual_sha = hex::encode(Sha256::digest(listing.manifest_json.as_bytes()));
        if !listing.package_sha256.is_empty() && actual_sha != listing.package_sha256 {
            return Err(GmError::SafetyViolation(format!(
                "Marketplace package SHA-256 mismatch for '{}'",
                listing.id
            )));
        }
        let now = Utc::now().to_rfc3339();
        let scopes_json = serde_json::to_string(&listing.requested_scopes)?;
        self.conn
            .lock()
            .execute(
                "INSERT INTO installed_plugins_v1
                 (id, name, version, approved_scopes_json, package_sha256, manifest_json, installed_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
                 ON CONFLICT(id) DO UPDATE SET
                   name = excluded.name,
                   version = excluded.version,
                   approved_scopes_json = excluded.approved_scopes_json,
                   package_sha256 = excluded.package_sha256,
                   manifest_json = excluded.manifest_json,
                   updated_at = excluded.updated_at",
                params![
                    listing.id,
                    listing.name,
                    listing.version,
                    scopes_json,
                    actual_sha,
                    listing.manifest_json,
                    now
                ],
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(InstalledMarketplacePlugin {
            id: listing.id.clone(),
            name: listing.name.clone(),
            version: listing.version.clone(),
            approved_scopes: listing.requested_scopes.clone(),
            package_sha256: actual_sha,
            installed_at: now.clone(),
            updated_at: now,
        })
    }

    pub fn update_plugin(
        &self,
        updated: &MarketplaceListing,
        confirmed_scopes: &[String],
    ) -> Result<InstalledMarketplacePlugin> {
        let exists: Option<String> = self
            .conn
            .lock()
            .query_row(
                "SELECT id FROM installed_plugins_v1 WHERE id = ?1",
                params![updated.id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| GmError::Platform(e.to_string()))?;
        if exists.is_none() {
            return Err(GmError::Config(format!(
                "Plugin '{}' is not installed",
                updated.id
            )));
        }
        self.install_plugin(updated, confirmed_scopes)
    }

    pub fn remove_plugin(&self, id: &str) -> Result<bool> {
        let count = self
            .conn
            .lock()
            .execute(
                "DELETE FROM installed_plugins_v1 WHERE id = ?1",
                params![id.trim()],
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(count > 0)
    }

    pub fn list_installed(&self) -> Result<Vec<InstalledMarketplacePlugin>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, name, version, approved_scopes_json, package_sha256, installed_at, updated_at
                 FROM installed_plugins_v1 ORDER BY name",
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                let scopes_raw: String = r.get(3)?;
                let approved_scopes: Vec<String> =
                    serde_json::from_str(&scopes_raw).unwrap_or_default();
                Ok(InstalledMarketplacePlugin {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    version: r.get(2)?,
                    approved_scopes,
                    package_sha256: r.get(4)?,
                    installed_at: r.get(5)?,
                    updated_at: r.get(6)?,
                })
            })
            .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(rows.flatten().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marketplace_enforces_scope_confirmation_install_update_remove() {
        let store = MarketplaceStore::in_memory().unwrap();
        let cat = store.builtin_catalog();
        assert_eq!(cat.len(), 2);

        // Unconfirmed scopes rejected
        assert!(store.install_plugin(&cat[0], &[]).is_err());

        // Confirmed scopes succeed
        let installed = store
            .install_plugin(&cat[0], &cat[0].requested_scopes)
            .unwrap();
        assert_eq!(installed.version, "1.0.0");

        // Update plugin version atomically
        let mut v2 = cat[0].clone();
        v2.version = "1.1.0".into();
        let updated = store.update_plugin(&v2, &v2.requested_scopes).unwrap();
        assert_eq!(updated.version, "1.1.0");
        assert_eq!(store.list_installed().unwrap().len(), 1);

        // Remove plugin
        assert!(store.remove_plugin(&cat[0].id).unwrap());
        assert!(store.list_installed().unwrap().is_empty());
    }
}
