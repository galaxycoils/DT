//! Side-by-Side Split Page & Page Canvas (`Tier 3` of the Escape Dismissal Stack):
//! - Markdown Viewer/Editor with CAS SHA-256 optimistic concurrency save
//! - Page capability launcher & HTML source inspector
//! - `"Message about <Title>"` quick action that focuses the Smart Composer with context

use crate::ui::state::{GrokUiState, SplitPageTarget};
use crate::ui::theme::GrokPalette;
use eframe::egui::{self, Align, Layout, RichText, Stroke};
use gm_core::ipc::DaemonRequest;
use serde_json::Value;

#[allow(clippy::too_many_arguments)]
pub fn draw_split_page_panel(
    ctx: &egui::Context,
    ui_state: &mut GrokUiState,
    palette: &GrokPalette,
    draft: &mut String,
    identity_text: &mut String,
    identity_sha256: &Option<String>,
    file_preview: &Value,
    space_html_draft: &mut String,
    mut send: impl FnMut(DaemonRequest),
) {
    let Some(target) = ui_state.split_page.clone() else {
        return;
    };

    let viewport = ctx.content_rect();
    let details_docked = if ui_state.details_drawer_open
        && !ui_state.should_overlay_right_panel(viewport.width(), ui_state.details_width, 0.0)
    {
        ui_state.details_width
    } else {
        0.0
    };
    let use_overlay = ui_state.should_overlay_right_panel(viewport.width(), 420.0, details_docked);
    let draw_inner = |ui: &mut egui::Ui| match target {
        SplitPageTarget::MarkdownFile {
            bot_id,
            file,
            title,
            mut editing,
        } => {
            let is_runtime_file = [
                "SOUL.md",
                "IDENTITY.md",
                "AGENTS.md",
                "HEARTBEAT.md",
                "TOOLS.md",
                "USER.md",
                "MEMORY.md",
                "PROACTIVE_PREFERENCES.md",
            ]
            .contains(&file.as_str());

            ui.horizontal(|ui| {
                ui.strong(RichText::new(title.as_str()).color(palette.text_primary));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .small_button("Close")
                        .on_hover_text("Close Split Page (Esc)")
                        .clicked()
                    {
                        ui_state.split_page = None;
                        return;
                    }
                    if is_runtime_file
                        && ui
                            .small_button(if editing { "Preview" } else { "Edit" })
                            .clicked()
                    {
                        editing = !editing;
                        ui_state.split_page = Some(SplitPageTarget::MarkdownFile {
                            bot_id: bot_id.clone(),
                            file: file.clone(),
                            title: title.clone(),
                            editing,
                        });
                    }
                    if ui.small_button(format!("Message about {file}")).clicked() {
                        if !draft.is_empty() && !draft.ends_with(' ') {
                            draft.push(' ');
                        }
                        draft.push_str(&format!("Regarding `{file}`: "));
                        ctx.memory_mut(|m| m.request_focus(egui::Id::new("main-draft")));
                    }
                });
            });
            ui.separator();

            let body_text = if is_runtime_file {
                identity_text.clone()
            } else {
                file_preview["text"]
                    .as_str()
                    .unwrap_or("Loading file preview…")
                    .to_string()
            };

            egui::ScrollArea::vertical()
                .id_salt("grok_split_md_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if is_runtime_file && editing {
                        ui.add(
                            egui::TextEdit::multiline(identity_text)
                                .desired_rows(22)
                                .desired_width(ui.available_width())
                                .code_editor(),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("Save File (CAS SHA-256)").clicked() {
                                send(DaemonRequest::SaveRuntimeFile {
                                    bot_id: Some(bot_id.clone()),
                                    file: file.clone(),
                                    content: identity_text.clone(),
                                    expected_sha256: identity_sha256.clone(),
                                });
                            }
                            if ui.button("Copy Content").clicked() {
                                ctx.copy_text(identity_text.clone());
                            }
                        });
                    } else {
                        ui.add(egui::Label::new(body_text.as_str()).selectable(true).wrap());
                        ui.add_space(8.0);
                        if ui.small_button("Copy Text").clicked() {
                            ctx.copy_text(body_text);
                        }
                    }
                });
        }

        SplitPageTarget::StaticSpace {
            slug,
            title,
            url,
            mut editing_html,
        } => {
            ui.horizontal(|ui| {
                ui.strong(RichText::new(title.as_str()).color(palette.text_primary));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .small_button("Close")
                        .on_hover_text("Close Space Split Page (Esc)")
                        .clicked()
                    {
                        ui_state.split_page = None;
                        return;
                    }
                    if ui
                        .small_button(if editing_html {
                            "Overview"
                        } else {
                            "Edit HTML"
                        })
                        .clicked()
                    {
                        editing_html = !editing_html;
                        ui_state.split_page = Some(SplitPageTarget::StaticSpace {
                            slug: slug.clone(),
                            title: title.clone(),
                            url: url.clone(),
                            editing_html,
                        });
                    }
                    if ui.small_button(format!("Message about {title}")).clicked() {
                        if !draft.is_empty() && !draft.ends_with(' ') {
                            draft.push(' ');
                        }
                        draft.push_str(&format!("Regarding Space `{slug}` ({title}): "));
                        ctx.memory_mut(|m| m.request_focus(egui::Id::new("main-draft")));
                    }
                });
            });
            ui.separator();

            egui::ScrollArea::vertical()
                .id_salt("grok_split_space_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.small(format!(
                        "Origin-Isolated Page (`{slug}`) · Strict CSP (`script-src 'self'`)"
                    ));
                    if let Some(ref cap_url) = url {
                        ui.horizontal_wrapped(|ui| {
                            ui.hyperlink_to("Open Tokenized Space in Browser ↗", cap_url);
                            if ui.small_button("Copy Capability URL").clicked() {
                                ctx.copy_text(cap_url.clone());
                            }
                        });
                    } else {
                        ui.small("Capability URL unavailable (verify Spaces server is enabled).");
                    }

                    ui.checkbox(&mut ui_state.publish_confirmed, "I confirm publishing this Space");
                    if ui.add_enabled(ui_state.publish_confirmed, egui::Button::new("Publish (Confirmed)")).clicked() { send(DaemonRequest::ManageSpace {op:"publish".into(),slug:slug.clone(),payload:serde_json::json!({"confirmed":true})}); ui_state.publish_confirmed=false; }
                    if ui.button("Unpublish").clicked() { send(DaemonRequest::ManageSpace {op:"unpublish".into(),slug:slug.clone(),payload:serde_json::json!({})}); }
                    ui.add_space(8.0);
                    if editing_html {
                        if ui.button("Load current HTML").clicked() { send(DaemonRequest::ManageSpace {op:"read".into(),slug:slug.clone(),payload:serde_json::json!({"relative_path":"index.html"})}); }
                        if let Some(value) = ui_state.responses.as_object_mut().and_then(|o|o.remove("space_file")) { *space_html_draft=value["content"].as_str().unwrap_or_default().to_string(); }
                        ui.label("Update Static HTML (`index.html`):");
                        ui.add(
                            egui::TextEdit::multiline(space_html_draft)
                                .desired_rows(16)
                                .desired_width(ui.available_width())
                                .code_editor(),
                        );
                        if ui.button("Save Page HTML").clicked() {
                            send(DaemonRequest::ManageSpace { op:"edit".into(), slug:slug.clone(), payload:serde_json::json!({"relative_path":"index.html","content":space_html_draft}) });
                        }
                    } else {
                        ui.group(|ui| {
                            ui.label(
                                "Desktop native mode launches Pages in your system browser via capability-tokenized loopback URL so WebViews never inflate gm-ui RSS.",
                            );
                        });
                    }
                });
        }
    };

    if use_overlay {
        let overlay_w = (viewport.width() - 32.0).clamp(260.0, 440.0);
        let max_h = (viewport.height() - 80.0).max(240.0);
        egui::Area::new(egui::Id::new("grok_split_page_overlay"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-12.0, 56.0))
            .show(ctx, |ui| {
                egui::Frame::window(&ctx.style())
                    .fill(palette.bg_sidebar)
                    .stroke(Stroke::new(1.0_f32, palette.border_subtle))
                    .inner_margin(egui::Margin::symmetric(14, 12))
                    .show(ui, |ui| {
                        ui.set_width(overlay_w);
                        ui.set_max_height(max_h);
                        draw_inner(ui);
                    });
            });
    } else {
        egui::SidePanel::right("grok_split_page_canvas")
            .default_width(420.0)
            .min_width(300.0)
            .max_width(680.0)
            .resizable(true)
            .frame(
                egui::Frame::NONE
                    .fill(palette.bg_sidebar)
                    .stroke(Stroke::new(1.0_f32, palette.border_subtle))
                    .inner_margin(egui::Margin::symmetric(14, 10)),
            )
            .show(ctx, draw_inner);
    }
}
