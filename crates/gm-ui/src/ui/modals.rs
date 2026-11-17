//! GrokBot Global Modals (`Tier 2` of the Escape Dismissal Stack):
//! - `CommandPalette` (`Cmd/Ctrl+K`)
//! - `Settings` (`Cmd/Ctrl+,` — 5 Panes: `General`, `Rules & Access`, `Vault & Payments`, `Usage & Budget`, `System & Updates`)
//! - `Studio` (`Cmd/Ctrl+Shift+M` — `Templates`, `Skills`, `Routines`, `Sections`)
//! - `Shortcuts` (`Cmd/Ctrl+/`)
//! - `ConnectApps` (Built-in Bridges + Custom MCP/Webhook Connectors)
//! - `AvatarPicker` (6 Vector Shapes + 12 Preset Colors + Custom Hex + Image Upload)
//! - `CreateBot`, `CreateChannel`, `Lightbox`, `ExpandedTable`

use crate::ui::cache::BoundedTextureCache;
use crate::ui::state::{ActiveModal, GrokUiState, SettingsPane, WorkspaceSection};
use crate::ui::theme::{
    BOT_COLOR_PALETTE, BotAvatarShape, GrokPalette, paint_bot_avatar, parse_hex_color,
};
use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Sense, Vec2};
use gm_core::config::GmConfig;
use gm_core::ipc::DaemonRequest;
use serde_json::Value;

