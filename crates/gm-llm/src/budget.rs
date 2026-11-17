use gm_core::config::BudgetConfig;
use gm_core::error::{GmError, Result};
use parking_lot::Mutex;
use rusqlite::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageBucket {
    pub key: String,
    pub amount_usd: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageDailyPoint {
    pub day: String,
    pub amount_usd: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageBreakdown {
    pub daily_spent_usd: f64,
    pub weekly_spent_usd: f64,
    pub daily_cap_usd: f64,
    pub per_job_cap_usd: f64,
    pub max_daily_notifications: u32,
    pub by_bot: Vec<UsageBucket>,
    pub by_source: Vec<UsageBucket>,
    pub daily_series: Vec<UsageDailyPoint>,
}

#[derive(Debug, Clone)]
struct MemoryUsageEntry {
    day: chrono::NaiveDate,
    bot_id: String,
    source: String,
    amount: f64,
}

#[derive(Debug)]
struct DailyLedger {
    day: chrono::NaiveDate,
    spent: f64,
    reserved: f64,
    poisoned: bool,
    entries: BTreeMap<String, MemoryUsageEntry>,
}

#[derive(Debug, Default)]
struct JobLedger {
    spent: f64,
    reserved: f64,
}

#[derive(Debug, Clone)]
pub struct BudgetManager {
    config: BudgetConfig,
    daily: Arc<Mutex<DailyLedger>>,
    job: Arc<Mutex<JobLedger>>,
    store: Option<Arc<Mutex<Connection>>>,
}

/// An admitted request reserves its estimated maximum exposure before dispatch.
/// On cancellation/failure, retain that exposure conservatively because a provider may have billed it.
pub struct BudgetReservation {
    manager: BudgetManager,
    amount: f64,
    settled: bool,
    id: String,
}

impl BudgetReservation {
    pub fn release(mut self) {
        let mut daily = self.manager.daily.lock();
        let mut job = self.manager.job.lock();
        if let Err(error) = self.manager.persist_settlement(&self.id, 0.0) {
            daily.poisoned = true;
            tracing::error!(%error, "Budget release failed; further requests blocked");
            return;
        }
        daily.entries.remove(&self.id);
        daily.reserved = (daily.reserved - self.amount).max(0.0);
        job.reserved = (job.reserved - self.amount).max(0.0);
        self.settled = true;
    }

    pub fn settle(mut self, actual: f64) -> Result<()> {
        if !actual.is_finite() || actual < 0.0 {
            return Err(GmError::BudgetExceeded("Invalid provider cost".into()));
        }
        let mut daily = self.manager.daily.lock();
        let mut job = self.manager.job.lock();
        if let Err(error) = self.manager.persist_settlement(&self.id, actual) {
            daily.poisoned = true;
            return Err(error);
        }
        if let Some(entry) = daily.entries.get_mut(&self.id) {
            entry.amount = actual;
        }
        daily.reserved = (daily.reserved - self.amount).max(0.0);
        job.reserved = (job.reserved - self.amount).max(0.0);
        daily.spent += actual;
        job.spent += actual;
        self.settled = true;
        if (self.manager.store.is_none() && daily.spent > self.manager.config.daily_spend_cap_usd)
            || (self.manager.store.is_some()
                && self.manager.persisted_total()? > self.manager.config.daily_spend_cap_usd)
            || job.spent > self.manager.config.per_job_spend_cap_usd
        {
            return Err(GmError::BudgetExceeded(
                "Actual provider usage exceeded the estimate; further requests are blocked".into(),
            ));
        }
        Ok(())
    }
}

impl Drop for BudgetReservation {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let mut daily = self.manager.daily.lock();
        let mut job = self.manager.job.lock();
        if let Err(error) = self.manager.persist_settlement(&self.id, self.amount) {
            daily.poisoned = true;
            tracing::error!(%error, "Budget settlement failed; reservation remains charged");
        }
        if let Some(entry) = daily.entries.get_mut(&self.id) {
            entry.amount = self.amount;
        }
        daily.reserved = (daily.reserved - self.amount).max(0.0);
        job.reserved = (job.reserved - self.amount).max(0.0);
        daily.spent += self.amount;
        job.spent += self.amount;
    }
}

impl BudgetManager {
    pub fn new(config: BudgetConfig) -> Self {
        Self {
            config,
            daily: Arc::new(Mutex::new(DailyLedger {
                day: chrono::Utc::now().date_naive(),
                spent: 0.0,
                reserved: 0.0,
                poisoned: false,
                entries: BTreeMap::new(),
            })),
            job: Arc::new(Mutex::new(JobLedger::default())),
            store: None,
        }
    }

    /// Durable admission ledger. Pending reservations count after crashes and
    /// independent connections serialize admission with BEGIN IMMEDIATE.
    pub fn open(config: BudgetConfig, storage: &Path) -> Result<Self> {
        gm_core::storage::ensure_private_dir(storage)?;
        let connection = Connection::open(storage.join("usage.sqlite3"))
            .map_err(|e| GmError::Config(format!("Cannot open usage ledger: {e}")))?;
        connection
            .busy_timeout(std::time::Duration::from_secs(3))
            .map_err(db_error)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS usage_v1 (
                id TEXT PRIMARY KEY, day TEXT NOT NULL, amount REAL NOT NULL CHECK(amount >= 0),
                settled INTEGER NOT NULL DEFAULT 0,
                bot_id TEXT NOT NULL DEFAULT 'muse',
                source TEXT NOT NULL DEFAULT 'chat');
            CREATE INDEX IF NOT EXISTS usage_v1_day ON usage_v1(day);",
            )
            .map_err(db_error)?;

        // Idempotently migrate older usage_v1 schemas before creating attributed index.
        let mut has_bot_id = false;
        let mut has_source = false;
        {
            let mut stmt = connection
                .prepare("PRAGMA table_info(usage_v1)")
                .map_err(db_error)?;
            let mut rows = stmt.query([]).map_err(db_error)?;
            while let Some(row) = rows.next().map_err(db_error)? {
                let col: String = row.get(1).map_err(db_error)?;
                if col == "bot_id" {
                    has_bot_id = true;
                } else if col == "source" {
                    has_source = true;
                }
            }
        }
        if !has_bot_id {
            connection
                .execute(
                    "ALTER TABLE usage_v1 ADD COLUMN bot_id TEXT NOT NULL DEFAULT 'muse'",
                    [],
                )
                .map_err(db_error)?;
        }
        if !has_source {
            connection
                .execute(
                    "ALTER TABLE usage_v1 ADD COLUMN source TEXT NOT NULL DEFAULT 'chat'",
                    [],
                )
                .map_err(db_error)?;
        }
        connection
            .execute(
                "CREATE INDEX IF NOT EXISTS usage_v1_bot_day ON usage_v1(day, bot_id)",
                [],
            )
            .map_err(db_error)?;

        let manager = Self {
            store: Some(Arc::new(Mutex::new(connection))),
            ..Self::new(config)
        };
        manager.daily.lock().spent = manager.persisted_total()?;
        Ok(manager)
    }

    fn persisted_total(&self) -> Result<f64> {
        let Some(store) = &self.store else {
            return Ok(0.0);
        };
        store
            .lock()
            .query_row(
                "SELECT coalesce(sum(amount),0) FROM usage_v1 WHERE day=?1",
                [chrono::Utc::now().date_naive().to_string()],
                |row| row.get(0),
            )
            .map_err(db_error)
    }

    fn persist_settlement(&self, id: &str, actual: f64) -> Result<()> {
        if let Some(store) = &self.store {
            let changed = store
                .lock()
                .execute(
                    "UPDATE usage_v1 SET amount=?1, settled=1 WHERE id=?2 AND settled=0",
                    params![actual, id],
                )
                .map_err(db_error)?;
            if changed != 1 {
                return Err(GmError::BudgetExceeded(
                    "Usage reservation is missing or already settled".into(),
                ));
            }
        }
        Ok(())
    }

    pub fn scoped_job(&self) -> Self {
        Self {
            config: self.config.clone(),
            daily: self.daily.clone(),
            store: self.store.clone(),
            job: Arc::new(Mutex::new(JobLedger::default())),
        }
    }

    pub fn reserve(&self, amount: f64) -> Result<BudgetReservation> {
        self.reserve_attributed("muse", "chat", amount)
    }

    pub fn reserve_attributed(
        &self,
        bot_id: &str,
        source: &str,
        amount: f64,
    ) -> Result<BudgetReservation> {
        if !amount.is_finite() || amount < 0.0 {
            return Err(GmError::BudgetExceeded("Invalid request estimate".into()));
        }
        let bot_clean = sanitize_attribution(bot_id, "muse");
        let source_clean = sanitize_attribution(source, "chat");

        let mut daily = self.daily.lock();
        let mut job = self.job.lock();
        let day = chrono::Utc::now().date_naive();
        if daily.day != day && daily.reserved == 0.0 {
            daily.day = day;
            daily.spent = 0.0;
        }
        if daily.poisoned
            || (self.store.is_none()
                && daily.spent + daily.reserved + amount > self.config.daily_spend_cap_usd)
            || job.spent + job.reserved + amount > self.config.per_job_spend_cap_usd
        {
            return Err(GmError::BudgetExceeded(
                "Request would exceed the available daily or task budget".into(),
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        if let Some(store) = &self.store {
            let mut connection = store.lock();
            let tx = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(db_error)?;
            let total: f64 = tx
                .query_row(
                    "SELECT coalesce(sum(amount),0) FROM usage_v1 WHERE day=?1",
                    [day.to_string()],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            if total + amount > self.config.daily_spend_cap_usd {
                return Err(GmError::BudgetExceeded(
                    "Persistent daily budget is exhausted".into(),
                ));
            }
            tx.execute(
                "INSERT INTO usage_v1(id,day,amount,bot_id,source) VALUES (?1,?2,?3,?4,?5)",
                params![id, day.to_string(), amount, bot_clean, source_clean],
            )
            .map_err(db_error)?;
            tx.commit().map_err(db_error)?;
        }
        daily.entries.insert(
            id.clone(),
            MemoryUsageEntry {
                day,
                bot_id: bot_clean,
                source: source_clean,
                amount,
            },
        );
        daily.reserved += amount;
        job.reserved += amount;
        Ok(BudgetReservation {
            manager: self.clone(),
            amount,
            settled: false,
            id,
        })
    }

    pub fn record_spend(&self, cost_usd: f64) -> Result<()> {
        self.record_spend_attributed("muse", "chat", cost_usd)
    }

    pub fn record_spend_attributed(&self, bot_id: &str, source: &str, cost_usd: f64) -> Result<()> {
        self.reserve_attributed(bot_id, source, cost_usd)?
            .settle(cost_usd)
    }

    pub fn usage_breakdown(&self, days: u32) -> Result<UsageBreakdown> {
        let window_days = days.clamp(1, 90);
        let today = chrono::Utc::now().date_naive();
        let since_day = today - chrono::Duration::days((window_days - 1) as i64);
        let weekly_since = today - chrono::Duration::days(6);

        let mut by_bot_map: BTreeMap<String, f64> = BTreeMap::new();
        let mut by_source_map: BTreeMap<String, f64> = BTreeMap::new();
        let mut by_day_map: BTreeMap<String, f64> = BTreeMap::new();
        let mut daily_spent = 0.0;
        let mut weekly_spent = 0.0;

        if let Some(store) = &self.store {
            let conn = store.lock();
            let since_str = since_day.to_string();
            let weekly_str = weekly_since.to_string();
            let today_str = today.to_string();

            daily_spent = conn
                .query_row(
                    "SELECT coalesce(sum(amount),0) FROM usage_v1 WHERE day=?1",
                    [&today_str],
                    |row| row.get(0),
                )
                .map_err(db_error)?;

            weekly_spent = conn
                .query_row(
                    "SELECT coalesce(sum(amount),0) FROM usage_v1 WHERE day>=?1",
                    [&weekly_str],
                    |row| row.get(0),
                )
                .map_err(db_error)?;

            {
                let mut stmt = conn
                    .prepare(
                        "SELECT bot_id, coalesce(sum(amount),0) FROM usage_v1 WHERE day>=?1 GROUP BY bot_id",
                    )
                    .map_err(db_error)?;
                let mut rows = stmt.query([&since_str]).map_err(db_error)?;
                while let Some(row) = rows.next().map_err(db_error)? {
                    let key: String = row.get(0).map_err(db_error)?;
                    let amt: f64 = row.get(1).map_err(db_error)?;
                    if amt > 0.0 {
                        *by_bot_map.entry(key).or_insert(0.0) += amt;
                    }
                }
            }

            {
                let mut stmt = conn
                    .prepare(
                        "SELECT source, coalesce(sum(amount),0) FROM usage_v1 WHERE day>=?1 GROUP BY source",
                    )
                    .map_err(db_error)?;
                let mut rows = stmt.query([&since_str]).map_err(db_error)?;
                while let Some(row) = rows.next().map_err(db_error)? {
                    let key: String = row.get(0).map_err(db_error)?;
                    let amt: f64 = row.get(1).map_err(db_error)?;
                    if amt > 0.0 {
                        *by_source_map.entry(key).or_insert(0.0) += amt;
                    }
                }
            }

            {
                let mut stmt = conn
                    .prepare(
                        "SELECT day, coalesce(sum(amount),0) FROM usage_v1 WHERE day>=?1 GROUP BY day ORDER BY day ASC",
                    )
                    .map_err(db_error)?;
                let mut rows = stmt.query([&since_str]).map_err(db_error)?;
                while let Some(row) = rows.next().map_err(db_error)? {
                    let day_key: String = row.get(0).map_err(db_error)?;
                    let amt: f64 = row.get(1).map_err(db_error)?;
                    if amt > 0.0 {
                        by_day_map.insert(day_key, amt);
                    }
                }
            }
        } else {
            let daily = self.daily.lock();
            for entry in daily.entries.values() {
                if entry.amount <= 0.0 {
                    continue;
                }
                if entry.day == today {
                    daily_spent += entry.amount;
                }
                if entry.day >= weekly_since {
                    weekly_spent += entry.amount;
                }
                if entry.day >= since_day {
                    *by_bot_map.entry(entry.bot_id.clone()).or_insert(0.0) += entry.amount;
                    *by_source_map.entry(entry.source.clone()).or_insert(0.0) += entry.amount;
                    *by_day_map.entry(entry.day.to_string()).or_insert(0.0) += entry.amount;
                }
            }
        }

        let mut by_bot: Vec<UsageBucket> = by_bot_map
            .into_iter()
            .map(|(key, amount_usd)| UsageBucket { key, amount_usd })
            .collect();
        by_bot.sort_by(|a, b| b.amount_usd.total_cmp(&a.amount_usd));

        let mut by_source: Vec<UsageBucket> = by_source_map
            .into_iter()
            .map(|(key, amount_usd)| UsageBucket { key, amount_usd })
            .collect();
        by_source.sort_by(|a, b| b.amount_usd.total_cmp(&a.amount_usd));

        let daily_series: Vec<UsageDailyPoint> = by_day_map
            .into_iter()
            .map(|(day, amount_usd)| UsageDailyPoint { day, amount_usd })
            .collect();

        Ok(UsageBreakdown {
            daily_spent_usd: daily_spent,
            weekly_spent_usd: weekly_spent,
            daily_cap_usd: self.config.daily_spend_cap_usd,
            per_job_cap_usd: self.config.per_job_spend_cap_usd,
            max_daily_notifications: self.config.max_daily_notifications,
            by_bot,
            by_source,
            daily_series,
        })
    }

    pub fn reset_job_spend(&self) {
        let mut job = self.job.lock();
        if job.reserved == 0.0 {
            job.spent = 0.0;
        }
    }

    pub fn get_daily_spent(&self) -> f64 {
        if self.store.is_some() {
            self.persisted_total()
                .unwrap_or(self.config.daily_spend_cap_usd)
        } else {
            self.daily.lock().spent
        }
    }

    pub fn get_job_spent(&self) -> f64 {
        self.job.lock().spent
    }

    pub fn config(&self) -> &BudgetConfig {
        &self.config
    }
}

fn sanitize_attribution(raw: &str, fallback: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

fn db_error(e: rusqlite::Error) -> GmError {
    GmError::BudgetExceeded(format!("Usage ledger is unavailable: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_budget_enforcement() {
        let config = BudgetConfig {
            daily_spend_cap_usd: 1.0,
            per_job_spend_cap_usd: 0.5,
            max_daily_notifications: 3,
        };
        let manager = BudgetManager::new(config);
        assert!(manager.record_spend(0.3).is_ok());
        assert_eq!(manager.get_job_spent(), 0.3);

        // Exceed job spend
        assert!(manager.record_spend(0.3).is_err());

        manager.reset_job_spend();
        assert_eq!(manager.get_job_spent(), 0.0);
        assert_eq!(manager.get_daily_spent(), 0.3);

        assert!(manager.record_spend(0.4).is_ok());
        assert_eq!(manager.get_daily_spent(), 0.7);

        // Exceed daily spend
        assert!(manager.record_spend(0.4).is_err());
    }
}

#[cfg(test)]
mod reservation_tests {
    use super::*;
    #[test]
    fn parallel_jobs_share_daily_admission_but_not_job_limits() {
        let manager = BudgetManager::new(BudgetConfig {
            daily_spend_cap_usd: 1.0,
            per_job_spend_cap_usd: 0.75,
            max_daily_notifications: 3,
        });
        let first = manager.scoped_job();
        let second = manager.scoped_job();
        let reservation = first.reserve(0.6).unwrap();
        assert!(second.reserve(0.6).is_err());
        reservation.settle(0.2).unwrap();
        second.reserve(0.6).unwrap().settle(0.3).unwrap();
        assert_eq!(first.get_job_spent(), 0.2);
        assert_eq!(second.get_job_spent(), 0.3);
        assert_eq!(manager.get_daily_spent(), 0.5);
    }
    #[test]
    fn cancelled_request_retains_estimated_exposure_and_invalid_costs_fail() {
        let manager = BudgetManager::new(BudgetConfig {
            daily_spend_cap_usd: 1.0,
            per_job_spend_cap_usd: 1.0,
            max_daily_notifications: 3,
        });
        drop(manager.reserve(0.6).unwrap());
        assert_eq!(manager.get_daily_spent(), 0.6);
        assert!(manager.reserve(0.5).is_err());
        assert!(manager.record_spend(f64::NAN).is_err());
        assert!(manager.record_spend(-1.0).is_err());
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    #[test]
    fn independent_connections_and_restart_cannot_overspend() {
        let dir = std::env::temp_dir().join(format!("gm-usage-{}", uuid::Uuid::new_v4()));
        let config = BudgetConfig {
            daily_spend_cap_usd: 1.0,
            per_job_spend_cap_usd: 1.0,
            max_daily_notifications: 3,
        };
        let first = BudgetManager::open(config.clone(), &dir).unwrap();
        let second = BudgetManager::open(config.clone(), &dir).unwrap();
        let request = first.reserve(0.7).unwrap();
        assert!(second.reserve(0.4).is_err());
        request.settle(0.2).unwrap();
        let pending = second.reserve(0.6).unwrap();
        // Simulate a process dying without running reservation destructors.
        std::mem::forget(pending);
        drop(first);
        drop(second);
        let restart = BudgetManager::open(config, &dir).unwrap();
        assert!((restart.get_daily_spent() - 0.8).abs() < 1e-9);
        assert!(restart.reserve(0.3).is_err());
        let request = restart.reserve(0.1).unwrap();
        request.release();
        assert!((restart.get_daily_spent() - 0.8).abs() < 1e-9);
        drop(restart);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn attributed_spend_breakdown_and_legacy_schema_migration() {
        let dir = std::env::temp_dir().join(format!("gm-usage-attr-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // Seed a legacy pre-attribution usage.sqlite3 database cleanly.
        {
            let conn = Connection::open(dir.join("usage.sqlite3")).unwrap();
            conn.execute_batch(
                "CREATE TABLE usage_v1 (
                    id TEXT PRIMARY KEY,
                    day TEXT NOT NULL,
                    amount REAL NOT NULL CHECK(amount >= 0),
                    settled INTEGER NOT NULL DEFAULT 0
                );",
            )
            .unwrap();
            let today = chrono::Utc::now().date_naive().to_string();
            conn.execute(
                "INSERT INTO usage_v1(id, day, amount, settled) VALUES ('legacy-1', ?1, 0.25, 1)",
                [today],
            )
            .unwrap();
        }

        let cfg = BudgetConfig {
            daily_spend_cap_usd: 10.0,
            per_job_spend_cap_usd: 5.0,
            max_daily_notifications: 3,
        };
        let mgr = BudgetManager::open(cfg, &dir).unwrap();
        mgr.record_spend_attributed("coder", "queue", 0.50).unwrap();
        mgr.record_spend_attributed("researcher", "routine", 0.30)
            .unwrap();
        mgr.record_spend_attributed("muse", "chat", 0.15).unwrap();

        let breakdown = mgr.usage_breakdown(7).unwrap();
        assert!((breakdown.daily_spent_usd - 1.20).abs() < 1e-6);
        assert!((breakdown.weekly_spent_usd - 1.20).abs() < 1e-6);
        assert_eq!(breakdown.by_bot.len(), 3);
        assert_eq!(breakdown.by_bot[0].key, "coder");
        assert!((breakdown.by_bot[0].amount_usd - 0.50).abs() < 1e-6);

        let muse_entry = breakdown.by_bot.iter().find(|b| b.key == "muse").unwrap();
        assert!((muse_entry.amount_usd - 0.40).abs() < 1e-6);

        let queue_src = breakdown
            .by_source
            .iter()
            .find(|s| s.key == "queue")
            .unwrap();
        assert!((queue_src.amount_usd - 0.50).abs() < 1e-6);

        drop(mgr);
        let _ = std::fs::remove_dir_all(dir);
    }
}
