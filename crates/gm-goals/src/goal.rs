use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoalStatus {
    Active,
    #[deprecated(
        note = "Muse goals use Active, Completed, or Abandoned; Paused normalizes to Active on load"
    )]
    Paused,
    Completed,
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: GoalStatus,
    pub stop_conditions: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub next_check_at: DateTime<Utc>,
    pub verified_close: bool,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub parent_goal_id: Option<String>,
    #[serde(default)]
    pub goal_doc_path: Option<PathBuf>,
    #[serde(default)]
    pub track_brief_schedule: Option<String>,
    #[serde(default)]
    pub pause_notes: Option<String>,
}

pub fn slugify_goal_title(title: &str, fallback_id: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in title.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            slug.push(lower);
            prev_dash = false;
        } else if !prev_dash && !slug.is_empty() {
            slug.push('-');
            prev_dash = true;
        }
        if slug.len() >= 60 {
            break;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        let fallback: String = fallback_id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(16)
            .map(|c| c.to_ascii_lowercase())
            .collect();
        if fallback.is_empty() {
            "goal".to_string()
        } else {
            format!("goal-{fallback}")
        }
    } else {
        slug
    }
}

impl Goal {
    pub fn new(title: impl Into<String>, description: impl Into<String>) -> Self {
        let now = Utc::now();
        let id = Uuid::new_v4().to_string();
        let title_s = title.into();
        let slug = slugify_goal_title(&title_s, &id);
        Self {
            id,
            title: title_s,
            description: description.into(),
            status: GoalStatus::Active,
            stop_conditions: Vec::new(),
            created_at: now,
            next_check_at: now + chrono::Duration::hours(24),
            verified_close: false,
            slug,
            category: None,
            parent_goal_id: None,
            goal_doc_path: None,
            track_brief_schedule: None,
            pause_notes: None,
        }
    }

    pub fn normalize_legacy(&mut self) {
        #[allow(deprecated)]
        if self.status == GoalStatus::Paused {
            self.status = GoalStatus::Active;
            if self.pause_notes.is_none() {
                self.pause_notes = Some("migrated from Paused".into());
            }
        }
        if self.slug.trim().is_empty() {
            self.slug = slugify_goal_title(&self.title, &self.id);
        }
    }

    pub fn from_json_normalized(json: &str) -> std::result::Result<Self, serde_json::Error> {
        let mut goal: Self = serde_json::from_str(json)?;
        goal.normalize_legacy();
        Ok(goal)
    }

    pub fn list_from_json_normalized(
        json: &str,
    ) -> std::result::Result<Vec<Self>, serde_json::Error> {
        let mut goals: Vec<Self> = serde_json::from_str(json)?;
        for goal in &mut goals {
            goal.normalize_legacy();
        }
        Ok(goals)
    }

    pub fn complete(&mut self, verified: bool) {
        self.status = GoalStatus::Completed;
        self.verified_close = verified;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v0_fixture_deserializes_and_normalizes_paused_status() {
        let raw = include_str!("../tests/fixtures/goal_v0.json");
        let goals = Goal::list_from_json_normalized(raw).unwrap();
        assert_eq!(goals.len(), 2);

        let paused_legacy = &goals[0];
        assert_eq!(paused_legacy.status, GoalStatus::Active);
        assert_eq!(
            paused_legacy.pause_notes.as_deref(),
            Some("migrated from Paused")
        );
        assert_eq!(paused_legacy.slug, "run-a-half-marathon");
        assert!(paused_legacy.parent_goal_id.is_none());

        let active_legacy = &goals[1];
        assert_eq!(active_legacy.status, GoalStatus::Active);
        assert_eq!(active_legacy.pause_notes, None);
        assert_eq!(active_legacy.slug, "learn-conversational-japanese");
    }
}
