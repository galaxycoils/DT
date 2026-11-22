use crate::bridges::{BridgeInboundMessage, BridgeRouter, verify_slack_signature};
use crate::spaces::{SPACE_TOKEN_TTL_SECS, SpaceKind};
use crate::supervisor::OpenBotSupervisor;
use axum::Router;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::header::{self, HeaderName, HeaderValue};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use gm_core::error::GmError;
use gm_core::ipc::{DaemonEvent, QueueControlAction};
use gm_goals::{
    FeedCategory, FeedPost, Goal, IdeaSource, ProactiveIdea, TimezonesConfig,
    generate_ideas_with_llm,
};
use gm_memory::{MAX_IMPORT_BYTES, import_memory_portable_with_stores};
use gm_muse::shared_files::{SharedFileStore, SharedUpdateOutcome};
use gm_muse::skills::{SkillRegistry, SkillYaml};
use gm_platform::MockPlatform;
use gm_safety::AuditCategory;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

const COMPANION_UI_HTML: &str = include_str!("companion_ui.html");
const COMPANION_UI_CSS: &str = include_str!("companion_ui.css");
const COMPANION_UI_JS: &str = include_str!("companion_ui.js");

struct ApiErr(StatusCode, String, Option<serde_json::Value>);

impl ApiErr {
    fn msg(status: StatusCode, message: impl Into<String>) -> Self {
        Self(status, message.into(), None)
    }

    fn conflict(message: impl Into<String>, current_sha256: Option<String>) -> Self {
        Self(
            StatusCode::CONFLICT,
            message.into(),
            Some(serde_json::json!({ "current_sha256": current_sha256 })),
        )
    }
}

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        let mut obj = serde_json::Map::new();
        obj.insert("error".into(), serde_json::Value::String(self.1));
        if let Some(serde_json::Value::Object(extra)) = self.2 {
            for (k, v) in extra {
                obj.insert(k, v);
            }
        }
        (self.0, Json(serde_json::Value::Object(obj))).into_response()
    }
}

impl From<StatusCode> for ApiErr {
    fn from(code: StatusCode) -> Self {
        Self::msg(code, code.canonical_reason().unwrap_or("error"))
    }
}

fn map_gm_error(err: GmError) -> ApiErr {
    match err {
        GmError::SafetyViolation(msg) => {
            if msg.contains("RULE-TIER-003") || msg.contains("RULE-FORGET-001") {
                ApiErr::msg(StatusCode::FORBIDDEN, msg)
            } else {
                ApiErr::msg(StatusCode::BAD_REQUEST, msg)
            }
        }
        GmError::Config(msg) => {
            if msg.contains("Optimistic concurrency conflict") {
                ApiErr::msg(StatusCode::CONFLICT, msg)
            } else if msg.contains("not found")
                || msg.contains("does not exist")
                || msg.contains("Unknown ")
            {
                ApiErr::msg(StatusCode::NOT_FOUND, msg)
            } else {
                ApiErr::msg(StatusCode::BAD_REQUEST, msg)
            }
        }
        other => ApiErr::msg(StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
    }
}

#[async_trait::async_trait]
pub trait CompanionUpdates: Send + Sync {
    async fn check(&self) -> gm_core::error::Result<serde_json::Value>;
    async fn download_verified(&self) -> gm_core::error::Result<serde_json::Value>;
}

#[async_trait::async_trait]
pub trait CompanionComponents: Send + Sync {
    async fn list(&self) -> gm_core::error::Result<serde_json::Value>;
    async fn install(&self, id: &str) -> gm_core::error::Result<serde_json::Value>;
    async fn verify(&self, id: &str) -> gm_core::error::Result<serde_json::Value>;
}

#[derive(Clone)]
pub struct WorkspaceSupervisor(pub OpenBotSupervisor);

struct ActiveSupervisor(OpenBotSupervisor);

#[async_trait::async_trait]
impl axum::extract::FromRequestParts<CompanionWebState> for ActiveSupervisor {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &CompanionWebState,
    ) -> std::result::Result<Self, Self::Rejection> {
        if let Some(WorkspaceSupervisor(sup)) = parts.extensions.get::<WorkspaceSupervisor>() {
            Ok(Self(sup.clone()))
        } else {
            Ok(Self(state.supervisor.clone()))
        }
    }
}

#[derive(Clone)]
pub struct CompanionWebState {
    pub supervisor: OpenBotSupervisor,
    pub bridge_router: BridgeRouter,
    pub bearer_token: Option<String>,
    pub slack_signing_secret: Option<String>,
    pub interactions: crate::interactions::SharedInteractionState,
    pub shared_config: Arc<parking_lot::RwLock<gm_core::config::GmConfig>>,
    pub config_path: Option<PathBuf>,
    pub budget: Option<Arc<gm_llm::BudgetManager>>,
    pub llm_client: Option<Arc<gm_llm::HybridLlmClient>>,
    pub router: Option<gm_llm::LlmRouter>,
    pub kill_switch: Option<gm_safety::KillSwitch>,
    pub platform: Option<Arc<dyn gm_platform::Platform>>,
    pub updates: Option<Arc<dyn CompanionUpdates>>,
    pub components: Option<Arc<dyn CompanionComponents>>,
    pub voice_manager: crate::voice::VoiceSessionManager,
    pub hosted_plane: Option<crate::hosted::HostedPlaneBackend>,
    pub workspace_supervisors:
        Arc<parking_lot::RwLock<std::collections::HashMap<String, OpenBotSupervisor>>>,
    pub sanitized_doctor: Option<Arc<parking_lot::RwLock<Option<serde_json::Value>>>>,
    pub web_dist: Option<PathBuf>,
}

impl CompanionWebState {
    pub async fn supervisor_for_workspace(
        &self,
        ws_id: &str,
    ) -> gm_core::error::Result<OpenBotSupervisor> {
        let clean = crate::hosted::validate_workspace_slug(ws_id)?;
        let Some(ref plane) = self.hosted_plane else {
            return Ok(self.supervisor.clone());
        };
        if let Some(existing) = self.workspace_supervisors.read().get(&clean).cloned() {
            return Ok(existing);
        }
        let Some(record) = plane.get_workspace(&clean).await? else {
            let expected_local =
                std::env::var("GM_WORKSPACE_ID").unwrap_or_else(|_| "default".into());
            if clean == expected_local || clean == "local" {
                return Ok(self.supervisor.clone());
            }
            return Err(GmError::Config(format!("Unknown workspace '{clean}'")));
        };
        let mut ws_cfg = self.shared_config.read().clone();
        ws_cfg.storage_dir = PathBuf::from(&record.worker_dir);
        ws_cfg.openbot.bots_dir = ws_cfg.storage_dir.join("bots");
        let audit = Arc::new(gm_safety::AuditLogger::with_file(
            ws_cfg.storage_dir.join("audit.jsonl"),
        )?);
        let secret_backend: Arc<dyn crate::vault::SecretBackend> = match plane {
            crate::hosted::HostedPlaneBackend::Sqlite(sqlite) => {
                Arc::new(sqlite.tenant_secret_backend(&clean)?)
            }
            crate::hosted::HostedPlaneBackend::Postgres(_) => {
                Arc::new(crate::vault::InMemorySecretBackend::default())
            }
        };
        let ws_sup = OpenBotSupervisor::open_with_secret_backend(
            ws_cfg,
            audit,
            self.supervisor.daemon_event_sender(),
            secret_backend,
        )?;
        let mut map = self.workspace_supervisors.write();
        Ok(map.entry(clean).or_insert(ws_sup).clone())
    }
}

#[derive(Debug, Deserialize)]
pub struct EnqueueRequest {
    pub bot_id: Option<String>,
    pub channel_id: Option<String>,
    pub instruction: String,
    #[serde(default)]
    pub priority: i32,
}

