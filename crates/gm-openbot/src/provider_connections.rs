//! Shared, revision-checked provider setup used by desktop IPC and the companion.
use crate::OpenBotSupervisor;
use gm_core::config::{GmConfig, ProviderConnectionState, ProviderPreset, ProviderProfileConfig};
use gm_core::{GmError, Result};
use gm_llm::{BudgetManager, ChatMessage, HybridLlmClient, LlmRouter, ReqwestLlmBackend, TaskType};
use parking_lot::{Mutex, RwLock};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::watch;

type ProviderPersistence = (Arc<RwLock<GmConfig>>, Option<PathBuf>);

#[derive(Default)]
pub(crate) struct ProviderConnections {
    attempts: Mutex<HashMap<String, (String, watch::Sender<bool>)>>,
    persistence: Mutex<Option<ProviderPersistence>>,
}

impl OpenBotSupervisor {
    pub fn bind_provider_persistence(&self, config: &Arc<RwLock<GmConfig>>, path: Option<&Path>) {
        *self.connections.persistence.lock() = Some((config.clone(), path.map(Path::to_path_buf)));
    }

    pub(crate) fn persist_connection(&self) -> Result<()> {
        let target = self.connections.persistence.lock().clone();
        if let Some((config, path)) = target {
            crate::persist_providers_config(&config, path.as_deref(), self)?;
        }
        let _ = self
            .daemon_events_tx
            .send(gm_core::ipc::DaemonEvent::ActivityUpdated {
                reason: "providers_updated".into(),
            });
        Ok(())
    }

    fn profile_snapshot(&self, profile_id: &str) -> Result<ProviderProfileConfig> {
        let profile = self
            .providers_config()
            .profile_by_id(profile_id)
            .cloned()
            .ok_or_else(|| GmError::Config(format!("Unknown provider profile '{profile_id}'")))?;
        gm_core::config::validate_oauth_destination(
            profile.preset,
            profile.auth_mode,
            profile.protocol,
            &profile.base_url,
        )?;
        Ok(profile)
    }

    fn update_connection(
        &self,
        profile: &ProviderProfileConfig,
        update: impl FnOnce(&mut ProviderProfileConfig),
    ) -> Result<ProviderProfileConfig> {
        let mut config = self.providers_override.write();
        let current = config
            .profile_by_id_mut(&profile.id)
            .ok_or_else(|| GmError::Config("Provider was removed during this operation".into()))?;
        if current.revision != profile.revision {
            return Err(GmError::Config(
                "Provider changed during this operation. Retry with the current settings.".into(),
            ));
        }
        update(current);
        Ok(current.clone())
    }

    pub fn cancel_provider_sign_in(&self, profile_id: &str) {
        if let Some((_, cancel)) = self.connections.attempts.lock().remove(profile_id) {
            let _ = cancel.send(true);
        }
    }

    pub async fn discover_provider_models(&self, profile_id: &str) -> Result<Vec<String>> {
        let profile = self.profile_snapshot(profile_id)?;
        if profile.connection_state == ProviderConnectionState::NeedsRevalidation
            || (profile.credential_ref.is_some()
                && profile.validated_origin.as_deref()
                    != Some(gm_core::config::extract_provider_origin(&profile.base_url)?.as_str()))
        {
            return Err(GmError::SafetyViolation(
                "Revalidate this provider destination before sending credentials".into(),
            ));
        }
        self.update_connection(&profile, |value| {
            value.health.catalog_status = "loading".into()
        })?;
        let result = async {
            let bearer = if matches!(
                profile.preset,
                ProviderPreset::KiloGateway | ProviderPreset::OpenRouter
            ) && profile
                .credential_ref
                .as_deref()
                .and_then(|r| r.strip_prefix("secret://keychain/"))
                .is_none_or(|a| self.vault().backend().get_secret(a).is_err())
            {
                None
            } else {
                self.provider_bearer(&profile).await?
            };
            ReqwestLlmBackend::with_keys(None, None)
                .discover_model_catalog(
                    &self.profile_snapshot(profile_id)?.base_url,
                    bearer.as_deref(),
                )
                .await
        }
        .await;
        match result {
            Ok(catalog) => {
                let ids: Vec<String> = catalog.iter().map(|model| model.id.clone()).collect();
                self.update_connection(&profile, |value| {
                    value.model_catalog = catalog;
                    value.discovered_models = ids.clone();
                    value.health.catalog_status = if ids.is_empty() { "empty" } else { "ready" }.into();
                    value.health.models_refreshed_at = Some(chrono::Utc::now().to_rfc3339());
                    if !value.selected_model.is_empty() && !ids.contains(&value.selected_model) {
                        value.health.verification_status = "not_tested".into();
                        value.health.verified_model=None;value.health.verified_at=None;
                        if value.connection_state==ProviderConnectionState::Connected{value.connection_state=ProviderConnectionState::Authenticated;}
                        value.last_error = Some("The selected model is absent from the current catalog. Choose a model or verify a custom ID.".into());
                    } else { value.last_error = None; }
                })?;
                self.persist_connection()?;
                Ok(ids)
            }
            Err(error) => {
                self.update_connection(&profile, |value| {
                    value.health.catalog_status = if value.discovered_models.is_empty() {
                        "error"
                    } else {
                        "stale"
                    }
                    .into();
                    value.last_error = Some(error.to_string());
                })?;
                self.persist_connection()?;
                Err(error)
            }
        }
    }

