//! Full-Canvas Workspace Views for `Feed`, `Goals`, `Spaces`, and `Audit`
//! when selected from the 48px Left Workspace Rail.

use crate::ui::state::{GrokUiState, SplitPageTarget, WorkspaceSection};
use crate::ui::theme::GrokPalette;
use eframe::egui::{self, Align, Layout, RichText};
use gm_core::ipc::{DaemonRequest, QueueControlAction};
use serde_json::Value;

#[allow(clippy::too_many_arguments)]
pub fn draw_workspace_section_view(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    ui_state: &mut GrokUiState,
    palette: &GrokPalette,
    activity_posts: &[Value],
    brief_prompt: &Option<String>,
    activity_notice: &str,
    goals: &[Value],
    goal_title: &mut String,
    goal_description: &mut String,
    goal_category: &mut String,
    goal_parent: &mut String,
    goal_notice: &str,
    spaces_data: &Value,
    space_slug: &mut String,
    space_title: &mut String,
    space_html: &mut String,
    spaces_notice: &str,
    tasks: &[Value],
    resume_note: &mut String,
    draft: &mut String,
    mut send: impl FnMut(DaemonRequest),
) {
    match ui_state.section {
        WorkspaceSection::Bots => {}

        WorkspaceSection::Feed => {
            ui.horizontal(|ui| {
                ui.heading(
                    RichText::new("⚡ Proactive Activity & Digest Feed")
                        .color(palette.text_primary),
                );
                if ui.button("↻ Refresh").clicked() {
                    send(DaemonRequest::ListActivity);
                }
                if !activity_notice.is_empty() {
                    ui.small(activity_notice);
                }
            });
            ui.collapsing("Brief Prompt & Manual Post",|ui| {
                ui.text_edit_multiline(&mut ui_state.feed_prompt);
                if ui.button("Save brief prompt").clicked(){send(DaemonRequest::ManageFeed {op:"prompt".into(),payload:serde_json::json!({"prompt":ui_state.feed_prompt})});}
                ui.text_edit_singleline(&mut ui_state.feed_post_title);ui.text_edit_multiline(&mut ui_state.feed_post_body);
                if ui.button("Post to Feed").clicked(){send(DaemonRequest::ManageFeed {op:"post".into(),payload:serde_json::json!({"title":ui_state.feed_post_title,"body":ui_state.feed_post_body})});}
            });
            ui.collapsing("Ideas",|ui| {
                ui.text_edit_multiline(&mut ui_state.idea_prompt);
                if ui.button("Create Idea").clicked(){send(DaemonRequest::ManageFeed {op:"create_idea".into(),payload:serde_json::json!({"title":ui_state.idea_prompt,"action_prompt":ui_state.idea_prompt})});}
                let ideas=ui_state.server_snapshot["feed"]["ideas"].as_array().cloned().unwrap_or_default();
                for idea in ideas{ui.horizontal_wrapped(|ui|{ui.label(idea["title"].as_str().unwrap_or("Idea"));for (label,op) in [("Run","run_idea"),("Dismiss","dismiss_idea")]{if ui.button(label).clicked(){send(DaemonRequest::ManageFeed {op:op.into(),payload:serde_json::json!({"id":idea["id"]})});}}});}
            });
            if let Some(prompt) = brief_prompt
                && !prompt.trim().is_empty()
            {
                ui.small(format!("Morning/Evening Brief Focus: {prompt}"));
            }
            ui.separator();

            egui::ScrollArea::vertical()
                .id_salt("grok_feed_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if activity_posts.is_empty() {
                        ui.label("No proactive activity posts yet.");
                        return;
                    }
                    for post in activity_posts {
                        let id = post["id"].as_str().unwrap_or("").to_string();
                        let category = post["category"].as_str().unwrap_or("Observation");
                        let kicker = post["kicker"].as_str().unwrap_or("");
                        let title = post["title"].as_str().unwrap_or("Update");
                        let body = post["body"].as_str().unwrap_or("");
                        ui.push_id(&id, |ui| {
                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    ui.small(if kicker.is_empty() {
                                        category.to_string()
                                    } else {
                                        format!("{category} · {kicker}")
                                    });
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if !id.is_empty() && ui.small_button("🗑").clicked() {
                                            send(DaemonRequest::ManageFeed {
                                                op: "delete".into(),
                                                payload: serde_json::json!({ "id": id }),
                                            });
                                        }
                                        if !id.is_empty() && ui.small_button("👎").clicked() {
                                            send(DaemonRequest::RateActivity {
                                                id: id.clone(),
                                                rating: -1,
                                            });
                                        }
                                        if !id.is_empty() && ui.small_button("👍").clicked() {
                                            send(DaemonRequest::RateActivity {
                                                id: id.clone(),
                                                rating: 5,
                                            });
                                        }
                                        if ui.small_button("💬 Discuss in Chat").clicked() {
                                            *draft = format!("Regarding Feed update \"{title}\": ");
                                            ui_state.section = WorkspaceSection::Bots;
                                        }
                                    });
                                });
                                ui.strong(title);
                                ui.add(egui::Label::new(body).selectable(true).wrap());
                            });
                        });
                        ui.add_space(6.0);
                    }
                });
        }

        WorkspaceSection::Goals => {
            ui.horizontal(|ui| {
                ui.heading(
                    RichText::new("🎯 Hierarchical Goals Workspace").color(palette.text_primary),
                );
                if ui.button("↻ Refresh").clicked() {
                    send(DaemonRequest::ListGoals);
                }
                if !goal_notice.is_empty() {
                    ui.small(goal_notice);
                }
            });
            ui.separator();

            ui.group(|ui| {
                ui.strong("Create Goal or Subgoal");
                ui.horizontal(|ui| {
                    ui.label("Title:");
                    ui.add(
                        egui::TextEdit::singleline(goal_title)
                            .desired_width(220.0)
                            .hint_text("Launch v0.5.0"),
                    );
                    ui.label("Category:");
                    ui.add(
                        egui::TextEdit::singleline(goal_category)
                            .desired_width(120.0)
                            .hint_text("Engineering"),
                    );
                    ui.label("Parent Slug:");
                    ui.add(
                        egui::TextEdit::singleline(goal_parent)
                            .desired_width(140.0)
                            .hint_text("optional"),
                    );
                });
                ui.add(
                    egui::TextEdit::multiline(goal_description)
                        .desired_rows(2)
                        .desired_width(ui.available_width())
                        .hint_text("Goal description & acceptance criteria…"),
                );
                if ui
                    .add_enabled(!goal_title.trim().is_empty(), egui::Button::new("＋ Create Goal"))
                    .clicked()
                {
                    let req = if !goal_parent.trim().is_empty() {
                        DaemonRequest::SaveGoal {
                            op: "create_subgoal".into(),
                            slug: None,
                            payload: serde_json::json!({
                                "parent_slug": goal_parent.trim(),
                                "title": goal_title.trim(),
                                "description": goal_description.trim(),
                            }),
                        }
                    } else {
                        DaemonRequest::SaveGoal {
                            op: "create".into(),
                            slug: None,
                            payload: serde_json::json!({
                                "title": goal_title.trim(),
                                "description": goal_description.trim(),
                                "category": if goal_category.trim().is_empty() { None } else { Some(goal_category.trim()) },
                            }),
                        }
                    };
                    send(req);
                    goal_title.clear();
                    goal_description.clear();
                    goal_category.clear();
                    goal_parent.clear();
                }
            });
            ui.add_space(8.0);

            egui::ScrollArea::vertical()
                .id_salt("grok_goals_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for entry in goals {
                        let g = entry.get("goal").unwrap_or(entry);
                        let slug = g["slug"].as_str().unwrap_or("").to_string();
                        let title = g["title"].as_str().unwrap_or(&slug);
                        let status = g["status"].as_str().unwrap_or("Active");
                        let desc = g["description"].as_str().unwrap_or("");
                        let is_sub = g["parent_goal_id"].as_str().is_some();
                        ui.push_id(&slug, |ui| {
                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    ui.strong(format!(
                                        "{}{title}",
                                        if is_sub { "↳ " } else { "" }
                                    ));
                                    ui.small(format!("({slug}) · {status}"));
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if ui.small_button("🗑 Delete").clicked() {
                                            send(DaemonRequest::SaveGoal {
                                                op: "delete".into(),
                                                slug: Some(slug.clone()),
                                                payload: Value::Null,
                                            });
                                        }
                                        if status == "Active" {
                                            if ui.small_button("✓ Complete").clicked() {
                                                send(DaemonRequest::SaveGoal {
                                                    op: "complete".into(),
                                                    slug: Some(slug.clone()),
                                                    payload: serde_json::json!({ "verified": true }),
                                                });
                                            }
                                            if !is_sub && ui.small_button("＋ Subgoal").clicked() {
                                                *goal_parent = slug.clone();
                                            }
                                        }
                                    });
                                });
                                if !desc.is_empty() {
                                    ui.small(desc);
                                }
                            });
                        });
                        ui.add_space(4.0);
                    }
                });
        }

        WorkspaceSection::Spaces => {
            ui.horizontal(|ui| {
                ui.heading(RichText::new("🧩 Static Spaces Library").color(palette.text_primary));
                if ui.button("↻ Refresh").clicked() {
                    send(DaemonRequest::ListSpaces);
                }
                if !spaces_notice.is_empty() {
                    ui.small(spaces_notice);
                }
            });
            ui.separator();

            let enabled = spaces_data["enable_spaces"].as_bool().unwrap_or(false);
            let port = spaces_data["spaces_port"].as_u64().unwrap_or(0);
            ui.small(if enabled {
                format!("Origin-isolated Static Spaces server active on loopback port {port}")
            } else {
                "Static Spaces server disabled in config".into()
            });

            ui.group(|ui| {
                ui.strong("Create Static Space");
                ui.horizontal(|ui| {
                    ui.label("Slug:");
                    ui.add(
                        egui::TextEdit::singleline(space_slug)
                            .desired_width(150.0)
                            .hint_text("team-dashboard"),
                    );
                    ui.label("Title:");
                    ui.add(
                        egui::TextEdit::singleline(space_title)
                            .desired_width(180.0)
                            .hint_text("Team Dashboard"),
                    );
                    if ui
                        .add_enabled(
                            enabled && !space_slug.trim().is_empty(),
                            egui::Button::new("＋ Create Space"),
                        )
                        .clicked()
                    {
                        send(DaemonRequest::CreateSpace {
                            slug: space_slug.trim().to_string(),
                            title: if space_title.trim().is_empty() {
                                None
                            } else {
                                Some(space_title.trim().to_string())
                            },
                            kind: Some("static".into()),
                            html: if space_html.trim().is_empty() {
                                None
                            } else {
                                Some(space_html.trim().to_string())
                            },
                        });
                        space_slug.clear();
                        space_title.clear();
                        space_html.clear();
                    }
                });
            });
            ui.add_space(8.0);

            let spaces = spaces_data["spaces"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            egui::ScrollArea::vertical()
                .id_salt("grok_spaces_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for sp in &spaces {
                        let slug = sp["slug"].as_str().unwrap_or("").to_string();
                        let title = sp["title"].as_str().unwrap_or(&slug).to_string();
                        let published = sp["published"].as_bool().unwrap_or(false);
                        let cap_url = spaces_data["urls"][&slug].as_str().map(ToString::to_string);
                        ui.push_id(&slug, |ui| {
                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    ui.strong(format!("{title} ({slug})"));
                                    ui.small(if published { "published" } else { "local" });
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if ui.small_button("🗑 Delete").clicked() {
                                            send(DaemonRequest::ManageSpace {
                                                op: "delete".into(),
                                                slug: slug.clone(),
                                                payload: Value::Null,
                                            });
                                        }
                                        if published && ui.small_button("Unpublish").clicked() {
                                            send(DaemonRequest::ManageSpace {
                                                op: "unpublish".into(),
                                                slug: slug.clone(),
                                                payload: Value::Null,
                                            });
                                        }
                                        if ui.small_button("Open Split Page ◧").clicked() {
                                            ui_state.split_page =
                                                Some(SplitPageTarget::StaticSpace {
                                                    slug: slug.clone(),
                                                    title: title.clone(),
                                                    url: cap_url.clone(),
                                                    editing_html: false,
                                                });
                                            ui_state.section = WorkspaceSection::Bots;
                                        }
                                        if let Some(ref u) = cap_url {
                                            ui.hyperlink_to("Browser ↗", u);
                                            if ui.small_button("Copy URL").clicked() {
                                                ctx.copy_text(u.clone());
                                            }
                                        }
                                    });
                                });
                            });
                        });
                        ui.add_space(4.0);
                    }
                });
        }

        WorkspaceSection::Audit => {
            ui.collapsing("Governance Audit", |ui| {
                if ui.button("Refresh Audit").clicked() {
                    send(DaemonRequest::QueryAudit {
                        category: None,
                        bot_id: None,
                        limit: Some(100),
                    });
                }
                if !ui_state.responses["audit"].is_null() {
                    ui.monospace(
                        serde_json::to_string_pretty(&ui_state.responses["audit"])
                            .unwrap_or_default(),
                    );
                }
            });
            ui.horizontal(|ui| {
                ui.heading(
                    RichText::new("🛡 Durable Task Queue & Governance Audit")
                        .color(palette.text_primary),
                );
                if ui.button("↻ Refresh Queue").clicked() {
                    send(DaemonRequest::ListQueue { bot_id: None });
                }
            });
            ui.separator();

            egui::ScrollArea::vertical()
                .id_salt("grok_audit_tasks_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if tasks.is_empty() {
                        ui.label("No queued or historical tasks yet.");
                        return;
                    }
                    for task in tasks.iter().rev().take(100) {
                        let id = task["id"].as_str().unwrap_or("").to_string();
                        let bot_id = task["bot_id"].as_str().unwrap_or("muse");
                        let state = task["status"].as_str().unwrap_or("unknown");
                        let instruction = task["instruction"].as_str().unwrap_or("");
                        ui.push_id(&id, |ui| {
                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    ui.strong(format!("@{bot_id} · {state}"));
                                    ui.small(&id);
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if matches!(
                                            state,
                                            "queued"
                                                | "running"
                                                | "paused"
                                                | "waiting_for_human"
                                                | "pause_requested"
                                        ) && ui.small_button("Cancel").clicked()
                                        {
                                            send(DaemonRequest::QueueControl {
                                                task_id: id.clone(),
                                                action: QueueControlAction::Cancel,
                                            });
                                        }
                                        if matches!(state, "queued" | "running")
                                            && ui.small_button("Pause").clicked()
                                        {
                                            send(DaemonRequest::QueueControl {
                                                task_id: id.clone(),
                                                action: QueueControlAction::Pause,
                                            });
                                        }
                                    });
                                });
                                ui.label(instruction);
                                if matches!(state, "paused" | "waiting_for_human") {
                                    ui.horizontal(|ui| {
                                        ui.add(
                                            egui::TextEdit::singleline(resume_note)
                                                .desired_width(220.0)
                                                .hint_text("Resume note…"),
                                        );
                                        if ui
                                            .add_enabled(
                                                !resume_note.trim().is_empty(),
                                                egui::Button::new("Hand back to bot"),
                                            )
                                            .clicked()
                                        {
                                            send(DaemonRequest::TakeTheWheelResume {
                                                task_id: id.clone(),
                                                note: Some(resume_note.clone()),
                                            });
                                        }
                                    });
                                }
                            });
                        });
                        ui.add_space(4.0);
                    }
                });
        }
    }
}
