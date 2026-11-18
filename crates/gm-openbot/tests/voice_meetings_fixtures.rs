use axum::{
    Json, Router,
    extract::Path,
    http::StatusCode,
    routing::{delete, get, post},
};
use base64::Engine;
use gm_core::config::GmConfig;
use gm_openbot::{
    AsteriskAriConfig, AsteriskSipClient, BridgeChannelMapping, BridgeMappingStore, BridgeRouter,
    ConnectorStore, ConnectorTransport, CustomConnectorInput, GoogleMeetClient, GoogleMeetConfig,
    InMemorySecretBackend, LocalVoiceEngine, OpenBotSupervisor, OutboundRelayClient,
    RelayPollBatch, SignedRelayEnvelope, SttMode, TtsMode, VoiceConfig, VoiceSession,
    VoiceSessionState, ZoomRtmsClient, ZoomRtmsConfig,
};
use sha1::{Digest, Sha1};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn spawn_ws_fixture(frames: Vec<String>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let key = req
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    if k.trim().eq_ignore_ascii_case("sec-websocket-key") {
                        Some(v.trim().to_string())
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
            let mut sha = Sha1::new();
            sha.update(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes());
            let accept = base64::engine::general_purpose::STANDARD.encode(sha.finalize());
            let resp = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            for payload in frames {
                let bytes = payload.as_bytes();
                let mut frame = vec![0x81u8];
                if bytes.len() < 126 {
                    frame.push(bytes.len() as u8);
                } else {
                    frame.push(126);
                    frame.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
                }
                frame.extend_from_slice(bytes);
                let _ = stream.write_all(&frame).await;
            }
            // Close frame
            let _ = stream.write_all(&[0x88, 0x00]).await;
        }
    });
    format!("ws://127.0.0.1:{}/events", addr.port())
}