#[derive(Debug, Deserialize)]
pub struct WheelResumeBody {
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReassignBody {
    pub target_bot_id: String,
}

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    pub category: Option<String>,
    pub bot_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct ChannelPostBody {
    pub author: Option<String>,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateChannelBody {
    pub id: String,
    #[serde(default)]
    pub topic: String,
    pub owner_bot_id: Option<String>,
    #[serde(default)]
    pub participants: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AgUiRunRequest {
    pub bot_id: Option<String>,
    pub instruction: String,
}

#[derive(Debug, Deserialize)]
pub struct ActivityQuery {
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct CreateActivityBody {
    pub id: Option<String>,
    pub kicker: Option<String>,
    pub category: Option<String>,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub signal_citations: Vec<String>,
    pub user_prompt: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ActivityPromptBody {
    pub prompt: String,
}

#[derive(Debug, Deserialize)]
pub struct RateActivityBody {
    pub rating: i32,
}

#[derive(Debug, Deserialize)]
pub struct CreateIdeaBody {
    pub title: Option<String>,
    pub description: Option<String>,
    pub action_prompt: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RunIdeaBody {
    pub bot_id: Option<String>,
    pub channel_id: Option<String>,
    pub instruction: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateGoalBody {
    pub parent_slug: Option<String>,
    pub parent_goal_id: Option<String>,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub category: Option<String>,
    #[serde(default)]
    pub stop_conditions: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct PatchGoalBody {
    pub slug: Option<String>,
    pub id: Option<String>,
    pub action: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub stop_conditions: Option<Vec<String>>,
    pub track_brief_schedule: Option<String>,
    pub verified: Option<bool>,
}

#[derive(Debug, Serialize)]
struct GoalWithFiles {
    #[serde(flatten)]
    goal: Goal,
    files: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSpaceBody {
    pub slug: String,
    pub title: Option<String>,
    pub kind: Option<String>,
    pub html: Option<String>,
    pub content: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PublishSpaceBody {
    pub confirmed: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CatalogQuery {
    pub q: Option<String>,
    pub load: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateCustomSkillBody {
    pub name: String,
    pub title: String,
    pub description: String,
    pub category: Option<String>,
    pub include_in_prompt: Option<bool>,
    pub body_markdown: String,
}

#[derive(Debug, Deserialize)]
pub struct PutIdentityFileBody {
    pub content: String,
    pub expected_sha256: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProactivePrefsBody {
    pub add_tell: Option<String>,
    pub add_opt_out: Option<String>,
    pub remove_opt_out: Option<String>,
    pub quiet_start_hour: Option<u32>,
    pub quiet_end_hour: Option<u32>,
    pub home_tz: Option<String>,
    pub work_tz: Option<String>,
    pub feedback_note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ImportMemoryBody {
    pub input: String,
    #[serde(default)]
    pub apply: bool,
    pub report_sha256: Option<String>,
    pub bot_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ExplainMemoryBody {
    #[serde(alias = "query")]
    pub target: String,
}

#[derive(Debug, Deserialize)]
pub struct ForgetPlanBody {
    pub request: String,
    pub bot_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ForgetConfirmBody {
    pub plan_id: String,
    #[serde(default)]
    pub confirmed: bool,
    pub verification_query: Option<String>,
    pub bot_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PutDiscretionBody {
    #[serde(default)]
    pub rules: Vec<String>,
    pub rules_text: Option<String>,
    pub expected_sha256: Option<String>,
    #[serde(default)]
    pub loosens: bool,
}

#[derive(Debug, Deserialize)]
pub struct AddReactionBody {
    pub emoji: String,
    pub actor: Option<String>,
    pub chat_kind: Option<String>,
    pub bot_id: Option<String>,
}

const INTERNAL_SESSION_HEADER: &str = "x-dt-verified-session";
const HOSTED_SESSION_COOKIE: &str = "dt_hosted_session";

fn check_auth(
    headers: &HeaderMap,
    expected_token: Option<&str>,
) -> std::result::Result<(), StatusCode> {
    if headers
        .get(INTERNAL_SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
        == Some("1")
    {
        return Ok(());
    }
    let Some(expected) = expected_token.filter(|t| !t.trim().is_empty()) else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    let auth_val = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if crate::bridges::constant_time_eq(
        auth_val.as_bytes(),
        format!("Bearer {expected}").as_bytes(),
    ) {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

fn extract_cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let Some((k, v)) = part.trim().split_once('=') else {
            continue;
        };
        if k.trim() == name && !v.trim().is_empty() {
            return Some(v.trim().to_string());
        }
    }
    None
}

async fn api_v1_auth_and_csrf_middleware(
    State(state): State<CompanionWebState>,
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    request.headers_mut().remove(INTERNAL_SESSION_HEADER);
    let path = request.uri().path().to_string();
    let rel_path = path.strip_prefix("/api/v1").unwrap_or(&path);
    // Public/health/webhook/login endpoints handle their own verification.
    if (rel_path == "/auth/session" && request.method() == axum::http::Method::POST)
        || rel_path == "/billing/webhooks/stripe"
        || rel_path == "/health/live"
        || rel_path == "/health/ready"
    {
        return next.run(request).await;
    }

    let requested_ws = rel_path
        .strip_prefix("/workspaces/")
        .and_then(|rest| rest.split('/').next())
        .map(ToString::to_string)
        .or_else(|| {
            request
                .headers()
                .get("x-dt-workspace-id")
                .and_then(|v| v.to_str().ok())
                .map(ToString::to_string)
        });

    if let Some(session_id) = extract_cookie_value(request.headers(), HOSTED_SESSION_COOKIE) {
        let Some(ref plane) = state.hosted_plane else {
            return ApiErr::msg(
                StatusCode::UNAUTHORIZED,
                "Hosted session store is not configured",
            )
            .into_response();
        };
        let workspace = if rel_path == "/auth/session" || rel_path == "/workspaces" {
            None
        } else {
            requested_ws.as_deref()
        };
        let session = match plane.authorize_session(&session_id, workspace).await {
            Ok(s) => s,
            Err(err) => {
                let code = if err.to_string().contains("Cross-tenant") {
                    StatusCode::FORBIDDEN
                } else {
                    StatusCode::UNAUTHORIZED
                };
                return ApiErr::msg(code, err.to_string()).into_response();
            }
        };
        let is_mutation = !matches!(
            *request.method(),
            axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
        );
        if is_mutation {
            let csrf = request
                .headers()
                .get("x-dt-csrf-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if !crate::bridges::constant_time_eq(csrf.as_bytes(), session.csrf_token.as_bytes()) {
                return ApiErr::msg(StatusCode::FORBIDDEN, "Missing or invalid CSRF token")
                    .into_response();
            }
        }
        let active_ws = session.ws_id.clone();
        if let Ok(ws_sup) = state.supervisor_for_workspace(&active_ws).await {
            request.extensions_mut().insert(WorkspaceSupervisor(ws_sup));
        }
        request.extensions_mut().insert(session);
        request
            .headers_mut()
            .insert(INTERNAL_SESSION_HEADER, HeaderValue::from_static("1"));
        return next.run(request).await;
    }

    // Fallback to Bearer token authentication (local loopback companion / mobile / operator).
    if check_auth(request.headers(), state.bearer_token.as_deref()).is_err() {
        return ApiErr::msg(StatusCode::UNAUTHORIZED, "Authentication required").into_response();
    }
    if let (Some(ws), Some(plane)) = (requested_ws.as_deref(), state.hosted_plane.as_ref()) {
        let token = request
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .unwrap_or("");
        let is_operator = state.bearer_token.as_deref().is_some_and(|expected| {
            crate::bridges::constant_time_eq(token.as_bytes(), expected.as_bytes())
        });
        let expected_local = std::env::var("GM_WORKSPACE_ID").unwrap_or_else(|_| "default".into());
        let workspace_exists = plane.get_workspace(ws).await.ok().flatten().is_some();
        if !is_operator
            && ws != expected_local
            && ws != "local"
            && plane.authorize_workspace(ws, token).await.is_err()
        {
            return ApiErr::msg(
                StatusCode::FORBIDDEN,
                format!("Workspace access denied for '{ws}'"),
            )
            .into_response();
        }
        if workspace_exists && let Ok(ws_sup) = state.supervisor_for_workspace(ws).await {
            request.extensions_mut().insert(WorkspaceSupervisor(ws_sup));
        }
    }
    next.run(request).await
}

pub fn build_companion_router(state: CompanionWebState) -> Router {
    let spaces_port_atomic = state.supervisor.spaces_port_atomic();
    let local_space_previews =
        state.hosted_plane.is_none() && state.supervisor.config().muse.enable_spaces;
    let sync_supervisor = state.supervisor.clone();
    Router::new()
        .route("/", get(serve_index))
        .route("/companion.css", get(serve_css))
        .route("/companion.js", get(serve_script))
        .route("/admin/audit", get(serve_index))
        .route(
            "/app",
            get(|| async {
                with_web_app_security_headers(axum::response::Redirect::permanent("/app/"))
            }),
        )
        .route("/app/", get(serve_web_app_index))
        .route("/app/*path", get(serve_web_app_asset))
        .route("/.well-known/agent.json", get(serve_agent_card))
        .route("/a2a/card", get(serve_agent_card))
        .route("/a2a/tasks", post(create_a2a_task))
        .route("/a2a/tasks/:id", get(get_a2a_task))
        .route("/api/bootstrap", get(get_bootstrap_route))
        .route(
            "/api/ui-settings",
            get(get_ui_settings_route).put(put_ui_settings_route),
        )
        .route("/api/runtime", get(runtime_status))
        .route("/api/events", get(replay_events))
        .route("/api/bots", get(list_bots).post(upsert_bot_route))
        .route("/api/bots/:id", axum::routing::delete(delete_bot_route))
        .route("/api/bots/:id/duplicate", post(duplicate_bot_route))
        .route(
            "/api/bots/:id/ui-prefs",
            axum::routing::put(put_bot_ui_prefs_route),
        )
        .route(
            "/api/bots/:id/avatar",
            get(get_bot_avatar_route)
                .post(post_bot_avatar_route)
                .route_layer(DefaultBodyLimit::max(36 * 1024 * 1024)),
        )
        .route("/api/bots/:id/files", get(list_bot_files_route))
        .route("/api/bots/:id/files/preview", get(preview_bot_file_route))
        .route(
            "/api/bots/:id/attachments",
            post(upload_bot_attachment_route).route_layer(DefaultBodyLimit::max(36 * 1024 * 1024)),
        )
        .route("/api/bots/:id/library", get(list_bot_library_route))
        .route("/api/library/:id/pin", post(pin_library_item_route))
        .route(
            "/api/sections",
            get(list_sections_route).post(upsert_section_route),
        )
        .route(
            "/api/sections/:id",
            axum::routing::patch(patch_section_route).delete(delete_section_route),
        )
        .route(
            "/api/dm/:bot_id/messages",
            get(list_dm_messages_route).post(post_dm_message_route),
        )
        .route("/api/search/messages", get(search_messages_route))
        .route("/api/chats/read", post(mark_chat_read_route))
        .route("/api/queue", get(list_queue).post(enqueue_task))
        .route("/api/tasks/:id/:action", post(control_task_route))
        .route("/api/channels", get(list_channels).post(create_channel))
        .route("/api/channels/:id", axum::routing::patch(patch_channel_route))
        .route(
            "/api/channels/:id/messages",
            get(list_channel_messages).post(post_channel_message),
        )
        .route("/api/messages/:id/reactions", post(post_message_reaction))
        .route("/api/approvals/:id/respond", post(respond_approval_route))
        .route("/api/secrets/:id/submit", post(submit_secret_prompt_route))
        .route("/api/questions/:id/respond", post(respond_question_route))
        .route("/api/rules", get(list_rules_route).post(upsert_rule_route))
        .route("/api/rules/:id", axum::routing::delete(delete_rule_route))
        .route(
            "/api/vault",
            get(list_vault_route).post(upsert_vault_route),
        )
        .route(
            "/api/vault/import",
            post(import_vault_csv_route).route_layer(DefaultBodyLimit::max(7 * 1024 * 1024)),
        )
        .route(
            "/api/vault/:id",
            axum::routing::delete(delete_vault_route),
        )
        .route("/api/vault/:id/reveal", post(reveal_vault_route))
        .route(
            "/api/vault/:id/default-payment",
            post(set_default_payment_route),
        )
        .route("/api/usage", get(get_usage_route))
        .route(
            "/api/routines",
            get(list_routines_route).post(upsert_routine_route),
        )
        .route(
            "/api/routines/:id",
            axum::routing::delete(delete_routine_route),
        )
        .route("/api/routines/:id/test", post(test_routine_route))
        .route("/api/routines/:id/trigger", post(trigger_routine_webhook_route))
        .route("/api/routines/:id/webhook-key", get(get_routine_webhook_key_route))
        .route("/api/routines/:id/runs", get(list_routine_runs_route))
        .route(
            "/api/connectors/custom",
            get(list_custom_connectors_route).post(upsert_custom_connector_route),
        )
        .route(
            "/api/connectors/custom/:id",
            axum::routing::delete(delete_custom_connector_route),
        )
        .route(
            "/api/connectors/custom/:id/test",
            post(test_custom_connector_route),
        )
        .route("/api/computer/preview", get(capture_computer_preview_route))
        .route("/api/teach/start", post(start_teach_route))
        .route("/api/teach/step", post(record_teach_step_route))
        .route("/api/teach/stop", post(stop_teach_route))
        .route("/api/audit", get(query_audit))
        .route("/api/activity", get(list_activity).post(create_activity_post))
        .route("/api/activity/prompt", post(update_activity_prompt))
        .route("/api/activity/:id/rate", post(rate_activity_post))
        .route(
            "/api/activity/:id",
            axum::routing::delete(delete_activity_post_route),
        )
        .route("/api/ideas", get(list_ideas).post(create_idea_route))
        .route("/api/ideas/:id/run", post(run_idea_route))
        .route("/api/ideas/:id/dismiss", post(dismiss_idea_route))
        .route(
            "/api/goals",
            get(list_goals_route)
                .post(create_goal_route)
                .patch(patch_goal_route),
        )
        .route("/api/goals/:slug/files/*path", get(get_goal_file_route))
        .route("/api/spaces", get(list_spaces_route).post(create_space_route))
        .route("/api/spaces/:slug/url", get(get_space_url_route))
        .route("/api/spaces/:slug/publish", post(publish_space_route))
        .route("/api/spaces/:slug/unpublish", post(unpublish_space_route))
        .route(
            "/api/spaces/:slug/file",
            get(get_space_file_route).put(put_space_file_route),
        )
        .route(
            "/api/spaces/:slug",
            axum::routing::delete(delete_space_route),
        )
        .route("/api/skills", get(list_skills_route))
        .route("/api/skills/custom", post(create_custom_skill_route))
        .route(
            "/api/skills/custom/:name",
            axum::routing::delete(delete_custom_skill_route),
        )
        .route("/api/skills/:name/amend", post(amend_skill_route))
        .route("/api/connectors", get(list_connectors_route))
        .route(
            "/api/identity/:bot_id/:file",
            get(read_identity_file_route).put(write_identity_file_route),
        )
        .route(
            "/api/proactive-prefs",
            get(get_proactive_prefs_route).put(put_proactive_prefs_route),
        )
        .route(
            "/api/memory/import",
            post(import_memory_route).route_layer(DefaultBodyLimit::max(2 * 1024 * 1024)),
        )
        .route("/api/memory/explain", post(explain_memory_route))
        .route("/api/memory/forget/plan", post(forget_plan_route))
        .route("/api/memory/forget/confirm", post(forget_confirm_route))
        .route(
            "/api/side-chats/:chat_id/discretion",
            get(get_side_chat_discretion_route).put(put_side_chat_discretion_route),
        )
        .route("/api/templates/export", post(export_template_route))
        .route("/api/templates/preview", post(preview_template_route))
        .route("/api/templates/import", post(import_template_route))
        .route("/api/system/health", get(system_health_route))
        .route("/api/system/pause", post(system_pause_route))
        .route("/api/system/kill", post(system_kill_route))
        .route("/api/system/updates/check", post(system_updates_check_route))
        .route("/api/system/updates/download", post(system_updates_download_route))
        .route("/api/system/components", get(system_components_list_route))
        .route(
            "/api/system/components/:id/install",
            post(system_components_install_route),
        )
        .route(
            "/api/system/components/:id/verify",
            post(system_components_verify_route),
        )
        .route("/api/voice/status", get(voice_status_route))
        .route("/api/voice/start", post(voice_start_route))
        .route("/api/voice/consent", post(voice_consent_route))
        .route(
            "/api/voice/transcribe",
            post(voice_transcribe_route).route_layer(DefaultBodyLimit::max(24 * 1024 * 1024)),
        )
        .route("/api/voice/speak", post(voice_speak_route))
        .route("/api/voice/barge_in", post(voice_barge_in_route))
        .route("/api/voice/stop", post(voice_stop_route))
        .route("/api/system/providers", get(system_providers_get_route))
        .route("/api/system/providers/profile", post(system_provider_profile_route))
        .route("/api/system/providers/:id/discover-models", post(system_provider_discover_models_route))
        .route("/api/system/providers/:id/oauth/:op", post(system_provider_oauth_route))
        .route("/api/system/providers/key", post(system_provider_key_route))
        .route(
            "/api/system/support-bundle",
            post(system_support_bundle_route),
        )
        .route("/api/system/feedback", post(system_feedback_route))
        .route("/ag-ui/runs", post(start_agui_run))
        .route("/ag-ui/events", get(stream_agui_events))
        .route("/bridges/inbound", post(handle_bridge_inbound))
        .route("/bridges/slack/events", post(handle_slack_webhook))
        .nest("/api/v1", api_v1_router(state.clone()))
        .layer(axum::middleware::from_fn(
            move |mut request: axum::extract::Request, next: axum::middleware::Next| {
                let sp_atomic = Arc::clone(&spaces_port_atomic);
                let sync_supervisor = sync_supervisor.clone();
                async move {
                    request.headers_mut().remove(INTERNAL_SESSION_HEADER);
                    let req_path = request.uri().path().to_string();
                    let is_mutation = request.method() != axum::http::Method::GET && req_path.starts_with("/api/");
                    let is_html_route = req_path == "/" || req_path == "/admin/audit";
                    let is_app_route = req_path == "/app" || req_path.starts_with("/app/");
                    let mut response = next.run(request).await;
                    if is_mutation && response.status().is_success() {
                        sync_supervisor.emit_agui("STATE_INVALIDATED", "ui", "", serde_json::json!({"path":req_path}));
                    }
                    if is_app_route {
                        if local_space_previews
                            && response.status() == StatusCode::OK
                            && response.headers().get(header::CONTENT_TYPE)
                                == Some(&HeaderValue::from_static("text/html; charset=utf-8"))
                        {
                            let port = sp_atomic.load(Ordering::SeqCst);
                            if port != 0 {
                                let csp = format!("{WEB_APP_CSP}; frame-src http://127.0.0.1:{port}");
                                if let Ok(value) = HeaderValue::from_str(&csp) {
                                    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY, value);
                                }
                            }
                        }
                        return response;
                    }
                    let csp = if is_html_route {
                        let spaces_port = sp_atomic.load(Ordering::SeqCst);
                        format!(
                            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; media-src 'self' blob:; connect-src 'self'; frame-src http://127.0.0.1:{spaces_port}; frame-ancestors 'none'; base-uri 'none'; object-src 'none'"
                        )
                    } else {
                        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; media-src 'self' blob:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; object-src 'none'".to_string()
                    };
                    for (key, value) in [
                        ("x-content-type-options", "nosniff"),
                        ("referrer-policy", "no-referrer"),
                        ("cache-control", "no-store"),
                        ("content-security-policy", csp.as_str()),
                    ] {
                        if let (Ok(hn), Ok(hv)) = (
                            HeaderName::from_bytes(key.as_bytes()),
                            HeaderValue::from_str(value),
                        ) {
                            response.headers_mut().insert(hn, hv);
                        }
                    }
                    response
                }
            },
        ))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state(state)
}

pub struct CompanionWebServer {
    state: CompanionWebState,
}

impl CompanionWebServer {
    pub fn new(
        supervisor: OpenBotSupervisor,
        bridge_router: BridgeRouter,
        bearer_token: Option<String>,
        slack_signing_secret: Option<String>,
    ) -> Self {
        let shared_config = Arc::new(parking_lot::RwLock::new(supervisor.config().clone()));
        let voice_manager =
            crate::voice::VoiceSessionManager::new(supervisor.config().storage_dir.clone());
        Self {
            state: CompanionWebState {
                supervisor,
                bridge_router,
                bearer_token,
                slack_signing_secret,
                interactions: crate::interactions::SharedInteractionState::new(),
                shared_config,
                config_path: None,
                budget: None,
                llm_client: None,
                router: None,
                kill_switch: None,
                platform: None,
                updates: None,
                components: None,
                voice_manager,
                hosted_plane: None,
                workspace_supervisors: Arc::new(parking_lot::RwLock::new(
                    std::collections::HashMap::new(),
                )),
                sanitized_doctor: None,
                web_dist: None,
            },
        }
    }

    pub fn with_hosted_plane(mut self, plane: Arc<crate::hosted::HostedControlPlane>) -> Self {
        self.state.hosted_plane = Some(crate::hosted::HostedPlaneBackend::Sqlite(plane));
        self
    }

    pub fn with_hosted_backend(mut self, backend: crate::hosted::HostedPlaneBackend) -> Self {
        self.state.hosted_plane = Some(backend);
        self
    }

    pub fn with_sanitized_doctor(
        mut self,
        doctor: Arc<parking_lot::RwLock<Option<serde_json::Value>>>,
    ) -> Self {
        self.state.sanitized_doctor = Some(doctor);
        self
    }

    pub fn with_updates(mut self, updates: Arc<dyn CompanionUpdates>) -> Self {
        self.state.updates = Some(updates);
        self
    }

    pub fn with_web_dist(mut self, dir: Option<PathBuf>) -> Self {
        self.state.web_dist = dir;
        self
    }

    pub fn with_components(mut self, components: Arc<dyn CompanionComponents>) -> Self {
        self.state.components = Some(components);
        self
    }

    pub fn with_voice_manager(mut self, voice_manager: crate::voice::VoiceSessionManager) -> Self {
        self.state.voice_manager = voice_manager;
        self
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_runtime_deps(
        mut self,
        interactions: crate::interactions::SharedInteractionState,
        shared_config: Arc<parking_lot::RwLock<gm_core::config::GmConfig>>,
        config_path: Option<PathBuf>,
        budget: Option<Arc<gm_llm::BudgetManager>>,
        llm_client: Option<Arc<gm_llm::HybridLlmClient>>,
        router: Option<gm_llm::LlmRouter>,
        kill_switch: Option<gm_safety::KillSwitch>,
        platform: Option<Arc<dyn gm_platform::Platform>>,
    ) -> Self {
        self.state.interactions = interactions;
        self.state.shared_config = shared_config;
        self.state.config_path = config_path;
        self.state.budget = budget;
        self.state.llm_client = llm_client;
        self.state.router = router;
        self.state.kill_switch = kill_switch;
        self.state.platform = platform;
        self
    }

    pub async fn bind_and_serve(
        self,
        bind_addr: &str,
    ) -> gm_core::error::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
        if self
            .state
            .bearer_token
            .as_ref()
            .is_none_or(|t| t.trim().is_empty())
        {
            return Err(gm_core::error::GmError::Config(
                "Companion authentication token is required".into(),
            ));
        }
        let requested: SocketAddr = bind_addr.parse().map_err(|_| {
            gm_core::error::GmError::Config("Use an explicit loopback IP and port".into())
        })?;
        if !requested.ip().is_loopback() {
            return Err(gm_core::error::GmError::Config(
                "Remote companion access requires a TLS pairing gateway; bind to loopback".into(),
            ));
        }
        let listener = TcpListener::bind(requested).await?;
        let local_addr = listener.local_addr()?;
        self.state.supervisor.set_companion_port(local_addr.port());
        let router = build_companion_router(self.state);
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok((local_addr, handle))
    }
}

async fn serve_index() -> Html<&'static str> {
    Html(COMPANION_UI_HTML)
}

async fn serve_css() -> impl IntoResponse {
    (
        [("content-type", "text/css; charset=utf-8")],
        COMPANION_UI_CSS,
    )
}

async fn serve_script() -> impl IntoResponse {
    (
        [("content-type", "text/javascript; charset=utf-8")],
        COMPANION_UI_JS,
    )
}

const WEB_APP_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'";
const WEB_APP_NOT_INSTALLED_HTML: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>DT Web App Not Installed</title></head><body><h1>Web app not installed.</h1><p>Extract the DT web client bundle or build <code>clients/web</code>, then point <code>DT_WEB_DIST</code> at its <code>dist</code> directory.</p></body></html>";

fn with_web_app_security_headers(resp: impl IntoResponse) -> axum::response::Response {
    let mut response = resp.into_response();
    for (key, value) in [
        ("content-security-policy", WEB_APP_CSP),
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("referrer-policy", "no-referrer"),
    ] {
        if let (Ok(hn), Ok(hv)) = (
            HeaderName::from_bytes(key.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            response.headers_mut().insert(hn, hv);
        }
    }
    response
}

fn web_app_mime_for_path(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

async fn serve_web_app_index(State(state): State<CompanionWebState>) -> axum::response::Response {
    let Some(ref dist_dir) = state.web_dist else {
        return with_web_app_security_headers((
            StatusCode::NOT_FOUND,
            [
                ("content-type", "text/html; charset=utf-8"),
                ("cache-control", "no-cache"),
            ],
            WEB_APP_NOT_INSTALLED_HTML,
        ));
    };
    let index_path = dist_dir.join("index.html");
    match tokio::fs::read(&index_path).await {
        Ok(bytes) => with_web_app_security_headers((
            StatusCode::OK,
            [
                ("content-type", "text/html; charset=utf-8"),
                ("cache-control", "no-cache"),
            ],
            bytes,
        )),
        Err(_) => with_web_app_security_headers((
            StatusCode::NOT_FOUND,
            [
                ("content-type", "text/html; charset=utf-8"),
                ("cache-control", "no-cache"),
            ],
            WEB_APP_NOT_INSTALLED_HTML,
        )),
    }
}

async fn serve_web_app_asset(
    State(state): State<CompanionWebState>,
    Path(path): Path<String>,
) -> axum::response::Response {
    let Some(ref dist_dir) = state.web_dist else {
        return with_web_app_security_headers((
            StatusCode::NOT_FOUND,
            [
                ("content-type", "text/html; charset=utf-8"),
                ("cache-control", "no-cache"),
            ],
            WEB_APP_NOT_INSTALLED_HTML,
        ));
    };
    if path.is_empty() {
        return serve_web_app_index(State(state)).await;
    }
    if path.contains("..") || path.contains('\\') || path.contains('\0') || path.starts_with('/') {
        return with_web_app_security_headers((StatusCode::NOT_FOUND, "Not found"));
    }
    let Ok(canonical_dist) = tokio::fs::canonicalize(dist_dir).await else {
        return with_web_app_security_headers((
            StatusCode::NOT_FOUND,
            [
                ("content-type", "text/html; charset=utf-8"),
                ("cache-control", "no-cache"),
            ],
            WEB_APP_NOT_INSTALLED_HTML,
        ));
    };
    let candidate = dist_dir.join(&path);
    if let Ok(resolved) = tokio::fs::canonicalize(&candidate).await {
        if !resolved.starts_with(&canonical_dist) {
            return with_web_app_security_headers((StatusCode::NOT_FOUND, "Not found"));
        }
        if let Ok(meta) = tokio::fs::metadata(&resolved).await
            && meta.is_file()
            && let Ok(bytes) = tokio::fs::read(&resolved).await
        {
            let mime = web_app_mime_for_path(&resolved);
            let cache = if path.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
            return with_web_app_security_headers((
                StatusCode::OK,
                [("content-type", mime), ("cache-control", cache)],
                bytes,
            ));
        }
    }
    let has_extension = std::path::Path::new(&path)
        .file_name()
        .and_then(|n| std::path::Path::new(n).extension())
        .is_some();
    if !has_extension {
        return serve_web_app_index(State(state)).await;
    }
    with_web_app_security_headers((StatusCode::NOT_FOUND, "Not found"))
}

async fn serve_agent_card(State(state): State<CompanionWebState>) -> impl IntoResponse {
    Json(state.supervisor.a2a().agent_card())
}

async fn runtime_status(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    Ok(Json(state.supervisor.runtime_status()))
}

#[derive(Deserialize)]
struct EventCursor {
    #[serde(default)]
    after: u64,
}
async fn replay_events(
    headers: HeaderMap,
    Query(cursor): Query<EventCursor>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let events = state
        .supervisor
        .queue()
        .events_after(cursor.after, 1000)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let next = events.last().map(|(id, _)| *id).unwrap_or(cursor.after);
    let events: Vec<_> = events
        .into_iter()
        .map(|(id, mut e)| {
            e["sequence"] = id.into();
            e
        })
        .collect();
    Ok(Json(
        serde_json::json!({"protocol_version":1,"events":events,"next_cursor":next}),
    ))
}

async fn list_bots(
    headers: HeaderMap,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    Ok(Json(sup.roster().list_bots()))
}

async fn list_queue(
    headers: HeaderMap,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let tasks = sup
        .queue()
        .list_tasks(None)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(tasks))
}

async fn enqueue_task(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(req): Json<EnqueueRequest>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let task = state
        .supervisor
        .submit_task(
            req.bot_id.as_deref(),
            req.channel_id.as_deref(),
            &req.instruction,
            req.priority,
        )
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok((StatusCode::CREATED, Json(task)))
}

async fn create_a2a_task(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(req): Json<EnqueueRequest>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let task = state
        .supervisor
        .submit_task(
            req.bot_id.as_deref(),
            req.channel_id.as_deref(),
            &req.instruction,
            req.priority,
        )
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok((StatusCode::CREATED, Json(task)))
}

async fn get_a2a_task(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let task = state
        .supervisor
        .queue()
        .get_task(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(task))
}

async fn control_task_route(
    headers: HeaderMap,
    Path((id, action)): Path<(String, String)>,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
    body: Option<Json<serde_json::Value>>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let ok = match action.as_str() {
        "pause" => sup
            .control_task(&id, &QueueControlAction::Pause)
            .map_err(|_| StatusCode::BAD_REQUEST)?,
        "resume" => sup
            .control_task(&id, &QueueControlAction::Resume)
            .map_err(|_| StatusCode::BAD_REQUEST)?,
        "retry" => sup
            .control_task(&id, &QueueControlAction::Retry)
            .map_err(|_| StatusCode::BAD_REQUEST)?,
        "cancel" => sup
            .control_task(&id, &QueueControlAction::Cancel)
            .map_err(|_| StatusCode::BAD_REQUEST)?,
        "wheel-resume" => {
            let note = body
                .as_ref()
                .and_then(|b| b.get("note"))
                .and_then(|v| v.as_str());
            let bot_id = sup
                .queue()
                .get_task(&id)
                .ok()
                .flatten()
                .map(|t| t.bot_id)
                .unwrap_or_else(|| "muse".into());
            let resumed = sup
                .queue()
                .resume_from_human(&id, note)
                .map_err(|_| StatusCode::BAD_REQUEST)?;
            let wheel_req_id = format!("wheel:{id}");
            let _ = state
                .interactions
                .resolve_approval(&wheel_req_id, true)
                .await;
            update_web_dm_card_status(
                &sup,
                &wheel_req_id,
                "resumed",
                note.map(|n| serde_json::json!({ "note": n })),
            );
            let _ = sup
                .daemon_event_sender()
                .send(DaemonEvent::WheelHandoffResumed {
                    task_id: id.clone(),
                    bot_id: bot_id.clone(),
                });
            sup.broadcast_queue_state(&bot_id);
            resumed
        }
        "reassign" => {
            let target = body
                .as_ref()
                .and_then(|b| b.get("target_bot_id"))
                .and_then(|v| v.as_str())
                .ok_or(StatusCode::BAD_REQUEST)?;
            sup.control_task(
                &id,
                &QueueControlAction::Reassign {
                    target_bot_id: target.to_string(),
                },
            )
            .map_err(|_| StatusCode::BAD_REQUEST)?
        }
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    sup.emit_agui(
        "TASK_UPDATED",
        &id,
        "",
        serde_json::json!({ "task_id": id, "action": action, "ok": ok }),
    );
    Ok(Json(serde_json::json!({ "ok": ok, "task_id": id })))
}

async fn list_channels(
    headers: HeaderMap,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let chans = sup
        .channels()
        .list_channels(false)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(chans))
}

async fn create_channel(
    headers: HeaderMap,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
    Json(body): Json<CreateChannelBody>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    if sup
        .channels()
        .get_channel(&body.id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .is_some()
    {
        return Err(StatusCode::CONFLICT);
    }
    let channel = sup
        .create_channel(
            &body.id,
            &body.topic,
            body.owner_bot_id.as_deref(),
            &body.participants,
        )
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok((StatusCode::CREATED, Json(channel)))
}

async fn list_channel_messages(
    headers: HeaderMap,
    Path(id): Path<String>,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    sup.channels()
        .get_channel(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let msgs = sup
        .channels()
        .list_messages(&id, 100)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(msgs))
}

async fn post_channel_message(
    headers: HeaderMap,
    Path(id): Path<String>,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
    Json(body): Json<ChannelPostBody>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let author = body.author.as_deref().unwrap_or("operator");
    let (msg, spawned) = sup
        .post_channel_message(&id, author, None, &body.content)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(Json(serde_json::json!({
        "message": msg,
        "spawned_tasks": spawned,
    })))
}

async fn query_audit(
    headers: HeaderMap,
    Query(q): Query<AuditQuery>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let cat = match q.category.as_deref() {
        Some("permitted") => Some(AuditCategory::Permitted),
        Some("refused") => Some(AuditCategory::Refused),
        Some("failed") => Some(AuditCategory::Failed),
        _ => None,
    };
    let records = state.supervisor.audit().query_governance_entries(
        cat,
        q.bot_id.as_deref(),
        q.limit.unwrap_or(100),
    );
    Ok(Json(records))
}

async fn start_agui_run(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(req): Json<AgUiRunRequest>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let task = state
        .supervisor
        .submit_task(req.bot_id.as_deref(), Some("general"), &req.instruction, 10)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    state.supervisor.emit_agui(
        "RUN_QUEUED",
        &task.id,
        &task.bot_id,
        serde_json::json!({ "instruction": task.instruction }),
    );
    Ok((StatusCode::CREATED, Json(task)))
}

async fn stream_agui_events(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
) -> std::result::Result<
    Sse<impl tokio_stream::Stream<Item = std::result::Result<Event, Infallible>>>,
    StatusCode,
> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let rx = state.supervisor.agui_sender().subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|item| match item {
        Ok(evt) => {
            let json = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".into());
            Some(Ok(Event::default()
                .id(evt.sequence.to_string())
                .event(evt.event_type)
                .data(json)))
        }
        // A lagged client must reload snapshots rather than silently miss state changes.
        Err(_) => Some(Ok(Event::default().event("RESYNC_REQUIRED").data("{}"))),
    });
    Ok(Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

async fn handle_bridge_inbound(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(msg): Json<BridgeInboundMessage>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let reply = state
        .bridge_router
        .handle_inbound(&msg)
        .map_err(|_| StatusCode::FORBIDDEN)?;
    Ok(Json(reply))
}

async fn handle_slack_webhook(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    raw_body: String,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    {
        let secret = state
            .slack_signing_secret
            .as_ref()
            .filter(|s| !s.trim().is_empty())
            .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
        let ts = headers
            .get("x-slack-request-timestamp")
            .and_then(|v| v.to_str().ok())
            .ok_or(StatusCode::UNAUTHORIZED)?;
        let sig = headers
            .get("x-slack-signature")
            .and_then(|v| v.to_str().ok())
            .ok_or(StatusCode::UNAUTHORIZED)?;
        if !verify_slack_signature(secret, ts, &raw_body, sig) {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    let val: serde_json::Value =
        serde_json::from_str(&raw_body).map_err(|_| StatusCode::BAD_REQUEST)?;

    // Handle Slack URL verification challenge
    if val.get("type").and_then(|v| v.as_str()) == Some("url_verification") {
        let challenge = val.get("challenge").and_then(|v| v.as_str()).unwrap_or("");
        return Ok(Json(serde_json::json!({ "challenge": challenge })));
    }

    let event = val.get("event").ok_or(StatusCode::BAD_REQUEST)?;
    if event.get("bot_id").is_some() || event.get("subtype").is_some() {
        return Ok(Json(serde_json::json!({"accepted":false,"ignored":true})));
    }
    let event_id = val
        .get("event_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or(StatusCode::BAD_REQUEST)?;
    // Delivery admission is durable across daemon restarts.
    if !state
        .supervisor
        .queue()
        .claim_delivery(&format!("slack:{event_id}"))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        return Ok(Json(serde_json::json!({"accepted":true,"duplicate":true})));
    }

    let channel_id = val
        .get("event")
        .and_then(|e| e.get("channel"))
        .and_then(|v| v.as_str())
        .unwrap_or("general")
        .to_string();
    let user = val
        .get("event")
        .and_then(|e| e.get("user"))
        .and_then(|v| v.as_str())
        .unwrap_or("slack_user")
        .to_string();
    let text = val
        .get("event")
        .and_then(|e| e.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let inbound = BridgeInboundMessage {
        platform: "slack".into(),
        external_channel_id: channel_id,
        sender_name: user,
        text,
    };
    let reply = state
        .bridge_router
        .handle_inbound(&inbound)
        .map_err(|_| StatusCode::FORBIDDEN)?;
    Ok(Json(serde_json::json!(reply)))
}

async fn post_message_reaction(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
    Json(body): Json<AddReactionBody>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let actor = body.actor.as_deref().unwrap_or("operator");
    let explicit_kind = body.chat_kind.as_deref().map(str::trim);
    let outcome =
        match explicit_kind {
            Some(k) if k.eq_ignore_ascii_case("dm") => {
                state
                    .supervisor
                    .toggle_message_reaction("dm", &id, &body.emoji, actor)
            }
            Some(k) if k.eq_ignore_ascii_case("channel") => state
                .supervisor
                .toggle_message_reaction("channel", &id, &body.emoji, actor),
            _ => state
                .supervisor
                .toggle_message_reaction("channel", &id, &body.emoji, actor)
                .or_else(|_| {
                    state
                        .supervisor
                        .toggle_message_reaction("dm", &id, &body.emoji, actor)
                }),
        };
    let (chat_id, reactions) = outcome.map_err(|err| {
        let msg = err.to_string();
        if msg.contains("Unknown ") || msg.contains("not found") {
            ApiErr::msg(StatusCode::NOT_FOUND, msg)
        } else {
            map_gm_error(err)
        }
    })?;
    let _ = state
        .supervisor
        .daemon_event_sender()
        .send(DaemonEvent::ActivityUpdated {
            reason: "message_reaction".into(),
        });
    Ok(Json(serde_json::json!({
        "chat_id": chat_id,
        "message_id": id,
        "reactions": reactions,
    })))
}

async fn list_activity(
    headers: HeaderMap,
    Query(q): Query<ActivityQuery>,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let posts = sup.feed_store().list_posts(limit).map_err(map_gm_error)?;
    let brief_prompt = sup.feed_store().get_brief_prompt().map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({
        "posts": posts,
        "brief_prompt": brief_prompt,
    })))
}

async fn create_activity_post(
    headers: HeaderMap,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
    Json(body): Json<CreateActivityBody>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let title = body.title.trim();
    let content = body.body.trim();
    if title.is_empty() || content.is_empty() {
        return Err(ApiErr::msg(
            StatusCode::BAD_REQUEST,
            "Activity title and body cannot be empty",
        ));
    }
    let category = body
        .category
        .as_deref()
        .map(FeedCategory::from_db_str)
        .unwrap_or(FeedCategory::Editorial);
    let post = FeedPost {
        id: body
            .id
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| format!("post-{}", uuid::Uuid::new_v4())),
        kicker: body
            .kicker
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "Editorial".into()),
        category,
        title: title.to_string(),
        body: content.to_string(),
        signal_citations: body.signal_citations,
        rating: None,
        user_prompt: body.user_prompt,
        created_at: chrono::Utc::now(),
    };
    sup.feed_store().upsert_post(&post).map_err(map_gm_error)?;
    let _ = sup
        .daemon_event_sender()
        .send(DaemonEvent::ActivityUpdated {
            reason: "feed_post_created".into(),
        });
    Ok((StatusCode::CREATED, Json(post)))
}

async fn update_activity_prompt(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<ActivityPromptBody>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    state
        .supervisor
        .feed_store()
        .set_brief_prompt(&body.prompt)
        .map_err(map_gm_error)?;
    let brief_prompt = state
        .supervisor
        .feed_store()
        .get_brief_prompt()
        .map_err(map_gm_error)?;
    let _ = state
        .supervisor
        .daemon_event_sender()
        .send(DaemonEvent::ActivityUpdated {
            reason: "brief_prompt_updated".into(),
        });
    Ok(Json(serde_json::json!({
        "ok": true,
        "brief_prompt": brief_prompt,
    })))
}

async fn rate_activity_post(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
    Json(body): Json<RateActivityBody>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let ok = state
        .supervisor
        .feed_store()
        .rate_post(&id, body.rating)
        .map_err(map_gm_error)?;
    if !ok {
        return Err(ApiErr::msg(
            StatusCode::NOT_FOUND,
            format!("Feed post '{id}' not found"),
        ));
    }
    let _ = state
        .supervisor
        .daemon_event_sender()
        .send(DaemonEvent::ActivityUpdated {
            reason: "feed_rated".into(),
        });
    Ok(Json(serde_json::json!({
        "ok": true,
        "id": id,
        "rating": body.rating.clamp(-1, 5),
    })))
}

fn custom_ideas_path(state: &CompanionWebState) -> PathBuf {
    state
        .supervisor
        .shared_paths()
        .storage_dir()
        .join("workspace/custom_ideas.json")
}

fn load_custom_ideas(state: &CompanionWebState) -> std::result::Result<Vec<ProactiveIdea>, ApiErr> {
    let path = custom_ideas_path(state);
    let Some((raw, _)) = SharedFileStore::global()
        .read_text_with_sha256(&path)
        .map_err(map_gm_error)?
    else {
        return Ok(Vec::new());
    };
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&raw)
        .map_err(|e| ApiErr::msg(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn collect_effective_ideas(
    state: &CompanionWebState,
) -> std::result::Result<Vec<ProactiveIdea>, ApiErr> {
    let default_bot = state.supervisor.config().openbot.default_bot_id.clone();
    let memory = state
        .supervisor
        .roster()
        .memory_store(&default_bot)
        .map_err(map_gm_error)?;
    let mut ideas = generate_ideas_with_llm(
        Option::<&MockPlatform>::None,
        &memory,
        None,
        chrono::Utc::now(),
    )
    .await
    .map_err(map_gm_error)?;

    let mut seen_ids: BTreeSet<String> = ideas.iter().map(|i| i.id.clone()).collect();
    for custom in load_custom_ideas(state)? {
        if seen_ids.insert(custom.id.clone()) {
            ideas.push(custom);
        }
    }

    if ideas.is_empty() {
        let now = chrono::Utc::now();
        ideas.push(ProactiveIdea {
            id: "idea-starter-goal-review".into(),
            title: "Review active goals & morning brief".into(),
            description:
                "Check due milestones across your Goals workspace and summarize next steps.".into(),
            source: IdeaSource::Hybrid,
            action_prompt: Some("Review my active goals and suggest the top next step.".into()),
            confidence: 0.72,
            created_at: now,
        });
        ideas.push(ProactiveIdea {
            id: "idea-starter-memory-sync".into(),
            title: "Import portable context into MEMORY.md".into(),
            description: "Bring over recurring preferences or background notes using the two-step safe importer.".into(),
            source: IdeaSource::Memory,
            action_prompt: Some("Help me organize key preferences into MEMORY.md.".into()),
            confidence: 0.68,
            created_at: now,
        });
    }

    let prefs = state
        .supervisor
        .prefs_store()
        .load()
        .map_err(map_gm_error)?;
    let dismissed: BTreeSet<String> = prefs
        .one_time_dismissals
        .iter()
        .map(|s| s.trim().to_lowercase())
        .collect();
    let opted_out: Vec<String> = prefs
        .never_tell_me_about
        .iter()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    ideas.retain(|idea| {
        let lower_id = idea.id.to_lowercase();
        if dismissed.contains(&lower_id) || dismissed.contains(&format!("idea:{lower_id}")) {
            return false;
        }
        let haystack = format!("{} {}", idea.title, idea.description).to_lowercase();
        !opted_out.iter().any(|topic| haystack.contains(topic))
    });

    Ok(ideas)
}

async fn list_ideas(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let ideas = collect_effective_ideas(&state).await?;
    Ok(Json(ideas))
}

async fn create_idea_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    body: Option<Json<CreateIdeaBody>>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    if let Some(Json(payload)) = body
        && let Some(raw_title) = payload.title.as_deref()
        && !raw_title.trim().is_empty()
    {
        let title = raw_title.trim().to_string();
        let idea = ProactiveIdea {
            id: format!("idea-{}", uuid::Uuid::new_v4()),
            title: title.clone(),
            description: payload
                .description
                .unwrap_or_else(|| "Custom proactive idea added by operator".into()),
            source: IdeaSource::Hybrid,
            action_prompt: payload.action_prompt.or(Some(title)),
            confidence: 0.90,
            created_at: chrono::Utc::now(),
        };
        let path = custom_ideas_path(&state);
        let idea_for_save = idea.clone();
        SharedFileStore::global()
            .update_sync(&path, None, move |existing| {
                let mut list: Vec<ProactiveIdea> = if existing.trim().is_empty() {
                    Vec::new()
                } else {
                    serde_json::from_str(existing).unwrap_or_default()
                };
                list.push(idea_for_save);
                Ok(serde_json::to_string_pretty(&list)?)
            })
            .map_err(map_gm_error)?;
        let _ = state
            .supervisor
            .daemon_event_sender()
            .send(DaemonEvent::ActivityUpdated {
                reason: "idea_created".into(),
            });
        return Ok((
            StatusCode::CREATED,
            Json(serde_json::to_value(idea).unwrap()),
        ));
    }

    let ideas = collect_effective_ideas(&state).await?;
    Ok((StatusCode::OK, Json(serde_json::to_value(ideas).unwrap())))
}

async fn run_idea_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
    body: Option<Json<RunIdeaBody>>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let ideas = collect_effective_ideas(&state).await?;
    let matched = ideas.into_iter().find(|i| i.id == id);
    let body_ref = body.as_ref().map(|b| &b.0);
    let override_instr = body_ref
        .and_then(|b| b.instruction.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let instruction = match (override_instr, matched) {
        (Some(instr), _) => instr.to_string(),
        (None, Some(idea)) => idea
            .action_prompt
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(idea.title),
        (None, None) => {
            return Err(ApiErr::msg(
                StatusCode::NOT_FOUND,
                format!("Idea '{id}' not found"),
            ));
        }
    };

    let bot_id = body_ref.and_then(|b| b.bot_id.as_deref());
    let channel_id = body_ref
        .and_then(|b| b.channel_id.as_deref())
        .or(Some("general"));
    let task = state
        .supervisor
        .submit_task(bot_id, channel_id, &instruction, 5)
        .map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "idea_id": id,
        "task": task,
    })))
}

async fn dismiss_idea_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let clean = id.trim();
    if clean.is_empty() {
        return Err(ApiErr::msg(StatusCode::BAD_REQUEST, "Idea ID is required"));
    }
    // One-time dismissal MUST write to FEEDBACK.md without adding a permanent opt-out.
    state
        .supervisor
        .prefs_store()
        .add_one_time_dismissal(&format!("idea:{clean}"))
        .map_err(map_gm_error)?;
    let _ = state
        .supervisor
        .daemon_event_sender()
        .send(DaemonEvent::ActivityUpdated {
            reason: "idea_dismissed".into(),
        });
    Ok(Json(serde_json::json!({
        "ok": true,
        "dismissed_once": clean,
    })))
}

async fn list_goals_route(
    headers: HeaderMap,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let mgr = sup.goal_manager();
    let goals = mgr.list_goals().map_err(map_gm_error)?;
    let mut enriched = Vec::with_capacity(goals.len());
    for goal in goals {
        let files = mgr.list_goal_files(&goal.slug).unwrap_or_default();
        enriched.push(GoalWithFiles { goal, files });
    }
    Ok(Json(enriched))
}

async fn create_goal_route(
    headers: HeaderMap,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
    Json(body): Json<CreateGoalBody>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let mgr = sup.goal_manager();
    let parent = body
        .parent_slug
        .as_deref()
        .or(body.parent_goal_id.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let created = match parent {
        Some(p) => mgr
            .create_subgoal(p, &body.title, &body.description, body.stop_conditions)
            .map_err(map_gm_error)?,
        None => mgr
            .create_goal(
                &body.title,
                &body.description,
                body.category.as_deref(),
                body.stop_conditions,
            )
            .map_err(map_gm_error)?,
    };
    let _ = sup
        .daemon_event_sender()
        .send(DaemonEvent::ActivityUpdated {
            reason: "goal_created".into(),
        });
    Ok((StatusCode::CREATED, Json(created)))
}

async fn patch_goal_route(
    headers: HeaderMap,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
    Json(body): Json<PatchGoalBody>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let target = body
        .slug
        .as_deref()
        .or(body.id.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiErr::msg(StatusCode::BAD_REQUEST, "Goal slug or id is required"))?;
    let mgr = sup.goal_manager();
    let result = match body.action.as_deref() {
        Some("complete") => {
            let g = mgr
                .complete_goal(target, body.verified.unwrap_or(true))
                .map_err(map_gm_error)?;
            serde_json::to_value(g)
                .map_err(|e| ApiErr::msg(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        }
        Some("abandon") => {
            let g = mgr.abandon_goal(target).map_err(map_gm_error)?;
            serde_json::to_value(g)
                .map_err(|e| ApiErr::msg(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        }
        Some("delete") => {
            let deleted = mgr.delete_goal(target).map_err(map_gm_error)?;
            serde_json::json!({ "deleted_slugs": deleted })
        }
        Some("update") | None => {
            let sched_opt = body.track_brief_schedule.as_deref().map(Some);
            let cat_opt = body.category.as_deref().map(Some);
            let g = mgr
                .update_goal(
                    target,
                    body.title.as_deref(),
                    body.description.as_deref(),
                    body.stop_conditions,
                    sched_opt,
                    cat_opt,
                )
                .map_err(map_gm_error)?;
            serde_json::to_value(g)
                .map_err(|e| ApiErr::msg(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        }
        Some(other) => {
            return Err(ApiErr::msg(
                StatusCode::BAD_REQUEST,
                format!("Unsupported goal action '{other}'"),
            ));
        }
    };
    let _ = sup
        .daemon_event_sender()
        .send(DaemonEvent::ActivityUpdated {
            reason: "goal_updated".into(),
        });
    Ok(Json(result))
}

async fn get_goal_file_route(
    headers: HeaderMap,
    Path((slug, rel_path)): Path<(String, String)>,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let mgr = sup.goal_manager();
    let text = mgr.read_goal_file(&slug, &rel_path).map_err(map_gm_error)?;
    let ctype = if rel_path.ends_with(".json") {
        "application/json; charset=utf-8"
    } else {
        "text/plain; charset=utf-8"
    };
    Ok(([(header::CONTENT_TYPE, ctype)], text))
}

async fn list_spaces_route(
    headers: HeaderMap,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let spaces = sup.spaces().list_spaces().map_err(map_gm_error)?;
    Ok(Json(spaces))
}

async fn create_space_route(
    headers: HeaderMap,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
    Json(body): Json<CreateSpaceBody>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    if !sup.config().muse.enable_spaces {
        return Err(ApiErr::msg(
            StatusCode::FORBIDDEN,
            "Spaces feature is disabled in MuseConfig",
        ));
    }
    let kind = match body
        .kind
        .as_deref()
        .unwrap_or("static")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "static" => SpaceKind::Static,
        "ts" | "typescript" => SpaceKind::Ts,
        other => {
            return Err(ApiErr::msg(
                StatusCode::BAD_REQUEST,
                format!("Unknown Space kind '{other}'"),
            ));
        }
    };
    let initial = body.html.as_deref().or(body.content.as_deref());
    let created = sup
        .spaces()
        .create_space(&body.slug, body.title.as_deref(), kind, initial)
        .map_err(map_gm_error)?;
    let _ = sup
        .daemon_event_sender()
        .send(DaemonEvent::ActivityUpdated {
            reason: "space_created".into(),
        });
    Ok((StatusCode::CREATED, Json(created)))
}

async fn get_space_url_route(
    headers: HeaderMap,
    Path(slug): Path<String>,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    if !sup.config().muse.enable_spaces {
        return Err(ApiErr::msg(
            StatusCode::FORBIDDEN,
            "Spaces feature is disabled in MuseConfig",
        ));
    }
    let sp_port = state.supervisor.spaces_port();
    let url = sup
        .spaces()
        .mint_url_for_slug(&slug, sp_port)
        .map_err(map_gm_error)?;
    let preview_parent_origin = state
        .hosted_plane
        .is_none()
        .then(|| format!("http://127.0.0.1:{}", state.supervisor.companion_port()));
    Ok(Json(serde_json::json!({
        "slug": slug,
        "url": url,
        "spaces_port": sp_port,
        "preview_parent_origin": preview_parent_origin,
        "expires_in_secs": SPACE_TOKEN_TTL_SECS,
    })))
}

async fn publish_space_route(
    headers: HeaderMap,
    Path(slug): Path<String>,
    State(state): State<CompanionWebState>,
    body: Option<Json<PublishSpaceBody>>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    if !state.supervisor.config().muse.enable_spaces {
        return Err(ApiErr::msg(
            StatusCode::FORBIDDEN,
            "Spaces feature is disabled in MuseConfig",
        ));
    }
    let confirmed = body.and_then(|b| b.0.confirmed).unwrap_or(false);
    let published = state
        .supervisor
        .spaces()
        .publish_space(&slug, confirmed)
        .map_err(map_gm_error)?;
    let _ = state
        .supervisor
        .daemon_event_sender()
        .send(DaemonEvent::ActivityUpdated {
            reason: "space_published".into(),
        });
    Ok(Json(published))
}

async fn list_skills_route(
    headers: HeaderMap,
    Query(q): Query<CatalogQuery>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let reg = state.supervisor.skill_registry().map_err(map_gm_error)?;
    if let Some(name) = q.load.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let loaded = reg.load_skill(name).map_err(map_gm_error)?;
        return Ok(Json(serde_json::to_value(loaded).map_err(|e| {
            ApiErr::msg(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?));
    }
    let entries = reg.search_skills(q.q.as_deref().unwrap_or(""));
    Ok(Json(serde_json::to_value(entries).map_err(|e| {
        ApiErr::msg(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?))
}

async fn list_connectors_route(
    headers: HeaderMap,
    Query(q): Query<CatalogQuery>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let reg = SkillRegistry::open_with_scrubber(
        &state.supervisor.shared_paths(),
        state.supervisor.scrubber().clone(),
    )
    .map_err(map_gm_error)?;
    if let Some(id) = q.load.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let loaded = reg.load_connector(id).map_err(map_gm_error)?;
        return Ok(Json(serde_json::to_value(loaded).map_err(|e| {
            ApiErr::msg(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?));
    }
    let entries = reg.search_connectors(q.q.as_deref().unwrap_or(""));
    Ok(Json(serde_json::to_value(entries).map_err(|e| {
        ApiErr::msg(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?))
}

async fn create_custom_skill_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<CreateCustomSkillBody>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    if !state.supervisor.config().muse.enable_skills {
        return Err(ApiErr::msg(
            StatusCode::FORBIDDEN,
            "Skills feature is disabled in MuseConfig",
        ));
    }
    let mut reg = SkillRegistry::open_with_scrubber(
        &state.supervisor.shared_paths(),
        state.supervisor.scrubber().clone(),
    )
    .map_err(map_gm_error)?;
    let manifest = SkillYaml {
        name: body.name.clone(),
        title: body.title,
        description: body.description,
        category: body.category.unwrap_or_else(|| "custom".into()),
        include_in_prompt: body.include_in_prompt.unwrap_or(true),
        skill_improvement: None,
        amends: None,
    };
    let dir = reg
        .scaffold_custom_skill(manifest, &body.body_markdown)
        .map_err(map_gm_error)?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "name": body.name,
            "path": dir,
        })),
    ))
}

fn resolve_allowed_identity_path(
    state: &CompanionWebState,
    bot_id: &str,
    file: &str,
) -> std::result::Result<PathBuf, ApiErr> {
    if state.supervisor.roster().get_bot(bot_id).is_none() {
        return Err(ApiErr::msg(
            StatusCode::NOT_FOUND,
            format!("Unknown bot '{bot_id}'"),
        ));
    }
    let bot_paths = state.supervisor.roster().runtime_paths_for_bot(bot_id);
    let shared = state.supervisor.shared_paths();
    match file {
        "SOUL.md" => bot_paths
            .soul_md()
            .ok_or_else(|| ApiErr::msg(StatusCode::NOT_FOUND, "Missing bot SOUL.md")),
        "IDENTITY.md" => bot_paths
            .identity_md()
            .ok_or_else(|| ApiErr::msg(StatusCode::NOT_FOUND, "Missing bot IDENTITY.md")),
        "AGENTS.md" => bot_paths
            .agents_md()
            .ok_or_else(|| ApiErr::msg(StatusCode::NOT_FOUND, "Missing bot AGENTS.md")),
        "HEARTBEAT.md" => bot_paths
            .heartbeat_md()
            .ok_or_else(|| ApiErr::msg(StatusCode::NOT_FOUND, "Missing bot HEARTBEAT.md")),
        "TOOLS.md" => bot_paths
            .tools_md()
            .ok_or_else(|| ApiErr::msg(StatusCode::NOT_FOUND, "Missing bot TOOLS.md")),
        "USER.md" => Ok(shared.user_md()),
        "MEMORY.md" => Ok(shared.memory_md()),
        "PROACTIVE_PREFERENCES.md" => Ok(shared.proactive_preferences_md()),
        _ => Err(ApiErr::msg(
            StatusCode::BAD_REQUEST,
            format!(
                "Unsupported runtime file '{file}'; allowed: SOUL.md, IDENTITY.md, AGENTS.md, HEARTBEAT.md, TOOLS.md, USER.md, MEMORY.md, PROACTIVE_PREFERENCES.md"
            ),
        )),
    }
}

async fn read_identity_file_route(
    headers: HeaderMap,
    Path((bot_id, file)): Path<(String, String)>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let path = resolve_allowed_identity_path(&state, &bot_id, &file)?;
    let pair = SharedFileStore::global()
        .read_text_with_sha256(&path)
        .map_err(map_gm_error)?;
    let (exists, content, sha256) = match pair {
        Some((c, sha)) => (true, c, Some(sha)),
        None => (false, String::new(), None),
    };
    Ok(Json(serde_json::json!({
        "bot_id": bot_id,
        "file": file,
        "exists": exists,
        "content": content,
        "sha256": sha256,
    })))
}

async fn write_identity_file_route(
    headers: HeaderMap,
    Path((bot_id, file)): Path<(String, String)>,
    State(state): State<CompanionWebState>,
    Json(body): Json<PutIdentityFileBody>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let path = resolve_allowed_identity_path(&state, &bot_id, &file)?;
    if state.supervisor.scrubber().scrub_str(&body.content) != body.content {
        return Err(ApiErr::msg(
            StatusCode::BAD_REQUEST,
            "Refused plaintext secret in runtime file",
        ));
    }
    let existing = SharedFileStore::global()
        .read_text_with_sha256(&path)
        .map_err(map_gm_error)?;
    let expected_opt = body.expected_sha256.as_deref();
    if existing.is_some() && expected_opt.is_none() {
        return Err(ApiErr::msg(
            StatusCode::BAD_REQUEST,
            "expected_sha256 is required when overwriting an existing runtime file",
        ));
    }
    let outcome = SharedFileStore::global()
        .write_exact_sync(&path, expected_opt, &body.content)
        .map_err(map_gm_error)?;
    match outcome {
        SharedUpdateOutcome::Conflict { current_sha256 } => Err(ApiErr::conflict(
            "Optimistic concurrency conflict: expected_sha256 does not match",
            current_sha256,
        )),
        SharedUpdateOutcome::Updated { sha256, bytes } => {
            if file == "PROACTIVE_PREFERENCES.md" {
                let _ = state.supervisor.sync_quiet_gate_from_prefs();
            }
            let _ = state
                .supervisor
                .daemon_event_sender()
                .send(DaemonEvent::ActivityUpdated {
                    reason: "identity_updated".into(),
                });
            Ok(Json(serde_json::json!({
                "ok": true,
                "bot_id": bot_id,
                "file": file,
                "sha256": sha256,
                "bytes": bytes,
            })))
        }
    }
}

async fn get_proactive_prefs_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let snap = state
        .supervisor
        .prefs_store()
        .load()
        .map_err(map_gm_error)?;
    Ok(Json(snap))
}

async fn put_proactive_prefs_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<UpdateProactivePrefsBody>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let store = state.supervisor.prefs_store();
    if let Some(topic) = body
        .add_tell
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        store.add_tell_me_about(topic).map_err(map_gm_error)?;
    }
    if let Some(topic) = body
        .add_opt_out
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        store
            .add_permanent_opt_out(topic, Some(state.supervisor.quiet_gate()))
            .map_err(map_gm_error)?;
    }
    if let Some(topic) = body
        .remove_opt_out
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        store
            .remove_permanent_opt_out(topic, Some(state.supervisor.quiet_gate()))
            .map_err(map_gm_error)?;
    }
    if let (Some(start_hr), Some(end_hr)) = (body.quiet_start_hour, body.quiet_end_hour) {
        store
            .set_quiet_hours(start_hr, end_hr, Some(state.supervisor.quiet_gate()))
            .map_err(map_gm_error)?;
    }
    if let Some(home_tz) = body
        .home_tz
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let cfg = TimezonesConfig {
            home_tz: home_tz.to_string(),
            work_tz: body
                .work_tz
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        };
        store.write_timezones(&cfg).map_err(map_gm_error)?;
    }
    if let Some(note) = body
        .feedback_note
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        store.add_feedback_note(note).map_err(map_gm_error)?;
    }
    let _ = state.supervisor.sync_quiet_gate_from_prefs();
    let updated = store.load().map_err(map_gm_error)?;
    Ok(Json(updated))
}

async fn import_memory_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<ImportMemoryBody>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    if body.input.len() > MAX_IMPORT_BYTES {
        return Err(ApiErr::msg(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("Import payload exceeds {} byte limit", MAX_IMPORT_BYTES),
        ));
    }
    let bot_id = body
        .bot_id
        .as_deref()
        .unwrap_or(&state.supervisor.config().openbot.default_bot_id);
    let paths = state.supervisor.shared_paths();
    let memory = state
        .supervisor
        .roster()
        .memory_store(bot_id)
        .map_err(map_gm_error)?;
    let report = import_memory_portable_with_stores(
        &body.input,
        &paths,
        Some(state.supervisor.provenance()),
        Some(&memory),
        body.apply,
        body.report_sha256.as_deref(),
    )
    .map_err(map_gm_error)?;
    if report.applied {
        let _ = state
            .supervisor
            .daemon_event_sender()
            .send(DaemonEvent::ActivityUpdated {
                reason: "memory_imported".into(),
            });
    }
    Ok(Json(report))
}

async fn explain_memory_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<ExplainMemoryBody>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let explanations = state
        .supervisor
        .provenance()
        .explain_memory(&body.target)
        .map_err(map_gm_error)?;
    Ok(Json(explanations))
}

async fn forget_plan_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<ForgetPlanBody>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let plan = state
        .supervisor
        .plan_forget(body.bot_id.as_deref(), &body.request)
        .map_err(map_gm_error)?;
    Ok(Json(plan))
}

async fn forget_confirm_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<ForgetConfirmBody>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let report = state
        .supervisor
        .confirm_forget(
            body.bot_id.as_deref(),
            &body.plan_id,
            body.verification_query.as_deref(),
            body.confirmed,
        )
        .map_err(map_gm_error)?;
    Ok(Json(report))
}

async fn get_side_chat_discretion_route(
    headers: HeaderMap,
    Path(chat_id): Path<String>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let policy = state
        .supervisor
        .discretion()
        .load_or_init(&chat_id)
        .map_err(map_gm_error)?;
    Ok(Json(policy))
}

async fn put_side_chat_discretion_route(
    headers: HeaderMap,
    Path(chat_id): Path<String>,
    State(state): State<CompanionWebState>,
    Json(body): Json<PutDiscretionBody>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let mut rules: Vec<String> = body
        .rules
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if let Some(text) = body.rules_text {
        for line in text.lines() {
            let clean = line.trim();
            if !clean.is_empty() {
                rules.push(clean.to_string());
            }
        }
    }
    if rules.is_empty() {
        return Err(ApiErr::msg(
            StatusCode::BAD_REQUEST,
            "At least one discretion rule is required",
        ));
    }
    let policy = state
        .supervisor
        .discretion()
        .update_user_rules(&chat_id, &rules, body.expected_sha256.as_deref())
        .map_err(map_gm_error)?;
    state.supervisor.audit().log_governance(
        "discretion_updated",
        Some("muse"),
        AuditCategory::Permitted,
        if body.loosens {
            "RULE-PRIVACY-DISCRETION"
        } else {
            "RULE-POLICY-001"
        },
        if body.loosens {
            "Operator updated bridge discretion rules (loosened)"
        } else {
            "Operator added bridge discretion user rule"
        },
        format!("Updated DISCRETION.md for chat '{chat_id}'"),
        serde_json::json!({ "chat_id": chat_id, "loosens": body.loosens }),
    );
    Ok(Json(policy))
}

fn update_web_dm_card_status(
    sup: &OpenBotSupervisor,
    request_id: &str,
    status: &str,
    extra: Option<serde_json::Value>,
) {
    if let Ok(Some(updated)) = sup
        .direct_chat()
        .update_card_by_request_id(request_id, status, extra)
        && let Some(card) = updated.card
    {
        let _ = sup
            .daemon_event_sender()
            .send(DaemonEvent::MessageCardUpdated {
                bot_id: updated.bot_id.clone(),
                message_id: updated.id.clone(),
                card: card.clone(),
            });
        sup.emit_agui(
            "CARD_UPDATED",
            request_id,
            &updated.bot_id,
            serde_json::json!({ "message_id": updated.id, "card": card }),
        );
    }
}

async fn check_platform_connected(platform: &Option<Arc<dyn gm_platform::Platform>>) -> bool {
    if let Some(plat) = platform.as_ref() {
        tokio::time::timeout(Duration::from_millis(100), plat.permissions())
            .await
            .map(|r| r.is_ok())
            .unwrap_or(false)
    } else {
        false
    }
}

async fn get_bootstrap_route(
    headers: HeaderMap,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let helper_connected = check_platform_connected(&state.platform).await;
    let daily_spend = state
        .budget
        .as_ref()
        .map(|b| b.get_daily_spent())
        .unwrap_or(0.0);
    let paused = sup.runtime_paused.load(std::sync::atomic::Ordering::SeqCst);
    let mut snap = sup.bootstrap_snapshot(daily_spend, paused, helper_connected);
    if let Some(obj) = snap.as_object_mut() {
        let ui_settings =
            crate::get_ui_settings_snapshot(&state.shared_config.read().clone(), &sup);
        obj.insert("ui_settings".into(), ui_settings);
    }
    Ok(Json(snap))
}

async fn get_ui_settings_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let cfg = state.shared_config.read().clone();
    let snap = crate::get_ui_settings_snapshot(&cfg, &state.supervisor);
    Ok(Json(snap))
}

async fn put_ui_settings_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let updated = crate::apply_and_save_ui_settings(
        &state.shared_config,
        state.config_path.as_deref(),
        &state.supervisor,
        &body,
    )
    .map_err(map_gm_error)?;
    Ok(Json(updated))
}

async fn upsert_bot_route(
    headers: HeaderMap,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
    Json(profile): Json<crate::roster::BotProfile>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let saved = sup.roster().upsert_bot(profile).map_err(map_gm_error)?;
    let val = serde_json::to_value(&saved).unwrap_or_default();
    let _ = sup.daemon_event_sender().send(DaemonEvent::BotUpdated {
        bot_id: saved.id.clone(),
    });
    sup.emit_agui("BOT_UPDATED", &saved.id, &saved.id, val);
    Ok((StatusCode::CREATED, Json(saved)))
}

async fn delete_bot_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let deleted = state
        .supervisor
        .roster()
        .remove_bot(&id)
        .map_err(map_gm_error)?;
    let _ = state
        .supervisor
        .daemon_event_sender()
        .send(DaemonEvent::BotUpdated { bot_id: id.clone() });
    state.supervisor.emit_agui(
        "BOT_DELETED",
        &id,
        &id,
        serde_json::json!({ "bot_id": id, "deleted": deleted }),
    );
    Ok(Json(
        serde_json::json!({ "bot_id": id, "deleted": deleted }),
    ))
}

async fn duplicate_bot_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
    body: Option<Json<serde_json::Value>>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let new_id = body
        .as_ref()
        .and_then(|b| b.get("new_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty());
    let new_name = body
        .as_ref()
        .and_then(|b| b.get("new_display_name").or_else(|| b.get("new_name")))
        .and_then(|v| v.as_str());
    let dup = state
        .supervisor
        .roster()
        .duplicate_bot(&id, new_id, new_name)
        .map_err(map_gm_error)?;
    let val = serde_json::to_value(&dup).unwrap_or_default();
    let _ = state
        .supervisor
        .daemon_event_sender()
        .send(DaemonEvent::BotUpdated {
            bot_id: dup.id.clone(),
        });
    state
        .supervisor
        .emit_agui("BOT_UPDATED", &dup.id, &dup.id, val);
    Ok((StatusCode::CREATED, Json(dup)))
}

async fn put_bot_ui_prefs_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
    Json(prefs): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let saved = state
        .supervisor
        .roster()
        .save_bot_ui_prefs(&id, &prefs)
        .map_err(map_gm_error)?;
    let val = serde_json::to_value(&saved).unwrap_or_default();
    let _ = state
        .supervisor
        .daemon_event_sender()
        .send(DaemonEvent::BotUpdated {
            bot_id: saved.id.clone(),
        });
    state
        .supervisor
        .emit_agui("BOT_UPDATED", &saved.id, &saved.id, val);
    Ok(Json(saved))
}

async fn get_bot_avatar_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<Response, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let Some((asset, bytes)) = state
        .supervisor
        .roster()
        .read_bot_avatar_bytes(&id)
        .map_err(map_gm_error)?
    else {
        return Err(ApiErr::msg(
            StatusCode::NOT_FOUND,
            "Bot has no custom avatar",
        ));
    };
    let lower = asset.to_ascii_lowercase();
    let mime = if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else {
        "image/png"
    };
    Ok(([(header::CONTENT_TYPE, mime)], bytes).into_response())
}

async fn post_bot_avatar_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let ext = body
        .get("extension")
        .and_then(|v| v.as_str())
        .unwrap_or("png");
    let b64 = body
        .get("bytes_base64")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiErr::msg(StatusCode::BAD_REQUEST, "bytes_base64 is required"))?;
    use base64::Engine as _;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| ApiErr::msg(StatusCode::BAD_REQUEST, format!("Invalid base64: {e}")))?;
    let avatar_path = state
        .supervisor
        .roster()
        .save_bot_avatar_bytes(&id, ext, &raw)
        .map_err(map_gm_error)?;
    let bot = state.supervisor.roster().get_bot(&id);
    let val = serde_json::json!({
        "bot_id": id,
        "avatar_path": avatar_path,
        "bot": bot,
    });
    let _ = state
        .supervisor
        .daemon_event_sender()
        .send(DaemonEvent::BotUpdated { bot_id: id.clone() });
    state
        .supervisor
        .emit_agui("BOT_UPDATED", &id, &id, val.clone());
    Ok(Json(val))
}

async fn list_bot_files_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let files = sup
        .roster()
        .workspace_sandbox(&id)
        .map_err(map_gm_error)?
        .shared_files()
        .map_err(map_gm_error)?;
    let items: Vec<serde_json::Value> = files
        .into_iter()
        .map(|(path, size)| serde_json::json!({ "path": path, "size": size }))
        .collect();
    Ok(Json(serde_json::json!({ "bot_id": id, "files": items })))
}

#[derive(Debug, Deserialize)]
struct PreviewBotFileQuery {
    path: String,
}

async fn preview_bot_file_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<PreviewBotFileQuery>,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let bytes = sup
        .roster()
        .workspace_sandbox(&id)
        .map_err(map_gm_error)?
        .preview_shared_file(&q.path)
        .map_err(map_gm_error)?;
    let preview =
        crate::attachments::preview_attachment_bounded(&q.path, &bytes).map_err(map_gm_error)?;
    use base64::Engine as _;
    let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else if bytes.get(4..8) == Some(b"ftyp") {
        Some("video/mp4")
    } else {
        None
    };
    let text = if mime.is_none() {
        String::from_utf8_lossy(&bytes).to_string()
    } else {
        String::new()
    };
    let data_url = mime.map(|mime| {
        format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        )
    });
    Ok(Json(serde_json::json!({
        "bot_id": id,
        "path": q.path,
        "text": text,
        "mime": mime,
        "data_url": data_url,
        "preview": preview,
    })))
}

async fn upload_bot_attachment_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    if let Some(src) = body
        .get("source_path")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        let dest = state
            .supervisor
            .import_attachment(&id, std::path::Path::new(src))
            .map_err(map_gm_error)?;
        return Ok((
            StatusCode::CREATED,
            Json(serde_json::json!({
                "bot_id": id,
                "attachment_path": dest,
            })),
        ));
    }
    let filename = body
        .get("filename")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ApiErr::msg(StatusCode::BAD_REQUEST, "filename is required"))?;
    let b64 = body
        .get("bytes_base64")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiErr::msg(StatusCode::BAD_REQUEST, "bytes_base64 is required"))?;
    use base64::Engine as _;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| ApiErr::msg(StatusCode::BAD_REQUEST, format!("Invalid base64: {e}")))?;
    if raw.len() > 25 * 1024 * 1024 {
        return Err(ApiErr::msg(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Attachment exceeds 25 MiB limit",
        ));
    }
    let tmp_dir = state
        .supervisor
        .shared_paths()
        .storage_dir()
        .join("workspace/tmp_uploads");
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| ApiErr::msg(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let safe_leaf: String = std::path::Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("attachment.bin")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    let safe_leaf = if safe_leaf.is_empty() {
        "attachment.bin".to_string()
    } else {
        safe_leaf
    };
    let tmp_file = tmp_dir.join(safe_leaf);
    std::fs::write(&tmp_file, &raw)
        .map_err(|e| ApiErr::msg(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let res = state.supervisor.import_attachment(&id, &tmp_file);
    let _ = std::fs::remove_file(&tmp_file);
    let dest = res.map_err(map_gm_error)?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "bot_id": id,
            "attachment_path": dest,
        })),
    ))
}