    async fn provider_bearer(&self, profile: &ProviderProfileConfig) -> Result<Option<String>> {
        use gm_core::config::ProviderAuthMode;
        if profile.auth_mode == ProviderAuthMode::None {
            return Ok(None);
        }
        let Some(reference) = &profile.credential_ref else {
            return Err(GmError::Config(
                "Connect this provider before requesting account models".into(),
            ));
        };
        let account = reference
            .strip_prefix("secret://keychain/")
            .ok_or_else(|| GmError::Config("Provider credential reference is invalid".into()))?;
        let raw = self.vault().backend().get_secret(account)?;
        if matches!(
            profile.auth_mode,
            ProviderAuthMode::ChatGptOAuth | ProviderAuthMode::NousPortalOAuth
        ) {
            let mut bundle: gm_llm::OAuthTokenBundle = serde_json::from_str(&raw)
                .map_err(|_| GmError::Config("Provider credentials need a fresh sign-in".into()))?;
            if profile.preset == ProviderPreset::ChatGpt {
                if let Some(identity) = &bundle.identity {
                    if !identity
                        .scopes
                        .iter()
                        .any(|scope| scope == "chatgpt.tokens.use.direct")
                    {
                        return Err(GmError::Provider("Signed in to ChatGPT. Enable ChatGPT plan usage to load and use models.".into()));
                    }
                } else {
                    return Err(GmError::Provider(
                        "This legacy ChatGPT connection needs a fresh sign-in with DT.".into(),
                    ));
                }
            }
            if bundle.is_expiring_soon() {
                bundle = gm_llm::refresh_oauth_bundle(
                    &provider_http_client()?,
                    &gm_llm::KeychainSecrets::default(),
                    profile.preset,
                    reference,
                )
                .await?;
            }
            if profile.preset == ProviderPreset::NousPortal
                && let Some(base) = &bundle.inference_base_url
            {
                gm_core::config::validate_oauth_destination(
                    profile.preset,
                    profile.auth_mode,
                    profile.protocol,
                    base,
                )?;
                if base != &profile.base_url {
                    self.update_connection(profile, |p| {
                        p.base_url = base.clone();
                        p.validated_origin = gm_core::config::extract_provider_origin(base).ok();
                    })?;
                    self.persist_connection()?;
                }
            }
            Ok(Some(bundle.access_token))
        } else {
            Ok(Some(raw))
        }
    }

