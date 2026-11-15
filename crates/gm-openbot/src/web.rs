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

#[derive(Clone)]
pub struct CompanionWebState {
    pub supervisor: OpenBotSupervisor,
    pub bridge_router: BridgeRouter,
    pub bearer_token: Option<String>,
    pub slack_signing_secret: Option<String>,
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
}

fn check_auth(
    headers: &HeaderMap,
    expected_token: Option<&str>,
) -> std::result::Result<(), StatusCode> {
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

pub fn build_companion_router(state: CompanionWebState) -> Router {
    let spaces_port_atomic = state.supervisor.spaces_port_atomic();
    Router::new()
        .route("/", get(serve_index))
        .route("/companion.js", get(serve_script))
        .route("/admin/audit", get(serve_index))
        .route("/.well-known/agent.json", get(serve_agent_card))
        .route("/a2a/card", get(serve_agent_card))
        .route("/a2a/tasks", post(create_a2a_task))
        .route("/a2a/tasks/:id", get(get_a2a_task))
        .route("/api/runtime", get(runtime_status))
        .route("/api/events", get(replay_events))
        .route("/api/bots", get(list_bots))
        .route("/api/queue", get(list_queue).post(enqueue_task))
        .route("/api/tasks/:id/:action", post(control_task_route))
        .route("/api/channels", get(list_channels).post(create_channel))
        .route(
            "/api/channels/:id/messages",
            get(list_channel_messages).post(post_channel_message),
        )
        .route("/api/messages/:id/reactions", post(post_message_reaction))
        .route("/api/audit", get(query_audit))
        .route("/api/activity", get(list_activity).post(create_activity_post))
        .route("/api/activity/prompt", post(update_activity_prompt))
        .route("/api/activity/:id/rate", post(rate_activity_post))
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
        .route("/api/skills", get(list_skills_route))
        .route("/api/skills/custom", post(create_custom_skill_route))
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
        .route("/ag-ui/runs", post(start_agui_run))
        .route("/ag-ui/events", get(stream_agui_events))
        .route("/bridges/inbound", post(handle_bridge_inbound))
        .route("/bridges/slack/events", post(handle_slack_webhook))
        .layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let sp_atomic = Arc::clone(&spaces_port_atomic);
                async move {
                    let req_path = request.uri().path().to_string();
                    let is_html_route = req_path == "/" || req_path == "/admin/audit";
                    let mut response = next.run(request).await;
                    let csp = if is_html_route {
                        let spaces_port = sp_atomic.load(Ordering::SeqCst);
                        format!(
                            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; frame-src http://127.0.0.1:{spaces_port}; frame-ancestors 'none'; base-uri 'none'; object-src 'none'"
                        )
                    } else {
                        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; object-src 'none'".to_string()
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
        Self {
            state: CompanionWebState {
                supervisor,
                bridge_router,
                bearer_token,
                slack_signing_secret,
            },
        }
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

async fn serve_script() -> impl IntoResponse {
    (
        [("content-type", "text/javascript; charset=utf-8")],
        COMPANION_UI_JS,
    )
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
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    Ok(Json(state.supervisor.roster().list_bots()))
}

async fn list_queue(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let tasks = state
        .supervisor
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
    State(state): State<CompanionWebState>,
    body: Option<Json<serde_json::Value>>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let ok = match action.as_str() {
        "pause" => state
            .supervisor
            .control_task(&id, &QueueControlAction::Pause)
            .map_err(|_| StatusCode::BAD_REQUEST)?,
        "resume" => state
            .supervisor
            .control_task(&id, &QueueControlAction::Resume)
            .map_err(|_| StatusCode::BAD_REQUEST)?,
        "retry" => state
            .supervisor
            .control_task(&id, &QueueControlAction::Retry)
            .map_err(|_| StatusCode::BAD_REQUEST)?,
        "cancel" => state
            .supervisor
            .control_task(&id, &QueueControlAction::Cancel)
            .map_err(|_| StatusCode::BAD_REQUEST)?,
        "wheel-resume" => {
            let note = body
                .as_ref()
                .and_then(|b| b.get("note"))
                .and_then(|v| v.as_str());
            state
                .supervisor
                .queue()
                .resume_from_human(&id, note)
                .map_err(|_| StatusCode::BAD_REQUEST)?
        }
        "reassign" => {
            let target = body
                .as_ref()
                .and_then(|b| b.get("target_bot_id"))
                .and_then(|v| v.as_str())
                .ok_or(StatusCode::BAD_REQUEST)?;
            state
                .supervisor
                .control_task(
                    &id,
                    &QueueControlAction::Reassign {
                        target_bot_id: target.to_string(),
                    },
                )
                .map_err(|_| StatusCode::BAD_REQUEST)?
        }
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    Ok(Json(serde_json::json!({ "ok": ok, "task_id": id })))
}

async fn list_channels(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let chans = state
        .supervisor
        .channels()
        .list_channels(false)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(chans))
}

async fn create_channel(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<CreateChannelBody>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    if state
        .supervisor
        .channels()
        .get_channel(&body.id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .is_some()
    {
        return Err(StatusCode::CONFLICT);
    }
    let channel = state
        .supervisor
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
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    state
        .supervisor
        .channels()
        .get_channel(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let msgs = state
        .supervisor
        .channels()
        .list_messages(&id, 100)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(msgs))
}

async fn post_channel_message(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<CompanionWebState>,
    Json(body): Json<ChannelPostBody>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let author = body.author.as_deref().unwrap_or("operator");
    let (msg, spawned) = state
        .supervisor
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
    let reactions = state
        .supervisor
        .channels()
        .add_reaction(&id, &body.emoji, actor)
        .map_err(|err| {
            let msg = err.to_string();
            if msg.contains("Unknown channel message") {
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
        "message_id": id,
        "reactions": reactions,
    })))
}

async fn list_activity(
    headers: HeaderMap,
    Query(q): Query<ActivityQuery>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let posts = state
        .supervisor
        .feed_store()
        .list_posts(limit)
        .map_err(map_gm_error)?;
    let brief_prompt = state
        .supervisor
        .feed_store()
        .get_brief_prompt()
        .map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({
        "posts": posts,
        "brief_prompt": brief_prompt,
    })))
}

async fn create_activity_post(
    headers: HeaderMap,
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
    state
        .supervisor
        .feed_store()
        .upsert_post(&post)
        .map_err(map_gm_error)?;
    let _ = state
        .supervisor
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
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let mgr = state.supervisor.goal_manager();
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
    State(state): State<CompanionWebState>,
    Json(body): Json<CreateGoalBody>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let mgr = state.supervisor.goal_manager();
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
    let _ = state
        .supervisor
        .daemon_event_sender()
        .send(DaemonEvent::ActivityUpdated {
            reason: "goal_created".into(),
        });
    Ok((StatusCode::CREATED, Json(created)))
}

async fn patch_goal_route(
    headers: HeaderMap,
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
    let mgr = state.supervisor.goal_manager();
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
    let _ = state
        .supervisor
        .daemon_event_sender()
        .send(DaemonEvent::ActivityUpdated {
            reason: "goal_updated".into(),
        });
    Ok(Json(result))
}

async fn get_goal_file_route(
    headers: HeaderMap,
    Path((slug, rel_path)): Path<(String, String)>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let mgr = state.supervisor.goal_manager();
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
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    let spaces = state
        .supervisor
        .spaces()
        .list_spaces()
        .map_err(map_gm_error)?;
    Ok(Json(spaces))
}

async fn create_space_route(
    headers: HeaderMap,
    State(state): State<CompanionWebState>,
    Json(body): Json<CreateSpaceBody>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    if !state.supervisor.config().muse.enable_spaces {
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
    let created = state
        .supervisor
        .spaces()
        .create_space(&body.slug, body.title.as_deref(), kind, initial)
        .map_err(map_gm_error)?;
    let _ = state
        .supervisor
        .daemon_event_sender()
        .send(DaemonEvent::ActivityUpdated {
            reason: "space_created".into(),
        });
    Ok((StatusCode::CREATED, Json(created)))
}

async fn get_space_url_route(
    headers: HeaderMap,
    Path(slug): Path<String>,
    State(state): State<CompanionWebState>,
) -> std::result::Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, state.bearer_token.as_deref())?;
    if !state.supervisor.config().muse.enable_spaces {
        return Err(ApiErr::msg(
            StatusCode::FORBIDDEN,
            "Spaces feature is disabled in MuseConfig",
        ));
    }
    let sp_port = state.supervisor.spaces_port();
    let url = state
        .supervisor
        .spaces()
        .mint_url_for_slug(&slug, sp_port)
        .map_err(map_gm_error)?;
    Ok(Json(serde_json::json!({
        "slug": slug,
        "url": url,
        "spaces_port": sp_port,
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
    let reg = SkillRegistry::open_with_scrubber(
        &state.supervisor.shared_paths(),
        state.supervisor.scrubber().clone(),
    )
    .map_err(map_gm_error)?;
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
        "USER.md" => Ok(shared.user_md()),
        "MEMORY.md" => Ok(shared.memory_md()),
        "PROACTIVE_PREFERENCES.md" => Ok(shared.proactive_preferences_md()),
        _ => Err(ApiErr::msg(
            StatusCode::BAD_REQUEST,
            format!(
                "Unsupported runtime file '{file}'; allowed: SOUL.md, IDENTITY.md, USER.md, MEMORY.md, PROACTIVE_PREFERENCES.md"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spaces::SpacesWebServer;
    use gm_core::config::GmConfig;
    use gm_safety::AuditLogger;
    use tokio::sync::broadcast;

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
        let sup =
            OpenBotSupervisor::open(cfg.clone(), Arc::new(AuditLogger::in_memory()), tx).unwrap();
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

        comp_handle.abort();
        spaces_handle.abort();
    }
}
