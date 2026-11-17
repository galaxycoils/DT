//! Two-phase discretionary Forget planner and executor following Muse's
//! 9-section artifact inventory while preserving unrelated content,
//! platform migrations, and visible chat transcripts.

use crate::PersonalMemoryStore;
use crate::provenance::{ClaimStatus, ProvenanceStore, sha256_hex};
use chrono::Utc;
use gm_core::error::{GmError, Result};
use gm_core::muse_paths::MuseRuntimePaths;
use gm_muse::shared_files::SharedFileStore;
use gm_safety::{AuditCategory, AuditLogger};
use rusqlite::{Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

pub type FeedResetFenceFn = Arc<dyn Fn(&ForgetPlan) -> Result<()> + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueTable {
    BotTasks,
    TaskAttempts,
    RuntimeEvents,
    BridgeDeliveries,
}

impl QueueTable {
    pub fn as_str(&self) -> &'static str {
        match self {
            QueueTable::BotTasks => "bot_tasks",
            QueueTable::TaskAttempts => "task_attempts",
            QueueTable::RuntimeEvents => "runtime_events",
            QueueTable::BridgeDeliveries => "bridge_deliveries",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutineTable {
    RoutinesV1,
    RoutineRunsV1,
}

impl RoutineTable {
    pub fn as_str(&self) -> &'static str {
        match self {
            RoutineTable::RoutinesV1 => "routines_v1",
            RoutineTable::RoutineRunsV1 => "routine_runs_v1",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum FileCleanupTarget {
    RemoveLine {
        relative_path: String,
        line_number: usize,
        line_sha256: String,
    },
    DeleteStandaloneFile {
        relative_path: String,
        file_sha256: String,
    },
    RedactGoalCatalog {
        relative_path: String,
        file_sha256: String,
        goal_ids: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target", rename_all = "snake_case")]
pub enum ForgetTarget {
    DariusRecord {
        record_id: String,
    },
    ProvenanceClaim {
        claim_id: String,
        record_id: String,
        source_locator: String,
    },
    UserMd {
        line_number: usize,
        line_sha256: String,
    },
    MemoryMd {
        line_number: usize,
        line_sha256: String,
    },
    ProactivePreferencesMd {
        line_number: usize,
        line_sha256: String,
    },
    FeedbackMd {
        line_number: usize,
        line_sha256: String,
    },
    MemoryCorpus(FileCleanupTarget),
    DreamsCorpus(FileCleanupTarget),
    GoalsCorpus(FileCleanupTarget),
    SideChatsCorpus(FileCleanupTarget),
    CustomSkillsCorpus(FileCleanupTarget),
    QueueRow {
        table: QueueTable,
        row_id: String,
    },
    RoutineRow {
        table: RoutineTable,
        row_id: String,
    },
    AuditLogLine {
        line_number: usize,
        line_sha256: String,
    },
    FeedSignals,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgetPlan {
    pub plan_id: String,
    /// First 16 hex chars of SHA-256 of normalized request; never stores raw request text.
    pub request_fingerprint: String,
    /// Claim IDs matched in `provenance.sqlite`.
    pub claims: Vec<String>,
    /// File line / standalone locators (only paths/line numbers, never sensitive text).
    pub citations: Vec<String>,
    /// Discreet typed cleanup actions containing IDs and hashes only.
    pub targets: Vec<ForgetTarget>,
    /// Discreet per-category counts for user confirmation prompts.
    pub summary_counts: BTreeMap<String, usize>,
    /// Plain-language disclosures of stores intentionally excluded from local erasure.
    pub excluded_disclosures: Vec<String>,
    pub feed_reset_required: bool,
    pub redact_audit: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForgetStatus {
    Forgotten,
    IncompleteFilesChanged,
    IncompleteRemainingHits,
}

impl ForgetStatus {
    pub fn label(&self) -> &'static str {
        match self {
            ForgetStatus::Forgotten => "forgotten",
            ForgetStatus::IncompleteFilesChanged => "incomplete: files changed since plan",
            ForgetStatus::IncompleteRemainingHits => "incomplete",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgetReport {
    pub plan_id: String,
    pub request_fingerprint: String,
    pub status: ForgetStatus,
    pub message: String,
    pub removed_lines: usize,
    pub deleted_files: usize,
    pub deleted_darius_records: usize,
    pub retracted_claims: Vec<String>,
    pub deleted_queue_rows: usize,
    pub deleted_routine_rows: usize,
    pub redacted_audit_entries: usize,
    pub feed_reset_executed: bool,
    pub remaining: Vec<String>,
    pub excluded_disclosures: Vec<String>,
}

#[derive(Clone)]
pub struct ForgetResources {
    pub paths: MuseRuntimePaths,
    pub memory: PersonalMemoryStore,
    pub provenance: ProvenanceStore,
    pub openbot_db_path: Option<PathBuf>,
    pub feed_db_path: Option<PathBuf>,
    pub audit_log_path: Option<PathBuf>,
    pub audit_logger: Option<Arc<AuditLogger>>,
    pub forget_redact_audit: bool,
    pub feed_reset_fence: Option<FeedResetFenceFn>,
}

impl ForgetResources {
    pub fn new(
        paths: MuseRuntimePaths,
        memory: PersonalMemoryStore,
        provenance: ProvenanceStore,
    ) -> Self {
        let storage = paths.storage_dir().to_path_buf();
        Self {
            paths,
            memory,
            provenance,
            openbot_db_path: Some(storage.join("openbot.sqlite")),
            feed_db_path: Some(storage.join("feed.sqlite")),
            audit_log_path: Some(storage.join("audit.jsonl")),
            audit_logger: None,
            forget_redact_audit: true,
            feed_reset_fence: None,
        }
    }
}

pub fn default_excluded_disclosures() -> Vec<String> {
    vec![
        "Visible chat transcripts (channel messages) are excluded and left intact.".into(),
        "Platform migration bookkeeping (self_improvement/migrations/) is preserved and never deleted."
            .into(),
        "Copies already sent to external model providers or third-party services cannot be erased locally."
            .into(),
        "This also resets signals and interests derived for Feed; future Feed posts rebuild only from new events."
            .into(),
    ]
}

pub struct ForgetPlanner;
pub struct ForgetExecutor;

fn compute_request_fingerprint(request: &str) -> String {
    let normalized = request.trim().to_lowercase();
    sha256_hex(&normalized)[..16].to_string()
}

fn contains_ignore_ascii_case(haystack: &str, needle_lower: &str) -> bool {
    haystack.to_lowercase().contains(needle_lower)
}

fn is_unsafe_relative_path(rel: &str) -> bool {
    rel.is_empty()
        || rel.contains('*')
        || rel.contains('?')
        || rel.contains('[')
        || rel.contains("..")
        || rel.starts_with('/')
        || rel.contains('\\')
        || rel.contains('\0')
}

fn collect_regular_files_no_follow(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return Ok(());
    };
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Ok(());
    }
    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    entries.sort();
    for path in entries {
        let Ok(m) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if m.file_type().is_symlink() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name == ".muse.lock" || name.starts_with(".gm-write-") {
            continue;
        }
        if m.is_dir() {
            collect_regular_files_no_follow(&path, out)?;
        } else if m.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn is_standalone_deletable_file(rel: &str) -> bool {
    if rel == "USER.md"
        || rel == "MEMORY.md"
        || rel == "PROACTIVE_PREFERENCES.md"
        || rel == "FEEDBACK.md"
        || rel == "goals/goals.json"
        || rel == "memory/bank/experience.md"
        || rel == "memory/bank/opinions.md"
        || rel == "memory/bank/world.md"
        || rel == "memory/shopping/PROFILE.md"
        || rel == "memory/travel/PROFILE.md"
        || rel.ends_with("/INDEX.md")
        || rel == "INDEX.md"
        || rel.ends_with(".json")
    {
        return false;
    }
    let ext = Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    matches!(ext, "md" | "txt" | "yaml" | "yml")
}

fn is_supported_corpus_file(rel: &str, abs: &Path) -> bool {
    if rel == "goals/goals.json" {
        return true;
    }
    let ext = abs
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(ext.as_str(), "md" | "txt" | "yaml" | "yml" | "json")
}

fn inspect_file_for_matches(
    abs: &Path,
    rel: &str,
    needle_lower: &str,
    allow_standalone: bool,
) -> Result<Vec<FileCleanupTarget>> {
    if allow_standalone && !is_supported_corpus_file(rel, abs) {
        return Ok(Vec::new());
    }
    let is_json = rel.ends_with(".json");
    let (content, file_sha) = match SharedFileStore::global().read_text_with_sha256(abs) {
        Ok(Some(pair)) => pair,
        Ok(None) => return Ok(Vec::new()),
        Err(err) => {
            // Corrupt or non-UTF8 JSON files must fail explicitly; unrelated binary artifacts are skipped.
            if is_json {
                return Err(err);
            }
            return Ok(Vec::new());
        }
    };

    if rel == "goals/goals.json" {
        if content.trim().is_empty() {
            return Ok(Vec::new());
        }
        let goals = gm_core::goal::Goal::list_from_json_normalized(&content)
            .map_err(|e| GmError::Config(format!("Corrupt goals catalog '{rel}': {e}")))?;
        let mut matched_goal_ids = Vec::new();
        for goal in &goals {
            if goal.matches_structural_field(needle_lower) {
                return Err(GmError::Config(format!(
                    "Refusing unsupported structured edit to required/link field in '{rel}' (goal '{}')",
                    goal.id
                )));
            }
            if goal.matches_supported_text_field(needle_lower) {
                matched_goal_ids.push(goal.id.clone());
            }
        }
        if matched_goal_ids.is_empty() {
            if contains_ignore_ascii_case(&content, needle_lower) {
                return Err(GmError::Config(format!(
                    "Refusing unsupported structured edit in '{rel}'"
                )));
            }
            return Ok(Vec::new());
        }
        return Ok(vec![FileCleanupTarget::RedactGoalCatalog {
            relative_path: rel.to_string(),
            file_sha256: file_sha,
            goal_ids: matched_goal_ids,
        }]);
    }

    if is_json {
        if contains_ignore_ascii_case(&content, needle_lower) {
            return Err(GmError::Config(format!(
                "Refusing unsupported line-based edit on structured JSON file '{rel}'"
            )));
        }
        return Ok(Vec::new());
    }

    let lines: Vec<&str> = content.lines().collect();
    let mut matched_lines = Vec::new();
    let mut non_header_unmatched = 0usize;

    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if contains_ignore_ascii_case(line, needle_lower) {
            matched_lines.push((idx + 1, sha256_hex(line)));
        } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
            non_header_unmatched += 1;
        }
    }

    if matched_lines.is_empty() {
        return Ok(Vec::new());
    }

    if allow_standalone && is_standalone_deletable_file(rel) && non_header_unmatched == 0 {
        return Ok(vec![FileCleanupTarget::DeleteStandaloneFile {
            relative_path: rel.to_string(),
            file_sha256: file_sha,
        }]);
    }

    Ok(matched_lines
        .into_iter()
        .map(|(line_number, line_sha256)| FileCleanupTarget::RemoveLine {
            relative_path: rel.to_string(),
            line_number,
            line_sha256,
        })
        .collect())
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |r| r.get(0),
        )
        .map_err(|e| GmError::Platform(e.to_string()))?;
    Ok(count > 0)
}

struct SqliteScanMatches {
    queue_rows: Vec<(QueueTable, String)>,
    routine_rows: Vec<(RoutineTable, String)>,
}

fn scan_sqlite_targets(db_path: &Path, needle_lower: &str) -> Result<SqliteScanMatches> {
    if !db_path.exists() {
        return Ok(SqliteScanMatches {
            queue_rows: Vec::new(),
            routine_rows: Vec::new(),
        });
    }
    let conn = Connection::open(db_path)
        .map_err(|e| GmError::Platform(format!("Failed to open '{}': {e}", db_path.display())))?;
    let mut queue_rows = Vec::new();
    let mut matched_task_ids = HashSet::new();

    if table_exists(&conn, "bot_tasks")? {
        let mut stmt = conn
            .prepare(
                "SELECT id, instruction, COALESCE(blocker_note,''), COALESCE(human_resume_note,''), COALESCE(result_summary,'')
                 FROM bot_tasks",
            )
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    format!(
                        "{} {} {} {}",
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?
                    ),
                ))
            })
            .map_err(|e| GmError::Platform(e.to_string()))?;
        for row in rows {
            let (id, text) = row.map_err(|e| GmError::Platform(e.to_string()))?;
            if contains_ignore_ascii_case(&text, needle_lower) {
                matched_task_ids.insert(id.clone());
                queue_rows.push((QueueTable::BotTasks, id));
            }
        }
    }

    if table_exists(&conn, "task_attempts")? {
        let mut stmt = conn
            .prepare("SELECT id, task_id, COALESCE(checkpoint,'') FROM task_attempts")
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| GmError::Platform(e.to_string()))?;
        for row in rows {
            let (id, task_id, checkpoint) = row.map_err(|e| GmError::Platform(e.to_string()))?;
            if matched_task_ids.contains(&task_id)
                || contains_ignore_ascii_case(&checkpoint, needle_lower)
            {
                queue_rows.push((QueueTable::TaskAttempts, id));
            }
        }
    }

    if table_exists(&conn, "runtime_events")? {
        let mut stmt = conn
            .prepare("SELECT sequence, event_json FROM runtime_events")
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| GmError::Platform(e.to_string()))?;
        for row in rows {
            let (seq, json) = row.map_err(|e| GmError::Platform(e.to_string()))?;
            if contains_ignore_ascii_case(&json, needle_lower) {
                queue_rows.push((QueueTable::RuntimeEvents, seq.to_string()));
            }
        }
    }

    if table_exists(&conn, "bridge_deliveries")? {
        let mut stmt = conn
            .prepare("SELECT delivery_id FROM bridge_deliveries")
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| GmError::Platform(e.to_string()))?;
        for row in rows {
            let delivery_id = row.map_err(|e| GmError::Platform(e.to_string()))?;
            if contains_ignore_ascii_case(&delivery_id, needle_lower) {
                queue_rows.push((QueueTable::BridgeDeliveries, delivery_id));
            }
        }
    }

    let mut routine_rows = Vec::new();
    let mut matched_routine_ids = HashSet::new();
    if table_exists(&conn, "routines_v1")? {
        let mut stmt = conn
            .prepare("SELECT id, config_json FROM routines_v1")
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| GmError::Platform(e.to_string()))?;
        for row in rows {
            let (id, config_json) = row.map_err(|e| GmError::Platform(e.to_string()))?;
            if contains_ignore_ascii_case(&id, needle_lower)
                || contains_ignore_ascii_case(&config_json, needle_lower)
            {
                matched_routine_ids.insert(id.clone());
                routine_rows.push((RoutineTable::RoutinesV1, id));
            }
        }
    }

    if table_exists(&conn, "routine_runs_v1")? {
        let mut stmt = conn
            .prepare("SELECT id, routine_id, COALESCE(task_id,''), status FROM routine_runs_v1")
            .map_err(|e| GmError::Platform(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| GmError::Platform(e.to_string()))?;
        for row in rows {
            let (id, routine_id, task_id, status) =
                row.map_err(|e| GmError::Platform(e.to_string()))?;
            if matched_routine_ids.contains(&routine_id)
                || matched_task_ids.contains(&task_id)
                || contains_ignore_ascii_case(&status, needle_lower)
            {
                routine_rows.push((RoutineTable::RoutineRunsV1, id));
            }
        }
    }

    Ok(SqliteScanMatches {
        queue_rows,
        routine_rows,
    })
}

impl ForgetPlanner {
    /// Phase 1: Scan Muse & Darius stores discreetly and persist an ID/hash-only
    /// plan to `workspace/memory/forget/pending.json`.
    pub fn plan(request: &str, res: &ForgetResources) -> Result<ForgetPlan> {
        let trimmed = request.trim();
        if trimmed.is_empty() {
            return Err(GmError::Config("Forget request cannot be empty".into()));
        }
        let needle_lower = trimmed.to_lowercase();
        let fingerprint = compute_request_fingerprint(trimmed);
        let plan_id = format!("forget-{}", Uuid::new_v4());

        let mut targets = Vec::new();
        let mut claims_set = BTreeSet::new();
        let mut citations = Vec::new();
        let mut summary_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut matched_record_ids = HashSet::new();

        // 1. Darius PersonalMemoryStore (FTS + full scan for substring/stopword resilience)
        for rec in res.memory.search(trimmed, 200)? {
            matched_record_ids.insert(rec.id);
        }
        for rec in res.memory.all_records(1000)? {
            let hay = format!(
                "{} {} {}",
                rec.title.as_deref().unwrap_or(""),
                rec.body,
                rec.tags.join(" ")
            );
            if contains_ignore_ascii_case(&hay, &needle_lower) {
                matched_record_ids.insert(rec.id);
            }
        }
        for record_id in &matched_record_ids {
            targets.push(ForgetTarget::DariusRecord {
                record_id: record_id.clone(),
            });
            citations.push(format!("memory://record/{record_id}"));
            *summary_counts.entry("memory_records".into()).or_insert(0) += 1;
        }

        // 2. Standing markdown files
        type StandingTargetCtor = fn(usize, String) -> ForgetTarget;
        let standing: [(&str, PathBuf, StandingTargetCtor); 4] = [
            (
                "USER.md",
                res.paths.user_md(),
                |line_number, line_sha256| ForgetTarget::UserMd {
                    line_number,
                    line_sha256,
                },
            ),
            (
                "MEMORY.md",
                res.paths.memory_md(),
                |line_number, line_sha256| ForgetTarget::MemoryMd {
                    line_number,
                    line_sha256,
                },
            ),
            (
                "PROACTIVE_PREFERENCES.md",
                res.paths.proactive_preferences_md(),
                |line_number, line_sha256| ForgetTarget::ProactivePreferencesMd {
                    line_number,
                    line_sha256,
                },
            ),
            (
                "FEEDBACK.md",
                res.paths.feedback_md(),
                |line_number, line_sha256| ForgetTarget::FeedbackMd {
                    line_number,
                    line_sha256,
                },
            ),
        ];

        let mut matched_locators = Vec::new();
        for (rel, abs, ctor) in standing {
            for item in inspect_file_for_matches(&abs, rel, &needle_lower, false)? {
                if let FileCleanupTarget::RemoveLine {
                    relative_path,
                    line_number,
                    line_sha256,
                } = item
                {
                    let loc = format!("{relative_path}#L{line_number}");
                    matched_locators.push(loc.clone());
                    citations.push(loc);
                    targets.push(ctor(line_number, line_sha256));
                    *summary_counts.entry("standing_context".into()).or_insert(0) += 1;
                }
            }
        }

        // 3. Corpus trees: memory/**, dreams/**, goals/** (+workspace/goals/**), side-chats/**, custom-skills/**
        let storage = res.paths.storage_dir();
        type CorpusTargetCtor = fn(FileCleanupTarget) -> ForgetTarget;
        let tree_specs: [(&str, &str, PathBuf, CorpusTargetCtor); 6] = [
            (
                "memory",
                "memory_notes",
                res.paths.memory_dir(),
                ForgetTarget::MemoryCorpus,
            ),
            (
                "dreams",
                "dreams",
                res.paths.dreams_dir(),
                ForgetTarget::DreamsCorpus,
            ),
            (
                "goals",
                "goals",
                res.paths.goals_dir(),
                ForgetTarget::GoalsCorpus,
            ),
            (
                "workspace/goals",
                "goals",
                res.paths.workspace_goals_dir(),
                ForgetTarget::GoalsCorpus,
            ),
            (
                "side-chats",
                "side_chats",
                res.paths.side_chats_dir(),
                ForgetTarget::SideChatsCorpus,
            ),
            (
                "custom-skills",
                "custom_skills",
                res.paths.custom_skills_dir(),
                ForgetTarget::CustomSkillsCorpus,
            ),
        ];

        for (_label, cat_key, root_dir, ctor) in tree_specs {
            let mut files = Vec::new();
            collect_regular_files_no_follow(&root_dir, &mut files)?;
            for abs in files {
                let Ok(rel_os) = abs.strip_prefix(storage) else {
                    continue;
                };
                let rel = rel_os.to_string_lossy().replace('\\', "/");
                if rel == "memory/provenance.sqlite"
                    || rel.starts_with("memory/provenance.sqlite-")
                    || rel == "memory/people/INDEX.md"
                    || rel == "memory/groups/INDEX.md"
                {
                    continue;
                }
                if abs
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e == "sqlite" || e == "db")
                {
                    continue;
                }
                let allow_standalone = true;
                for item in inspect_file_for_matches(&abs, &rel, &needle_lower, allow_standalone)? {
                    match &item {
                        FileCleanupTarget::RemoveLine {
                            relative_path,
                            line_number,
                            ..
                        } => {
                            let loc = format!("{relative_path}#L{line_number}");
                            matched_locators.push(loc.clone());
                            citations.push(loc);
                        }
                        FileCleanupTarget::DeleteStandaloneFile { relative_path, .. } => {
                            matched_locators.push(relative_path.clone());
                            citations.push(relative_path.clone());
                        }
                        FileCleanupTarget::RedactGoalCatalog {
                            relative_path,
                            goal_ids,
                            ..
                        } => {
                            let loc = format!("{relative_path}#{}", goal_ids.join(","));
                            matched_locators.push(loc.clone());
                            citations.push(loc);
                        }
                    }
                    targets.push(ctor(item));
                    *summary_counts.entry(cat_key.to_string()).or_insert(0) += 1;
                }
            }
        }

        // 4. Provenance claims matching Darius record IDs, locators, or active quote hashes
        let quote_sha = sha256_hex(trimmed);
        for claim in res.provenance.list_claims(None, 5000)? {
            if claim.status == ClaimStatus::Retracted {
                continue;
            }
            let by_record = matched_record_ids.contains(&claim.record_id);
            let by_quote = claim.quote_sha256 == quote_sha;
            let by_locator = matched_locators.iter().any(|l| {
                claim.source_locator == *l
                    || claim.source_locator.contains(l)
                    || l.starts_with(&claim.source_locator)
            });
            let by_query_loc = contains_ignore_ascii_case(&claim.source_locator, &needle_lower);
            if (by_record || by_quote || by_locator || by_query_loc)
                && claims_set.insert(claim.claim_id.clone())
            {
                targets.push(ForgetTarget::ProvenanceClaim {
                    claim_id: claim.claim_id,
                    record_id: claim.record_id,
                    source_locator: claim.source_locator,
                });
                *summary_counts
                    .entry("provenance_claims".into())
                    .or_insert(0) += 1;
            }
        }

        // 5. OpenBot queue + routines SQLite tables
        if let Some(ref openbot_db) = res.openbot_db_path {
            let matches = scan_sqlite_targets(openbot_db, &needle_lower)?;
            for (table, row_id) in matches.queue_rows {
                citations.push(format!("openbot://{}/{row_id}", table.as_str()));
                targets.push(ForgetTarget::QueueRow { table, row_id });
                *summary_counts.entry("queued_work".into()).or_insert(0) += 1;
            }
            for (table, row_id) in matches.routine_rows {
                citations.push(format!("openbot://{}/{row_id}", table.as_str()));
                targets.push(ForgetTarget::RoutineRow { table, row_id });
                *summary_counts.entry("routines".into()).or_insert(0) += 1;
            }
        }

        // 6. Governance audit log redaction candidates (when forget_redact_audit = true)
        if res.forget_redact_audit
            && let Some(ref audit_path) = res.audit_log_path
            && let Some((content, _)) =
                SharedFileStore::global().read_text_with_sha256(audit_path)?
        {
            for (idx, line) in content.lines().enumerate() {
                if contains_ignore_ascii_case(line, &needle_lower) {
                    let line_number = idx + 1;
                    citations.push(format!("audit.jsonl#L{line_number}"));
                    targets.push(ForgetTarget::AuditLogLine {
                        line_number,
                        line_sha256: sha256_hex(line),
                    });
                    *summary_counts.entry("audit_entries".into()).or_insert(0) += 1;
                }
            }
        }

        // 7. Always schedule Feed signal reset fence per upstream inventory section 4
        targets.push(ForgetTarget::FeedSignals);
        *summary_counts.entry("feed_signals".into()).or_insert(0) += 1;

        let plan = ForgetPlan {
            plan_id,
            request_fingerprint: fingerprint,
            claims: claims_set.into_iter().collect(),
            citations,
            targets,
            summary_counts,
            excluded_disclosures: default_excluded_disclosures(),
            feed_reset_required: true,
            redact_audit: res.forget_redact_audit,
            created_at: Utc::now().to_rfc3339(),
        };

        let serialized = serde_json::to_string_pretty(&plan)?;
        let pending_path = res.paths.forget_pending_path();
        SharedFileStore::global().write_exact_sync(&pending_path, None, &serialized)?;

        Ok(plan)
    }
}

