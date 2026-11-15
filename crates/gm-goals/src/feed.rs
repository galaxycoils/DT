use crate::notice::ProactiveNotice;
use chrono::{DateTime, Utc};
use gm_core::error::{GmError, Result};
use gm_core::storage::ensure_private_dir;
use gm_memory::PersonalMemoryStore;
use gm_safety::AuditLogger;
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeedCategory {
    Observation,
    PatrolInsight,
    TaskCompleted,
    GettingStarted,
    Editorial,
}

impl FeedCategory {
    pub fn badge(&self) -> &'static str {
        match self {
            FeedCategory::Observation => "📝 [Observation]",
            FeedCategory::PatrolInsight => "📬 [Patrol Insight]",
            FeedCategory::TaskCompleted => "⚡ [Task Completed]",
            FeedCategory::GettingStarted => "👋 [Getting started]",
            FeedCategory::Editorial => "📰 [Editorial]",
        }
    }

    pub fn as_db_str(&self) -> &'static str {
        match self {
            FeedCategory::Observation => "observation",
            FeedCategory::PatrolInsight => "patrol_insight",
            FeedCategory::TaskCompleted => "task_completed",
            FeedCategory::GettingStarted => "getting_started",
            FeedCategory::Editorial => "editorial",
        }
    }

    pub fn from_db_str(s: &str) -> Self {
        match s {
            "patrol_insight" => FeedCategory::PatrolInsight,
            "task_completed" => FeedCategory::TaskCompleted,
            "getting_started" => FeedCategory::GettingStarted,
            "editorial" => FeedCategory::Editorial,
            _ => FeedCategory::Observation,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedItem {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub category: FeedCategory,
    pub title: String,
    pub summary: String,
    pub details: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedPost {
    pub id: String,
    pub kicker: String,
    pub category: FeedCategory,
    pub title: String,
    pub body: String,
    pub signal_citations: Vec<String>,
    pub rating: Option<i32>,
    pub user_prompt: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct FeedStore {
    conn: Arc<Mutex<Connection>>,
    db_path: Option<PathBuf>,
}

impl std::fmt::Debug for FeedStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeedStore")
            .field("db_path", &self.db_path)
            .finish()
    }
}

fn parse_feed_post_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FeedPost> {
    let cat_str: String = row.get(2)?;
    let citations_json: String = row.get(5)?;
    let created_str: String = row.get(8)?;
    let created_at = DateTime::parse_from_rfc3339(&created_str)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    let signal_citations: Vec<String> = serde_json::from_str(&citations_json).unwrap_or_default();
    Ok(FeedPost {
        id: row.get(0)?,
        kicker: row.get(1)?,
        category: FeedCategory::from_db_str(&cat_str),
        title: row.get(3)?,
        body: row.get(4)?,
        signal_citations,
        rating: row.get(6)?,
        user_prompt: row.get(7)?,
        created_at,
    })
}

impl FeedStore {
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        let path = db_path.as_ref().to_path_buf();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            ensure_private_dir(parent)?;
        }
        let conn = Connection::open(&path)
            .map_err(|e| GmError::Platform(format!("Failed to open feed DB: {e}")))?;
        Self::init_conn(conn, Some(path))
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| GmError::Platform(format!("Failed to open in-memory feed DB: {e}")))?;
        Self::init_conn(conn, None)
    }

    fn init_conn(conn: Connection, db_path: Option<PathBuf>) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| GmError::Platform(e.to_string()))?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA secure_delete = ON;
            CREATE TABLE IF NOT EXISTS feed_posts_v1 (
                id TEXT PRIMARY KEY,
                kicker TEXT NOT NULL,
                category TEXT NOT NULL,
                title TEXT NOT NULL,
                body TEXT NOT NULL,
                signal_citations TEXT NOT NULL DEFAULT '[]',
                rating INTEGER,
                user_prompt TEXT,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_feed_posts_created ON feed_posts_v1(created_at DESC);
            CREATE TABLE IF NOT EXISTS feed_signals (
                id TEXT PRIMARY KEY,
                signal_key TEXT NOT NULL,
                source TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS feed_meta_v1 (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )
        .map_err(|e| GmError::Platform(format!("Failed to init feed schema: {e}")))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path,
        })
    }

    pub fn db_path(&self) -> Option<&Path> {
        self.db_path.as_deref()
    }

    /// Seed default `Getting started` intro cards once per DB (deleted cards stay deleted).
    pub fn seed_getting_started_if_never_seeded(&self) -> Result<usize> {
        let mut conn = self.conn.lock();
        let already: Option<String> = conn
            .query_row(
                "SELECT value FROM feed_meta_v1 WHERE key = 'seeded_getting_started_v1'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| GmError::Platform(e.to_string()))?;
        if already.is_some() {
            return Ok(0);
        }

        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let now = Utc::now().to_rfc3339();
        let intro_posts = [
            (
                "intro-muse-brief",
                "Getting started",
                "Welcome to your Muse Feed",
                "I write fresh posts here based on your brief, active goals, and memory — ask me about any of it in Chat or steer coverage above.",
            ),
            (
                "intro-muse-tabs",
                "Getting started",
                "Explore Goals, Ideas, and Spaces",
                "Use Goals to track durable milestones (with one level of subgoals), Ideas for proactive suggestions, and Library / Spaces for your static workspaces.",
            ),
        ];

        for (id, kicker, title, body) in intro_posts {
            tx.execute(
                "INSERT OR IGNORE INTO feed_posts_v1(
                    id, kicker, category, title, body, signal_citations, rating, user_prompt, created_at
                 ) VALUES (?1, ?2, 'getting_started', ?3, ?4, '[]', NULL, NULL, ?5)",
                params![id, kicker, title, body, now],
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO feed_meta_v1(key, value) VALUES ('seeded_getting_started_v1', ?1)",
            [now],
        )
        .map_err(|e| GmError::Platform(e.to_string()))?;
        tx.commit().map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(intro_posts.len())
    }

    pub fn upsert_post(&self, post: &FeedPost) -> Result<()> {
        let citations_json = serde_json::to_string(&post.signal_citations)?;
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO feed_posts_v1(
                id, kicker, category, title, body, signal_citations, rating, user_prompt, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                kicker = excluded.kicker,
                category = excluded.category,
                title = excluded.title,
                body = excluded.body,
                signal_citations = excluded.signal_citations,
                rating = COALESCE(excluded.rating, feed_posts_v1.rating),
                user_prompt = excluded.user_prompt",
            params![
                post.id,
                post.kicker,
                post.category.as_db_str(),
                post.title,
                post.body,
                citations_json,
                post.rating,
                post.user_prompt,
                post.created_at.to_rfc3339(),
            ],
        )
        .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(())
    }

    pub fn list_posts(&self, limit: usize) -> Result<Vec<FeedPost>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, kicker, category, title, body, signal_citations, rating, user_prompt, created_at
                 FROM feed_posts_v1
                 ORDER BY created_at DESC, id DESC
                 LIMIT ?1",
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let rows = stmt
            .query_map([limit as i64], parse_feed_post_row)
            .map_err(|e| GmError::Platform(e.to_string()))?;
        rows.map(|r| r.map_err(|e| GmError::Platform(e.to_string())))
            .collect()
    }

    pub fn delete_post(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let rows = conn
            .execute("DELETE FROM feed_posts_v1 WHERE id = ?1", [id])
            .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(rows > 0)
    }

    pub fn rate_post(&self, id: &str, rating: i32) -> Result<bool> {
        let clamped = rating.clamp(-1, 5);
        let conn = self.conn.lock();
        let rows = conn
            .execute(
                "UPDATE feed_posts_v1 SET rating = ?2 WHERE id = ?1",
                params![id, clamped],
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(rows > 0)
    }

    pub fn get_brief_prompt(&self) -> Result<String> {
        let conn = self.conn.lock();
        let prompt: Option<String> = conn
            .query_row(
                "SELECT value FROM feed_meta_v1 WHERE key = 'feed_brief_prompt'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(prompt.unwrap_or_default())
    }

    pub fn set_brief_prompt(&self, prompt: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO feed_meta_v1(key, value) VALUES ('feed_brief_prompt', ?1)",
            [prompt.trim()],
        )
        .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(())
    }

    pub fn record_signal(&self, signal_key: &str, source: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO feed_signals(id, signal_key, source, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                Uuid::new_v4().to_string(),
                signal_key,
                source,
                Utc::now().to_rfc3339()
            ],
        )
        .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(())
    }

    pub fn signal_count(&self) -> Result<usize> {
        let conn = self.conn.lock();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM feed_signals", [], |r| r.get(0))
            .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(count.max(0) as usize)
    }

    /// Global Feed reset fence invoked by two-phase Forget: clears all derived Feed signals
    /// and post signal citations in a single transaction, then checkpoints/VACUUMs outside it.
    pub fn reset_feed_signals(&self) -> Result<usize> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let deleted = tx
            .execute("DELETE FROM feed_signals", [])
            .map_err(|e| GmError::Platform(e.to_string()))?;
        tx.execute("UPDATE feed_posts_v1 SET signal_citations = '[]'", [])
            .map_err(|e| GmError::Platform(e.to_string()))?;
        tx.commit().map_err(|e| GmError::Platform(e.to_string()))?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")
            .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(deleted)
    }
}

