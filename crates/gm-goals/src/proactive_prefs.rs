//! Proactive preference, feedback, and dual-timezone persistence (`PROACTIVE_PREFERENCES.md`,
//! `FEEDBACK.md`, `workspace/user/timezones.yaml`) synced with `QuietGate` using targeted
//! prose-preserving writes through `SharedFileStore`.

use crate::quiet_gate::QuietGate;
use chrono_tz::Tz;
use gm_core::error::{GmError, Result};
use gm_core::muse_paths::MuseRuntimePaths;
use gm_muse::shared_files::SharedFileStore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimezonesConfig {
    pub home_tz: String,
    #[serde(default)]
    pub work_tz: Option<String>,
}

impl Default for TimezonesConfig {
    fn default() -> Self {
        Self {
            home_tz: "UTC".into(),
            work_tz: None,
        }
    }
}

impl TimezonesConfig {
    pub fn validate(&self) -> Result<()> {
        Tz::from_str(self.home_tz.trim())
            .map_err(|_| GmError::Config(format!("Invalid home_tz timezone '{}'", self.home_tz)))?;
        if let Some(ref work) = self.work_tz
            && !work.trim().is_empty()
        {
            Tz::from_str(work.trim())
                .map_err(|_| GmError::Config(format!("Invalid work_tz timezone '{}'", work)))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProactivePreferencesSnapshot {
    pub tell_me_about: Vec<String>,
    pub never_tell_me_about: Vec<String>,
    pub quiet_hours: Option<(u32, u32)>,
    pub one_time_dismissals: Vec<String>,
    pub feedback_notes: Vec<String>,
    pub timezones: TimezonesConfig,
}

#[derive(Debug, Clone)]
pub struct ProactivePrefsStore {
    paths: MuseRuntimePaths,
}

fn clean_bullet_item(line: &str) -> Option<String> {
    let t = line.trim();
    if t.is_empty() || t.starts_with('#') || (t.starts_with('_') && t.ends_with('_')) {
        return None;
    }
    let body = t
        .strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .unwrap_or(t)
        .trim();
    if body.is_empty() {
        None
    } else {
        Some(body.to_string())
    }
}

fn parse_quiet_hours_line(line: &str) -> Option<(u32, u32)> {
    let lower = line.to_lowercase();
    let rest = if let Some((_, rhs)) = lower.split_once("quiet hours:") {
        rhs.trim()
    } else if let Some((_, rhs)) = lower.split_once("quiet:") {
        rhs.trim()
    } else {
        return None;
    };
    let (a, b) = rest.split_once('-')?;
    let parse_hr = |s: &str| -> Option<u32> {
        let digits: String = s
            .trim()
            .split(':')
            .next()
            .unwrap_or("")
            .chars()
            .filter(|c| c.is_ascii_digit())
            .collect();
        let h: u32 = digits.parse().ok()?;
        if h < 24 { Some(h) } else { None }
    };
    Some((parse_hr(a)?, parse_hr(b)?))
}

fn upsert_section_bullet(existing: &str, heading: &str, bullet_text: &str, dedup: bool) -> String {
    let target_line = format!("- {bullet_text}");
    let lines: Vec<String> = existing.lines().map(|l| l.to_string()).collect();

    let Some(heading_idx) = lines.iter().position(|l| l.trim() == heading) else {
        let mut out = if existing.trim().is_empty() {
            String::new()
        } else {
            let mut s = existing.to_string();
            if !s.ends_with('\n') {
                s.push('\n');
            }
            s.push('\n');
            s
        };
        out.push_str(heading);
        out.push('\n');
        out.push_str(&target_line);
        out.push('\n');
        return out;
    };

    let section_end = lines[(heading_idx + 1)..]
        .iter()
        .position(|l| l.trim_start().starts_with("## "))
        .map(|rel| heading_idx + 1 + rel)
        .unwrap_or(lines.len());

    if dedup
        && lines[(heading_idx + 1)..section_end]
            .iter()
            .any(|l| l.trim().eq_ignore_ascii_case(&target_line))
    {
        let mut s = lines.join("\n");
        if existing.ends_with('\n') || !s.is_empty() {
            s.push('\n');
        }
        return s;
    }

    let mut insert_at = heading_idx + 1;
    while insert_at < section_end {
        let trimmed = lines[insert_at].trim();
        if trimmed.is_empty() || trimmed.starts_with('_') {
            insert_at += 1;
        } else {
            insert_at += 1;
            break;
        }
    }
    while insert_at < section_end && !lines[insert_at].trim().is_empty() {
        insert_at += 1;
    }

    let mut out_lines = lines;
    out_lines.insert(insert_at, target_line);
    let mut joined = out_lines.join("\n");
    if !joined.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

fn remove_section_bullet(existing: &str, heading: &str, topic: &str) -> String {
    let lines: Vec<&str> = existing.lines().collect();
    let Some(heading_idx) = lines.iter().position(|l| l.trim() == heading) else {
        return existing.to_string();
    };
    let section_end = lines[(heading_idx + 1)..]
        .iter()
        .position(|l| l.trim_start().starts_with("## "))
        .map(|rel| heading_idx + 1 + rel)
        .unwrap_or(lines.len());

    let mut out = Vec::with_capacity(lines.len());
    for (idx, line) in lines.iter().enumerate() {
        if idx > heading_idx
            && idx < section_end
            && let Some(item) = clean_bullet_item(line)
            && item.eq_ignore_ascii_case(topic.trim())
        {
            continue;
        }
        out.push(*line);
    }
    let mut joined = out.join("\n");
    if existing.ends_with('\n') && !joined.is_empty() {
        joined.push('\n');
    }
    joined
}

impl ProactivePrefsStore {
    pub fn new(paths: MuseRuntimePaths) -> Self {
        Self { paths }
    }

    pub fn read_timezones(&self) -> Result<TimezonesConfig> {
        let path = self.paths.timezones_yaml();
        let Some((raw, _)) = SharedFileStore::global().read_text_with_sha256(&path)? else {
            return Ok(TimezonesConfig::default());
        };
        if raw.trim().is_empty() {
            return Ok(TimezonesConfig::default());
        }
        let cfg: TimezonesConfig = serde_yaml_ng::from_str(&raw).map_err(|e| {
            GmError::Config(format!(
                "Invalid timezones YAML at '{}': {e}",
                path.display()
            ))
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn write_timezones(&self, cfg: &TimezonesConfig) -> Result<()> {
        cfg.validate()?;
        let yaml = serde_yaml_ng::to_string(cfg)
            .map_err(|e| GmError::Config(format!("Failed to serialize timezones YAML: {e}")))?;
        SharedFileStore::global().write_exact_sync(self.paths.timezones_yaml(), None, &yaml)?;
        Ok(())
    }

    pub fn load(&self) -> Result<ProactivePreferencesSnapshot> {
        let mut snap = ProactivePreferencesSnapshot {
            timezones: self.read_timezones()?,
            ..Default::default()
        };

        if let Some((prefs_md, _)) = SharedFileStore::global()
            .read_text_with_sha256(self.paths.proactive_preferences_md())?
        {
            let mut section = String::new();
            for line in prefs_md.lines() {
                let t = line.trim();
                if let Some(h) = t.strip_prefix("## ") {
                    section = h.trim().to_lowercase();
                    continue;
                }
                if let Some((s, e)) = parse_quiet_hours_line(t) {
                    snap.quiet_hours = Some((s, e));
                    continue;
                }
                if let Some(item) = clean_bullet_item(t) {
                    match section.as_str() {
                        "tell me about" => snap.tell_me_about.push(item),
                        "never tell me about" => {
                            let stripped = item
                                .strip_prefix("Opt-out:")
                                .or_else(|| item.strip_prefix("opt-out:"))
                                .unwrap_or(&item)
                                .trim()
                                .to_string();
                            if !stripped.is_empty() {
                                snap.never_tell_me_about.push(stripped);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        if let Some((fb_md, _)) =
            SharedFileStore::global().read_text_with_sha256(self.paths.feedback_md())?
        {
            let mut section = String::new();
            for line in fb_md.lines() {
                let t = line.trim();
                if let Some(h) = t.strip_prefix("## ") {
                    section = h.trim().to_lowercase();
                    continue;
                }
                if let Some(item) = clean_bullet_item(t) {
                    if section == "dismissed once" {
                        snap.one_time_dismissals.push(item);
                    } else {
                        snap.feedback_notes.push(item);
                    }
                }
            }
        }

        Ok(snap)
    }

    /// Synchronize parsed permanent opt-outs and quiet hours onto `QuietGate`.
    /// One-time dismissals in `FEEDBACK.md` stay separate and do NOT add topic opt-outs.
    pub fn sync_with_quiet_gate(&self, gate: &QuietGate) -> Result<ProactivePreferencesSnapshot> {
        let snap = self.load()?;
        for topic in &snap.never_tell_me_about {
            gate.opt_out_topic(topic);
        }
        if let Some((start_hr, end_hr)) = snap.quiet_hours {
            gate.set_quiet_hours(start_hr, end_hr);
        }
        Ok(snap)
    }

    pub fn add_tell_me_about(&self, topic: &str) -> Result<()> {
        let clean = topic.trim().to_string();
        if clean.is_empty() {
            return Err(GmError::Config("Topic cannot be empty".into()));
        }
        SharedFileStore::global().update_sync(
            self.paths.proactive_preferences_md(),
            None,
            move |existing| {
                let base = if existing.trim().is_empty() {
                    "# PROACTIVE_PREFERENCES.md\n\n## Tell me about\n\n## Never tell me about\n\n## When\n\n## How\n\n"
                } else {
                    existing
                };
                Ok(upsert_section_bullet(base, "## Tell me about", &clean, true))
            },
        )?;
        Ok(())
    }

    pub fn add_permanent_opt_out(&self, topic: &str, gate: Option<&QuietGate>) -> Result<()> {
        let clean = topic.trim().to_string();
        if clean.is_empty() {
            return Err(GmError::Config("Opt-out topic cannot be empty".into()));
        }
        let clean_clone = clean.clone();
        SharedFileStore::global().update_sync(
            self.paths.proactive_preferences_md(),
            None,
            move |existing| {
                let base = if existing.trim().is_empty() {
                    "# PROACTIVE_PREFERENCES.md\n\n## Tell me about\n\n## Never tell me about\n\n## When\n\n## How\n\n"
                } else {
                    existing
                };
                Ok(upsert_section_bullet(
                    base,
                    "## Never tell me about",
                    &clean_clone,
                    true,
                ))
            },
        )?;
        if let Some(g) = gate {
            g.opt_out_topic(&clean);
        }
        Ok(())
    }

    pub fn remove_permanent_opt_out(&self, topic: &str, gate: Option<&QuietGate>) -> Result<()> {
        let clean = topic.trim().to_string();
        let clean_clone = clean.clone();
        SharedFileStore::global().update_sync(
            self.paths.proactive_preferences_md(),
            None,
            move |existing| {
                Ok(remove_section_bullet(
                    existing,
                    "## Never tell me about",
                    &clean_clone,
                ))
            },
        )?;
        if let Some(g) = gate {
            g.opt_in_topic(&clean);
        }
        Ok(())
    }

    pub fn set_quiet_hours(
        &self,
        start_hour: u32,
        end_hour: u32,
        gate: Option<&QuietGate>,
    ) -> Result<()> {
        if start_hour > 23 || end_hour > 23 {
            return Err(GmError::Config(
                "Quiet hours must be between 0 and 23".into(),
            ));
        }
        let bullet = format!("Quiet hours: {start_hour:02}:00-{end_hour:02}:00");
        SharedFileStore::global().update_sync(
            self.paths.proactive_preferences_md(),
            None,
            move |existing| {
                let base = if existing.trim().is_empty() {
                    "# PROACTIVE_PREFERENCES.md\n\n## Tell me about\n\n## Never tell me about\n\n## When\n\n## How\n\n"
                } else {
                    existing
                };
                // Replace any previous `- Quiet hours: ...` line in place while preserving unrelated prose
                let filtered: Vec<&str> = base
                    .lines()
                    .filter(|l| parse_quiet_hours_line(l).is_none())
                    .collect();
                let mut rebuilt = filtered.join("\n");
                if base.ends_with('\n') && !rebuilt.is_empty() {
                    rebuilt.push('\n');
                }
                Ok(upsert_section_bullet(&rebuilt, "## When", &bullet, false))
            },
        )?;
        if let Some(g) = gate {
            g.set_quiet_hours(start_hour, end_hour);
        }
        Ok(())
    }

    /// Record a one-time dismissal in `FEEDBACK.md` without adding any permanent opt-out.
    pub fn add_one_time_dismissal(&self, item: &str) -> Result<()> {
        let clean = item.trim().to_string();
        if clean.is_empty() {
            return Err(GmError::Config("Dismissal note cannot be empty".into()));
        }
        SharedFileStore::global().update_sync(self.paths.feedback_md(), None, move |existing| {
            let base = if existing.trim().is_empty() {
                "# FEEDBACK.md\n\n"
            } else {
                existing
            };
            Ok(upsert_section_bullet(
                base,
                "## Dismissed once",
                &clean,
                true,
            ))
        })?;
        Ok(())
    }

    pub fn add_feedback_note(&self, note: &str) -> Result<()> {
        let clean = note.trim().to_string();
        if clean.is_empty() {
            return Err(GmError::Config("Feedback note cannot be empty".into()));
        }
        SharedFileStore::global().update_sync(self.paths.feedback_md(), None, move |existing| {
            let base = if existing.trim().is_empty() {
                "# FEEDBACK.md\n\n"
            } else {
                existing
            };
            Ok(upsert_section_bullet(base, "## Notes", &clean, false))
        })?;
        Ok(())
    }

    pub fn permanently_opted_out_set(&self) -> Result<BTreeSet<String>> {
        Ok(self
            .load()?
            .never_tell_me_about
            .into_iter()
            .map(|s| s.to_lowercase())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quiet_gate::QuietGateConfig;
    use chrono::{TimeZone, Utc};

    #[test]
    fn targeted_edits_preserve_unrelated_prose_and_separate_dismissals_from_opt_outs() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MuseRuntimePaths::shared(dir.path());
        SharedFileStore::global()
            .write_exact_sync(
                paths.proactive_preferences_md(),
                None,
                "# PROACTIVE_PREFERENCES.md\n\nCustom user note that must never be clobbered.\n\n## Tell me about\n- Flight delays\n\n## Never tell me about\n\n## When\n- Only urgent alerts late\n\n## How\n- Keep short.\n",
            )
            .unwrap();

        let store = ProactivePrefsStore::new(paths.clone());
        let gate = QuietGate::new(QuietGateConfig::default());

        // One-time dismissal stays in FEEDBACK.md and does NOT opt out topic permanently
        store
            .add_one_time_dismissal("Dismissed crypto digest once (notice-123)")
            .unwrap();
        store
            .add_permanent_opt_out("marketing", Some(&gate))
            .unwrap();
        store.set_quiet_hours(21, 9, Some(&gate)).unwrap();
        store
            .write_timezones(&TimezonesConfig {
                home_tz: "America/Los_Angeles".into(),
                work_tz: Some("America/New_York".into()),
            })
            .unwrap();

        let after = std::fs::read_to_string(paths.proactive_preferences_md()).unwrap();
        assert!(after.contains("Custom user note that must never be clobbered."));
        assert!(after.contains("- Flight delays"));
        assert!(after.contains("- Keep short."));
        assert!(after.contains("- marketing"));
        assert!(after.contains("Quiet hours: 21:00-09:00"));

        let fresh_gate = QuietGate::new(QuietGateConfig::default());
        let snap = store.sync_with_quiet_gate(&fresh_gate).unwrap();
        assert_eq!(snap.quiet_hours, Some((21, 9)));
        assert_eq!(snap.timezones.home_tz, "America/Los_Angeles");
        assert_eq!(snap.one_time_dismissals.len(), 1);

        let nine_pm = Utc.with_ymd_and_hms(2026, 11, 3, 21, 15, 0).unwrap();
        let (ok_quiet, _) = fresh_gate.can_deliver("calendar", "h1", false, nine_pm);
        assert!(!ok_quiet, "Synced quiet hours 21-09 must suppress 21:15");

        let noon = Utc.with_ymd_and_hms(2026, 11, 3, 12, 0, 0).unwrap();
        let (ok_optout, _) = fresh_gate.can_deliver("marketing", "h2", false, noon);
        assert!(!ok_optout);
        let (ok_once, _) = fresh_gate.can_deliver("crypto", "h3", false, noon);
        assert!(
            ok_once,
            "One-time dismissal must NOT permanently opt out crypto"
        );
    }
}