fn validate_targeted_file<'a>(
    target: &'a ForgetTarget,
    paths: &MuseRuntimePaths,
) -> Option<(PathBuf, usize, &'a str, bool)> {
    let storage = paths.storage_dir();
    match target {
        ForgetTarget::UserMd {
            line_number,
            line_sha256,
        } => Some((paths.user_md(), *line_number, line_sha256.as_str(), false)),
        ForgetTarget::MemoryMd {
            line_number,
            line_sha256,
        } => Some((paths.memory_md(), *line_number, line_sha256.as_str(), false)),
        ForgetTarget::ProactivePreferencesMd {
            line_number,
            line_sha256,
        } => Some((
            paths.proactive_preferences_md(),
            *line_number,
            line_sha256.as_str(),
            false,
        )),
        ForgetTarget::FeedbackMd {
            line_number,
            line_sha256,
        } => Some((
            paths.feedback_md(),
            *line_number,
            line_sha256.as_str(),
            false,
        )),
        ForgetTarget::MemoryCorpus(item)
        | ForgetTarget::DreamsCorpus(item)
        | ForgetTarget::GoalsCorpus(item)
        | ForgetTarget::SideChatsCorpus(item)
        | ForgetTarget::CustomSkillsCorpus(item) => match item {
            FileCleanupTarget::RemoveLine {
                relative_path,
                line_number,
                line_sha256,
            } => Some((
                storage.join(relative_path),
                *line_number,
                line_sha256.as_str(),
                false,
            )),
            FileCleanupTarget::DeleteStandaloneFile {
                relative_path,
                file_sha256,
            } => Some((storage.join(relative_path), 0, file_sha256.as_str(), true)),
            FileCleanupTarget::RedactGoalCatalog { .. } => None,
        },
        _ => None,
    }
}