#[tokio::test]
async fn relay_bridges_connectors_voice_sip_zoom_and_meet_fixtures() {
    // 1. Outbound relay + persistent bridge channel mappings + dedup + discretion outbound block
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = GmConfig {
        storage_dir: dir.path().join("storage"),
        ..Default::default()
    };
    cfg.openbot.bridges.slack_enabled = true;
    cfg.openbot.bridges.slack_allowed_channel_ids = vec!["C-RELAY".into()];
    let (tx, _) = tokio::sync::broadcast::channel(16);
    let audit = Arc::new(gm_safety::AuditLogger::in_memory());
    let sup = OpenBotSupervisor::open_with_secret_backend(
        cfg.clone(),
        audit,
        tx,
        Arc::new(InMemorySecretBackend::default()),
    )
    .unwrap();
    let mappings = BridgeMappingStore::open(dir.path().join("mappings.sqlite")).unwrap();
    mappings
        .upsert_mapping(&BridgeChannelMapping {
            platform: "slack".into(),
            external_channel_id: "C-MAPPED".into(),
            internal_channel_id: "eng-alerts".into(),
            space_id: Some("space-alpha".into()),
            bot_id: Some("muse".into()),
            enabled: true,
            updated_at: String::new(),
        })
        .unwrap();

    let sent_replies = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let sent_ref = sent_replies.clone();
    let relay_client_signer =
        OutboundRelayClient::new("https://placeholder.example.com", "relay-secret", "tok-1")
            .unwrap();
    let mut env = SignedRelayEnvelope {
        delivery_id: "deliv-100".into(),
        platform: "slack".into(),
        external_channel_id: "C-MAPPED".into(),
        sender_name: "alice".into(),
        text: "@muse check alerts".into(),
        timestamp: chrono::Utc::now().timestamp(),
        signature: String::new(),
    };
    relay_client_signer.sign_envelope(&mut env);
    let batch = Arc::new(RelayPollBatch {
        envelopes: vec![env.clone(), env],
        next_cursor: Some("cur-2".into()),
    });
    let batch_ref = batch.clone();

    let ari_ws_url = spawn_ws_fixture(vec![
        serde_json::json!({ "speaker": "caller", "transcript": "Please schedule a follow-up." })
            .to_string(),
    ])
    .await;
    let zoom_ws_url = spawn_ws_fixture(vec![
        serde_json::json!({ "event": "participant.joined", "participant": "Darius" }).to_string(),
        serde_json::json!({
            "event": "transcript.data",
            "user_name": "Darius",
            "data": "Action item: ship v0.6.0 today."
        })
        .to_string(),
    ])
    .await;

    let app = Router::new()
        .route(
            "/v1/relay/poll",
            get({
                let b = batch_ref;
                move || {
                    let out = (*b).clone();
                    async move { Json(out) }
                }
            }),
        )
        .route(
            "/v1/relay/send",
            post({
                let s = sent_ref;
                move |Json(v): Json<serde_json::Value>| {
                    let s = s.clone();
                    async move {
                        s.lock().unwrap().push(v);
                        Json(serde_json::json!({ "ok": true }))
                    }
                }
            }),
        )
        .route(
            "/mcp/health",
            get(|| async { Json(serde_json::json!({ "status": "ok" })) }),
        )
        .route(
            "/mcp/fail",
            get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "down") }),
        )
        .route(
            "/voice/inference",
            post(|| async { Json(serde_json::json!({ "text": "hello world" })) }),
        )
        .route("/voice/tts", post(|| async { vec![1u8, 2, 3, 4] }))
        .route(
            "/ari/channels",
            post(|| async { Json(serde_json::json!({ "id": "chan-sip-1" })) }),
        )
        .route(
            "/ari/channels/externalMedia",
            post(|| async { Json(serde_json::json!({ "id": "ext-1" })) }),
        )
        .route(
            "/ari/channels/:id/play",
            post(|Path(_id): Path<String>| async { Json(serde_json::json!({ "playing": true })) }),
        )
        .route(
            "/ari/channels/:id/redirect",
            post(|Path(_id): Path<String>| async { Json(serde_json::json!({ "redirected": true })) }),
        )
        .route(
            "/ari/channels/:id",
            delete(|Path(_id): Path<String>| async { StatusCode::NO_CONTENT }),
        )
        .route(
            "/zoom/live_meetings/:id/rtms/join",
            post(|| async { Json(serde_json::json!({ "ok": true })) }),
        )
        .route(
            "/zoom/live_meetings/:id/rtms/leave",
            post(|| async { Json(serde_json::json!({ "ok": true })) }),
        )
        .route(
            "/meet/v2/spaces/:code/join",
            post(|| async {
                Json(serde_json::json!({ "conference": "conferenceRecords/conf-42" }))
            }),
        )
        .route(
            "/meet/v2/conferenceRecords/:id/transcripts/entries",
            get(|| async {
                Json(serde_json::json!({
                    "transcriptEntries": [
                        { "name": "entries/1", "participant": "Alice", "text": "Agreed on milestone" }
                    ]
                }))
            }),
        )
        .route(
            "/meet/v2/conferenceRecords/:id/endActiveConference",
            post(|| async { Json(serde_json::json!({ "ended": true })) }),
        );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = format!("http://127.0.0.1:{}", addr.port());

    let relay = OutboundRelayClient::new(&base, "relay-secret", "tok-1").unwrap();
    let router = BridgeRouter::new(sup.clone(), cfg.openbot.bridges.clone());
    // Same batch contains duplicate delivery_id -> only 1 should ingest!
    let (replies, cursor) = relay
        .poll_and_ingest(None, &router, &mappings)
        .await
        .unwrap();
    assert_eq!(replies.len(), 1);
    assert!(replies[0].accepted);
    assert_eq!(cursor.as_deref(), Some("cur-2"));

    // Outbound discretion blocks private details from USER.md and unrequested outbound replies on mapped channels
    std::fs::write(
        sup.shared_paths().user_md(),
        "# User\n- Secret: hunter2-confidential-detail\n",
    )
    .unwrap();
    assert!(
        relay
            .send_outbound_reply(
                &router,
                "slack",
                "C-MAPPED",
                "Here is hunter2-confidential-detail",
                true,
                false,
            )
            .await
            .is_err()
    );
    assert!(
        relay
            .send_outbound_reply(
                &router,
                "slack",
                "C-MAPPED",
                "Unsolicited ping",
                false,
                false
            )
            .await
            .is_err()
    );
    let sent = relay
        .send_outbound_reply(
            &router,
            "slack",
            "C-MAPPED",
            "Alerts are clear.",
            true,
            false,
        )
        .await
        .unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent_replies.lock().unwrap().len(), 1);

    // 2. ConnectorStore capability probing (never claims connected for broken or unconfigured)
    let conn_store = ConnectorStore::open_with_backend(
        dir.path().join("connectors.sqlite"),
        Some(Arc::new(InMemorySecretBackend::default())),
    )
    .unwrap();
    assert_eq!(
        conn_store
            .probe_integration_capability(&cfg, "gmail")
            .await
            .status,
        "setup_required"
    );
    conn_store
        .upsert_connector(CustomConnectorInput {
            id: Some("gmail-ok".into()),
            name: "Gmail Probe".into(),
            transport: ConnectorTransport::RemoteHttps,
            url_or_command: format!("{base}/mcp/health"),
            args: vec![],
            auth_header_name: None,
            auth_secret: Some("tok".into()),
            account_label: "darius@example.com".into(),
            enabled: true,
            bot_ids: vec![],
        })
        .unwrap();
    assert_eq!(
        conn_store
            .probe_integration_capability(&cfg, "gmail-ok")
            .await
            .status,
        "connected"
    );
    conn_store
        .upsert_connector(CustomConnectorInput {
            id: Some("broken-cal".into()),
            name: "Broken Cal".into(),
            transport: ConnectorTransport::RemoteHttps,
            url_or_command: format!("{base}/mcp/fail"),
            args: vec![],
            auth_header_name: None,
            auth_secret: None,
            account_label: "".into(),
            enabled: true,
            bot_ids: vec![],
        })
        .unwrap();
    let broken = conn_store
        .probe_integration_capability(&cfg, "broken-cal")
        .await;
    assert_eq!(broken.status, "degraded");
    assert!(!broken.reachable);

    // 3. Local voice STT/TTS + consent + barge-in + handoff + end
    let voice_engine = LocalVoiceEngine::new(VoiceConfig {
        stt_mode: SttMode::LocalWhisperHttp,
        tts_mode: TtsMode::LocalHttp,
        stt_endpoint: format!("{base}/voice/inference"),
        tts_endpoint: format!("{base}/voice/tts"),
        ..Default::default()
    })
    .unwrap();
    let mut vsess = VoiceSession::start("muse");
    assert!(
        voice_engine
            .transcribe_wav(&mut vsess, b"RIFF")
            .await
            .is_err()
    );
    vsess.acknowledge_consent();
    assert_eq!(
        voice_engine
            .transcribe_wav(&mut vsess, b"RIFF")
            .await
            .unwrap(),
        "hello world"
    );
    assert_eq!(
        voice_engine
            .synthesize_speech(&mut vsess, "Hi there")
            .await
            .unwrap(),
        vec![1, 2, 3, 4]
    );
    vsess.request_barge_in();
    assert!(
        voice_engine
            .synthesize_speech(&mut vsess, "Should yield")
            .await
            .unwrap()
            .is_empty()
    );
    vsess.handoff_to_human();
    assert_eq!(vsess.state, VoiceSessionState::HandedOff);
    vsess.end();
    assert_eq!(vsess.state, VoiceSessionState::Ended);

    // 4. Asterisk ARI SIP Trunk: Tier-3 carrier cost block -> consent -> live WS transcript -> handoff -> recover -> hangup
    let sip = AsteriskSipClient::new(AsteriskAriConfig {
        ari_base_url: format!("{base}/ari"),
        ws_events_url: ari_ws_url,
        app_name: "grok-muse".into(),
        trunk_endpoint: "PJSIP/twilio".into(),
        username: "ari".into(),
        password: "secret".into(),
    })
    .unwrap();
    assert!(
        sip.originate_call("muse", "+15551234567", false)
            .await
            .is_err()
    );
    let mut sip_sess = sip
        .originate_call("muse", "+15551234567", true)
        .await
        .unwrap();
    assert_eq!(sip_sess.external_media_channel_id.as_deref(), Some("ext-1"));
    sip.announce_consent(&mut sip_sess, "recording notice")
        .await
        .unwrap();
    assert!(sip_sess.consent_announced);
    assert_eq!(
        sip.stream_ari_transcript_events(&mut sip_sess, 2)
            .await
            .unwrap(),
        1
    );
    sip.transfer_to_human(&mut sip_sess, "PJSIP/100")
        .await
        .unwrap();
    assert_eq!(sip_sess.state, VoiceSessionState::HandedOff);
    sip.reconnect_session(&mut sip_sess).unwrap();
    assert_eq!(sip_sess.reconnect_attempts, 1);
    sip.hangup_and_recover(&mut sip_sess).await.unwrap();
    assert_eq!(sip_sess.state, VoiceSessionState::Ended);
    assert!(sip_sess.external_media_channel_id.is_none());

    // 5. Zoom RTMS: HMAC signature + consent gate + WS transcript + handoff + recover + leave
    let zoom = ZoomRtmsClient::new(ZoomRtmsConfig {
        rest_base_url: format!("{base}/zoom"),
        client_id: "zm-id".into(),
        client_secret: "zm-secret".into(),
    })
    .unwrap();
    let raw = r#"{"event":"meeting.rtms_started"}"#;
    let sig = format!(
        "v0={}",
        gm_openbot::compute_hmac_sha256_hex(
            b"zm-secret",
            format!("v0:1700000000:{raw}").as_bytes()
        )
    );
    assert!(zoom.verify_webhook_signature("1700000000", raw, &sig));
    assert!(!zoom.verify_webhook_signature("1700000000", raw, "v0=bad"));
    assert!(
        zoom.join_stream("muse", "mtg-1", "stream-1", &zoom_ws_url, false)
            .await
            .is_err()
    );
    let mut zm_sess = zoom
        .join_stream("muse", "mtg-1", "stream-1", &zoom_ws_url, true)
        .await
        .unwrap();
    assert_eq!(zoom.ingest_ws_events(&mut zm_sess, 5).await.unwrap(), 1);
    assert_eq!(zm_sess.participants, vec!["Darius".to_string()]);
    zoom.handoff_host_control(&mut zm_sess);
    zoom.recover_reconnect(&mut zm_sess).unwrap();
    let summary = zoom.leave_and_summarize(&mut zm_sess).await.unwrap();
    assert!(summary.contains("ship v0.6.0 today"));
    assert_eq!(zm_sess.state, VoiceSessionState::Ended);

    // 6. Google Meet REST/Events: consent gate + join + transcript poll + handoff + recover + end
    let meet = GoogleMeetClient::new(GoogleMeetConfig {
        meet_api_base: format!("{base}/meet"),
        oauth_access_token: "ya29.test".into(),
    })
    .unwrap();
    assert!(
        meet.join_conference("muse", "abc-defg-hij", false)
            .await
            .is_err()
    );
    let mut gm_sess = meet
        .join_conference("muse", "abc-defg-hij", true)
        .await
        .unwrap();
    assert_eq!(meet.poll_transcript_entries(&mut gm_sess).await.unwrap(), 1);
    assert_eq!(meet.poll_transcript_entries(&mut gm_sess).await.unwrap(), 0);
    meet.handoff_to_host(&mut gm_sess);
    meet.recover_subscription(&mut gm_sess).unwrap();
    meet.end_session(&mut gm_sess).await.unwrap();
    assert_eq!(gm_sess.state, VoiceSessionState::Ended);

    srv.abort();
}
