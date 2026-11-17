//! 340px 4-Tab `Bot Details` Drawer (`Cmd/Ctrl+Alt+B`):
//! 1. `Settings`: Name, Role, System Prompt, Model/Provider, Tool Toggles (`allow_gui`, `allow_shell`, `allow_network`), Avatar Picker
//! 2. `Computer`: Live Computer Preview (`2–5 FPS` only while visible, texture freed immediately on close) + `Teach a Task` recorder
//! 3. `Memory`: All 8 Muse runtime files (`SOUL.md`, `IDENTITY.md`, `AGENTS.md`, `HEARTBEAT.md`, `TOOLS.md`, `USER.md`, `MEMORY.md`, `PROACTIVE_PREFERENCES.md`) + Portable Memory Importer + Two-Phase Forget + Discretion
//! 4. `Library`: Filter pills (`All`, `Pinned`, `Outputs`, `Uploads`) + 5-item pagination (`limit = 5`) + Pin/Unpin + Open in Split Page

use crate::ui::cache::BoundedTextureCache;
use crate::ui::state::{ActiveModal, BotDetailsTab, GrokUiState, SplitPageTarget};
use crate::ui::theme::GrokPalette;
use eframe::egui::{self, CornerRadius, RichText, Stroke, Vec2};
use gm_core::ipc::DaemonRequest;
use serde_json::Value;

pub const RUNTIME_IDENTITY_FILES: [&str; 8] = [
    "SOUL.md",
    "IDENTITY.md",
    "AGENTS.md",
    "HEARTBEAT.md",
    "TOOLS.md",
    "USER.md",
    "MEMORY.md",
    "PROACTIVE_PREFERENCES.md",
];