/// Rebuild `memory/people/INDEX.md` and `memory/groups/INDEX.md` cleanly from remaining files.
pub fn rebuild_relationship_indexes(paths: &MuseRuntimePaths) -> Result<()> {
    for (title, dir, index_path) in [
        ("People Index", paths.people_dir(), paths.people_index_md()),
        ("Groups Index", paths.groups_dir(), paths.groups_index_md()),
    ] {
        if !dir.exists() {
            continue;
        }
        let mut files = Vec::new();
        collect_regular_files_no_follow(&dir, &mut files)?;
        let mut entries = Vec::new();
        for abs in files {
            if abs == index_path {
                continue;
            }
            if abs.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Ok(rel) = abs.strip_prefix(&dir) else {
                continue;
            };
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let label = SharedFileStore::global()
                .read_text_with_sha256(&abs)?
                .and_then(|(text, _)| {
                    text.lines()
                        .find(|l| l.trim().starts_with('#'))
                        .map(|h| h.trim().trim_start_matches('#').trim().to_string())
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    abs.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or(&rel_str)
                        .to_string()
                });
            entries.push(format!("- [{label}]({rel_str})"));
        }
        let mut doc = format!("# {title}\n\n");
        if entries.is_empty() {
            doc.push_str("_No entries recorded._\n");
        } else {
            doc.push_str(&entries.join("\n"));
            doc.push('\n');
        }
        SharedFileStore::global().write_exact_sync(&index_path, None, &doc)?;
    }
    Ok(())
}

