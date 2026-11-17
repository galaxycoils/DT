//! Durable 1:1 DM transcripts, threaded replies, card states, reactions, read cursors,
//! live status phrases, and 2-stage search across DMs & Channels.

use crate::channels::MessageReactionRecord;
use chrono::Utc;
use gm_core::error::{GmError, Result};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectMessageRecord {
    pub seq: u64,
    pub id: String,
    pub bot_id: String,
    pub role: String,
    pub author: String,
    pub content: String,
    #[serde(default)]
    pub reply_to_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub card: Option<serde_json::Value>,
    #[serde(default)]
    pub reactions: Vec<MessageReactionRecord>,
    #[serde(default)]
    pub request_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BotStatusState {
    pub bot_id: String,
    pub phrase: Option<String>,
    pub needs_attention: bool,
    pub unread_count: usize,
    pub last_read_seq: u64,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageSearchHit {
    pub chat_kind: String,
    pub chat_id: String,
    pub message_id: String,
    pub author: String,
    pub content_excerpt: String,
    pub timestamp: String,
}

#[derive(Clone)]
pub struct DirectChatStore {
    conn: Arc<Mutex<Connection>>,
}

impl DirectChatStore {
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        let conn = crate::db_recovery::open_sqlite_with_recovery(db_path.as_ref())?;
        Self::init_conn(conn)
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| GmError::Platform(format!("Failed to open in-memory direct chat DB: {e}")))?;
        Self::init_conn(conn)
    }

    fn init_conn(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS direct_messages_v1 (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                id TEXT UNIQUE NOT NULL,
                bot_id TEXT NOT NULL,
                role TEXT NOT NULL,
                author TEXT NOT NULL,
                content TEXT NOT NULL,
                reply_to_id TEXT,
                thread_id TEXT,
                card_json TEXT,
                request_id TEXT,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_direct_msgs_bot_seq
                ON direct_messages_v1(bot_id, seq DESC);
            CREATE INDEX IF NOT EXISTS idx_direct_msgs_req
                ON direct_messages_v1(request_id);
            CREATE TABLE IF NOT EXISTS dm_reactions_v1 (
                id TEXT PRIMARY KEY,
                message_id TEXT NOT NULL,
                emoji TEXT NOT NULL,
                actor TEXT NOT NULL,
                created_at TEXT NOT NULL,
                UNIQUE(message_id, emoji, actor)
            );
            CREATE INDEX IF NOT EXISTS idx_dm_reactions_msg
                ON dm_reactions_v1(message_id);
            CREATE TABLE IF NOT EXISTS chat_read_cursors_v1 (
                chat_kind TEXT NOT NULL,
                chat_id TEXT NOT NULL,
                last_read_seq INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(chat_kind, chat_id)
            );
            CREATE TABLE IF NOT EXISTS bot_status_phrases_v1 (
                bot_id TEXT PRIMARY KEY,
                phrase TEXT,
                needs_attention INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL
            );
            "#,
        )
        .map_err(|e| GmError::Platform(format!("DirectChatStore schema init failed: {e}")))?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn append_message(
        &self,
        bot_id: &str,
        role: &str,
        author: &str,
        content: &str,
        reply_to_id: Option<&str>,
        thread_id: Option<&str>,
        card: Option<&serde_json::Value>,
        request_id: Option<&str>,
    ) -> Result<DirectMessageRecord> {
        let clean_bot = bot_id.trim().to_ascii_lowercase();
        if clean_bot.is_empty() {
            return Err(GmError::Config("bot_id cannot be empty".into()));
        }
        if content.trim().is_empty() && card.is_none() {
            return Err(GmError::Config(
                "Direct message must contain text or a structured card".into(),
            ));
        }

        let id = format!("dm-{}", uuid::Uuid::new_v4());
        let now = Utc::now().to_rfc3339();
        let card_json = card.map(serde_json::to_string).transpose()?;

        let conn = self.conn.lock();
        conn.execute(
            r#"
            INSERT INTO direct_messages_v1(
                id, bot_id, role, author, content, reply_to_id, thread_id, card_json, request_id, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            params![
                id,
                clean_bot,
                role.trim(),
                author.trim(),
                content,
                reply_to_id,
                thread_id,
                card_json,
                request_id,
                now
            ],
        )
        .map_err(|e| GmError::Platform(e.to_string()))?;

        let seq = conn.last_insert_rowid() as u64;
        if role.trim() == "user" {
            let _ = conn.execute(
                r#"
                INSERT INTO chat_read_cursors_v1(chat_kind, chat_id, last_read_seq)
                VALUES ('dm', ?1, ?2)
                ON CONFLICT(chat_kind, chat_id) DO UPDATE SET last_read_seq = max(last_read_seq, excluded.last_read_seq)
                "#,
                params![clean_bot, seq as i64],
            );
        }

        Ok(DirectMessageRecord {
            seq,
            id,
            bot_id: clean_bot,
            role: role.trim().to_string(),
            author: author.trim().to_string(),
            content: content.to_string(),
            reply_to_id: reply_to_id.map(str::to_string),
            thread_id: thread_id.map(str::to_string),
            card: card.cloned(),
            reactions: Vec::new(),
            request_id: request_id.map(str::to_string),
            created_at: now,
        })
    }

    pub fn list_messages(
        &self,
        bot_id: &str,
        before_seq: Option<u64>,
        limit: usize,
    ) -> Result<Vec<DirectMessageRecord>> {
        let clean_bot = bot_id.trim().to_ascii_lowercase();
        let limit_i64 = limit.clamp(1, 500) as i64;
        let conn = self.conn.lock();
        let mut out = Vec::new();
        {
            let sql = if before_seq.is_some() {
                r#"
                SELECT seq, id, bot_id, role, author, content, reply_to_id, thread_id, card_json, request_id, created_at
                FROM direct_messages_v1
                WHERE bot_id = ?1 AND seq < ?2
                ORDER BY seq DESC
                LIMIT ?3
                "#
            } else {
                r#"
                SELECT seq, id, bot_id, role, author, content, reply_to_id, thread_id, card_json, request_id, created_at
                FROM direct_messages_v1
                WHERE bot_id = ?1
                ORDER BY seq DESC
                LIMIT ?2
                "#
            };

            let mut stmt = conn.prepare(sql).map_err(|e| GmError::Platform(e.to_string()))?;
            let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<DirectMessageRecord> {
                let seq_i64: i64 = row.get(0)?;
                let card_str: Option<String> = row.get(8)?;
                let card = card_str
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok());
                Ok(DirectMessageRecord {
                    seq: seq_i64.max(0) as u64,
                    id: row.get(1)?,
                    bot_id: row.get(2)?,
                    role: row.get(3)?,
                    author: row.get(4)?,
                    content: row.get(5)?,
                    reply_to_id: row.get(6)?,
                    thread_id: row.get(7)?,
                    card,
                    reactions: Vec::new(),
                    request_id: row.get(9)?,
                    created_at: row.get(10)?,
                })
            };

            let rows = if let Some(bseq) = before_seq {
                stmt.query_map(params![clean_bot, bseq as i64, limit_i64], map_row)
            } else {
                stmt.query_map(params![clean_bot, limit_i64], map_row)
            }
            .map_err(|e| GmError::Platform(e.to_string()))?;

            for r in rows {
                out.push(r.map_err(|e| GmError::Platform(e.to_string()))?);
            }
        }
        out.reverse();
        for msg in &mut out {
            msg.reactions = Self::list_reactions_with_conn(&conn, &msg.id)?;
        }
        Ok(out)
    }

    pub fn update_card_by_message_id(
        &self,
        message_id: &str,
        card: &serde_json::Value,
    ) -> Result<Option<DirectMessageRecord>> {
        let conn = self.conn.lock();
        let card_json = serde_json::to_string(card)?;
        let changed = conn
            .execute(
                "UPDATE direct_messages_v1 SET card_json = ?1 WHERE id = ?2",
                params![card_json, message_id.trim()],
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        if changed == 0 {
            return Ok(None);
        }
        Self::get_by_id_with_conn(&conn, message_id.trim())
    }

    pub fn update_card_by_request_id(
        &self,
        request_id: &str,
        status: &str,
        extra: Option<serde_json::Value>,
    ) -> Result<Option<DirectMessageRecord>> {
        let conn = self.conn.lock();
        let existing: Option<(String, Option<String>)> = conn
            .query_row(
                "SELECT id, card_json FROM direct_messages_v1 WHERE request_id = ?1 ORDER BY seq DESC LIMIT 1",
                params![request_id.trim()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| GmError::Platform(e.to_string()))?;

        let Some((msg_id, card_str)) = existing else {
            return Ok(None);
        };
        let mut card_val: serde_json::Value = card_str
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        if let Some(obj) = card_val.as_object_mut() {
            obj.insert("status".into(), serde_json::json!(status));
            if let Some(serde_json::Value::Object(extra_map)) = extra {
                for (k, v) in extra_map {
                    obj.insert(k, v);
                }
            }
        }
        let encoded = serde_json::to_string(&card_val)?;
        conn.execute(
            "UPDATE direct_messages_v1 SET card_json = ?1 WHERE id = ?2",
            params![encoded, msg_id],
        )
        .map_err(|e| GmError::Platform(e.to_string()))?;
        Self::get_by_id_with_conn(&conn, &msg_id)
    }

    fn get_by_id_with_conn(conn: &Connection, id: &str) -> Result<Option<DirectMessageRecord>> {
        let rec: Option<DirectMessageRecord> = conn
            .query_row(
                r#"
                SELECT seq, id, bot_id, role, author, content, reply_to_id, thread_id, card_json, request_id, created_at
                FROM direct_messages_v1 WHERE id = ?1
                "#,
                params![id],
                |row| {
                    let seq_i64: i64 = row.get(0)?;
                    let card_str: Option<String> = row.get(8)?;
                    let card = card_str
                        .as_deref()
                        .and_then(|s| serde_json::from_str(s).ok());
                    Ok(DirectMessageRecord {
                        seq: seq_i64.max(0) as u64,
                        id: row.get(1)?,
                        bot_id: row.get(2)?,
                        role: row.get(3)?,
                        author: row.get(4)?,
                        content: row.get(5)?,
                        reply_to_id: row.get(6)?,
                        thread_id: row.get(7)?,
                        card,
                        reactions: Vec::new(),
                        request_id: row.get(9)?,
                        created_at: row.get(10)?,
                    })
                },
            )
            .optional()
            .map_err(|e| GmError::Platform(e.to_string()))?;
        if let Some(mut msg) = rec {
            msg.reactions = Self::list_reactions_with_conn(conn, &msg.id)?;
            Ok(Some(msg))
        } else {
            Ok(None)
        }
    }

    fn list_reactions_with_conn(
        conn: &Connection,
        message_id: &str,
    ) -> Result<Vec<MessageReactionRecord>> {
        let mut stmt = conn
            .prepare(
                "SELECT id, message_id, emoji, actor, created_at FROM dm_reactions_v1 WHERE message_id = ?1 ORDER BY created_at ASC, id ASC",
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let rows = stmt
            .query_map(params![message_id], |r| {
                Ok(MessageReactionRecord {
                    id: r.get(0)?,
                    message_id: r.get(1)?,
                    emoji: r.get(2)?,
                    actor: r.get(3)?,
                    created_at: r.get(4)?,
                })
            })
            .map_err(|e| GmError::Platform(e.to_string()))?;
        rows.map(|r| r.map_err(|e| GmError::Platform(e.to_string())))
            .collect()
    }

    pub fn toggle_reaction(
        &self,
        message_id: &str,
        emoji: &str,
        actor: &str,
    ) -> Result<(String, Vec<MessageReactionRecord>)> {
        let clean_msg = message_id.trim();
        let clean_emoji = emoji.trim();
        let clean_actor = if actor.trim().is_empty() {
            "operator"
        } else {
            actor.trim()
        };
        if clean_msg.is_empty() {
            return Err(GmError::Config("Message ID cannot be empty".into()));
        }
        if clean_emoji.is_empty() || clean_emoji.chars().count() > 32 || clean_emoji.contains('\n') {
            return Err(GmError::Config("Reaction emoji must be 1-32 characters".into()));
        }

        let conn = self.conn.lock();
        let bot_id: Option<String> = conn
            .query_row(
                "SELECT bot_id FROM direct_messages_v1 WHERE id = ?1",
                params![clean_msg],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let Some(bot_id) = bot_id else {
            return Err(GmError::Config(format!("Unknown direct message '{clean_msg}'")));
        };

        let deleted = conn
            .execute(
                "DELETE FROM dm_reactions_v1 WHERE message_id = ?1 AND emoji = ?2 AND actor = ?3",
                params![clean_msg, clean_emoji, clean_actor],
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;

        if deleted == 0 {
            let id = format!("dmrx-{}", uuid::Uuid::new_v4());
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "INSERT OR IGNORE INTO dm_reactions_v1(id, message_id, emoji, actor, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, clean_msg, clean_emoji, clean_actor, now],
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        }

        let list = Self::list_reactions_with_conn(&conn, clean_msg)?;
        Ok((bot_id, list))
    }

    pub fn mark_chat_read(&self, chat_kind: &str, chat_id: &str) -> Result<u64> {
        let kind = chat_kind.trim().to_ascii_lowercase();
        let id = chat_id.trim().to_ascii_lowercase();
        if kind.is_empty() || id.is_empty() {
            return Err(GmError::Config("chat_kind and chat_id required".into()));
        }
        let conn = self.conn.lock();
        let max_seq: i64 = if kind == "dm" {
            conn.query_row(
                "SELECT coalesce(max(seq), 0) FROM direct_messages_v1 WHERE bot_id = ?1",
                params![&id],
                |r| r.get(0),
            )
            .unwrap_or(0)
        } else {
            conn.query_row(
                "SELECT coalesce(max(rowid), 0) FROM channel_messages WHERE channel_id = ?1",
                params![&id],
                |r| r.get(0),
            )
            .unwrap_or(0)
        };
        conn.execute(
            r#"
            INSERT INTO chat_read_cursors_v1(chat_kind, chat_id, last_read_seq)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(chat_kind, chat_id) DO UPDATE SET last_read_seq = max(last_read_seq, excluded.last_read_seq)
            "#,
            params![kind, id, max_seq],
        )
        .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(max_seq.max(0) as u64)
    }

    pub fn unread_counts(&self) -> Result<BTreeMap<String, usize>> {
        let conn = self.conn.lock();
        let mut counts = BTreeMap::new();
        {
            let mut stmt = conn
                .prepare(
                    r#"
                    SELECT dm.bot_id, COUNT(*)
                    FROM direct_messages_v1 dm
                    LEFT JOIN chat_read_cursors_v1 c
                        ON c.chat_kind = 'dm' AND c.chat_id = dm.bot_id
                    WHERE dm.role != 'user' AND dm.seq > coalesce(c.last_read_seq, 0)
                    GROUP BY dm.bot_id
                    "#,
                )
                .map_err(|e| GmError::Platform(e.to_string()))?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                .map_err(|e| GmError::Platform(e.to_string()))?;
            for r in rows {
                let (bot_id, cnt) = r.map_err(|e| GmError::Platform(e.to_string()))?;
                if cnt > 0 {
                    counts.insert(format!("dm:{bot_id}"), cnt as usize);
                }
            }
        }
        // Channels (best effort if channel_messages table exists in same SQLite DB)
        let chan_table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='channel_messages'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(false);
        if chan_table_exists {
            let mut stmt = conn
                .prepare(
                    r#"
                    SELECT m.channel_id, COUNT(*)
                    FROM channel_messages m
                    LEFT JOIN chat_read_cursors_v1 c
                        ON c.chat_kind = 'channel' AND c.chat_id = m.channel_id
                    WHERE m.bot_id IS NOT NULL AND m.rowid > coalesce(c.last_read_seq, 0)
                    GROUP BY m.channel_id
                    "#,
                )
                .map_err(|e| GmError::Platform(e.to_string()))?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                .map_err(|e| GmError::Platform(e.to_string()))?;
            for r in rows {
                let (chan_id, cnt) = r.map_err(|e| GmError::Platform(e.to_string()))?;
                if cnt > 0 {
                    counts.insert(format!("channel:{chan_id}"), cnt as usize);
                }
            }
        }
        Ok(counts)
    }

    pub fn set_bot_status(
        &self,
        bot_id: &str,
        phrase: Option<&str>,
        needs_attention: bool,
    ) -> Result<()> {
        let clean_bot = bot_id.trim().to_ascii_lowercase();
        let clean_phrase = phrase
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(120).collect::<String>());
        let now = Utc::now().to_rfc3339();
        self.conn
            .lock()
            .execute(
                r#"
                INSERT INTO bot_status_phrases_v1(bot_id, phrase, needs_attention, updated_at)
                VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT(bot_id) DO UPDATE SET
                    phrase = excluded.phrase,
                    needs_attention = excluded.needs_attention,
                    updated_at = excluded.updated_at
                "#,
                params![clean_bot, clean_phrase, needs_attention as i32, now],
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        Ok(())
    }

    pub fn list_bot_statuses(&self) -> Result<BTreeMap<String, BotStatusState>> {
        let unread = self.unread_counts()?;
        let conn = self.conn.lock();
        let mut out = BTreeMap::new();
        {
            let mut stmt = conn
                .prepare(
                    "SELECT bot_id, phrase, needs_attention, updated_at FROM bot_status_phrases_v1",
                )
                .map_err(|e| GmError::Platform(e.to_string()))?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, i32>(2)? != 0,
                        r.get::<_, String>(3)?,
                    ))
                })
                .map_err(|e| GmError::Platform(e.to_string()))?;
            for r in rows {
                let (bot_id, phrase, needs_attention, updated_at) =
                    r.map_err(|e| GmError::Platform(e.to_string()))?;
                let u = unread.get(&format!("dm:{bot_id}")).copied().unwrap_or(0);
                out.insert(
                    bot_id.clone(),
                    BotStatusState {
                        bot_id,
                        phrase,
                        needs_attention,
                        unread_count: u,
                        last_read_seq: 0,
                        updated_at,
                    },
                );
            }
        }
        Ok(out)
    }

    /// Two-stage search: when `include_older == false`, inspects only the latest 200 messages
    /// per table; when `include_older == true`, searches full persisted SQLite history.
    pub fn search_messages(
        &self,
        query: &str,
        bot_id: Option<&str>,
        include_older: bool,
        limit: usize,
    ) -> Result<Vec<MessageSearchHit>> {
        let needle = query.trim();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let like_pattern = format!("%{}%", escape_like(needle));
        let max_hits = limit.clamp(1, 100);
        let conn = self.conn.lock();
        let mut hits = Vec::new();

        // 1. Search DMs
        {
            let sql = match (bot_id, include_older) {
                (Some(_), true) => {
                    "SELECT bot_id, id, author, content, created_at FROM direct_messages_v1 WHERE bot_id = ?1 AND content LIKE ?2 ESCAPE '\\' ORDER BY seq DESC LIMIT ?3"
                }
                (Some(_), false) => {
                    "SELECT bot_id, id, author, content, created_at FROM (SELECT * FROM direct_messages_v1 WHERE bot_id = ?1 ORDER BY seq DESC LIMIT 200) WHERE content LIKE ?2 ESCAPE '\\' ORDER BY seq DESC LIMIT ?3"
                }
                (None, true) => {
                    "SELECT bot_id, id, author, content, created_at FROM direct_messages_v1 WHERE content LIKE ?1 ESCAPE '\\' ORDER BY seq DESC LIMIT ?2"
                }
                (None, false) => {
                    "SELECT bot_id, id, author, content, created_at FROM (SELECT * FROM direct_messages_v1 ORDER BY seq DESC LIMIT 200) WHERE content LIKE ?1 ESCAPE '\\' ORDER BY seq DESC LIMIT ?2"
                }
            };
            let mut stmt = conn.prepare(sql).map_err(|e| GmError::Platform(e.to_string()))?;
            let map_hit = |r: &rusqlite::Row<'_>| -> rusqlite::Result<MessageSearchHit> {
                let body: String = r.get(3)?;
                let excerpt = make_excerpt(&body, needle, 140);
                Ok(MessageSearchHit {
                    chat_kind: "dm".into(),
                    chat_id: r.get(0)?,
                    message_id: r.get(1)?,
                    author: r.get(2)?,
                    content_excerpt: excerpt,
                    timestamp: r.get(4)?,
                })
            };
            let rows = if let Some(b) = bot_id {
                stmt.query_map(
                    params![b.trim().to_ascii_lowercase(), like_pattern, max_hits as i64],
                    map_hit,
                )
            } else {
                stmt.query_map(params![like_pattern, max_hits as i64], map_hit)
            }
            .map_err(|e| GmError::Platform(e.to_string()))?;
            for r in rows {
                hits.push(r.map_err(|e| GmError::Platform(e.to_string()))?);
            }
        }

        // 2. Search Channels (when present and not filtered exclusively to non-channel or full search)
        let chan_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='channel_messages'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(false);
        if chan_exists && bot_id.is_none() {
            let sql = if include_older {
                "SELECT channel_id, id, author, content, timestamp FROM channel_messages WHERE content LIKE ?1 ESCAPE '\\' ORDER BY rowid DESC LIMIT ?2"
            } else {
                "SELECT channel_id, id, author, content, timestamp FROM (SELECT rowid, channel_id, id, author, content, timestamp FROM channel_messages ORDER BY rowid DESC LIMIT 200) WHERE content LIKE ?1 ESCAPE '\\' ORDER BY rowid DESC LIMIT ?2"
            };
            let mut stmt = conn.prepare(sql).map_err(|e| GmError::Platform(e.to_string()))?;
            let rows = stmt
                .query_map(params![like_pattern, max_hits as i64], |r| {
                    let body: String = r.get(3)?;
                    Ok(MessageSearchHit {
                        chat_kind: "channel".into(),
                        chat_id: r.get(0)?,
                        message_id: r.get(1)?,
                        author: r.get(2)?,
                        content_excerpt: make_excerpt(&body, needle, 140),
                        timestamp: r.get(4)?,
                    })
                })
                .map_err(|e| GmError::Platform(e.to_string()))?;
            for r in rows {
                hits.push(r.map_err(|e| GmError::Platform(e.to_string()))?);
            }
        }

        hits.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        hits.truncate(max_hits);
        Ok(hits)
    }

    pub fn collect_urls_and_media_for_bot(&self, bot_id: &str, limit: usize) -> Result<Vec<(String, String, String)>> {
        let clean_bot = bot_id.trim().to_ascii_lowercase();
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, content, created_at FROM direct_messages_v1 WHERE bot_id = ?1 ORDER BY seq DESC LIMIT ?2",
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let rows = stmt
            .query_map(params![clean_bot, limit.clamp(1, 500) as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| GmError::Platform(e.to_string()))?);
        }
        Ok(out)
    }
}

fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

fn make_excerpt(body: &str, needle: &str, max_chars: usize) -> String {
    let lower_body = body.to_lowercase();
    let lower_needle = needle.to_lowercase();
    if let Some(pos) = lower_body.find(&lower_needle) {
        let prefix_chars: String = body[..pos].chars().rev().take(32).collect::<Vec<_>>().into_iter().rev().collect();
        let match_and_rest: String = body[pos..].chars().take(max_chars.saturating_sub(prefix_chars.len())).collect();
        let prefix_ellipsis = if prefix_chars.len() < pos { "…" } else { "" };
        format!("{prefix_ellipsis}{prefix_chars}{match_and_rest}")
    } else {
        body.chars().take(max_chars).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_chat_reactions_read_cursors_and_two_stage_search() {
        let store = DirectChatStore::in_memory().unwrap();
        let m1 = store
            .append_message("muse", "user", "operator", "Check invoice #901 please", None, None, None, None)
            .unwrap();
        let m2 = store
            .append_message(
                "muse",
                "assistant",
                "Muse Operator",
                "Found invoice #901",
                Some(&m1.id),
                Some(&m1.id),
                Some(&serde_json::json!({"kind": "approval", "status": "pending"})),
                Some("req-101"),
            )
            .unwrap();

        let unread = store.unread_counts().unwrap();
        assert_eq!(unread.get("dm:muse").copied(), Some(1));

        store.mark_chat_read("dm", "muse").unwrap();
        assert_eq!(store.unread_counts().unwrap().get("dm:muse").copied(), None);

        let (bot, rxns) = store.toggle_reaction(&m2.id, "👍", "operator").unwrap();
        assert_eq!(bot, "muse");
        assert_eq!(rxns.len(), 1);
        let (_, rxns_after_toggle) = store.toggle_reaction(&m2.id, "👍", "operator").unwrap();
        assert!(rxns_after_toggle.is_empty());

        let updated = store
            .update_card_by_request_id("req-101", "approved", None)
            .unwrap()
            .unwrap();
        assert_eq!(updated.card.unwrap()["status"], "approved");

        let hits = store.search_messages("invoice", Some("muse"), false, 20).unwrap();
        assert_eq!(hits.len(), 2);

        // Push >200 filler messages so m1 falls outside recent 200 window
        for i in 0..205 {
            store
                .append_message("muse", "assistant", "Muse Operator", &format!("filler {i}"), None, None, None, None)
                .unwrap();
        }
        let recent_hits = store.search_messages("invoice #901", Some("muse"), false, 20).unwrap();
        assert!(recent_hits.is_empty());
        let older_hits = store.search_messages("invoice #901", Some("muse"), true, 20).unwrap();
        assert_eq!(older_hits.len(), 2);
    }
}