#[derive(Debug, Deserialize)]
struct LibraryQuery {
    filter: Option<String>,
    cursor: Option<usize>,
    limit: Option<usize>,
}

async fn list_bot_library_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<LibraryQuery>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let page = state
        .supervisor
        .list_library_page(
            &id,
            q.filter.as_deref().unwrap_or("all"),
            q.cursor.unwrap_or(0),
            q.limit.unwrap_or(5).clamp(1, 50),
        )
        .map_err(map_gm_error)?;
    Ok(Json(page))
}

async fn pin_library_item_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let bot_id = body
        .get("bot_id")
        .and_then(|v| v.as_str())
        .unwrap_or("muse");
    let pinned = body.get("pinned").and_then(|v| v.as_bool()).unwrap_or(true);
    let updated = state
        .supervisor
        .library()
        .set_pinned(bot_id, &id, pinned)
        .map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({
        "bot_id": bot_id,
        "id": id,
        "pinned": pinned,
        "updated": updated,
    })))
}

async fn list_sections_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let sections = state
        .supervisor
        .sections()
        .list_sections()
        .map_err(map_gm_error)?;
    Ok(Json(sections))
}

async fn upsert_section_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let id = body
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty());
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Section");
    let collapsed = body.get("collapsed").and_then(|v| v.as_bool());
    let sort_order = body
        .get("sort_order")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);
    let sec = state
        .supervisor
        .sections()
        .upsert_section(id, name, sort_order, collapsed)
        .map_err(map_gm_error)?;
    let _ = state
        .supervisor
        .daemon_event_sender()
        .send(DaemonEvent::ActivityUpdated {
            reason: "sections_updated".into(),
        });
    state.supervisor.emit_agui(
        "SECTION_UPDATED",
        &sec.id,
        "",
        serde_json::to_value(&sec).unwrap_or_default(),
    );
    Ok((StatusCode::CREATED, Json(sec)))
}

