//! Nightly Dream & Self-Improvement Job (`T4.6`): guarded by `muse.enable_dreams`,
//! synthesizes daily dreams, `ALIGNMENT_SYNTHESIS.md`, relationship pages + rebuilt `INDEX.md`,
//! quiet-moment candidates, and staged `workspace/skill-amendments/` proposals (never modifying `skills/`).

use crate::goal_workspace::GoalWorkspaceManager;
use crate::nightly_review::synthesize_nightly_review;
use chrono::{DateTime, Utc};
use gm_core::config::MuseConfig;
use gm_core::error::Result;
use gm_core::muse_paths::MuseRuntimePaths;
use gm_memory::{PersonalMemoryStore, rebuild_relationship_indexes};
use gm_muse::shared_files::SharedFileStore;
use gm_safety::AuditLogger;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelfImprovementReport {
    pub executed: bool,
    pub date: String,
    pub dream_path: Option<PathBuf>,
    pub alignment_synthesis_path: Option<PathBuf>,
    pub quiet_moment_candidates: Vec<String>,
    pub relationship_files_updated: Vec<PathBuf>,
    pub amendment_proposals: Vec<PathBuf>,
}

pub struct SelfImprovementJob;

fn extract_tag_value(tags: &[String], prefix: &str) -> Option<String> {
    tags.iter().find_map(|t| {
        t.strip_prefix(prefix)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(sanitize_slug_filename)
    })
}

fn sanitize_slug_filename(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            out.push(lower);
        } else if (lower == '-' || lower == '_' || lower == ' ') && !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "entry".to_string()
    } else {
        trimmed
    }
}