    pub async fn verify_provider_model(&self, profile_id: &str) -> Result<Value> {
        let profile = self.profile_snapshot(profile_id)?;
        if profile.selected_model.trim().is_empty() {
            return Err(GmError::Config("Select a model to verify".into()));
        }
        self.update_connection(&profile, |value| {
            value.health.verification_status = "testing".into();
            value.last_error = None;
        })?;
        let result = async {
            let bearer = self.provider_bearer(&profile).await?;
            let router = LlmRouter::new(self.providers_config())
                .with_preference(Some(profile_id), Some(&profile.selected_model))?;
            let (target, _) = router.route(TaskType::Planning);
            let backend = Arc::new(ReqwestLlmBackend::for_provider(&target, bearer));
            let budget = Arc::new(BudgetManager::open(
                self.connections
                    .persistence
                    .lock()
                    .as_ref()
                    .map(|(config, _)| config.read().budget.clone())
                    .unwrap_or_else(|| self.config.budget.clone()),
                &self.config.storage_dir,
            )?);
            let client = HybridLlmClient::new(backend, budget);
            let (sender, mut receiver) = tokio::sync::mpsc::channel(16);
            let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
            let response = client
                .complete_stream_attributed(
                    &target,
                    None,
                    &[ChatMessage::text("user", "Reply with OK.")],
                    512,
                    0.0,
                    sender,
                    "provider-check",
                    "setup",
                )
                .await;
            let _ = drain.await;
            response.and_then(|value| {
                if value.content.trim().is_empty() {
                    Err(GmError::Provider(
                        "Model completed without a text reply".into(),
                    ))
                } else {
                    Ok(value)
                }
            })
        }
        .await;
        match result {
            Ok(response) => {
                let saved = self.update_connection(&profile, |value| {
                    value.connection_state = ProviderConnectionState::Connected;
                    value.health.verification_status = "verified".into();
                    value.health.verified_model = Some(profile.selected_model.clone());
                    value.health.verified_at = Some(chrono::Utc::now().to_rfc3339());
                    value.last_error = None;
                })?;
                {
                    let mut config = self.providers_override.write();
                    if config.workspace_default_profile_id.is_none()
                        && config.account_default_profile_id.is_none()
                    {
                        config.workspace_default_profile_id = Some(profile.id.clone());
                    }
                }
                self.persist_connection()?;
                Ok(
                    json!({"profile":saved,"providers":self.providers_config(),"reply":response.content,"verified":true}),
                )
            }
            Err(error) => {
                let saved = self.update_connection(&profile, |value| {
                    value.connection_state = ProviderConnectionState::Error;
                    value.health.verification_status = "error".into();
                    value.last_error = Some(error.to_string());
                })?;
                self.persist_connection()?;
                Ok(
                    json!({"profile":saved,"providers":self.providers_config(),"verified":false,"error":error.to_string()}),
                )
            }
        }
    }

    fn save_connected_bundle(
        &self,
        profile: &ProviderProfileConfig,
        bundle: gm_llm::OAuthTokenBundle,
    ) -> Result<()> {
        let account = gm_llm::keychain_account_for_oauth(profile.preset, &bundle.account_id)?;
        let mut config = self.providers_override.write();
        let current = config
            .profile_by_id_mut(&profile.id)
            .ok_or_else(|| GmError::Config("Provider was removed".into()))?;
        if current.revision != profile.revision {
            return Err(GmError::Config(
                "Sign-in was cancelled or settings changed".into(),
            ));
        }
        self.scrubber().register(&account, &bundle.access_token);
        if let Some(refresh) = &bundle.refresh_token {
            self.scrubber()
                .register(format!("{account}-refresh"), refresh);
        }
        self.vault()
            .backend()
            .set_secret(&account, &serde_json::to_string(&bundle)?)?;
        current.credential_ref = Some(format!("secret://keychain/{account}"));
        current.account_label = Some(bundle.account_label);
        current.account_id = Some(bundle.account_id);
        current.connection_state = ProviderConnectionState::Authenticated;
        current.last_error = None;
        current.health = Default::default();
        drop(config);
        self.persist_connection()
    }

    pub async fn oauth_action_for_profile(
        &self,
        profile_id: &str,
        op: &str,
        payload: &Value,
    ) -> Result<Value> {
        let op = if op == "switch_account" {
            self.perform_oauth_action(profile_id, "disconnect", &json!({}))
                .await?;
            "start"
        } else {
            op
        };
        let result = self.perform_oauth_action(profile_id, op, payload).await;
        if op == "start" && result.is_err() {
            self.cancel_provider_sign_in(profile_id);
            if let Ok(profile) = self.profile_snapshot(profile_id)
                && profile.connection_state == ProviderConnectionState::Connecting
            {
                self.fail_sign_in(
                    &profile,
                    "Sign-in could not start. Check the connection and try again.".into(),
                );
            }
        }
        result
    }

