//! Workspace-backed Goal storage (`~/goals/goals.json` + `~/workspace/goals/<slug>/`)
//! supporting 1-level subgoals, deliverable directories, progress logs, and shared locking.

use crate::goal::{Goal, GoalStatus, slugify_goal_title};
use chrono::{DateTime, Utc};
use gm_core::error::{GmError, Result};
use gm_core::muse_paths::MuseRuntimePaths;
use gm_core::storage::ensure_private_dir;
use gm_muse::shared_files::SharedFileStore;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub struct GoalWorkspaceManager {
    paths: MuseRuntimePaths,
}

pub fn validate_goal_slug(slug: &str) -> Result<()> {
    let valid = !slug.is_empty()
        && slug.len() <= 64
        && slug
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !valid {
        return Err(GmError::SafetyViolation(format!(
            "RULE-PATH-002: Invalid goal slug '{slug}'"
        )));
    }
    Ok(())
}

fn validate_rel_subpath(rel: &str) -> Result<PathBuf> {
    if rel.is_empty() || rel.contains('\0') || rel.starts_with('/') || rel.contains('\\') {
        return Err(GmError::SafetyViolation(format!(
            "RULE-PATH-002: Unsafe goal file path '{rel}'"
        )));
    }
    let mut clean = PathBuf::new();
    for comp in Path::new(rel).components() {
        match comp {
            Component::Normal(seg) => clean.push(seg),
            _ => {
                return Err(GmError::SafetyViolation(format!(
                    "RULE-PATH-002: Traversal or root component not allowed in '{rel}'"
                )));
            }
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(GmError::SafetyViolation(
            "RULE-PATH-002: Empty relative path".into(),
        ));
    }
    Ok(clean)
}

impl GoalWorkspaceManager {
    pub fn new(paths: MuseRuntimePaths) -> Self {
        Self { paths }
    }

    pub fn catalog_path(&self) -> PathBuf {
        self.paths.goals_dir().join("goals.json")
    }

    pub fn goal_workspace_dir(&self, slug: &str) -> Result<PathBuf> {
        validate_goal_slug(slug)?;
        Ok(self.paths.workspace_goals_dir().join(slug))
    }

    pub fn goal_doc_path(&self, slug: &str) -> Result<PathBuf> {
        Ok(self.goal_workspace_dir(slug)?.join("GOAL.md"))
    }

    pub fn goal_files_dir(&self, slug: &str) -> Result<PathBuf> {
        Ok(self.goal_workspace_dir(slug)?.join("files"))
    }

    pub fn goal_hidden_dir(&self, slug: &str) -> Result<PathBuf> {
        Ok(self.goal_workspace_dir(slug)?.join("hidden_files"))
    }

    fn ensure_goal_scaffold(&self, goal: &Goal) -> Result<PathBuf> {
        validate_goal_slug(&goal.slug)?;
        let ws = self.goal_workspace_dir(&goal.slug)?;
        let files_dir = ws.join("files");
        let hidden_dir = ws.join("hidden_files");
        ensure_private_dir(&ws)?;
        ensure_private_dir(&files_dir)?;
        ensure_private_dir(&hidden_dir)?;

        let doc_path = ws.join("GOAL.md");
        if !doc_path.exists() {
            let parent_line = goal
                .parent_goal_id
                .as_deref()
                .map(|p| format!("- Parent goal: `{p}`\n"))
                .unwrap_or_default();
            let category_line = goal
                .category
                .as_deref()
                .map(|c| format!("- Category: `{c}`\n"))
                .unwrap_or_default();
            let stops = if goal.stop_conditions.is_empty() {
                "_None specified_\n".to_string()
            } else {
                goal.stop_conditions
                    .iter()
                    .map(|s| format!("- [ ] {s}"))
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n"
            };
            let md = format!(
                "# {title}\n\n- ID: `{id}`\n- Slug: `{slug}`\n{category_line}{parent_line}- Status: `{status:?}`\n\n## Description\n\n{desc}\n\n## Stop Conditions\n\n{stops}\n## Progress Log\n\n",
                title = goal.title,
                id = goal.id,
                slug = goal.slug,
                status = goal.status,
                desc = goal.description,
            );
            SharedFileStore::global().write_exact_sync(&doc_path, None, &md)?;
        }
        Ok(doc_path)
    }

    pub fn list_goals(&self) -> Result<Vec<Goal>> {
        let catalog = self.catalog_path();
        let Some((raw, _)) = SharedFileStore::global().read_text_with_sha256(&catalog)? else {
            return Ok(Vec::new());
        };
        if raw.trim().is_empty() {
            return Ok(Vec::new());
        }
        Goal::list_from_json_normalized(&raw).map_err(GmError::Json)
    }

    fn save_catalog(&self, goals: &[Goal]) -> Result<()> {
        let json = serde_json::to_string_pretty(goals)?;
        SharedFileStore::global().write_exact_sync(self.catalog_path(), None, &json)?;
        Ok(())
    }

    pub fn get_goal(&self, slug_or_id: &str) -> Result<Option<Goal>> {
        Ok(self
            .list_goals()?
            .into_iter()
            .find(|g| g.id == slug_or_id || g.slug == slug_or_id))
    }

    fn next_unique_slug(&self, base_title: &str, id: &str, existing: &[Goal]) -> String {
        let base = slugify_goal_title(base_title, id);
        if !existing.iter().any(|g| g.slug == base) {
            return base;
        }
        for n in 2..=999 {
            let candidate = format!("{base}-{n}");
            if !existing.iter().any(|g| g.slug == candidate) {
                return candidate;
            }
        }
        format!("{base}-{}", &id[..8.min(id.len())])
    }

    pub fn create_goal(
        &self,
        title: &str,
        description: &str,
        category: Option<&str>,
        stop_conditions: Vec<String>,
    ) -> Result<Goal> {
        let trimmed_title = title.trim();
        if trimmed_title.is_empty() {
            return Err(GmError::Config("Goal title cannot be empty".into()));
        }
        let mut goals = self.list_goals()?;
        let mut goal = Goal::new(trimmed_title, description);
        goal.slug = self.next_unique_slug(trimmed_title, &goal.id, &goals);
        goal.category = category
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        goal.stop_conditions = stop_conditions;
        let doc_path = self.ensure_goal_scaffold(&goal)?;
        goal.goal_doc_path = Some(doc_path);
        goals.push(goal.clone());
        self.save_catalog(&goals)?;
        Ok(goal)
    }

    /// Create a child subgoal under an existing top-level goal. Rejects nesting deeper than 1 level.
    pub fn create_subgoal(
        &self,
        parent_slug_or_id: &str,
        title: &str,
        description: &str,
        stop_conditions: Vec<String>,
    ) -> Result<Goal> {
        let trimmed_title = title.trim();
        if trimmed_title.is_empty() {
            return Err(GmError::Config("Subgoal title cannot be empty".into()));
        }
        let mut goals = self.list_goals()?;
        let parent = goals
            .iter()
            .find(|g| g.id == parent_slug_or_id || g.slug == parent_slug_or_id)
            .cloned()
            .ok_or_else(|| {
                GmError::Config(format!("Parent goal '{parent_slug_or_id}' not found"))
            })?;

        if parent.parent_goal_id.is_some() {
            return Err(GmError::Config(
                "Only one subgoal level is allowed: a subgoal cannot have its own subgoal".into(),
            ));
        }

        let mut child = Goal::new(trimmed_title, description);
        let raw_slug = format!(
            "{}-{}",
            parent.slug,
            slugify_goal_title(trimmed_title, &child.id)
        );
        let clipped_slug: String = raw_slug.chars().take(60).collect();
        child.slug = self.next_unique_slug(&clipped_slug, &child.id, &goals);
        child.category = parent.category.clone();
        child.parent_goal_id = Some(parent.id);
        child.stop_conditions = stop_conditions;
        let doc_path = self.ensure_goal_scaffold(&child)?;
        child.goal_doc_path = Some(doc_path);
        goals.push(child.clone());
        self.save_catalog(&goals)?;
        Ok(child)
    }

    pub fn update_goal(
        &self,
        slug_or_id: &str,
        title: Option<&str>,
        description: Option<&str>,
        stop_conditions: Option<Vec<String>>,
        track_brief_schedule: Option<Option<&str>>,
        category: Option<Option<&str>>,
    ) -> Result<Goal> {
        let mut goals = self.list_goals()?;
        let idx = goals
            .iter()
            .position(|g| g.id == slug_or_id || g.slug == slug_or_id)
            .ok_or_else(|| GmError::Config(format!("Goal '{slug_or_id}' not found")))?;
        if let Some(t) = title
            && !t.trim().is_empty()
        {
            goals[idx].title = t.trim().to_string();
        }
        if let Some(d) = description {
            goals[idx].description = d.to_string();
        }
        if let Some(sc) = stop_conditions {
            goals[idx].stop_conditions = sc;
        }
        if let Some(sched) = track_brief_schedule {
            goals[idx].track_brief_schedule = sched.map(str::to_string);
        }
        if let Some(cat) = category {
            goals[idx].category = cat.map(str::to_string);
        }
        goals[idx].normalize_legacy();
        let updated = goals[idx].clone();
        self.save_catalog(&goals)?;
        Ok(updated)
    }

    pub fn complete_goal(&self, slug_or_id: &str, verified: bool) -> Result<Goal> {
        let mut goals = self.list_goals()?;
        let idx = goals
            .iter()
            .position(|g| g.id == slug_or_id || g.slug == slug_or_id)
            .ok_or_else(|| GmError::Config(format!("Goal '{slug_or_id}' not found")))?;
        goals[idx].complete(verified);
        let out = goals[idx].clone();
        self.save_catalog(&goals)?;
        Ok(out)
    }

    pub fn abandon_goal(&self, slug_or_id: &str) -> Result<Goal> {
        let mut goals = self.list_goals()?;
        let idx = goals
            .iter()
            .position(|g| g.id == slug_or_id || g.slug == slug_or_id)
            .ok_or_else(|| GmError::Config(format!("Goal '{slug_or_id}' not found")))?;
        goals[idx].status = GoalStatus::Abandoned;
        let out = goals[idx].clone();
        self.save_catalog(&goals)?;
        Ok(out)
    }

    pub fn delete_goal(&self, slug_or_id: &str) -> Result<Vec<String>> {
        let mut goals = self.list_goals()?;
        let Some(root) = goals
            .iter()
            .find(|g| g.id == slug_or_id || g.slug == slug_or_id)
            .cloned()
        else {
            return Err(GmError::Config(format!("Goal '{slug_or_id}' not found")));
        };
        let mut deleted_slugs = Vec::new();
        goals.retain(|g| {
            let remove = g.id == root.id || g.parent_goal_id.as_deref() == Some(&root.id);
            if remove {
                deleted_slugs.push(g.slug.clone());
            }
            !remove
        });
        self.save_catalog(&goals)?;
        for slug in &deleted_slugs {
            if let Ok(doc) = self.goal_doc_path(slug)
                && doc.exists()
            {
                let _ = SharedFileStore::global().remove_file_sync(&doc, None);
            }
        }
        Ok(deleted_slugs)
    }

    pub fn log_progress(&self, slug_or_id: &str, entry: &str) -> Result<()> {
        let goal = self
            .get_goal(slug_or_id)?
            .ok_or_else(|| GmError::Config(format!("Goal '{slug_or_id}' not found")))?;
        let doc = self.ensure_goal_scaffold(&goal)?;
        let stamped = format!(
            "- [{}] {}",
            Utc::now().format("%Y-%m-%d %H:%M UTC"),
            entry.trim()
        );
        SharedFileStore::global().update_sync(&doc, None, move |existing| {
            let mut s = existing.to_string();
            if !s.ends_with('\n') && !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&stamped);
            s.push('\n');
            Ok(s)
        })?;
        Ok(())
    }

    pub fn due_goals(&self, now: DateTime<Utc>) -> Result<Vec<Goal>> {
        Ok(self
            .list_goals()?
            .into_iter()
            .filter(|g| g.status == GoalStatus::Active && g.next_check_at <= now)
            .collect())
    }

    pub fn mark_checked(&self, slug_or_id: &str, next_check_at: DateTime<Utc>) -> Result<()> {
        let mut goals = self.list_goals()?;
        if let Some(goal) = goals
            .iter_mut()
            .find(|g| g.id == slug_or_id || g.slug == slug_or_id)
        {
            goal.next_check_at = next_check_at;
            self.save_catalog(&goals)?;
        }
        Ok(())
    }

    pub fn write_deliverable(&self, slug: &str, rel_file: &str, content: &str) -> Result<PathBuf> {
        let goal = self
            .get_goal(slug)?
            .ok_or_else(|| GmError::Config(format!("Goal '{slug}' not found")))?;
        let _ = self.ensure_goal_scaffold(&goal)?;
        let clean = validate_rel_subpath(rel_file)?;
        let files_dir = self.goal_files_dir(&goal.slug)?;
        let target = files_dir.join(clean);
        SharedFileStore::global().write_exact_sync(&target, None, content)?;
        Ok(target)
    }

    pub fn list_goal_files(&self, slug: &str) -> Result<Vec<String>> {
        validate_goal_slug(slug)?;
        let files_dir = self.goal_files_dir(slug)?;
        if !files_dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        collect_rel_files_no_follow(&files_dir, &files_dir, &mut out)?;
        out.sort();
        Ok(out)
    }

    pub fn read_goal_file(&self, slug: &str, rel_file: &str) -> Result<String> {
        validate_goal_slug(slug)?;
        let clean = validate_rel_subpath(rel_file)?;
        let target = self.goal_files_dir(slug)?.join(clean);
        SharedFileStore::global()
            .read_text_with_sha256(&target)?
            .map(|(text, _)| text)
            .ok_or_else(|| GmError::Config(format!("Goal deliverable '{rel_file}' not found")))
    }

    pub fn delete_deliverable(&self, slug: &str, rel_file: &str) -> Result<bool> {
        validate_goal_slug(slug)?;
        let clean = validate_rel_subpath(rel_file)?;
        let target = self.goal_files_dir(slug)?.join(clean);
        if !target.exists() {
            return Ok(false);
        }
        SharedFileStore::global().remove_file_sync(&target, None)
    }
}

