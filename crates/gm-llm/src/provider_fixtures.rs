use crate::{
    BudgetManager, ChatMessage, HybridLlmClient, KeychainSecrets, LlmBackend, LlmRouter,
    ModelTarget, OAuthTokenBundle, ProviderKind, ReqwestLlmBackend, TaskType,
    build_oauth_authorize_url, disconnect_oauth_account, exchange_oauth_code, load_oauth_bundle,
    store_oauth_bundle,
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use gm_core::config::{
    BudgetConfig, ProviderAuthMode, ProviderPreset, ProviderProfileConfig, ProvidersConfig,
    WireProtocol,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::net::TcpListener;

#[derive(Clone, Default)]
struct FixtureState {
    nous_calls: Arc<AtomicUsize>,
    saw_auth_on_changed_dest: Arc<AtomicUsize>,
    saw_image_when_disabled: Arc<AtomicUsize>,
}

async fn spawn_fixture_server() -> (String, FixtureState) {
    let state = FixtureState::default();
    let app = Router::new()
        .route("/oauth/token", post(handle_oauth_token))
        .route("/v1/models", get(handle_models))
        .route("/v1/responses", post(handle_responses))
        .route("/v1/chat/completions", post(handle_chat_completions))
        .route("/changed/v1/chat/completions", post(handle_changed_dest))
        .with_state(state.clone());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), state)
}

async fn handle_oauth_token(body: String) -> impl IntoResponse {
    if body.contains("grant_type=refresh_token") {
        Json(json!({
            "access_token": "fresh_oauth_token_v2",
            "refresh_token": "refresh_tok_v2",
            "expires_in": 3600
        }))
    } else {
        Json(json!({
            "access_token": "initial_oauth_token_v1",
            "refresh_token": "refresh_tok_v1",
            "expires_in": 3600
        }))
    }
}

async fn handle_models() -> impl IntoResponse {
    Json(json!({
        "data": [
            { "id": "fixture-fast-1" },
            { "id": "fixture-reasoning-2" }
        ]
    }))
}

async fn handle_responses(Json(body): Json<Value>) -> impl IntoResponse {
    let stream = body["stream"].as_bool().unwrap_or(false);
    let model = body["model"].as_str().unwrap_or("gpt-5").to_string();
    if stream {
        let sse = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello \"}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Responses!\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_123\",\"usage\":{\"input_tokens\":12,\"output_tokens\":4}}}\n\n"
        );
        (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            sse.to_string(),
        )
            .into_response()
    } else {
        Json(json!({
            "id": "resp_sync",
            "model": model,
            "output_text": "Sync Responses OK",
            "usage": { "input_tokens": 10, "output_tokens": 3 }
        }))
        .into_response()
    }
}

async fn handle_chat_completions(
    State(state): State<FixtureState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if let Some(arr) = body["messages"].as_array() {
        for m in arr {
            if m["content"].is_array() {
                state.saw_image_when_disabled.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let model = body["model"].as_str().unwrap_or("unknown");

    // Simulate Nous Portal expired token on first call, refreshed token on retry
    if model == "Hermes-3-Llama-3.1-70B" {
        let count = state.nous_calls.fetch_add(1, Ordering::SeqCst);
        if count == 0 && auth.contains("stale_nous_token") {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "expired" })),
            )
                .into_response();
        }
    }

    let stream = body["stream"].as_bool().unwrap_or(false);
    if stream {
        let sse = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"Stream({model})\"}}}}]}}\n\n\
             data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":8,\"completion_tokens\":5}}}}\n\n\
             data: [DONE]\n\n"
        );
        (StatusCode::OK, [("content-type", "text/event-stream")], sse).into_response()
    } else {
        Json(json!({
            "id": "chat_1",
            "model": model,
            "choices": [{ "message": { "content": format!("Sync({model})") } }],
            "usage": { "prompt_tokens": 8, "completion_tokens": 5 }
        }))
        .into_response()
    }
}