    async fn perform_oauth_action(
        &self,
        profile_id: &str,
        op: &str,
        payload: &Value,
    ) -> Result<Value> {
        match op {
            "configure" => {
                let profile: ProviderProfileConfig =
                    serde_json::from_value(payload["profile"].clone())?;
                if profile.id != profile_id {
                    return Err(GmError::Config("Provider ID does not match".into()));
                }
                let saved = self.upsert_provider_profile(
                    profile,
                    payload["api_key"].as_str(),
                    payload["revalidate_credentials"].as_bool().unwrap_or(false),
                )?;
                self.persist_connection()?;
                let _ = self.discover_provider_models(profile_id).await;
                if !saved.selected_model.trim().is_empty() {
                    return self.verify_provider_model(profile_id).await;
                }
                return Ok(
                    json!({"providers":self.providers_config(),"profile":self.profile_snapshot(profile_id)?}),
                );
            }
            "select_model" => {
                let mut profile = if payload["profile"].is_object() {
                    serde_json::from_value(payload["profile"].clone())?
                } else {
                    self.profile_snapshot(profile_id)?
                };
                if profile.id != profile_id {
                    return Err(GmError::Config("Provider ID does not match".into()));
                }
                let model = payload["model"].as_str().unwrap_or_default().trim();
                if model.is_empty() {
                    return Err(GmError::Config("Select a model".into()));
                }
                profile.selected_model = model.into();
                self.upsert_provider_profile(profile, None, false)?;
                self.persist_connection()?;
                return self.verify_provider_model(profile_id).await;
            }
            "refresh" => {
                self.discover_provider_models(profile_id).await?;
                return Ok(
                    json!({"providers":self.providers_config(),"profile":self.profile_snapshot(profile_id)?}),
                );
            }
            "make_default" => {
                let profile = self.profile_snapshot(profile_id)?;
                if profile.health.verification_status != "verified"
                    || profile.health.verified_model.as_deref()
                        != Some(profile.selected_model.as_str())
                {
                    return Err(GmError::Config(
                        "Verify this model before making it the default".into(),
                    ));
                }
                self.set_default_provider_profiles(Some(profile_id.into()), None)?;
                self.persist_connection()?;
                return Ok(json!({"providers":self.providers_config()}));
            }
            "status" => {
                return Ok(
                    json!({"profile":self.profile_snapshot(profile_id)?,"providers":self.providers_config()}),
                );
            }
            "verify" => return self.verify_provider_model(profile_id).await,
            "disconnect" | "cancel" => {
                self.cancel_provider_sign_in(profile_id);
                let mut config = self.providers_override.write();
                let profile = config
                    .profile_by_id_mut(profile_id)
                    .ok_or_else(|| GmError::Config("Unknown provider".into()))?;
                if op == "disconnect" {
                    if let Some(account) = profile
                        .credential_ref
                        .as_deref()
                        .and_then(|r| r.strip_prefix("secret://keychain/"))
                    {
                        self.vault().backend().delete_secret(account)?;
                    }
                    profile.credential_ref = None;
                    profile.account_id = None;
                    profile.account_label = None;
                }
                profile.revision = profile.revision.saturating_add(1);
                profile.health = Default::default();
                if op == "disconnect" {
                    profile.model_catalog.clear();
                    profile.discovered_models.clear();
                }
                profile.connection_state = if profile.credential_ref.is_some() {
                    ProviderConnectionState::CredentialsSaved
                } else {
                    ProviderConnectionState::Unconfigured
                };
                let saved = profile.clone();
                drop(config);
                self.persist_connection()?;
                return Ok(
                    json!({"profile":saved,"providers":self.providers_config(),"connected":false}),
                );
            }
            "start" => {}
            _ => return Err(GmError::Config("Unknown provider connection action".into())),
        }
        self.cancel_provider_sign_in(profile_id);
        let original = self.profile_snapshot(profile_id)?;
        if !matches!(
            original.preset,
            ProviderPreset::ChatGpt | ProviderPreset::NousPortal
        ) {
            return Err(GmError::Config(
                "This provider uses API-key or local authentication".into(),
            ));
        }
        let mut profile = self.update_connection(&original, |value| {
            value.revision = value.revision.saturating_add(1);
            value.connection_state = ProviderConnectionState::Connecting;
            value.health = Default::default();
            value.last_error = None;
        })?;
        // The start operation owns the incremented generation until it completes or is cancelled.
        profile.health = Default::default();
        let (cancel, mut cancelled) = watch::channel(false);
        let attempt_id = uuid::Uuid::new_v4().to_string();
        self.connections
            .attempts
            .lock()
            .insert(profile_id.into(), (attempt_id.clone(), cancel));
        if profile.preset == ProviderPreset::NousPortal {
            return self
                .start_nous_sign_in(profile, attempt_id, cancelled)
                .await;
        }
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let redirect_uri = format!(
            "http://127.0.0.1:{}/auth/callback",
            listener.local_addr()?.port()
        );
        let host_path = self.config.storage_dir.join("provider-host-id");
        let host_id = match std::fs::read_to_string(&host_path) {
            Ok(value) if !value.trim().is_empty() => value,
            _ => {
                let value = format!("urn:uuid:{}", uuid::Uuid::new_v4());
                gm_core::storage::atomic_write_private(&host_path, value.as_bytes())?;
                value
            }
        };
        let saved = profile
            .credential_ref
            .as_deref()
            .and_then(|reference| reference.strip_prefix("secret://keychain/"))
            .and_then(|account| self.vault().backend().get_secret(account).ok())
            .and_then(|raw| serde_json::from_str::<gm_llm::OAuthTokenBundle>(&raw).ok())
            .and_then(|bundle| bundle.identity);
        let attempt = Arc::new(gm_llm::begin_chatgpt_sign_in(
            &host_id,
            saved.as_ref(),
            &redirect_uri,
        )?);
        let authorize_url = attempt.request.authorize_url.clone();
        let supervisor = self.clone();
        let callback_profile = profile.clone();
        let (completed, mut completion) = watch::channel(false);
        let consumed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let callback = axum::Router::new().route("/auth/callback", axum::routing::get(move |axum::extract::Query(query): axum::extract::Query<HashMap<String,String>>| {
            let supervisor = supervisor.clone(); let attempt = attempt.clone(); let profile = callback_profile.clone(); let completed = completed.clone(); let consumed = consumed.clone();
            async move {
                use axum::{http::StatusCode, response::Html};
                if query.get("state") != Some(&attempt.request.state) || consumed.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    return (StatusCode::BAD_REQUEST, Html("This sign-in callback is invalid or already used."));
                }
                let result = async {
                    if query.contains_key("error") { return Err(GmError::Provider("Sign-in was declined. You can try again in DT.".into())); }
                    let code = query.get("code").ok_or_else(|| GmError::Provider("Missing authorization code".into()))?;
                    let issued = query.get("client_id").map(String::as_str).unwrap_or(&attempt.client_id);
                    let bundle = gm_llm::complete_chatgpt_sign_in(&provider_http_client()?, &attempt, issued, code).await?;
                    supervisor.save_connected_bundle(&profile, bundle)?;
                    // Discovery is account-specific; verification follows the user's model selection.
                    if let Ok(models)=supervisor.discover_provider_models(&profile.id).await&& let Ok(mut current)=supervisor.profile_snapshot(&profile.id){if !models.contains(&current.selected_model)&& let Some(first)=models.first(){current.selected_model=first.clone();let _=supervisor.upsert_provider_profile(current,None,false);}let _=supervisor.verify_provider_model(&profile.id).await;}
                    Ok::<_,GmError>(())
                }.await;
                if let Err(error) = &result { supervisor.fail_sign_in(&profile, error.to_string()); }
                let _ = completed.send(true);
                if result.is_ok() { (StatusCode::OK, Html("<title>DT</title><h1>Signed in</h1><p>Return to DT to choose and verify a model.</p>")) }
                else { (StatusCode::BAD_REQUEST, Html("<title>DT</title><h1>Sign-in could not finish</h1><p>Return to DT for details and retry.</p>")) }
            }
        }));
        let owner = self.clone();
        let timeout_profile = profile.clone();
        tokio::spawn(async move {
            let server = axum::serve(listener, callback).with_graceful_shutdown(async move {
                tokio::select! { _ = cancelled.changed() => {}, _ = completion.changed() => {}, _ = tokio::time::sleep(std::time::Duration::from_secs(300)) => {} }
            });
            let _ = server.await;
            if let Ok(current) = owner.profile_snapshot(&timeout_profile.id)
                && current.revision == timeout_profile.revision
                && current.connection_state == ProviderConnectionState::Connecting
            {
                owner.fail_sign_in(
                    &timeout_profile,
                    "Sign-in timed out. Start a fresh sign-in.".into(),
                );
            }
        });
        self.persist_connection()?;
        Ok(
            json!({"attempt_id":attempt_id,"authorize_url":authorize_url,"profile":profile,"providers":self.providers_config()}),
        )
    }

    fn fail_sign_in(&self, profile: &ProviderProfileConfig, message: String) {
        let _ = self.update_connection(profile, |value| {
            value.connection_state = ProviderConnectionState::Error;
            value.last_error = Some(message);
        });
        let _ = self.persist_connection();
    }

    async fn start_nous_sign_in(
        &self,
        profile: ProviderProfileConfig,
        attempt_id: String,
        mut cancelled: watch::Receiver<bool>,
    ) -> Result<Value> {
        let client = provider_http_client()?;
        // Nous publishes this public device client in its reference implementation.
        let client_id = "hermes-cli";
        let response = client
            .post("https://portal.nousresearch.com/api/oauth/device/code")
            .form(&[("client_id", client_id), ("scope", "inference:invoke")])
            .send()
            .await
            .map_err(|_| GmError::Provider("Nous Portal could not be reached".into()))?;
        if !response.status().is_success() {
            self.fail_sign_in(
                &profile,
                format!("Nous sign-in returned HTTP {}", response.status()),
            );
            return Err(GmError::Provider(
                "Nous Portal could not start device sign-in. Retry shortly.".into(),
            ));
        }
        let device: Value = response
            .json()
            .await
            .map_err(|_| GmError::Provider("Invalid Nous device response".into()))?;
        let device_code = device["device_code"]
            .as_str()
            .ok_or_else(|| GmError::Provider("Nous did not return a device code".into()))?
            .to_owned();
        let authorize_url = device["verification_uri_complete"]
            .as_str()
            .or_else(|| device["verification_uri"].as_str())
            .ok_or_else(|| GmError::Provider("Nous did not return a sign-in URL".into()))?
            .to_owned();
        if gm_core::config::extract_provider_origin(&authorize_url)?
            != "https://portal.nousresearch.com"
        {
            return Err(GmError::SafetyViolation(
                "Nous returned an unexpected sign-in destination".into(),
            ));
        }
        let user_code = device["user_code"].as_str().unwrap_or_default().to_owned();
        let timeout = device["expires_in"].as_u64().unwrap_or(300).min(900);
        let interval = device["interval"].as_u64().unwrap_or(5).clamp(1, 30);
        let supervisor = self.clone();
        let poll_profile = profile.clone();
        tokio::spawn(async move {
            let poll = async {
                let mut interval = interval;
                loop {
                    tokio::select! { _ = cancelled.changed() => return Err(GmError::Provider("Sign-in cancelled".into())), _ = tokio::time::sleep(std::time::Duration::from_secs(interval)) => {} }
                    let response = client
                        .post("https://portal.nousresearch.com/api/oauth/token")
                        .form(&[
                            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                            ("client_id", client_id),
                            ("device_code", &device_code),
                        ])
                        .send()
                        .await
                        .map_err(|_| {
                            GmError::Provider("Nous token service is unavailable".into())
                        })?;
                    let success = response.status().is_success();
                    let mut value: Value = response
                        .json()
                        .await
                        .map_err(|_| GmError::Provider("Invalid Nous token response".into()))?;
                    if !success {
                        match value["error"].as_str().unwrap_or_default() {
                            "authorization_pending" => continue,
                            "slow_down" => {
                                interval = (interval + 5).min(30);
                                continue;
                            }
                            _ => {
                                return Err(GmError::Provider(
                                    "Nous sign-in was declined or expired. Try again.".into(),
                                ));
                            }
                        }
                    }
                    if let Some(refresh) = value["refresh_token"].as_str().map(str::to_owned) {
                        let renewal = provider_http_client()?
                            .post("https://portal.nousresearch.com/api/oauth/token")
                            .header("x-nous-refresh-token", &refresh)
                            .form(&[("grant_type", "refresh_token"), ("client_id", client_id)])
                            .send()
                            .await
                            .map_err(|_| {
                                GmError::Provider(
                                    "Nous inference grant could not be renewed".into(),
                                )
                            })?;
                        if !renewal.status().is_success() {
                            return Err(GmError::Provider(
                                "Nous could not grant inference access. Try signing in again."
                                    .into(),
                            ));
                        }
                        let mut renewed: Value = renewal.json().await.map_err(|_| {
                            GmError::Provider("Invalid Nous inference grant".into())
                        })?;
                        if renewed["refresh_token"].is_null() {
                            renewed["refresh_token"] = json!(refresh);
                        }
                        value = renewed;
                    }
                    let access_token = value["access_token"].as_str().ok_or_else(|| {
                        GmError::Provider("Nous did not return an access token".into())
                    })?;
                    let bundle = gm_llm::OAuthTokenBundle {
                        provider_id: "nous_portal".into(),
                        account_id: poll_profile.id.clone(),
                        account_label: "Nous Portal account".into(),
                        access_token: access_token.into(),
                        refresh_token: value["refresh_token"].as_str().map(str::to_owned),
                        expires_at: value["expires_in"].as_i64().map(|seconds| {
                            (chrono::Utc::now() + chrono::Duration::seconds(seconds)).to_rfc3339()
                        }),
                        token_endpoint: "https://portal.nousresearch.com/api/oauth/token".into(),
                        client_id: client_id.into(),
                        identity: None,
                        inference_base_url: value["inference_base_url"].as_str().map(str::to_owned),
                    };
                    supervisor.save_connected_bundle(&poll_profile, bundle)?;
                    if let Some(base) = value["inference_base_url"].as_str() {
                        if matches!(
                            base.trim_end_matches('/'),
                            "https://inference-api.nousresearch.com/v1"
                                | "https://welcome-api.nousresearch.com/v1"
                        ) {
                            supervisor.update_connection(&poll_profile, |p| {
                                p.base_url = base.trim_end_matches('/').into();
                                p.validated_origin =
                                    gm_core::config::extract_provider_origin(&p.base_url).ok();
                            })?;
                        } else {
                            return Err(GmError::Provider(
                                "Nous returned an unrecognized inference destination".into(),
                            ));
                        }
                    }
                    if let Ok(models) = supervisor.discover_provider_models(&poll_profile.id).await
                        && let Ok(mut current) = supervisor.profile_snapshot(&poll_profile.id)
                    {
                        if !models.contains(&current.selected_model)
                            && let Some(first) = models.first()
                        {
                            current.selected_model = first.clone();
                            let _ = supervisor.upsert_provider_profile(current, None, false);
                        }
                        let _ = supervisor.verify_provider_model(&poll_profile.id).await;
                    }
                    return Ok::<_, GmError>(());
                }
            };
            match tokio::time::timeout(std::time::Duration::from_secs(timeout), poll).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => supervisor.fail_sign_in(&poll_profile, error.to_string()),
                Err(_) => supervisor
                    .fail_sign_in(&poll_profile, "Nous sign-in timed out. Try again.".into()),
            }
        });
        self.persist_connection()?;
        Ok(
            json!({"attempt_id":attempt_id,"authorize_url":authorize_url,"user_code":user_code,"profile":profile,"providers":self.providers_config()}),
        )
    }
}