#[allow(clippy::too_many_arguments)]
pub fn draw_active_modal(
    ctx: &egui::Context,
    ui_state: &mut GrokUiState,
    palette: &GrokPalette,
    textures: &mut BoundedTextureCache,
    bots: &[Value],
    sections: &[Value],
    routines: &[Value],
    skills_data: &Value,
    rules: &[Value],
    vault_items: &[Value],
    usage_data: &Value,
    connectors_data: &Value,
    active_bot: &mut String,
    new_channel: &mut String,
    settings_config: &mut GmConfig,
    meta_key: &mut String,
    xai_key: &mut String,
    credentials_notice: &str,
    settings_notice: &mut String,
    mut save_api_keys: impl FnMut(),
    mut save_config_disk: impl FnMut(),
    mut send: impl FnMut(DaemonRequest),
) {
    let Some(modal) = ui_state.active_modal.clone() else {
        return;
    };

    let mut keep_open = true;
    let mut close_requested = false;

    match modal {
        ActiveModal::CommandPalette => {
            egui::Window::new("⌘ Command Palette")
                .open(&mut keep_open)
                .collapsible(false)
                .resizable(false)
                .default_width(480.0)
                .anchor(egui::Align2::CENTER_TOP, Vec2::new(0.0, 72.0))
                .show(ctx, |ui| {
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut ui_state.command_palette_query)
                            .desired_width(ui.available_width())
                            .hint_text("Type a command, bot name, or view…"),
                    );
                    resp.request_focus();
                    ui.separator();

                    let q = ui_state.command_palette_query.trim().to_ascii_lowercase();
                    let mut actions: Vec<(String, String, Box<dyn FnOnce(&mut GrokUiState, &mut String, &mut dyn FnMut(DaemonRequest))>)> = Vec::new();

                    for b in bots {
                        if let Some(id) = b["id"].as_str() {
                            let name = b["display_name"].as_str().unwrap_or(id);
                            let label = format!("🤖 Switch to Bot: {name} (@{id})");
                            let bid = id.to_string();
                            actions.push((
                                label,
                                "Bot DM".into(),
                                Box::new(move |st, ab, s| {
                                    *ab = bid.clone();
                                    st.section = WorkspaceSection::Bots;
                                    st.active_chat_kind = "dm".into();
                                    s(DaemonRequest::GetDirectChatHistory {
                                        bot_id: bid,
                                        before_seq: None,
                                        limit: Some(100),
                                    });
                                }),
                            ));
                        }
                    }

                    actions.push((
                        "⚡ Open Activity Feed".into(),
                        "Workspace".into(),
                        Box::new(|st, _, s| {
                            st.section = WorkspaceSection::Feed;
                            s(DaemonRequest::ListActivity);
                        }),
                    ));
                    actions.push((
                        "🎯 Open Hierarchical Goals".into(),
                        "Workspace".into(),
                        Box::new(|st, _, s| {
                            st.section = WorkspaceSection::Goals;
                            s(DaemonRequest::ListGoals);
                        }),
                    ));
                    actions.push((
                        "🧩 Open Static Spaces Library".into(),
                        "Workspace".into(),
                        Box::new(|st, _, s| {
                            st.section = WorkspaceSection::Spaces;
                            s(DaemonRequest::ListSpaces);
                        }),
                    ));
                    actions.push((
                        "🛡 Open Queue & Governance Audit".into(),
                        "Workspace".into(),
                        Box::new(|st, _, s| {
                            st.section = WorkspaceSection::Audit;
                            s(DaemonRequest::ListQueue { bot_id: None });
                        }),
                    ));
                    actions.push((
                        "⚙ Open Settings (Cmd/Ctrl+,)".into(),
                        "Modal".into(),
                        Box::new(|st, _, _| {
                            st.active_modal = Some(ActiveModal::Settings);
                        }),
                    ));
                    actions.push((
                        "✦ Open Studio — Templates, Skills & Routines (Cmd/Ctrl+Shift+M)".into(),
                        "Modal".into(),
                        Box::new(|st, _, _| {
                            st.active_modal = Some(ActiveModal::Studio);
                        }),
                    ));
                    actions.push((
                        "🔌 Connect Apps & Custom MCP/Webhook Connectors".into(),
                        "Modal".into(),
                        Box::new(|st, _, s| {
                            s(DaemonRequest::ListCustomConnectors);
                            st.active_modal = Some(ActiveModal::ConnectApps);
                        }),
                    ));
                    actions.push((
                        "◧ Toggle Bot Details Drawer (Cmd/Ctrl+Alt+B)".into(),
                        "Drawer".into(),
                        Box::new(|st, _, _| {
                            st.details_drawer_open = !st.details_drawer_open;
                        }),
                    ));
                    actions.push((
                        "⛔ Emergency Kill — Stop All Tasks".into(),
                        "Safety".into(),
                        Box::new(|_, _, s| {
                            s(DaemonRequest::EmergencyKill);
                        }),
                    ));

                    let mut triggered: Option<Box<dyn FnOnce(&mut GrokUiState, &mut String, &mut dyn FnMut(DaemonRequest))>> = None;
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        for (label, cat, cb) in actions {
                            if !q.is_empty() && !label.to_ascii_lowercase().contains(&q) {
                                continue;
                            }
                            ui.horizontal(|ui| {
                                if ui.button(&label).clicked() {
                                    triggered = Some(cb);
                                }
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.small(RichText::new(cat).color(palette.text_muted));
                                });
                            });
                        }
                    });
                    if let Some(cb) = triggered {
                        ui_state.active_modal = None;
                        cb(ui_state, active_bot, &mut send);
                        return;
                    }
                });
        }

        ActiveModal::Shortcuts => {
            egui::Window::new("⌨ Keyboard Shortcuts")
                .open(&mut keep_open)
                .collapsible(false)
                .default_width(440.0)
                .show(ctx, |ui| {
                    let shortcuts = [
                        ("Cmd/Ctrl + K", "Open Command Palette"),
                        ("Cmd/Ctrl + ,", "Open 5-Pane Settings Modal"),
                        ("Cmd/Ctrl + Shift + M", "Open Studio (Templates, Skills, Routines)"),
                        ("Cmd/Ctrl + /", "Open Keyboard Shortcuts Cheat-Sheet"),
                        ("Cmd/Ctrl + Alt + B", "Toggle 340px Bot Details Drawer"),
                        ("Cmd/Ctrl + F", "Toggle Two-Stage FTS5 Message Search"),
                        ("Cmd/Ctrl + D", "Toggle Dictation Mode"),
                        ("Cmd/Ctrl + Enter", "Send Message / Task"),
                        ("Cmd/Ctrl + 1..9", "Switch to Bot 1..9"),
                        ("-> then Backspace", "Auto-replace with → and single-Backspace undo"),
                        ("Escape", "5-Tier Dismissal (Popover → Modal → Split/Computer → Drawer → Reply/Search)"),
                    ];
                    egui::Grid::new("grok_shortcuts_grid")
                        .striped(true)
                        .num_columns(2)
                        .show(ui, |ui| {
                            for (chord, desc) in shortcuts {
                                ui.monospace(RichText::new(chord).color(palette.accent));
                                ui.label(desc);
                                ui.end_row();
                            }
                        });
                });
        }

        ActiveModal::Settings => {
            egui::Window::new("⚙ Settings & Governance")
                .open(&mut keep_open)
                .default_width(680.0)
                .max_height(620.0)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        for pane in SettingsPane::ALL {
                            let active = ui_state.settings_pane == pane;
                            if ui.selectable_label(active, pane.label()).clicked() {
                                ui_state.settings_pane = pane;
                                match pane {
                                    SettingsPane::RulesAndAccess => {
                                        send(DaemonRequest::ManageCustomRule {
                                            op: "list".into(),
                                            rule: None,
                                            id: None,
                                        });
                                    }
                                    SettingsPane::VaultAndPayments => {
                                        send(DaemonRequest::ManageVault {
                                            op: "list".into(),
                                            item: None,
                                            id: None,
                                            reveal_confirmed: false,
                                        });
                                    }
                                    SettingsPane::UsageAndBudget => {
                                        send(DaemonRequest::GetUsageBreakdown { days: Some(7) });
                                    }
                                    _ => {}
                                }
                            }
                        }
                    });
                    ui.separator();

                    egui::ScrollArea::vertical()
                        .id_salt("grok_settings_pane_scroll")
                        .max_height(500.0)
                        .show(ui, |ui| match ui_state.settings_pane {
                            SettingsPane::General => {
                                ui.strong("Appearance & Accent");
                                ui.horizontal(|ui| {
                                    ui.label("Theme:");
                                    for (val, lbl) in [
                                        ("dark", "Dark (#0B0D10)"),
                                        ("light", "Light (#F8FAFC)"),
                                        ("system", "Follow System"),
                                    ] {
                                        if ui
                                            .selectable_label(ui_state.theme_pref == val, lbl)
                                            .clicked()
                                        {
                                            ui_state.theme_pref = val.to_string();
                                            send(DaemonRequest::SaveUiSettings {
                                                ui: serde_json::json!({ "theme": val }),
                                            });
                                        }
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Accent Mode:");
                                    for (val, lbl) in [
                                        ("bot_color", "Active Bot Color"),
                                        ("black", "Monochrome High-Contrast"),
                                    ] {
                                        if ui
                                            .selectable_label(ui_state.accent_mode == val, lbl)
                                            .clicked()
                                        {
                                            ui_state.accent_mode = val.to_string();
                                            send(DaemonRequest::SaveUiSettings {
                                                ui: serde_json::json!({ "accent_mode": val }),
                                            });
                                        }
                                    }
                                });

                                ui.separator();
                                ui.strong("Runtime Preferences");
                                let mut ui_dirty = false;
                                ui_dirty |= ui
                                    .checkbox(
                                        &mut ui_state.sleep_prevention,
                                        "Prevent system sleep while tasks are running",
                                    )
                                    .changed();
                                ui_dirty |= ui
                                    .checkbox(&mut ui_state.wake_at_login, "Launch daemon at login")
                                    .changed();
                                ui_dirty |= ui
                                    .checkbox(
                                        &mut ui_state.hardware_security_keys,
                                        "Require hardware security key confirmation for Vault reveal",
                                    )
                                    .changed();
                                if ui_dirty {
                                    send(DaemonRequest::SaveUiSettings {
                                        ui: serde_json::json!({
                                            "sleep_prevention": ui_state.sleep_prevention,
                                            "wake_at_login": ui_state.wake_at_login,
                                            "hardware_security_keys": ui_state.hardware_security_keys,
                                        }),
                                    });
                                }

                                ui.separator();
                                ui.strong("Provider API Credentials (OS Keychain)");
                                ui.horizontal(|ui| {
                                    ui.label("Meta API Key:");
                                    ui.add(
                                        egui::TextEdit::singleline(meta_key)
                                            .password(true)
                                            .desired_width(220.0),
                                    );
                                });
                                ui.horizontal(|ui| {
                                    ui.label("xAI API Key:");
                                    ui.add(
                                        egui::TextEdit::singleline(xai_key)
                                            .password(true)
                                            .desired_width(220.0),
                                    );
                                });
                                if ui
                                    .add_enabled(
                                        !meta_key.is_empty() || !xai_key.is_empty(),
                                        egui::Button::new("Save API Keys to Keychain"),
                                    )
                                    .clicked()
                                {
                                    save_api_keys();
                                }
                                if !credentials_notice.is_empty() {
                                    ui.small(credentials_notice);
                                }
                            }

                            SettingsPane::RulesAndAccess => {
                                ui.strong("Computer Execution Mode");
                                ui.horizontal(|ui| {
                                    for (val, lbl) in [
                                        ("always", "Autonomous (within safety tier)"),
                                        ("ask_first", "Ask First"),
                                        ("never", "Never Control Desktop"),
                                    ] {
                                        if ui
                                            .selectable_label(ui_state.execution_mode == val, lbl)
                                            .clicked()
                                        {
                                            ui_state.execution_mode = val.to_string();
                                            send(DaemonRequest::SaveUiSettings {
                                                ui: serde_json::json!({
                                                    "computer_execution_mode": val,
                                                }),
                                            });
                                        }
                                    }
                                });
                                if ui
                                    .checkbox(
                                        &mut settings_config.safety.allow_host_desktop,
                                        "Allow agents to observe and control host desktop",
                                    )
                                    .changed()
                                {
                                    send(DaemonRequest::SaveUiSettings {
                                        ui: serde_json::json!({
                                            "allow_host_desktop": settings_config.safety.allow_host_desktop,
                                        }),
                                    });
                                }

                                ui.separator();
                                ui.strong("Custom Auto-Review Rules (Max 20 per Mode · Never overrides Blocklist)");
                                ui.horizontal(|ui| {
                                    egui::ComboBox::from_id_salt("rule_mode_select")
                                        .selected_text(&ui_state.rule_mode_draft)
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut ui_state.rule_mode_draft,
                                                "allow".into(),
                                                "Allow",
                                            );
                                            ui.selectable_value(
                                                &mut ui_state.rule_mode_draft,
                                                "ask_first".into(),
                                                "Ask First",
                                            );
                                        });
                                    ui.add(
                                        egui::TextEdit::singleline(&mut ui_state.rule_pattern_draft)
                                            .desired_width(180.0)
                                            .hint_text("git status*"),
                                    );
                                    ui.add(
                                        egui::TextEdit::singleline(&mut ui_state.rule_desc_draft)
                                            .desired_width(150.0)
                                            .hint_text("Description"),
                                    );
                                    if ui
                                        .add_enabled(
                                            !ui_state.rule_pattern_draft.trim().is_empty(),
                                            egui::Button::new("＋ Add Rule"),
                                        )
                                        .clicked()
                                    {
                                        send(DaemonRequest::ManageCustomRule {
                                            op: "upsert".into(),
                                            id: None,
                                            rule: Some(serde_json::json!({
                                                "mode": ui_state.rule_mode_draft,
                                                "pattern": ui_state.rule_pattern_draft.trim(),
                                                "description": ui_state.rule_desc_draft.trim(),
                                                "enabled": true,
                                            })),
                                        });
                                        ui_state.rule_pattern_draft.clear();
                                        ui_state.rule_desc_draft.clear();
                                    }
                                });
                                for r in rules {
                                    let rid = r["id"].as_str().unwrap_or("");
                                    let mode = r["mode"].as_str().unwrap_or("ask_first");
                                    let pat = r["pattern"].as_str().unwrap_or("");
                                    ui.horizontal(|ui| {
                                        ui.monospace(format!("[{mode}] {pat}"));
                                        if ui.small_button("🗑").clicked() {
                                            send(DaemonRequest::ManageCustomRule {
                                                op: "delete".into(),
                                                id: Some(rid.to_string()),
                                                rule: None,
                                            });
                                        }
                                    });
                                }
                            }

                            SettingsPane::VaultAndPayments => {
                                ui.strong("Keychain Credential & Payment Vault (RULE-SECRET-001)");
                                ui.small("Metadata lives in SQLite; plaintext secrets live exclusively in OS Keychain.");
                                ui.horizontal(|ui| {
                                    egui::ComboBox::from_id_salt("vault_kind_select")
                                        .selected_text(&ui_state.vault_kind_draft)
                                        .show_ui(ui, |ui| {
                                            for k in [
                                                "login",
                                                "card",
                                                "payment_method",
                                                "contact_info",
                                                "hint",
                                                "password_manager",
                                            ] {
                                                ui.selectable_value(
                                                    &mut ui_state.vault_kind_draft,
                                                    k.into(),
                                                    k,
                                                );
                                            }
                                        });
                                    ui.add(
                                        egui::TextEdit::singleline(&mut ui_state.vault_label_draft)
                                            .desired_width(120.0)
                                            .hint_text("Label"),
                                    );
                                    ui.add(
                                        egui::TextEdit::singleline(
                                            &mut ui_state.vault_username_draft,
                                        )
                                        .desired_width(110.0)
                                        .hint_text("User / Last4"),
                                    );
                                    ui.add(
                                        egui::TextEdit::singleline(&mut *ui_state.vault_secret_draft)
                                            .password(true)
                                            .desired_width(120.0)
                                            .hint_text("Secret"),
                                    );
                                    if ui
                                        .add_enabled(
                                            !ui_state.vault_label_draft.trim().is_empty(),
                                            egui::Button::new("＋ Save"),
                                        )
                                        .clicked()
                                    {
                                        send(DaemonRequest::ManageVault {
                                            op: "upsert".into(),
                                            id: None,
                                            reveal_confirmed: false,
                                            item: Some(serde_json::json!({
                                                "kind": ui_state.vault_kind_draft,
                                                "label": ui_state.vault_label_draft.trim(),
                                                "subtitle_or_last4": ui_state.vault_username_draft.trim(),
                                                "username": ui_state.vault_username_draft.trim(),
                                                "domain": ui_state.vault_domain_draft.trim(),
                                                "hint": "",
                                                "secret_value": (*ui_state.vault_secret_draft).clone(),
                                                "is_default_payment": false,
                                            })),
                                        });
                                        ui_state.vault_label_draft.clear();
                                        ui_state.vault_username_draft.clear();
                                        use zeroize::Zeroize;
                                        ui_state.vault_secret_draft.zeroize();
                                    }
                                });

                                for item in vault_items {
                                    let vid = item["id"].as_str().unwrap_or("");
                                    let kind = item["kind"].as_str().unwrap_or("login");
                                    let label = item["label"].as_str().unwrap_or("");
                                    let is_def = item["is_default_payment"].as_bool().unwrap_or(false);
                                    ui.horizontal(|ui| {
                                        ui.strong(format!(
                                            "[{kind}] {label}{}",
                                            if is_def { " ★ Default" } else { "" }
                                        ));
                                        if matches!(kind, "card" | "payment_method")
                                            && !is_def
                                            && ui.small_button("Set Default").clicked()
                                        {
                                            send(DaemonRequest::ManageVault {
                                                op: "default_payment".into(),
                                                id: Some(vid.to_string()),
                                                item: None,
                                                reveal_confirmed: false,
                                            });
                                        }
                                        if ui.small_button("Reveal").clicked() {
                                            send(DaemonRequest::ManageVault {
                                                op: "reveal".into(),
                                                id: Some(vid.to_string()),
                                                item: None,
                                                reveal_confirmed: true,
                                            });
                                        }
                                        if ui.small_button("🗑").clicked() {
                                            send(DaemonRequest::ManageVault {
                                                op: "delete".into(),
                                                id: Some(vid.to_string()),
                                                item: None,
                                                reveal_confirmed: false,
                                            });
                                        }
                                    });
                                    let mut hide = false;
                                    if let Some(secret) = ui_state.revealed_secrets.get(vid) {
                                        ui.horizontal(|ui| {
                                            ui.monospace(secret.as_str());
                                            if ui.small_button("Copy").clicked() {
                                                ui.ctx().copy_text(secret.clone());
                                            }
                                            hide = ui.small_button("Hide").clicked();
                                        });
                                    }
                                    if hide && let Some(mut s) = ui_state.revealed_secrets.remove(vid) {
                                        use zeroize::Zeroize;
                                        s.zeroize();
                                    }
                                }

                                ui.separator();
                                ui.collapsing("Import Password Manager CSV (< 5 MB)", |ui| {
                                    ui.add(
                                        egui::TextEdit::multiline(&mut ui_state.vault_csv_draft)
                                            .desired_rows(3)
                                            .desired_width(ui.available_width())
                                            .hint_text("title,url,username,password…"),
                                    );
                                    if ui.small_button("Import CSV").clicked()
                                        && !ui_state.vault_csv_draft.trim().is_empty()
                                    {
                                        send(DaemonRequest::ImportVaultCsv {
                                            csv: ui_state.vault_csv_draft.clone(),
                                            source_hint: None,
                                        });
                                        ui_state.vault_csv_draft.clear();
                                    }
                                });
                            }

                            SettingsPane::UsageAndBudget => {
                                ui.strong("Spending Limits (USD)");
                                ui.horizontal(|ui| {
                                    ui.label("Daily Cap ($):");
                                    ui.add(
                                        egui::DragValue::new(
                                            &mut settings_config.budget.daily_spend_cap_usd,
                                        )
                                        .range(0.0..=10000.0)
                                        .speed(0.5),
                                    );
                                    ui.label("Per-Job Cap ($):");
                                    ui.add(
                                        egui::DragValue::new(
                                            &mut settings_config.budget.per_job_spend_cap_usd,
                                        )
                                        .range(0.0..=1000.0)
                                        .speed(0.1),
                                    );
                                });
                                ui.separator();
                                ui.horizontal(|ui| {
                                    ui.strong("Usage Breakdown");
                                    for d in [7u32, 30, 90] {
                                        if ui.small_button(format!("{d}d")).clicked() {
                                            send(DaemonRequest::GetUsageBreakdown { days: Some(d) });
                                        }
                                    }
                                });
                                let daily = usage_data["daily_spent_usd"].as_f64().unwrap_or(0.0);
                                let total = usage_data["window_total_usd"].as_f64().unwrap_or(0.0);
                                ui.label(format!(
                                    "Today: ${daily:.4} · Window Total: ${total:.4}"
                                ));
                            }

                            SettingsPane::SystemAndUpdates => {
                                ui.strong("Configuration & System");
                                if ui.button("Save Config File").clicked() {
                                    save_config_disk();
                                }
                                if !settings_notice.is_empty() {
                                    ui.small(settings_notice.as_str());
                                }
                            }
                        });
                });
        }

        ActiveModal::Studio => {
            egui::Window::new("✦ Studio — Templates, Skills, Routines & Sections")
                .open(&mut keep_open)
                .default_width(640.0)
                .max_height(600.0)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        for (k, lbl) in [
                            ("templates", "Team Templates"),
                            ("skills", "Skills"),
                            ("routines", "Routines"),
                            ("sections", "Bot Sections"),
                        ] {
                            if ui.selectable_label(ui_state.studio_tab == k, lbl).clicked() {
                                ui_state.studio_tab = k.to_string();
                                if k == "skills" {
                                    send(DaemonRequest::SearchSkills {
                                        query: String::new(),
                                    });
                                } else if k == "routines" {
                                    send(DaemonRequest::ListRoutines);
                                }
                            }
                        }
                    });
                    ui.separator();

                    egui::ScrollArea::vertical()
                        .id_salt("grok_studio_scroll")
                        .max_height(480.0)
                        .show(ui, |ui| match ui_state.studio_tab.as_str() {
                            "templates" => {
                                ui.strong("Export Portable Team Template");
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(
                                            &mut ui_state.template_export_name,
                                        )
                                        .desired_width(180.0),
                                    );
                                    if ui.button("Export Team JSON").clicked() {
                                        send(DaemonRequest::ManageSystem {
                                            op: "export_template".into(),
                                            payload: serde_json::json!({
                                                "name": ui_state.template_export_name,
                                                "description": ui_state.template_export_desc,
                                            }),
                                        });
                                    }
                                });
                                ui.separator();
                                ui.strong("Import Team Template (Credential-Scrubbed)");
                                ui.add(
                                    egui::TextEdit::multiline(&mut ui_state.template_import_json)
                                        .desired_rows(6)
                                        .desired_width(ui.available_width())
                                        .code_editor(),
                                );
                                ui.horizontal(|ui| {
                                    if ui.button("Preview Template").clicked()
                                        && !ui_state.template_import_json.trim().is_empty()
                                    {
                                        send(DaemonRequest::ManageSystem {
                                            op: "preview_template".into(),
                                            payload: serde_json::json!({
                                                "template": ui_state.template_import_json,
                                            }),
                                        });
                                    }
                                    if ui.button("Import & Apply").clicked()
                                        && !ui_state.template_import_json.trim().is_empty()
                                    {
                                        send(DaemonRequest::ManageSystem {
                                            op: "import_template".into(),
                                            payload: serde_json::json!({
                                                "template": ui_state.template_import_json,
                                                "overwrite": true,
                                            }),
                                        });
                                    }
                                });
                            }

                            "skills" => {
                                let skills = skills_data["skills"]
                                    .as_array()
                                    .cloned()
                                    .unwrap_or_default();
                                ui.strong(format!("Installed Skills ({})", skills.len()));
                                for sk in &skills {
                                    let name = sk["manifest"]["name"]
                                        .as_str()
                                        .or_else(|| sk["dir_name"].as_str())
                                        .unwrap_or("skill");
                                    let bundled = sk["bundled"].as_bool().unwrap_or(false);
                                    ui.horizontal(|ui| {
                                        ui.strong(name);
                                        ui.small(if bundled { "bundled" } else { "custom" });
                                        if !bundled && ui.small_button("🗑").clicked() {
                                            send(DaemonRequest::ManageSkill {
                                                op: "delete".into(),
                                                name: Some(name.to_string()),
                                                payload: Value::Null,
                                            });
                                        }
                                    });
                                }
                                ui.separator();
                                ui.strong("Author Custom Skill (SKILL.md)");
                                ui.add(
                                    egui::TextEdit::singleline(&mut ui_state.custom_skill_name)
                                        .hint_text("slug (e.g. weekly-report)"),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(&mut ui_state.custom_skill_title)
                                        .hint_text("Title"),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(&mut ui_state.custom_skill_desc)
                                        .hint_text("One-line description"),
                                );
                                ui.add(
                                    egui::TextEdit::multiline(&mut ui_state.custom_skill_body)
                                        .desired_rows(5)
                                        .desired_width(ui.available_width())
                                        .hint_text("Instructions (markdown)…"),
                                );
                                if ui
                                    .add_enabled(
                                        !ui_state.custom_skill_name.trim().is_empty()
                                            && !ui_state.custom_skill_body.trim().is_empty(),
                                        egui::Button::new("Save Skill"),
                                    )
                                    .clicked()
                                {
                                    send(DaemonRequest::ManageSkill {
                                        op: "create".into(),
                                        name: Some(ui_state.custom_skill_name.trim().to_string()),
                                        payload: serde_json::json!({
                                            "title": ui_state.custom_skill_title.trim(),
                                            "description": ui_state.custom_skill_desc.trim(),
                                            "instructions": ui_state.custom_skill_body,
                                        }),
                                    });
                                    ui_state.custom_skill_name.clear();
                                    ui_state.custom_skill_title.clear();
                                    ui_state.custom_skill_desc.clear();
                                    ui_state.custom_skill_body.clear();
                                }
                            }

                            "routines" => {
                                ui.strong(format!("Scheduled Routines ({})", routines.len()));
                                for st in routines {
                                    let r = &st["routine"];
                                    let rid = r["id"].as_str().unwrap_or("");
                                    let rname = r["name"].as_str().unwrap_or("Routine");
                                    let bot_id = r["bot_id"].as_str().unwrap_or("muse");
                                    ui.group(|ui| {
                                        ui.horizontal(|ui| {
                                            ui.strong(format!("{rname} (@{bot_id})"));
                                            if ui.small_button("▶ Test Now").clicked() {
                                                send(DaemonRequest::TriggerRoutineTest {
                                                    routine_id: rid.to_string(),
                                                });
                                            }
                                            if ui.small_button("🗑").clicked() {
                                                send(DaemonRequest::DeleteRoutine {
                                                    routine_id: rid.to_string(),
                                                });
                                            }
                                        });
                                    });
                                }
                            }

                            "sections" => {
                                ui.strong("Custom Bot Sidebar Sections");
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut ui_state.new_section_name)
                                            .desired_width(200.0)
                                            .hint_text("Engineering"),
                                    );
                                    if ui
                                        .add_enabled(
                                            !ui_state.new_section_name.trim().is_empty(),
                                            egui::Button::new("＋ Create Section"),
                                        )
                                        .clicked()
                                    {
                                        send(DaemonRequest::ManageBotSection {
                                            op: "upsert".into(),
                                            id: None,
                                            name: Some(ui_state.new_section_name.trim().to_string()),
                                            collapsed: Some(false),
                                            sort_order: None,
                                        });
                                        ui_state.new_section_name.clear();
                                    }
                                });
                                for sec in sections {
                                    let sid = sec["id"].as_str().unwrap_or("");
                                    let sname = sec["name"].as_str().unwrap_or("");
                                    ui.horizontal(|ui| {
                                        ui.label(sname);
                                        if ui.small_button("🗑").clicked() {
                                            send(DaemonRequest::ManageBotSection {
                                                op: "delete".into(),
                                                id: Some(sid.to_string()),
                                                name: None,
                                                collapsed: None,
                                                sort_order: None,
                                            });
                                        }
                                    });
                                }
                            }
                            _ => {}
                        });
                });
        }

        ActiveModal::ConnectApps => {
            egui::Window::new("🔌 Connect Apps & Custom MCP/Webhook Connectors")
                .open(&mut keep_open)
                .default_width(580.0)
                .show(ctx, |ui| {
                    ui.strong("Add Custom MCP or Webhook Connector");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut ui_state.connector_name_draft)
                                .desired_width(130.0)
                                .hint_text("Name"),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut ui_state.connector_endpoint_draft)
                                .desired_width(200.0)
                                .hint_text("http://127.0.0.1:8090/mcp"),
                        );
                        if ui
                            .add_enabled(
                                !ui_state.connector_name_draft.trim().is_empty()
                                    && !ui_state.connector_endpoint_draft.trim().is_empty(),
                                egui::Button::new("＋ Add"),
                            )
                            .clicked()
                        {
                            send(DaemonRequest::SaveCustomConnector {
                                op: "upsert".into(),
                                connector: Some(serde_json::json!({
                                    "name": ui_state.connector_name_draft.trim(),
                                    "kind": ui_state.connector_kind_draft,
                                    "endpoint_url": ui_state.connector_endpoint_draft.trim(),
                                    "enabled": true,
                                })),
                                id: None,
                            });
                            ui_state.connector_name_draft.clear();
                            ui_state.connector_endpoint_draft.clear();
                        }
                    });
                    ui.separator();
                    let conns = connectors_data["connectors"]
                        .as_array()
                        .or_else(|| connectors_data["custom_connectors"].as_array())
                        .cloned()
                        .unwrap_or_default();
                    for c in &conns {
                        let cid = c["id"].as_str().unwrap_or("");
                        let cname = c["name"].as_str().unwrap_or(cid);
                        let url = c["endpoint_url"].as_str().unwrap_or("");
                        ui.horizontal(|ui| {
                            ui.strong(cname);
                            ui.small(url);
                            if ui.small_button("Test Probe").clicked() {
                                send(DaemonRequest::SaveCustomConnector {
                                    op: "test".into(),
                                    connector: None,
                                    id: Some(cid.to_string()),
                                });
                            }
                            if ui.small_button("🗑").clicked() {
                                send(DaemonRequest::SaveCustomConnector {
                                    op: "delete".into(),
                                    connector: None,
                                    id: Some(cid.to_string()),
                                });
                            }
                        });
                    }
                });
        }

        ActiveModal::AvatarPicker { bot_id } => {
            egui::Window::new(format!("🎨 Customize @{bot_id} Shape & Color"))
                .open(&mut keep_open)
                .collapsible(false)
                .default_width(420.0)
                .show(ctx, |ui| {
                    let preview_color = parse_hex_color(&ui_state.avatar_color_draft);
                    let preview_shape = BotAvatarShape::from_name(&ui_state.avatar_shape_draft);
                    ui.horizontal(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(Vec2::new(48.0, 48.0), Sense::hover());
                        let tex = textures
                            .get(&format!("avatar:{bot_id}"))
                            .map(|(t, _)| t);
                        paint_bot_avatar(
                            ui.painter(),
                            rect,
                            preview_shape,
                            preview_color,
                            &bot_id,
                            tex,
                            Some("online"),
                            palette.bg_elevated,
                        );
                        ui.vertical(|ui| {
                            ui.strong(format!("@{bot_id}"));
                            ui.small(format!(
                                "Shape: {} · Color: {}",
                                preview_shape.label(),
                                ui_state.avatar_color_draft
                            ));
                        });
                    });
                    ui.separator();
                    ui.strong("Vector Shape");
                    ui.horizontal_wrapped(|ui| {
                        for sh in BotAvatarShape::ALL {
                            if ui
                                .selectable_label(
                                    ui_state.avatar_shape_draft == sh.as_str(),
                                    sh.label(),
                                )
                                .clicked()
                            {
                                ui_state.avatar_shape_draft = sh.as_str().to_string();
                            }
                        }
                    });
                    ui.separator();
                    ui.strong("12 Preset GrokBot Colors");
                    ui.horizontal_wrapped(|ui| {
                        for (name, hex, col) in BOT_COLOR_PALETTE {
                            let btn = egui::Button::new(RichText::new(*name).size(11.0).color(Color32::WHITE))
                                .fill(*col)
                                .corner_radius(CornerRadius::same(6));
                            if ui.add(btn).clicked() {
                                ui_state.avatar_color_draft = (*hex).to_string();
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Custom Hex:");
                        ui.add(
                            egui::TextEdit::singleline(&mut ui_state.avatar_color_draft)
                                .desired_width(100.0),
                        );
                    });
                    ui.separator();
                    if ui.button("Save Shape & Color").clicked() {
                        send(DaemonRequest::SetBotUiPrefs {
                            bot_id: bot_id.clone(),
                            prefs: serde_json::json!({
                                "avatar_shape": ui_state.avatar_shape_draft,
                                "color": ui_state.avatar_color_draft,
                            }),
                        });
                        close_requested = true;
                    }
                });
        }

        ActiveModal::CreateBot => {
            egui::Window::new("🤖 Create Specialist Bot")
                .open(&mut keep_open)
                .collapsible(false)
                .default_width(440.0)
                .show(ctx, |ui| {
                    ui.label("Bot ID (lowercase slug):");
                    ui.text_edit_singleline(&mut ui_state.new_bot_id);
                    ui.label("Display Name:");
                    ui.text_edit_singleline(&mut ui_state.new_bot_name);
                    ui.label("Role Title:");
                    ui.text_edit_singleline(&mut ui_state.new_bot_role);
                    ui.label("System Prompt:");
                    ui.add(
                        egui::TextEdit::multiline(&mut ui_state.new_bot_prompt)
                            .desired_rows(4)
                            .desired_width(ui.available_width()),
                    );
                    if ui
                        .add_enabled(
                            !ui_state.new_bot_id.trim().is_empty(),
                            egui::Button::new("Create Bot"),
                        )
                        .clicked()
                    {
                        let id = ui_state.new_bot_id.trim().to_string();
                        let name = if ui_state.new_bot_name.trim().is_empty() {
                            id.clone()
                        } else {
                            ui_state.new_bot_name.trim().to_string()
                        };
                        send(DaemonRequest::SaveBot {
                            profile: serde_json::json!({
                                "id": id,
                                "display_name": name,
                                "role_title": ui_state.new_bot_role.trim(),
                                "system_prompt": ui_state.new_bot_prompt.trim(),
                                "allow_gui": false,
                                "allow_shell": false,
                                "allow_network": true,
                            }),
                        });
                        ui_state.new_bot_id.clear();
                        ui_state.new_bot_name.clear();
                        close_requested = true;
                    }
                });
        }

        ActiveModal::CreateChannel => {
            egui::Window::new("# Create Group Channel")
                .open(&mut keep_open)
                .collapsible(false)
                .default_width(360.0)
                .show(ctx, |ui| {
                    ui.label("Channel Slug:");
                    ui.text_edit_singleline(new_channel);
                    if ui
                        .add_enabled(
                            !new_channel.trim().is_empty(),
                            egui::Button::new("Create Channel"),
                        )
                        .clicked()
                    {
                        send(DaemonRequest::CreateChannel {
                            channel_id: new_channel.trim().to_string(),
                            topic: String::new(),
                            owner_bot_id: Some(active_bot.clone()),
                            participants: vec![active_bot.clone()],
                        });
                        new_channel.clear();
                        close_requested = true;
                    }
                });
        }

        ActiveModal::RoutineEditor => {
            keep_open = false;
        }

        ActiveModal::Lightbox { title, uri, mime } => {
            egui::Window::new(format!("🖼 {title}"))
                .open(&mut keep_open)
                .default_width(640.0)
                .show(ctx, |ui| {
                    ui.small(format!("{mime} · {uri}"));
                    if let Some((tex_id, [w, h])) = textures.get(&format!("lightbox:{uri}")) {
                        let avail = ui.available_width().max(240.0);
                        let aspect = (h as f32 / (w.max(1) as f32)).clamp(0.3, 1.5);
                        ui.image(egui::load::SizedTexture::new(
                            tex_id,
                            Vec2::new(avail, avail * aspect),
                        ));
                    } else {
                        ui.label(format!("Media URI: {uri}"));
                    }
                });
            if !keep_open {
                textures.evict_prefix("lightbox:");
            }
        }

        ActiveModal::ExpandedTable {
            title,
            headers,
            rows,
            raw_markdown,
        } => {
            egui::Window::new(format!("📊 {title}"))
                .open(&mut keep_open)
                .default_width(720.0)
                .max_height(580.0)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("Copy Markdown").clicked() {
                            ctx.copy_text(raw_markdown.clone());
                        }
                    });
                    ui.separator();
                    egui::ScrollArea::both()
                        .id_salt("grok_expanded_table_scroll")
                        .show(ui, |ui| {
                            egui::Grid::new("grok_expanded_table_grid")
                                .striped(true)
                                .show(ui, |ui| {
                                    for h in &headers {
                                        ui.strong(h);
                                    }
                                    ui.end_row();
                                    for r in &rows {
                                        for c in r {
                                            ui.label(c);
                                        }
                                        ui.end_row();
                                    }
                                });
                        });
                });
        }
    }

    if !keep_open || close_requested {
        ui_state.active_modal = None;
    }
}