#[allow(clippy::too_many_arguments)]
pub fn draw_bot_details_drawer(
    ctx: &egui::Context,
    ui_state: &mut GrokUiState,
    palette: &GrokPalette,
    textures: &mut BoundedTextureCache,
    bots: &[Value],
    active_bot: &str,
    identity_file: &mut String,
    identity_text: &mut String,
    identity_sha256: &Option<String>,
    identity_notice: &str,
    memory_import_text: &mut String,
    memory_import_report: &Option<Value>,
    forget_request: &mut String,
    forget_plan: &Option<Value>,
    forget_notice: &str,
    library_page: &Value,
    computer_frame: &Option<(u32, u32, u64)>,
    mut send: impl FnMut(DaemonRequest),
) {
    if !ui_state.details_drawer_open {
        return;
    }

    egui::SidePanel::right("grok_bot_details_drawer")
        .exact_width(340.0)
        .resizable(false)
        .frame(
            egui::Frame::NONE
                .fill(palette.bg_sidebar)
                .stroke(Stroke::new(1.0_f32, palette.border_subtle))
                .inner_margin(egui::Margin::symmetric(12, 10)),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.strong(RichText::new(format!("@{active_bot} Details")).color(palette.text_primary));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("✕")
                        .on_hover_text("Close Bot Details (Esc / Cmd+Alt+B)")
                        .clicked()
                    {
                        ui_state.details_drawer_open = false;
                        if !ui_state.live_computer_open && !ui_state.live_computer_fullscreen {
                            textures.evict_prefix("live_computer:");
                        }
                    }
                });
            });
            ui.add_space(6.0);

            // 4-Tab Selector
            ui.horizontal(|ui| {
                for tab in BotDetailsTab::ALL {
                    let active = ui_state.details_tab == tab;
                    let btn = egui::Button::new(RichText::new(tab.label()).size(12.0).color(
                        if active {
                            palette.accent_text
                        } else {
                            palette.text_secondary
                        },
                    ))
                    .fill(if active {
                        palette.accent
                    } else {
                        palette.bg_elevated
                    })
                    .corner_radius(CornerRadius::same(6));
                    if ui.add(btn).clicked() {
                        let prev_tab = ui_state.details_tab;
                        ui_state.details_tab = tab;
                        if prev_tab == BotDetailsTab::Computer
                            && tab != BotDetailsTab::Computer
                            && !ui_state.live_computer_open
                        {
                            textures.evict_prefix("live_computer:");
                        }
                        match tab {
                            BotDetailsTab::Computer => {
                                send(DaemonRequest::CaptureComputerPreview {
                                    max_long_edge: Some(1280),
                                    quality: Some(75),
                                });
                            }
                            BotDetailsTab::Memory => {
                                send(DaemonRequest::ReadRuntimeFile {
                                    bot_id: Some(active_bot.to_string()),
                                    file: identity_file.clone(),
                                });
                            }
                            BotDetailsTab::Library => {
                                send(DaemonRequest::ListLibraryPage {
                                    bot_id: active_bot.to_string(),
                                    filter: ui_state.library_filter.clone(),
                                    cursor: Some(ui_state.library_cursor),
                                    limit: Some(5),
                                });
                            }
                            BotDetailsTab::Settings => {}
                        }
                    }
                }
            });
            ui.separator();

            egui::ScrollArea::vertical()
                .id_salt("grok_details_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| match ui_state.details_tab {
                    BotDetailsTab::Settings => {
                        if let Some(bot) = bots.iter().find(|b| b["id"].as_str() == Some(active_bot))
                        {
                            let mut draft_profile = bot.clone();
                            let mut changed = false;
                            ui.label("Display Name");
                            let mut name = draft_profile["display_name"]
                                .as_str()
                                .unwrap_or(active_bot)
                                .to_string();
                            if ui.text_edit_singleline(&mut name).changed() {
                                draft_profile["display_name"] = Value::String(name);
                                changed = true;
                            }
                            ui.label("Role Title");
                            let mut role = draft_profile["role_title"]
                                .as_str()
                                .unwrap_or("")
                                .to_string();
                            if ui.text_edit_singleline(&mut role).changed() {
                                draft_profile["role_title"] = Value::String(role);
                                changed = true;
                            }
                            ui.label("System Instructions");
                            let mut prompt = draft_profile["system_prompt"]
                                .as_str()
                                .unwrap_or("")
                                .to_string();
                            if ui
                                .add(
                                    egui::TextEdit::multiline(&mut prompt)
                                        .desired_rows(5)
                                        .desired_width(ui.available_width()),
                                )
                                .changed()
                            {
                                draft_profile["system_prompt"] = Value::String(prompt);
                                changed = true;
                            }
                            for (key, label) in [
                                ("allow_gui", "Allow Desktop GUI Control"),
                                ("allow_shell", "Allow Workspace VM Shell"),
                                ("allow_network", "Allow Outbound Network Tools"),
                            ] {
                                let mut flag = draft_profile[key].as_bool().unwrap_or(false);
                                if ui.checkbox(&mut flag, label).changed() {
                                    draft_profile[key] = Value::Bool(flag);
                                    changed = true;
                                }
                            }
                            if changed {
                                send(DaemonRequest::SaveBot {
                                    profile: draft_profile,
                                });
                            }
                            ui.add_space(6.0);
                            if ui.button("🎨 Customize Avatar, Shape & Color…").clicked() {
                                ui_state.open_avatar_picker(active_bot, bot);
                            }
                        }
                    }

                    BotDetailsTab::Computer => {
                        ui.horizontal(|ui| {
                            ui.strong("Live Computer Preview");
                            if ui.small_button("↻ Capture Frame").clicked() {
                                send(DaemonRequest::CaptureComputerPreview {
                                    max_long_edge: Some(1280),
                                    quality: Some(75),
                                });
                            }
                        });
                        ui.small("Adaptive 2–5 FPS while visible · 0% CPU/GPU when closed");
                        if let Some((tex_id, [w, h])) = textures.get("live_computer:latest") {
                            let avail = ui.available_width().max(180.0);
                            let aspect = (h as f32 / (w.max(1) as f32)).clamp(0.4, 1.2);
                            let size = Vec2::new(avail, avail * aspect);
                            ui.image(egui::load::SizedTexture::new(tex_id, size));
                            if let Some((fw, fh, ts)) = computer_frame {
                                ui.small(format!("{fw}×{fh} · frame ts {ts}"));
                            }
                        } else {
                            ui.group(|ui| {
                                ui.small(
                                    "No live frame captured yet. Ensure Host Desktop opt-in is enabled in Settings.",
                                );
                            });
                        }

                        ui.separator();
                        ui.strong("🎓 Teach a Task");
                        ui.small(
                            "Demonstrate steps to compile a reusable SKILL.md (all steps pass through SecretScrubber).",
                        );
                        if !ui_state.teach_active {
                            if ui.button("▶ Start Demonstration").clicked() {
                                ui_state.teach_active = true;
                                ui_state.teach_steps_recorded.clear();
                                send(DaemonRequest::StartTeachTask {
                                    bot_id: active_bot.to_string(),
                                });
                            }
                        } else {
                            ui.colored_label(
                                palette.status_warn,
                                format!(
                                    "Recording ({} steps captured)",
                                    ui_state.teach_steps_recorded.len()
                                ),
                            );
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut ui_state.teach_step_draft)
                                        .desired_width(190.0)
                                        .hint_text("Describe action step…"),
                                );
                                if ui
                                    .add_enabled(
                                        !ui_state.teach_step_draft.trim().is_empty(),
                                        egui::Button::new("＋ Step"),
                                    )
                                    .clicked()
                                {
                                    let step = ui_state.teach_step_draft.trim().to_string();
                                    ui_state.teach_steps_recorded.push(step.clone());
                                    send(DaemonRequest::ManageSystem {
                                        op: "record_teach_step".into(),
                                        payload: serde_json::json!({ "step": step }),
                                    });
                                    ui_state.teach_step_draft.clear();
                                }
                            });
                            ui.add(
                                egui::TextEdit::singleline(&mut ui_state.teach_skill_name_draft)
                                    .desired_width(ui.available_width())
                                    .hint_text("Skill slug (e.g. weekly-report)"),
                            );
                            if ui.button("⏹ Stop & Compile SKILL.md").clicked() {
                                ui_state.teach_active = false;
                                let skill_name = if ui_state.teach_skill_name_draft.trim().is_empty()
                                {
                                    None
                                } else {
                                    Some(ui_state.teach_skill_name_draft.trim().to_string())
                                };
                                send(DaemonRequest::StopTeachTask {
                                    save_skill_name: skill_name,
                                });
                            }
                        }
                    }

                    BotDetailsTab::Memory => {
                        ui.horizontal_wrapped(|ui| {
                            let prev = identity_file.clone();
                            egui::ComboBox::from_id_salt("drawer_identity_file")
                                .selected_text(identity_file.as_str())
                                .show_ui(ui, |ui| {
                                    for name in RUNTIME_IDENTITY_FILES {
                                        ui.selectable_value(identity_file, name.into(), name);
                                    }
                                });
                            if *identity_file != prev {
                                send(DaemonRequest::ReadRuntimeFile {
                                    bot_id: Some(active_bot.to_string()),
                                    file: identity_file.clone(),
                                });
                            }
                            if ui.small_button("Open Split ◧").clicked() {
                                ui_state.split_page = Some(SplitPageTarget::MarkdownFile {
                                    bot_id: active_bot.to_string(),
                                    file: identity_file.clone(),
                                    title: format!("@{active_bot} / {identity_file}"),
                                    editing: true,
                                });
                            }
                        });
                        if !identity_notice.is_empty() {
                            ui.small(identity_notice);
                        }
                        ui.add(
                            egui::TextEdit::multiline(identity_text)
                                .desired_rows(7)
                                .desired_width(ui.available_width()),
                        );
                        if ui.button("Save Runtime File (CAS SHA-256)").clicked() {
                            send(DaemonRequest::SaveRuntimeFile {
                                bot_id: Some(active_bot.to_string()),
                                file: identity_file.clone(),
                                content: identity_text.clone(),
                                expected_sha256: identity_sha256.clone(),
                            });
                        }

                        ui.separator();
                        ui.collapsing("Portable Memory Importer", |ui| {
                            ui.add(
                                egui::TextEdit::multiline(memory_import_text)
                                    .desired_rows(3)
                                    .desired_width(ui.available_width())
                                    .hint_text("Paste bullet points…"),
                            );
                            ui.horizontal(|ui| {
                                if ui.small_button("Dry-Run Preview").clicked() {
                                    send(DaemonRequest::ImportMemory {
                                        input: memory_import_text.clone(),
                                        apply: false,
                                        report_sha256: None,
                                    });
                                }
                                if let Some(rep) = memory_import_report
                                    && let Some(sha) = rep["report_sha256"].as_str()
                                    && ui.small_button("Apply Verified").clicked()
                                {
                                    send(DaemonRequest::ImportMemory {
                                        input: memory_import_text.clone(),
                                        apply: true,
                                        report_sha256: Some(sha.to_string()),
                                    });
                                }
                            });
                        });

                        ui.collapsing("Two-Phase Forget", |ui| {
                            ui.add(
                                egui::TextEdit::singleline(forget_request)
                                    .desired_width(ui.available_width())
                                    .hint_text("Topic or phrase to erase…"),
                            );
                            if ui.small_button("Plan Erasure").clicked()
                                && !forget_request.trim().is_empty()
                            {
                                send(DaemonRequest::ForgetPlan {
                                    bot_id: Some(active_bot.to_string()),
                                    request: forget_request.trim().to_string(),
                                });
                            }
                            if let Some(plan) = forget_plan
                                && let Some(plan_id) = plan["plan_id"].as_str()
                            {
                                ui.small(format!(
                                    "Plan {plan_id} ready ({})",
                                    plan["summary_counts"]
                                ));
                                if ui
                                    .small_button(
                                        RichText::new("Confirm Erase").color(palette.status_danger),
                                    )
                                    .clicked()
                                {
                                    send(DaemonRequest::ForgetConfirm {
                                        bot_id: Some(active_bot.to_string()),
                                        plan_id: plan_id.to_string(),
                                        verification_query: None,
                                        confirmed: true,
                                    });
                                }
                            }
                            if !forget_notice.is_empty() {
                                ui.small(forget_notice);
                            }
                        });
                    }

                    BotDetailsTab::Library => {
                        ui.horizontal(|ui| {
                            for (flt, lbl) in [
                                ("all", "All"),
                                ("pinned", "Pinned"),
                                ("outputs", "Outputs"),
                                ("uploads", "Uploads"),
                            ] {
                                let active = ui_state.library_filter == flt;
                                if ui.selectable_label(active, lbl).clicked() {
                                    ui_state.library_filter = flt.to_string();
                                    ui_state.library_cursor = 0;
                                    send(DaemonRequest::ListLibraryPage {
                                        bot_id: active_bot.to_string(),
                                        filter: flt.to_string(),
                                        cursor: Some(0),
                                        limit: Some(5),
                                    });
                                }
                            }
                        });
                        ui.add_space(4.0);
                        let items = library_page["items"].as_array().cloned().unwrap_or_default();
                        let total = library_page["total_count"]
                            .as_u64()
                            .or_else(|| library_page["total"].as_u64())
                            .unwrap_or(0);
                        ui.small(format!("{total} items (showing 5 per page)"));
                        for item in &items {
                            let item_id = item["id"].as_str().unwrap_or("");
                            let title = item["title"]
                                .as_str()
                                .or_else(|| item["uri_or_path"].as_str())
                                .unwrap_or("Item");
                            let path = item["uri_or_path"]
                                .as_str()
                                .or_else(|| item["relative_path"].as_str())
                                .or_else(|| item["path"].as_str())
                                .unwrap_or("");
                            let pinned = item["pinned"].as_bool().unwrap_or(false);
                            let kind = item["kind"].as_str().unwrap_or("");
                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    ui.small(if pinned {
                                        format!("📌 {title}")
                                    } else {
                                        title.to_string()
                                    });
                                    if !item_id.is_empty()
                                        && ui.small_button(if pinned { "Unpin" } else { "Pin" }).clicked()
                                    {
                                        send(DaemonRequest::ManageSystem {
                                            op: "pin_library_item".into(),
                                            payload: serde_json::json!({
                                                "bot_id": active_bot,
                                                "id": item_id,
                                                "pinned": !pinned,
                                            }),
                                        });
                                    }
                                    if !path.is_empty()
                                        && matches!(kind, "file" | "upload" | "output" | "shared_file" | "")
                                        && ui.small_button("Open ◧").clicked()
                                    {
                                        send(DaemonRequest::PreviewBotFile {
                                            bot_id: active_bot.to_string(),
                                            path: path.to_string(),
                                        });
                                        ui_state.split_page = Some(SplitPageTarget::MarkdownFile {
                                            bot_id: active_bot.to_string(),
                                            file: path.to_string(),
                                            title: title.to_string(),
                                            editing: false,
                                        });
                                    }
                                });
                            });
                        }
                        ui.horizontal(|ui| {
                            if ui_state.library_cursor >= 5 && ui.small_button("← Prev 5").clicked()
                            {
                                ui_state.library_cursor = ui_state.library_cursor.saturating_sub(5);
                                send(DaemonRequest::ListLibraryPage {
                                    bot_id: active_bot.to_string(),
                                    filter: ui_state.library_filter.clone(),
                                    cursor: Some(ui_state.library_cursor),
                                    limit: Some(5),
                                });
                            }
                            if let Some(next) = library_page["next_cursor"].as_u64()
                                && ui.small_button("Next 5 →").clicked()
                            {
                                ui_state.library_cursor = next as usize;
                                send(DaemonRequest::ListLibraryPage {
                                    bot_id: active_bot.to_string(),
                                    filter: ui_state.library_filter.clone(),
                                    cursor: Some(ui_state.library_cursor),
                                    limit: Some(5),
                                });
                            }
                        });
                    }
                });
        });
}

impl GrokUiState {
    pub fn open_avatar_picker(&mut self, bot_id: &str, bot: &Value) {
        self.avatar_shape_draft = bot["avatar_shape"]
            .as_str()
            .unwrap_or("Circle")
            .to_string();
        self.avatar_color_draft = bot["avatar_color"]
            .as_str()
            .or_else(|| bot["color"].as_str())
            .unwrap_or("#2F7CF6")
            .to_string();
        self.active_modal = Some(ActiveModal::AvatarPicker {
            bot_id: bot_id.to_string(),
        });
    }
}
