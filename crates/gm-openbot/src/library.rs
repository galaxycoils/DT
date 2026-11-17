//! Bot Library aggregator (`All | Pinned | Pages | Media | Links | Files | Apps`)
//! grouped strictly into `Pinned | Today | This week | Older` and paginated in 5-item batches.

use crate::connectors::ConnectorStore;
use crate::direct_chat::DirectChatStore;
use crate::roster::BotRoster;
use crate::spaces::SpacesManager;
use chrono::{DateTime, Duration, Utc};
use gm_core::config::GmConfig;
use gm_core::error::{GmError, Result};
use parking_lot::Mutex;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LibraryFilter {
    All,
    Pinned,
    Pages,
    Media,
    Links,
    Files,
    Apps,
}

impl LibraryFilter {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "pinned" => LibraryFilter::Pinned,
            "pages" | "page" | "spaces" => LibraryFilter::Pages,
            "media" | "images" | "videos" => LibraryFilter::Media,
            "links" | "urls" => LibraryFilter::Links,
            "files" | "file" => LibraryFilter::Files,
            "apps" | "app" => LibraryFilter::Apps,
            _ => LibraryFilter::All,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LibraryGroup {
    Pinned,
    Today,
    ThisWeek,
    Older,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryItem {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub subtitle: String,
    pub uri_or_path: String,
    pub bot_id: String,
    pub pinned: bool,
    pub group: LibraryGroup,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryPage {
    pub items: Vec<LibraryItem>,
    pub total_count: usize,
    pub next_cursor: Option<usize>,
}

#[derive(Clone)]
pub struct LibraryStore {
    conn: Arc<Mutex<Connection>>,
}

impl LibraryStore {
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        let conn = crate::db_recovery::open_sqlite_with_recovery(db_path.as_ref())?;
        Self::init_conn(conn)
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| GmError::Platform(format!("Failed to open in-memory library DB: {e}")))?;
        Self::init_conn(conn)
    }

    fn init_conn(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS library_pins_v1 (
                item_id TEXT PRIMARY KEY,
                bot_id TEXT NOT NULL,
                pinned_at TEXT NOT NULL
            );
            "#,
        )
        .map_err(|e| GmError::Platform(format!("LibraryStore schema init failed: {e}")))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn set_pinned(&self, bot_id: &str, item_id: &str, pinned: bool) -> Result<bool> {
        let conn = self.conn.lock();
        if pinned {
            conn.execute(
                "INSERT OR REPLACE INTO library_pins_v1(item_id, bot_id, pinned_at) VALUES (?1, ?2, ?3)",
                params![item_id.trim(), bot_id.trim(), Utc::now().to_rfc3339()],
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
            Ok(true)
        } else {
            conn.execute(
                "DELETE FROM library_pins_v1 WHERE item_id = ?1",
                params![item_id.trim()],
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
            Ok(false)
        }
    }

    fn pinned_ids(&self) -> BTreeSet<String> {
        let conn = self.conn.lock();
        let Ok(mut stmt) = conn.prepare("SELECT item_id FROM library_pins_v1") else {
            return BTreeSet::new();
        };
        let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
            return BTreeSet::new();
        };
        rows.flatten().collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn query(
        &self,
        bot_id: &str,
        filter: LibraryFilter,
        cursor: usize,
        limit: usize,
        roster: &BotRoster,
        direct_chat: &DirectChatStore,
        spaces: &SpacesManager,
        connectors: &ConnectorStore,
        cfg: &GmConfig,
    ) -> Result<LibraryPage> {
        let pins = self.pinned_ids();
        let now = Utc::now();
        let mut all = Vec::new();

        // 1. Pages (Static Spaces)
        if matches!(filter, LibraryFilter::All | LibraryFilter::Pinned | LibraryFilter::Pages)
            && let Ok(space_list) = spaces.list_spaces()
        {
            for s in space_list {
                let id = format!("page:{}", s.slug);
                let pinned = pins.contains(&id);
                let ts = s.updated_at.clone();
                all.push(LibraryItem {
                    group: classify_group(pinned, &ts, now),
                    id,
                    kind: "pages".into(),
                    title: s.title,
                    subtitle: format!("Static Space · /spaces/v2/{}", s.slug),
                    uri_or_path: s.slug,
                    bot_id: bot_id.to_string(),
                    pinned,
                    created_at: ts,
                });
            }
        }

        // 2. Files (Bot sandbox shared files in inbox/ & outbox/)
        if matches!(filter, LibraryFilter::All | LibraryFilter::Pinned | LibraryFilter::Files)
            && let Ok(sandbox) = roster.workspace_sandbox(bot_id)
            && let Ok(files) = sandbox.shared_files()
        {
            for (f, _size) in files {
                let id = format!("file:{bot_id}:{f}");
                let pinned = pins.contains(&id);
                let is_media = is_media_ext(&f);
                all.push(LibraryItem {
                    group: classify_group(pinned, "", now),
                    id,
                    kind: if is_media { "media" } else { "files" }.into(),
                    title: f.rsplit('/').next().unwrap_or(&f).to_string(),
                    subtitle: format!("Bot sandbox ({f})"),
                    uri_or_path: format!("sandbox://{f}"),
                    bot_id: bot_id.to_string(),
                    pinned,
                    created_at: now.to_rfc3339(),
                });
            }
        }

        // 3. Links & Media extracted from recent DM messages
        if matches!(
            filter,
            LibraryFilter::All | LibraryFilter::Pinned | LibraryFilter::Media | LibraryFilter::Links
        ) && let Ok(msgs) = direct_chat.collect_urls_and_media_for_bot(bot_id, 250)
        {
            let mut seen_urls = BTreeSet::new();
            for (_msg_id, content, ts) in msgs {
                for token in content.split_whitespace() {
                    let trimmed = token.trim_matches(|c: char| matches!(c, '(' | ')' | '[' | ']' | '<' | '>' | ',' | '.' | ';' | '"' | '\''));
                    if let Some(rel) = trimmed.strip_prefix("sandbox://")
                        && seen_urls.insert(trimmed.to_string())
                    {
                        let kind = if is_media_ext(rel) { "media" } else { "files" };
                        if (filter == LibraryFilter::Media && kind != "media")
                            || (filter == LibraryFilter::Links)
                        {
                            continue;
                        }
                        let id = format!("media:{bot_id}:{rel}");
                        let pinned = pins.contains(&id);
                        all.push(LibraryItem {
                            group: classify_group(pinned, &ts, now),
                            id,
                            kind: kind.into(),
                            title: rel.rsplit('/').next().unwrap_or(rel).to_string(),
                            subtitle: "Chat attachment".into(),
                            uri_or_path: trimmed.to_string(),
                            bot_id: bot_id.to_string(),
                            pinned,
                            created_at: ts.clone(),
                        });
                    } else if (trimmed.starts_with("https://") || trimmed.starts_with("http://"))
                        && seen_urls.insert(trimmed.to_string())
                    {
                        if filter == LibraryFilter::Media {
                            continue;
                        }
                        let id = format!("link:{}", trimmed);
                        let pinned = pins.contains(&id);
                        all.push(LibraryItem {
                            group: classify_group(pinned, &ts, now),
                            id,
                            kind: "links".into(),
                            title: trimmed.chars().take(64).collect(),
                            subtitle: "Shared URL".into(),
                            uri_or_path: trimmed.to_string(),
                            bot_id: bot_id.to_string(),
                            pinned,
                            created_at: ts.clone(),
                        });
                    }
                }
            }
        }

        // 4. Apps / Connectors
        if matches!(filter, LibraryFilter::All | LibraryFilter::Pinned | LibraryFilter::Apps) {
            if let Ok(custom) = connectors.list_connectors() {
                for c in custom {
                    if c.bot_ids.is_empty() || c.bot_ids.iter().any(|b| b == bot_id) {
                        let id = format!("app:{}", c.id);
                        let pinned = pins.contains(&id);
                        all.push(LibraryItem {
                            group: classify_group(pinned, &c.updated_at, now),
                            id,
                            kind: "apps".into(),
                            title: c.name,
                            subtitle: format!("{} · {}", c.transport.as_str(), c.status_text),
                            uri_or_path: c.url_or_command,
                            bot_id: bot_id.to_string(),
                            pinned,
                            created_at: c.updated_at,
                        });
                    }
                }
            }
            for builtin in connectors.builtin_integrations(cfg) {
                if builtin.status == "connected" {
                    let id = format!("app:{}", builtin.id);
                    let pinned = pins.contains(&id);
                    all.push(LibraryItem {
                        group: classify_group(pinned, "", now),
                        id,
                        kind: "apps".into(),
                        title: builtin.name,
                        subtitle: builtin.account_summary,
                        uri_or_path: format!("app://{}", builtin.id),
                        bot_id: bot_id.to_string(),
                        pinned,
                        created_at: now.to_rfc3339(),
                    });
                }
            }
        }

        // Apply filter & sort: Pinned first, then newest first
        all.retain(|item| match filter {
            LibraryFilter::All => true,
            LibraryFilter::Pinned => item.pinned,
            LibraryFilter::Pages => item.kind == "pages",
            LibraryFilter::Media => item.kind == "media",
            LibraryFilter::Links => item.kind == "links",
            LibraryFilter::Files => item.kind == "files",
            LibraryFilter::Apps => item.kind == "apps",
        });

        all.sort_by(|a, b| {
            b.pinned
                .cmp(&a.pinned)
                .then_with(|| b.created_at.cmp(&a.created_at))
                .then_with(|| a.title.cmp(&b.title))
        });

        let total_count = all.len();
        let page_limit = limit.clamp(1, 50);
        let items: Vec<LibraryItem> = all.into_iter().skip(cursor).take(page_limit).collect();
        let next_cursor = if cursor + items.len() < total_count {
            Some(cursor + items.len())
        } else {
            None
        };

        Ok(LibraryPage {
            items,
            total_count,
            next_cursor,
        })
    }
}

fn is_media_ext(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".webp")
        || lower.ends_with(".gif")
        || lower.ends_with(".mp4")
        || lower.ends_with(".mov")
        || lower.ends_with(".webm")
}

fn classify_group(pinned: bool, ts: &str, now: DateTime<Utc>) -> LibraryGroup {
    if pinned {
        return LibraryGroup::Pinned;
    }
    if let Ok(parsed) = DateTime::parse_from_rfc3339(ts) {
        let utc = parsed.with_timezone(&Utc);
        if utc.date_naive() == now.date_naive() {
            return LibraryGroup::Today;
        }
        if now.signed_duration_since(utc) <= Duration::days(7) {
            return LibraryGroup::ThisWeek;
        }
        return LibraryGroup::Older;
    }
    LibraryGroup::Today
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_pins_and_5_item_pagination() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = GmConfig {
            storage_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        cfg.openbot.bots_dir = dir.path().join("bots");
        let roster = BotRoster::open_with_storage(&cfg.openbot.bots_dir, &cfg.storage_dir).unwrap();
        let dc = DirectChatStore::in_memory().unwrap();
        let spaces = SpacesManager::new(
            gm_core::muse_paths::MuseRuntimePaths::shared(&cfg.storage_dir),
            Arc::new(gm_safety::AuditLogger::in_memory()),
            cfg.muse.clone(),
            7433,
        );
        let conns = ConnectorStore::in_memory().unwrap();
        let lib = LibraryStore::in_memory().unwrap();

        for i in 0..7 {
            dc.append_message(
                "muse",
                "assistant",
                "Muse",
                &format!("See https://example.com/item-{i}"),
                None,
                None,
                None,
                None,
            )
            .unwrap();
        }
        lib.set_pinned("muse", "link:https://example.com/item-0", true)
            .unwrap();

        let p1 = lib
            .query(
                "muse",
                LibraryFilter::Links,
                0,
                5,
                &roster,
                &dc,
                &spaces,
                &conns,
                &cfg,
            )
            .unwrap();
        assert_eq!(p1.total_count, 7);
        assert_eq!(p1.items.len(), 5);
        assert!(p1.items[0].pinned);
        assert_eq!(p1.items[0].group, LibraryGroup::Pinned);
        assert_eq!(p1.next_cursor, Some(5));

        let p2 = lib
            .query(
                "muse",
                LibraryFilter::Links,
                5,
                5,
                &roster,
                &dc,
                &spaces,
                &conns,
                &cfg,
            )
            .unwrap();
        assert_eq!(p2.items.len(), 2);
        assert_eq!(p2.next_cursor, None);
    }
}
