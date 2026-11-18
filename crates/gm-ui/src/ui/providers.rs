//! Provider setup uses the same daemon-owned connection actions as the companion.
use crate::ui::{state::GrokUiState, theme::GrokPalette};
use eframe::egui::{self, RichText};
use gm_core::{
    config::{
        GmConfig, ProviderAuthMode, ProviderConnectionState, ProviderProfileConfig, WireProtocol,
    },
    ipc::DaemonRequest,
};
use serde_json::{Value, json};

pub fn draw(
    ui: &mut egui::Ui,
    state: &mut GrokUiState,
    palette: &GrokPalette,
    config: &mut GmConfig,
    send: &mut impl FnMut(DaemonRequest),
) {
    ui.heading("Providers & models");
    ui.add_space(6.0);
    ui.label(
        RichText::new("Connect an account, load its models, then test a real reply.")
            .color(palette.text_secondary),
    );
    ui.small("Setup and model changes send a small test request. API credits or ChatGPT allowance may be used.");
    ui.add_space(18.0);
    // Add newly supported presets without changing an existing profile's credentials or selection.
    for profile in gm_core::config::default_provider_profiles() {
        if !config.providers.profiles.iter().any(|p| p.id == profile.id) {
            config.providers.profiles.push(profile);
        }
    }
    if !config
        .providers
        .profiles
        .iter()
        .any(|p| p.id == state.provider_selected_id)
    {
        state.provider_selected_id = "chatgpt-default".into();
    }
    ui.horizontal_wrapped(|ui| {
        for profile in &config.providers.profiles {
            if ui
                .selectable_label(
                    state.provider_selected_id == profile.id,
                    &profile.display_name,
                )
                .clicked()
            {
                state.provider_selected_id = profile.id.clone();
                state.provider_api_key_draft.clear();
                state.provider_revalidate_check = false;
            }
        }
        if ui.button("+ Custom").clicked() {
            let id = format!("custom-{}", uuid::Uuid::new_v4().simple());
            let p = ProviderProfileConfig {
                id: id.clone(),
                display_name: "Custom provider".into(),
                ..Default::default()
            };
            config.providers.profiles.push(p);
            state.provider_selected_id = id;
        }
    });
    ui.add_space(18.0);
    let Some(profile) = config
        .providers
        .profiles
        .iter_mut()
        .find(|p| p.id == state.provider_selected_id)
    else {
        return;
    };
    let id = profile.id.clone();
    let action = |op: &str, payload: Value| DaemonRequest::ProviderOAuthAction {
        profile_id: id.clone(),
        op: op.into(),
        payload,
    };
    egui::Frame::NONE.fill(palette.bg_canvas).corner_radius(16).inner_margin(20).show(ui,|ui|{
        ui.heading(&profile.display_name);
        if let Some(account)=&profile.account_label{ui.label(account);}
        let auth=match profile.connection_state{ProviderConnectionState::Connecting=>"Waiting for sign-in",ProviderConnectionState::CredentialsSaved=>"Credentials saved; awaiting verification",ProviderConnectionState::Connected|ProviderConnectionState::Authenticated=>"Authenticated",ProviderConnectionState::NeedsRevalidation=>"Destination approval required",ProviderConnectionState::Error=>"Connection needs attention",_=>if profile.auth_mode==ProviderAuthMode::None{"Local / no authentication"}else{"Sign in or enter an API key"}};
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui|{
            ui.label(format!("Account: {auth}"));
            ui.label(format!("Models: {}",profile.health.catalog_status.replace('_'," ")));
            ui.colored_label(if profile.health.verification_status=="verified"{palette.status_ok}else{palette.text_secondary},format!("Reply test: {}",profile.health.verification_status.replace('_'," ")));
        });
        if let Some(error)=&profile.last_error{ui.add_space(8.0);ui.colored_label(palette.status_warn,error);}
        ui.add_space(14.0);
        if matches!(profile.auth_mode,ProviderAuthMode::ChatGptOAuth|ProviderAuthMode::NousPortalOAuth){
            ui.horizontal_wrapped(|ui|{
                if profile.connection_state==ProviderConnectionState::Connecting{
                    if ui.button("Cancel sign-in").clicked(){send(action("cancel",json!({})));}
                }else if ui.button(if profile.auth_mode==ProviderAuthMode::ChatGptOAuth{"Continue with ChatGPT"}else{"Continue with Nous Portal"}).clicked(){send(action("start",json!({})));}
                if profile.credential_ref.is_some()&&ui.button("Disconnect").clicked(){send(action("disconnect",json!({})));}
            });
            if profile.auth_mode==ProviderAuthMode::ChatGptOAuth{ui.small("Uses your ChatGPT plan when your account permits it. OpenAI API keys are a separate provider.");ui.hyperlink_to("ChatGPT usage","https://chatgpt.com/settings/usage");}
            if let Some(url)=state.responses["provider_oauth"]["authorize_url"].as_str().map(str::to_string){
                let attempt=state.responses["provider_oauth"]["attempt_id"].as_str().unwrap_or_default().to_string();
                if !attempt.is_empty()&&state.provider_opened_attempt!=attempt{ui.ctx().open_url(egui::OpenUrl::new_tab(&url));state.provider_opened_attempt=attempt;}
                if profile.connection_state==ProviderConnectionState::Connecting{ui.hyperlink_to("Open sign-in page",url);if let Some(code)=state.responses["provider_oauth"]["user_code"].as_str(){ui.monospace(format!("Device code: {code}"));}}
            }
        }else{
            if profile.auth_mode==ProviderAuthMode::ApiKey{
                ui.label("API key");
                ui.add(egui::TextEdit::singleline(&mut *state.provider_api_key_draft).password(true).hint_text(if matches!(profile.connection_state,ProviderConnectionState::Connected|ProviderConnectionState::Authenticated|ProviderConnectionState::CredentialsSaved){"Saved securely. Enter a replacement key…"}else{"Paste your provider API key…"}).desired_width(f32::INFINITY));
            }
            if profile.credential_ref.is_some()&&matches!(profile.connection_state,ProviderConnectionState::Connected|ProviderConnectionState::Authenticated|ProviderConnectionState::CredentialsSaved|ProviderConnectionState::Error)&&ui.button("Disconnect").clicked(){send(action("disconnect",json!({})));}
            ui.collapsing("Connection settings",|ui|{
                ui.label("Name");ui.text_edit_singleline(&mut profile.display_name);
                ui.label("Base URL");ui.add(egui::TextEdit::singleline(&mut profile.base_url).desired_width(f32::INFINITY));
                ui.horizontal(|ui|{ui.label("Protocol");for protocol in [WireProtocol::ChatCompletions,WireProtocol::Responses]{ui.selectable_value(&mut profile.protocol,protocol,format!("{protocol:?}"));}});
                ui.checkbox(&mut state.provider_revalidate_check,"Approve credentials for this destination");
                ui.checkbox(&mut profile.allow_screenshot_egress,"Allow screenshots to be sent to this provider");
                if profile.preset==gm_core::config::ProviderPreset::Custom{ui.horizontal(|ui|{ui.label("Authentication");ui.selectable_value(&mut profile.auth_mode,ProviderAuthMode::None,"None / local");ui.selectable_value(&mut profile.auth_mode,ProviderAuthMode::ApiKey,"API key");});}
                ui.label("Fallback profile ID (blank disables fallback)");
                let mut fallback=profile.fallback_profile_id.clone().unwrap_or_default();if ui.text_edit_singleline(&mut fallback).changed(){profile.fallback_profile_id=if fallback.trim().is_empty(){None}else{Some(fallback)};}
            });
            if ui.button("Save & connect").clicked(){
                let key=std::mem::take(&mut *state.provider_api_key_draft);
                send(action("configure",json!({"profile":profile,"api_key":if key.is_empty(){None}else{Some(key)},"revalidate_credentials":state.provider_revalidate_check})));
            }
        }
        ui.add_space(18.0);ui.separator();ui.add_space(8.0);
        ui.horizontal_wrapped(|ui|{
            ui.strong("Model");
            if ui.button("Refresh models").clicked(){send(action("refresh",json!({})));}
            if let Some(date)=&profile.health.models_refreshed_at{ui.small(format!("Updated {}",date.chars().take(16).collect::<String>()));}
        });
        let previous=profile.selected_model.clone();
        egui::ComboBox::from_id_salt("provider_model_picker").selected_text(if previous.is_empty(){"Choose a model"}else{&previous}).width(ui.available_width().min(480.0)).show_ui(ui,|ui|{
            for model in &profile.model_catalog{ui.selectable_value(&mut profile.selected_model,model.id.clone(),if model.display_name.is_empty(){&model.id}else{&model.display_name});}
            if profile.model_catalog.is_empty(){for model in &profile.discovered_models{ui.selectable_value(&mut profile.selected_model,model.clone(),model);}}
        });
        if previous!=profile.selected_model{send(action("select_model",json!({"model":profile.selected_model,"profile":profile})));}
        ui.collapsing("Use a custom model ID",|ui|{ui.text_edit_singleline(&mut profile.selected_model);ui.small("Useful for local servers and models omitted from provider catalogs.");});
        if let Some(model)=profile.model_catalog.iter().find(|m|m.id==profile.selected_model){
            if let Some(context)=model.context_window{ui.small(format!("Context: {context} tokens"));}
            if !model.input_modalities.is_empty(){ui.small(format!("Advertised inputs: {}",model.input_modalities.join(", ")));}
            if !model.output_modalities.is_empty(){ui.small(format!("Advertised outputs: {}",model.output_modalities.join(", ")));}
            if let Some(tools)=model.supports_tools{ui.small(format!("Advertised tool support: {}",if tools{"yes"}else{"no"}));}
        }
        ui.small("The reply test verifies text generation. Vision and tools require their own working flows.");
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui|{
            if ui.add_enabled(!profile.selected_model.trim().is_empty(),egui::Button::new("Test selected model")).clicked(){send(action("select_model",json!({"model":profile.selected_model,"profile":profile})));}
            if ui.add_enabled(profile.health.verification_status=="verified",egui::Button::new("Use as workspace default")).clicked(){send(action("make_default",json!({})));}
            if config.providers.workspace_default_profile_id.as_deref()==Some(&id){ui.colored_label(palette.status_ok,"Workspace default");}
        });
    });
    if profile.connection_state == ProviderConnectionState::Connecting {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));
        if state.provider_poll_at.elapsed() >= std::time::Duration::from_secs(1) {
            send(action("status", json!({})));
            state.provider_poll_at = std::time::Instant::now();
        }
    }
}
