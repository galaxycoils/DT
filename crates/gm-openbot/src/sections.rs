//! Custom collapsible sidebar sections (`Folders`) for organizing Bots & Groups.

use chrono::Utc;
use gm_core::database::Database;
use gm_core::error::{GmError, Result};
use parking_lot::Mutex;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BotSection {
    pub id: String,
    pub name: String,
    pub sort_order: i32,
    pub collapsed: bool,
    pub created_at: String,
}

#[derive(Clone)]
pub struct SectionStore {
    conn: Arc<Mutex<Database>>,
}

impl SectionStore {
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        let conn = gm_core::database::Database::open(db_path.as_ref())?;
        Self::init_conn(conn)
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Database::in_memory()
            .map_err(|e| GmError::Platform(format!("Failed to open in-memory sections DB: {e}")))?;
        Self::init_conn(conn)
    }

    fn init_conn(conn: Database) -> Result<Self> {
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS bot_sections_v1 (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                sort_order INTEGER NOT NULL DEFAULT 0,
                collapsed INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_bot_sections_order
                ON bot_sections_v1(sort_order ASC, name ASC);
            "#,
        )
        .map_err(|e| GmError::Platform(format!("SectionStore schema init failed: {e}")))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn list_sections(&self) -> Result<Vec<BotSection>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, name, sort_order, collapsed, created_at FROM bot_sections_v1 ORDER BY sort_order ASC, created_at ASC",
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                let collapsed_int: i32 = r.get(3)?;
                Ok(BotSection {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    sort_order: r.get(2)?,
                    collapsed: collapsed_int != 0,
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

    pub fn upsert_section(
        &self,
        id: Option<&str>,
        name: &str,
        sort_order: Option<i32>,
        collapsed: Option<bool>,
    ) -> Result<BotSection> {
        let clean_name = name.trim();
        if clean_name.is_empty() || clean_name.len() > 64 || clean_name.contains('\n') {
            return Err(GmError::Config(
                "Section name must be 1-64 single-line characters".into(),
            ));
        }

        let sec_id = match id.map(str::trim).filter(|s| !s.is_empty()) {
            Some(existing) => {
                if existing.len() > 64
                    || !existing
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                {
                    return Err(GmError::Config("Invalid section id".into()));
                }
                existing.to_string()
            }
            None => {
                let slug: String = clean_name
                    .to_ascii_lowercase()
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                    .collect();
                let slug = slug.trim_matches('-');
                let suffix = &uuid::Uuid::new_v4().simple().to_string()[..6];
                if slug.is_empty() {
                    format!("sec-{suffix}")
                } else {
                    format!("sec-{slug}-{suffix}")
                }
            }
        };

        let conn = self.conn.lock();
        let existing: Option<(i32, i32, String)> = conn
            .query_row(
                "SELECT sort_order, collapsed, created_at FROM bot_sections_v1 WHERE id = ?1",
                params![&sec_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| GmError::Platform(e.to_string()))?;

        let (final_order, final_collapsed, created_at) = match existing {
            Some((old_ord, old_col, old_ts)) => (
                sort_order.unwrap_or(old_ord),
                collapsed.unwrap_or(old_col != 0),
                old_ts,
            ),
            None => {
                let max_order: i32 = conn
                    .query_row(
                        "SELECT coalesce(max(sort_order), -1) FROM bot_sections_v1",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(-1);
                (
                    sort_order.unwrap_or(max_order + 1),
                    collapsed.unwrap_or(false),
                    Utc::now().to_rfc3339(),
                )
            }
        };

        conn.execute(
            r#"
            INSERT INTO bot_sections_v1(id, name, sort_order, collapsed, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                sort_order = excluded.sort_order,
                collapsed = excluded.collapsed
            "#,
            params![
                sec_id,
                clean_name,
                final_order,
                final_collapsed as i32,
                created_at
            ],
        )
        .map_err(|e| GmError::Platform(e.to_string()))?;

        Ok(BotSection {
            id: sec_id,
            name: clean_name.to_string(),
            sort_order: final_order,
            collapsed: final_collapsed,
            created_at,
        })
    }

    pub fn set_collapsed(&self, id: &str, collapsed: bool) -> Result<bool> {
        let rows = self
            .conn
            .lock()
            .execute(
                "UPDATE bot_sections_v1 SET collapsed = ?1 WHERE id = ?2",
                params![collapsed as i32, id.trim()],
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(rows > 0)
    }

    pub fn delete_section(&self, id: &str) -> Result<bool> {
        let rows = self
            .conn
            .lock()
            .execute(
                "DELETE FROM bot_sections_v1 WHERE id = ?1",
                params![id.trim()],
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(rows > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_crud_and_ordering() {
        let store = SectionStore::in_memory().unwrap();
        let s1 = store.upsert_section(None, "Work", Some(1), None).unwrap();
        let s2 = store
            .upsert_section(Some("personal"), "Personal", Some(0), Some(true))
            .unwrap();
        assert_eq!(s2.id, "personal");
        assert!(s2.collapsed);

        let list = store.list_sections().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "personal");
        assert_eq!(list[1].id, s1.id);

        assert!(store.set_collapsed(&s1.id, true).unwrap());
        let after = store.list_sections().unwrap();
        assert!(after[1].collapsed);

        assert!(store.delete_section("personal").unwrap());
        assert_eq!(store.list_sections().unwrap().len(), 1);
    }
}
