//! Durable local-time schedules. Queue insertion and schedule advancement commit together.
use crate::{DurableTaskQueue, OpenBotSupervisor};
use chrono::{DateTime, Datelike, Days, LocalResult, NaiveTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use gm_core::error::{GmError, Result};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
fn error(e: impl std::fmt::Display) -> GmError {
    GmError::Config(format!("Routine: {e}"))
}
fn constant_time_secret_eq(a: &str, b: &str) -> bool {
    use sha2::{Digest, Sha256};
    let da = Sha256::digest(a.as_bytes());
    let db = Sha256::digest(b.as_bytes());
    let mut diff = 0u8;
    for (x, y) in da.iter().zip(db.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuietHours {
    pub start_hour: u32,
    pub end_hour: u32,
}

impl QuietHours {
    pub fn is_quiet(&self, hour: u32) -> bool {
        if self.start_hour <= self.end_hour {
            hour >= self.start_hour && hour < self.end_hour
        } else {
            hour >= self.start_hour || hour < self.end_hour
        }
    }
}

pub const MAX_ROUTINES_PER_BOT: usize = 50;
pub const MIN_ROUTINE_INTERVAL_MINUTES: u32 = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Routine {
    pub id: String,
    pub name: String,
    pub bot_id: String,
    pub channel_id: Option<String>,
    pub instruction: String,
    pub timezone: String,
    pub hour: u32,
    pub minute: u32,
    /// Monday=0 through Sunday=6. Empty means every day.
    pub weekdays: Vec<u32>,
    #[serde(default)]
    pub quiet_hours: Option<QuietHours>,
    #[serde(default)]
    pub interval_minutes: Option<u32>,
    #[serde(default)]
    pub webhook_secret_ref: Option<String>,
    #[serde(default)]
    pub preferred_provider: Option<String>,
    #[serde(default)]
    pub preferred_model: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutineRunRecord {
    pub id: String,
    pub routine_id: String,
    pub task_id: Option<String>,
    pub scheduled_for: String,
    pub fired_at: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct RoutineState {
    pub routine: Routine,
    pub next_run_utc: String,
    pub last_task_id: Option<String>,
}
impl Routine {
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty()
            || self.id.len() > 64
            || !self
                .id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            || self.name.trim().is_empty()
            || self.name.len() > 256
            || self.instruction.trim().is_empty()
            || self.instruction.len() > 64 * 1024
            || self.weekdays.len() > 7
            || self.weekdays.iter().any(|d| *d > 6)
            || self.hour > 23
            || self.minute > 59
        {
            return Err(error("Invalid schedule, identifier or text limits"));
        }
        if let Some(mins) = self.interval_minutes
            && !(MIN_ROUTINE_INTERVAL_MINUTES..=10_080).contains(&mins)
        {
            return Err(error(format!(
                "Interval must be at least {MIN_ROUTINE_INTERVAL_MINUTES} minutes (max 10080)"
            )));
        }
        if let Some(ref q) = self.quiet_hours
            && (q.start_hour > 23 || q.end_hour > 23)
        {
            return Err(error("Invalid quiet hours: must be 0-23"));
        }
        self.timezone.parse::<Tz>().map_err(error)?;
        Ok(())
    }
    /// DST policy: once on the earlier offset of a repeated time; a skipped
    /// time advances to the first valid local minute on that date.
    pub fn next_after(&self, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
        self.validate()?;
        if let Some(mins) = self.interval_minutes {
            return Ok(now + chrono::Duration::minutes(mins as i64));
        }
        let timezone = self.timezone.parse::<Tz>().map_err(error)?;
        let today = now.with_timezone(&timezone).date_naive();
        for offset in 0..15 {
            let date = today
                .checked_add_days(Days::new(offset))
                .ok_or_else(|| error("Date exceeds supported range"))?;
            if !self.weekdays.is_empty()
                && !self
                    .weekdays
                    .contains(&date.weekday().num_days_from_monday())
            {
                continue;
            }
            let requested = NaiveTime::from_hms_opt(self.hour, self.minute, 0)
                .ok_or_else(|| error("Invalid local time"))?;
            for minutes in 0..180 {
                let candidate = date.and_time(requested) + chrono::Duration::minutes(minutes);
                if candidate.date() != date {
                    break;
                }
                let candidate = match timezone.from_local_datetime(&candidate) {
                    LocalResult::Single(time) => Some(time),
                    LocalResult::Ambiguous(first, second) => Some(first.min(second)),
                    LocalResult::None => None,
                };
                if let Some(candidate) = candidate {
                    let candidate = candidate.with_timezone(&Utc);
                    if candidate > now {
                        return Ok(candidate);
                    }
                    break;
                }
            }
        }
        Err(error("No next occurrence within 15 days"))
    }
}
impl DurableTaskQueue {
    pub(crate) fn init_routines(&self) -> Result<()> {
        self.conn.lock().execute_batch("CREATE TABLE IF NOT EXISTS routines_v1 (
            id TEXT PRIMARY KEY, config_json TEXT NOT NULL, next_due TEXT NOT NULL, last_task_id TEXT);
            CREATE INDEX IF NOT EXISTS routines_due ON routines_v1(next_due);
            CREATE TABLE IF NOT EXISTS routine_runs_v1 (
            id TEXT PRIMARY KEY, routine_id TEXT NOT NULL, task_id TEXT, scheduled_for TEXT NOT NULL, fired_at TEXT NOT NULL, status TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS routine_runs_idx ON routine_runs_v1(routine_id, fired_at DESC);").map_err(error)
    }
    pub fn save_routine(&self, routine: &Routine, now: DateTime<Utc>) -> Result<()> {
        let next = routine.next_after(now)?.to_rfc3339();
        let conn = self.conn.lock();
        let existing_count = {
            let mut stmt = conn
                .prepare("SELECT id, config_json FROM routines_v1")
                .map_err(error)?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(error)?;
            let mut cnt = 0usize;
            for r in rows {
                let (id, cfg_json) = r.map_err(error)?;
                if id != routine.id
                    && let Ok(parsed) = serde_json::from_str::<Routine>(&cfg_json)
                    && parsed.bot_id == routine.bot_id
                {
                    cnt += 1;
                }
            }
            cnt
        };
        if existing_count >= MAX_ROUTINES_PER_BOT {
            return Err(error(format!(
                "Bot '{}' has reached the maximum of {MAX_ROUTINES_PER_BOT} routines",
                routine.bot_id
            )));
        }
        conn.execute(
            "INSERT INTO routines_v1(id,config_json,next_due) VALUES (?1,?2,?3)
            ON CONFLICT(id) DO UPDATE SET config_json=excluded.config_json,next_due=excluded.next_due",
            params![routine.id, serde_json::to_string(routine)?, next],
        )
        .map_err(error)?;
        Ok(())
    }

    pub fn delete_routine(&self, routine_id: &str) -> Result<bool> {
        let rows = self
            .conn
            .lock()
            .execute(
                "DELETE FROM routines_v1 WHERE id = ?1",
                params![routine_id.trim()],
            )
            .map_err(error)?;
        Ok(rows > 0)
    }
    pub fn list_routines(&self) -> Result<Vec<RoutineState>> {
        let connection = self.conn.lock();
        let mut statement = connection
            .prepare("SELECT config_json,next_due,last_task_id FROM routines_v1 ORDER BY id")
            .map_err(error)?;
        let rows = statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(error)?;
        rows.map(|row| {
            let (json, next_run_utc, last_task_id) = row.map_err(error)?;
            Ok(RoutineState {
                routine: serde_json::from_str(&json)?,
                next_run_utc,
                last_task_id,
            })
        })
        .collect()
    }
    pub fn list_routine_runs(
        &self,
        routine_id: &str,
        limit: usize,
    ) -> Result<Vec<RoutineRunRecord>> {
        let connection = self.conn.lock();
        let mut statement = connection
            .prepare("SELECT id, routine_id, task_id, scheduled_for, fired_at, status FROM routine_runs_v1 WHERE routine_id=?1 ORDER BY fired_at DESC LIMIT ?2")
            .map_err(error)?;
        let rows = statement
            .query_map(params![routine_id, limit as i64], |r| {
                Ok(RoutineRunRecord {
                    id: r.get(0)?,
                    routine_id: r.get(1)?,
                    task_id: r.get(2)?,
                    scheduled_for: r.get(3)?,
                    fired_at: r.get(4)?,
                    status: r.get(5)?,
                })
            })
            .map_err(error)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(error)?);
        }
        Ok(out)
    }
    fn fire_routine(&self, routine: &Routine, now: DateTime<Utc>) -> Result<Option<String>> {
        let mut connection = self.conn.lock();
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let current: Option<(String, String, Option<String>)> = tx
            .query_row(
                "SELECT config_json,next_due,last_task_id FROM routines_v1 WHERE id=?1",
                [&routine.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(error)?;
        let Some((json, due, last)) = current else {
            return Ok(None);
        };
        // Compare the reviewed configuration again under the transaction, so a
        // concurrent disable/edit cannot fire a stale schedule.
        if json != serde_json::to_string(routine)? || !routine.enabled {
            return Ok(None);
        }
        let due = DateTime::parse_from_rfc3339(&due)
            .map_err(error)?
            .with_timezone(&Utc);
        if due > now {
            return Ok(None);
        }
        let next = routine.next_after(now)?.to_rfc3339();
        let (_, task_id) = admit_run(&tx, routine, last, due, now, &due.timestamp().to_string())?;
        tx.execute(
            "UPDATE routines_v1 SET next_due=?1 WHERE id=?2",
            params![next, routine.id],
        )
        .map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(task_id)
    }

    /// Runs an enabled routine now for an external trigger (webhook/app event).
    /// `delivery_id` is claimed in the same transaction, so a redelivery never
    /// queues twice and a failed admission leaves the delivery retryable.
    /// Returns `None` for an unknown routine. The schedule is not advanced.
    pub fn trigger_routine(
        &self,
        routine_id: &str,
        delivery_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<RoutineTriggerOutcome>> {
        if delivery_id.is_empty()
            || delivery_id.len() > 128
            || !delivery_id.bytes().all(|c| c.is_ascii_graphic())
        {
            return Err(error(
                "Delivery identifier must be 1-128 visible ASCII characters",
            ));
        }
        let mut connection = self.conn.lock();
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let current: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT config_json,last_task_id FROM routines_v1 WHERE id=?1",
                [routine_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(error)?;
        let Some((json, last)) = current else {
            return Ok(None);
        };
        let routine: Routine = serde_json::from_str(&json)?;
        let outcome = |status: &str, task_id| {
            Ok(Some(RoutineTriggerOutcome {
                status: status.into(),
                task_id,
            }))
        };
        // Disabled (including freshly imported) routines do not consume the delivery.
        if !routine.enabled {
            return outcome("disabled", None);
        }
        let claimed = tx
            .execute(
                "INSERT OR IGNORE INTO bridge_deliveries(delivery_id,received_at) VALUES (?1,?2)",
                params![
                    format!("routine:{routine_id}:{delivery_id}"),
                    now.timestamp()
                ],
            )
            .map_err(error)?
            > 0;
        if !claimed {
            return outcome("duplicate", None);
        }
        let key = format!("hook-{}", uuid::Uuid::new_v4().simple());
        let (status, task_id) = admit_run(&tx, &routine, last, now, now, &key)?;
        tx.commit().map_err(error)?;
        outcome(status, task_id)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RoutineTriggerOutcome {
    /// `queued`, `duplicate`, `disabled`, `overlap_suppressed` or `skipped_quiet_hours`.
    pub status: String,
    pub task_id: Option<String>,
}

/// Applies quiet hours and overlap prevention inside the caller's transaction,
/// records the run, and queues a task when admitted.
fn admit_run(
    tx: &rusqlite::Transaction<'_>,
    routine: &Routine,
    last: Option<String>,
    scheduled_for: DateTime<Utc>,
    now: DateTime<Utc>,
    key: &str,
) -> Result<(&'static str, Option<String>)> {
    let record = |run_id: String, task_id: Option<&str>, status: &str| {
        tx.execute(
            "INSERT OR REPLACE INTO routine_runs_v1(id,routine_id,task_id,scheduled_for,fired_at,status) VALUES (?1,?2,?3,?4,?5,?6)",
            params![run_id, routine.id, task_id, scheduled_for.to_rfc3339(), now.to_rfc3339(), status],
        )
        .map_err(error)
    };
    if let Some(ref q) = routine.quiet_hours {
        let tz = routine.timezone.parse::<Tz>().map_err(error)?;
        if q.is_quiet(now.with_timezone(&tz).hour()) {
            let status = "skipped_quiet_hours";
            record(format!("run-quiet-{}-{key}", routine.id), None, status)?;
            return Ok((status, None));
        }
    }
    let active = if let Some(last) = last {
        tx.query_row(
            "SELECT status NOT IN ('completed','failed','cancelled') FROM bot_tasks WHERE id=?1",
            [last],
            |r| r.get::<_, bool>(0),
        )
        .optional()
        .map_err(error)?
        .unwrap_or(false)
    } else {
        false
    };
    if active {
        let status = "overlap_suppressed";
        record(format!("run-overlap-{}-{key}", routine.id), None, status)?;
        return Ok((status, None));
    }
    let task_id = format!("routine-{}-{key}", routine.id);
    let timestamp = now.to_rfc3339();
    tx.execute("INSERT OR REPLACE INTO bot_tasks(id,bot_id,channel_id,status,instruction,created_at,updated_at) VALUES (?1,?2,?3,'queued',?4,?5,?5)",
        params![task_id,routine.bot_id,routine.channel_id,routine.instruction,timestamp]).map_err(error)?;
    record(
        format!("run-{}-{key}", routine.id),
        Some(&task_id),
        "queued",
    )?;
    tx.execute(
        "UPDATE routines_v1 SET last_task_id=?1 WHERE id=?2",
        params![task_id, routine.id],
    )
    .map_err(error)?;
    Ok(("queued", Some(task_id)))
}
impl OpenBotSupervisor {
    pub fn save_routine(&self, routine: &Routine) -> Result<()> {
        routine.validate()?;
        if self.roster().get_bot(&routine.bot_id).is_none() {
            return Err(error("Unknown bot"));
        }
        if let Some(channel) = &routine.channel_id
            && self.channels().get_channel(channel)?.is_none()
        {
            return Err(error("Unknown channel"));
        }

        let mut normalized = routine.clone();
        if normalized.webhook_secret_ref.is_none() {
            normalized.webhook_secret_ref = Some(format!(
                "secret://keychain/gm_routine_hook_{}",
                normalized.id
            ));
        }
        if let Some(ref secret_ref) = normalized.webhook_secret_ref
            && let Some(account) = secret_ref.strip_prefix("secret://keychain/")
            && self.vault().backend().get_secret(account).is_err()
        {
            let generated = format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            );
            let _ = self.vault().backend().set_secret(account, &generated);
        }

        self.queue().save_routine(&normalized, Utc::now())?;
        let _ = self
            .daemon_event_sender()
            .send(gm_core::ipc::DaemonEvent::RoutineUpdated {
                routine_id: Some(normalized.id.clone()),
                bot_id: Some(normalized.bot_id.clone()),
            });
        self.emit_agui(
            "ROUTINE_UPDATED",
            &normalized.id,
            &normalized.bot_id,
            serde_json::json!({ "routine_id": normalized.id, "enabled": normalized.enabled }),
        );
        Ok(())
    }

    pub fn verify_routine_webhook_key(&self, routine_id: &str, supplied_key: &str) -> Result<bool> {
        let trimmed = supplied_key.trim();
        if trimmed.is_empty() {
            return Ok(false);
        }
        let Some(state) = self
            .queue()
            .list_routines()?
            .into_iter()
            .find(|s| s.routine.id == routine_id)
        else {
            return Ok(false);
        };
        let account = state
            .routine
            .webhook_secret_ref
            .as_deref()
            .and_then(|s| s.strip_prefix("secret://keychain/"))
            .map(str::to_string)
            .unwrap_or_else(|| format!("gm_routine_hook_{}", state.routine.id));
        let Ok(expected) = self.vault().backend().get_secret(&account) else {
            return Ok(false);
        };
        Ok(constant_time_secret_eq(&expected, trimmed))
    }

    pub fn get_routine_webhook_key(&self, routine_id: &str) -> Result<Option<String>> {
        let Some(state) = self
            .queue()
            .list_routines()?
            .into_iter()
            .find(|s| s.routine.id == routine_id)
        else {
            return Ok(None);
        };
        let account = state
            .routine
            .webhook_secret_ref
            .as_deref()
            .and_then(|s| s.strip_prefix("secret://keychain/"))
            .map(str::to_string)
            .unwrap_or_else(|| format!("gm_routine_hook_{}", state.routine.id));
        Ok(self.vault().backend().get_secret(&account).ok())
    }

    pub fn routine_preference_for_task(
        &self,
        task_id: &str,
    ) -> Result<Option<(Option<String>, Option<String>)>> {
        let routine_id: Option<String> = self
            .queue()
            .conn
            .lock()
            .query_row(
                "SELECT routine_id FROM routine_runs_v1 WHERE task_id = ?1 LIMIT 1",
                [task_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(error)?;
        let Some(rid) = routine_id else {
            return Ok(None);
        };
        let found = self
            .queue()
            .list_routines()?
            .into_iter()
            .find(|s| s.routine.id == rid)
            .map(|s| (s.routine.preferred_provider, s.routine.preferred_model));
        Ok(found)
    }

    pub fn delete_routine(&self, routine_id: &str) -> Result<bool> {
        let deleted = self.queue().delete_routine(routine_id)?;
        if deleted {
            let _ = self
                .daemon_event_sender()
                .send(gm_core::ipc::DaemonEvent::RoutineUpdated {
                    routine_id: Some(routine_id.to_string()),
                    bot_id: None,
                });
            self.emit_agui(
                "ROUTINE_UPDATED",
                routine_id,
                "",
                serde_json::json!({ "routine_id": routine_id, "deleted": true }),
            );
        }
        Ok(deleted)
    }

    /// Manual "Test" button trigger: queues an immediate run bypassing `enabled` / `QuietGate` wait
    /// while recording a normal `RoutineRunRecord` and broadcasting delta updates.
    pub fn trigger_routine_test(&self, routine_id: &str) -> Result<Option<RoutineTriggerOutcome>> {
        let Some(state) = self
            .queue()
            .list_routines()?
            .into_iter()
            .find(|s| s.routine.id == routine_id)
        else {
            return Ok(None);
        };
        let routine = state.routine;
        if self.roster().get_bot(&routine.bot_id).is_none() {
            return Err(error("Routine bot is unavailable"));
        }
        let now = Utc::now();
        let task = self.submit_task(
            Some(&routine.bot_id),
            routine.channel_id.as_deref(),
            &routine.instruction,
            5,
        )?;
        let run_id = format!("run-test-{}-{}", routine.id, uuid::Uuid::new_v4().simple());
        self.queue().conn.lock().execute(
            "INSERT OR REPLACE INTO routine_runs_v1(id,routine_id,task_id,scheduled_for,fired_at,status) VALUES (?1,?2,?3,?4,?5,'queued')",
            params![run_id, routine.id, task.id, now.to_rfc3339(), now.to_rfc3339()],
        ).map_err(error)?;
        let _ = self.queue().conn.lock().execute(
            "UPDATE routines_v1 SET last_task_id=?1 WHERE id=?2",
            params![task.id, routine.id],
        );

        let run_record = RoutineRunRecord {
            id: run_id,
            routine_id: routine.id.clone(),
            task_id: Some(task.id.clone()),
            scheduled_for: now.to_rfc3339(),
            fired_at: now.to_rfc3339(),
            status: "queued".into(),
        };
        let _ = self
            .daemon_event_sender()
            .send(gm_core::ipc::DaemonEvent::RoutineRunRecorded {
                routine_id: routine.id.clone(),
                run: serde_json::to_value(&run_record).unwrap_or_default(),
            });
        let _ = self
            .daemon_event_sender()
            .send(gm_core::ipc::DaemonEvent::RoutineUpdated {
                routine_id: Some(routine.id.clone()),
                bot_id: Some(routine.bot_id.clone()),
            });
        self.emit_agui(
            "ROUTINE_QUEUED",
            &task.id,
            &routine.bot_id,
            serde_json::json!({ "routine_id": routine.id, "trigger": "manual_test" }),
        );
        Ok(Some(RoutineTriggerOutcome {
            status: "queued".into(),
            task_id: Some(task.id),
        }))
    }

    pub(crate) fn tick_routines(&self, now: DateTime<Utc>) -> Result<usize> {
        let mut count = 0;
        for state in self.queue().list_routines()? {
            let routine = state.routine;
            if !routine.enabled || self.roster().get_bot(&routine.bot_id).is_none() {
                continue;
            }
            if let Some(channel) = &routine.channel_id {
                match self.channels().get_channel(channel)? {
                    Some(c) if !c.paused && !c.archived => {}
                    _ => continue,
                }
            }
            if let Some(id) = self.queue().fire_routine(&routine, now)? {
                count += 1;
                let _ =
                    self.daemon_event_sender()
                        .send(gm_core::ipc::DaemonEvent::RoutineUpdated {
                            routine_id: Some(routine.id.clone()),
                            bot_id: Some(routine.bot_id.clone()),
                        });
                self.emit_agui(
                    "ROUTINE_QUEUED",
                    id,
                    &routine.bot_id,
                    serde_json::json!({"routine_id":routine.id}),
                );
            }
        }
        Ok(count)
    }
    pub fn list_routine_runs(
        &self,
        routine_id: &str,
        limit: usize,
    ) -> Result<Vec<RoutineRunRecord>> {
        self.queue().list_routine_runs(routine_id, limit)
    }
    /// External trigger entry point at the current wall-clock time.
    /// Applies bot/channel gates, `QuietGate`, and records a Feed post on delivery.
    pub fn trigger_routine(
        &self,
        routine_id: &str,
        delivery_id: &str,
    ) -> Result<Option<RoutineTriggerOutcome>> {
        self.trigger_routine_at(routine_id, delivery_id, Utc::now())
    }

    /// Deterministic timestamped variant of [`OpenBotSupervisor::trigger_routine`].
    pub fn trigger_routine_at(
        &self,
        routine_id: &str,
        delivery_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<RoutineTriggerOutcome>> {
        let Some(state) = self
            .queue()
            .list_routines()?
            .into_iter()
            .find(|s| s.routine.id == routine_id)
        else {
            return Ok(None);
        };
        let routine = state.routine;
        if self.roster().get_bot(&routine.bot_id).is_none() {
            return Err(error("Routine bot is unavailable"));
        }
        if let Some(channel) = &routine.channel_id {
            match self.channels().get_channel(channel)? {
                Some(c) if !c.paused && !c.archived => {}
                _ => return Err(error("Routine channel is paused, archived or missing")),
            }
        }
        let _ = self.sync_quiet_gate_from_prefs();
        let dedup_key = format!("routine:{routine_id}:{delivery_id}");
        if self.quiet_gate().is_topic_opted_out("routine")
            || self.quiet_gate().is_topic_opted_out(&routine.name)
        {
            return Ok(Some(RoutineTriggerOutcome {
                status: "skipped_quiet_hours".into(),
                task_id: None,
            }));
        }
        let (allowed, _) = self
            .quiet_gate()
            .can_deliver("routine", &dedup_key, false, now);
        if !allowed {
            return Ok(Some(RoutineTriggerOutcome {
                status: "skipped_quiet_hours".into(),
                task_id: None,
            }));
        }
        let outcome = self.queue().trigger_routine(routine_id, delivery_id, now)?;
        if let Some(RoutineTriggerOutcome {
            task_id: Some(id), ..
        }) = &outcome
        {
            self.quiet_gate().record_delivery_at(&dedup_key, now);
            let _ = self
                .feed_store()
                .record_signal(&dedup_key, "routine_trigger");
            let _ = self.feed_store().upsert_post(&gm_goals::FeedPost {
                id: format!("routine-{id}"),
                kicker: "Routine".into(),
                category: gm_goals::FeedCategory::PatrolInsight,
                title: format!("Routine triggered: {}", routine.name),
                body: routine.instruction.clone(),
                signal_citations: vec![dedup_key],
                rating: None,
                user_prompt: None,
                created_at: now,
            });
            let _ = self
                .daemon_event_sender()
                .send(gm_core::ipc::DaemonEvent::RoutineUpdated {
                    routine_id: Some(routine.id.clone()),
                    bot_id: Some(routine.bot_id.clone()),
                });
            self.emit_agui(
                "ROUTINE_QUEUED",
                id,
                &routine.bot_id,
                serde_json::json!({"routine_id":routine.id,"trigger":"webhook"}),
            );
        }
        Ok(outcome)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn daily() -> Routine {
        Routine {
            id: "daily".into(),
            name: "Daily report".into(),
            bot_id: "researcher".into(),
            channel_id: None,
            instruction: "Prepare report".into(),
            timezone: "America/New_York".into(),
            hour: 2,
            minute: 30,
            weekdays: vec![],
            quiet_hours: None,
            interval_minutes: None,
            webhook_secret_ref: None,
            preferred_provider: None,
            preferred_model: None,
            enabled: true,
        }
    }
    fn time(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }
    #[test]
    fn dst_gap_and_fold_fire_once_with_an_explicit_policy() {
        let mut routine = daily();
        assert_eq!(
            routine.next_after(time("2026-03-08T05:00:00Z")).unwrap(),
            time("2026-03-08T07:00:00Z")
        );
        routine.hour = 1;
        routine.minute = 30;
        assert_eq!(
            routine.next_after(time("2026-11-01T04:00:00Z")).unwrap(),
            time("2026-11-01T05:30:00Z")
        );
        assert_eq!(
            routine.next_after(time("2026-11-01T05:31:00Z")).unwrap(),
            time("2026-11-02T06:30:00Z")
        );
        routine.timezone = "Invalid/Zone".into();
        assert!(routine.validate().is_err());
    }
    #[test]
    fn restart_and_parallel_connections_enqueue_once_and_suppress_overlap() {
        let path = std::env::temp_dir().join(format!("gm-routine-{}.sqlite", uuid::Uuid::new_v4()));
        let first = DurableTaskQueue::open(&path).unwrap();
        let second = DurableTaskQueue::open(&path).unwrap();
        let routine = daily();
        first
            .save_routine(&routine, time("2026-11-04T00:00:00Z"))
            .unwrap();
        let now = time("2026-11-07T12:00:00Z");
        let task = first.fire_routine(&routine, now).unwrap().unwrap();
        assert!(second.fire_routine(&routine, now).unwrap().is_none());
        assert!(
            second
                .fire_routine(&routine, time("2026-11-08T12:00:00Z"))
                .unwrap()
                .is_none()
        );
        assert_eq!(second.list_tasks(None).unwrap().len(), 1);
        first.cancel(&task).unwrap();
        assert!(
            second
                .fire_routine(&routine, time("2026-11-09T12:00:00Z"))
                .unwrap()
                .is_some()
        );
        let mut disabled = routine.clone();
        disabled.enabled = false;
        first
            .save_routine(&disabled, time("2026-11-09T12:00:00Z"))
            .unwrap();
        assert!(
            second
                .fire_routine(&routine, time("2026-11-10T12:00:00Z"))
                .unwrap()
                .is_none()
        );
        drop(first);
        drop(second);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_routine_quiet_hours_and_run_history_tracking() {
        let path =
            std::env::temp_dir().join(format!("gm-routine-quiet-{}.sqlite", uuid::Uuid::new_v4()));
        let queue = DurableTaskQueue::open(&path).unwrap();
        let mut routine = daily();
        routine.quiet_hours = Some(QuietHours {
            start_hour: 22,
            end_hour: 6, // 10 PM to 6 AM is quiet
        });
        queue
            .save_routine(&routine, time("2026-11-04T00:00:00Z"))
            .unwrap();

        // 2:30 AM EST is 07:30:00Z, which is within quiet hours (22 -> 6)
        let quiet_time = time("2026-11-04T07:30:00Z");
        let result = queue.fire_routine(&routine, quiet_time).unwrap();
        assert!(result.is_none());

        let runs = queue.list_routine_runs(&routine.id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "skipped_quiet_hours");

        // Now disable quiet hours
        routine.quiet_hours = None;
        queue
            .save_routine(&routine, time("2026-11-04T08:00:00Z"))
            .unwrap();

        let fire_time = time("2026-11-05T08:00:00Z");
        let task = queue.fire_routine(&routine, fire_time).unwrap();
        assert!(task.is_some());

        let runs2 = queue.list_routine_runs(&routine.id, 10).unwrap();
        assert_eq!(runs2.len(), 2);
        assert_eq!(runs2[0].status, "queued");

        drop(queue);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_trigger_routine_deduplication_and_outcomes() {
        let path =
            std::env::temp_dir().join(format!("gm-routine-trig-{}.sqlite", uuid::Uuid::new_v4()));
        let queue = DurableTaskQueue::open(&path).unwrap();
        let routine = daily();
        queue
            .save_routine(&routine, time("2026-11-04T00:00:00Z"))
            .unwrap();

        let now = time("2026-11-04T12:00:00Z");
        let outcome1 = queue
            .trigger_routine(&routine.id, "delivery-1", now)
            .unwrap()
            .unwrap();
        assert_eq!(outcome1.status, "queued");
        assert!(outcome1.task_id.is_some());

        // Duplicate delivery_id is rejected
        let outcome2 = queue
            .trigger_routine(&routine.id, "delivery-1", now)
            .unwrap()
            .unwrap();
        assert_eq!(outcome2.status, "duplicate");
        assert!(outcome2.task_id.is_none());

        // Second delivery while first task is active is overlap_suppressed
        let outcome3 = queue
            .trigger_routine(&routine.id, "delivery-2", now)
            .unwrap()
            .unwrap();
        assert_eq!(outcome3.status, "overlap_suppressed");
        assert!(outcome3.task_id.is_none());

        // Cancel the first task so it is no longer active
        queue.cancel(outcome1.task_id.as_deref().unwrap()).unwrap();

        // Third delivery now succeeds
        let outcome4 = queue
            .trigger_routine(&routine.id, "delivery-3", now)
            .unwrap()
            .unwrap();
        assert_eq!(outcome4.status, "queued");
        assert!(outcome4.task_id.is_some());

        drop(queue);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_supervisor_trigger_routine_uses_quiet_gate_and_creates_feed_post() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = gm_core::config::GmConfig {
            storage_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        cfg.openbot.bots_dir = dir.path().join("bots");
        let (tx, _) = tokio::sync::broadcast::channel(16);
        let sup = OpenBotSupervisor::open_with_secret_backend(
            cfg,
            std::sync::Arc::new(gm_safety::AuditLogger::in_memory()),
            tx,
            std::sync::Arc::new(crate::vault::InMemorySecretBackend::default()),
        )
        .unwrap();

        let routine = daily();
        sup.save_routine(&routine).unwrap();

        let quiet_utc = time("2026-11-04T02:00:00Z");
        let quiet_res = sup
            .trigger_routine_at(&routine.id, "d-quiet", quiet_utc)
            .unwrap()
            .unwrap();
        assert_eq!(quiet_res.status, "skipped_quiet_hours");
        assert!(quiet_res.task_id.is_none());

        let initial_posts = sup.feed_store().list_posts(20).unwrap().len();
        let daytime_utc = time("2026-11-04T14:00:00Z");
        let ok_res = sup
            .trigger_routine_at(&routine.id, "d-day", daytime_utc)
            .unwrap()
            .unwrap();
        assert_eq!(ok_res.status, "queued");
        assert!(ok_res.task_id.is_some());

        let posts = sup.feed_store().list_posts(20).unwrap();
        assert_eq!(posts.len(), initial_posts + 1);
        assert!(posts.iter().any(|p| p.kicker == "Routine"));
    }

    #[test]
    fn routine_50_per_bot_cap_5_min_interval_and_manual_test_trigger() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = gm_core::config::GmConfig {
            storage_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        cfg.openbot.bots_dir = dir.path().join("bots");
        let (tx, _) = tokio::sync::broadcast::channel(16);
        let sup = OpenBotSupervisor::open_with_secret_backend(
            cfg,
            std::sync::Arc::new(gm_safety::AuditLogger::in_memory()),
            tx,
            std::sync::Arc::new(crate::vault::InMemorySecretBackend::default()),
        )
        .unwrap();

        let mut too_fast = daily();
        too_fast.id = "too-fast".into();
        too_fast.interval_minutes = Some(4);
        assert!(sup.save_routine(&too_fast).is_err());

        for i in 0..MAX_ROUTINES_PER_BOT {
            let mut r = daily();
            r.id = format!("r-{i}");
            r.interval_minutes = Some(5);
            r.enabled = false;
            sup.save_routine(&r).unwrap();
        }
        let mut r51 = daily();
        r51.id = "r-50".into();
        assert!(sup.save_routine(&r51).is_err());

        // Test trigger runs even when routine is disabled
        let out = sup.trigger_routine_test("r-0").unwrap().unwrap();
        assert_eq!(out.status, "queued");
        assert!(sup.list_routine_runs("r-0", 20).unwrap().len() == 1);

        assert!(sup.delete_routine("r-0").unwrap());
        // Slot freed!
        sup.save_routine(&r51).unwrap();
    }

    #[test]
    fn test_routine_dedup_and_bounded_previews() {
        test_trigger_routine_deduplication_and_outcomes();
        let dir = tempfile::tempdir().unwrap();
        let img = image::RgbImage::from_pixel(64, 48, image::Rgb([30, 144, 255]));
        let img_path = dir.path().join("sample.png");
        img.save(&img_path).unwrap();
        let img_bytes = std::fs::read(&img_path).unwrap();
        let preview =
            crate::attachments::preview_attachment_bounded("sample.png", &img_bytes).unwrap();
        assert_eq!(
            preview.kind,
            crate::attachments::AttachmentPreviewKind::Image
        );
        assert_eq!(preview.width, Some(64));
        assert_eq!(preview.height, Some(48));

        let pdf_bytes =
            b"%PDF-1.4\n1 0 obj << /Type /Page >> endobj\n(Bounded preview test) Tj\n%%EOF";
        let pdf_prev =
            crate::attachments::preview_attachment_bounded("doc.pdf", pdf_bytes).unwrap();
        assert_eq!(
            pdf_prev.kind,
            crate::attachments::AttachmentPreviewKind::Pdf
        );
        assert_eq!(pdf_prev.pdf_pages_estimate, Some(1));
        assert!(
            pdf_prev
                .text_snippet
                .as_deref()
                .unwrap_or("")
                .contains("Bounded preview test")
        );
    }
}