async fn handle_changed_dest(
    State(state): State<FixtureState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if headers.contains_key("authorization") {
        state
            .saw_auth_on_changed_dest
            .fetch_add(1, Ordering::SeqCst);
    }
    Json(json!({
        "id": "never_called",
        "choices": [{ "message": { "content": "nope" } }]
    }))
}

#[tokio::test]
async fn provider_presets_oauth_responses_discovery_and_revalidation_fixtures() {
    let (base, state) = spawn_fixture_server().await;
    let secrets = KeychainSecrets::in_memory();

    // 1. ChatGPT Sign-In PKCE -> Exchange -> Streamed /v1/responses -> Separate Allowance -> Disconnect
    let pkce = build_oauth_authorize_url(
        ProviderPreset::ChatGpt,
        "client_123",
        "http://127.0.0.1:7433/oauth/cb",
        None,
    )
    .unwrap();
    assert!(pkce.authorize_url.contains("code_challenge_method=S256"));

    let http = reqwest::Client::new();
    let chatgpt_bundle = exchange_oauth_code(
        &http,
        ProviderPreset::ChatGpt,
        &format!("{base}/oauth/token"),
        "client_123",
        "auth_code_xyz",
        &pkce.redirect_uri,
        &pkce.code_verifier,
        "acct_work",
        "Work ChatGPT",
    )
    .await
    .unwrap();
    let chatgpt_ref =
        store_oauth_bundle(&secrets, ProviderPreset::ChatGpt, &chatgpt_bundle).unwrap();
    assert!(chatgpt_ref.contains("gm_chatgpt_oauth_acct_work"));

    let mut chatgpt_target =
        ModelTarget::simple(ProviderKind::ChatGpt, format!("{base}/v1"), "gpt-5", false);
    chatgpt_target.credential_ref = Some(chatgpt_ref.clone());

    let backend = Arc::new(ReqwestLlmBackend::with_secrets(&secrets));
    let budget = Arc::new(BudgetManager::new(BudgetConfig::default()));
    let client = HybridLlmClient::new(backend.clone(), budget.clone());

    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let resp = client
        .complete_stream(
            &chatgpt_target,
            None,
            &[ChatMessage::text("user", "Say hi")],
            128,
            0.2,
            tx,
        )
        .await
        .unwrap();
    assert_eq!(resp.content, "Hello Responses!");
    assert_eq!(resp.estimated_cost_usd, 0.0);
    let mut streamed = String::new();
    while let Some(tok) = rx.recv().await {
        streamed.push_str(&tok);
    }
    assert_eq!(streamed, "Hello Responses!");

    let breakdown = budget.usage_breakdown(7).unwrap();
    assert_eq!(breakdown.daily_spent_usd, 0.0);
    assert_eq!(breakdown.chatgpt_allowance_requests, 1);
    assert_eq!(breakdown.chatgpt_allowance_tokens, 16);

    disconnect_oauth_account(&secrets, &chatgpt_ref).unwrap();
    assert!(load_oauth_bundle(&secrets, &chatgpt_ref).is_err());

    // 2. Nous Portal OAuth refresh-on-401 (isolated from Darius)
    let nous_bundle = OAuthTokenBundle {
        provider_id: "nous_portal".into(),
        account_id: "nous_1".into(),
        account_label: "Nous Account".into(),
        access_token: "stale_nous_token".into(),
        refresh_token: Some("rt_nous".into()),
        expires_at: None,
        token_endpoint: format!("{base}/oauth/token"),
        client_id: "nous_client".into(),
    };
    let nous_ref = store_oauth_bundle(&secrets, ProviderPreset::NousPortal, &nous_bundle).unwrap();
    assert!(nous_ref.contains("gm_nous_portal_nous_1"));
    let mut nous_target = ModelTarget::simple(
        ProviderKind::NousPortal,
        format!("{base}/v1"),
        "Hermes-3-Llama-3.1-70B",
        false,
    );
    nous_target.credential_ref = Some(nous_ref.clone());
    let nous_resp = client
        .execute_routed(&nous_target, None, &[ChatMessage::text("user", "Ping")])
        .await
        .unwrap();
    assert_eq!(nous_resp.content, "Sync(Hermes-3-Llama-3.1-70B)");
    assert_eq!(
        load_oauth_bundle(&secrets, &nous_ref).unwrap().access_token,
        "fresh_oauth_token_v2"
    );
    disconnect_oauth_account(&secrets, &nous_ref).unwrap();

    // 3. Kilo Gateway & OpenRouter streaming + model discovery
    let discovered = backend
        .discover_models(&format!("{base}/v1"), None)
        .await
        .unwrap();
    assert_eq!(
        discovered,
        vec![
            "fixture-fast-1".to_string(),
            "fixture-reasoning-2".to_string()
        ]
    );

    let mut kilo_target = ModelTarget::simple(
        ProviderKind::KiloGateway,
        format!("{base}/v1"),
        "anthropic/claude-sonnet-4",
        false,
    );
    kilo_target.auth_mode = ProviderAuthMode::None;
    let (tx2, mut rx2) = tokio::sync::mpsc::channel(8);
    let kilo_resp = backend
        .complete_stream(
            &kilo_target,
            &[ChatMessage::text("user", "Kilo")],
            64,
            0.2,
            tx2,
        )
        .await
        .unwrap();
    while rx2.recv().await.is_some() {}
    assert_eq!(kilo_resp.content, "Stream(anthropic/claude-sonnet-4)");

    // 4. Custom endpoint: Responses + Screenshot egress gate + Destination change revalidation
    let mut custom_resp_target = ModelTarget::simple(
        ProviderKind::Custom,
        format!("{base}/v1"),
        "custom-model",
        false,
    );
    custom_resp_target.protocol = WireProtocol::Responses;
    let custom_resp = backend
        .complete(
            &custom_resp_target,
            &[ChatMessage::with_jpeg("user", "Private screen", &[1, 2, 3])],
        )
        .await
        .unwrap();
    assert_eq!(custom_resp.content, "Sync Responses OK");
    assert_eq!(state.saw_image_when_disabled.load(Ordering::SeqCst), 0);

    // Destination change without revalidation blocks sending credential
    let mut cfg = ProvidersConfig::default();
    let mut prof = ProviderProfileConfig {
        id: "custom-fixture".into(),
        display_name: "Fixture".into(),
        preset: ProviderPreset::Custom,
        protocol: WireProtocol::ChatCompletions,
        auth_mode: ProviderAuthMode::ApiKey,
        base_url: format!("{base}/v1"),
        validated_origin: Some(base.clone()),
        selected_model: "fixture-fast-1".into(),
        discovered_models: vec![],
        credential_ref: Some("secret://keychain/custom_fixture_key".into()),
        account_id: None,
        account_label: None,
        allow_screenshot_egress: false,
        fallback_profile_id: None,
        connection_state: gm_core::config::ProviderConnectionState::Connected,
        last_error: None,
    };
    cfg.upsert_profile(prof.clone(), true).unwrap();
    prof.base_url = "http://127.0.0.1:1/changed/v1".to_string();
    let _ = cfg.upsert_profile(prof, false).unwrap();
    let router = LlmRouter::new(cfg)
        .with_preference(Some("custom-fixture"), None)
        .unwrap();
    let (changed_target, _) = router.route(TaskType::Planning);
    assert!(changed_target.validated_origin.is_none());
    let err = backend
        .complete(&changed_target, &[ChatMessage::text("user", "Hi")])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("revalidate credentials"));
    assert_eq!(state.saw_auth_on_changed_dest.load(Ordering::SeqCst), 0);
}