impl SelfImprovementJob {
    /// Run nightly dreaming, relationship synthesis, quiet-moment pass, and skill amendment proposals.
    /// Does nothing if `muse.enable_dreams` is `false`. Never modifies bundled `skills/`.
    pub fn run(
        paths: &MuseRuntimePaths,
        muse: &MuseConfig,
        memory: &PersonalMemoryStore,
        audit: &AuditLogger,
        goals: Option<&GoalWorkspaceManager>,
        now: DateTime<Utc>,
    ) -> Result<SelfImprovementReport> {
        let date = now.format("%Y-%m-%d").to_string();
        if !muse.enable_dreams {
            return Ok(SelfImprovementReport {
                executed: false,
                date,
                dream_path: None,
                alignment_synthesis_path: None,
                quiet_moment_candidates: Vec::new(),
                relationship_files_updated: Vec::new(),
                amendment_proposals: Vec::new(),
            });
        }

        let memories = memory.all_records(200)?;
        let entries = audit.entries();
        let review = synthesize_nightly_review(&date, &entries, &memories, None, &[], &[]);

        // 1. Write `dreams/daily/YYYY-MM-DD.md` via SharedFileStore
        let dream_path = paths.dreams_daily_dir().join(format!("{date}.md"));
        let active_goals_note = match goals {
            Some(mgr) => {
                let list = mgr.list_goals().unwrap_or_default();
                format!("Active/tracked goals: {}", list.len())
            }
            None => "Active/tracked goals: unavailable".into(),
        };
        let dream_md = format!(
            "# Daily Dream Reflection — {date}\n\n{active_goals_note}\n\n{}\n",
            review.review_markdown.trim()
        );
        SharedFileStore::global().write_exact_sync(&dream_path, None, &dream_md)?;

        // 2. Write `ALIGNMENT_SYNTHESIS.md`
        let alignment_path = paths.alignment_synthesis_md();
        let pref_count = memories
            .iter()
            .filter(|m| matches!(m.kind, gm_memory::RecordKind::Preference))
            .count();
        let alignment_md = format!(
            "# ALIGNMENT_SYNTHESIS.md\n\nUpdated: {}\n- Captured preferences: {}\n- Suggested priorities: {}\n",
            now.to_rfc3339(),
            pref_count,
            review.suggested_priorities.join("; "),
        );
        SharedFileStore::global().write_exact_sync(&alignment_path, None, &alignment_md)?;

        // 3. Relationship synthesis (`memory/people/*.md`, `memory/groups/*.md`, and rebuilt `INDEX.md`)
        let mut relationship_files_updated = Vec::new();
        for rec in &memories {
            if let Some(person_slug) = extract_tag_value(&rec.tags, "person:") {
                let p_path = paths.people_dir().join(format!("{person_slug}.md"));
                let line = format!("- {}", rec.body.trim());
                SharedFileStore::global().update_sync(&p_path, None, move |existing| {
                    let mut out = if existing.trim().is_empty() {
                        format!("# {}\n\n", person_slug)
                    } else {
                        let mut s = existing.to_string();
                        if !s.ends_with('\n') {
                            s.push('\n');
                        }
                        s
                    };
                    if !existing.lines().any(|l| l.trim() == line) {
                        out.push_str(&line);
                        out.push('\n');
                    }
                    Ok(out)
                })?;
                relationship_files_updated.push(p_path);
            }
            if let Some(group_slug) = extract_tag_value(&rec.tags, "group:") {
                let g_path = paths.groups_dir().join(format!("{group_slug}.md"));
                let line = format!("- {}", rec.body.trim());
                SharedFileStore::global().update_sync(&g_path, None, move |existing| {
                    let mut out = if existing.trim().is_empty() {
                        format!("# {}\n\n", group_slug)
                    } else {
                        let mut s = existing.to_string();
                        if !s.ends_with('\n') {
                            s.push('\n');
                        }
                        s
                    };
                    if !existing.lines().any(|l| l.trim() == line) {
                        out.push_str(&line);
                        out.push('\n');
                    }
                    Ok(out)
                })?;
                relationship_files_updated.push(g_path);
            }
        }
        rebuild_relationship_indexes(paths)?;

        // 4. Quiet-moment candidates derived from high-importance or recent observations
        let mut quiet_moment_candidates = Vec::new();
        for rec in memories.iter().take(10) {
            if rec.importance >= 0.75 {
                quiet_moment_candidates.push(format!(
                    "Consolidate insight: {}",
                    rec.title.as_deref().unwrap_or(&rec.body)
                ));
            }
        }
        if !quiet_moment_candidates.is_empty() {
            let qm_path = paths.dreams_dir().join(format!("quiet_moments/{date}.md"));
            let qm_body = format!(
                "# Quiet-Moment Candidates — {date}\n\n{}\n",
                quiet_moment_candidates
                    .iter()
                    .map(|c| format!("- {c}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            SharedFileStore::global().write_exact_sync(&qm_path, None, &qm_body)?;
        }

        // 5. Skill-amendment proposals in `workspace/skill-amendments/` ONLY (never edits `skills/`)
        let mut amendment_proposals = Vec::new();
        let failed_count = entries
            .iter()
            .filter(|e| e.event_type.contains("failed") || e.event_type.contains("refused"))
            .count();
        if failed_count > 0 {
            let prop_path = paths.skill_amendments_dir().join("self-awareness.md");
            let note = format!(
                "- [{date}] Proposed clarification after {failed_count} refused/failed governance events.\n"
            );
            let date_marker = format!("[{date}]");
            SharedFileStore::global().update_sync(&prop_path, None, move |existing| {
                let mut s = if existing.trim().is_empty() {
                    "# Proposed Skill Amendments (self-awareness)\n\n".to_string()
                } else {
                    let mut cur = existing.to_string();
                    if !cur.ends_with('\n') {
                        cur.push('\n');
                    }
                    cur
                };
                if !existing.contains(&date_marker) {
                    s.push_str(&note);
                }
                Ok(s)
            })?;
            amendment_proposals.push(prop_path);
        }

        Ok(SelfImprovementReport {
            executed: true,
            date,
            dream_path: Some(dream_path),
            alignment_synthesis_path: Some(alignment_path),
            quiet_moment_candidates,
            relationship_files_updated,
            amendment_proposals,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_improvement_writes_dreams_relationships_amendments_and_never_edits_skills() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MuseRuntimePaths::shared(dir.path());
        gm_muse::templates::bootstrap_shared_runtime_files(&paths).unwrap();
        let skills_before =
            std::fs::read_to_string(paths.skills_dir().join("self-awareness/SKILL.md")).unwrap();

        let mem = PersonalMemoryStore::in_memory().unwrap();
        mem.record_fact(
            Some("Alice note"),
            "Alice prefers async design reviews",
            &["person:alice", "work"],
        )
        .unwrap();
        let audit = AuditLogger::in_memory();
        audit.log(
            "workspace_write_refused",
            None,
            None,
            None,
            None,
            Some("Refused write".into()),
            serde_json::json!({}),
        );

        let disabled_cfg = MuseConfig {
            enable_dreams: false,
            ..Default::default()
        };
        let skipped =
            SelfImprovementJob::run(&paths, &disabled_cfg, &mem, &audit, None, Utc::now()).unwrap();
        assert!(!skipped.executed);

        let enabled_cfg = MuseConfig {
            enable_dreams: true,
            ..Default::default()
        };
        let report =
            SelfImprovementJob::run(&paths, &enabled_cfg, &mem, &audit, None, Utc::now()).unwrap();
        assert!(report.executed);
        assert!(report.dream_path.as_ref().unwrap().exists());
        assert!(report.alignment_synthesis_path.as_ref().unwrap().exists());
        assert!(paths.people_dir().join("alice.md").exists());
        assert!(
            std::fs::read_to_string(paths.people_index_md())
                .unwrap()
                .contains("alice")
        );
        assert!(
            paths
                .skill_amendments_dir()
                .join("self-awareness.md")
                .exists()
        );
        // Bundled skills MUST remain completely unmodified
        let skills_after =
            std::fs::read_to_string(paths.skills_dir().join("self-awareness/SKILL.md")).unwrap();
        assert_eq!(skills_before, skills_after);
    }
}
