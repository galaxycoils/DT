//! Periodic HeartbeatWorker (`T4.6`): cheap file/goal hash pre-check before touching the LLM,
//! budget & `heartbeat_daily_token_cap` enforcement, pause flag and `KillSwitch` checks,
//! and `QuietGate` filtering before emitting goal check-ins or Feed editorial posts.

use crate::feed::{FeedCategory, FeedPost, FeedStore};
use crate::goal_workspace::GoalWorkspaceManager;
use crate::proactive_prefs::ProactivePrefsStore;
use crate::quiet_gate::QuietGate;
use chrono::{DateTime, NaiveDate, Utc};
use gm_core::config::MuseConfig;
use gm_core::error::Result;
use gm_core::muse_paths::MuseRuntimePaths;
use gm_llm::{BudgetManager, ChatMessage, HybridLlmClient, LlmRouter, TaskType};
use gm_muse::shared_files::SharedFileStore;
use gm_safety::KillSwitch;
use hex::encode as hex_encode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatState {
    pub last_hash: String,
    pub day: NaiveDate,
    pub tokens_used_today: u64,
    pub last_outcome: String,
}

impl Default for HeartbeatState {
    fn default() -> Self {
        Self {
            last_hash: String::new(),
            day: Utc::now().date_naive(),
            tokens_used_today: 0,
            last_outcome: "init".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum HeartbeatOutcome {
    Disabled,
    Paused,
    KillSwitchActive,
    NothingToDo,
    SuppressedQuietGate {
        reason: String,
    },
    TokenCapReached {
        used: u64,
        cap: u64,
    },
    BudgetExhausted {
        reason: String,
    },
    Dispatched {
        tokens_used: u64,
        due_goals_count: usize,
        feed_post_id: Option<String>,
    },
}

#[derive(Clone)]
pub struct HeartbeatWorker {
    paths: MuseRuntimePaths,
    muse_config: MuseConfig,
    quiet_gate: QuietGate,
    budget: Arc<BudgetManager>,
    kill_switch: KillSwitch,
    paused: Arc<AtomicBool>,
    feed_store: Option<FeedStore>,
    llm: Option<(Arc<HybridLlmClient>, LlmRouter)>,
}

impl HeartbeatWorker {
    pub fn new(
        paths: MuseRuntimePaths,
        muse_config: MuseConfig,
        quiet_gate: QuietGate,
        budget: Arc<BudgetManager>,
        kill_switch: KillSwitch,
    ) -> Self {
        Self {
            paths,
            muse_config,
            quiet_gate,
            budget,
            kill_switch,
            paused: Arc::new(AtomicBool::new(false)),
            feed_store: None,
            llm: None,
        }
    }

    pub fn with_feed_store(mut self, feed: FeedStore) -> Self {
        self.feed_store = Some(feed);
        self
    }

    pub fn with_llm(mut self, client: Arc<HybridLlmClient>, router: LlmRouter) -> Self {
        self.llm = Some((client, router));
        self
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    pub fn state_path(&self) -> PathBuf {
        self.paths
            .bot_workspace_dir()
            .unwrap_or_else(|| self.paths.storage_dir().join("workspace"))
            .join(".heartbeat_state.json")
    }

    fn load_state(&self, now: DateTime<Utc>) -> Result<HeartbeatState> {
        let path = self.state_path();
        let mut st = match SharedFileStore::global().read_text_with_sha256(&path)? {
            Some((raw, _)) if !raw.trim().is_empty() => {
                serde_json::from_str::<HeartbeatState>(&raw).unwrap_or_default()
            }
            _ => HeartbeatState::default(),
        };
        if st.day != now.date_naive() {
            st.day = now.date_naive();
            st.tokens_used_today = 0;
        }
        Ok(st)
    }

    fn save_state(&self, state: &HeartbeatState) -> Result<()> {
        let json = serde_json::to_string_pretty(state)?;
        SharedFileStore::global().write_exact_sync(self.state_path(), None, &json)?;
        Ok(())
    }

    fn compute_precheck_hash(
        &self,
        due_goal_ids: &[String],
        due_routine_ids: &[String],
    ) -> Result<String> {
        let mut hasher = Sha256::new();
        if let Some(hb) = self.paths.heartbeat_md()
            && let Some((text, _)) = SharedFileStore::global().read_text_with_sha256(hb)?
        {
            hasher.update(b"HEARTBEAT:");
            hasher.update(text.as_bytes());
        }
        if let Some((prefs, _)) = SharedFileStore::global()
            .read_text_with_sha256(self.paths.proactive_preferences_md())?
        {
            hasher.update(b"PREFS:");
            hasher.update(prefs.as_bytes());
        }
        if let Some((fb, _)) =
            SharedFileStore::global().read_text_with_sha256(self.paths.feedback_md())?
        {
            hasher.update(b"FEEDBACK:");
            hasher.update(fb.as_bytes());
        }
        if let Some((tz, _)) =
            SharedFileStore::global().read_text_with_sha256(self.paths.timezones_yaml())?
        {
            hasher.update(b"TZ:");
            hasher.update(tz.as_bytes());
        }
        for g in due_goal_ids {
            hasher.update(b"GOAL:");
            hasher.update(g.as_bytes());
        }
        for r in due_routine_ids {
            hasher.update(b"ROUTINE:");
            hasher.update(r.as_bytes());
        }
        Ok(hex_encode(hasher.finalize()))
    }

    pub async fn tick(
        &self,
        now: DateTime<Utc>,
        due_routine_ids: &[String],
    ) -> Result<HeartbeatOutcome> {
        if !self.muse_config.enable_heartbeat {
            return Ok(HeartbeatOutcome::Disabled);
        }
        if self.is_paused() {
            return Ok(HeartbeatOutcome::Paused);
        }
        if self.kill_switch.is_triggered() {
            return Ok(HeartbeatOutcome::KillSwitchActive);
        }

        // Sync latest preferences & quiet hours into QuietGate
        let prefs_store = ProactivePrefsStore::new(self.paths.clone());
        let _ = prefs_store.sync_with_quiet_gate(&self.quiet_gate);

        let goals_mgr = GoalWorkspaceManager::new(self.paths.clone());
        let due_goals = goals_mgr.due_goals(now)?;
        let due_goal_ids: Vec<String> = due_goals.iter().map(|g| g.id.clone()).collect();

        let state_hash = self.compute_precheck_hash(&due_goal_ids, due_routine_ids)?;
        let mut state = self.load_state(now)?;

        // 1. Cheap pre-check: if nothing changed and nothing is due, skip LLM completely
        if !state.last_hash.is_empty()
            && state.last_hash == state_hash
            && due_goals.is_empty()
            && due_routine_ids.is_empty()
        {
            state.last_outcome = "nothing_to_do".into();
            self.save_state(&state)?;
            return Ok(HeartbeatOutcome::NothingToDo);
        }

        // 2. Enforce QuietGate (quiet hours / opt-outs / snooze / daily cap) before LLM
        let dedup_key = format!("hb_{}_{}", now.date_naive(), &state_hash[..12]);
        let (allowed, suppressed_reason) =
            self.quiet_gate
                .can_deliver("heartbeat", &dedup_key, false, now);
        if !allowed {
            let reason = suppressed_reason
                .unwrap_or("Suppressed by QuietGate")
                .to_string();
            state.last_outcome = format!("suppressed: {reason}");
            self.save_state(&state)?;
            return Ok(HeartbeatOutcome::SuppressedQuietGate { reason });
        }

        // 3. Enforce heartbeat_daily_token_cap + BudgetManager::reserve before LLM invocation
        const EST_TOKENS_PER_TICK: u64 = 1_500;
        const EST_COST_PER_TICK_USD: f64 = 0.005;
        if state.tokens_used_today + EST_TOKENS_PER_TICK
            > self.muse_config.heartbeat_daily_token_cap
        {
            state.last_outcome = "token_cap_reached".into();
            self.save_state(&state)?;
            return Ok(HeartbeatOutcome::TokenCapReached {
                used: state.tokens_used_today,
                cap: self.muse_config.heartbeat_daily_token_cap,
            });
        }

        let reservation = match self.budget.reserve(EST_COST_PER_TICK_USD) {
            Ok(r) => r,
            Err(err) => {
                state.last_outcome = format!("budget_exhausted: {err}");
                self.save_state(&state)?;
                return Ok(HeartbeatOutcome::BudgetExhausted {
                    reason: err.to_string(),
                });
            }
        };

        // 4. Compose heartbeat summary (LLM-enriched when wired, deterministic offline fallback otherwise)
        let mut tokens_used = EST_TOKENS_PER_TICK;
        let summary_body = if let Some((ref client, ref router)) = self.llm {
            // Release synthetic headroom probe so `execute_routed` can perform exact admission/settlement.
            reservation.release();
            let (primary, fallback) = router.route(TaskType::ProactiveTriage);
            let goal_lines = if due_goals.is_empty() || self.quiet_gate.is_topic_opted_out("goals")
            {
                "No goals currently due or goal check-ins opted out.".to_string()
            } else {
                due_goals
                    .iter()
                    .map(|g| format!("- {} ({})", g.title, g.slug))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let prompt = format!(
                "Heartbeat check-in ({}). Due goals:\n{}\nDue routines: {}\nWrite a concise 2-sentence status update for the Feed.",
                now.format("%Y-%m-%d %H:%M UTC"),
                goal_lines,
                due_routine_ids.len()
            );
            let msgs = vec![
                ChatMessage::text(
                    "system",
                    "You are Muse running a brief background heartbeat.",
                ),
                ChatMessage::text("user", prompt),
            ];
            match client
                .execute_routed(&primary, fallback.as_ref(), &msgs)
                .await
            {
                Ok(resp) => {
                    let used = u64::from(resp.prompt_tokens) + u64::from(resp.completion_tokens);
                    if used > 0 {
                        tokens_used = used;
                    }
                    resp.content
                }
                Err(err) => {
                    state.last_outcome = format!("budget_or_llm_failed: {err}");
                    self.save_state(&state)?;
                    return Ok(HeartbeatOutcome::BudgetExhausted {
                        reason: err.to_string(),
                    });
                }
            }
        } else {
            reservation.release();
            format!(
                "Heartbeat checked {} due goal(s) and {} due routine(s).",
                due_goals.len(),
                due_routine_ids.len()
            )
        };

        // Update checked goals' next_check_at
        for g in &due_goals {
            let _ = goals_mgr.mark_checked(&g.id, now + chrono::Duration::hours(24));
        }

        // Emit Editorial post to FeedStore if attached
        let feed_post_id = if let Some(ref feed) = self.feed_store {
            let post_id = format!("hb-{}", Uuid::new_v4());
            feed.upsert_post(&FeedPost {
                id: post_id.clone(),
                kicker: "Heartbeat".into(),
                category: FeedCategory::Editorial,
                title: format!("Heartbeat Check-in ({})", now.format("%Y-%m-%d %H:%M UTC")),
                body: summary_body,
                signal_citations: Vec::new(),
                rating: None,
                user_prompt: None,
                created_at: now,
            })?;
            Some(post_id)
        } else {
            None
        };

        self.quiet_gate.record_delivery_at(&dedup_key, now);
        state.last_hash = self.compute_precheck_hash(&[], due_routine_ids)?;
        state.tokens_used_today += tokens_used;
        state.last_outcome = "dispatched".into();
        self.save_state(&state)?;

        Ok(HeartbeatOutcome::Dispatched {
            tokens_used,
            due_goals_count: due_goals.len(),
            feed_post_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quiet_gate::QuietGateConfig;
    use async_trait::async_trait;
    use chrono::TimeZone;
    use gm_core::config::{BudgetConfig, ProvidersConfig};
    use gm_llm::{ChatCompletionResponse, LlmBackend, ModelTarget};
    use std::sync::atomic::AtomicUsize;

    struct CountingMockBackend {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl LlmBackend for CountingMockBackend {
        async fn complete(
            &self,
            target: &ModelTarget,
            _messages: &[ChatMessage],
        ) -> gm_core::error::Result<ChatCompletionResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ChatCompletionResponse {
                id: "hb-mock".into(),
                content: "All due goals reviewed calmly.".into(),
                model: target.model.clone(),
                prompt_tokens: 1_000,
                completion_tokens: 500,
                estimated_cost_usd: 0.002,
            })
        }

        fn is_mock(&self) -> bool {
            true
        }
    }

    fn counting_llm(
        budget: Arc<BudgetManager>,
    ) -> (Arc<AtomicUsize>, Arc<HybridLlmClient>, LlmRouter) {
        let calls = Arc::new(AtomicUsize::new(0));
        let backend = Arc::new(CountingMockBackend {
            calls: Arc::clone(&calls),
        });
        let client = Arc::new(HybridLlmClient::new(backend, budget));
        let router = LlmRouter::new(ProvidersConfig::default());
        (calls, client, router)
    }

    #[tokio::test]
    async fn heartbeat_skips_when_unchanged_suppresses_in_quiet_hours_and_enforces_token_cap() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path();
        let bot_ws = storage.join("bots/muse/workspace");
        gm_core::storage::ensure_private_dir(&bot_ws).unwrap();
        let paths = MuseRuntimePaths::for_bot(storage, storage.join("bots/muse"), "muse");

        SharedFileStore::global()
            .write_exact_sync(
                paths.heartbeat_md().unwrap(),
                None,
                "# HEARTBEAT.md\n- Check goals\n",
            )
            .unwrap();
        SharedFileStore::global()
            .write_exact_sync(
                paths.proactive_preferences_md(),
                None,
                "# PROACTIVE_PREFERENCES.md\n\n## When\n- Quiet hours: 22:00-08:00\n",
            )
            .unwrap();

        let feed = FeedStore::in_memory().unwrap();
        let budget = Arc::new(BudgetManager::new(BudgetConfig::default()));
        let gate = QuietGate::new(QuietGateConfig::default());
        let ks = KillSwitch::new();

        let muse = MuseConfig {
            enable_heartbeat: true,
            heartbeat_daily_token_cap: 3_000,
            ..Default::default()
        };

        let (calls, client, router) = counting_llm(Arc::clone(&budget));
        let worker = HeartbeatWorker::new(
            paths.clone(),
            muse.clone(),
            gate.clone(),
            budget.clone(),
            ks.clone(),
        )
        .with_feed_store(feed.clone())
        .with_llm(client, router);

        // 1. Quiet hours (02:00 UTC) suppress the run before touching LLM
        let midnight = Utc.with_ymd_and_hms(2026, 11, 3, 2, 0, 0).unwrap();
        let q_out = worker.tick(midnight, &[]).await.unwrap();
        assert!(matches!(
            q_out,
            HeartbeatOutcome::SuppressedQuietGate { .. }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        // 2. Daytime (14:00 UTC) runs once because no prior state hash existed (1500 tokens)
        let daytime = Utc.with_ymd_and_hms(2026, 11, 3, 14, 0, 0).unwrap();
        let first = worker.tick(daytime, &[]).await.unwrap();
        assert!(matches!(first, HeartbeatOutcome::Dispatched { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(feed.list_posts(10).unwrap().len(), 1);

        // 3. Re-running with nothing changed returns NothingToDo (0 new mock LLM calls)
        let no_change = worker
            .tick(daytime + chrono::Duration::minutes(10), &[])
            .await
            .unwrap();
        assert_eq!(no_change, HeartbeatOutcome::NothingToDo);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // 4. Token cap stops execution when cap reached (2 ticks * 1500 = 3000)
        SharedFileStore::global()
            .write_exact_sync(
                paths.heartbeat_md().unwrap(),
                None,
                "# HEARTBEAT.md\n- Check goals\n- Modified line 1\n",
            )
            .unwrap();
        let second = worker
            .tick(daytime + chrono::Duration::minutes(20), &[])
            .await
            .unwrap();
        assert!(matches!(second, HeartbeatOutcome::Dispatched { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // After 3000 tokens used today, next change hits the 3000 token cap with 0 additional LLM calls
        SharedFileStore::global()
            .write_exact_sync(
                paths.heartbeat_md().unwrap(),
                None,
                "# HEARTBEAT.md\n- Check goals\n- Modified line 2\n",
            )
            .unwrap();
        let capped = worker
            .tick(daytime + chrono::Duration::minutes(30), &[])
            .await
            .unwrap();
        assert!(matches!(capped, HeartbeatOutcome::TokenCapReached { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