async fn patch_section_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty());
    let collapsed = body.get("collapsed").and_then(|v| v.as_bool());
    let sort_order = body
        .get("sort_order")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);
    if let Some(new_name) = name {
        let _ = state
            .supervisor
            .sections()
            .upsert_section(Some(&id), new_name, sort_order, collapsed)
            .map_err(map_gm_error)?;
    } else if let Some(c) = collapsed {
        let _ = state
            .supervisor
            .sections()
            .set_collapsed(&id, c)
            .map_err(map_gm_error)?;
    }
    let sections = state
        .supervisor
        .sections()
        .list_sections()
        .map_err(map_gm_error)?;
    let _ = state
        .supervisor
        .daemon_event_sender()
        .send(DaemonEvent::ActivityUpdated {
            reason: "sections_updated".into(),
        });
    state.supervisor.emit_agui(
        "SECTIONS_UPDATED",
        &id,
        "",
        serde_json::to_value(&sections).unwrap_or_default(),
    );
    Ok(Json(sections))
}

async fn delete_section_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let deleted = state
        .supervisor
        .delete_section_and_clear_bots(&id)
        .map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({ "id": id, "deleted": deleted })))
}

#[derive(Debug, Deserialize)]
struct DmListQuery {
    before_seq: Option<u64>,
    limit: Option<usize>,
}

async fn list_dm_messages_route(
    headers: HeaderMap,
    Path(bot_id): Path<String>,
    Query(q): Query<DmListQuery>,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let msgs = sup
        .direct_chat()
        .list_messages(&bot_id, q.before_seq, q.limit.unwrap_or(50))
        .map_err(map_gm_error)?;
    Ok(Json(msgs))
}

#[derive(Debug, Deserialize)]
struct PostDmBody {
    content: String,
    reply_to_id: Option<String>,
    thread_id: Option<String>,
    client_msg_id: Option<String>,
}

async fn post_dm_message_route(
    headers: HeaderMap,
    Path(bot_id): Path<String>,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
    Json(body): Json<PostDmBody>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    if let (Some(llm), Some(router)) = (state.llm_client.as_ref(), state.router.as_ref()) {
        let cfg = state.shared_config.read().clone();
        let outcome = sup
            .dispatch_direct_chat(
                &bot_id,
                &body.content,
                body.reply_to_id.as_deref(),
                body.thread_id.as_deref(),
                body.client_msg_id.as_deref(),
                llm,
                router,
                state.budget.as_deref(),
                state.kill_switch.as_ref(),
                Some(&cfg),
            )
            .await
            .map_err(map_gm_error)?;
        return Ok(Json(serde_json::to_value(outcome).unwrap_or_default()));
    }
    let trimmed = body.content.trim();
    let clean_client_id = body
        .client_msg_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let user_msg = if let Some(cid) = clean_client_id {
        if let Some((existing_user, existing_reply)) = sup
            .direct_chat()
            .find_turn_by_client_id(&bot_id, cid)
            .map_err(map_gm_error)?
        {
            if let Some(reply) = existing_reply {
                return Ok(Json(serde_json::json!({
                    "mode": "chat",
                    "user_message": existing_user,
                    "assistant_message": reply,
                    "queued_task": null,
                    "reply_error": null,
                    "provider_setup_required": false,
                })));
            }
            existing_user
        } else {
            sup.post_direct_chat_with_id(
                &bot_id,
                "user",
                "You",
                &body.content,
                body.reply_to_id.as_deref(),
                body.thread_id.as_deref(),
                None,
                Some(cid),
                Some(cid),
            )
            .map_err(map_gm_error)?
        }
    } else {
        sup.post_direct_chat(
            &bot_id,
            "user",
            "You",
            &body.content,
            body.reply_to_id.as_deref(),
            body.thread_id.as_deref(),
            None,
            None,
        )
        .map_err(map_gm_error)?
    };
    if let Some(inst) = trimmed
        .strip_prefix("/do ")
        .or_else(|| trimmed.strip_prefix("/task "))
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let task = sup
            .submit_task(Some(&bot_id), None, inst, 0)
            .map_err(map_gm_error)?;
        return Ok(Json(serde_json::json!({
            "mode": "task",
            "user_message": user_msg,
            "assistant_message": null,
            "queued_task": task,
        })));
    }
    let assistant_msg = sup
        .post_direct_chat(
            &bot_id,
            "assistant",
            &bot_id,
            &format!("Acknowledged by {bot_id}: {trimmed}"),
            Some(&user_msg.id),
            body.thread_id.as_deref(),
            None,
            clean_client_id,
        )
        .map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({
        "mode": "chat",
        "user_message": user_msg,
        "assistant_message": assistant_msg,
        "queued_task": null,
        "reply_error": null,
        "provider_setup_required": false,
    })))
}

#[derive(Debug, Deserialize)]
struct SearchMessagesQuery {
    q: Option<String>,
    include_older: Option<bool>,
    bot_id: Option<String>,
    limit: Option<usize>,
}

async fn search_messages_route(
    headers: HeaderMap,
    Query(q): Query<SearchMessagesQuery>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let query = q.q.as_deref().unwrap_or("");
    let include_older = q.include_older.unwrap_or(false);
    let limit = q.limit.unwrap_or(40).clamp(1, 200);
    let dm_hits = state
        .supervisor
        .direct_chat()
        .search_messages(query, q.bot_id.as_deref(), include_older, limit)
        .map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({
        "hits": dm_hits,
    })))
}

async fn mark_chat_read_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let chat_kind = body
        .get("chat_kind")
        .and_then(|v| v.as_str())
        .unwrap_or("dm");
    let chat_id = body
        .get("chat_id")
        .and_then(|v| v.as_str())
        .unwrap_or("muse");
    let seq = state
        .supervisor
        .direct_chat()
        .mark_chat_read(chat_kind, chat_id)
        .map_err(map_gm_error)?;
    let unread = state
        .supervisor
        .direct_chat()
        .unread_counts()
        .map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({
        "chat_kind": chat_kind,
        "chat_id": chat_id,
        "last_read_seq": seq,
        "unread_counts": unread,
    })))
}

async fn patch_channel_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let topic = body.get("topic").and_then(|v| v.as_str());
    let instructions = body.get("instructions").and_then(|v| v.as_str());
    let paused = body.get("paused").and_then(|v| v.as_bool());
    let archived = body.get("archived").and_then(|v| v.as_bool());
    let chan = state
        .supervisor
        .channels()
        .update_channel_metadata(&id, topic, instructions, paused, archived)
        .map_err(map_gm_error)?;
    let val = serde_json::to_value(&chan).unwrap_or_default();
    let _ = state
        .supervisor
        .daemon_event_sender()
        .send(DaemonEvent::ActivityUpdated {
            reason: "channel_updated".into(),
        });
    state.supervisor.emit_agui("CHANNEL_UPDATED", &id, "", val);
    Ok(Json(chan))
}

async fn respond_approval_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let approved = body
        .get("approved")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let remember_rule = body
        .get("remember_rule")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let resolved = state.interactions.resolve_approval(&id, approved).await;
    if resolved && let Some(pattern) = remember_rule {
        let mode = if approved {
            crate::rules::RuleMode::Allow
        } else {
            crate::rules::RuleMode::AskFirst
        };
        sup.rules()
            .upsert_rule(None, mode, pattern, None, "Saved from approval card", true)
            .map_err(map_gm_error)?;
    }
    let final_status = if resolved {
        if approved { "approved" } else { "denied" }
    } else {
        "expired"
    };
    update_web_dm_card_status(
        &sup,
        &id,
        final_status,
        if resolved {
            remember_rule.map(|r| serde_json::json!({ "remember_rule": r }))
        } else {
            None
        },
    );
    let _ = sup.daemon_event_sender().send(if resolved {
        DaemonEvent::ApprovalResolved {
            request_id: id.clone(),
        }
    } else {
        DaemonEvent::ApprovalExpired {
            request_id: id.clone(),
        }
    });
    Ok(Json(serde_json::json!({
        "request_id": id,
        "approved": approved,
        "status": final_status,
        "rule_saved": resolved && remember_rule.is_some(),
        "resolved": resolved,
    })))
}

async fn submit_secret_prompt_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let cancelled = body
        .get("cancelled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let save_to_vault = body
        .get("save_to_vault")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let bot_id = body
        .get("bot_id")
        .and_then(|v| v.as_str())
        .unwrap_or("muse");
    let mut fields: Vec<(String, String)> = Vec::new();
    if let Some(arr) = body.get("fields").and_then(|v| v.as_array()) {
        for entry in arr {
            if let Some(pair) = entry.as_array()
                && pair.len() == 2
                && let (Some(k), Some(v)) = (pair[0].as_str(), pair[1].as_str())
            {
                fields.push((k.to_string(), v.to_string()));
            } else if let Some(obj) = entry.as_object()
                && let (Some(k), Some(v)) = (
                    obj.get("key").and_then(|x| x.as_str()),
                    obj.get("value").and_then(|x| x.as_str()),
                )
            {
                fields.push((k.to_string(), v.to_string()));
            }
        }
    } else if let Some(s) = body.get("secret").and_then(|v| v.as_str()) {
        fields.push(("password".to_string(), s.to_string()));
    }
    let mut vault_item_id: Option<String> = None;
    if !cancelled {
        for (k, v) in &fields {
            if !v.trim().is_empty() {
                state
                    .supervisor
                    .scrubber()
                    .register(format!("prompt:{id}:{k}"), v);
                let _ = state
                    .supervisor
                    .vault()
                    .backend()
                    .set_secret(&format!("gm_prompt_{id}_{k}"), v);
            }
        }
        if save_to_vault && !fields.is_empty() {
            let label = body
                .get("label")
                .or_else(|| body.get("vault_label"))
                .and_then(|v| v.as_str())
                .unwrap_or(&id)
                .to_string();
            let domain = body
                .get("domain")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let username = fields
                .iter()
                .find(|(k, _)| {
                    let kl = k.to_ascii_lowercase();
                    kl.contains("user") || kl.contains("email")
                })
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            let secret_val = fields.last().map(|(_, v)| v.clone());
            if let Ok(saved) = state
                .supervisor
                .vault()
                .upsert_item(crate::vault::VaultItemInput {
                    id: None,
                    kind: crate::vault::VaultItemKind::Login,
                    label,
                    subtitle_or_last4: domain.clone(),
                    username,
                    domain,
                    hint: String::new(),
                    secret_value: secret_val,
                    external_secret_ref: None,
                    is_default_payment: false,
                    bot_scope: Some(bot_id.to_string()),
                })
            {
                vault_item_id = Some(saved.id);
            }
        }
    }
    let resolved = state
        .interactions
        .resolve_secret(
            &id,
            if cancelled || fields.is_empty() {
                None
            } else {
                Some(fields)
            },
        )
        .await;
    update_web_dm_card_status(
        &state.supervisor,
        &id,
        if cancelled { "cancelled" } else { "submitted" },
        Some(serde_json::json!({
            "save_to_vault": save_to_vault,
            "vault_item_id": vault_item_id,
        })),
    );
    Ok(Json(serde_json::json!({
        "request_id": id,
        "cancelled": cancelled,
        "resolved": resolved,
        "vault_item_id": vault_item_id,
    })))
}

async fn respond_question_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let mut parts: Vec<String> = Vec::new();
    if let Some(arr) = body.get("answers").and_then(|v| v.as_array()) {
        for item in arr {
            if let Some(s) = item.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                parts.push(s.to_string());
            }
        }
    } else if let Some(s) = body
        .get("answer")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(s.to_string());
    }
    if let Some(other) = body
        .get("other_text")
        .or_else(|| body.get("custom_text"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(other.to_string());
    }
    let combined = if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    };
    let resolved = state
        .interactions
        .resolve_question(&id, combined.clone())
        .await;
    update_web_dm_card_status(
        &state.supervisor,
        &id,
        if combined.is_some() {
            "answered"
        } else {
            "dismissed"
        },
        combined
            .as_ref()
            .map(|a| serde_json::json!({ "answer": a })),
    );
    Ok(Json(serde_json::json!({
        "request_id": id,
        "resolved": resolved,
        "answer": combined,
    })))
}

async fn list_rules_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let rules = state
        .supervisor
        .rules()
        .list_rules()
        .map_err(map_gm_error)?;
    Ok(Json(rules))
}

async fn upsert_rule_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let id = body
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty());
    let pattern = body.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
    let mode = crate::rules::RuleMode::parse(
        body.get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("ask_first"),
    )
    .map_err(map_gm_error)?;
    let scope_bot_id = body
        .get("scope_bot_id")
        .or_else(|| body.get("bot_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty() && *s != "all");
    let description = body
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let enabled = body
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let rule = state
        .supervisor
        .rules()
        .upsert_rule(id, mode, pattern, scope_bot_id, description, enabled)
        .map_err(map_gm_error)?;
    Ok((StatusCode::CREATED, Json(rule)))
}

async fn delete_rule_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let deleted = state
        .supervisor
        .rules()
        .delete_rule(&id)
        .map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({ "id": id, "deleted": deleted })))
}

async fn list_vault_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let items = state
        .supervisor
        .vault()
        .list_items(None)
        .map_err(map_gm_error)?;
    Ok(Json(items))
}