pub fn build_feed(
    memory: &PersonalMemoryStore,
    notices: &[ProactiveNotice],
    audit: &AuditLogger,
    limit: usize,
) -> Result<Vec<FeedItem>> {
    let mut feed = Vec::new();

    // 1. Observations from memory
    if let Ok(records) = memory.all_records(limit * 2) {
        for r in records {
            let ts = DateTime::from_timestamp_millis(r.created_at).unwrap_or_else(Utc::now);
            feed.push(FeedItem {
                id: format!("obs_{}", r.id),
                timestamp: ts,
                category: FeedCategory::Observation,
                title: r.title.unwrap_or_else(|| "Personal Note".into()),
                summary: r.body,
                details: serde_json::json!({
                    "kind": format!("{:?}", r.kind),
                    "tags": r.tags,
                    "importance": r.importance,
                    "source": r.source,
                }),
            });
        }
    }

    // 2. Patrol insights from proactive notices
    for n in notices {
        feed.push(FeedItem {
            id: format!("notice_{}", n.id),
            timestamp: n.sent_at,
            category: FeedCategory::PatrolInsight,
            title: n.title.clone(),
            summary: n.body.clone(),
            details: serde_json::json!({
                "topic": n.topic,
                "urgency": n.is_urgent,
                "rating": n.useful_rating,
            }),
        });
    }

    // 3. Completed tasks from audit log
    for entry in audit.entries() {
        if entry.event_type == "task_done" {
            let ts = gm_safety::parse_flexible_timestamp(&entry.timestamp).unwrap_or_else(Utc::now);

            let title = entry
                .action_summary
                .clone()
                .unwrap_or_else(|| "Autonomous Task".into());
            let instruction = entry
                .details
                .get("instruction")
                .and_then(|v| v.as_str())
                .unwrap_or("Executed computer-use task");

            feed.push(FeedItem {
                id: format!("task_{}", entry.timestamp),
                timestamp: ts,
                category: FeedCategory::TaskCompleted,
                title,
                summary: instruction.to_string(),
                details: entry.details.clone(),
            });
        }
    }

    // Sort descending by timestamp (newest first)
    feed.sort_by_key(|a| std::cmp::Reverse(a.timestamp));
    feed.truncate(limit);

    Ok(feed)
}

