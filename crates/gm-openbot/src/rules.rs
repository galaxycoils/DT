//! Custom Auto-Review Rules (`Allow` / `AskFirst`) capped strictly at 20 per mode (<= 1000 chars).
//! Custom Allow rules NEVER override `BlocklistManager`, `SecretScrubber`, or irreversible safety gates.

use chrono::Utc;
use gm_core::error::{GmError, Result};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

pub const MAX_RULES_PER_MODE: usize = 20;
pub const MAX_RULE_PATTERN_CHARS: usize = 1000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuleMode {
    Allow,
    AskFirst,
}

impl RuleMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            RuleMode::Allow => "allow",
            RuleMode::AskFirst => "ask_first",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "allow" => Ok(RuleMode::Allow),
            "ask_first" | "ask" | "askfirst" => Ok(RuleMode::AskFirst),
            other => Err(GmError::Config(format!(
                "Invalid rule mode '{other}' (expected 'allow' or 'ask_first')"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomApprovalRule {
    pub id: String,
    pub mode: RuleMode,
    pub pattern: String,
    #[serde(default)]
    pub scope_bot_id: Option<String>,
    #[serde(default)]
    pub description: String,
    pub enabled: bool,
    pub created_at: String,
}

#[derive(Clone)]
pub struct ApprovalRulesStore {
    conn: Arc<Mutex<Connection>>,
}

impl ApprovalRulesStore {
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        let conn = crate::db_recovery::open_sqlite_with_recovery(db_path.as_ref())?;
        Self::init_conn(conn)
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| GmError::Platform(format!("Failed to open in-memory rules DB: {e}")))?;
        Self::init_conn(conn)
    }

    fn init_conn(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS approval_rules_v1 (
                id TEXT PRIMARY KEY,
                mode TEXT NOT NULL,
                pattern TEXT NOT NULL,
                scope_bot_id TEXT,
                description TEXT NOT NULL DEFAULT '',
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_approval_rules_mode
                ON approval_rules_v1(mode, enabled);
            "#,
        )
        .map_err(|e| GmError::Platform(format!("ApprovalRulesStore schema init failed: {e}")))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn list_rules(&self) -> Result<Vec<CustomApprovalRule>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, mode, pattern, scope_bot_id, description, enabled, created_at FROM approval_rules_v1 ORDER BY mode ASC, created_at ASC",
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                let mode_str: String = r.get(1)?;
                let mode = RuleMode::parse(&mode_str).unwrap_or(RuleMode::AskFirst);
                Ok(CustomApprovalRule {
                    id: r.get(0)?,
                    mode,
                    pattern: r.get(2)?,
                    scope_bot_id: r.get(3)?,
                    description: r.get(4)?,
                    enabled: r.get::<_, i32>(5)? != 0,
                    created_at: r.get(6)?,
                })
            })
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| GmError::Platform(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn upsert_rule(
        &self,
        id: Option<&str>,
        mode: RuleMode,
        pattern: &str,
        scope_bot_id: Option<&str>,
        description: &str,
        enabled: bool,
    ) -> Result<CustomApprovalRule> {
        let clean_pattern = pattern.trim();
        if clean_pattern.is_empty() || clean_pattern.chars().count() > MAX_RULE_PATTERN_CHARS {
            return Err(GmError::Config(format!(
                "Rule pattern must be 1-{MAX_RULE_PATTERN_CHARS} characters"
            )));
        }
        let clean_scope = scope_bot_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_lowercase());
        let clean_desc = description.trim().chars().take(500).collect::<String>();

        let rule_id = match id.map(str::trim).filter(|s| !s.is_empty()) {
            Some(existing) => existing.to_string(),
            None => format!("rule-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]),
        };

        let conn = self.conn.lock();
        let existing_created: Option<String> = conn
            .query_row(
                "SELECT created_at FROM approval_rules_v1 WHERE id = ?1",
                params![&rule_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| GmError::Platform(e.to_string()))?;

        let count_in_mode: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM approval_rules_v1 WHERE mode = ?1 AND id != ?2",
                params![mode.as_str(), &rule_id],
                |r| r.get(0),
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        if count_in_mode as usize >= MAX_RULES_PER_MODE {
            return Err(GmError::Config(format!(
                "Custom '{}' rule limit reached ({MAX_RULES_PER_MODE} max)",
                mode.as_str()
            )));
        }

        let created_at = existing_created.unwrap_or_else(|| Utc::now().to_rfc3339());
        conn.execute(
            r#"
            INSERT INTO approval_rules_v1(id, mode, pattern, scope_bot_id, description, enabled, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(id) DO UPDATE SET
                mode = excluded.mode,
                pattern = excluded.pattern,
                scope_bot_id = excluded.scope_bot_id,
                description = excluded.description,
                enabled = excluded.enabled
            "#,
            params![
                rule_id,
                mode.as_str(),
                clean_pattern,
                clean_scope,
                clean_desc,
                enabled as i32,
                created_at
            ],
        )
        .map_err(|e| GmError::Platform(e.to_string()))?;

        Ok(CustomApprovalRule {
            id: rule_id,
            mode,
            pattern: clean_pattern.to_string(),
            scope_bot_id: clean_scope,
            description: clean_desc,
            enabled,
            created_at,
        })
    }

    pub fn delete_rule(&self, id: &str) -> Result<bool> {
        let rows = self
            .conn
            .lock()
            .execute(
                "DELETE FROM approval_rules_v1 WHERE id = ?1",
                params![id.trim()],
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(rows > 0)
    }

    /// Evaluates custom rules for `(bot_id, action_text)`.
    /// `AskFirst` always takes precedence over `Allow`.
    /// Note: Callers MUST enforce `BlocklistManager`, secret guards, and `ActionTier::Irreversible`
    /// independently BEFORE consulting `Allow` rules!
    pub fn evaluate(&self, bot_id: &str, action_text: &str) -> Option<RuleMode> {
        let rules = self.list_rules().ok()?;
        let clean_bot = bot_id.trim().to_ascii_lowercase();
        let target_lower = action_text.to_ascii_lowercase();

        for mode in [RuleMode::AskFirst, RuleMode::Allow] {
            for rule in rules.iter().filter(|r| r.enabled && r.mode == mode) {
                if let Some(ref scope) = rule.scope_bot_id
                    && scope != &clean_bot
                {
                    continue;
                }
                if wildcard_or_substr_match(&rule.pattern.to_ascii_lowercase(), &target_lower) {
                    return Some(mode);
                }
            }
        }
        None
    }
}

fn wildcard_or_substr_match(pattern: &str, text: &str) -> bool {
    if pattern.contains('*') {
        let parts: Vec<&str> = pattern.split('*').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            return false;
        }
        let mut cursor = 0usize;
        for part in parts {
            let Some(pos) = text[cursor..].find(part) else {
                return false;
            };
            cursor += pos + part.len();
        }
        true
    } else {
        text.contains(pattern)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_rules_enforce_20_cap_and_ask_first_precedence() {
        let store = ApprovalRulesStore::in_memory().unwrap();
        for i in 0..MAX_RULES_PER_MODE {
            store
                .upsert_rule(
                    None,
                    RuleMode::Allow,
                    &format!("cargo check {i}"),
                    None,
                    "",
                    true,
                )
                .unwrap();
        }
        // 21st Allow rule is rejected
        assert!(
            store
                .upsert_rule(None, RuleMode::Allow, "cargo check 20", None, "", true)
                .is_err()
        );
        // Updating existing rule still works
        let first_id = store.list_rules().unwrap()[0].id.clone();
        store
            .upsert_rule(Some(&first_id), RuleMode::Allow, "git *", None, "read git", true)
            .unwrap();

        // 1001 char pattern rejected
        let long_pattern = "a".repeat(MAX_RULE_PATTERN_CHARS + 1);
        assert!(
            store
                .upsert_rule(None, RuleMode::AskFirst, &long_pattern, None, "", true)
                .is_err()
        );

        // AskFirst beats Allow on overlap
        store
            .upsert_rule(
                None,
                RuleMode::AskFirst,
                "git push*",
                Some("coder"),
                "confirm push",
                true,
            )
            .unwrap();
        assert_eq!(store.evaluate("coder", "git status"), Some(RuleMode::Allow));
        assert_eq!(
            store.evaluate("coder", "git push origin main"),
            Some(RuleMode::AskFirst)
        );
        // Scope isolation
        assert_eq!(
            store.evaluate("muse", "git push origin main"),
            Some(RuleMode::Allow)
        );
    }
}