fn redact_case_insensitive(text: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return text.to_string();
    }
    let pattern = format!("(?i){}", regex::escape(needle));
    if let Ok(re) = regex::Regex::new(&pattern) {
        re.replace_all(text, replacement).to_string()
    } else {
        text.to_string()
    }
}

fn run_wal_checkpoint_and_vacuum_file(db_path: &Path) -> Result<()> {
    if !db_path.exists() {
        return Ok(());
    }
    let conn = Connection::open(db_path).map_err(|e| {
        GmError::Platform(format!(
            "Failed to open '{}' for VACUUM: {e}",
            db_path.display()
        ))
    })?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| GmError::Platform(e.to_string()))?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")
        .map_err(|e| {
            GmError::Platform(format!(
                "Failed to checkpoint/VACUUM '{}': {e}",
                db_path.display()
            ))
        })?;
    Ok(())
}

impl ForgetExecutor {
    /// Phase 2: Execute a confirmed Forget plan strictly in the 10-step safety order.
    ///
    /// *Step 1*: Requires `approved == true` (validated `ActionTier::Irreversible` approval).
    /// *verification_query*: Optional live query string from memory (never persisted on disk) used in
    /// audit redaction and Step 10 re-search confirmation.
    pub fn confirm(
        plan: &ForgetPlan,
        res: &ForgetResources,
        approved: bool,
        verification_query: Option<&str>,
    ) -> Result<ForgetReport> {
        // Step 1: Require explicit Irreversible confirmation
        if !approved {
            return Err(GmError::SafetyViolation(
                "RULE-FORGET-001: Forget execution requires Irreversible user confirmation".into(),
            ));
        }

        let storage = res.paths.storage_dir();

        // Validate every file path BEFORE touching anything
        for target in &plan.targets {
            match target {
                ForgetTarget::MemoryCorpus(item)
                | ForgetTarget::DreamsCorpus(item)
                | ForgetTarget::GoalsCorpus(item)
                | ForgetTarget::SideChatsCorpus(item)
                | ForgetTarget::CustomSkillsCorpus(item) => {
                    let rel = match item {
                        FileCleanupTarget::RemoveLine { relative_path, .. } => relative_path,
                        FileCleanupTarget::DeleteStandaloneFile { relative_path, .. } => {
                            relative_path
                        }
                        FileCleanupTarget::RedactGoalCatalog { relative_path, .. } => relative_path,
                    };
                    if is_unsafe_relative_path(rel)
                        || rel.starts_with("self_improvement/migrations")
                        || rel.starts_with("skills/")
                        || rel.starts_with("docs/")
                    {
                        return Err(GmError::SafetyViolation(format!(
                            "RULE-PATH-002: Refused unsafe Forget target path '{rel}'"
                        )));
                    }
                }
                _ => {}
            }
        }

        // Step 2: Re-check every line hash, standalone file hash, and structured JSON catalog hash
        let mut stale_files = Vec::new();
        for target in &plan.targets {
            if let ForgetTarget::GoalsCorpus(FileCleanupTarget::RedactGoalCatalog {
                relative_path,
                file_sha256,
                ..
            }) = target
            {
                let abs = storage.join(relative_path);
                let Ok(Some((_, actual_sha))) =
                    SharedFileStore::global().read_text_with_sha256(&abs)
                else {
                    stale_files.push(relative_path.clone());
                    continue;
                };
                if actual_sha != *file_sha256 {
                    stale_files.push(relative_path.clone());
                }
                continue;
            }
            if let Some((abs, line_no, expected_sha, is_standalone)) =
                validate_targeted_file(target, &res.paths)
            {
                let rel_display = abs
                    .strip_prefix(storage)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| abs.display().to_string());
                if is_standalone {
                    let Ok(Some(_)) = SharedFileStore::global().read_text_with_sha256(&abs) else {
                        stale_files.push(rel_display);
                        continue;
                    };
                    let bytes = std::fs::read(&abs)?;
                    let actual_sha = SharedFileStore::sha256_hex(&bytes);
                    if actual_sha != expected_sha {
                        stale_files.push(rel_display);
                    }
                } else {
                    let Ok(Some((text, _))) = SharedFileStore::global().read_text_with_sha256(&abs)
                    else {
                        stale_files.push(format!("{rel_display}#L{line_no}"));
                        continue;
                    };
                    let Some(line) = text.lines().nth(line_no.saturating_sub(1)) else {
                        stale_files.push(format!("{rel_display}#L{line_no}"));
                        continue;
                    };
                    if sha256_hex(line) != expected_sha {
                        stale_files.push(format!("{rel_display}#L{line_no}"));
                    }
                }
            }
        }

        if !stale_files.is_empty() {
            return Ok(ForgetReport {
                plan_id: plan.plan_id.clone(),
                request_fingerprint: plan.request_fingerprint.clone(),
                status: ForgetStatus::IncompleteFilesChanged,
                message: "incomplete: files changed since plan".into(),
                removed_lines: 0,
                deleted_files: 0,
                deleted_darius_records: 0,
                retracted_claims: Vec::new(),
                deleted_queue_rows: 0,
                deleted_routine_rows: 0,
                redacted_audit_entries: 0,
                feed_reset_executed: false,
                remaining: stale_files,
                excluded_disclosures: plan.excluded_disclosures.clone(),
            });
        }

        // Step 3: Remove exact lines from markdown files while preserving unrelated content
        let mut line_edits: BTreeMap<PathBuf, Vec<(usize, String)>> = BTreeMap::new();
        for target in &plan.targets {
            if let Some((abs, line_no, expected_sha, false)) =
                validate_targeted_file(target, &res.paths)
            {
                line_edits
                    .entry(abs)
                    .or_default()
                    .push((line_no, expected_sha.to_string()));
            }
        }