fn collect_rel_files_no_follow(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return Ok(());
    };
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let Ok(m) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if m.file_type().is_symlink() {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == ".muse.lock" {
            continue;
        }
        if m.is_dir() {
            collect_rel_files_no_follow(root, &path, out)?;
        } else if m.is_file()
            && let Ok(rel) = path.strip_prefix(root)
        {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_workspace_scaffolds_directories_and_enforces_one_level_subgoals() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MuseRuntimePaths::shared(dir.path());
        let mgr = GoalWorkspaceManager::new(paths.clone());

        let parent = mgr
            .create_goal(
                "Learn Rust Async",
                "Build comfort with Tokio streams and select loops.",
                Some("career"),
                vec!["Ship one async service".into()],
            )
            .unwrap();
        assert_eq!(parent.slug, "learn-rust-async");
        assert!(mgr.goal_doc_path(&parent.slug).unwrap().exists());
        assert!(mgr.goal_files_dir(&parent.slug).unwrap().is_dir());
        assert!(mgr.goal_hidden_dir(&parent.slug).unwrap().is_dir());

        let deliverable = mgr
            .write_deliverable(&parent.slug, "study-plan.md", "# Study Plan\nWeek 1\n")
            .unwrap();
        assert!(deliverable.exists());
        assert_eq!(
            mgr.list_goal_files(&parent.slug).unwrap(),
            vec!["study-plan.md".to_string()]
        );

        let sub = mgr
            .create_subgoal(
                &parent.slug,
                "Master Pinning",
                "Read Rust async book chapter on Pin.",
                vec![],
            )
            .unwrap();
        assert_eq!(sub.parent_goal_id.as_deref(), Some(parent.id.as_str()));

        // A subgoal of a subgoal MUST be rejected
        let err = mgr
            .create_subgoal(&sub.slug, "Sub-subgoal", "Should fail", vec![])
            .unwrap_err();
        assert!(err.to_string().contains("one subgoal level"));
    }
}
