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
use base64::Engine;
use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Sense, Vec2};
use gm_core::config::GmConfig;
use gm_core::ipc::DaemonRequest;
use serde_json::Value;

type PaletteAction = Box<dyn FnOnce(&mut GrokUiState, &mut String, &mut dyn FnMut(DaemonRequest))>;
type PaletteEntry = (String, String, PaletteAction);

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
    settings_notice: &mut String,
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
            egui::Window::new("Command Palette")
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
                    let mut actions: Vec<PaletteEntry> = Vec::new();

                    for b in bots {
                        if let Some(id) = b["id"].as_str() {
                            let name = b["display_name"].as_str().unwrap_or(id);
                            let label = format!("Switch to Bot: {name} (@{id})");
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
                        "Open Activity Feed".into(),
                        "Workspace".into(),
                        Box::new(|st, _, s| {
                            st.section = WorkspaceSection::Feed;
                            s(DaemonRequest::ListActivity);
                        }),
                    ));
                    actions.push((
                        "Open Hierarchical Goals".into(),
                        "Workspace".into(),
                        Box::new(|st, _, s| {
                            st.section = WorkspaceSection::Goals;
                            s(DaemonRequest::ListGoals);
                        }),
                    ));
                    actions.push((
                        "Open Pages Library".into(),
                        "Workspace".into(),
                        Box::new(|st, _, s| {
                            st.section = WorkspaceSection::Spaces;
                            s(DaemonRequest::ListSpaces);
                        }),
                    ));
                    actions.push((
                        "Open Queue & Governance Audit".into(),
                        "Workspace".into(),
                        Box::new(|st, _, s| {
                            st.section = WorkspaceSection::Audit;
                            s(DaemonRequest::ListQueue { bot_id: None });
                        }),
                    ));
                    actions.push((
                        "Open Settings (Cmd/Ctrl+,)".into(),
                        "Modal".into(),
                        Box::new(|st, _, _| {
                            st.active_modal = Some(ActiveModal::Settings);
                        }),
                    ));
                    actions.push((
                        "Open Studio — Templates, Skills & Routines (Cmd/Ctrl+Shift+M)".into(),
                        "Modal".into(),
                        Box::new(|st, _, _| {
                            st.active_modal = Some(ActiveModal::Studio);
                        }),
                    ));
                    actions.push((
                        "Connect Apps & Custom MCP/Webhook Connectors".into(),
                        "Modal".into(),
                        Box::new(|st, _, s| {
                            s(DaemonRequest::ListCustomConnectors);
                            st.active_modal = Some(ActiveModal::ConnectApps);
                        }),
                    ));
                    actions.push((
                        "Toggle Bot Details Drawer (Cmd/Ctrl+Alt+B)".into(),
                        "Drawer".into(),
                        Box::new(|st, _, _| {
                            st.details_drawer_open = !st.details_drawer_open;
                        }),
                    ));
                    actions.push((
                        "Emergency Kill — Stop All Tasks".into(),
                        "Safety".into(),
                        Box::new(|_, _, s| {
                            s(DaemonRequest::EmergencyKill);
                        }),
                    ));

                    actions.retain(|(label, _, _)| {
                        q.is_empty() || label.to_ascii_lowercase().contains(&q)
                    });
                    let down = ctx.input_mut(|i| {
                        i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)
                            || i.consume_key(egui::Modifiers::CTRL, egui::Key::N)
                    });
                    let up = ctx.input_mut(|i| {
                        i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)
                            || i.consume_key(egui::Modifiers::CTRL, egui::Key::P)
                    });
                    if down {
                        ui_state.command_palette_selected = (ui_state.command_palette_selected + 1)
                            .min(actions.len().saturating_sub(1));
                    }
                    if up {
                        ui_state.command_palette_selected =
                            ui_state.command_palette_selected.saturating_sub(1);
                    }
                    let enter =
                        ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
                    let mut triggered: Option<PaletteAction> = None;
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            for (action_index, (label, cat, cb)) in actions.into_iter().enumerate()
                            {
                                if !q.is_empty() && !label.to_ascii_lowercase().contains(&q) {
                                    continue;
                                }
                                ui.horizontal(|ui| {
                                    if ui
                                        .selectable_label(
                                            action_index == ui_state.command_palette_selected,
                                            &label,
                                        )
                                        .clicked()
                                        || (enter
                                            && action_index == ui_state.command_palette_selected)
                                    {
                                        triggered = Some(cb);
                                    }
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        ui.small(RichText::new(cat).color(palette.text_muted));
                                    });
                                });
                            }
                        });
                    if ui.button("Search older history").clicked() {
                        send(DaemonRequest::SearchMessages {
                            query: ui_state.command_palette_query.clone(),
                            include_older: true,
                            bot_id: None,
                        });
                        ui_state.search_bar_open = true;
                    }
                    if let Some(cb) = triggered {
                        ui_state.active_modal = None;
                        cb(ui_state, active_bot, &mut send);
                    }
                });
        }

        ActiveModal::Shortcuts => {
            egui::Window::new("Keyboard Shortcuts")
                .open(&mut keep_open)
                .collapsible(false)
                .default_width(440.0)
                .show(ctx, |ui| {
                    let shortcuts = [
                        ("Cmd/Ctrl + K", "Open Command Palette"),
                        ("Cmd/Ctrl + ,", "Open Settings Modal"),
                        ("Cmd/Ctrl + B", "Toggle Sidebar Collapse"),
                        ("Cmd/Ctrl + Shift + F", "Expand Sidebar & Search Conversations"),
                        ("Cmd/Ctrl + Shift + M", "Open Studio (Templates, Skills, Routines)"),
                        ("Cmd/Ctrl + /", "Open Keyboard Shortcuts Cheat-Sheet"),
                        ("Cmd/Ctrl + Alt + B", "Toggle 340px Bot Details Drawer"),
                        ("Cmd/Ctrl + F", "Toggle Two-Stage FTS5 Message Search"),
                        ("Cmd/Ctrl + D", "Start / Stop Dictation Session"),
                        ("Cmd/Ctrl + Enter", "Send Message / Task"),
                        ("Cmd/Ctrl + 1..5", "Switch Section (Bots, Feed, Goals, Pages, Audit)"),
                        ("Cmd/Ctrl + 6..9", "Switch to Bot 6..9"),
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
            let settings_size = egui::vec2(
                (ctx.content_rect().width() - 48.0).min(1080.0),
                (ctx.content_rect().height() - 60.0).min(740.0),
            );
            egui::Window::new("Settings")
                .open(&mut keep_open)
                .fixed_size(settings_size)
                .fixed_pos(ctx.content_rect().center()-settings_size*0.5)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(egui::vec2(200.0,ui.available_height()),egui::Layout::top_down(egui::Align::Min),|ui| {
                        for pane in SettingsPane::ALL {
                            let active = ui_state.settings_pane == pane;
                            if ui.add_sized([190.0,38.0],egui::Button::new(pane.label()).selected(active)).clicked() {
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
                                    SettingsPane::SystemAndUpdates => {
                                        send(DaemonRequest::ManageSystem {
                                            op: "components_list".into(),
                                            payload: Value::Null,
                                        });
                                    }
                                    _ => {}
                                }
                            }
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_width((settings_size.x-240.0).max(240.0));

                    egui::ScrollArea::vertical()
                        .id_salt("grok_settings_pane_scroll")
                        .max_height((ctx.content_rect().height()-150.0).min(660.0))
                        .show(ui, |ui| match ui_state.settings_pane {
                            SettingsPane::Providers => crate::ui::providers::draw(ui,ui_state,palette,settings_config,&mut send),
                            SettingsPane::General => {
                                ui.strong("Appearance & Accent");
                                ui.horizontal(|ui| {
                                    ui.label("Theme:");
                                    for (val, lbl) in [
                                        ("dark", "Dark"),
                                        ("light", "Light"),
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
                                ui.small("Bot colors appear in avatars and message authors.");

                                ui.separator();
                                ui.strong("Performance");
                                if ui.checkbox(&mut settings_config.ui.hardware_acceleration, "Use hardware acceleration").changed() {
                                    send(DaemonRequest::SaveUiSettings { ui: serde_json::json!({"hardware_acceleration": settings_config.ui.hardware_acceleration}) });
                                    *settings_notice = "Restart required to apply graphics backend change".into();
                                }
                                ui.strong("Notifications & Locale");
                                ui.checkbox(&mut settings_config.ui.notifications_enabled,"Desktop alerts");
                                ui.checkbox(&mut settings_config.ui.notification_sound,"Alert sound");
                                ui.checkbox(&mut settings_config.ui.suppress_notifications_when_active,"Suppress when active");
                                ui.checkbox(&mut settings_config.ui.spellcheck,"Spellcheck");
                                ui.label("Language");ui.text_edit_singleline(&mut settings_config.ui.language);
                                ui.label("IANA Timezone");ui.text_edit_singleline(&mut settings_config.ui.timezone);
                                let mut quiet_start=settings_config.ui.quiet_hours_start.clone().unwrap_or_default();
                                let mut quiet_end=settings_config.ui.quiet_hours_end.clone().unwrap_or_default();
                                ui.label("Quiet hours start/end (HH:MM, empty disables)");
                                ui.text_edit_singleline(&mut quiet_start);ui.text_edit_singleline(&mut quiet_end);
                                settings_config.ui.quiet_hours_start=if quiet_start.is_empty(){None}else{Some(quiet_start)};
                                settings_config.ui.quiet_hours_end=if quiet_end.is_empty(){None}else{Some(quiet_end)};
                                if ui.button("Save Notification & Locale Preferences").clicked(){send(DaemonRequest::SaveUiSettings {ui:serde_json::to_value(&settings_config.ui).unwrap_or_default()});}
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
                                if ui.button("Providers & models").clicked(){ui_state.settings_pane=SettingsPane::Providers;}

                            }

                            SettingsPane::RulesAndAccess => {
                                ui.strong("Computer Execution Mode");
                                ui.horizontal(|ui| {
                                    for (val, lbl) in [
                                        ("always", "Autonomous (within safety tier)"),
                                        ("ask", "Ask First"),
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
                                            egui::Button::new("+ Add Rule"),
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
                                        if ui.small_button("Delete").clicked() {
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
                                            egui::Button::new("+ Save"),
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
                                            if is_def { " (Default)" } else { "" }
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
                                let total = usage_data["weekly_spent_usd"].as_f64().unwrap_or(0.0);
                                let cgpt_req = usage_data["chatgpt_allowance_requests"]
                                    .as_u64()
                                    .unwrap_or(0);
                                let cgpt_tok = usage_data["chatgpt_allowance_tokens"]
                                    .as_u64()
                                    .unwrap_or(0);
                                ui.label(format!(
                                    "API Spend · Today: ${daily:.4} · Weekly Total: ${total:.4}"
                                ));
                                ui.small(format!(
                                    "ChatGPT Included Allowance (separate from USD spend): {cgpt_req} requests · {cgpt_tok} tokens"
                                ));
                                for key in ["by_bot", "by_source"] {
                                    ui.collapsing(key, |ui| {
                                        ui.monospace(
                                            serde_json::to_string_pretty(&usage_data[key])
                                                .unwrap_or_default(),
                                        );
                                    });
                                }


                                ui.separator();
                                ui.label("Legacy Meta / xAI endpoints & models");
                                ui.text_edit_singleline(&mut settings_config.providers.meta_api_base);
                                ui.text_edit_singleline(&mut settings_config.providers.meta_reasoning_model);
                                ui.text_edit_singleline(&mut settings_config.providers.xai_api_base);
                                ui.text_edit_singleline(&mut settings_config.providers.xai_model);
                                ui.checkbox(&mut settings_config.providers.allow_screenshot_egress_meta,"Allow Meta screenshot egress");
                                ui.checkbox(&mut settings_config.providers.allow_screenshot_egress_xai,"Allow xAI screenshot egress");
                                if ui.button("Save Providers & Budget").clicked(){send(DaemonRequest::SaveUiSettings {ui:serde_json::json!({"providers":settings_config.providers,"budget":settings_config.budget})});}

                            }

                            SettingsPane::SystemAndUpdates => {
                                ui.strong("Configuration & System");
                                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                                ui.horizontal_wrapped(|ui| {
                                    if ui.button("Check for Updates").clicked() {
                                        send(DaemonRequest::CheckUpdates);
                                    }
                                    if ui.button("DT Doctor Check").clicked() {
                                        send(DaemonRequest::ManageSystem {
                                            op: "health".into(),
                                            payload: Value::Null,
                                        });
                                    }
                                    if ui.button("Export Support Bundle").clicked() {
                                        send(DaemonRequest::ManageSystem {
                                            op: "support_bundle".into(),
                                            payload: Value::Null,
                                        });
                                    }
                                    if ui.button("Refresh Local Components").clicked() {
                                        send(DaemonRequest::ManageSystem {
                                            op: "components_list".into(),
                                            payload: Value::Null,
                                        });
                                    }
                                });
                                let update = ui_state.responses["updates"].clone();
                                if !update.is_null() {
                                    ui.monospace(
                                        serde_json::to_string_pretty(&update).unwrap_or_default(),
                                    );
                                    if let Some(url) = update["release_url"].as_str() {
                                        ui.hyperlink_to("Release notes", url);
                                    }
                                    if update["update_available"].as_bool() == Some(true)
                                        && ui.button("Download & Verify Update").clicked()
                                    {
                                        send(DaemonRequest::ManageSystem {
                                            op: "install_update".into(),
                                            payload: serde_json::json!({"confirmed": true}),
                                        });
                                    }
                                }
                                ui.checkbox(
                                    &mut settings_config.ui.auto_updates,
                                    "Automatic updates when idle",
                                );
                                if ui.button("Save Update Preference").clicked() {
                                    send(DaemonRequest::SaveUiSettings {
                                        ui: serde_json::json!({"auto_updates": settings_config.ui.auto_updates}),
                                    });
                                }

                                ui.separator();
                                ui.strong("Local Components (On-Demand Models, Speech & VM)");
                                ui.small("Pinned by SHA-256 and downloaded on demand with resumable verification.");
                                let comp_list = ui_state.responses["components_list"]["components"]
                                    .as_array()
                                    .cloned()
                                    .unwrap_or_default();
                                if comp_list.is_empty() {
                                    ui.small("Click 'Refresh Local Components' to inspect catalog status.");
                                } else {
                                    for comp in &comp_list {
                                        let cid = comp["id"].as_str().unwrap_or("");
                                        let ver = comp["version"].as_str().unwrap_or("");
                                        let status = comp["status"].as_str().unwrap_or("not_installed");
                                        let total_b = comp["total_size_bytes"].as_u64().unwrap_or(0);
                                        let dl_b = comp["bytes_downloaded"].as_u64().unwrap_or(0);
                                        let size_mb = total_b as f64 / (1024.0 * 1024.0);
                                        let summary = comp["check_summary"].as_str().unwrap_or("");
                                        let err = comp["last_error"].as_str().unwrap_or("");
                                        ui.group(|ui| {
                                            ui.horizontal_wrapped(|ui| {
                                                ui.strong(cid);
                                                ui.small(format!("v{ver} · {size_mb:.1} MB"));
                                                let status_color = match status {
                                                    "verified" => palette.status_ok,
                                                    "failed" => palette.status_danger,
                                                    _ => palette.text_secondary,
                                                };
                                                ui.colored_label(status_color, status);
                                                if status != "verified" && status != "downloading" {
                                                    let btn_label = if status == "failed" {
                                                        "Retry"
                                                    } else if status == "installed" {
                                                        "Re-verify"
                                                    } else {
                                                        "Install"
                                                    };
                                                    if ui.small_button(btn_label).clicked() {
                                                        send(DaemonRequest::ManageSystem {
                                                            op: if status == "installed" {
                                                                "components_verify".into()
                                                            } else {
                                                                "components_install".into()
                                                            },
                                                            payload: serde_json::json!({ "id": cid }),
                                                        });
                                                    }
                                                }
                                                if matches!(status, "installed" | "verified")
                                                    && ui.small_button("Verify").clicked()
                                                {
                                                    send(DaemonRequest::ManageSystem {
                                                        op: "components_verify".into(),
                                                        payload: serde_json::json!({ "id": cid }),
                                                    });
                                                }
                                            });
                                            if status == "downloading" && total_b > 0 {
                                                let frac = (dl_b as f32 / total_b as f32).clamp(0.0, 1.0);
                                                ui.add(egui::ProgressBar::new(frac).show_percentage());
                                            }
                                            if !summary.is_empty() {
                                                ui.small(summary);
                                            }
                                            if !err.is_empty() {
                                                ui.colored_label(palette.status_danger, err);
                                            }
                                        });
                                    }
                                }

                                ui.separator();
                                ui.text_edit_multiline(&mut ui_state.feedback_note);
                                ui.checkbox(
                                    &mut ui_state.diagnostics_consent,
                                    "Include redacted diagnostics",
                                );
                                if ui.button("Send Feedback").clicked() {
                                    send(DaemonRequest::ManageSystem {
                                        op: "feedback".into(),
                                        payload: serde_json::json!({
                                            "note": ui_state.feedback_note,
                                            "include_diagnostics": ui_state.diagnostics_consent,
                                        }),
                                    });
                                }
                                for key in ["health", "support_bundle", "feedback", "install_update"] {
                                    if !ui_state.responses[key].is_null() {
                                        ui.monospace(
                                            serde_json::to_string_pretty(&ui_state.responses[key])
                                                .unwrap_or_default(),
                                        );
                                    }
                                }

                                if ui.button("Save Config File").clicked() {
                                    save_config_disk();
                                }
                                if !settings_notice.is_empty() {
                                    ui.small(settings_notice.as_str());
                                }
                            }
                        });
                    });
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
                                        if !bundled && ui.small_button("Delete").clicked() {
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
                                if ui.button("+ New Routine").clicked() {
                                    ui_state.routine_draft = serde_json::json!({
                                        "id": uuid::Uuid::new_v4().to_string(), "name": "New Routine",
                                        "bot_id": active_bot, "channel_id": null, "instruction": "",
                                        "timezone": "UTC", "hour": 9, "minute": 0, "weekdays": [],
                                        "enabled": true,
                                    });
                                    ui_state.active_modal = Some(ActiveModal::RoutineEditor);
                                }
                                for st in routines {
                                    let r = &st["routine"];
                                    let rid = r["id"].as_str().unwrap_or("");
                                    let rname = r["name"].as_str().unwrap_or("Routine");
                                    let bot_id = r["bot_id"].as_str().unwrap_or("muse");
                                    ui.group(|ui| {
                                        ui.horizontal(|ui| {
                                            ui.strong(format!("{rname} (@{bot_id})"));
                                            if ui.small_button("Edit").clicked() {
                                                ui_state.routine_draft = r.clone();
                                                ui_state.active_modal = Some(ActiveModal::RoutineEditor);
                                            }
                                            if ui.small_button("Test Now").clicked() {
                                                send(DaemonRequest::TriggerRoutineTest {
                                                    routine_id: rid.to_string(),
                                                });
                                            }
                                            if ui.small_button("Delete").clicked() {
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
                                            egui::Button::new("+ Create Section"),
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
                                        if ui.small_button("Delete").clicked() {
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
            egui::Window::new("Connect Apps & Custom MCP/Webhook Connectors")
                .open(&mut keep_open)
                .default_width(580.0)
                .show(ctx, |ui| {
                    ui.collapsing("Built-in Integrations", |ui| {
                        if let Some(integrations) = connectors_data["integrations"].as_array() {
                            for app in integrations {
                                let aid = app["id"].as_str().unwrap_or("");
                                ui.group(|ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.strong(app["name"].as_str().unwrap_or("Integration"));
                                        ui.label(app["status"].as_str().unwrap_or("Setup required"));
                                        if !aid.is_empty() && ui.small_button("Probe").clicked() {
                                            send(DaemonRequest::ManageSystem {
                                                op: "connector_probe".into(),
                                                payload: serde_json::json!({ "id": aid }),
                                            });
                                        }
                                    });
                                    ui.small(app["account_summary"].as_str().unwrap_or_default());
                                    ui.small(app["setup_hint"].as_str().unwrap_or("Setup / Connect bridge required"));
                                });
                            }
                        }
                        if !ui_state.responses["connector_probe"].is_null() {
                            ui.monospace(
                                serde_json::to_string_pretty(&ui_state.responses["connector_probe"])
                                    .unwrap_or_default(),
                            );
                        }
                    });
                    ui.collapsing("Marketplace Plugins", |ui| {
                        if ui.small_button("Refresh Marketplace").clicked() {
                            send(DaemonRequest::ManageSystem {
                                op: "marketplace_list".into(),
                                payload: Value::Null,
                            });
                        }
                        let catalog = ui_state.responses["marketplace_list"]["catalog"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default();
                        let installed = ui_state.responses["marketplace_list"]["installed"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default();
                        for item in &catalog {
                            let pid = item["id"].as_str().unwrap_or("");
                            let pname = item["name"].as_str().unwrap_or(pid);
                            let pver = item["version"].as_str().unwrap_or("");
                            let pdesc = item["description"].as_str().unwrap_or("");
                            let scopes: Vec<&str> = item["requested_scopes"]
                                .as_array()
                                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                                .unwrap_or_default();
                            let is_installed = installed
                                .iter()
                                .any(|ins| ins["id"].as_str() == Some(pid));
                            ui.group(|ui| {
                                ui.horizontal_wrapped(|ui| {
                                    ui.strong(pname);
                                    ui.small(format!("v{pver}"));
                                    if is_installed {
                                        ui.colored_label(palette.status_ok, "Installed");
                                        if ui.small_button("Remove").clicked() {
                                            send(DaemonRequest::ManageSystem {
                                                op: "marketplace_remove".into(),
                                                payload: serde_json::json!({ "id": pid }),
                                            });
                                        }
                                    } else if ui.small_button("Confirm Scopes & Install").clicked() {
                                        send(DaemonRequest::ManageSystem {
                                            op: "marketplace_install".into(),
                                            payload: serde_json::json!({
                                                "id": pid,
                                                "confirm_scopes": true,
                                            }),
                                        });
                                    }
                                });
                                ui.small(pdesc);
                                if !scopes.is_empty() {
                                    ui.small(format!("Requested scopes: {}", scopes.join(", ")));
                                }
                            });
                        }
                    });
                    ui.collapsing("Plan & Billing", |ui| {
                        if ui.small_button("Refresh Plan Status").clicked() {
                            send(DaemonRequest::ManageSystem {
                                op: "billing_status".into(),
                                payload: Value::Null,
                            });
                        }
                        if !ui_state.responses["billing_status"].is_null() {
                            let plan = ui_state.responses["billing_status"]["plan"]
                                .as_str()
                                .unwrap_or("free_self_hosted");
                            let byok = ui_state.responses["billing_status"]["byok_allowed"]
                                .as_bool()
                                .unwrap_or(true);
                            ui.label(format!(
                                "Plan: {plan} · BYOK enabled: {}",
                                if byok { "Yes" } else { "No" }
                            ));
                        }
                    });
                    if ui.button("Skills Library & Studio").clicked() { ui_state.studio_tab = "skills".into(); ui_state.active_modal = Some(ActiveModal::Studio); }
                    ui.strong("Add Custom MCP Connector");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut ui_state.connector_kind_draft, "remote_https".into(), "Remote HTTPS");
                        ui.selectable_value(&mut ui_state.connector_kind_draft, "stdio_command".into(), "Stdio Command");
                    });
                    ui.label("Arguments (JSON array)");
                    ui.text_edit_singleline(&mut ui_state.connector_args_draft);
                    ui.label("Account label");
                    ui.text_edit_singleline(&mut ui_state.connector_account_draft);
                    ui.label("Auth header");
                    ui.text_edit_singleline(&mut ui_state.connector_header_draft);
                    ui.add(egui::TextEdit::singleline(&mut *ui_state.connector_secret_draft).password(true).hint_text("Secret (OS Keychain)"));
                    let args = serde_json::from_str::<Vec<String>>(&ui_state.connector_args_draft);
                    if args.is_err() { ui.colored_label(palette.status_danger, "Arguments must be a JSON string array"); }
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
                                args.is_ok() && !ui_state.connector_name_draft.trim().is_empty()
                                    && !ui_state.connector_endpoint_draft.trim().is_empty(),
                                egui::Button::new("+ Add"),
                            )
                            .clicked()
                        {
                            send(DaemonRequest::SaveCustomConnector {
                                op: "upsert".into(),
                                connector: Some(serde_json::json!({
                                    "name": ui_state.connector_name_draft.trim(),
                                    "transport": ui_state.connector_kind_draft,
                                    "url_or_command": ui_state.connector_endpoint_draft.trim(),
                                    "args": args.as_ref().ok(), "bot_ids": [],
                                    "auth_header_name": ui_state.connector_header_draft,
                                    "auth_secret": if ui_state.connector_secret_draft.is_empty() { None } else { Some(ui_state.connector_secret_draft.as_str()) },
                                    "account_label": ui_state.connector_account_draft,
                                    "enabled": true,
                                })),
                                id: None,
                            });
                            use zeroize::Zeroize;
                            ui_state.connector_secret_draft.zeroize();
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
                        let url = c["url_or_command"].as_str().unwrap_or("");
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
                            if ui.small_button("Delete").clicked() {
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
            egui::Window::new(format!("Customize @{bot_id} Shape & Color"))
                .open(&mut keep_open)
                .collapsible(false)
                .default_width(420.0)
                .show(ctx, |ui| {
                    let preview_color = parse_hex_color(&ui_state.avatar_color_draft);
                    let preview_shape = BotAvatarShape::from_name(&ui_state.avatar_shape_draft);
                    ui.horizontal(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(Vec2::new(48.0, 48.0), Sense::hover());
                        let tex = textures.get(&format!("avatar:{bot_id}")).map(|(t, _)| t);
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
                    ui.label("Custom image path (PNG, JPEG, WebP; ≤25 MB)");
                    ui.text_edit_singleline(&mut ui_state.avatar_upload_path);
                    if ui.button("Upload Image").clicked() {
                        let path = std::path::Path::new(&ui_state.avatar_upload_path);
                        let result = (|| -> std::result::Result<(), String> {
                            let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
                            use std::io::Read;
                            let mut bytes = Vec::new();
                            file.take(25 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
                            if bytes.len() > 25 * 1024 * 1024 { return Err("Avatar exceeds 25 MB".into()); }
                            let format = image::guess_format(&bytes).map_err(|e| e.to_string())?;
                            let extension = match format { image::ImageFormat::Png => "png", image::ImageFormat::Jpeg => "jpg", image::ImageFormat::WebP => "webp", _ => return Err("Use PNG, JPEG or WebP".into()) };
                            send(DaemonRequest::ManageSystem { op: "set_bot_avatar".into(), payload: serde_json::json!({"bot_id": bot_id, "extension": extension, "bytes_base64": base64::engine::general_purpose::STANDARD.encode(&bytes)}) });
                            Ok(())
                        })();
                        *settings_notice = result.err().unwrap_or_else(|| "Avatar upload requested".into());
                    }
                    ui.strong("12 Preset GrokBot Colors");
                    ui.horizontal_wrapped(|ui| {
                        for (name, hex, col) in BOT_COLOR_PALETTE {
                            let btn = egui::Button::new(
                                RichText::new(*name).size(11.0).color(Color32::WHITE),
                            )
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
            egui::Window::new("Create Specialist Bot")
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

        ActiveModal::EditChannel => {
            egui::Window::new("Edit Group Chat").open(&mut keep_open).show(ctx,|ui| {
                let draft=&mut ui_state.channel_draft;
                for (key,label) in [("topic","Topic"),("instructions","Instructions")] {
                    ui.label(label);let mut value=draft[key].as_str().unwrap_or_default().to_string();if ui.text_edit_multiline(&mut value).changed(){draft[key]=value.into();}
                }
                for (key,label) in [("paused","Paused"),("archived","Archived")] {let mut flag=draft[key].as_bool().unwrap_or(false);if ui.checkbox(&mut flag,label).changed(){draft[key]=flag.into();}}
                ui.small(format!("Participants: {}",draft["participants"]));
                if ui.button("Save Group").clicked(){send(DaemonRequest::ManageSystem {op:"update_channel".into(),payload:serde_json::json!({"channel_id":draft["id"],"topic":draft["topic"],"instructions":draft["instructions"],"paused":draft["paused"],"archived":draft["archived"]})});close_requested=true;}
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

        ActiveModal::AttachFile => {
            egui::Window::new("Attach Local File")
                .open(&mut keep_open)
                .show(ctx, |ui| {
                    ui.label("Local file path (≤25 MB)");
                    ui.text_edit_singleline(&mut ui_state.attachment_path);
                    if ui.button("Import Attachment").clicked() {
                        send(DaemonRequest::ImportAttachment {
                            bot_id: active_bot.clone(),
                            source_path: std::path::PathBuf::from(ui_state.attachment_path.trim()),
                        });
                        close_requested = true;
                    }
                });
        }
        ActiveModal::RoutineEditor => {
            egui::Window::new("Routine Editor")
                .open(&mut keep_open)
                .default_width(440.0)
                .show(ctx, |ui| {
                    let draft = &mut ui_state.routine_draft;
                    for (key, label) in [
                        ("name", "Name"),
                        ("bot_id", "Bot"),
                        ("timezone", "Timezone (IANA)"),
                        ("instruction", "Instruction"),
                    ] {
                        ui.label(label);
                        let mut text = draft[key].as_str().unwrap_or_default().to_string();
                        let response = if key == "instruction" {
                            ui.text_edit_multiline(&mut text)
                        } else {
                            ui.text_edit_singleline(&mut text)
                        };
                        if response.changed() {
                            draft[key] = Value::String(text);
                        }
                    }
                    ui.horizontal(|ui| {
                        for (key, max) in [("hour", 23), ("minute", 59)] {
                            let mut number = draft[key].as_u64().unwrap_or(0);
                            ui.label(key);
                            if ui
                                .add(egui::DragValue::new(&mut number).range(0..=max))
                                .changed()
                            {
                                draft[key] = number.into();
                            }
                        }
                    });
                    let mut days = draft["weekdays"].as_array().cloned().unwrap_or_default();
                    ui.horizontal_wrapped(|ui| {
                        for (day, label) in ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
                            .into_iter()
                            .enumerate()
                        {
                            let value = Value::from(day);
                            let mut selected = days.contains(&value);
                            if ui.checkbox(&mut selected, label).changed() {
                                days.retain(|d| d != &value);
                                if selected {
                                    days.push(value);
                                }
                            }
                        }
                    });
                    draft["weekdays"] = days.into();
                    ui.small("No weekdays selected means every day.");
                    let mut enabled = draft["enabled"].as_bool().unwrap_or(true);
                    if ui.checkbox(&mut enabled, "Enabled").changed() {
                        draft["enabled"] = enabled.into();
                    }
                    let mut interval = draft["interval_minutes"].as_u64().unwrap_or(0);
                    ui.horizontal(|ui| {
                        ui.label("Interval minutes (0 = daily; otherwise ≥5)");
                        if ui
                            .add(egui::DragValue::new(&mut interval).range(0..=10080))
                            .changed()
                        {
                            draft["interval_minutes"] = if interval == 0 {
                                Value::Null
                            } else {
                                interval.into()
                            };
                        }
                    });
                    let id = draft["id"].as_str().unwrap_or_default().to_string();
                    let port = ui_state.server_snapshot["status"]["companion_port"]
                        .as_u64()
                        .unwrap_or(7433);
                    let hook = format!("http://127.0.0.1:{port}/api/routines/{id}/trigger");
                    ui.label("POST Webhook URL (x-webhook-key)");
                    if ui.button("Copy Webhook URL").clicked() {
                        ctx.copy_text(hook);
                    }
                    if ui.button("Load Webhook Key").clicked() {
                        send(DaemonRequest::ManageSystem {
                            op: "get_routine_webhook_key".into(),
                            payload: serde_json::json!({"routine_id":id}),
                        });
                    }
                    if let Some(secret) = ui_state.revealed_secrets.get(&format!("webhook:{id}"))
                        && ui.button("Copy x-webhook-key").clicked()
                    {
                        ctx.copy_text(secret.clone());
                    }
                    let valid = ["name", "bot_id", "instruction", "timezone"]
                        .iter()
                        .all(|key| draft[key].as_str().is_some_and(|s| !s.trim().is_empty()));
                    if ui
                        .add_enabled(valid, egui::Button::new("Save Routine"))
                        .clicked()
                    {
                        send(DaemonRequest::SaveRoutine {
                            routine: draft.clone(),
                        });
                        close_requested = true;
                    }
                });
        }

        ActiveModal::Lightbox { title, uri, mime } => {
            egui::Window::new(title.to_string())
                .open(&mut keep_open)
                .default_width(640.0)
                .show(ctx, |ui| {
                    ui.small(format!("{mime} · {uri}"));
                    ui.horizontal(|ui| {
                        if ui.button("−").clicked() {
                            ui_state.media_zoom = (ui_state.media_zoom - 0.25).max(0.25);
                        }
                        if ui.button("+").clicked() {
                            ui_state.media_zoom = (ui_state.media_zoom + 0.25).min(4.0);
                        }
                        if ui.button("Reset Zoom").clicked() {
                            ui_state.media_zoom = 1.0;
                        }
                        if ui.button("Copy Media Path").clicked() {
                            ctx.copy_text(uri.clone());
                        }
                    });
                    if let Some((tex_id, [w, h])) = textures.get(&format!("lightbox:{uri}")) {
                        let avail = ui.available_width().max(240.0);
                        let aspect = (h as f32 / (w.max(1) as f32)).clamp(0.3, 1.5);
                        ui.image(egui::load::SizedTexture::new(
                            tex_id,
                            Vec2::new(
                                avail * ui_state.media_zoom,
                                avail * aspect * ui_state.media_zoom,
                            ),
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
            egui::Window::new(title.to_string())
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
        use zeroize::Zeroize;
        ui_state.vault_secret_draft.zeroize();
        ui_state.connector_secret_draft.zeroize();
        ui_state.vault_csv_draft.zeroize();
        for secret in ui_state.revealed_secrets.values_mut() {
            secret.zeroize();
        }
        ui_state.revealed_secrets.clear();
        ui_state.active_modal = None;
    }
}