        let mut removed_lines = 0usize;
        for (abs, removals) in line_edits {
            let expected_map: BTreeMap<usize, String> = removals.into_iter().collect();
            let removal_count = expected_map.len();
            SharedFileStore::global().update_sync(&abs, None, move |existing| {
                let ended_with_newline = existing.ends_with('\n');
                let mut kept = Vec::new();
                for (idx, line) in existing.lines().enumerate() {
                    let line_no = idx + 1;
                    if let Some(expected_sha) = expected_map.get(&line_no) {
                        if sha256_hex(line) != *expected_sha {
                            return Err(GmError::Config(format!(
                                "Line hash changed concurrently at line {line_no}"
                            )));
                        }
                    } else {
                        kept.push(line);
                    }
                }
                let mut out = kept.join("\n");
                if ended_with_newline && !out.is_empty() {
                    out.push('\n');
                }
                Ok(out)
            })?;
            removed_lines += removal_count;
        }

        // Step 3b: JSON-aware redaction for goals/goals.json (never line-delete structured JSON)
        for target in &plan.targets {
            if let ForgetTarget::GoalsCorpus(FileCleanupTarget::RedactGoalCatalog {
                relative_path,
                file_sha256,
                goal_ids,
            }) = target
            {
                let needle = verification_query
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        GmError::Config(
                            "Goal catalog redaction requires a live verification query".into(),
                        )
                    })?
                    .to_string();
                let abs = storage.join(relative_path);
                let ids = goal_ids.clone();
                let outcome = SharedFileStore::global().update_sync(
                    &abs,
                    Some(file_sha256),
                    move |existing| {
                        let mut goals = gm_core::goal::Goal::list_from_json_normalized(existing)
                            .map_err(|e| {
                                GmError::Config(format!(
                                    "Corrupt goals catalog during confirm: {e}"
                                ))
                            })?;
                        let expected_count = goals.len();
                        for goal in &mut goals {
                            if ids.contains(&goal.id) {
                                let next_title =
                                    redact_case_insensitive(&goal.title, &needle, "[forgotten]");
                                goal.title = if next_title.trim().is_empty() {
                                    "[forgotten]".into()
                                } else {
                                    next_title
                                };
                                goal.description = redact_case_insensitive(
                                    &goal.description,
                                    &needle,
                                    "[forgotten]",
                                );
                                goal.stop_conditions = goal
                                    .stop_conditions
                                    .iter()
                                    .map(|sc| redact_case_insensitive(sc, &needle, "[forgotten]"))
                                    .collect();
                                goal.category = goal
                                    .category
                                    .as_deref()
                                    .map(|c| redact_case_insensitive(c, &needle, "[forgotten]"));
                                goal.pause_notes = goal
                                    .pause_notes
                                    .as_deref()
                                    .map(|p| redact_case_insensitive(p, &needle, "[forgotten]"));
                            }
                        }
                        let serialized = serde_json::to_string_pretty(&goals)?;
                        let validated = gm_core::goal::Goal::list_from_json_normalized(&serialized)
                            .map_err(|e| {
                                GmError::Config(format!(
                                    "Goal catalog validation failed after redaction: {e}"
                                ))
                            })?;
                        if validated.len() != expected_count {
                            return Err(GmError::Config(
                                "Goal count mismatch after catalog redaction".into(),
                            ));
                        }
                        Ok(serialized)
                    },
                )?;
                if let gm_muse::shared_files::SharedUpdateOutcome::Conflict { .. } = outcome {
                    return Err(GmError::Config(
                        "Goals catalog changed concurrently during Forget confirmation".into(),
                    ));
                }
                removed_lines += goal_ids.len();
            }
        }

        // Step 4: Delete exact standalone files only (never directories or globs)
        let mut deleted_files = 0usize;
        for target in &plan.targets {
            if let Some((abs, _, expected_sha, true)) = validate_targeted_file(target, &res.paths)
                && SharedFileStore::global().remove_file_sync(&abs, Some(expected_sha))?
            {
                deleted_files += 1;
            }
        }

        // Step 5: SQL deletes & claim retractions in one transaction per DB
        let mut deleted_darius_records = 0usize;
        let mut darius_ids = Vec::new();
        for target in &plan.targets {
            if let ForgetTarget::DariusRecord { record_id } = target {
                darius_ids.push(record_id.clone());
                if res.memory.delete_record(record_id)? {
                    deleted_darius_records += 1;
                }
            }
        }

        let mut claim_seeds: Vec<String> = plan.claims.clone();
        for target in &plan.targets {
            if let ForgetTarget::ProvenanceClaim { claim_id, .. } = target
                && !claim_seeds.contains(claim_id)
            {
                claim_seeds.push(claim_id.clone());
            }
        }
        let mut retracted_claims = res.provenance.retract_claims(&claim_seeds)?;
        let extra_retracted = res
            .provenance
            .retract_by_record_or_locator(&darius_ids, &plan.citations)?;
        for c in extra_retracted {
            if !retracted_claims.contains(&c) {
                retracted_claims.push(c);
            }
        }

        let mut deleted_queue_rows = 0usize;
        let mut deleted_routine_rows = 0usize;
        if let Some(ref openbot_db) = res.openbot_db_path
            && openbot_db.exists()
        {
            let mut conn =
                Connection::open(openbot_db).map_err(|e| GmError::Platform(e.to_string()))?;
            conn.busy_timeout(std::time::Duration::from_secs(5))
                .map_err(|e| GmError::Platform(e.to_string()))?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| GmError::Platform(e.to_string()))?;

            for target in &plan.targets {
                match target {
                    ForgetTarget::QueueRow { table, row_id } => {
                        if !table_exists(&tx, table.as_str())? {
                            continue;
                        }
                        let changed = match table {
                            QueueTable::BotTasks => tx
                                .execute("DELETE FROM bot_tasks WHERE id = ?1", [row_id])
                                .map_err(|e| GmError::Platform(e.to_string()))?,
                            QueueTable::TaskAttempts => tx
                                .execute("DELETE FROM task_attempts WHERE id = ?1", [row_id])
                                .map_err(|e| GmError::Platform(e.to_string()))?,
                            QueueTable::RuntimeEvents => {
                                let seq: i64 = row_id.parse().map_err(|_| {
                                    GmError::Config(format!(
                                        "Invalid runtime_events seq '{row_id}'"
                                    ))
                                })?;
                                tx.execute("DELETE FROM runtime_events WHERE sequence = ?1", [seq])
                                    .map_err(|e| GmError::Platform(e.to_string()))?
                            }
                            QueueTable::BridgeDeliveries => tx
                                .execute(
                                    "DELETE FROM bridge_deliveries WHERE delivery_id = ?1",
                                    [row_id],
                                )
                                .map_err(|e| GmError::Platform(e.to_string()))?,
                        };
                        deleted_queue_rows += changed;
                    }
                    ForgetTarget::RoutineRow { table, row_id } => {
                        if !table_exists(&tx, table.as_str())? {
                            continue;
                        }
                        let changed = match table {
                            RoutineTable::RoutinesV1 => tx
                                .execute("DELETE FROM routines_v1 WHERE id = ?1", [row_id])
                                .map_err(|e| GmError::Platform(e.to_string()))?,
                            RoutineTable::RoutineRunsV1 => tx
                                .execute("DELETE FROM routine_runs_v1 WHERE id = ?1", [row_id])
                                .map_err(|e| GmError::Platform(e.to_string()))?,
                        };
                        deleted_routine_rows += changed;
                    }
                    _ => {}
                }
            }
            tx.commit().map_err(|e| GmError::Platform(e.to_string()))?;
        }

        // Also clear Feed signals DB tables inside a transaction if present
        if let Some(ref feed_db) = res.feed_db_path
            && feed_db.exists()
        {
            let mut conn =
                Connection::open(feed_db).map_err(|e| GmError::Platform(e.to_string()))?;
            conn.busy_timeout(std::time::Duration::from_secs(5))
                .map_err(|e| GmError::Platform(e.to_string()))?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| GmError::Platform(e.to_string()))?;
            if table_exists(&tx, "feed_signals")? {
                tx.execute("DELETE FROM feed_signals", [])
                    .map_err(|e| GmError::Platform(e.to_string()))?;
            }
            if table_exists(&tx, "feed_label_cache")? {
                tx.execute("DELETE FROM feed_label_cache", [])
                    .map_err(|e| GmError::Platform(e.to_string()))?;
            }
            if table_exists(&tx, "feed_posts_v1")? {
                tx.execute("UPDATE feed_posts_v1 SET signal_citations = '[]'", [])
                    .map_err(|e| GmError::Platform(e.to_string()))?;
            }
            tx.commit().map_err(|e| GmError::Platform(e.to_string()))?;
        }

        // Step 6: OUTSIDE transactions, run WAL checkpoint(TRUNCATE) + VACUUM on touched DBs
        let darius_db_path = res.memory.db_path();
        if darius_db_path != Path::new(":memory:") {
            run_wal_checkpoint_and_vacuum_file(&darius_db_path)?;
        }
        if res.provenance.db_path().is_some() {
            res.provenance.wal_checkpoint_and_vacuum()?;
        }
        if let Some(ref openbot_db) = res.openbot_db_path {
            run_wal_checkpoint_and_vacuum_file(openbot_db)?;
        }
        if let Some(ref feed_db) = res.feed_db_path {
            run_wal_checkpoint_and_vacuum_file(feed_db)?;
        }

        // Step 7: Redact audit log (when enabled) and write discreet governance record
        let mut redacted_audit_entries = 0usize;
        if plan.redact_audit
            && let Some(ref audit_path) = res.audit_log_path
            && audit_path.exists()
        {
            let target_lines: BTreeMap<usize, String> = plan
                .targets
                .iter()
                .filter_map(|t| match t {
                    ForgetTarget::AuditLogLine {
                        line_number,
                        line_sha256,
                    } => Some((*line_number, line_sha256.clone())),
                    _ => None,
                })
                .collect();

            if !target_lines.is_empty() {
                let query_owned = verification_query
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                redacted_audit_entries = target_lines.len();
                SharedFileStore::global().update_sync(audit_path, None, move |existing| {
                    let ended_with_newline = existing.ends_with('\n');
                    let mut out_lines = Vec::new();
                    for (idx, line) in existing.lines().enumerate() {
                        let line_no = idx + 1;
                        if let Some(expected_sha) = target_lines.get(&line_no)
                            && sha256_hex(line) == *expected_sha
                        {
                            let rewritten = if let Some(ref needle) = query_owned {
                                redact_case_insensitive(line, needle, "[forgotten]")
                            } else if let Ok(mut json_val) =
                                serde_json::from_str::<serde_json::Value>(line)
                            {
                                if let Some(obj) = json_val.as_object_mut() {
                                    obj.insert(
                                        "action_summary".into(),
                                        serde_json::json!("[forgotten]"),
                                    );
                                    obj.insert(
                                        "details".into(),
                                        serde_json::json!({"redacted": "[forgotten]"}),
                                    );
                                }
                                serde_json::to_string(&json_val)
                                    .unwrap_or_else(|_| "[forgotten]".into())
                            } else {
                                "[forgotten]".into()
                            };
                            out_lines.push(rewritten);
                        } else {
                            out_lines.push(line.to_string());
                        }
                    }
                    let mut joined = out_lines.join("\n");
                    if ended_with_newline && !joined.is_empty() {
                        joined.push('\n');
                    }
                    Ok(joined)
                })?;
            }
        }

        let summary = format!(
            "Confirmed two-phase Forget plan {} (fingerprint={}): {} lines removed, {} files deleted, {} claims retracted",
            plan.plan_id,
            plan.request_fingerprint,
            removed_lines,
            deleted_files,
            retracted_claims.len()
        );
        let gov_details = serde_json::json!({
            "plan_id": plan.plan_id,
            "request_fingerprint": plan.request_fingerprint,
            "removed_lines": removed_lines,
            "deleted_files": deleted_files,
            "retracted_claims": retracted_claims.len(),
            "redacted_audit_entries": redacted_audit_entries,
        });
        if let Some(ref logger) = res.audit_logger {
            if let Some(needle) = verification_query.map(str::trim).filter(|s| !s.is_empty()) {
                logger.redact_in_memory(needle, "[forgotten]");
            }
            logger.log_governance(
                "muse_forget_confirm",
                None,
                AuditCategory::Permitted,
                "RULE-FORGET-001",
                "Two-phase discretionary Forget execution confirmed",
                summary.clone(),
                gov_details,
            );
        } else if let Some(ref audit_path) = res.audit_log_path {
            let logger = AuditLogger::with_file(audit_path)?;
            logger.log_governance(
                "muse_forget_confirm",
                None,
                AuditCategory::Permitted,
                "RULE-FORGET-001",
                "Two-phase discretionary Forget execution confirmed",
                summary,
                gov_details,
            );
        }

        // Step 8: Run the Feed reset fence
        if let Some(ref fence) = res.feed_reset_fence {
            fence(plan)?;
        }
        let feed_reset_executed = true;

        // Step 9: Rebuild relationship INDEX.md files
        rebuild_relationship_indexes(&res.paths)?;

        // Clear or mark pending.json as completed without ever adding sensitive text
        let pending_path = res.paths.forget_pending_path();
        if pending_path.exists() {
            let cleared = serde_json::json!({
                "claims": [],
                "citations": [],
                "last_completed_plan_id": plan.plan_id,
                "last_fingerprint": plan.request_fingerprint,
            });
            SharedFileStore::global().write_exact_sync(
                &pending_path,
                None,
                &serde_json::to_string_pretty(&cleared)?,
            )?;
        }

        // Step 10: Re-search and verify zero active hits remain
        let mut remaining = Vec::new();
        for cid in &retracted_claims {
            if let Some(claim) = res.provenance.get_claim(cid)?
                && claim.status == ClaimStatus::Active
            {
                remaining.push(format!("provenance:{cid}"));
            }
        }
        for rec in res.memory.all_records(1000)? {
            if darius_ids.contains(&rec.id) {
                remaining.push(format!("darius:{}", rec.id));
            }
        }

        if let Some(q) = verification_query.map(str::trim).filter(|s| !s.is_empty()) {
            let needle_lower = q.to_lowercase();
            for rec in res.memory.search(q, 50)? {
                remaining.push(format!("darius:fts:{}", rec.id));
            }
            for rec in res.memory.all_records(1000)? {
                let hay = format!(
                    "{} {} {}",
                    rec.title.as_deref().unwrap_or(""),
                    rec.body,
                    rec.tags.join(" ")
                );
                if contains_ignore_ascii_case(&hay, &needle_lower) {
                    remaining.push(format!("darius:scan:{}", rec.id));
                }
            }

            for (rel, abs) in [
                ("USER.md", res.paths.user_md()),
                ("MEMORY.md", res.paths.memory_md()),
                (
                    "PROACTIVE_PREFERENCES.md",
                    res.paths.proactive_preferences_md(),
                ),
                ("FEEDBACK.md", res.paths.feedback_md()),
            ] {
                if let Some((text, _)) = SharedFileStore::global().read_text_with_sha256(&abs)? {
                    for (idx, line) in text.lines().enumerate() {
                        if contains_ignore_ascii_case(line, &needle_lower) {
                            remaining.push(format!("{rel}#L{}", idx + 1));
                        }
                    }
                }
            }

            for root in [
                res.paths.memory_dir(),
                res.paths.dreams_dir(),
                res.paths.goals_dir(),
                res.paths.workspace_goals_dir(),
                res.paths.side_chats_dir(),
                res.paths.custom_skills_dir(),
            ] {
                let mut files = Vec::new();
                collect_regular_files_no_follow(&root, &mut files)?;
                for abs in files {
                    let Ok(rel_os) = abs.strip_prefix(storage) else {
                        continue;
                    };
                    let rel = rel_os.to_string_lossy().replace('\\', "/");
                    if rel == "memory/provenance.sqlite"
                        || rel.starts_with("memory/provenance.sqlite-")
                        || !is_supported_corpus_file(&rel, &abs)
                        || abs
                            .extension()
                            .and_then(|e| e.to_str())
                            .is_some_and(|e| e == "sqlite" || e == "db")
                    {
                        continue;
                    }
                    if let Some((text, _)) =
                        SharedFileStore::global().read_text_with_sha256(&abs)?
                    {
                        for (idx, line) in text.lines().enumerate() {
                            if contains_ignore_ascii_case(line, &needle_lower) {
                                remaining.push(format!("{rel}#L{}", idx + 1));
                            }
                        }
                    }
                }
            }

            if let Some(ref openbot_db) = res.openbot_db_path {
                let matches = scan_sqlite_targets(openbot_db, &needle_lower)?;
                for (t, id) in matches.queue_rows {
                    remaining.push(format!("openbot:{}:{id}", t.as_str()));
                }
                for (t, id) in matches.routine_rows {
                    remaining.push(format!("openbot:{}:{id}", t.as_str()));
                }
            }

            if plan.redact_audit
                && let Some(ref audit_path) = res.audit_log_path
                && let Some((text, _)) =
                    SharedFileStore::global().read_text_with_sha256(audit_path)?
            {
                for (idx, line) in text.lines().enumerate() {
                    if contains_ignore_ascii_case(line, &needle_lower) {
                        remaining.push(format!("audit.jsonl#L{}", idx + 1));
                    }
                }
            }
        }

        let (status, message) = if remaining.is_empty() {
            (ForgetStatus::Forgotten, "forgotten".to_string())
        } else {
            (
                ForgetStatus::IncompleteRemainingHits,
                format!("incomplete: {} location(s) remain", remaining.len()),
            )
        };

        Ok(ForgetReport {
            plan_id: plan.plan_id.clone(),
            request_fingerprint: plan.request_fingerprint.clone(),
            status,
            message,
            removed_lines,
            deleted_files,
            deleted_darius_records,
            retracted_claims,
            deleted_queue_rows,
            deleted_routine_rows,
            redacted_audit_entries,
            feed_reset_executed,
            remaining,
            excluded_disclosures: plan.excluded_disclosures.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn two_phase_forget_end_to_end_preserves_mixed_files_migrations_and_chat_rows() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path();
        let paths = MuseRuntimePaths::shared(storage);
        let secret = "zxq_canary_allergy_99";

        // 1. Seed mixed files & standalone files
        SharedFileStore::global()
            .write_exact_sync(
                paths.user_md(),
                None,
                &format!("# USER.md\n- Name: Alex\n- Health: {secret}\n- City: Portland\n"),
            )
            .unwrap();
        SharedFileStore::global()
            .write_exact_sync(
                paths.memory_md(),
                None,
                &format!("# MEMORY.md\n- Likes Rust\n- Avoid {secret}\n"),
            )
            .unwrap();
        SharedFileStore::global()
            .write_exact_sync(
                paths.proactive_preferences_md(),
                None,
                &format!("# Prefs\n- Topic: no {secret} updates\n- Quiet hours: 22-08\n"),
            )
            .unwrap();
        SharedFileStore::global()
            .write_exact_sync(
                paths.feedback_md(),
                None,
                &format!("# Feedback\n- Never mention {secret}\n- Keep concise\n"),
            )
            .unwrap();

        let person_file = paths.people_dir().join("doctor.md");
        SharedFileStore::global()
            .write_exact_sync(&person_file, None, &format!("# Dr Kim\n- Note: {secret}\n"))
            .unwrap();
        let keep_person = paths.people_dir().join("sam.md");
        SharedFileStore::global()
            .write_exact_sync(&keep_person, None, "# Sam\n- Friend\n")
            .unwrap();
        let dream_file = paths.dreams_daily_dir().join("2026-04-01.md");
        SharedFileStore::global()
            .write_exact_sync(&dream_file, None, &format!("- Reflected on {secret}\n"))
            .unwrap();

        // Excluded platform migration artifact that contains similar token for safety check
        let mig_file = paths
            .self_improvement_migrations_dir()
            .join("copy_first_objective_workspace_v1.json");
        SharedFileStore::global()
            .write_exact_sync(&mig_file, None, &format!(r#"{{"note":"{secret}"}}"#))
            .unwrap();

        // 2. Seed Darius PersonalMemoryStore + ProvenanceStore with supersession chain
        let memory_db = storage.join("memory.sqlite");
        let memory = PersonalMemoryStore::open(&memory_db).unwrap();
        let rec = memory
            .record_fact(
                Some("Health Note"),
                &format!("User has {secret}"),
                &["health"],
            )
            .unwrap();
        let provenance = ProvenanceStore::open(paths.provenance_db_path()).unwrap();
        provenance
            .record_claim(
                "c-v1",
                &rec.id,
                "older fact",
                Some("chat-1"),
                "MEMORY.md#L3",
            )
            .unwrap();
        provenance
            .supersede_claim(
                "c-v1",
                "c-v2",
                &rec.id,
                &format!("User has {secret}"),
                Some("chat-1"),
                "MEMORY.md#L3",
            )
            .unwrap();

        // 3. Seed openbot.sqlite queue/routines + excluded channels.sqlite
        let openbot_db = storage.join("openbot.sqlite");
        {
            let conn = Connection::open(&openbot_db).unwrap();
            conn.execute_batch(
                "CREATE TABLE bot_tasks (id TEXT PRIMARY KEY, instruction TEXT NOT NULL, blocker_note TEXT, human_resume_note TEXT, result_summary TEXT);
                 CREATE TABLE task_attempts (id TEXT PRIMARY KEY, task_id TEXT NOT NULL, checkpoint TEXT);
                 CREATE TABLE runtime_events (sequence INTEGER PRIMARY KEY AUTOINCREMENT, event_json TEXT NOT NULL);
                 CREATE TABLE bridge_deliveries (delivery_id TEXT PRIMARY KEY, received_at INTEGER NOT NULL);
                 CREATE TABLE routines_v1 (id TEXT PRIMARY KEY, config_json TEXT NOT NULL, next_due TEXT NOT NULL, last_task_id TEXT);
                 CREATE TABLE routine_runs_v1 (id TEXT PRIMARY KEY, routine_id TEXT NOT NULL, task_id TEXT, scheduled_for TEXT NOT NULL, fired_at TEXT NOT NULL, status TEXT NOT NULL);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO bot_tasks VALUES ('task-1', ?1, NULL, NULL, NULL)",
                [format!("Check {secret}")],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO task_attempts VALUES ('att-1', 'task-1', 'ckpt')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO routines_v1 VALUES ('r-1', ?1, '2026-04-02T00:00:00Z', NULL)",
                [format!(r#"{{"topic":"{secret}"}}"#)],
            )
            .unwrap();
        }
        let channels_db = storage.join("channels.sqlite");
        {
            let conn = Connection::open(&channels_db).unwrap();
            conn.execute_batch(
                "CREATE TABLE channel_messages (id TEXT PRIMARY KEY, content TEXT NOT NULL);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO channel_messages VALUES ('msg-1', ?1)",
                [format!("User originally said {secret}")],
            )
            .unwrap();
        }

        // 4. Seed audit.jsonl entry referencing secret
        let audit_path = storage.join("audit.jsonl");
        let logger = Arc::new(AuditLogger::with_file(&audit_path).unwrap());
        logger.log(
            "memory_note",
            None,
            None,
            None,
            None,
            Some(format!("Logged {secret}")),
            serde_json::json!({ "note": secret }),
        );

        let fence_called = Arc::new(AtomicBool::new(false));
        let fence_flag = fence_called.clone();
        let mut resources = ForgetResources::new(paths.clone(), memory.clone(), provenance.clone());
        resources.audit_logger = Some(logger.clone());
        resources.feed_reset_fence = Some(Arc::new(move |_| {
            fence_flag.store(true, Ordering::SeqCst);
            Ok(())
        }));

        // Phase 1: Plan
        let plan = ForgetPlanner::plan(secret, &resources).unwrap();
        let plan_json = serde_json::to_string(&plan).unwrap();
        assert!(!plan_json.contains(secret), "Plan JSON leaked secret!");
        let pending_disk = std::fs::read_to_string(paths.forget_pending_path()).unwrap();
        assert!(
            !pending_disk.contains(secret),
            "pending.json leaked secret!"
        );
        assert_eq!(plan.request_fingerprint.len(), 16);

        // Unapproved confirm MUST fail
        let denied = ForgetExecutor::confirm(&plan, &resources, false, Some(secret)).unwrap_err();
        assert!(denied.to_string().contains("RULE-FORGET-001"));

        // Phase 2: Approved confirm
        let report = ForgetExecutor::confirm(&plan, &resources, true, Some(secret)).unwrap();
        assert_eq!(report.status, ForgetStatus::Forgotten);
        assert_eq!(report.message, "forgotten");
        assert!(
            report.remaining.is_empty(),
            "Remaining: {:?}",
            report.remaining
        );
        assert!(fence_called.load(Ordering::SeqCst));

        // Mixed-file lines preserved
        let user_after = std::fs::read_to_string(paths.user_md()).unwrap();
        assert!(user_after.contains("- Name: Alex"));
        assert!(user_after.contains("- City: Portland"));
        assert!(!user_after.contains(secret));

        let mem_after = std::fs::read_to_string(paths.memory_md()).unwrap();
        assert!(mem_after.contains("- Likes Rust"));
        assert!(!mem_after.contains(secret));

        // Standalone person & dream files deleted, unrelated person kept + INDEX rebuilt
        assert!(!person_file.exists());
        assert!(!dream_file.exists());
        assert!(keep_person.exists());
        let index_md = std::fs::read_to_string(paths.people_index_md()).unwrap();
        assert!(index_md.contains("Sam"));
        assert!(!index_md.contains("doctor"));

        // Claims v1 & v2 retracted
        assert!(
            provenance
                .list_claims(Some(ClaimStatus::Active), 50)
                .unwrap()
                .is_empty()
        );

        // Darius FTS re-search = 0 hits
        assert!(memory.search(secret, 10).unwrap().is_empty());

        // Audit redacted cleanly without secret
        let audit_after = std::fs::read_to_string(&audit_path).unwrap();
        assert!(!audit_after.contains(secret));
        assert!(audit_after.contains("[forgotten]"));
        assert!(audit_after.contains("muse_forget_confirm"));

        // Excluded stores MUST remain untouched!
        assert!(mig_file.exists());
        assert!(std::fs::read_to_string(&mig_file).unwrap().contains(secret));
        let chat_conn = Connection::open(&channels_db).unwrap();
        let chat_count: i64 = chat_conn
            .query_row("SELECT COUNT(*) FROM channel_messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(chat_count, 1);
    }

    #[test]
    fn confirm_aborts_if_file_changed_since_plan() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MuseRuntimePaths::shared(dir.path());
        SharedFileStore::global()
            .write_exact_sync(paths.memory_md(), None, "# MEMORY.md\n- secret_item\n")
            .unwrap();
        let memory = PersonalMemoryStore::in_memory().unwrap();
        let provenance = ProvenanceStore::in_memory().unwrap();
        let resources = ForgetResources::new(paths.clone(), memory, provenance);

        let plan = ForgetPlanner::plan("secret_item", &resources).unwrap();
        SharedFileStore::global()
            .write_exact_sync(
                paths.memory_md(),
                None,
                "# MEMORY.md\n- secret_item_modified\n",
            )
            .unwrap();

        let report = ForgetExecutor::confirm(&plan, &resources, true, Some("secret_item")).unwrap();
        assert_eq!(report.status, ForgetStatus::IncompleteFilesChanged);
        assert_eq!(report.message, "incomplete: files changed since plan");
    }

    #[test]
    fn forgetting_a_goal_field_leaves_all_other_goals_listable() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MuseRuntimePaths::shared(dir.path());
        std::fs::create_dir_all(paths.goals_dir()).unwrap();
        let secret = "zxq_canary_goal_secret_42";
        let goals_json = serde_json::json!([
            {
                "id": "g-1",
                "title": "Run a marathon",
                "description": "Train 4 days per week",
                "status": "Active",
                "stop_conditions": ["Finish 26.2 miles"],
                "created_at": "2026-11-01T00:00:00Z",
                "next_check_at": "2026-11-02T00:00:00Z",
                "verified_close": false,
                "slug": "run-a-marathon",
                "category": "Fitness",
                "parent_goal_id": null,
                "goal_doc_path": null,
                "track_brief_schedule": null,
                "pause_notes": null
            },
            {
                "id": "g-2",
                "title": "Private medical milestone",
                "description": format!("Discuss {secret} with specialist"),
                "status": "Active",
                "stop_conditions": ["Initial intake", format!("Confirm {secret} cleared")],
                "created_at": "2026-11-01T00:00:00Z",
                "next_check_at": "2026-11-02T00:00:00Z",
                "verified_close": false,
                "slug": "private-medical-milestone",
                "category": "Health",
                "parent_goal_id": "g-1",
                "goal_doc_path": null,
                "track_brief_schedule": null,
                "pause_notes": format!("Paused due to {secret}")
            },
            {
                "id": "g-3",
                "title": "Learn Rust async",
                "description": "Build tokio service",
                "status": "Active",
                "stop_conditions": ["Ship v1"],
                "created_at": "2026-11-01T00:00:00Z",
                "next_check_at": "2026-11-02T00:00:00Z",
                "verified_close": false,
                "slug": "learn-rust-async",
                "category": "Engineering",
                "parent_goal_id": null,
                "goal_doc_path": null,
                "track_brief_schedule": null,
                "pause_notes": null
            }
        ]);
        let catalog_path = paths.goals_dir().join("goals.json");
        SharedFileStore::global()
            .write_exact_sync(
                &catalog_path,
                None,
                &serde_json::to_string_pretty(&goals_json).unwrap(),
            )
            .unwrap();

        // Add an unrelated binary PNG artifact under workspace/goals to ensure binary files are skipped cleanly
        let bin_dir = paths.workspace_goals_dir().join("run-a-marathon/files");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin_path = bin_dir.join("chart.png");
        let bin_bytes: Vec<u8> = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0xff, 0xfe];
        std::fs::write(&bin_path, &bin_bytes).unwrap();

        let memory = PersonalMemoryStore::in_memory().unwrap();
        let provenance = ProvenanceStore::in_memory().unwrap();
        let resources = ForgetResources::new(paths.clone(), memory, provenance);

        let plan = ForgetPlanner::plan(secret, &resources).unwrap();
        let report = ForgetExecutor::confirm(&plan, &resources, true, Some(secret)).unwrap();
        assert_eq!(report.status, ForgetStatus::Forgotten);

        let after = std::fs::read_to_string(&catalog_path).unwrap();
        let parsed = gm_core::goal::Goal::list_from_json_normalized(&after)
            .expect("goals.json must remain valid Goal catalog after forget");
        assert_eq!(parsed.len(), 3, "All 3 goals must remain intact");
        assert_eq!(parsed[1].id, "g-2");
        assert_eq!(parsed[1].slug, "private-medical-milestone");
        assert_eq!(parsed[1].parent_goal_id.as_deref(), Some("g-1"));
        assert!(!after.contains(secret));
        assert_eq!(std::fs::read(&bin_path).unwrap(), bin_bytes);
    }

    #[test]
    fn forget_refuses_corrupt_goals_json_and_unsupported_structured_edits() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MuseRuntimePaths::shared(dir.path());
        std::fs::create_dir_all(paths.goals_dir()).unwrap();
        let catalog_path = paths.goals_dir().join("goals.json");
        let corrupt = "[{\"id\": \"g-1\", \"title\": ";
        SharedFileStore::global()
            .write_exact_sync(&catalog_path, None, corrupt)
            .unwrap();

        let memory = PersonalMemoryStore::in_memory().unwrap();
        let provenance = ProvenanceStore::in_memory().unwrap();
        let resources = ForgetResources::new(paths.clone(), memory.clone(), provenance.clone());

        assert!(ForgetPlanner::plan("g-1", &resources).is_err());
        assert_eq!(std::fs::read_to_string(&catalog_path).unwrap(), corrupt);

        // Valid catalog, but targeting a structural field (`slug`) must refuse
        let goal = gm_core::goal::Goal::new("SecretSlugTitle", "Ordinary description");
        SharedFileStore::global()
            .write_exact_sync(
                &catalog_path,
                None,
                &serde_json::to_string_pretty(&vec![&goal]).unwrap(),
            )
            .unwrap();
        assert!(ForgetPlanner::plan(&goal.slug, &resources).is_err());

        // Arbitrary JSON file under memory/ matching query must refuse line-based editing
        std::fs::create_dir_all(paths.memory_dir()).unwrap();
        let other_json = paths.memory_dir().join("structured.json");
        SharedFileStore::global()
            .write_exact_sync(&other_json, None, r#"{"token":"needle_json_99"}"#)
            .unwrap();
        assert!(ForgetPlanner::plan("needle_json_99", &resources).is_err());
    }

    #[test]
    fn forget_goal_catalog_detects_stale_plan_when_modified_before_confirm() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MuseRuntimePaths::shared(dir.path());
        std::fs::create_dir_all(paths.goals_dir()).unwrap();
        let secret = "stale_goal_secret_77";
        let goal = gm_core::goal::Goal::new("Stale check goal", format!("Contains {secret}"));
        let catalog_path = paths.goals_dir().join("goals.json");
        SharedFileStore::global()
            .write_exact_sync(
                &catalog_path,
                None,
                &serde_json::to_string_pretty(&vec![&goal]).unwrap(),
            )
            .unwrap();

        let memory = PersonalMemoryStore::in_memory().unwrap();
        let provenance = ProvenanceStore::in_memory().unwrap();
        let resources = ForgetResources::new(paths.clone(), memory, provenance);

        let plan = ForgetPlanner::plan(secret, &resources).unwrap();
        let mut modified = goal.clone();
        modified.description = format!("Updated {secret}");
        SharedFileStore::global()
            .write_exact_sync(
                &catalog_path,
                None,
                &serde_json::to_string_pretty(&vec![&modified]).unwrap(),
            )
            .unwrap();

        let report = ForgetExecutor::confirm(&plan, &resources, true, Some(secret)).unwrap();
        assert_eq!(report.status, ForgetStatus::IncompleteFilesChanged);
    }
}