fn provider_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| GmError::Provider("Unable to initialize provider connection".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::{InMemorySecretBackend, SecretBackend};
    use gm_core::config::ProviderAuthMode;
    use gm_safety::AuditLogger;

    fn supervisor(dir: &Path, backend: Arc<dyn SecretBackend>) -> OpenBotSupervisor {
        let mut config = GmConfig {
            storage_dir: dir.to_path_buf(),
            ..Default::default()
        };
        config.openbot.bots_dir = dir.join("bots");
        let (events, _) = tokio::sync::broadcast::channel(16);
        OpenBotSupervisor::open_with_secret_backend(
            config,
            Arc::new(AuditLogger::in_memory()),
            events,
            backend,
        )
        .unwrap()
    }
    #[tokio::test]
    async fn catalog_is_not_verification_and_stale_work_cannot_revive_a_disconnect() {
        use axum::{
            Json, Router,
            routing::{get, post},
        };
        let app=Router::new().route("/v1/models",get(||async{Json(json!({"data":[{"id":"working","name":"Working model","context_length":8192},{"id":"denied"}]}))})).route("/v1/chat/completions",post(|Json(body):Json<Value>|async move{
            if body["model"]=="denied"{return (axum::http::StatusCode::FORBIDDEN,[("content-type","text/plain")],"denied".to_string());}
            (axum::http::StatusCode::OK,[("content-type","text/event-stream")],"data: {\"choices\":[{\"delta\":{\"content\":\"OK\"}}]}\n\ndata: [DONE]\n\n".to_string())
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let sup = supervisor(dir.path(), Arc::new(InMemorySecretBackend::default()));
        let profile = ProviderProfileConfig {
            id: "fixture".into(),
            base_url: format!("http://{address}/v1"),
            selected_model: "working".into(),
            auth_mode: ProviderAuthMode::None,
            ..Default::default()
        };
        sup.upsert_provider_profile(profile, None, false).unwrap();
        assert_eq!(
            sup.discover_provider_models("fixture").await.unwrap().len(),
            2
        );
        let snapshot = sup.profile_snapshot("fixture").unwrap();
        assert_ne!(
            snapshot.connection_state,
            ProviderConnectionState::Connected
        );
        assert_eq!(snapshot.health.verification_status, "not_tested");
        assert_eq!(
            sup.verify_provider_model("fixture").await.unwrap()["verified"],
            true
        );
        let denied = sup
            .oauth_action_for_profile("fixture", "select_model", &json!({"model":"denied"}))
            .await
            .unwrap();
        assert_eq!(denied["verified"], false);
        assert_ne!(denied["profile"]["connection_state"], "connected");
        let old = sup.profile_snapshot("fixture").unwrap();
        sup.oauth_action_for_profile("fixture", "disconnect", &json!({}))
            .await
            .unwrap();
        assert!(
            sup.update_connection(&old, |p| p.connection_state =
                ProviderConnectionState::Connected)
                .is_err()
        );
        server.abort();
    }
    struct BrokenSecrets;
    impl SecretBackend for BrokenSecrets {
        fn get_secret(&self, _: &str) -> Result<String> {
            Err(GmError::Config("locked".into()))
        }
        fn set_secret(&self, _: &str, _: &str) -> Result<()> {
            Err(GmError::Config("locked".into()))
        }
        fn delete_secret(&self, _: &str) -> Result<()> {
            Err(GmError::Config("locked".into()))
        }
    }
    #[test]
    fn keychain_write_failure_cannot_mark_a_profile_authenticated() {
        let dir = tempfile::tempdir().unwrap();
        let sup = supervisor(dir.path(), Arc::new(BrokenSecrets));
        let before = sup.profile_snapshot("xai-default").unwrap();
        assert!(
            sup.upsert_provider_profile(before.clone(), Some("fixture-secret"), true)
                .is_err()
        );
        assert_eq!(sup.profile_snapshot("xai-default").unwrap(), before);
        let mut invalid = before.clone();
        invalid.base_url = "http://untrusted.example/v1".into();
        assert!(
            sup.upsert_provider_profile(invalid, Some("fixture-secret"), true)
                .is_err()
        );
        assert_eq!(sup.profile_snapshot("xai-default").unwrap(), before);
    }

    #[test]
    fn test_provider_states_and_fail_in_place() {
        keychain_write_failure_cannot_mark_a_profile_authenticated();
    }
}