async fn upsert_vault_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let input: crate::vault::VaultItemInput = if body.get("subtitle_or_last4").is_some() {
        serde_json::from_value(body)
            .map_err(|e| ApiErr::msg(StatusCode::BAD_REQUEST, e.to_string()))?
    } else {
        let id = body
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(ToString::to_string);
        let kind = crate::vault::VaultItemKind::parse(
            body.get("kind").and_then(|v| v.as_str()).unwrap_or("login"),
        )
        .map_err(map_gm_error)?;
        let label = body
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("Credential")
            .to_string();
        let subtitle_or_last4 = body
            .get("subtitle")
            .or_else(|| body.get("last4"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let username = body
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let domain = body
            .get("domain")
            .or_else(|| body.get("service"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let hint = body
            .get("hint")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let secret_value = body
            .get("secret_value")
            .or_else(|| body.get("secret"))
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        let external_secret_ref = body
            .get("external_secret_ref")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        let is_default_payment = body
            .get("is_default_payment")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let bot_scope = body
            .get("bot_scope")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        crate::vault::VaultItemInput {
            id,
            kind,
            label,
            subtitle_or_last4,
            username,
            domain,
            hint,
            secret_value,
            external_secret_ref,
            is_default_payment,
            bot_scope,
        }
    };
    let summary = state
        .supervisor
        .vault()
        .upsert_item(input)
        .map_err(map_gm_error)?;
    Ok((StatusCode::CREATED, Json(summary)))
}

async fn import_vault_csv_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let source_hint = body
        .get("source_hint")
        .or_else(|| body.get("format"))
        .and_then(|v| v.as_str());
    let csv = body.get("csv").and_then(|v| v.as_str()).unwrap_or("");
    let report = state
        .supervisor
        .vault()
        .import_csv(csv, source_hint)
        .map_err(map_gm_error)?;
    Ok(Json(report))
}

async fn delete_vault_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let deleted = state
        .supervisor
        .vault()
        .delete_item(&id)
        .map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({ "id": id, "deleted": deleted })))
}

async fn reveal_vault_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
    body: Option<Json<serde_json::Value>>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let confirmed = body
        .and_then(|b| {
            b.get("confirmed")
                .or_else(|| b.get("reveal_confirmed"))
                .and_then(|v| v.as_bool())
        })
        .unwrap_or(false);
    let secret = state
        .supervisor
        .vault()
        .reveal_secret(
            &id,
            state.supervisor.ui_config().hardware_security_keys,
            confirmed,
        )
        .map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({ "id": id, "secret": secret })))
}

async fn set_default_payment_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let updated = state
        .supervisor
        .vault()
        .set_default_payment(&id)
        .map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({ "id": id, "updated": updated })))
}

#[derive(Debug, Deserialize)]
struct UsageQuery {
    days: Option<u32>,
}

async fn get_usage_route(
    headers: HeaderMap,
    Query(q): Query<UsageQuery>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let days = q.days.unwrap_or(7).clamp(1, 90);
    if let Some(ref budget) = state.budget {
        let breakdown = budget.usage_breakdown(days).map_err(map_gm_error)?;
        return Ok(Json(serde_json::to_value(breakdown).unwrap_or_default()));
    }
    let budget_cfg = &state.supervisor.config().budget;
    let fallback = gm_llm::UsageBreakdown {
        daily_spent_usd: 0.0,
        weekly_spent_usd: 0.0,
        daily_cap_usd: budget_cfg.daily_spend_cap_usd,
        per_job_cap_usd: budget_cfg.per_job_spend_cap_usd,
        max_daily_notifications: budget_cfg.max_daily_notifications,
        chatgpt_allowance_requests: 0,
        chatgpt_allowance_tokens: 0,
        by_bot: Vec::new(),
        by_source: Vec::new(),
        daily_series: Vec::new(),
    };
    Ok(Json(serde_json::to_value(fallback).unwrap_or_default()))
}

async fn list_routines_route(
    headers: HeaderMap,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let routines = sup.queue().list_routines().map_err(map_gm_error)?;
    Ok(Json(routines))
}

async fn upsert_routine_route(
    headers: HeaderMap,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let id = body
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("rtn-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Routine")
        .to_string();
    let bot_id = body
        .get("bot_id")
        .and_then(|v| v.as_str())
        .unwrap_or("muse")
        .to_string();
    let channel_id = body
        .get("channel_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(ToString::to_string);
    let instruction = body
        .get("instruction")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let timezone = body
        .get("timezone")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("UTC")
        .to_string();
    let hour = body.get("hour").and_then(|v| v.as_u64()).unwrap_or(9) as u32;
    let minute = body.get("minute").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let weekdays: Vec<u32> = body
        .get("weekdays")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_u64().map(|n| n as u32))
                .collect()
        })
        .unwrap_or_default();
    let quiet_hours = body
        .get("quiet_hours")
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    let interval_minutes = body
        .get("interval_minutes")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let webhook_secret_ref = body
        .get("webhook_secret_ref")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(ToString::to_string);
    let preferred_provider = body
        .get("preferred_provider")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    let preferred_model = body
        .get("preferred_model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    let enabled = body
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let routine = crate::routines::Routine {
        id,
        name,
        bot_id,
        channel_id,
        instruction,
        timezone,
        hour,
        minute,
        weekdays,
        quiet_hours,
        interval_minutes,
        webhook_secret_ref,
        preferred_provider,
        preferred_model,
        enabled,
    };
    sup.save_routine(&routine).map_err(map_gm_error)?;
    Ok((StatusCode::CREATED, Json(routine)))
}

async fn delete_routine_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let deleted = sup.delete_routine(&id).map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({ "id": id, "deleted": deleted })))
}

async fn test_routine_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let run = sup.trigger_routine_test(&id).map_err(map_gm_error)?;
    Ok(Json(run))
}

async fn get_routine_webhook_key_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let key = state
        .supervisor
        .get_routine_webhook_key(&id)
        .map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({
        "routine_id": id,
        "webhook_key": key,
    })))
}

async fn trigger_routine_webhook_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
    body: Option<Json<serde_json::Value>>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    let bearer_ok = check_auth(&headers, state.bearer_token.as_deref()).is_ok();
    let webhook_ok = if !bearer_ok {
        match headers.get("x-webhook-key").and_then(|v| v.to_str().ok()) {
            Some(supplied) => state
                .supervisor
                .verify_routine_webhook_key(&id, supplied)
                .map_err(map_gm_error)?,
            None => false,
        }
    } else {
        true
    };
    if !webhook_ok {
        return Err(ApiErr::msg(
            StatusCode::UNAUTHORIZED,
            "Valid Bearer token or X-Webhook-Key is required",
        ));
    }
    let delivery_id = headers
        .get("x-delivery-id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            body.as_ref()
                .and_then(|b| b.get("delivery_id"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| format!("wh-{}", uuid::Uuid::new_v4().simple()));
    let Some(outcome) = state
        .supervisor
        .trigger_routine(&id, &delivery_id)
        .map_err(map_gm_error)?
    else {
        return Err(ApiErr::msg(
            StatusCode::NOT_FOUND,
            format!("Routine '{id}' not found"),
        ));
    };
    Ok(Json(serde_json::json!({
        "routine_id": id,
        "delivery_id": delivery_id,
        "status": outcome.status,
        "task_id": outcome.task_id,
    })))
}

async fn list_routine_runs_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let runs = sup.list_routine_runs(&id, 25).map_err(map_gm_error)?;
    Ok(Json(runs))
}

async fn list_custom_connectors_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let custom = state
        .supervisor
        .connectors()
        .list_connectors()
        .map_err(map_gm_error)?;
    let integrations = state
        .supervisor
        .connectors()
        .builtin_integrations(&state.shared_config.read().clone());
    Ok(Json(serde_json::json!({
        "integrations": integrations,
        "custom_connectors": custom,
    })))
}

async fn upsert_custom_connector_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let input: crate::connectors::CustomConnectorInput = serde_json::from_value(body)
        .map_err(|e| ApiErr::msg(StatusCode::BAD_REQUEST, e.to_string()))?;
    let record = state
        .supervisor
        .connectors()
        .upsert_connector(input)
        .map_err(map_gm_error)?;
    Ok((StatusCode::CREATED, Json(record)))
}

async fn delete_custom_connector_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let deleted = state
        .supervisor
        .connectors()
        .delete_connector(&id)
        .map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({ "id": id, "deleted": deleted })))
}

async fn test_custom_connector_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let probe = crate::probe_custom_connector(&state.supervisor, &id)
        .await
        .map_err(map_gm_error)?;
    Ok(Json(probe))
}

#[derive(Debug, Deserialize)]
struct PreviewComputerQuery {
    max_long_edge: Option<u32>,
    quality: Option<u8>,
}

async fn capture_computer_preview_route(
    headers: HeaderMap,
    Query(q): Query<PreviewComputerQuery>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let max_edge = q.max_long_edge.unwrap_or(1280).clamp(320, 1920);
    let quality = q.quality.unwrap_or(75).clamp(20, 90);
    let Some(ref plat) = state.platform else {
        return Err(ApiErr::msg(
            StatusCode::SERVICE_UNAVAILABLE,
            "Computer-use platform helper is not attached",
        ));
    };
    let payload = crate::capture_preview_payload(
        plat.as_ref(),
        Some(max_edge),
        Some(quality),
        Some(&state.supervisor),
    )
    .await
    .map_err(map_gm_error)?;
    Ok(Json(payload))
}

async fn start_teach_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let bot_id = body
        .get("bot_id")
        .and_then(|v| v.as_str())
        .unwrap_or("muse");
    let snap = state
        .interactions
        .start_teach_task(bot_id)
        .map_err(map_gm_error)?;
    Ok(Json(snap))
}

async fn record_teach_step_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let step = if let Some(s) = body.get("step").and_then(|v| v.as_str()) {
        s.to_string()
    } else {
        let action = body
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("click");
        let target = body.get("target").and_then(|v| v.as_str()).unwrap_or("");
        let detail = body.get("detail").and_then(|v| v.as_str()).unwrap_or("");
        format!("{action} {target} {detail}").trim().to_string()
    };
    state
        .interactions
        .record_teach_step(&step)
        .map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn stop_teach_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let save_skill_name = body
        .get("save_skill_name")
        .or_else(|| body.get("title"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty());
    let outcome = state
        .interactions
        .stop_teach_task(&state.supervisor.shared_paths(), save_skill_name)
        .map_err(map_gm_error)?;
    Ok(Json(outcome))
}

async fn delete_activity_post_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let deleted = state
        .supervisor
        .feed_store()
        .delete_post(&id)
        .map_err(map_gm_error)?;
    let _ = state
        .supervisor
        .daemon_event_sender()
        .send(DaemonEvent::ActivityUpdated {
            reason: "feed_post_deleted".into(),
        });
    Ok(Json(serde_json::json!({ "id": id, "deleted": deleted })))
}

async fn unpublish_space_route(
    headers: HeaderMap,
    Path(slug): Path<String>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let space = state
        .supervisor
        .spaces()
        .unpublish_space(&slug)
        .map_err(map_gm_error)?;
    let _ = state
        .supervisor
        .daemon_event_sender()
        .send(DaemonEvent::ActivityUpdated {
            reason: "space_unpublished".into(),
        });
    Ok(Json(space))
}

#[derive(Debug, Deserialize)]
struct SpaceFileQuery {
    path: Option<String>,
}

async fn get_space_file_route(
    headers: HeaderMap,
    Path(slug): Path<String>,
    Query(q): Query<SpaceFileQuery>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let rel = q.path.as_deref().unwrap_or("index.html");
    let file_path = state
        .supervisor
        .spaces()
        .resolve_asset_file(&slug, rel)
        .map_err(map_gm_error)?;
    let pair = SharedFileStore::global()
        .read_text_with_sha256(&file_path)
        .map_err(map_gm_error)?;
    let (content, sha256) = pair.unwrap_or_default();
    Ok(Json(serde_json::json!({
        "slug": slug,
        "relative_path": rel,
        "content": content,
        "sha256": sha256,
    })))
}

async fn put_space_file_route(
    headers: HeaderMap,
    Path(slug): Path<String>,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let rel = body
        .get("relative_path")
        .or_else(|| body.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or("index.html");
    let content = body.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let path = state
        .supervisor
        .spaces()
        .update_space_file(&slug, rel, content)
        .map_err(map_gm_error)?;
    let _ = state
        .supervisor
        .daemon_event_sender()
        .send(DaemonEvent::ActivityUpdated {
            reason: "space_edited".into(),
        });
    Ok(Json(serde_json::json!({
        "slug": slug,
        "relative_path": rel,
        "path": path,
    })))
}

async fn delete_space_route(
    headers: HeaderMap,
    Path(slug): Path<String>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let deleted = state
        .supervisor
        .spaces()
        .delete_space(&slug, true)
        .map_err(map_gm_error)?;
    let _ = state
        .supervisor
        .daemon_event_sender()
        .send(DaemonEvent::ActivityUpdated {
            reason: "space_deleted".into(),
        });
    Ok(Json(
        serde_json::json!({ "slug": slug, "deleted": deleted }),
    ))
}

async fn delete_custom_skill_route(
    headers: HeaderMap,
    Path(name): Path<String>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let deleted = state
        .supervisor
        .delete_custom_skill(&name)
        .map_err(map_gm_error)?;
    Ok(Json(
        serde_json::json!({ "name": name, "deleted": deleted }),
    ))
}

async fn amend_skill_route(
    headers: HeaderMap,
    Path(name): Path<String>,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let note = body.get("note").and_then(|v| v.as_str()).unwrap_or("");
    let reg = state.supervisor.skill_registry().map_err(map_gm_error)?;
    let path = reg
        .append_amendment(gm_muse::skills::AmendmentTarget::Skill, &name, note)
        .map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({ "name": name, "path": path })))
}

async fn export_template_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("DT Team");
    let desc = body
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("Exported team template");
    let tpl = crate::export_team(
        state.supervisor.roster(),
        state.supervisor.channels(),
        state.supervisor.queue(),
        name,
        desc,
    )
    .map_err(map_gm_error)?;
    Ok(Json(tpl))
}

async fn preview_template_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let raw = if let Some(s) = body
        .get("bundle_json")
        .or_else(|| body.get("template"))
        .and_then(|v| v.as_str())
    {
        s.to_string()
    } else {
        serde_json::to_string(body.get("template").unwrap_or(&body)).unwrap_or_default()
    };
    let preview = crate::preview_template(&raw).map_err(map_gm_error)?;
    Ok(Json(preview))
}

async fn import_template_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let tpl: crate::TeamTemplate = if let Some(s) = body
        .get("bundle_json")
        .or_else(|| body.get("template"))
        .and_then(|v| v.as_str())
    {
        serde_json::from_str(s).map_err(|e| ApiErr::msg(StatusCode::BAD_REQUEST, e.to_string()))?
    } else {
        serde_json::from_value(
            body.get("template")
                .cloned()
                .unwrap_or_else(|| body.clone()),
        )
        .map_err(|e| ApiErr::msg(StatusCode::BAD_REQUEST, e.to_string()))?
    };
    let overwrite = body
        .get("overwrite")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let summary = crate::import_team(&state.supervisor, &tpl, overwrite).map_err(map_gm_error)?;
    Ok((StatusCode::CREATED, Json(summary)))
}

async fn system_health_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let helper_connected = check_platform_connected(&state.platform).await;
    let cfg = state.shared_config.read().clone();
    let checks = serde_json::json!([
        {
            "name": "storage_dir",
            "ok": cfg.storage_dir.exists(),
            "detail": cfg.storage_dir.display().to_string(),
        },
        {
            "name": "bots_roster",
            "ok": !state.supervisor.roster().list_bots().is_empty(),
            "detail": format!("{} bots configured", state.supervisor.roster().list_bots().len()),
        },
        {
            "name": "cu_helper",
            "ok": helper_connected,
            "detail": if helper_connected { "connected" } else { "offline" },
        }
    ]);
    Ok(Json(serde_json::json!({
        "ok": true,
        "helper_connected": helper_connected,
        "checks": checks,
    })))
}

async fn system_pause_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let current = state
        .supervisor
        .runtime_paused
        .load(std::sync::atomic::Ordering::SeqCst);
    let paused = body
        .get("paused")
        .and_then(|v| v.as_bool())
        .unwrap_or(!current);
    state
        .supervisor
        .set_runtime_paused(paused)
        .map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({ "paused": paused })))
}

async fn system_kill_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    if let Some(ref ks) = state.kill_switch {
        ks.trigger();
    }
    let _ = state.supervisor.set_runtime_paused(true);
    let mut cancelled = 0usize;
    if let Ok(tasks) = state.supervisor.queue().list_tasks(None) {
        for t in tasks {
            if matches!(
                t.status,
                crate::queue::QueuedTaskStatus::Queued
                    | crate::queue::QueuedTaskStatus::Running
                    | crate::queue::QueuedTaskStatus::Paused
                    | crate::queue::QueuedTaskStatus::WaitingForHuman
            ) && state
                .supervisor
                .control_task(&t.id, &QueueControlAction::Cancel)
                .unwrap_or(false)
            {
                cancelled += 1;
            }
        }
    }
    Ok(Json(serde_json::json!({
        "emergency_stopped": true,
        "cancelled_tasks": cancelled,
    })))
}

async fn system_updates_check_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let Some(updates) = state.updates else {
        return Err(ApiErr::msg(
            StatusCode::SERVICE_UNAVAILABLE,
            "Update service is unavailable in this standalone server",
        ));
    };
    Ok(Json(updates.check().await.map_err(map_gm_error)?))
}

async fn system_updates_download_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    if body["confirmed"].as_bool() != Some(true) {
        return Err(ApiErr::msg(
            StatusCode::FORBIDDEN,
            "Confirm downloading a verified update",
        ));
    }
    let Some(updates) = state.updates else {
        return Err(ApiErr::msg(
            StatusCode::SERVICE_UNAVAILABLE,
            "Update service unavailable",
        ));
    };
    Ok(Json(
        updates.download_verified().await.map_err(map_gm_error)?,
    ))
}

async fn system_components_list_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let Some(components) = state.components else {
        return Err(ApiErr::msg(
            StatusCode::SERVICE_UNAVAILABLE,
            "Component catalog manager is unavailable in this standalone server",
        ));
    };
    Ok(Json(components.list().await.map_err(map_gm_error)?))
}

async fn system_components_install_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let Some(components) = state.components else {
        return Err(ApiErr::msg(
            StatusCode::SERVICE_UNAVAILABLE,
            "Component catalog manager is unavailable in this standalone server",
        ));
    };
    Ok(Json(components.install(&id).await.map_err(map_gm_error)?))
}

async fn system_components_verify_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let Some(components) = state.components else {
        return Err(ApiErr::msg(
            StatusCode::SERVICE_UNAVAILABLE,
            "Component catalog manager is unavailable in this standalone server",
        ));
    };
    Ok(Json(components.verify(&id).await.map_err(map_gm_error)?))
}

async fn voice_status_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    Ok(Json(state.voice_manager.status()))
}

async fn voice_start_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let bot_id = body
        .get("bot_id")
        .and_then(|v| v.as_str())
        .unwrap_or("muse");
    let mode = body.get("mode").and_then(|v| v.as_str()).unwrap_or("voice");
    Ok(Json(state.voice_manager.start(bot_id, mode)))
}

async fn voice_consent_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let granted = body
        .get("granted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let bot_id = body.get("bot_id").and_then(|v| v.as_str());
    Ok(Json(state.voice_manager.set_consent(granted, bot_id)))
}

async fn voice_transcribe_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let b64 = body
        .get("wav_base64")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    use base64::Engine as _;
    let wav = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| ApiErr::msg(StatusCode::BAD_REQUEST, format!("Invalid wav_base64: {e}")))?;
    let res = state
        .voice_manager
        .transcribe_wav(&wav)
        .await
        .map_err(map_gm_error)?;
    Ok(Json(res))
}

async fn voice_speak_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let res = state
        .voice_manager
        .speak(text)
        .await
        .map_err(map_gm_error)?;
    Ok(Json(res))
}

async fn voice_barge_in_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    Ok(Json(state.voice_manager.barge_in()))
}

async fn voice_stop_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    Ok(Json(state.voice_manager.stop()))
}

async fn system_provider_key_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let provider = body
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("xai")
        .trim()
        .to_ascii_lowercase();
    let api_key = body
        .get("api_key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let key_name = match provider.as_str() {
        "meta" => gm_llm::META_KEY_ACCOUNT.to_string(),
        "xai" => gm_llm::XAI_KEY_ACCOUNT.to_string(),
        _ => format!("gm_provider_key_{provider}"),
    };
    if api_key.is_empty() {
        let _ = state.supervisor.vault().backend().delete_secret(&key_name);
    } else {
        state.supervisor.scrubber().register(&key_name, api_key);
        state
            .supervisor
            .vault()
            .backend()
            .set_secret(&key_name, api_key)
            .map_err(map_gm_error)?;
    }
    Ok(Json(serde_json::json!({
        "provider": provider,
        "configured": !api_key.is_empty(),
    })))
}

async fn system_providers_get_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    Ok(Json(state.supervisor.providers_config()))
}

async fn system_provider_profile_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    if body.get("profile").is_none()
        && (body.get("workspace_default_profile_id").is_some()
            || body.get("account_default_profile_id").is_some())
    {
        let ws_def = body
            .get("workspace_default_profile_id")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        let acc_def = body
            .get("account_default_profile_id")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        state
            .supervisor
            .set_default_provider_profiles(ws_def, acc_def)
            .map_err(map_gm_error)?;
        let cfg = crate::persist_providers_config(
            &state.shared_config,
            state.config_path.as_deref(),
            &state.supervisor,
        )
        .map_err(map_gm_error)?;
        return Ok(Json(serde_json::json!({ "providers": cfg })));
    }

    let profile_val = body.get("profile").cloned().unwrap_or_else(|| body.clone());
    let profile: gm_core::config::ProviderProfileConfig = serde_json::from_value(profile_val)
        .map_err(|e| ApiErr::msg(StatusCode::BAD_REQUEST, format!("Invalid profile: {e}")))?;
    let api_key = body
        .get("api_key")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    if let Some(ref k) = api_key {
        state
            .supervisor
            .scrubber()
            .register(format!("profile_{}", profile.id), k);
    }
    let revalidate = body
        .get("revalidate_credentials")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let saved = state
        .supervisor
        .upsert_provider_profile(profile, api_key.as_deref(), revalidate)
        .map_err(map_gm_error)?;
    let ws_def = body
        .get("workspace_default_profile_id")
        .and_then(|v| v.as_str())
        .map(ToString::to_string);
    let acc_def = body
        .get("account_default_profile_id")
        .and_then(|v| v.as_str())
        .map(ToString::to_string);
    if ws_def.is_some() || acc_def.is_some() {
        state
            .supervisor
            .set_default_provider_profiles(ws_def, acc_def)
            .map_err(map_gm_error)?;
    }
    let cfg = crate::persist_providers_config(
        &state.shared_config,
        state.config_path.as_deref(),
        &state.supervisor,
    )
    .map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({
        "profile": saved,
        "providers": cfg,
    })))
}

async fn system_provider_discover_models_route(
    headers: HeaderMap,
    Path(profile_id): Path<String>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let models = state
        .supervisor
        .discover_provider_models(&profile_id)
        .await
        .map_err(map_gm_error)?;
    let cfg = crate::persist_providers_config(
        &state.shared_config,
        state.config_path.as_deref(),
        &state.supervisor,
    )
    .map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({
        "profile_id": profile_id,
        "models": models,
        "providers": cfg,
    })))
}

async fn system_provider_oauth_route(
    headers: HeaderMap,
    Path((profile_id, op)): Path<(String, String)>,
    State(state): State<CompanionWebState>,
    body: Option<Json<serde_json::Value>>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let payload = body.map(|b| b.0).unwrap_or_else(|| serde_json::json!({}));
    state
        .supervisor
        .bind_provider_persistence(&state.shared_config, state.config_path.as_deref());
    let out = state
        .supervisor
        .oauth_action_for_profile(&profile_id, &op, &payload)
        .await
        .map_err(map_gm_error)?;
    crate::persist_providers_config(
        &state.shared_config,
        state.config_path.as_deref(),
        &state.supervisor,
    )
    .map_err(map_gm_error)?;
    Ok(Json(out))
}

