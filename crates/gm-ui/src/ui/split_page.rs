//! Side-by-Side Split Page & Static Space Canvas (`Tier 3` of the Escape Dismissal Stack):
//! - Markdown Viewer/Editor with CAS SHA-256 optimistic concurrency save
//! - Static Space capability launcher & HTML source inspector
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
        .show(ctx, |ui| match target {
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
                    ui.strong(RichText::new(format!("📄 {title}")).color(palette.text_primary));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .small_button("✕")
                            .on_hover_text("Close Split Page (Esc)")
                            .clicked()
                        {
                            ui_state.split_page = None;
                            return;
                        }
                        if is_runtime_file
                            && ui
                                .small_button(if editing { "👁 Preview" } else { "✎ Edit" })
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
                        if ui
                            .small_button(format!("💬 Message about {file}"))
                            .clicked()
                        {
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
                            if ui.small_button("⎘ Copy Text").clicked() {
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
                    ui.strong(RichText::new(format!("🧩 {title}")).color(palette.text_primary));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .small_button("✕")
                            .on_hover_text("Close Space Split Page (Esc)")
                            .clicked()
                        {
                            ui_state.split_page = None;
                            return;
                        }
                        if ui
                            .small_button(if editing_html {
                                "👁 Overview"
                            } else {
                                "✎ Edit HTML"
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
                        if ui
                            .small_button(format!("💬 Message about {title}"))
                            .clicked()
                        {
                            if !draft.is_empty() && !draft.ends_with(' ') {
                                draft.push(' ');
                            }
                            draft.push_str(&format!("Regarding Space `{slug}` ({title}): "));
                            ctx.memory_mut(|m| m.request_focus(egui::Id::new("main-draft")));
                        }
                    });
                });
                ui.separator();

                ui.small(format!(
                    "Origin-Isolated Static Space (`{slug}`) · Strict CSP (`script-src 'self'`)"
                ));
                if let Some(ref cap_url) = url {
                    ui.horizontal_wrapped(|ui| {
                        ui.hyperlink_to("🚀 Open Tokenized Space in Browser ↗", cap_url);
                        if ui.small_button("⎘ Copy Capability URL").clicked() {
                            ctx.copy_text(cap_url.clone());
                        }
                    });
                } else {
                    ui.small("Capability URL unavailable (verify Spaces server is enabled).");
                }

                ui.add_space(8.0);
                if editing_html {
                    ui.label("Update Static HTML (`index.html`):");
                    ui.add(
                        egui::TextEdit::multiline(space_html_draft)
                            .desired_rows(16)
                            .desired_width(ui.available_width())
                            .code_editor(),
                    );
                    if ui.button("Save Static Space HTML").clicked() {
                        send(DaemonRequest::CreateSpace {
                            slug: slug.clone(),
                            title: Some(title.clone()),
                            kind: Some("static".into()),
                            html: Some(space_html_draft.clone()),
                        });
                    }
                } else {
                    ui.group(|ui| {
                        ui.label(
                            "Desktop native mode launches Static Spaces in your system browser via capability-tokenized loopback URL so WebViews never inflate gm-ui RSS.",
                        );
                    });
                }
            }
        });
}