pub fn format_feed(items: &[FeedItem]) -> String {
    let mut out = String::new();
    out.push_str("═══════════════════════════════════════════════════════════\n");
    out.push_str("📡 Grok-Muse Proactive Live Feed\n");
    out.push_str("═══════════════════════════════════════════════════════════\n\n");

    if items.is_empty() {
        out.push_str("No recent observations, insights, or tasks recorded.\n");
    } else {
        for item in items {
            let time_str = item.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string();
            out.push_str(&format!(
                "[{}] {} {}\n",
                time_str,
                item.category.badge(),
                item.title
            ));
            out.push_str(&format!("  • Details: {}\n\n", item.summary));
        }
    }

    out.push_str("═══════════════════════════════════════════════════════════\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feed_building_and_formatting() {
        let memory = PersonalMemoryStore::in_memory().unwrap();
        memory
            .record_fact(
                Some("Morning preference"),
                "User starts work at 9:00 AM",
                &["preferences"],
            )
            .unwrap();

        let notice = ProactiveNotice::new(
            "calendar",
            "Upcoming: Standup",
            "Meeting starts in 15 mins",
            false,
        );

        let audit = AuditLogger::in_memory();
        audit.log(
            "task_done",
            Some("meta".into()),
            Some("muse-spark".into()),
            Some(120),
            Some(0.001),
            Some("Organized Downloads folder".into()),
            serde_json::json!({ "instruction": "Sort PDFs into Documents" }),
        );

        let items = build_feed(&memory, &[notice], &audit, 10).unwrap();
        assert_eq!(items.len(), 3);
        assert!(
            items
                .iter()
                .any(|i| i.category == FeedCategory::Observation)
        );
        assert!(
            items
                .iter()
                .any(|i| i.category == FeedCategory::PatrolInsight)
        );
        assert!(
            items
                .iter()
                .any(|i| i.category == FeedCategory::TaskCompleted)
        );

        let formatted = format_feed(&items);
        assert!(formatted.contains("Morning preference"));
        assert!(formatted.contains("Upcoming: Standup"));
        assert!(formatted.contains("Organized Downloads folder"));
    }

    #[test]
    fn feed_store_persists_posts_seeds_once_and_resets_signals() {
        let dir = tempfile::tempdir().unwrap();
        let store = FeedStore::open(dir.path().join("feed.sqlite")).unwrap();

        assert_eq!(store.seed_getting_started_if_never_seeded().unwrap(), 2);
        let posts = store.list_posts(10).unwrap();
        assert_eq!(posts.len(), 2);
        assert!(
            posts
                .iter()
                .all(|p| p.category == FeedCategory::GettingStarted)
        );

        // Deleted starter post must NOT be resurrected on re-seed
        assert!(store.delete_post("intro-muse-brief").unwrap());
        assert_eq!(store.seed_getting_started_if_never_seeded().unwrap(), 0);
        assert_eq!(store.list_posts(10).unwrap().len(), 1);

        // Upsert Editorial post with signal citations, rate, and reset signals
        store.record_signal("rust-release-42", "mail").unwrap();
        assert_eq!(store.signal_count().unwrap(), 1);
        store
            .upsert_post(&FeedPost {
                id: "ed-1".into(),
                kicker: "Editorial".into(),
                category: FeedCategory::Editorial,
                title: "Morning Brief".into(),
                body: "Goal check-in and weekly focus.".into(),
                signal_citations: vec!["rust-release-42".into()],
                rating: None,
                user_prompt: Some("More Rust ecosystem news".into()),
                created_at: Utc::now(),
            })
            .unwrap();
        assert!(store.rate_post("ed-1", 5).unwrap());

        assert_eq!(store.reset_feed_signals().unwrap(), 1);
        assert_eq!(store.signal_count().unwrap(), 0);
        let listed = store.list_posts(10).unwrap();
        let ed = listed.iter().find(|p| p.id == "ed-1").unwrap();
        assert!(ed.signal_citations.is_empty());
        assert_eq!(ed.rating, Some(5));
    }
}