fn api_v1_router(state: CompanionWebState) -> Router<CompanionWebState> {
    Router::new()
        .route("/health/live", get(health_live_v1_route))
        .route("/health/ready", get(health_ready_v1_route))
        .route("/auth/enroll", post(auth_enroll_v1_route))
        .route(
            "/auth/session",
            get(get_auth_session_v1_route).post(auth_session_v1_route),
        )
        .route("/auth/logout", post(auth_logout_v1_route))
        .route("/auth/revoke", post(auth_revoke_v1_route))
        .route(
            "/workspaces",
            get(list_workspaces_v1_route).post(create_workspace_v1_route),
        )
        .route("/doctor", get(doctor_status_v1_route))
        .route("/bootstrap", get(get_bootstrap_route))
        .route("/activity", get(list_activity).post(create_activity_post))
        .route("/queue", get(list_queue))
        .route("/tasks/:id/:action", post(control_task_route))
        .route("/workspaces/:ws_id/events", get(stream_workspace_events_v1))
        .route("/bots", get(list_bots).post(upsert_bot_route))
        .route("/bots/:id/files", get(list_bot_files_route))
        .route("/bots/:id/files/preview", get(preview_bot_file_route))
        .route(
            "/dm/:bot_id/messages",
            get(list_dm_messages_route).post(post_dm_message_route),
        )
        .route("/channels", get(list_channels).post(create_channel))
        .route(
            "/channels/:id/messages",
            get(list_channel_messages).post(post_channel_message),
        )
        .route("/approvals/:id", post(respond_approval_route))
        .route("/approvals/:id/respond", post(respond_approval_route))
        .route("/questions/:id", post(respond_question_route))
        .route("/questions/:id/respond", post(respond_question_route))
        .route(
            "/routines",
            get(list_routines_route).post(upsert_routine_route),
        )
        .route("/routines/:id", axum::routing::delete(delete_routine_route))
        .route("/routines/:id/test", post(test_routine_route))
        .route("/routines/:id/trigger", post(trigger_routine_webhook_route))
        .route("/routines/:id/runs", get(list_routine_runs_route))
        .route(
            "/goals",
            get(list_goals_route)
                .post(create_goal_route)
                .patch(patch_goal_route),
        )
        .route("/spaces", get(list_spaces_route).post(create_space_route))
        .route("/spaces/:slug/url", get(get_space_url_route))
        .route("/usage", get(get_usage_route))
        .route("/system/providers", get(system_providers_get_route))
        .route(
            "/system/providers/profile",
            post(system_provider_profile_route),
        )
        .route("/system/providers/key", post(system_provider_key_route))
        .route(
            "/system/providers/:id/discover-models",
            post(system_provider_discover_models_route),
        )
        .route(
            "/system/providers/:id/oauth/:op",
            post(system_provider_oauth_route),
        )
        .route("/connectors", get(list_connectors_route))
        .route("/connectors/probe/:id", get(probe_connector_v1_route))
        .route("/marketplace", get(list_marketplace_v1_route))
        .route("/marketplace/install", post(install_marketplace_v1_route))
        .route("/marketplace/:id/update", post(update_marketplace_v1_route))
        .route(
            "/marketplace/:id",
            axum::routing::delete(remove_marketplace_v1_route),
        )
        .route("/billing/status", get(billing_status_v1_route))
        .route("/billing/checkout", post(billing_checkout_v1_route))
        .route("/billing/webhooks/stripe", post(stripe_webhook_v1_route))
        .layer(axum::middleware::from_fn_with_state(
            state,
            api_v1_auth_and_csrf_middleware,
        ))
}

fn require_operator_bearer(
    headers: &HeaderMap,
    state: &CompanionWebState,
) -> std::result::Result<(), ApiErr> {
    let expected = state
        .bearer_token
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| ApiErr::msg(StatusCode::UNAUTHORIZED, "Operator token required"))?;
    let auth_val = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !crate::bridges::constant_time_eq(
        auth_val.as_bytes(),
        format!("Bearer {expected}").as_bytes(),
    ) {
        return Err(ApiErr::msg(
            StatusCode::UNAUTHORIZED,
            "Operator token required",
        ));
    }
    Ok(())
}

async fn create_workspace_v1_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    require_operator_bearer(&headers, &state)?;
    let plane = state
        .hosted_plane
        .as_ref()
        .ok_or_else(|| ApiErr::msg(StatusCode::NOT_IMPLEMENTED, "Hosted plane not enabled"))?;
    let ws_id = body["ws_id"]
        .as_str()
        .or_else(|| body["id"].as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiErr::msg(StatusCode::BAD_REQUEST, "Missing ws_id"))?;
    let tenant_id = body["tenant_id"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(ws_id);
    let name = body["name"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(ws_id);
    let owner_user_id = body["owner_user_id"]
        .as_str()
        .or_else(|| body["user_id"].as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("operator");
    let owner_email = body["owner_email"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("operator@dt.local");
    let bearer_token = body["bearer_token"].as_str().unwrap_or("");
    let rec = plane
        .provision_workspace(
            ws_id,
            tenant_id,
            owner_email,
            name,
            owner_user_id,
            bearer_token,
        )
        .await
        .map_err(map_gm_error)?;
    // Warm up workspace supervisor directory cleanly.
    let _ = state.supervisor_for_workspace(&rec.ws_id).await;
    Ok((StatusCode::CREATED, Json(rec)))
}

async fn auth_enroll_v1_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    // Only operator bearer token can mint enrollments.
    require_operator_bearer(&headers, &state)?;
    let plane = state
        .hosted_plane
        .as_ref()
        .ok_or_else(|| ApiErr::msg(StatusCode::NOT_IMPLEMENTED, "Hosted plane not enabled"))?;
    let ws_id = body["ws_id"]
        .as_str()
        .ok_or_else(|| ApiErr::msg(StatusCode::BAD_REQUEST, "Missing ws_id"))?;
    let user_id = body["user_id"]
        .as_str()
        .ok_or_else(|| ApiErr::msg(StatusCode::BAD_REQUEST, "Missing user_id"))?;
    let role = body["role"].as_str().unwrap_or("member");
    let ttl = body["ttl_seconds"].as_i64().unwrap_or(900);
    let issued = plane
        .issue_enrollment(ws_id, user_id, role, ttl)
        .await
        .map_err(map_gm_error)?;
    Ok(Json(issued))
}

async fn get_auth_session_v1_route(
    request: axum::extract::Request,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    let session = request
        .extensions()
        .get::<crate::hosted::HostedSessionIssued>()
        .ok_or_else(|| ApiErr::msg(StatusCode::UNAUTHORIZED, "Sign in to DT"))?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "authenticated": true,
            "user_id": session.user_id,
            "ws_id": session.ws_id,
            "tenant_id": session.tenant_id,
            "role": session.role,
            "csrf_token": session.csrf_token,
            "expires_at": session.expires_at,
        })),
    ))
}

async fn auth_session_v1_route(
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    let plane = state
        .hosted_plane
        .as_ref()
        .ok_or_else(|| ApiErr::msg(StatusCode::NOT_IMPLEMENTED, "Hosted plane not enabled"))?;
    let enrollment_token = body["enrollment_token"]
        .as_str()
        .ok_or_else(|| ApiErr::msg(StatusCode::BAD_REQUEST, "Missing enrollment_token"))?;
    let ttl = body["ttl_seconds"]
        .as_i64()
        .unwrap_or(3600)
        .clamp(300, 86400);
    let session = plane
        .redeem_enrollment(enrollment_token, ttl)
        .await
        .map_err(|e| ApiErr::msg(StatusCode::UNAUTHORIZED, e.to_string()))?;

    let cookie = format!(
        "{HOSTED_SESSION_COOKIE}={}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={ttl}",
        session.session_id
    );
    let mut resp = Json(serde_json::json!({
        "authenticated": true,
        "user_id": session.user_id,
        "ws_id": session.ws_id,
        "tenant_id": session.tenant_id,
        "role": session.role,
        "csrf_token": session.csrf_token,
        "expires_at": session.expires_at,
    }))
    .into_response();
    if let Ok(hv) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().insert(header::SET_COOKIE, hv);
    }
    Ok(resp)
}

async fn auth_logout_v1_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    if let (Some(session_id), Some(plane)) = (
        extract_cookie_value(&headers, HOSTED_SESSION_COOKIE),
        state.hosted_plane.as_ref(),
    ) {
        let _ = plane.revoke_session(&session_id).await;
    }
    let mut resp = Json(serde_json::json!({ "signed_out": true })).into_response();
    if let Ok(hv) = HeaderValue::from_str(&format!(
        "{HOSTED_SESSION_COOKIE}=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0"
    )) {
        resp.headers_mut().insert(header::SET_COOKIE, hv);
    }
    Ok(resp)
}

async fn auth_revoke_v1_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    require_operator_bearer(&headers, &state)?;
    let plane = state
        .hosted_plane
        .as_ref()
        .ok_or_else(|| ApiErr::msg(StatusCode::NOT_IMPLEMENTED, "Hosted plane not enabled"))?;
    let ws_id = body["ws_id"]
        .as_str()
        .ok_or_else(|| ApiErr::msg(StatusCode::BAD_REQUEST, "Missing ws_id"))?;
    let user_id = body["user_id"]
        .as_str()
        .ok_or_else(|| ApiErr::msg(StatusCode::BAD_REQUEST, "Missing user_id"))?;
    plane
        .revoke_membership(ws_id, user_id)
        .await
        .map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({
        "revoked": true,
        "ws_id": ws_id,
        "user_id": user_id,
    })))
}

async fn list_workspaces_v1_route(
    State(state): State<CompanionWebState>,
    request: axum::extract::Request,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    let sess = request
        .extensions()
        .get::<crate::hosted::HostedSessionIssued>()
        .cloned();
    if let Some(sess) = sess {
        let workspaces = if let Some(ref plane) = state.hosted_plane {
            plane
                .list_user_workspaces(&sess.user_id)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        return Ok(Json(serde_json::json!({
            "active_workspace": sess.ws_id,
            "user_id": sess.user_id,
            "role": sess.role,
            "workspaces": workspaces,
        })));
    }
    let requested = request
        .headers()
        .get("x-dt-workspace-id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    let workspaces = if let Some(ref plane) = state.hosted_plane {
        plane.list_workspaces().await.unwrap_or_default()
    } else {
        Vec::new()
    };
    let ws = requested
        .or_else(|| std::env::var("GM_WORKSPACE_ID").ok())
        .unwrap_or_else(|| "default".into());
    Ok(Json(serde_json::json!({
        "active_workspace": ws,
        "role": "owner",
        "workspaces": workspaces,
    })))
}

async fn doctor_status_v1_route(
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    if let Some(ref doc) = state.sanitized_doctor
        && let Some(val) = doc.read().clone()
    {
        return Ok(Json(val));
    }
    Ok(Json(serde_json::json!({
        "schema": 1,
        "installation_readiness": "unavailable",
        "sanitized": true,
        "note": "Run `dt doctor check --json` locally for full diagnostics."
    })))
}

async fn health_live_v1_route() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "live",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn health_ready_v1_route(State(state): State<CompanionWebState>) -> impl IntoResponse {
    let storage_dir = state.shared_config.read().storage_dir.clone();
    if let Err(err) = std::fs::create_dir_all(&storage_dir) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "status": "not_ready",
                "reason": format!("Worker storage unavailable: {err}"),
            })),
        );
    }
    let probe = storage_dir.join(format!(".ready-probe-{}", uuid::Uuid::new_v4()));
    if let Err(err) = std::fs::write(&probe, b"ok") {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "status": "not_ready",
                "reason": format!("Worker storage not writable: {err}"),
            })),
        );
    }
    let _ = std::fs::remove_file(&probe);

    if let Err(err) = state.supervisor.queue().min_event_sequence() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "status": "not_ready",
                "reason": format!("Event store unavailable: {err}"),
            })),
        );
    }

    let mut hosted_backend = "local";
    if let Some(ref plane) = state.hosted_plane {
        hosted_backend = plane.backend_kind();
        if let Err(err) = plane.ready_check().await {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "status": "not_ready",
                    "hosted_backend": hosted_backend,
                    "reason": err.to_string(),
                })),
            );
        }
    }

    if std::env::var("DT_HOSTED_REQUIRED").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        && state.hosted_plane.is_none()
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "status": "not_ready",
                "reason": "Hosted control plane is required but not configured",
            })),
        );
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "ready",
            "hosted_backend": hosted_backend,
            "version": env!("CARGO_PKG_VERSION"),
        })),
    )
}

async fn stream_workspace_events_v1(
    headers: HeaderMap,
    Query(cursor): Query<EventCursor>,
    Path(ws_id): Path<String>,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
    session_ext: Option<axum::Extension<crate::hosted::HostedSessionIssued>>,
) -> std::result::Result<
    Sse<impl tokio_stream::Stream<Item = std::result::Result<Event, Infallible>>>,
    ApiErr,
> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let clean = crate::hosted::validate_workspace_slug(&ws_id).map_err(map_gm_error)?;
    let session_user_id = if let Some(axum::Extension(sess)) = session_ext {
        if sess.ws_id != clean {
            return Err(ApiErr::msg(
                StatusCode::FORBIDDEN,
                format!("Cross-tenant SSE subscription denied for workspace '{clean}'"),
            ));
        }
        Some(sess.user_id)
    } else {
        let expected_ws = std::env::var("GM_WORKSPACE_ID").unwrap_or_else(|_| "default".into());
        let workspace_known = match state.hosted_plane.as_ref() {
            Some(plane) => plane.get_workspace(&clean).await.ok().flatten().is_some(),
            None => false,
        };
        if clean != expected_ws && clean != "local" && !workspace_known {
            return Err(ApiErr::msg(
                StatusCode::FORBIDDEN,
                format!("Cross-tenant SSE subscription denied for workspace '{clean}'"),
            ));
        }
        None
    };

    let last_event_id = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(cursor.after);

    // Subscribe to live broadcasts and revocations BEFORE reading persisted cursor
    // so zero events committed during replay->live transition can fall into a gap.
    let same_sup = sup.config().storage_dir == state.supervisor.config().storage_dir;
    let mut live_rx = sup.agui_sender().subscribe();
    let mut root_live_rx = (!same_sup).then(|| state.supervisor.agui_sender().subscribe());
    let mut revocation_rx = state
        .hosted_plane
        .as_ref()
        .map(|p| p.subscribe_revocations());
    let ws_min_seq = sup.queue().min_event_sequence().map_err(map_gm_error)?;
    let root_min_seq = if same_sup {
        None
    } else {
        state
            .supervisor
            .queue()
            .min_event_sequence()
            .map_err(map_gm_error)?
    };
    let min_seq = match (ws_min_seq, root_min_seq) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let mut backlog = sup
        .queue()
        .events_after(last_event_id, 500)
        .map_err(map_gm_error)?;
    if !same_sup {
        let root_backlog = state
            .supervisor
            .queue()
            .events_after(last_event_id, 500)
            .map_err(map_gm_error)?;
        for (seq, val) in root_backlog {
            if val
                .get("payload")
                .and_then(|p| p.get("ws_id"))
                .and_then(|w| w.as_str())
                == Some(clean.as_str())
            {
                backlog.push((seq, val));
            }
        }
        backlog.sort_by_key(|(seq, _)| *seq);
    }
    let plane_for_task = state.hosted_plane.clone();

    let (tx, rx) = tokio::sync::mpsc::channel::<std::result::Result<Event, Infallible>>(128);
    tokio::spawn(async move {
        let ws_matches = |val: &serde_json::Value| -> bool {
            match val
                .get("payload")
                .and_then(|p| p.get("ws_id"))
                .and_then(|w| w.as_str())
            {
                Some(tag) => tag == clean,
                None => true,
            }
        };

        if last_event_id > 0
            && let Some(min_available) = min_seq
            && last_event_id.saturating_add(1) < min_available
        {
            let resync = Event::default()
                .event("RESYNC_REQUIRED")
                .data(serde_json::json!({ "min_sequence": min_available, "requested_after": last_event_id }).to_string());
            if tx.send(Ok(resync)).await.is_err() {
                return;
            }
        }

        let mut highest_replayed = last_event_id;
        for (seq, mut val) in backlog {
            if seq > highest_replayed {
                highest_replayed = seq;
            }
            if !ws_matches(&val) {
                continue;
            }
            if let Some(obj) = val.as_object_mut() {
                obj.insert("sequence".into(), serde_json::json!(seq));
            }
            let event_name = val
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("MESSAGE")
                .to_string();
            let evt = Event::default()
                .id(seq.to_string())
                .event(event_name)
                .data(serde_json::to_string(&val).unwrap_or_else(|_| "{}".into()));
            if tx.send(Ok(evt)).await.is_err() {
                return;
            }
        }

        loop {
            if let (Some(plane), Some(uid)) = (&plane_for_task, &session_user_id)
                && plane.authorize_membership(&clean, uid).await.is_err()
            {
                break;
            }
            tokio::select! {
                rev = async {
                    match revocation_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Ok((rev_ws, rev_uid)) = rev
                        && rev_ws == clean
                        && session_user_id.as_deref() == Some(rev_uid.as_str())
                    {
                        break;
                    }
                }
                root_item = async {
                    match root_live_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Ok(evt) = root_item
                        && evt.payload.get("ws_id").and_then(|w| w.as_str()) == Some(clean.as_str())
                    {
                        if evt.sequence > 0 && evt.sequence <= highest_replayed {
                            continue;
                        }
                        if evt.sequence > highest_replayed {
                            highest_replayed = evt.sequence;
                        }
                        let json = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".into());
                        let sse_evt = Event::default()
                            .id(evt.sequence.to_string())
                            .event(evt.event_type)
                            .data(json);
                        if tx.send(Ok(sse_evt)).await.is_err() {
                            break;
                        }
                    }
                }
                item = live_rx.recv() => {
                    match item {
                        Ok(evt) => {
                            if evt.sequence > 0 && evt.sequence <= highest_replayed {
                                continue;
                            }
                            if evt.sequence > highest_replayed {
                                highest_replayed = evt.sequence;
                            }
                            if let Some(tag) = evt.payload.get("ws_id").and_then(|w| w.as_str())
                                && tag != clean
                            {
                                continue;
                            }
                            let json = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".into());
                            let sse_evt = Event::default()
                                .id(evt.sequence.to_string())
                                .event(evt.event_type)
                                .data(json);
                            if tx.send(Ok(sse_evt)).await.is_err() {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let sse_evt = Event::default()
                                .event("RESYNC_REQUIRED")
                                .data("{}");
                            if tx.send(Ok(sse_evt)).await.is_err() {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    Ok(Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new().interval(std::time::Duration::from_secs(15)),
    ))
}

async fn probe_connector_v1_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let cfg = state.shared_config.read().clone();
    let result = sup
        .connectors()
        .probe_integration_capability(&cfg, &id)
        .await;
    Ok(Json(result))
}

async fn list_marketplace_v1_route(
    headers: HeaderMap,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let catalog = sup.marketplace().builtin_catalog();
    let installed = sup.marketplace().list_installed().map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({
        "catalog": catalog,
        "installed": installed,
    })))
}

async fn install_marketplace_v1_route(
    headers: HeaderMap,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let confirmed_scopes: Vec<String> = body
        .get("confirmed_scopes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default();
    let listing: crate::marketplace::MarketplaceListing = if let Some(l) = body.get("listing") {
        serde_json::from_value(l.clone())
            .map_err(|e| ApiErr::msg(StatusCode::BAD_REQUEST, format!("Invalid listing: {e}")))?
    } else {
        let id = body["id"]
            .as_str()
            .ok_or_else(|| ApiErr::msg(StatusCode::BAD_REQUEST, "Missing plugin id or listing"))?;
        sup.marketplace()
            .builtin_catalog()
            .into_iter()
            .find(|c| c.id == id)
            .ok_or_else(|| ApiErr::msg(StatusCode::NOT_FOUND, format!("Unknown plugin '{id}'")))?
    };
    let installed = sup
        .marketplace()
        .install_plugin(&listing, &confirmed_scopes)
        .map_err(map_gm_error)?;
    Ok(Json(installed))
}

async fn update_marketplace_v1_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let confirmed_scopes: Vec<String> = body
        .get("confirmed_scopes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default();
    let mut listing: crate::marketplace::MarketplaceListing = if let Some(l) = body.get("listing") {
        serde_json::from_value(l.clone())
            .map_err(|e| ApiErr::msg(StatusCode::BAD_REQUEST, format!("Invalid listing: {e}")))?
    } else if body.get("name").is_some() {
        serde_json::from_value(body.clone())
            .map_err(|e| ApiErr::msg(StatusCode::BAD_REQUEST, format!("Invalid listing: {e}")))?
    } else {
        state
            .supervisor
            .marketplace()
            .builtin_catalog()
            .into_iter()
            .find(|c| c.id == id)
            .ok_or_else(|| ApiErr::msg(StatusCode::NOT_FOUND, format!("Unknown plugin '{id}'")))?
    };
    if let Some(ver) = body.get("version").and_then(|v| v.as_str()) {
        listing.version = ver.to_string();
    }
    listing.id = id;
    let updated = sup
        .marketplace()
        .update_plugin(&listing, &confirmed_scopes)
        .map_err(map_gm_error)?;
    Ok(Json(updated))
}

async fn remove_marketplace_v1_route(
    headers: HeaderMap,
    Path(id): Path<String>,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let removed = sup.marketplace().remove_plugin(&id).map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({ "id": id, "removed": removed })))
}

async fn billing_status_v1_route(
    headers: HeaderMap,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
    session_ext: Option<axum::Extension<crate::hosted::HostedSessionIssued>>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let ws_id = session_ext
        .map(|axum::Extension(s)| s.ws_id)
        .unwrap_or_else(|| std::env::var("GM_WORKSPACE_ID").unwrap_or_else(|_| "default".into()));
    let ent = sup
        .billing()
        .get_entitlements(&ws_id)
        .map_err(map_gm_error)?;
    Ok(Json(ent))
}

async fn billing_checkout_v1_route(
    headers: HeaderMap,
    ActiveSupervisor(sup): ActiveSupervisor,
    State(state): State<CompanionWebState>,
    session_ext: Option<axum::Extension<crate::hosted::HostedSessionIssued>>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    if body.get("stripe_secret_key").is_some()
        || body.get("success_url").is_some()
        || body.get("cancel_url").is_some()
    {
        return Err(ApiErr::msg(
            StatusCode::BAD_REQUEST,
            "Stripe secrets and redirect URLs are server-controlled and cannot be supplied by clients",
        ));
    }
    let stripe_key = std::env::var("STRIPE_SECRET_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .ok_or_else(|| {
            ApiErr::msg(
                StatusCode::SERVICE_UNAVAILABLE,
                "Server STRIPE_SECRET_KEY is not configured",
            )
        })?;
    let ws_id = session_ext
        .map(|axum::Extension(s)| s.ws_id)
        .unwrap_or_else(|| std::env::var("GM_WORKSPACE_ID").unwrap_or_else(|_| "default".into()));
    let allowed_prices = std::env::var("DT_STRIPE_ALLOWED_PRICES")
        .unwrap_or_else(|_| "price_default,price_pro".into());
    let price_id = body["price_id"].as_str().unwrap_or("price_default");
    if !allowed_prices.split(',').any(|p| p.trim() == price_id) {
        return Err(ApiErr::msg(
            StatusCode::BAD_REQUEST,
            format!("Requested Stripe price_id '{price_id}' is not in server allowlist"),
        ));
    }
    let success = std::env::var("DT_STRIPE_SUCCESS_URL")
        .unwrap_or_else(|_| "https://localhost/billing/success".into());
    let cancel = std::env::var("DT_STRIPE_CANCEL_URL")
        .unwrap_or_else(|_| "https://localhost/billing/cancel".into());
    let res = sup
        .billing()
        .create_sandbox_checkout_session(&stripe_key, &ws_id, &success, &cancel, price_id)
        .await
        .map_err(map_gm_error)?;
    Ok(Json(res))
}

async fn stripe_webhook_v1_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    raw_body: String,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    let active_secret = std::env::var("STRIPE_WEBHOOK_SECRET")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            ApiErr::msg(
                StatusCode::UNAUTHORIZED,
                "STRIPE_WEBHOOK_SECRET is not configured on server",
            )
        })?;
    let sig = headers
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let (applied, ent) = state
        .supervisor
        .billing()
        .process_stripe_webhook(&active_secret, sig, &raw_body)
        .map_err(|e| ApiErr::msg(StatusCode::UNAUTHORIZED, e.to_string()))?;
    Ok(Json(serde_json::json!({
        "applied": applied,
        "entitlements": ent,
    })))
}

async fn system_support_bundle_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    body: Option<Json<serde_json::Value>>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let note = body
        .as_ref()
        .and_then(|b| b.get("note"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let helper_connected = check_platform_connected(&state.platform).await;
    let cfg = state.shared_config.read().clone();
    let out_path = cfg.storage_dir.join(format!(
        "support-bundle-{}.json",
        chrono::Utc::now().timestamp()
    ));
    let bundle = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "created_at": chrono::Utc::now().to_rfc3339(),
        "note": note,
        "helper_connected": helper_connected,
        "bot_count": state.supervisor.roster().list_bots().len(),
    });
    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        &out_path,
        serde_json::to_string_pretty(&bundle).unwrap_or_default(),
    );
    Ok(Json(serde_json::json!({
        "output_path": out_path.display().to_string(),
        "bundle": bundle,
    })))
}

