//! Claim-level provenance tracking for Muse memory (`<storage>/memory/provenance.sqlite`).
//! Records source locators, SHA-256 content hashes, conversation handles,
//! supersession links, and atomic claim-chain retraction.

use chrono::Utc;
use gm_core::error::{GmError, Result};
use gm_core::storage::ensure_private_dir;
use hex::encode as hex_encode;
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    Active,
    Superseded,
    Retracted,
}

impl ClaimStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ClaimStatus::Active => "active",
            ClaimStatus::Superseded => "superseded",
            ClaimStatus::Retracted => "retracted",
        }
    }

    pub fn from_db(raw: &str) -> Result<Self> {
        match raw {
            "active" => Ok(ClaimStatus::Active),
            "superseded" => Ok(ClaimStatus::Superseded),
            "retracted" => Ok(ClaimStatus::Retracted),
            other => Err(GmError::Platform(format!(
                "Invalid claim status in provenance DB: '{other}'"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryClaim {
    pub claim_id: String,
    pub record_id: String,
    pub quote_sha256: String,
    pub conversation_handle: Option<String>,
    pub source_locator: String,
    pub status: ClaimStatus,
    pub superseded_by: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimExplanation {
    pub claim: MemoryClaim,
    /// Full lineage from newest active/retracted head down to oldest superseded claim (`[v2, v1, ...]`).
    pub supersession_chain: Vec<MemoryClaim>,
    /// Claims directly/transitively superseded by `claim`.
    pub supersedes: Vec<MemoryClaim>,
    /// Newer claims that replaced `claim`, nearest first (`v2`, `v3`, ...).
    pub superseded_by_chain: Vec<MemoryClaim>,
}

#[derive(Clone)]
pub struct ProvenanceStore {
    conn: Arc<Mutex<Connection>>,
    db_path: Option<PathBuf>,
}

impl std::fmt::Debug for ProvenanceStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProvenanceStore")
            .field("db_path", &self.db_path)
            .finish()
    }
}

pub fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex_encode(hasher.finalize())
}

fn parse_claim_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryClaim> {
    let status_raw: String = row.get(5)?;
    let status = ClaimStatus::from_db(&status_raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(MemoryClaim {
        claim_id: row.get(0)?,
        record_id: row.get(1)?,
        quote_sha256: row.get(2)?,
        conversation_handle: row.get(3)?,
        source_locator: row.get(4)?,
        status,
        superseded_by: row.get(6)?,
        created_at: row.get(7)?,
    })
}

impl ProvenanceStore {
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        let path = db_path.as_ref().to_path_buf();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            ensure_private_dir(parent)?;
        }
        let conn = Connection::open(&path)
            .map_err(|e| GmError::Platform(format!("Failed to open provenance DB: {e}")))?;
        Self::init_conn(conn, Some(path))
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(|e| {
            GmError::Platform(format!("Failed to open in-memory provenance DB: {e}"))
        })?;
        Self::init_conn(conn, None)
    }

    fn init_conn(conn: Connection, db_path: Option<PathBuf>) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| GmError::Platform(e.to_string()))?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA secure_delete = ON;
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS memory_claims_v1(
                claim_id TEXT PRIMARY KEY,
                record_id TEXT NOT NULL,
                quote_sha256 TEXT NOT NULL,
                conversation_handle TEXT,
                source_locator TEXT NOT NULL,
                status TEXT NOT NULL CHECK(status IN ('active','superseded','retracted')),
                superseded_by TEXT REFERENCES memory_claims_v1(claim_id),
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_claims_record ON memory_claims_v1(record_id);
            CREATE INDEX IF NOT EXISTS idx_claims_locator ON memory_claims_v1(source_locator);
            CREATE INDEX IF NOT EXISTS idx_claims_superseded_by ON memory_claims_v1(superseded_by);
            "#,
        )
        .map_err(|e| GmError::Platform(format!("Failed to initialize provenance schema: {e}")))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path,
        })
    }

    pub fn db_path(&self) -> Option<&Path> {
        self.db_path.as_deref()
    }

    /// Record an active memory claim hashing `quote_text` with SHA-256.
    pub fn record_claim(
        &self,
        claim_id: &str,
        record_id: &str,
        quote_text: &str,
        conversation_handle: Option<&str>,
        source_locator: &str,
    ) -> Result<MemoryClaim> {
        let quote_sha = sha256_hex(quote_text);
        self.record_claim_with_sha(
            claim_id,
            record_id,
            &quote_sha,
            conversation_handle,
            source_locator,
        )
    }

    pub fn record_claim_with_sha(
        &self,
        claim_id: &str,
        record_id: &str,
        quote_sha256: &str,
        conversation_handle: Option<&str>,
        source_locator: &str,
    ) -> Result<MemoryClaim> {
        let created_at = Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO memory_claims_v1(
                claim_id, record_id, quote_sha256, conversation_handle,
                source_locator, status, superseded_by, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'active', NULL, ?6)",
            params![
                claim_id,
                record_id,
                quote_sha256,
                conversation_handle,
                source_locator,
                created_at,
            ],
        )
        .map_err(|e| GmError::Platform(format!("Failed to record memory claim: {e}")))?;
        Ok(MemoryClaim {
            claim_id: claim_id.to_string(),
            record_id: record_id.to_string(),
            quote_sha256: quote_sha256.to_string(),
            conversation_handle: conversation_handle.map(str::to_string),
            source_locator: source_locator.to_string(),
            status: ClaimStatus::Active,
            superseded_by: None,
            created_at,
        })
    }

    /// Insert `new_claim_id` as active and transition `old_claim_id` -> `superseded` (`superseded_by = new_claim_id`)
    /// inside a single immediate transaction.
    pub fn supersede_claim(
        &self,
        old_claim_id: &str,
        new_claim_id: &str,
        new_record_id: &str,
        new_quote_text: &str,
        conversation_handle: Option<&str>,
        source_locator: &str,
    ) -> Result<MemoryClaim> {
        let quote_sha = sha256_hex(new_quote_text);
        let created_at = Utc::now().to_rfc3339();
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| GmError::Platform(e.to_string()))?;

        let existing: Option<String> = tx
            .query_row(
                "SELECT claim_id FROM memory_claims_v1 WHERE claim_id = ?1",
                [old_claim_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| GmError::Platform(e.to_string()))?;
        if existing.is_none() {
            return Err(GmError::Config(format!(
                "Cannot supersede missing claim '{old_claim_id}'"
            )));
        }

        tx.execute(
            "INSERT INTO memory_claims_v1(
                claim_id, record_id, quote_sha256, conversation_handle,
                source_locator, status, superseded_by, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'active', NULL, ?6)",
            params![
                new_claim_id,
                new_record_id,
                quote_sha,
                conversation_handle,
                source_locator,
                created_at,
            ],
        )
        .map_err(|e| GmError::Platform(format!("Failed to insert replacement claim: {e}")))?;

        tx.execute(
            "UPDATE memory_claims_v1 SET status = 'superseded', superseded_by = ?2 WHERE claim_id = ?1",
            params![old_claim_id, new_claim_id],
        )
        .map_err(|e| GmError::Platform(format!("Failed to mark superseded claim: {e}")))?;

        tx.commit().map_err(|e| GmError::Platform(e.to_string()))?;

        Ok(MemoryClaim {
            claim_id: new_claim_id.to_string(),
            record_id: new_record_id.to_string(),
            quote_sha256: quote_sha,
            conversation_handle: conversation_handle.map(str::to_string),
            source_locator: source_locator.to_string(),
            status: ClaimStatus::Active,
            superseded_by: None,
            created_at,
        })
    }

    pub fn get_claim(&self, claim_id: &str) -> Result<Option<MemoryClaim>> {
        let conn = self.conn.lock();
        Self::get_claim_with_conn(&conn, claim_id)
    }

    fn get_claim_with_conn(conn: &Connection, claim_id: &str) -> Result<Option<MemoryClaim>> {
        conn.query_row(
            "SELECT claim_id, record_id, quote_sha256, conversation_handle,
                    source_locator, status, superseded_by, created_at
             FROM memory_claims_v1
             WHERE claim_id = ?1",
            [claim_id],
            parse_claim_row,
        )
        .optional()
        .map_err(|e| GmError::Platform(e.to_string()))
    }

    /// Explain one or more claims matching `query_or_id` (claim ID, record ID,
    /// source locator substring, conversation handle, or quote SHA-256),
    /// including their full supersession chain.
    pub fn explain_memory(&self, query_or_id: &str) -> Result<Vec<ClaimExplanation>> {
        let needle = query_or_id.trim();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock();

        let mut stmt = conn
            .prepare(
                "SELECT claim_id, record_id, quote_sha256, conversation_handle,
                        source_locator, status, superseded_by, created_at
                 FROM memory_claims_v1
                 WHERE claim_id = ?1
                    OR record_id = ?1
                    OR source_locator = ?1
                    OR conversation_handle = ?1
                    OR quote_sha256 = ?1
                    OR instr(lower(source_locator), lower(?1)) > 0
                 ORDER BY created_at DESC, claim_id DESC",
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;

        let rows = stmt
            .query_map([needle], parse_claim_row)
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let mut matched = Vec::new();
        for row in rows {
            matched.push(row.map_err(|e| GmError::Platform(e.to_string()))?);
        }
        drop(stmt);

        let mut out = Vec::with_capacity(matched.len());
        for claim in matched {
            let superseded_by_chain = Self::collect_upward(&conn, &claim)?;
            let supersedes = Self::collect_downward(&conn, &claim.claim_id)?;

            // Construct chronological newest -> oldest chain containing ancestors, claim, and descendants.
            let mut full_chain = Vec::new();
            for newer in superseded_by_chain.iter().rev() {
                full_chain.push(newer.clone());
            }
            full_chain.push(claim.clone());
            for older in &supersedes {
                full_chain.push(older.clone());
            }

            out.push(ClaimExplanation {
                claim,
                supersession_chain: full_chain,
                supersedes,
                superseded_by_chain,
            });
        }
        Ok(out)
    }

    fn collect_upward(conn: &Connection, start: &MemoryClaim) -> Result<Vec<MemoryClaim>> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        seen.insert(start.claim_id.clone());
        let mut cursor = start.superseded_by.clone();
        while let Some(next_id) = cursor {
            if !seen.insert(next_id.clone()) {
                break;
            }
            let Some(next_claim) = Self::get_claim_with_conn(conn, &next_id)? else {
                break;
            };
            cursor = next_claim.superseded_by.clone();
            out.push(next_claim);
        }
        Ok(out)
    }

    fn collect_downward(conn: &Connection, claim_id: &str) -> Result<Vec<MemoryClaim>> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        seen.insert(claim_id.to_string());
        let mut queue = VecDeque::new();
        queue.push_back(claim_id.to_string());

        let mut stmt = conn
            .prepare(
                "SELECT claim_id, record_id, quote_sha256, conversation_handle,
                        source_locator, status, superseded_by, created_at
                 FROM memory_claims_v1
                 WHERE superseded_by = ?1
                 ORDER BY created_at DESC, claim_id DESC",
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;

        while let Some(cur) = queue.pop_front() {
            let rows = stmt
                .query_map([&cur], parse_claim_row)
                .map_err(|e| GmError::Platform(e.to_string()))?;
            for row in rows {
                let c = row.map_err(|e| GmError::Platform(e.to_string()))?;
                if seen.insert(c.claim_id.clone()) {
                    queue.push_back(c.claim_id.clone());
                    out.push(c);
                }
            }
        }
        Ok(out)
    }

    /// Retract the given claim IDs and every claim reachable on their supersession chains
    /// inside a single atomic transaction. Returns all retracted claim IDs.
    pub fn retract_claims(&self, seed_claim_ids: &[String]) -> Result<Vec<String>> {
        if seed_claim_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| GmError::Platform(e.to_string()))?;

        let all_ids = Self::collect_connected_claim_ids(&tx, seed_claim_ids)?;
        for cid in &all_ids {
            tx.execute(
                "UPDATE memory_claims_v1 SET status = 'retracted' WHERE claim_id = ?1",
                [cid],
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        }
        tx.commit().map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(all_ids)
    }

    pub fn retract_by_record_or_locator(
        &self,
        record_ids: &[String],
        locators: &[String],
    ) -> Result<Vec<String>> {
        let mut seeds = Vec::new();
        {
            let conn = self.conn.lock();
            for rec in record_ids {
                let mut stmt = conn
                    .prepare("SELECT claim_id FROM memory_claims_v1 WHERE record_id = ?1")
                    .map_err(|e| GmError::Platform(e.to_string()))?;
                let rows = stmt
                    .query_map([rec], |r| r.get::<_, String>(0))
                    .map_err(|e| GmError::Platform(e.to_string()))?;
                for r in rows {
                    seeds.push(r.map_err(|e| GmError::Platform(e.to_string()))?);
                }
            }
            for loc in locators {
                let mut stmt = conn
                    .prepare(
                        "SELECT claim_id FROM memory_claims_v1
                         WHERE source_locator = ?1 OR instr(source_locator, ?1) > 0",
                    )
                    .map_err(|e| GmError::Platform(e.to_string()))?;
                let rows = stmt
                    .query_map([loc], |r| r.get::<_, String>(0))
                    .map_err(|e| GmError::Platform(e.to_string()))?;
                for r in rows {
                    seeds.push(r.map_err(|e| GmError::Platform(e.to_string()))?);
                }
            }
        }
        self.retract_claims(&seeds)
    }

    fn collect_connected_claim_ids(conn: &Connection, seed_ids: &[String]) -> Result<Vec<String>> {
        let mut seen = HashSet::new();
        let mut queue = VecDeque::new();
        for s in seed_ids {
            if seen.insert(s.clone()) {
                queue.push_back(s.clone());
            }
        }

        let mut ordered = Vec::new();
        while let Some(cid) = queue.pop_front() {
            let Some(claim) = Self::get_claim_with_conn(conn, &cid)? else {
                continue;
            };
            ordered.push(claim.claim_id.clone());
            if let Some(parent) = claim.superseded_by
                && seen.insert(parent.clone())
            {
                queue.push_back(parent);
            }
            let mut stmt = conn
                .prepare("SELECT claim_id FROM memory_claims_v1 WHERE superseded_by = ?1")
                .map_err(|e| GmError::Platform(e.to_string()))?;
            let rows = stmt
                .query_map([&cid], |r| r.get::<_, String>(0))
                .map_err(|e| GmError::Platform(e.to_string()))?;
            for row in rows {
                let child = row.map_err(|e| GmError::Platform(e.to_string()))?;
                if seen.insert(child.clone()) {
                    queue.push_back(child);
                }
            }
        }
        Ok(ordered)
    }

    pub fn list_claims(
        &self,
        status_filter: Option<ClaimStatus>,
        limit: usize,
    ) -> Result<Vec<MemoryClaim>> {
        let conn = self.conn.lock();
        if let Some(status) = status_filter {
            let mut stmt = conn
                .prepare(
                    "SELECT claim_id, record_id, quote_sha256, conversation_handle,
                            source_locator, status, superseded_by, created_at
                     FROM memory_claims_v1
                     WHERE status = ?1
                     ORDER BY created_at DESC, claim_id DESC
                     LIMIT ?2",
                )
                .map_err(|e| GmError::Platform(e.to_string()))?;
            let rows = stmt
                .query_map(params![status.as_str(), limit as i64], parse_claim_row)
                .map_err(|e| GmError::Platform(e.to_string()))?;
            rows.map(|r| r.map_err(|e| GmError::Platform(e.to_string())))
                .collect()
        } else {
            let mut stmt = conn
                .prepare(
                    "SELECT claim_id, record_id, quote_sha256, conversation_handle,
                            source_locator, status, superseded_by, created_at
                     FROM memory_claims_v1
                     ORDER BY created_at DESC, claim_id DESC
                     LIMIT ?1",
                )
                .map_err(|e| GmError::Platform(e.to_string()))?;
            let rows = stmt
                .query_map([limit as i64], parse_claim_row)
                .map_err(|e| GmError::Platform(e.to_string()))?;
            rows.map(|r| r.map_err(|e| GmError::Platform(e.to_string())))
                .collect()
        }
    }

    /// Execute WAL truncation and `VACUUM` **outside** any active SQL transaction.
    pub fn wal_checkpoint_and_vacuum(&self) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")
            .map_err(|e| GmError::Platform(format!("Provenance VACUUM failed: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supersession_chain_v2_supersedes_v1_and_retract_cascades() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("memory/provenance.sqlite");
        let store = ProvenanceStore::open(&db_path).unwrap();

        let c1 = store
            .record_claim(
                "claim-v1",
                "rec-1",
                "User lives in Seattle",
                Some("chat-main"),
                "MEMORY.md#L10",
            )
            .unwrap();
        assert_eq!(c1.status, ClaimStatus::Active);

        let c2 = store
            .supersede_claim(
                "claim-v1",
                "claim-v2",
                "rec-2",
                "User moved to Austin",
                Some("chat-main"),
                "MEMORY.md#L14",
            )
            .unwrap();
        assert_eq!(c2.status, ClaimStatus::Active);

        let v2_explained = store.explain_memory("claim-v2").unwrap();
        assert_eq!(v2_explained.len(), 1);
        assert_eq!(v2_explained[0].supersedes.len(), 1);
        assert_eq!(v2_explained[0].supersedes[0].claim_id, "claim-v1");
        assert_eq!(
            v2_explained[0].supersedes[0].status,
            ClaimStatus::Superseded
        );
        assert_eq!(
            v2_explained[0].supersedes[0].superseded_by.as_deref(),
            Some("claim-v2")
        );
        let chain_ids: Vec<&str> = v2_explained[0]
            .supersession_chain
            .iter()
            .map(|c| c.claim_id.as_str())
            .collect();
        assert_eq!(chain_ids, vec!["claim-v2", "claim-v1"]);

        // Explaining by v1 source locator shows v2 -> v1 lineage as well.
        let v1_by_loc = store.explain_memory("MEMORY.md#L10").unwrap();
        assert_eq!(v1_by_loc.len(), 1);
        assert_eq!(v1_by_loc[0].superseded_by_chain.len(), 1);
        assert_eq!(v1_by_loc[0].superseded_by_chain[0].claim_id, "claim-v2");

        // Retracting v2 cascades down the chain to v1.
        let retracted = store.retract_claims(&["claim-v2".into()]).unwrap();
        assert!(retracted.contains(&"claim-v1".to_string()));
        assert!(retracted.contains(&"claim-v2".to_string()));
        assert!(
            store
                .list_claims(Some(ClaimStatus::Active), 20)
                .unwrap()
                .is_empty()
        );

        store.wal_checkpoint_and_vacuum().unwrap();
    }
}