async fn system_feedback_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<serde_json::Value>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let category = body
        .get("category")
        .and_then(|v| v.as_str())
        .unwrap_or("general");
    let message = body.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let cfg = state.shared_config.read().clone();
    let dir = cfg.storage_dir.join("feedback");
    std::fs::create_dir_all(&dir)
        .map_err(|e| ApiErr::msg(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let id = format!("fb-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]);
    let path = dir.join(format!("{id}.json"));
    let entry = serde_json::json!({
        "id": id,
        "category": category,
        "message": message,
        "created_at": chrono::Utc::now().to_rfc3339(),
    });
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&entry).unwrap_or_default(),
    )
    .map_err(|e| ApiErr::msg(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({
        "id": id,
        "saved_path": path.display().to_string(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spaces::SpacesWebServer;
    use gm_core::config::GmConfig;
    use gm_safety::AuditLogger;
    use tokio::sync::broadcast;

    #[test]
    fn companion_assets_preserve_budget_and_security_contracts() {
        assert!(COMPANION_UI_CSS.len() + COMPANION_UI_JS.len() < 64 * 1024);
        assert!(!COMPANION_UI_JS.contains("setInterval"));
        assert!(!COMPANION_UI_JS.contains("innerHTML"));
        assert!(COMPANION_UI_HTML.contains("gm-drafts-v1"));
        assert!(COMPANION_UI_JS.contains("/api/bootstrap"));
        assert!(COMPANION_UI_JS.contains("/ag-ui/events"));
        assert!(COMPANION_UI_JS.contains("Authorization"));
        assert!(COMPANION_UI_HTML.contains("id=\"space-iframe\""));
        assert!(COMPANION_UI_HTML.contains("sandbox=\"allow-scripts\""));
    }

    #[tokio::test]
    async fn companion_assets_and_authenticated_sse_delivery() {
        let (comp_addr, _spaces_addr, sup, comp_handle, spaces_handle, _dir) =
            spawn_test_servers().await;
        let client = reqwest::Client::new();
        let base = format!("http://{comp_addr}");
        for (path, mime) in [
            ("/companion.css", "text/css"),
            ("/companion.js", "javascript"),
        ] {
            let response = client.get(format!("{base}{path}")).send().await.unwrap();
            assert_eq!(response.status(), 200);
            assert!(
                response.headers()["content-type"]
                    .to_str()
                    .unwrap()
                    .contains(mime)
            );
            assert!(
                response.headers()["content-security-policy"]
                    .to_str()
                    .unwrap()
                    .contains("frame-ancestors 'none'")
            );
        }
        for path in ["/api/bootstrap", "/ag-ui/events"] {
            assert_eq!(
                client
                    .get(format!("{base}{path}"))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                401
            );
        }
        let mut response = client
            .get(format!("{base}/ag-ui/events"))
            .bearer_auth("secret-token")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .contains("text/event-stream")
        );
        sup.emit_agui(
            "BOT_UPDATED",
            "muse",
            "muse",
            serde_json::json!({"id":"muse"}),
        );
        let mut received = String::new();
        tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(chunk) = response.chunk().await.unwrap() {
                received.push_str(&String::from_utf8_lossy(&chunk));
                if received.contains("BOT_UPDATED") {
                    break;
                }
            }
        })
        .await
        .expect("SSE must deliver the pushed update");
        assert!(received.contains("BOT_UPDATED"));
        comp_handle.abort();
        spaces_handle.abort();
    }

    async fn spawn_test_servers() -> (
        SocketAddr,
        SocketAddr,
        OpenBotSupervisor,
        tokio::task::JoinHandle<()>,
        tokio::task::JoinHandle<()>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = GmConfig {
            storage_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        cfg.openbot.bots_dir = dir.path().join("bots");
        let (tx, _) = broadcast::channel(64);
        let sup = OpenBotSupervisor::open_with_secret_backend(
            cfg.clone(),
            Arc::new(AuditLogger::in_memory()),
            tx,
            Arc::new(crate::vault::InMemorySecretBackend::default()),
        )
        .unwrap();
        let spaces_server = SpacesWebServer::new(sup.spaces().clone());
        let (spaces_addr, spaces_handle) =
            spaces_server.bind_and_serve("127.0.0.1:0").await.unwrap();
        sup.set_spaces_port(spaces_addr.port());

        let comp_server = CompanionWebServer::new(
            sup.clone(),
            BridgeRouter::new(sup.clone(), cfg.openbot.bridges),
            Some("secret-token".into()),
            None,
        );
        let (comp_addr, comp_handle) = comp_server.bind_and_serve("127.0.0.1:0").await.unwrap();
        (comp_addr, spaces_addr, sup, comp_handle, spaces_handle, dir)
    }

    #[tokio::test]
    async fn muse_web_api_auth_csp_cas_spaces_goals_forget_reactions_and_xss_literal() {
        let (comp_addr, spaces_addr, sup, comp_handle, spaces_handle, _dir) =
            spawn_test_servers().await;
        let client = reqwest::Client::new();
        let base = format!("http://{comp_addr}");

        // 1. HTML "/" includes frame-src for the dedicated Spaces port AND frame-ancestors 'none'
        let index_resp = client.get(format!("{base}/")).send().await.unwrap();
        assert_eq!(index_resp.status(), 200);
        let index_csp = index_resp.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .to_string();
        assert!(index_csp.contains("frame-ancestors 'none'"));
        assert!(index_csp.contains(&format!(
            "frame-src http://127.0.0.1:{}",
            spaces_addr.port()
        )));

        // 2. All /api/* endpoints reject missing auth (401) AND keep frame-ancestors 'none' WITHOUT frame-src
        for (method, path) in [
            ("GET", "/api/activity"),
            ("GET", "/api/ideas"),
            ("GET", "/api/goals"),
            ("GET", "/api/spaces"),
            ("GET", "/api/skills"),
            ("GET", "/api/connectors"),
            ("GET", "/api/identity/muse/USER.md"),
            ("GET", "/api/side-chats/slack-c1/discretion"),
        ] {
            let resp = client
                .request(method.parse().unwrap(), format!("{base}{path}"))
                .send()
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                401,
                "{method} {path} must require Bearer auth"
            );
            let csp = resp.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .to_string();
            assert!(csp.contains("frame-ancestors 'none'"));
            assert!(
                !csp.contains("frame-src"),
                "/api/* must not expose frame-src: {csp}"
            );
        }

        // 3. XSS literal round-trip + Reactions endpoint
        let xss_payload = "<img src=x onerror=alert(1)> hello";
        let posted: serde_json::Value = client
            .post(format!("{base}/api/channels/general/messages"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "content": xss_payload }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let msg_id = posted["message"]["id"].as_str().unwrap().to_string();
        assert_eq!(posted["message"]["content"], xss_payload);

        let rxn_resp = client
            .post(format!("{base}/api/messages/{msg_id}/reactions"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "emoji": "🚀" }))
            .send()
            .await
            .unwrap();
        assert_eq!(rxn_resp.status(), 200);
        let msgs: Vec<serde_json::Value> = client
            .get(format!("{base}/api/channels/general/messages"))
            .bearer_auth("secret-token")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let found = msgs.iter().find(|m| m["id"] == msg_id).unwrap();
        assert_eq!(found["content"], xss_payload);
        assert_eq!(found["reactions"][0]["emoji"], "🚀");

        // 4. Activity Feed + Brief Prompt + Rating + Ideas dismiss once (never opt-out)
        let activity: serde_json::Value = client
            .get(format!("{base}/api/activity"))
            .bearer_auth("secret-token")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let first_post_id = activity["posts"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(
            client
                .post(format!("{base}/api/activity/prompt"))
                .bearer_auth("secret-token")
                .json(&serde_json::json!({ "prompt": "Focus on Rust safety releases" }))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        assert_eq!(
            client
                .post(format!("{base}/api/activity/{first_post_id}/rate"))
                .bearer_auth("secret-token")
                .json(&serde_json::json!({ "rating": 5 }))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        let created_idea: serde_json::Value = client
            .post(format!("{base}/api/ideas"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "title": "Draft release notes", "action_prompt": "Draft release notes" }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let idea_id = created_idea["id"].as_str().unwrap().to_string();
        assert_eq!(
            client
                .post(format!("{base}/api/ideas/{idea_id}/dismiss"))
                .bearer_auth("secret-token")
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        let prefs_snap: serde_json::Value = client
            .get(format!("{base}/api/proactive-prefs"))
            .bearer_auth("secret-token")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(
            prefs_snap["never_tell_me_about"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        // 5. Goals + 1-level subgoal limit + deliverables
        let parent_goal: serde_json::Value = client
            .post(format!("{base}/api/goals"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "title": "Launch Companion", "description": "Ship UI" }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let parent_slug = parent_goal["slug"].as_str().unwrap().to_string();
        let child_goal: serde_json::Value = client
            .post(format!("{base}/api/goals"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "parent_slug": parent_slug, "title": "Add Feed Tab" }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let child_slug = child_goal["slug"].as_str().unwrap().to_string();
        let grandchild = client
            .post(format!("{base}/api/goals"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "parent_slug": child_slug, "title": "Too Deep" }))
            .send()
            .await
            .unwrap();
        assert_eq!(grandchild.status(), 400);

        sup.goal_manager()
            .write_deliverable(&parent_slug, "summary.md", "Goal deliverable ready")
            .unwrap();
        let deliv_resp = client
            .get(format!("{base}/api/goals/{parent_slug}/files/summary.md"))
            .bearer_auth("secret-token")
            .send()
            .await
            .unwrap();
        assert_eq!(deliv_resp.status(), 200);
        assert_eq!(deliv_resp.text().await.unwrap(), "Goal deliverable ready");

        // 6. Spaces two-port creation, ts rejection, publish confirmation (403), and fetch from Spaces port
        let ts_resp = client
            .post(format!("{base}/api/spaces"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "slug": "ts-app", "kind": "ts" }))
            .send()
            .await
            .unwrap();
        assert_eq!(ts_resp.status(), 400);

        let sp_resp = client
            .post(format!("{base}/api/spaces"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "slug": "demo-space", "title": "Demo Space", "kind": "static" }))
            .send()
            .await
            .unwrap();
        assert_eq!(sp_resp.status(), 201);

        let url_json: serde_json::Value = client
            .get(format!("{base}/api/spaces/demo-space/url"))
            .bearer_auth("secret-token")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let space_url = url_json["url"].as_str().unwrap().to_string();
        assert!(space_url.starts_with(&format!(
            "http://127.0.0.1:{}/spaces/v2/demo-space/",
            spaces_addr.port()
        )));
        let fetched_space = client.get(&space_url).send().await.unwrap();
        assert_eq!(fetched_space.status(), 200);
        assert!(
            fetched_space.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .contains(&format!(
                    "frame-ancestors http://127.0.0.1:{}",
                    comp_addr.port()
                ))
        );

        let pub_denied = client
            .post(format!("{base}/api/spaces/demo-space/publish"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "confirmed": false }))
            .send()
            .await
            .unwrap();
        assert_eq!(pub_denied.status(), 403);
        let pub_ok = client
            .post(format!("{base}/api/spaces/demo-space/publish"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "confirmed": true }))
            .send()
            .await
            .unwrap();
        assert_eq!(pub_ok.status(), 200);

        // 7. Identity CAS 409 conflict & Allowlist
        let id_get: serde_json::Value = client
            .get(format!("{base}/api/identity/muse/USER.md"))
            .bearer_auth("secret-token")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let valid_sha = id_get["sha256"].as_str().unwrap().to_string();
        let stale_put = client
            .put(format!("{base}/api/identity/muse/USER.md"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "content": "# USER.md\nUpdated\n", "expected_sha256": "deadbeef" }))
            .send()
            .await
            .unwrap();
        assert_eq!(stale_put.status(), 409);
        let ok_put = client
            .put(format!("{base}/api/identity/muse/USER.md"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "content": "# USER.md\n- Favorite color: Cobalt\n", "expected_sha256": valid_sha }))
            .send()
            .await
            .unwrap();
        assert_eq!(ok_put.status(), 200);

        // 8. Skills + Custom Skill secret rejection + Portable Memory Import + Two-Phase Forget
        let bad_skill = client
            .post(format!("{base}/api/skills/custom"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({
                "name": "bad-skill",
                "title": "Bad Skill",
                "description": "Leaks token",
                "body_markdown": "Use ghp_1234567890abcdef123456 directly"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(bad_skill.status(), 400);

        let dry: serde_json::Value = client
            .post(format!("{base}/api/memory/import"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "input": "- Uses Neovim for Rust editing", "apply": false }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let report_sha = dry["report_sha256"].as_str().unwrap().to_string();
        let applied: serde_json::Value = client
            .post(format!("{base}/api/memory/import"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({
                "input": "- Uses Neovim for Rust editing",
                "apply": true,
                "report_sha256": report_sha
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(applied["applied"], true);

        let plan: serde_json::Value = client
            .post(format!("{base}/api/memory/forget/plan"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "request": "Cobalt" }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(!plan.to_string().contains("Cobalt"));
        let plan_id = plan["plan_id"].as_str().unwrap().to_string();

        let confirm_denied = client
            .post(format!("{base}/api/memory/forget/confirm"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "plan_id": plan_id, "confirmed": false }))
            .send()
            .await
            .unwrap();
        assert_eq!(confirm_denied.status(), 403);

        let confirm_ok = client
            .post(format!("{base}/api/memory/forget/confirm"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "plan_id": plan_id, "confirmed": true, "verification_query": "Cobalt" }))
            .send()
            .await
            .unwrap();
        assert_eq!(confirm_ok.status(), 200);

        // 9. Side-chat Discretion GET/PUT
        let disc_put = client
            .put(format!("{base}/api/side-chats/slack-C1/discretion"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "rules": ["Keep updates under 3 bullets."] }))
            .send()
            .await
            .unwrap();
        assert_eq!(disc_put.status(), 200);

        // 10. GrokBot Parity Endpoints (/api/bootstrap, /api/ui-settings, /api/sections, /api/dm, /api/rules, /api/vault, /api/routines, /api/teach, /api/templates)
        let boot_resp: serde_json::Value = client
            .get(format!("{base}/api/bootstrap"))
            .bearer_auth("secret-token")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(boot_resp["bots"].is_array());
        assert!(boot_resp["ui_settings"].is_object());

        let ui_put = client
            .put(format!("{base}/api/ui-settings"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "theme": "dark", "accent_mode": "bot_color" }))
            .send()
            .await
            .unwrap();
        assert_eq!(ui_put.status(), 200);

        let sec_resp = client
            .post(format!("{base}/api/sections"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "name": "Engineering", "sort_order": 1 }))
            .send()
            .await
            .unwrap();
        assert_eq!(sec_resp.status(), 201);

        let dm_resp = client
            .post(format!("{base}/api/dm/muse/messages"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "content": "/do Check workspace status" }))
            .send()
            .await
            .unwrap();
        assert_eq!(dm_resp.status(), 200);

        let rule_resp = client
            .post(format!("{base}/api/rules"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "mode": "allow", "pattern": "git status*" }))
            .send()
            .await
            .unwrap();
        assert_eq!(rule_resp.status(), 201);

        let vault_resp = client
            .post(format!("{base}/api/vault"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({
                "kind": "login",
                "label": "GitHub",
                "username": "octocat",
                "domain": "github.com",
                "secret": "super-secret-val"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(vault_resp.status(), 201);
        let vault_json: serde_json::Value = vault_resp.json().await.unwrap();
        assert!(!vault_json.to_string().contains("super-secret-val"));

        let rtn_resp = client
            .post(format!("{base}/api/routines"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({
                "name": "Morning Check",
                "bot_id": "muse",
                "instruction": "Summarize goals",
                "timezone": "UTC",
                "hour": 9,
                "minute": 0
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(rtn_resp.status(), 201);

        let teach_start = client
            .post(format!("{base}/api/teach/start"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "bot_id": "muse" }))
            .send()
            .await
            .unwrap();
        assert_eq!(teach_start.status(), 200);
        let _ = client
            .post(format!("{base}/api/teach/step"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "step": "Click Settings button" }))
            .send()
            .await
            .unwrap();
        let teach_stop = client
            .post(format!("{base}/api/teach/stop"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "save_skill_name": "open-settings" }))
            .send()
            .await
            .unwrap();
        assert_eq!(teach_stop.status(), 200);

        let tpl_exp = client
            .post(format!("{base}/api/templates/export"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "name": "Test Team" }))
            .send()
            .await
            .unwrap();
        assert_eq!(tpl_exp.status(), 200);

        let prov_get = client
            .get(format!("{base}/api/system/providers"))
            .bearer_auth("secret-token")
            .send()
            .await
            .unwrap();
        assert_eq!(prov_get.status(), 200);
        let prov_body: serde_json::Value = prov_get.json().await.unwrap();
        assert!(
            prov_body["profiles"]
                .as_array()
                .is_some_and(|arr| arr.iter().any(|p| p["id"] == "chatgpt-default"))
        );

        let oauth_start = client
            .post(format!(
                "{base}/api/system/providers/chatgpt-default/oauth/start"
            ))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(oauth_start.status(), 200);
        let start_json: serde_json::Value = oauth_start.json().await.unwrap();
        assert!(
            start_json["authorize_url"]
                .as_str()
                .is_some_and(|s| s.contains("code_challenge_method=S256"))
        );

        let oauth_save = client
            .post(format!(
                "{base}/api/system/providers/chatgpt-default/oauth/save_bundle"
            ))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({
                "access_token": "tok-live-secret",
                "account_id": "acct-test",
                "account_label": "Darius ChatGPT"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(oauth_save.status(), 404);
        assert!(start_json.get("code_verifier").is_none());
        assert!(start_json.get("state").is_none());
        let oauth_url = reqwest::Url::parse(start_json["authorize_url"].as_str().unwrap()).unwrap();
        assert!(
            oauth_url
                .query_pairs()
                .any(|(key, value)| key == "client_id" && value == "dynamic_agent_client")
        );

        let custom_save = client
            .post(format!("{base}/api/system/providers/profile"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({
                "profile": {
                    "id": "custom-gw",
                    "display_name": "Custom Gateway",
                    "preset": "custom",
                    "protocol": "responses",
                    "auth_mode": "api_key",
                    "base_url": "https://api-a.example.com/v1",
                    "default_model": "gpt-4o",
                    "allow_screenshot_egress": false,
                    "enabled": true
                },
                "api_key": "sk-custom-secret",
                "workspace_default_profile_id": "custom-gw"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(custom_save.status(), 200);
        let cs_json: serde_json::Value = custom_save.json().await.unwrap();
        assert_eq!(cs_json["profile"]["connection_state"], "credentials_saved");
        assert_eq!(
            cs_json["providers"]["workspace_default_profile_id"],
            "custom-gw"
        );

        let oauth_disc = client
            .post(format!(
                "{base}/api/system/providers/chatgpt-default/oauth/disconnect"
            ))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(oauth_disc.status(), 200);
        assert_eq!(
            oauth_disc.json::<serde_json::Value>().await.unwrap()["connected"],
            false
        );

        // Phase 4 /api/v1 checks: cross-tenant SSE denial, marketplace scope confirmation, billing, probe
        let v1_boot = client
            .get(format!("{base}/api/v1/bootstrap"))
            .bearer_auth("secret-token")
            .send()
            .await
            .unwrap();
        assert_eq!(v1_boot.status(), 200);

        let v1_cross_sse = client
            .get(format!("{base}/api/v1/workspaces/other-tenant/events"))
            .bearer_auth("secret-token")
            .send()
            .await
            .unwrap();
        assert_eq!(v1_cross_sse.status(), 403);

        let probe_gmail = client
            .get(format!("{base}/api/v1/connectors/probe/gmail"))
            .bearer_auth("secret-token")
            .send()
            .await
            .unwrap();
        assert_eq!(probe_gmail.status(), 200);
        assert_eq!(
            probe_gmail.json::<serde_json::Value>().await.unwrap()["status"],
            "setup_required"
        );

        let mkt_list: serde_json::Value = client
            .get(format!("{base}/api/v1/marketplace"))
            .bearer_auth("secret-token")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(!mkt_list["catalog"].as_array().unwrap().is_empty());

        // Installing without confirming scopes must fail
        let mkt_denied = client
            .post(format!("{base}/api/v1/marketplace/install"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({ "id": "calendar-brief", "confirmed_scopes": [] }))
            .send()
            .await
            .unwrap();
        assert_eq!(mkt_denied.status(), 400);

        let mkt_ok = client
            .post(format!("{base}/api/v1/marketplace/install"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({
                "id": "calendar-brief",
                "confirmed_scopes": ["connectors.calendar.read"]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(mkt_ok.status(), 200);

        let bill_stat: serde_json::Value = client
            .get(format!("{base}/api/v1/billing/status"))
            .bearer_auth("secret-token")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(bill_stat["mode"], "self_hosted_byok");
        assert_eq!(bill_stat["byok_unrestricted"], true);

        // Client-supplied Stripe secret or redirect URL must be rejected.
        let bad_checkout = client
            .post(format!("{base}/api/v1/billing/checkout"))
            .bearer_auth("secret-token")
            .json(&serde_json::json!({
                "stripe_secret_key": "sk_test_client_supplied",
                "price_id": "price_default"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(bad_checkout.status(), 400);

        comp_handle.abort();
        spaces_handle.abort();
    }

    #[tokio::test]
    async fn hosted_cookie_sessions_enforce_csrf_membership_and_revocation() {
        let dir = tempfile::tempdir().unwrap();
        let workers = dir.path().join("workers");
        let plane = Arc::new(
            crate::hosted::HostedControlPlane::in_memory(
                &workers,
                b"test-hosted-master-key-32bytes",
            )
            .unwrap(),
        );
        plane
            .provision_workspace("ws_alpha", "t1", "Alpha", "tok-alpha")
            .unwrap();
        plane
            .provision_workspace("ws_beta", "t2", "Beta", "tok-beta")
            .unwrap();

        let cfg = GmConfig {
            storage_dir: dir.path().to_path_buf(),
            openbot: gm_core::config::OpenBotConfig {
                bots_dir: dir.path().join("bots"),
                ..Default::default()
            },
            ..Default::default()
        };
        let audit = Arc::new(AuditLogger::in_memory());
        let (tx, _rx) = broadcast::channel(64);
        let sup = OpenBotSupervisor::open(cfg, audit, tx).unwrap();
        let app = build_companion_router(
            CompanionWebServer::new(
                sup.clone(),
                BridgeRouter::new(sup, gm_core::config::ChatBridgesConfig::default()),
                Some("op-secret".into()),
                None,
            )
            .with_hosted_plane(plane.clone())
            .state,
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let base = format!("http://{addr}");
        let client = reqwest::Client::new();

        // 1. Operator issues expiring enrollment token for ws_alpha.
        let enroll_resp: serde_json::Value = client
            .post(format!("{base}/api/v1/auth/enroll"))
            .bearer_auth("op-secret")
            .json(&serde_json::json!({ "ws_id": "ws_alpha", "user_id": "alice", "role": "owner" }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let enroll_tok = enroll_resp["enrollment_token"]
            .as_str()
            .unwrap()
            .to_string();

        // 2. Redeem enrollment -> HttpOnly Secure cookie + CSRF token.
        let sess_resp = client
            .post(format!("{base}/api/v1/auth/session"))
            .json(&serde_json::json!({ "enrollment_token": enroll_tok }))
            .send()
            .await
            .unwrap();
        assert_eq!(sess_resp.status(), 200);
        let set_cookie = sess_resp
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(set_cookie.contains("HttpOnly"));
        assert!(set_cookie.contains("Secure"));
        let cookie_pair = set_cookie.split(';').next().unwrap().to_string();
        let sess_json: serde_json::Value = sess_resp.json().await.unwrap();
        let csrf = sess_json["csrf_token"].as_str().unwrap().to_string();

        let restored = client
            .get(format!("{base}/api/v1/auth/session"))
            .header(header::COOKIE, &cookie_pair)
            .header("x-dt-workspace-id", "default")
            .send()
            .await
            .unwrap();
        assert_eq!(restored.status(), 200);
        assert_eq!(restored.headers()[header::CACHE_CONTROL], "no-store");
        let restored: serde_json::Value = restored.json().await.unwrap();
        assert_eq!(restored["ws_id"], "ws_alpha");
        assert_eq!(restored["csrf_token"], csrf);
        assert!(restored.get("session_id").is_none());

        // 3. GET /api/v1/doctor succeeds with cookie and returns sanitized payload.
        let doc_ok = client
            .get(format!("{base}/api/v1/doctor"))
            .header(header::COOKIE, &cookie_pair)
            .send()
            .await
            .unwrap();
        assert_eq!(doc_ok.status(), 200);

        // 4. Mutation without CSRF token is rejected with 403.
        let no_csrf = client
            .post(format!("{base}/api/v1/marketplace/install"))
            .header(header::COOKIE, &cookie_pair)
            .json(&serde_json::json!({ "id": "calendar-brief", "confirmed_scopes": ["connectors.calendar.read"] }))
            .send()
            .await
            .unwrap();
        assert_eq!(no_csrf.status(), 403);

        // 5. Mutation with valid CSRF succeeds.
        let with_csrf = client
            .post(format!("{base}/api/v1/marketplace/install"))
            .header(header::COOKIE, &cookie_pair)
            .header("x-dt-csrf-token", &csrf)
            .json(&serde_json::json!({ "id": "calendar-brief", "confirmed_scopes": ["connectors.calendar.read"] }))
            .send()
            .await
            .unwrap();
        assert_eq!(with_csrf.status(), 200);

        // 6. Cross-tenant SSE for ws_beta is denied with 403.
        let cross_ws = client
            .get(format!("{base}/api/v1/workspaces/ws_beta/events"))
            .header(header::COOKIE, &cookie_pair)
            .send()
            .await
            .unwrap();
        assert_eq!(cross_ws.status(), 403);

        // 7. Server-side revocation invalidates session immediately.
        let revoked = client
            .post(format!("{base}/api/v1/auth/revoke"))
            .bearer_auth("op-secret")
            .json(&serde_json::json!({ "ws_id": "ws_alpha", "user_id": "alice" }))
            .send()
            .await
            .unwrap();
        assert_eq!(revoked.status(), 200);

        let after_revoke = client
            .get(format!("{base}/api/v1/doctor"))
            .header(header::COOKIE, &cookie_pair)
            .send()
            .await
            .unwrap();
        assert_eq!(after_revoke.status(), 401);

        handle.abort();
    }

    #[tokio::test]
    async fn test_hosted_cookie_session_and_csrf() {
        let dir = tempfile::tempdir().unwrap();
        let workers = dir.path().join("workers");
        let plane = Arc::new(
            crate::hosted::HostedControlPlane::in_memory(
                &workers,
                b"test-hosted-master-key-32bytes!",
            )
            .unwrap(),
        );
        plane
            .provision_workspace("ws_primary", "t1", "Primary", "tok-primary")
            .unwrap();
        let cfg = GmConfig {
            storage_dir: dir.path().to_path_buf(),
            openbot: gm_core::config::OpenBotConfig {
                bots_dir: dir.path().join("bots"),
                ..Default::default()
            },
            ..Default::default()
        };
        let (tx, _rx) = broadcast::channel(64);
        let sup = OpenBotSupervisor::open(cfg, Arc::new(AuditLogger::in_memory()), tx).unwrap();
        let app = build_companion_router(
            CompanionWebServer::new(
                sup.clone(),
                BridgeRouter::new(sup, gm_core::config::ChatBridgesConfig::default()),
                Some("op-token".into()),
                None,
            )
            .with_hosted_plane(plane.clone())
            .state,
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let base = format!("http://{addr}");
        let client = reqwest::Client::new();

        let enroll_resp: serde_json::Value = client
            .post(format!("{base}/api/v1/auth/enroll"))
            .bearer_auth("op-token")
            .json(&serde_json::json!({ "ws_id": "ws_primary", "user_id": "bob", "role": "owner" }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let enroll_tok = enroll_resp["enrollment_token"].as_str().unwrap();

        let sess_resp = client
            .post(format!("{base}/api/v1/auth/session"))
            .json(&serde_json::json!({ "enrollment_token": enroll_tok }))
            .send()
            .await
            .unwrap();
        assert_eq!(sess_resp.status(), 200);
        let cookie_pair = sess_resp
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let sess_json: serde_json::Value = sess_resp.json().await.unwrap();
        let csrf = sess_json["csrf_token"].as_str().unwrap().to_string();

        // /api/v1/workspaces includes user's active workspaces
        let ws_resp: serde_json::Value = client
            .get(format!("{base}/api/v1/workspaces"))
            .header(header::COOKIE, &cookie_pair)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(ws_resp["active_workspace"], "ws_primary");
        assert_eq!(ws_resp["workspaces"].as_array().unwrap().len(), 1);

        // Mutation without CSRF token is rejected
        let no_csrf = client
            .post(format!("{base}/api/v1/marketplace/install"))
            .header(header::COOKIE, &cookie_pair)
            .json(&serde_json::json!({ "id": "calendar-brief", "confirmed_scopes": ["connectors.calendar.read"] }))
            .send()
            .await
            .unwrap();
        assert_eq!(no_csrf.status(), 403);

        // Mutation with CSRF token succeeds
        let with_csrf = client
            .post(format!("{base}/api/v1/marketplace/install"))
            .header(header::COOKIE, &cookie_pair)
            .header("x-dt-csrf-token", &csrf)
            .json(&serde_json::json!({ "id": "calendar-brief", "confirmed_scopes": ["connectors.calendar.read"] }))
            .send()
            .await
            .unwrap();
        assert_eq!(with_csrf.status(), 200);

        handle.abort();
    }

    #[tokio::test]
    async fn host01_durable_replay_bridge_and_revocation_isolation() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = GmConfig {
            storage_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let (tx, _rx) = broadcast::channel(64);
        let sup =
            OpenBotSupervisor::open(cfg.clone(), Arc::new(AuditLogger::in_memory()), tx).unwrap();
        let bridge = BridgeRouter::new(sup.clone(), cfg.openbot.bridges.clone());
        let plane = Arc::new(
            crate::hosted::HostedControlPlane::open(
                dir.path().join("hosted.sqlite"),
                dir.path().join("workers"),
                b"0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        );
        plane
            .provision_workspace("ws_a", "t_a", "Workspace A", "tok-a")
            .unwrap();
        plane
            .provision_workspace("ws_b", "t_b", "Workspace B", "tok-b")
            .unwrap();

        let srv = CompanionWebServer::new(sup.clone(), bridge, Some("op-secret".into()), None)
            .with_hosted_plane(plane.clone());
        let app = build_companion_router(srv.state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let base = format!("http://{addr}");
        let client = reqwest::Client::new();

        // Health live and ready probes succeed with 200 without invoking a model.
        assert_eq!(
            client
                .get(format!("{base}/api/v1/health/live"))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        assert_eq!(
            client
                .get(format!("{base}/api/v1/health/ready"))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );

        let enroll_and_login = |ws_id: String, user_id: String| {
            let client = client.clone();
            let base = base.clone();
            async move {
                let enroll: serde_json::Value = client
                    .post(format!("{base}/api/v1/auth/enroll"))
                    .bearer_auth("op-secret")
                    .json(
                        &serde_json::json!({ "ws_id": ws_id, "user_id": user_id, "role": "owner" }),
                    )
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                let resp = client
                    .post(format!("{base}/api/v1/auth/session"))
                    .json(&serde_json::json!({ "enrollment_token": enroll["enrollment_token"] }))
                    .send()
                    .await
                    .unwrap();
                resp.headers()[header::SET_COOKIE]
                    .to_str()
                    .unwrap()
                    .split(';')
                    .next()
                    .unwrap()
                    .to_string()
            }
        };

        let cookie_a = enroll_and_login("ws_a".into(), "alice".into()).await;
        let cookie_b = enroll_and_login("ws_b".into(), "bob".into()).await;

        // Commit 40 baseline events + 3 while A is disconnected (41..43).
        for idx in 1..=40_u64 {
            sup.emit_agui(
                "BOT_EVENT",
                format!("run-{idx}"),
                "bot-1",
                serde_json::json!({ "ws_id": "ws_a", "n": idx }),
            );
        }
        for idx in 41..=43_u64 {
            sup.emit_agui(
                "BOT_EVENT",
                format!("run-{idx}"),
                "bot-1",
                serde_json::json!({ "ws_id": "ws_a", "n": idx }),
            );
        }

        // Reconnect A at cursor 40 while event 44 arrives live.
        let mut resp_a = client
            .get(format!("{base}/api/v1/workspaces/ws_a/events?after=40"))
            .header(header::COOKIE, &cookie_a)
            .send()
            .await
            .unwrap();
        assert_eq!(resp_a.status(), 200);

        let mut resp_b = client
            .get(format!("{base}/api/v1/workspaces/ws_b/events?after=0"))
            .header(header::COOKIE, &cookie_b)
            .send()
            .await
            .unwrap();
        assert_eq!(resp_b.status(), 200);

        sup.emit_agui(
            "BOT_EVENT",
            "run-44",
            "bot-1",
            serde_json::json!({ "ws_id": "ws_a", "n": 44 }),
        );

        let mut seen_a = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while seen_a.len() < 4 && tokio::time::Instant::now() < deadline {
            if let Ok(Ok(Some(bytes))) =
                tokio::time::timeout(Duration::from_millis(500), resp_a.chunk()).await
            {
                let text = String::from_utf8_lossy(&bytes);
                for line in text.lines() {
                    if let Some(id_str) = line.strip_prefix("id:")
                        && let Ok(seq) = id_str.trim().parse::<u64>()
                    {
                        seen_a.push(seq);
                    }
                }
            }
        }
        assert_eq!(seen_a, vec![41, 42, 43, 44]);

        // Revoke A while open; A stream must terminate while B continues receiving ws_b events.
        let rev = client
            .post(format!("{base}/api/v1/auth/revoke"))
            .bearer_auth("op-secret")
            .json(&serde_json::json!({ "ws_id": "ws_a", "user_id": "alice" }))
            .send()
            .await
            .unwrap();
        assert_eq!(rev.status(), 200);

        let a_closed = tokio::time::timeout(Duration::from_secs(2), async {
            while let Ok(Some(_)) = resp_a.chunk().await {}
        })
        .await
        .is_ok();
        assert!(a_closed, "Revoked workspace SSE stream must close promptly");

        sup.emit_agui(
            "BOT_EVENT",
            "run-b1",
            "bot-1",
            serde_json::json!({ "ws_id": "ws_b", "n": 1 }),
        );
        let mut saw_b = false;
        let b_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while !saw_b && tokio::time::Instant::now() < b_deadline {
            if let Ok(Ok(Some(chunk))) =
                tokio::time::timeout(Duration::from_millis(400), resp_b.chunk()).await
                && String::from_utf8_lossy(&chunk).contains("run-b1")
            {
                saw_b = true;
            }
        }
        assert!(
            saw_b,
            "Workspace B must continue unaffected after A revocation"
        );

        handle.abort();
    }

    #[tokio::test]
    async fn test_server_side_billing_and_marketplace_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = GmConfig {
            storage_dir: dir.path().to_path_buf(),
            openbot: gm_core::config::OpenBotConfig {
                bots_dir: dir.path().join("bots"),
                ..Default::default()
            },
            ..Default::default()
        };
        let (tx, _rx) = broadcast::channel(64);
        let sup = OpenBotSupervisor::open(cfg, Arc::new(AuditLogger::in_memory()), tx).unwrap();
        let routine = crate::routines::Routine {
            id: "r-v1-trig".into(),
            name: "V1 Trigger Routine".into(),
            bot_id: "researcher".into(),
            channel_id: None,
            instruction: "Run v1 check".into(),
            timezone: "UTC".into(),
            hour: 9,
            minute: 0,
            weekdays: vec![],
            quiet_hours: None,
            interval_minutes: Some(15),
            webhook_secret_ref: None,
            preferred_provider: None,
            preferred_model: None,
            enabled: true,
        };
        sup.save_routine(&routine).unwrap();

        let app = build_companion_router(
            CompanionWebServer::new(
                sup.clone(),
                BridgeRouter::new(sup, gm_core::config::ChatBridgesConfig::default()),
                Some("op-token".into()),
                None,
            )
            .state,
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let base = format!("http://{addr}");
        let client = reqwest::Client::new();

        // 1. /api/v1/routines/:id/trigger parity
        let trig_resp = client
            .post(format!("{base}/api/v1/routines/r-v1-trig/trigger"))
            .bearer_auth("op-token")
            .json(&serde_json::json!({ "delivery_id": "deliv-v1-1" }))
            .send()
            .await
            .unwrap();
        assert_eq!(trig_resp.status(), 200);

        // 2. Marketplace lifecycle: install -> update -> remove
        let install_bad = client
            .post(format!("{base}/api/v1/marketplace/install"))
            .bearer_auth("op-token")
            .json(&serde_json::json!({ "id": "calendar-brief", "confirmed_scopes": [] }))
            .send()
            .await
            .unwrap();
        assert_eq!(install_bad.status(), 400);

        let install_ok = client
            .post(format!("{base}/api/v1/marketplace/install"))
            .bearer_auth("op-token")
            .json(&serde_json::json!({
                "id": "calendar-brief",
                "confirmed_scopes": ["connectors.calendar.read"]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(install_ok.status(), 200);

        let update_ok = client
            .post(format!("{base}/api/v1/marketplace/calendar-brief/update"))
            .bearer_auth("op-token")
            .json(&serde_json::json!({
                "confirmed_scopes": ["connectors.calendar.read"]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(update_ok.status(), 200);

        let remove_ok = client
            .delete(format!("{base}/api/v1/marketplace/calendar-brief"))
            .bearer_auth("op-token")
            .send()
            .await
            .unwrap();
        assert_eq!(remove_ok.status(), 200);

        // 3. Server-side billing rejects client-supplied Stripe secret or redirect URL
        let bad_secret = client
            .post(format!("{base}/api/v1/billing/checkout"))
            .bearer_auth("op-token")
            .json(&serde_json::json!({
                "stripe_secret_key": "sk_live_spoof",
                "price_id": "price_default"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(bad_secret.status(), 400);

        let bad_redirect = client
            .post(format!("{base}/api/v1/billing/checkout"))
            .bearer_auth("op-token")
            .json(&serde_json::json!({
                "success_url": "https://evil.example/steal",
                "price_id": "price_default"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(bad_redirect.status(), 400);

        handle.abort();
    }

    #[tokio::test]
    async fn test_hosted_operator_workspace_creation_enrollment_and_data_isolation() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = gm_core::config::GmConfig {
            storage_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        cfg.openbot.bots_dir = dir.path().join("bots");
        let audit =
            Arc::new(gm_safety::AuditLogger::with_file(dir.path().join("audit.jsonl")).unwrap());
        let (tx, _rx) = tokio::sync::broadcast::channel(32);
        let sup = OpenBotSupervisor::open(cfg, audit, tx).unwrap();
        let bridge = BridgeRouter::new(sup.clone(), Default::default());
        let plane = Arc::new(
            crate::hosted::HostedControlPlane::open(
                dir.path().join("hosted-control.sqlite"),
                dir.path().join("workers"),
                b"super-strong-master-key-32bytes!",
            )
            .unwrap(),
        );
        let (addr, handle) = CompanionWebServer::new(sup, bridge, Some("op-secret".into()), None)
            .with_hosted_plane(plane)
            .bind_and_serve("127.0.0.1:0")
            .await
            .unwrap();
        let base = format!("http://{addr}");
        let client = reqwest::Client::new();

        // 1. Fresh server rejects unauthenticated workspace creation
        let unauth_ws = client
            .post(format!("{base}/api/v1/workspaces"))
            .json(&serde_json::json!({ "ws_id": "ws_fresh_a", "name": "Fresh A" }))
            .send()
            .await
            .unwrap();
        assert_eq!(unauth_ws.status(), 401);

        // 2. Operator creates ws_fresh_a and ws_fresh_b
        for (ws_id, name) in [("ws_fresh_a", "Fresh A"), ("ws_fresh_b", "Fresh B")] {
            let created = client
                .post(format!("{base}/api/v1/workspaces"))
                .bearer_auth("op-secret")
                .json(&serde_json::json!({ "ws_id": ws_id, "name": name }))
                .send()
                .await
                .unwrap();
            assert_eq!(created.status(), 201);
        }

        // 3. Operator enrolls Alice in ws_fresh_a and Bob in ws_fresh_b; both redeem sessions
        let login = |ws_id: &str, user_id: &str| {
            let client = client.clone();
            let base = base.clone();
            let ws_id = ws_id.to_string();
            let user_id = user_id.to_string();
            async move {
                let enr: serde_json::Value = client
                    .post(format!("{base}/api/v1/auth/enroll"))
                    .bearer_auth("op-secret")
                    .json(
                        &serde_json::json!({ "ws_id": ws_id, "user_id": user_id, "role": "owner" }),
                    )
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                let resp = client
                    .post(format!("{base}/api/v1/auth/session"))
                    .json(&serde_json::json!({ "enrollment_token": enr["enrollment_token"] }))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(resp.status(), 200);
                let cookie = resp
                    .headers()
                    .get(header::SET_COOKIE)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .split(';')
                    .next()
                    .unwrap()
                    .to_string();
                let body: serde_json::Value = resp.json().await.unwrap();
                (cookie, body["csrf_token"].as_str().unwrap().to_string())
            }
        };

        let (cookie_a, csrf_a) = login("ws_fresh_a", "alice").await;
        let (cookie_b, _csrf_b) = login("ws_fresh_b", "bob").await;

        // 4. Alice posts an idempotent DM in ws_fresh_a
        let post_dm = client
            .post(format!("{base}/api/v1/dm/muse/messages"))
            .header(header::COOKIE, &cookie_a)
            .header("x-dt-csrf-token", &csrf_a)
            .json(
                &serde_json::json!({ "content": "Secret Alpha DM", "client_msg_id": "dm-idemp-1" }),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(post_dm.status(), 200);

        // Retry with same client_msg_id does NOT duplicate user message
        let retry_dm = client
            .post(format!("{base}/api/v1/dm/muse/messages"))
            .header(header::COOKIE, &cookie_a)
            .header("x-dt-csrf-token", &csrf_a)
            .json(
                &serde_json::json!({ "content": "Secret Alpha DM", "client_msg_id": "dm-idemp-1" }),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(retry_dm.status(), 200);

        let msgs_a: Vec<serde_json::Value> = client
            .get(format!("{base}/api/v1/dm/muse/messages"))
            .header(header::COOKIE, &cookie_a)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let user_msgs_a: Vec<_> = msgs_a.iter().filter(|m| m["role"] == "user").collect();
        assert_eq!(user_msgs_a.len(), 1);
        assert_eq!(user_msgs_a[0]["content"], "Secret Alpha DM");

        // 5. Bob in ws_fresh_b sees ZERO of Alice's messages (routed through isolated workspace!)
        let msgs_b: Vec<serde_json::Value> = client
            .get(format!("{base}/api/v1/dm/muse/messages"))
            .header(header::COOKIE, &cookie_b)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(
            msgs_b.is_empty(),
            "Workspace B must not see Workspace A messages: {msgs_b:?}"
        );

        // 6. Bob trying to specify X-DT-Workspace-Id: ws_fresh_a gets 403 Forbidden
        let cross = client
            .get(format!("{base}/api/v1/dm/muse/messages"))
            .header(header::COOKIE, &cookie_b)
            .header("x-dt-workspace-id", "ws_fresh_a")
            .send()
            .await
            .unwrap();
        assert_eq!(cross.status(), 403);

        handle.abort();
    }

    async fn spawn_web_app_test_server(
        with_dist: bool,
    ) -> (tempfile::TempDir, String, tokio::task::JoinHandle<()>) {
        let dir = tempfile::tempdir().unwrap();
        let dist_dir = dir.path().join("dist");
        if with_dist {
            std::fs::create_dir_all(dist_dir.join("assets")).unwrap();
            std::fs::write(
                dist_dir.join("index.html"),
                "<!doctype html><html><body>DT_APP_MARKER</body></html>",
            )
            .unwrap();
            std::fs::write(
                dist_dir.join("assets/a.js"),
                "console.log('DT_ASSET_MARKER');",
            )
            .unwrap();
        }
        std::fs::write(dir.path().join("secret.txt"), "TOP_SECRET_OUTSIDE_DIST").unwrap();

        let cfg = GmConfig {
            storage_dir: dir.path().join("storage"),
            openbot: gm_core::config::OpenBotConfig {
                bots_dir: dir.path().join("bots"),
                ..Default::default()
            },
            ..Default::default()
        };
        let (tx, _rx) = broadcast::channel(32);
        let sup = OpenBotSupervisor::open(cfg, Arc::new(AuditLogger::in_memory()), tx).unwrap();
        sup.set_spaces_port(17434);
        let bridge = BridgeRouter::new(sup.clone(), Default::default());
        let srv = CompanionWebServer::new(sup, bridge, Some("op-secret".into()), None)
            .with_web_dist(if with_dist { Some(dist_dir) } else { None });
        let app = build_companion_router(srv.state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (dir, format!("http://{addr}"), handle)
    }

    fn header_str(resp: &reqwest::Response, name: impl reqwest::header::AsHeaderName) -> &str {
        match resp.headers().get(name) {
            Some(val) => val.to_str().unwrap_or_default(),
            None => "",
        }
    }

    #[tokio::test]
    async fn test_web_app_serves_index() {
        let (_dir, base, handle) = spawn_web_app_test_server(true).await;
        let resp = reqwest::get(format!("{base}/app/")).await.unwrap();
        assert_eq!(resp.status(), 200);
        let csp = header_str(&resp, "content-security-policy");
        assert!(csp.contains("default-src 'self'"));
        assert!(csp.contains("font-src 'self' data:"));
        assert!(csp.contains("frame-src http://127.0.0.1:17434"));
        assert!(csp.contains("frame-ancestors 'none'"));
        assert_eq!(header_str(&resp, "x-frame-options"), "DENY");
        let body = resp.text().await.unwrap();
        assert!(body.contains("DT_APP_MARKER"));
        handle.abort();
    }

    #[tokio::test]
    async fn test_web_app_redirects_bare_path() {
        let (_dir, base, handle) = spawn_web_app_test_server(true).await;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let resp = client.get(format!("{base}/app")).send().await.unwrap();
        assert_eq!(resp.status(), 308);
        assert_eq!(header_str(&resp, header::LOCATION), "/app/");
        handle.abort();
    }

    #[tokio::test]
    async fn test_web_app_serves_asset_with_mime_and_cache() {
        let (_dir, base, handle) = spawn_web_app_test_server(true).await;
        let resp = reqwest::get(format!("{base}/app/assets/a.js"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let ct = header_str(&resp, header::CONTENT_TYPE);
        assert!(ct.starts_with("text/javascript"), "content-type={ct}");
        let cc = header_str(&resp, header::CACHE_CONTROL);
        assert!(cc.contains("immutable"), "cache-control={cc}");
        let body = resp.text().await.unwrap();
        assert!(body.contains("DT_ASSET_MARKER"));
        handle.abort();
    }

    #[tokio::test]
    async fn test_web_app_spa_fallback() {
        let (_dir, base, handle) = spawn_web_app_test_server(true).await;
        let resp = reqwest::get(format!("{base}/app/threads/123"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = resp.text().await.unwrap();
        assert!(body.contains("DT_APP_MARKER"));
        handle.abort();
    }

    #[tokio::test]
    async fn test_web_app_missing_asset_404() {
        let (_dir, base, handle) = spawn_web_app_test_server(true).await;
        let resp = reqwest::get(format!("{base}/app/assets/nope.js"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
        handle.abort();
    }

    #[tokio::test]
    async fn test_web_app_blocks_traversal() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (_dir, base, handle) = spawn_web_app_test_server(true).await;
        let host_port = base.trim_start_matches("http://");
        for raw_path in [
            "/app/..%2fsecret.txt",
            "/app/%2e%2e/secret.txt",
            "/app/assets/..%2f..%2fsecret.txt",
        ] {
            let mut stream = tokio::net::TcpStream::connect(host_port).await.unwrap();
            let req = format!(
                "GET {raw_path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(req.as_bytes()).await.unwrap();
            let mut buf = Vec::new();
            stream.read_to_end(&mut buf).await.unwrap();
            let text = String::from_utf8_lossy(&buf);
            assert!(
                text.starts_with("HTTP/1.1 404") || text.starts_with("HTTP/1.1 400"),
                "expected 404/400 for {raw_path}, got: {text}"
            );
            assert!(
                !text.contains("TOP_SECRET_OUTSIDE_DIST"),
                "traversal leaked secret for {raw_path}"
            );
        }
        handle.abort();
    }

    #[tokio::test]
    async fn test_web_app_not_installed() {
        let (_dir, base, handle) = spawn_web_app_test_server(false).await;
        let resp = reqwest::get(format!("{base}/app/")).await.unwrap();
        assert_eq!(resp.status(), 404);
        let body = resp.text().await.unwrap();
        assert!(body.to_lowercase().contains("not installed"));
        handle.abort();
    }
}
