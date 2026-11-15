//! Shared native desktop UI. All execution goes through the authenticated local daemon IPC.
use crate::muse_render::{RenderedPart, parse_message_bubbles, sandbox_uri_to_bot_relpath};
use eframe::egui;
mod session;
use gm_core::{
    config::GmConfig,
    error::{GmError, Result},
    ipc::*,
};
use serde_json::Value;
use session::{SessionData, SessionStore};
use std::{
    collections::BTreeSet,
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::io::{AsyncRead, AsyncWrite};

trait Connection: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Connection for T {}
async fn connect(path: &Path) -> Result<Box<dyn Connection>> {
    #[cfg(unix)]
    {
        Ok(Box::new(tokio::net::UnixStream::connect(path).await?))
    }
    #[cfg(windows)]
    {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            match tokio::net::windows::named_pipe::ClientOptions::new().open(path) {
                Ok(pipe) => return Ok(Box::new(pipe)),
                Err(e)
                    if e.raw_os_error() == Some(231) && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}
pub(crate) async fn is_running(config: &GmConfig) -> bool {
    connect(&config.daemon_socket_path).await.is_ok()
}

async fn rpc(config: &GmConfig, request: DaemonRequest) -> Result<DaemonResponse> {
    tokio::time::timeout(std::time::Duration::from_secs(45), async {
        let mut connection = connect(&config.daemon_socket_path).await?;
        write_ipc_message(
            &mut connection,
            &IpcMessage {
                id: 1,
                payload: request,
            },
        )
        .await?;
        let reply: IpcMessage<DaemonResponse> = read_ipc_message(&mut connection).await?;
        match reply.payload {
            DaemonResponse::Error(error) => Err(GmError::Ipc(error)),
            reply => Ok(reply),
        }
    })
    .await
    .map_err(|_| GmError::Timeout("The daemon did not respond".into()))?
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingKind {
    Other,
    ImportAttachment,
    ListBotFiles,
    PreviewBotFile,
    ChannelHistory(String),
    ListActivity,
    RateActivity,
    ListGoals,
    SaveGoal,
    ListSpaces,
    CreateSpace,
    SearchSkills,
    ReadRuntimeFile,
    SaveRuntimeFile,
    ImportMemory,
    ForgetPlan,
    ForgetConfirm,
    UpdateDiscretion,
}

fn pending_kind(request: &DaemonRequest) -> PendingKind {
    match request {
        DaemonRequest::ImportAttachment { .. } => PendingKind::ImportAttachment,
        DaemonRequest::ListBotFiles { .. } => PendingKind::ListBotFiles,
        DaemonRequest::PreviewBotFile { .. } => PendingKind::PreviewBotFile,
        DaemonRequest::ChannelHistory { channel_id } => {
            PendingKind::ChannelHistory(channel_id.clone())
        }
        DaemonRequest::ListActivity => PendingKind::ListActivity,
        DaemonRequest::RateActivity { .. } => PendingKind::RateActivity,
        DaemonRequest::ListGoals => PendingKind::ListGoals,
        DaemonRequest::SaveGoal { .. } => PendingKind::SaveGoal,
        DaemonRequest::ListSpaces => PendingKind::ListSpaces,
        DaemonRequest::CreateSpace { .. } => PendingKind::CreateSpace,
        DaemonRequest::SearchSkills { .. } => PendingKind::SearchSkills,
        DaemonRequest::ReadRuntimeFile { .. } => PendingKind::ReadRuntimeFile,
        DaemonRequest::SaveRuntimeFile { .. } => PendingKind::SaveRuntimeFile,
        DaemonRequest::ImportMemory { .. } => PendingKind::ImportMemory,
        DaemonRequest::ForgetPlan { .. } => PendingKind::ForgetPlan,
        DaemonRequest::ForgetConfirm { .. } => PendingKind::ForgetConfirm,
        DaemonRequest::UpdateDiscretion { .. } => PendingKind::UpdateDiscretion,
        _ => PendingKind::Other,
    }
}

#[derive(Default)]
struct ViewState {
    status: String,
    credentials_notice: String,
    credentials_saving: bool,
    error: Option<String>,
    messages: Vec<(String, String)>,
    streaming: String,
    tasks: Vec<Value>,
    bots: Vec<Value>,
    channels: Vec<Value>,
    routines: Vec<Value>,
    files: Value,
    file_preview: Value,
    attachment_paths: Vec<(String, String)>,
    channel_messages: Vec<Value>,
    channel_history_id: String,
    approvals: Vec<(String, String, String)>,
    connected: bool,
    sending: bool,
    clear_draft: Option<String>,
    active_chat: Option<String>,
    completed_chat: Option<String>,
    activity_posts: Vec<Value>,
    brief_prompt: Option<String>,
    activity_notice: String,
    goals: Vec<Value>,
    goal_notice: String,
    spaces_data: Value,
    spaces_notice: String,
    skills_data: Value,
    runtime_file_bot: String,
    runtime_file_name: String,
    runtime_file_content: String,
    runtime_file_sha256: Option<String>,
    runtime_file_dirty: bool,
    identity_notice: String,
    memory_import_report: Option<Value>,
    forget_plan: Option<Value>,
    forget_report: Option<Value>,
    forget_notice: String,
    discretion_policy: Option<Value>,
    discretion_notice: String,
}
impl ViewState {
    fn finish_chat(&mut self, id: String, text: String) {
        if self.active_chat.as_ref() != Some(&id) || self.completed_chat.as_ref() == Some(&id) {
            return;
        }
        self.streaming.clear();
        self.messages.push(("Muse".into(), text));
        self.completed_chat = Some(id);
    }
    fn event(&mut self, event: DaemonEvent) {
        match event {
            DaemonEvent::HudStateChanged { state, detail, .. } => {
                self.status = format!("{state}: {detail}")
            }
            DaemonEvent::ChatTokenV1 {
                message_id,
                delta,
                done,
            } if self.active_chat.as_ref() == Some(&message_id)
                && self.completed_chat.as_ref() != Some(&message_id) =>
            {
                self.streaming.push_str(&delta);
                if done {
                    let text = std::mem::take(&mut self.streaming);
                    self.finish_chat(message_id, text);
                }
            }
            DaemonEvent::TaskFinished {
                task_id,
                ok,
                message,
            } => self.messages.push((
                if ok { "Completed" } else { "Task update" }.into(),
                format!("{task_id}\n{message}"),
            )),
            DaemonEvent::ApprovalResolved { request_id } => {
                self.approvals.retain(|(id, _, _)| id != &request_id)
            }
            DaemonEvent::ApprovalRequested {
                request_id,
                title,
                details,
            } => self.approvals.push((request_id, title, details)),
            DaemonEvent::WheelHandoffRequested {
                task_id,
                blocker,
                instructions,
                ..
            } => self.messages.push((
                "Human input needed".into(),
                format!("{task_id}\n{blocker}\n{instructions}"),
            )),
            DaemonEvent::ActivityUpdated { reason } => {
                self.activity_notice = format!("Updated ({reason})");
            }
            _ => {}
        }
        // Bound retained UI history; full task results remain in the daemon's durable store.
        if self.messages.len() > 500 {
            self.messages.drain(..100);
        }
    }
}
#[derive(Clone)]
struct Shared {
    state: Arc<Mutex<ViewState>>,
    context: egui::Context,
    session: Option<Arc<SessionStore>>,
}
impl Shared {
    fn update(&self, f: impl FnOnce(&mut ViewState)) {
        if let Ok(mut state) = self.state.lock() {
            let before = (
                state.messages.len(),
                state
                    .messages
                    .last()
                    .map(|(role, text)| (role.len(), text.len())),
            );
            f(&mut state);
            if before
                != (
                    state.messages.len(),
                    state
                        .messages
                        .last()
                        .map(|(role, text)| (role.len(), text.len())),
                )
                && let Some(store) = &self.session
            {
                store.edit(|d| d.messages = state.messages.clone());
            }
        }
        self.context.request_repaint();
    }
}
struct NativeApp {
    shared: Shared,
    requests: tokio::sync::mpsc::Sender<(DaemonRequest, Option<String>)>,
    draft: String,
    bot: String,
    task_mode: bool,
    search: String,
    resume_note: String,
    settings: bool,
    routines: bool,
    files: bool,
    show_feed: bool,
    show_goals: bool,
    show_spaces: bool,
    show_skills: bool,
    show_identity: bool,
    liked_messages: BTreeSet<usize>,
    goal_title: String,
    goal_description: String,
    goal_category: String,
    goal_parent: String,
    space_slug: String,
    space_title: String,
    space_html: String,
    skill_query: String,
    identity_bot: String,
    identity_file: String,
    identity_text: String,
    identity_sha256: Option<String>,
    memory_import_text: String,
    forget_request: String,
    forget_verify_query: String,
    discretion_chat_id: String,
    discretion_rules_text: String,
    discretion_sha256: Option<String>,
    routine_editor: Option<Value>,
    bot_editor: Option<Value>,
    channel: String,
    new_channel: String,
    channel_mode: bool,
    settings_config: GmConfig,
    config_path: std::path::PathBuf,
    config_original: Option<Vec<u8>>,
    settings_notice: String,
    meta_key: String,
    xai_key: String,
}
impl NativeApp {
    fn send(&self, request: DaemonRequest) {
        if let Err(error) = self.requests.try_send((request, None)) {
            self.shared
                .update(|s| s.error = Some(format!("Request queue is busy: {error}")));
        }
    }

    fn enqueue_chat_message(
        &self,
        text: String,
        messages: &[(String, String)],
        clear_draft: Option<String>,
    ) {
        let message_id = uuid::Uuid::new_v4().to_string();
        self.shared.update(|state| {
            state.active_chat = Some(message_id.clone());
            state.completed_chat = None;
            state.streaming.clear();
        });
        let request = DaemonRequest::SendChatMessageV2 {
            message_id,
            text: text.clone(),
            history: conversation_context(messages),
        };
        if self.requests.try_send((request, clear_draft)).is_ok() {
            self.shared.update(|s| {
                s.sending = true;
                s.error = None;
                s.messages.push(("You".into(), text));
            });
        } else {
            self.shared.update(|s| {
                s.error = Some("Request queue is busy; your draft is preserved".into())
            });
        }
    }

    fn render_muse_parts(
        &mut self,
        ui: &mut egui::Ui,
        id_prefix: &str,
        parts: &[RenderedPart],
        chat_messages: Option<&[(String, String)]>,
        can_interact: bool,
    ) {
        let multi = parts.len() > 1;
        for (part_idx, part) in parts.iter().enumerate() {
            ui.push_id((id_prefix, part_idx), |ui| {
                let mut render_body = |ui: &mut egui::Ui| {
                    if !part.text.is_empty() {
                        ui.add(egui::Label::new(part.text.as_str()).selectable(true).wrap());
                    }
                    for (att_idx, att) in part.attachments.iter().enumerate() {
                        ui.push_id(("att", att_idx), |ui| {
                            ui.group(|ui| {
                                ui.horizontal_wrapped(|ui| {
                                    let label = if att.alt.trim().is_empty() {
                                        att.uri.as_str()
                                    } else {
                                        att.alt.as_str()
                                    };
                                    ui.label(format!("📎 {label}"));
                                    if let Some(rel) = sandbox_uri_to_bot_relpath(&att.uri) {
                                        if ui
                                            .small_button("Preview file")
                                            .on_hover_text(&rel)
                                            .clicked()
                                        {
                                            self.files = true;
                                            self.send(DaemonRequest::ListBotFiles {
                                                bot_id: self.bot.clone(),
                                            });
                                            self.send(DaemonRequest::PreviewBotFile {
                                                bot_id: self.bot.clone(),
                                                path: rel,
                                            });
                                        }
                                    } else {
                                        ui.small(&att.uri);
                                    }
                                });
                            });
                        });
                    }
                    if !part.citations.is_empty() {
                        ui.collapsing(format!("Sources ({})", part.citations.len()), |ui| {
                            for cite in &part.citations {
                                ui.add(
                                    egui::Label::new(format!("[{}] {}", cite.index, cite.raw))
                                        .selectable(true)
                                        .wrap(),
                                );
                            }
                        });
                    }
                    if !part.options.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            for (opt_idx, opt) in part.options.iter().enumerate() {
                                ui.push_id(("opt", opt_idx), |ui| {
                                    if ui
                                        .add_enabled(can_interact, egui::Button::new(opt.as_str()))
                                        .clicked()
                                    {
                                        if let Some(msgs) = chat_messages {
                                            self.enqueue_chat_message(opt.clone(), msgs, None);
                                        } else {
                                            self.send(DaemonRequest::ChannelPost {
                                                channel_id: self.channel.clone(),
                                                author: "human".into(),
                                                content: opt.clone(),
                                            });
                                        }
                                    }
                                });
                            }
                        });
                    }
                };
                if multi {
                    ui.group(|ui| render_body(ui));
                } else {
                    render_body(ui);
                }
            });
        }
    }

    fn draw_feed_window(&mut self, ctx: &egui::Context) {
        if !self.show_feed {
            return;
        }
        let (posts, brief_prompt, notice) = {
            let s = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                s.activity_posts.clone(),
                s.brief_prompt.clone(),
                s.activity_notice.clone(),
            )
        };
        let mut open = self.show_feed;
        egui::Window::new("Activity Feed")
            .vscroll(true)
            .max_height(620.0)
            .open(&mut open)
            .default_width(580.0)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Refresh").clicked() {
                        self.send(DaemonRequest::ListActivity);
                    }
                    if !notice.is_empty() {
                        ui.small(&notice);
                    }
                });
                if let Some(prompt) = &brief_prompt
                    && !prompt.trim().is_empty()
                {
                    ui.small(format!("Brief focus: {prompt}"));
                }
                ui.separator();
                if posts.is_empty() {
                    ui.label("No activity posts yet.");
                    return;
                }
                for post in &posts {
                    let id = post["id"].as_str().unwrap_or("").to_string();
                    let kicker = post["kicker"].as_str().unwrap_or("");
                    let category = post["category"].as_str().unwrap_or("Observation");
                    let title = post["title"].as_str().unwrap_or("Update");
                    let body = post["body"].as_str().unwrap_or("");
                    ui.push_id(&id, |ui| {
                        ui.group(|ui| {
                            let meta = if kicker.is_empty() {
                                category.to_string()
                            } else {
                                format!("{category} · {kicker}")
                            };
                            ui.small(meta);
                            ui.strong(title);
                            ui.add(egui::Label::new(body).selectable(true).wrap());
                            if let Some(cites) = post["signal_citations"].as_array()
                                && !cites.is_empty()
                            {
                                let labels: Vec<&str> =
                                    cites.iter().filter_map(|v| v.as_str()).collect();
                                if !labels.is_empty() {
                                    ui.small(format!("Signals: {}", labels.join(", ")));
                                }
                            }
                            ui.horizontal(|ui| {
                                let rating = post["rating"].as_i64();
                                if let Some(r) = rating {
                                    ui.small(format!("Rated: {r}"));
                                }
                                if !id.is_empty() {
                                    if ui.small_button("Useful 👍").clicked() {
                                        self.send(DaemonRequest::RateActivity {
                                            id: id.clone(),
                                            rating: 5,
                                        });
                                    }
                                    if ui.small_button("Not helpful 👎").clicked() {
                                        self.send(DaemonRequest::RateActivity {
                                            id: id.clone(),
                                            rating: -1,
                                        });
                                    }
                                }
                            });
                        });
                    });
                }
            });
        self.show_feed = open;
    }

    fn draw_goals_window(&mut self, ctx: &egui::Context) {
        if !self.show_goals {
            return;
        }
        let (goals, notice) = {
            let s = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            (s.goals.clone(), s.goal_notice.clone())
        };
        let mut open = self.show_goals;
        egui::Window::new("Goals")
            .vscroll(true)
            .max_height(640.0)
            .open(&mut open)
            .default_width(600.0)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Refresh").clicked() {
                        self.send(DaemonRequest::ListGoals);
                    }
                    if !notice.is_empty() {
                        ui.small(&notice);
                    }
                });
                ui.group(|ui| {
                    ui.strong("New Goal or Subgoal");
                    ui.label("Title");
                    ui.text_edit_singleline(&mut self.goal_title);
                    ui.label("Description");
                    ui.add(
                        egui::TextEdit::multiline(&mut self.goal_description)
                            .desired_rows(2)
                            .desired_width(ui.available_width()),
                    );
                    ui.horizontal(|ui| {
                        ui.label("Category");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.goal_category)
                                .desired_width(140.0)
                                .hint_text("optional"),
                        );
                        ui.label("Parent slug");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.goal_parent)
                                .desired_width(160.0)
                                .hint_text("for subgoal"),
                        );
                    });
                    if ui
                        .add_enabled(
                            !self.goal_title.trim().is_empty(),
                            egui::Button::new("Create Goal"),
                        )
                        .clicked()
                    {
                        let title = self.goal_title.trim().to_string();
                        let description = self.goal_description.trim().to_string();
                        let category = non_empty_trimmed(&self.goal_category);
                        let parent = non_empty_trimmed(&self.goal_parent);
                        let req = if let Some(parent_slug) = parent {
                            DaemonRequest::SaveGoal {
                                op: "create_subgoal".into(),
                                slug: None,
                                payload: serde_json::json!({
                                    "parent_slug": parent_slug,
                                    "title": title,
                                    "description": description,
                                }),
                            }
                        } else {
                            DaemonRequest::SaveGoal {
                                op: "create".into(),
                                slug: None,
                                payload: serde_json::json!({
                                    "title": title,
                                    "description": description,
                                    "category": category,
                                }),
                            }
                        };
                        self.send(req);
                        self.goal_title.clear();
                        self.goal_description.clear();
                        self.goal_category.clear();
                        self.goal_parent.clear();
                    }
                });
                ui.separator();
                if goals.is_empty() {
                    ui.label("No goals yet.");
                    return;
                }
                for entry in &goals {
                    let g = &entry["goal"];
                    let slug = g["slug"].as_str().unwrap_or("").to_string();
                    let title = g["title"].as_str().unwrap_or(&slug);
                    let status = g["status"].as_str().unwrap_or("Active");
                    let desc = g["description"].as_str().unwrap_or("");
                    let parent_id = g["parent_goal_id"].as_str();
                    ui.push_id(&slug, |ui| {
                        ui.group(|ui| {
                            let prefix = if parent_id.is_some() { "↳ " } else { "" };
                            ui.strong(format!("{prefix}{title}"));
                            ui.small(format!(
                                "slug: {slug} · {status}{}",
                                g["category"]
                                    .as_str()
                                    .map(|c| format!(" · {c}"))
                                    .unwrap_or_default()
                            ));
                            if !desc.is_empty() {
                                ui.add(egui::Label::new(desc).selectable(true).wrap());
                            }
                            if let Some(files) = entry["files"].as_array()
                                && !files.is_empty()
                            {
                                let names: Vec<&str> =
                                    files.iter().filter_map(|v| v.as_str()).collect();
                                if !names.is_empty() {
                                    ui.small(format!("Deliverables: {}", names.join(", ")));
                                }
                            }
                            ui.horizontal_wrapped(|ui| {
                                if parent_id.is_none()
                                    && status == "Active"
                                    && ui.small_button("Subgoal").clicked()
                                {
                                    self.goal_parent = slug.clone();
                                }
                                if status == "Active" {
                                    if ui.small_button("Complete").clicked() {
                                        self.send(DaemonRequest::SaveGoal {
                                            op: "complete".into(),
                                            slug: Some(slug.clone()),
                                            payload: serde_json::json!({ "verified": true }),
                                        });
                                    }
                                    if ui.small_button("Abandon").clicked() {
                                        self.send(DaemonRequest::SaveGoal {
                                            op: "abandon".into(),
                                            slug: Some(slug.clone()),
                                            payload: Value::Null,
                                        });
                                    }
                                }
                                if ui.small_button("Delete").clicked() {
                                    self.send(DaemonRequest::SaveGoal {
                                        op: "delete".into(),
                                        slug: Some(slug.clone()),
                                        payload: Value::Null,
                                    });
                                }
                            });
                        });
                    });
                }
            });
        self.show_goals = open;
    }

    fn draw_spaces_window(&mut self, ctx: &egui::Context) {
        if !self.show_spaces {
            return;
        }
        let (data, notice) = {
            let s = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            (s.spaces_data.clone(), s.spaces_notice.clone())
        };
        let mut open = self.show_spaces;
        egui::Window::new("Spaces Library (Static)")
            .vscroll(true)
            .max_height(620.0)
            .open(&mut open)
            .default_width(580.0)
            .show(ctx, |ui| {
                let enabled = data["enable_spaces"].as_bool().unwrap_or(false);
                let port = data["spaces_port"].as_u64().unwrap_or(0);
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Refresh").clicked() {
                        self.send(DaemonRequest::ListSpaces);
                    }
                    ui.small(if enabled {
                        format!("Static origin enabled (port {port})")
                    } else {
                        "Spaces disabled in config".into()
                    });
                    if !notice.is_empty() {
                        ui.small(&notice);
                    }
                });
                ui.group(|ui| {
                    ui.strong("Create Static Space");
                    ui.horizontal(|ui| {
                        ui.label("Slug");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.space_slug)
                                .desired_width(160.0)
                                .hint_text("my-dashboard"),
                        );
                        ui.label("Title");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.space_title)
                                .desired_width(180.0)
                                .hint_text("optional"),
                        );
                    });
                    ui.label("Custom HTML (optional; defaults to static scaffold)");
                    ui.add(
                        egui::TextEdit::multiline(&mut self.space_html)
                            .desired_rows(3)
                            .desired_width(ui.available_width()),
                    );
                    if ui
                        .add_enabled(
                            enabled && !self.space_slug.trim().is_empty(),
                            egui::Button::new("Create Static Space"),
                        )
                        .clicked()
                    {
                        self.send(DaemonRequest::CreateSpace {
                            slug: self.space_slug.trim().to_string(),
                            title: non_empty_trimmed(&self.space_title),
                            kind: Some("static".into()),
                            html: non_empty_trimmed(&self.space_html),
                        });
                        self.space_slug.clear();
                        self.space_title.clear();
                        self.space_html.clear();
                    }
                });
                ui.separator();
                let spaces = data["spaces"].as_array().cloned().unwrap_or_default();
                if spaces.is_empty() {
                    ui.label("No spaces created yet.");
                    return;
                }
                for sp in &spaces {
                    let slug = sp["slug"].as_str().unwrap_or("");
                    let title = sp["title"].as_str().unwrap_or(slug);
                    let kind = sp["kind"].as_str().unwrap_or("static");
                    let published = sp["published"].as_bool().unwrap_or(false);
                    let cap_url = data["urls"][slug].as_str();
                    ui.push_id(slug, |ui| {
                        ui.group(|ui| {
                            ui.strong(format!("{title} ({slug})"));
                            ui.small(format!(
                                "kind: {kind} · {}",
                                if published { "published" } else { "local" }
                            ));
                            ui.horizontal_wrapped(|ui| {
                                if let Some(url) = cap_url {
                                    ui.hyperlink_to("Open Space in browser", url);
                                    if ui.small_button("Copy URL").clicked() {
                                        ctx.copy_text(url.to_string());
                                    }
                                } else {
                                    ui.small("Capability URL unavailable");
                                }
                            });
                        });
                    });
                }
            });
        self.show_spaces = open;
    }

    fn draw_skills_window(&mut self, ctx: &egui::Context) {
        if !self.show_skills {
            return;
        }
        let data = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .skills_data
            .clone();
        let mut open = self.show_skills;
        egui::Window::new("Skills & Connectors")
            .vscroll(true)
            .max_height(620.0)
            .open(&mut open)
            .default_width(580.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.skill_query)
                            .desired_width(260.0)
                            .hint_text("Filter skills or connectors"),
                    );
                    if ui.button("Search").clicked()
                        || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    {
                        self.send(DaemonRequest::SearchSkills {
                            query: self.skill_query.clone(),
                        });
                    }
                    if ui.button("Show all").clicked() {
                        self.skill_query.clear();
                        self.send(DaemonRequest::SearchSkills {
                            query: String::new(),
                        });
                    }
                });
                ui.separator();
                let skills = data["skills"].as_array().cloned().unwrap_or_default();
                ui.strong(format!("Skills ({})", skills.len()));
                for skill in &skills {
                    let m = &skill["manifest"];
                    let name = m["name"]
                        .as_str()
                        .or_else(|| skill["dir_name"].as_str())
                        .unwrap_or("skill");
                    let title = m["title"].as_str().unwrap_or(name);
                    let category = m["category"].as_str().unwrap_or("");
                    let bundled = skill["bundled"].as_bool().unwrap_or(false);
                    ui.push_id(("skill", name), |ui| {
                        ui.group(|ui| {
                            ui.strong(title);
                            ui.small(format!(
                                "{name} · {} · {category}",
                                if bundled { "bundled" } else { "custom" }
                            ));
                            if let Some(desc) = m["description"].as_str() {
                                ui.add(egui::Label::new(desc).selectable(true).wrap());
                            }
                        });
                    });
                }
                ui.separator();
                let connectors = data["connectors"].as_array().cloned().unwrap_or_default();
                ui.strong(format!("Connectors ({})", connectors.len()));
                for conn in &connectors {
                    let m = &conn["manifest"];
                    let id = m["id"]
                        .as_str()
                        .or_else(|| conn["dir_name"].as_str())
                        .unwrap_or("connector");
                    let title = m["title"].as_str().unwrap_or(id);
                    let bundled = conn["bundled"].as_bool().unwrap_or(false);
                    ui.push_id(("conn", id), |ui| {
                        ui.group(|ui| {
                            ui.strong(title);
                            ui.small(format!(
                                "{id} · {}",
                                if bundled { "bundled" } else { "custom" }
                            ));
                            if let Some(desc) = m["description"].as_str() {
                                ui.add(egui::Label::new(desc).selectable(true).wrap());
                            }
                        });
                    });
                }
            });
        self.show_skills = open;
    }

    fn draw_identity_window(&mut self, ctx: &egui::Context, bots: &[Value]) {
        if !self.show_identity {
            return;
        }
        let (
            identity_notice,
            import_report,
            forget_plan,
            forget_report,
            forget_notice,
            discretion_policy,
            discretion_notice,
        ) = {
            let s = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                s.identity_notice.clone(),
                s.memory_import_report.clone(),
                s.forget_plan.clone(),
                s.forget_report.clone(),
                s.forget_notice.clone(),
                s.discretion_policy.clone(),
                s.discretion_notice.clone(),
            )
        };
        let mut open = self.show_identity;
        egui::Window::new("Identity, Memory & Privacy")
            .vscroll(true)
            .max_height(660.0)
            .open(&mut open)
            .default_width(640.0)
            .show(ctx, |ui| {
                ui.heading("Persona & Proactive Files");
                ui.horizontal_wrapped(|ui| {
                    let prev_file = self.identity_file.clone();
                    egui::ComboBox::from_id_salt("identity-file-select")
                        .selected_text(&self.identity_file)
                        .show_ui(ui, |ui| {
                            for name in [
                                "SOUL.md",
                                "IDENTITY.md",
                                "USER.md",
                                "MEMORY.md",
                                "PROACTIVE_PREFERENCES.md",
                            ] {
                                ui.selectable_value(&mut self.identity_file, name.into(), name);
                            }
                        });
                    let prev_bot = self.identity_bot.clone();
                    egui::ComboBox::from_id_salt("identity-bot-select")
                        .selected_text(&self.identity_bot)
                        .show_ui(ui, |ui| {
                            for bot in bots {
                                if let Some(id) = bot["id"].as_str() {
                                    ui.selectable_value(&mut self.identity_bot, id.to_string(), id);
                                }
                            }
                        });
                    if prev_file != self.identity_file || prev_bot != self.identity_bot {
                        self.send(DaemonRequest::ReadRuntimeFile {
                            bot_id: Some(self.identity_bot.clone()),
                            file: self.identity_file.clone(),
                        });
                    }
                    if ui.button("Reload").clicked() {
                        self.send(DaemonRequest::ReadRuntimeFile {
                            bot_id: Some(self.identity_bot.clone()),
                            file: self.identity_file.clone(),
                        });
                    }
                });
                if let Some(sha) = &self.identity_sha256 {
                    let short: String = sha.chars().take(12).collect();
                    ui.small(format!("SHA-256 lock: {short}…"));
                }
                if !identity_notice.is_empty() {
                    ui.small(&identity_notice);
                }
                ui.add(
                    egui::TextEdit::multiline(&mut self.identity_text)
                        .desired_rows(8)
                        .desired_width(ui.available_width()),
                );
                if ui.button("Save file").clicked() {
                    self.send(DaemonRequest::SaveRuntimeFile {
                        bot_id: Some(self.identity_bot.clone()),
                        file: self.identity_file.clone(),
                        content: self.identity_text.clone(),
                        expected_sha256: self.identity_sha256.clone(),
                    });
                }

                ui.separator();
                ui.collapsing("Portable Memory Import", |ui| {
                    ui.label("Paste bullet points or notes. Dry-run filters prompt injections and plaintext secrets.");
                    ui.add(
                        egui::TextEdit::multiline(&mut self.memory_import_text)
                            .desired_rows(4)
                            .desired_width(ui.available_width()),
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !self.memory_import_text.trim().is_empty(),
                                egui::Button::new("Preview import (dry-run)"),
                            )
                            .clicked()
                        {
                            self.send(DaemonRequest::ImportMemory {
                                input: self.memory_import_text.clone(),
                                apply: false,
                                report_sha256: None,
                            });
                        }
                        if let Some(report) = &import_report
                            && !report["applied"].as_bool().unwrap_or(false)
                            && let Some(sha) = report["report_sha256"].as_str()
                            && ui.button("Apply verified import").clicked()
                        {
                            self.send(DaemonRequest::ImportMemory {
                                input: self.memory_import_text.clone(),
                                apply: true,
                                report_sha256: Some(sha.to_string()),
                            });
                        }
                    });
                    if let Some(report) = &import_report {
                        let adds = report["additions"].as_array().map_or(0, |a| a.len());
                        let rejs = report["rejected"].as_array().map_or(0, |a| a.len());
                        ui.small(format!(
                            "applied: {} · additions: {adds} · rejected: {rejs}",
                            report["applied"].as_bool().unwrap_or(false)
                        ));
                    }
                });

                ui.separator();
                ui.collapsing("Forget (Two-Phase Erasure)", |ui| {
                    ui.small("Remember: plans store blinded hashes/IDs only and require explicit confirmation.");
                    ui.horizontal(|ui| {
                        ui.label("Erase query");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.forget_request)
                                .desired_width(260.0)
                                .hint_text("exact fact, topic, or phrase"),
                        );
                        if ui
                            .add_enabled(
                                !self.forget_request.trim().is_empty(),
                                egui::Button::new("Create Forget Plan"),
                            )
                            .clicked()
                        {
                            self.send(DaemonRequest::ForgetPlan {
                                bot_id: Some(self.identity_bot.clone()),
                                request: self.forget_request.trim().to_string(),
                            });
                        }
                    });
                    if let Some(plan) = &forget_plan {
                        let plan_id = plan["plan_id"].as_str().unwrap_or("").to_string();
                        let fp = plan["request_fingerprint"].as_str().unwrap_or("");
                        ui.group(|ui| {
                            ui.strong(format!("Pending Plan {plan_id} (fingerprint {fp})"));
                            ui.small(format!("Targets summary: {}", plan["summary_counts"]));
                            if let Some(excl) = plan["excluded_disclosures"].as_array() {
                                for line in excl {
                                    if let Some(txt) = line.as_str() {
                                        ui.small(format!("Excluded: {txt}"));
                                    }
                                }
                            }
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.forget_verify_query)
                                        .desired_width(220.0)
                                        .hint_text("Optional verification query"),
                                );
                                if !plan_id.is_empty()
                                    && ui.button("Confirm & Permanently Erase").clicked()
                                {
                                    self.send(DaemonRequest::ForgetConfirm {
                                        bot_id: Some(self.identity_bot.clone()),
                                        plan_id,
                                        verification_query: non_empty_trimmed(
                                            &self.forget_verify_query,
                                        ),
                                        confirmed: true,
                                    });
                                }
                            });
                        });
                    }
                    if let Some(report) = &forget_report {
                        let msg = report["message"].as_str().unwrap_or("");
                        let rem = report["remaining"].as_array().map_or(0, |a| a.len());
                        ui.small(format!(
                            "Status: {} · {msg} · remaining hits: {rem}",
                            report["status"].as_str().unwrap_or("unknown")
                        ));
                    }
                    if !forget_notice.is_empty() {
                        ui.small(&forget_notice);
                    }
                });

                ui.separator();
                ui.collapsing("Bridge Discretion Rules (DISCRETION.md)", |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Chat ID");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.discretion_chat_id)
                                .desired_width(220.0),
                        );
                    });
                    ui.label("User rules (one rule per line; immutable default policy always applies)");
                    ui.add(
                        egui::TextEdit::multiline(&mut self.discretion_rules_text)
                            .desired_rows(3)
                            .desired_width(ui.available_width()),
                    );
                    if ui
                        .add_enabled(
                            !self.discretion_chat_id.trim().is_empty()
                                && !self.discretion_rules_text.trim().is_empty(),
                            egui::Button::new("Save Discretion Rules"),
                        )
                        .clicked()
                    {
                        let rules: Vec<String> = self
                            .discretion_rules_text
                            .lines()
                            .map(str::trim)
                            .filter(|l| !l.is_empty())
                            .map(str::to_string)
                            .collect();
                        self.send(DaemonRequest::UpdateDiscretion {
                            chat_id: self.discretion_chat_id.trim().to_string(),
                            rules,
                            expected_sha256: self.discretion_sha256.clone(),
                        });
                    }
                    if let Some(policy) = &discretion_policy
                        && let Some(rules) = policy["user_rules"].as_array()
                    {
                        let labels: Vec<&str> = rules.iter().filter_map(|v| v.as_str()).collect();
                        if !labels.is_empty() {
                            ui.small(format!("Active user rules: {}", labels.join(" | ")));
                        }
                    }
                    if !discretion_notice.is_empty() {
                        ui.small(&discretion_notice);
                    }
                });
            });
        self.show_identity = open;
    }
}

fn non_empty_trimmed(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
impl Drop for NativeApp {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.meta_key.zeroize();
        self.xai_key.zeroize();
    }
}
impl eframe::App for NativeApp {
    fn on_exit(&mut self) {
        if let Some(store) = &self.shared.session {
            store.edit(|d| {
                d.draft = self.draft.clone();
                d.bot = self.bot.clone();
                d.task_mode = self.task_mode;
            });
            if let Err(error) = store.flush() {
                tracing::error!(%error,"Could not save native conversation");
            }
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let shortcut = take_composer_shortcut(ctx);
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for file in dropped {
            if let Some(path) = file.path {
                self.send(DaemonRequest::ImportAttachment {
                    bot_id: self.bot.clone(),
                    source_path: path,
                });
            }
        }
        let attachments = std::mem::take(
            &mut self
                .shared
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .attachment_paths,
        );
        for (bot, path) in attachments {
            if bot == self.bot {
                if !self.draft.is_empty() {
                    self.draft.push('\n');
                }
                self.draft.push_str(&format!("Attached file: {path}"));
            } else {
                self.shared.update(|s| {
                    s.messages
                        .push(("File imported".into(), format!("@{bot}: {path}")))
                });
            }
        }
        // Render from a snapshot so the UI never holds a lock during network work or callbacks.
        let (status, error, connected, messages, streaming, tasks, bots, approvals, sending) = {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(sent) = state.clear_draft.take()
                && self.draft.trim() == sent
            {
                self.draft.clear();
            }
            if state.runtime_file_dirty {
                self.identity_bot = state.runtime_file_bot.clone();
                self.identity_file = state.runtime_file_name.clone();
                self.identity_text = state.runtime_file_content.clone();
                self.identity_sha256 = state.runtime_file_sha256.clone();
                state.runtime_file_dirty = false;
            } else if state.runtime_file_bot == self.identity_bot
                && state.runtime_file_name == self.identity_file
                && state.runtime_file_sha256.is_some()
            {
                self.identity_sha256 = state.runtime_file_sha256.clone();
            }
            if let Some(policy) = &state.discretion_policy
                && policy["chat_id"].as_str() == Some(self.discretion_chat_id.trim())
                && let Some(sha) = policy["sha256"].as_str()
            {
                self.discretion_sha256 = Some(sha.to_string());
            }
            (
                state.status.clone(),
                state.error.clone(),
                state.connected,
                state.messages.clone(),
                state.streaming.clone(),
                state.tasks.clone(),
                state.bots.clone(),
                state.approvals.clone(),
                state.sending,
            )
        };
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.heading("Grok Muse");
                if ui.button("Files").clicked() {
                    self.files = !self.files;
                    if self.files {
                        self.send(DaemonRequest::ListBotFiles {
                            bot_id: self.bot.clone(),
                        });
                    }
                }

                if ui.button("Routines").clicked() {
                    self.routines = !self.routines;
                }

                if ui.button("Feed").clicked() {
                    self.show_feed = !self.show_feed;
                    if self.show_feed {
                        self.send(DaemonRequest::ListActivity);
                    }
                }

                if ui.button("Goals").clicked() {
                    self.show_goals = !self.show_goals;
                    if self.show_goals {
                        self.send(DaemonRequest::ListGoals);
                    }
                }

                if ui.button("Spaces").clicked() {
                    self.show_spaces = !self.show_spaces;
                    if self.show_spaces {
                        self.send(DaemonRequest::ListSpaces);
                    }
                }

                if ui.button("Skills").clicked() {
                    self.show_skills = !self.show_skills;
                    if self.show_skills {
                        self.send(DaemonRequest::SearchSkills {
                            query: self.skill_query.clone(),
                        });
                    }
                }

                if ui.button("Identity").clicked() {
                    self.show_identity = !self.show_identity;
                    if self.show_identity {
                        self.identity_bot = self.bot.clone();
                        self.send(DaemonRequest::ReadRuntimeFile {
                            bot_id: Some(self.identity_bot.clone()),
                            file: self.identity_file.clone(),
                        });
                    }
                }

                if ui.button("Settings").clicked() {
                    self.settings = !self.settings;
                }

                ui.label(if connected {
                    "Connected"
                } else {
                    "Connecting…"
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Stop all").clicked() {
                        self.send(DaemonRequest::EmergencyKill);
                    }
                    if ui.button("Pause").clicked() {
                        self.send(DaemonRequest::SetPaused { paused: true });
                    }
                    if ui.button("Resume admission").clicked() {
                        self.send(DaemonRequest::SetPaused { paused: false });
                    }
                });
            });
            ui.label(status);
            if let Some(error) = error {
                ui.colored_label(egui::Color32::from_rgb(220, 70, 70), error);
            }
        });
        egui::SidePanel::right("tasks")
            .default_width(300.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.heading("Tasks");
                egui::ScrollArea::vertical()
                    .id_salt("task-scroll")
                    .show(ui, |ui| {
                        for task in tasks.iter().rev().take(100) {
                            let id = task["id"].as_str().unwrap_or("").to_string();
                            let state = task["status"].as_str().unwrap_or("unknown");
                            ui.push_id(&id, |ui| {
                                ui.group(|ui| {
                                    ui.label(format!(
                                        "{} · {state}",
                                        task["bot_id"].as_str().unwrap_or("bot")
                                    ));
                                    ui.label(task["instruction"].as_str().unwrap_or(""));
                                    if let Some(result) = task["result_summary"].as_str() {
                                        ui.collapsing("Result", |ui| {
                                            ui.add(
                                                egui::Label::new(result).selectable(true).wrap(),
                                            );
                                        });
                                    }
                                    if let Some(reason) = task["blocker_note"].as_str() {
                                        ui.label(reason);
                                    }
                                    ui.horizontal_wrapped(|ui| {
                                        for (label, action, enabled) in [
                                            (
                                                "Pause",
                                                QueueControlAction::Pause,
                                                matches!(state, "queued" | "running"),
                                            ),
                                            (
                                                "Cancel",
                                                QueueControlAction::Cancel,
                                                matches!(
                                                    state,
                                                    "queued"
                                                        | "running"
                                                        | "paused"
                                                        | "waiting_for_human"
                                                        | "pause_requested"
                                                ),
                                            ),
                                            (
                                                "Retry",
                                                QueueControlAction::Retry,
                                                matches!(state, "failed" | "cancelled")
                                                    && task["mutating_in_flight"] != true,
                                            ),
                                        ] {
                                            if ui
                                                .add_enabled(enabled, egui::Button::new(label))
                                                .clicked()
                                            {
                                                self.send(DaemonRequest::QueueControl {
                                                    task_id: id.clone(),
                                                    action,
                                                });
                                            }
                                        }
                                    });
                                    if matches!(state, "paused" | "waiting_for_human")
                                        || (task["mutating_in_flight"] == true
                                            && matches!(state, "failed" | "cancelled"))
                                    {
                                        ui.label("Review the task before handing control back.");
                                        ui.add(
                                            egui::TextEdit::singleline(&mut self.resume_note)
                                                .hint_text("What changed?"),
                                        );
                                        if ui
                                            .add_enabled(
                                                !self.resume_note.trim().is_empty(),
                                                egui::Button::new("Hand back to bot"),
                                            )
                                            .clicked()
                                        {
                                            self.send(DaemonRequest::TakeTheWheelResume {
                                                task_id: id.clone(),
                                                note: Some(self.resume_note.clone()),
                                            });
                                        }
                                    }
                                });
                            });
                        }
                        if tasks.is_empty() {
                            ui.label("Tasks appear here when submitted.");
                        }
                    });
            });
        egui::TopBottomPanel::bottom("composer").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.task_mode, "Run task");
                ui.checkbox(&mut self.channel_mode, "Channel message");
                if ui.button("Edit bot").clicked() {
                    self.bot_editor = bots
                        .iter()
                        .find(|b| b["id"].as_str() == Some(self.bot.as_str()))
                        .cloned();
                }

                egui::ComboBox::from_label("Bot")
                    .selected_text(&self.bot)
                    .show_ui(ui, |ui| {
                        for bot in &bots {
                            if let Some(id) = bot["id"].as_str() {
                                ui.selectable_value(&mut self.bot, id.to_string(), id);
                            }
                        }
                    });
            });
            let channels = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .channels
                .clone();
            ui.horizontal(|ui| {
                let previous = self.channel.clone();
                egui::ComboBox::from_label("Channel")
                    .selected_text(format!("#{}", self.channel))
                    .show_ui(ui, |ui| {
                        for channel in channels {
                            if let Some(id) = channel["id"].as_str() {
                                ui.selectable_value(
                                    &mut self.channel,
                                    id.to_string(),
                                    format!("#{id}"),
                                );
                            }
                        }
                    });
                if previous != self.channel {
                    self.send(DaemonRequest::ChannelHistory {
                        channel_id: self.channel.clone(),
                    });
                }
                if ui.button("Refresh channel").clicked() {
                    self.send(DaemonRequest::ChannelHistory {
                        channel_id: self.channel.clone(),
                    });
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_channel)
                        .desired_width(120.0)
                        .hint_text("new-channel"),
                );
                if ui
                    .add_enabled(
                        !self.new_channel.trim().is_empty(),
                        egui::Button::new("Create channel"),
                    )
                    .clicked()
                {
                    self.send(DaemonRequest::CreateChannel {
                        channel_id: self.new_channel.trim().into(),
                        topic: String::new(),
                        owner_bot_id: Some(self.bot.clone()),
                        participants: vec![self.bot.clone()],
                    });
                    self.new_channel.clear();
                }
            });
            ui.add(
                egui::TextEdit::multiline(&mut self.draft).id(egui::Id::new("main-draft"))
                    .desired_width(ui.available_width())
                    .desired_rows(3)
                    .hint_text("Ask a question, or enable Run task. Drop a file here to attach it to the selected bot."),
            );
            if ui
                .add_enabled(
                    connected && !sending && !self.draft.trim().is_empty(),
                    egui::Button::new(if cfg!(target_os="macos") {"Send  ⌘Enter"} else {"Send  Ctrl+Enter"}),
                )
                .clicked()
                || (shortcut && connected && !sending && !self.draft.trim().is_empty())
            {
                let text = self.draft.trim().to_string();
                if self.channel_mode {
                    let request = DaemonRequest::ChannelPost {
                        channel_id: self.channel.clone(),
                        author: "human".into(),
                        content: text.clone(),
                    };
                    if self
                        .requests
                        .try_send((request, Some(text.clone())))
                        .is_ok()
                    {
                        self.shared.update(|s| {
                            s.sending = true;
                            s.error = None;
                            s.messages.push(("You".into(), text));
                        });
                    } else {
                        self.shared.update(|s| {
                            s.error = Some("Request queue is busy; your draft is preserved".into())
                        });
                    }
                } else if self.task_mode {
                    let request = DaemonRequest::QueueTask {
                        bot_id: Some(self.bot.clone()),
                        channel_id: Some(self.channel.clone()),
                        instruction: text.clone(),
                        priority: 0,
                    };
                    if self
                        .requests
                        .try_send((request, Some(text.clone())))
                        .is_ok()
                    {
                        self.shared.update(|s| {
                            s.sending = true;
                            s.error = None;
                            s.messages.push(("You".into(), text));
                        });
                    } else {
                        self.shared.update(|s| {
                            s.error = Some("Request queue is busy; your draft is preserved".into())
                        });
                    }
                } else {
                    self.enqueue_chat_message(text.clone(), &messages, Some(text));
                }
            }
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.channel_mode {
                let (history_id, channel_messages) = {
                    let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
                    (
                        state.channel_history_id.clone(),
                        state.channel_messages.clone(),
                    )
                };
                ui.heading(format!("#{}", self.channel));
                ui.label("Messages can enqueue work for @mentions or the channel owner.");
                if history_id == self.channel {
                    egui::ScrollArea::vertical()
                        .id_salt("channel-history")
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for (idx, message) in channel_messages.iter().rev().enumerate() {
                                let author = message["author"].as_str().unwrap_or("bot");
                                let content = message["content"].as_str().unwrap_or("");
                                ui.push_id(("ch", idx), |ui| {
                                    ui.strong(author);
                                    let parts = if author == "human" {
                                        Vec::new()
                                    } else {
                                        parse_message_bubbles(content)
                                    };
                                    if parts.is_empty() {
                                        ui.add(egui::Label::new(content).selectable(true).wrap());
                                    } else {
                                        self.render_muse_parts(
                                            ui,
                                            "ch-part",
                                            &parts,
                                            None,
                                            connected && !sending,
                                        );
                                    }
                                    ui.add_space(8.0);
                                });
                            }
                        });
                } else {
                    ui.label("Use Refresh channel to load messages.");
                }
                return;
            }
            ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Search conversation"));
            let needle = self.search.to_lowercase();
            let visible_indices: Vec<usize> = messages
                .iter()
                .enumerate()
                .filter(|(_, (_, text))| {
                    needle.is_empty() || text.to_lowercase().contains(&needle)
                })
                .map(|(idx, _)| idx)
                .collect();
            egui::ScrollArea::vertical()
                .id_salt("conversation")
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for idx in visible_indices {
                        let (role, text) = &messages[idx];
                        ui.push_id(idx, |ui| {
                            ui.strong(role);
                            let parts = if role == "Muse" {
                                parse_message_bubbles(text)
                            } else {
                                Vec::new()
                            };
                            if parts.is_empty() {
                                ui.add(egui::Label::new(text.as_str()).selectable(true).wrap());
                            } else {
                                self.render_muse_parts(
                                    ui,
                                    "msg-part",
                                    &parts,
                                    Some(&messages),
                                    connected && !sending,
                                );
                            }
                            if role == "Muse" {
                                let liked = self.liked_messages.contains(&idx);
                                let label = if liked { "✓ Liked 👍" } else { "👍 Like" };
                                if ui.small_button(label).clicked() {
                                    if liked {
                                        self.liked_messages.remove(&idx);
                                    } else {
                                        self.liked_messages.insert(idx);
                                    }
                                }
                            }
                            ui.add_space(12.0);
                        });
                    }
                    if !streaming.is_empty() {
                        ui.strong("Muse");
                        let parts = parse_message_bubbles(&streaming);
                        if parts.is_empty() {
                            ui.add(egui::Label::new(&streaming).selectable(true).wrap());
                        } else {
                            self.render_muse_parts(ui, "stream-part", &parts, None, false);
                        }
                    }
                    if messages.is_empty() {
                        ui.heading("Your local assistant workspace");
                        ui.label("Start with a question or choose Run task. Desktop control requires explicit opt-in; unavailable capabilities are shown as errors.");
                    }
                });
        });
        if let Some(store) = &self.shared.session {
            let changed = {
                let d = store.data.lock().unwrap_or_else(|e| e.into_inner());
                d.draft != self.draft || d.bot != self.bot || d.task_mode != self.task_mode
            };
            if changed {
                store.edit(|d| {
                    d.draft = self.draft.clone();
                    d.bot = self.bot.clone();
                    d.task_mode = self.task_mode;
                });
            }
        }
        if self.files {
            let mut open = true;
            egui::Window::new("Attachments and artifacts").vscroll(true).max_height(600.0).open(&mut open).default_width(650.0).show(ctx,|ui| {
                ui.label(format!("Files for @{}. Drop files onto this window to import them into this bot's inbox (8 MiB maximum).",self.bot));
                if ui.button("Refresh files").clicked() {self.send(DaemonRequest::ListBotFiles {bot_id:self.bot.clone()});}
                let (files,preview)={let s=self.shared.state.lock().unwrap_or_else(|e|e.into_inner());(s.files.clone(),s.file_preview.clone())};
                if files["bot_id"].as_str()==Some(self.bot.as_str()) && let Some(files)=files["files"].as_array() {
                    egui::ScrollArea::vertical().id_salt("bot-files").max_height(220.0).show(ui,|ui| {
                        for file in files {let path=file["path"].as_str().unwrap_or("");ui.horizontal(|ui| {let width=(ui.available_width()-190.0).max(120.0);ui.add_sized([width,24.0],egui::Label::new(format!("{path} · {} bytes",file["size"])).truncate()).on_hover_text(path);
                            if ui.button("Preview").clicked() {self.send(DaemonRequest::PreviewBotFile {bot_id:self.bot.clone(),path:path.into()});}
                            if ui.button("Use in task").clicked() {if !self.draft.is_empty() {self.draft.push('\n');}self.draft.push_str(&format!("Attached file: {path}"));}
                        });}
                    });
                }
                if preview["bot_id"].as_str()==Some(self.bot.as_str()) && let Some(text)=preview["text"].as_str() {
                    ui.separator();ui.label(preview["path"].as_str().unwrap_or("Preview"));
                    if ui.button("Copy text").clicked() {ctx.copy_text(text.into());}
                    egui::ScrollArea::both().id_salt("artifact-preview").max_height(350.0).show(ui,|ui|{ui.add(egui::Label::new(egui::RichText::new(text).monospace()).selectable(true));});
                }
            });
            self.files = open;
        }
        if self.routines {
            let mut open = true;
            egui::Window::new("Routines").vscroll(true).max_height(600.0).open(&mut open).default_width(560.0).show(ctx,|ui| {
                ui.label("The daemon must be running. Missed runs coalesce into one catch-up task; overlapping runs are skipped.");
                if ui.button("New daily routine").clicked() {
                    self.routine_editor=Some(serde_json::json!({"id":uuid::Uuid::new_v4().to_string(),"name":"Daily routine","bot_id":self.bot,"channel_id":self.channel,"instruction":"","timezone":"UTC","hour":9,"minute":0,"weekdays":[],"enabled":false}));
                }
                let routines=self.shared.state.lock().unwrap_or_else(|e|e.into_inner()).routines.clone();
                for state in routines {
                    let r=&state["routine"];
                    ui.group(|ui| {
                        ui.strong(r["name"].as_str().unwrap_or("Routine"));
                        ui.label(format!("{} · {} · next {}",r["bot_id"].as_str().unwrap_or("bot"),r["timezone"].as_str().unwrap_or("UTC"),state["next_run_utc"].as_str().unwrap_or("unknown")));
                        ui.label(if r["enabled"]==true {"Enabled"} else {"Disabled"});
                        ui.horizontal(|ui| {
                            if ui.button("Edit").clicked() {self.routine_editor=Some(r.clone());}
                            if ui.button(if r["enabled"]==true {"Disable"} else {"Enable"}).clicked() {let mut routine=r.clone();routine["enabled"]=Value::Bool(r["enabled"]!=true);self.send(DaemonRequest::SaveRoutine {routine});}
                        });
                    });
                }
            });
            self.routines = open;
        }
        let mut save_routine = None;
        if let Some(routine) = &mut self.routine_editor {
            let mut open = true;
            egui::Window::new("Routine schedule").vscroll(true).max_height(600.0).open(&mut open).default_width(540.0).show(ctx,|ui| {
                for (key,label,multiline) in [("name","Name",false),("bot_id","Bot",false),("channel_id","Channel",false),("instruction","Task instructions",true),("timezone","IANA timezone (for example America/New_York)",false)] {
                    ui.label(label);let mut text=routine[key].as_str().unwrap_or("").to_string();
                    let changed=if multiline {ui.add(egui::TextEdit::multiline(&mut text).desired_rows(5).desired_width(ui.available_width())).changed()} else {ui.text_edit_singleline(&mut text).changed()};
                    if changed {routine[key]=if key=="channel_id" && text.trim().is_empty() {Value::Null} else {Value::String(text)};}
                }
                ui.horizontal(|ui| {
                    for (key,label,maximum) in [("hour","Hour",23),("minute","Minute",59)] {let mut value=routine[key].as_u64().unwrap_or(0) as u32;ui.label(label);if ui.add(egui::DragValue::new(&mut value).range(0..=maximum)).changed() {routine[key]=serde_json::json!(value);}}
                });
                let mut days=routine["weekdays"].as_array().cloned().unwrap_or_default();
                ui.horizontal_wrapped(|ui| {for (index,label) in ["Mon","Tue","Wed","Thu","Fri","Sat","Sun"].iter().enumerate() {let value=serde_json::json!(index);let mut enabled=days.contains(&value);if ui.checkbox(&mut enabled,*label).changed() {days.retain(|v|v!=&value);if enabled {days.push(value);}}}});
                routine["weekdays"]=Value::Array(days);
                ui.label("No weekdays selected means every day. Clock changes: skipped times move forward to the next valid minute; repeated times run once on the earlier offset.");
                let mut enabled=routine["enabled"].as_bool().unwrap_or(false);ui.checkbox(&mut enabled,"Enable automatic task submission");routine["enabled"]=Value::Bool(enabled);
                if ui.button("Save routine").clicked() {save_routine=Some(routine.clone());}
            });
            if !open {
                self.routine_editor = None;
            }
        }
        if let Some(routine) = save_routine {
            self.send(DaemonRequest::SaveRoutine { routine });
            self.routine_editor = None;
        }
        let mut save_bot = None;
        if let Some(profile) = &mut self.bot_editor {
            let mut open = true;
            egui::Window::new("Edit bot").open(&mut open).default_width(520.0).show(ctx,|ui| {
                for (key,label,multiline) in [("display_name","Name",false),("role_title","Role",false),("system_prompt","Instructions",true),("preferred_model","Preferred model (optional)",false)] {
                    ui.label(label);let mut text=profile[key].as_str().unwrap_or("").to_string();
                    let changed=if multiline {ui.add(egui::TextEdit::multiline(&mut text).desired_rows(6).desired_width(ui.available_width())).changed()} else {ui.text_edit_singleline(&mut text).changed()};
                    if changed {profile[key]=if key=="preferred_model" && text.is_empty() {Value::Null} else {Value::String(text)};}
                }
                let mut provider=profile["preferred_provider"].as_str().unwrap_or("auto").to_string();
                egui::ComboBox::from_label("Provider").selected_text(&provider).show_ui(ui,|ui| {for name in ["auto","meta","xai","local"] {ui.selectable_value(&mut provider,name.into(),name);}});
                profile["preferred_provider"]=if provider=="auto" {Value::Null} else {Value::String(provider)};
                for (key,label) in [("allow_gui","Desktop access"),("allow_shell","Workspace VM commands"),("allow_network","Network tools")] {
                    let mut allowed=profile[key].as_bool().unwrap_or(false);if ui.checkbox(&mut allowed,label).changed() {profile[key]=Value::Bool(allowed);}
                }
                ui.label("Applies to newly admitted tasks. Global desktop and screenshot settings also apply.");
                if ui.button("Save bot").clicked() {save_bot=Some(profile.clone());}
            });
            if !open {
                self.bot_editor = None;
            }
        }
        if let Some(profile) = save_bot {
            self.send(DaemonRequest::SaveBot { profile });
            self.bot_editor = None;
        }
        if self.settings {
            let mut open = true;
            egui::Window::new("Settings and setup").open(&mut open).default_width(550.0).show(ctx,|ui| {
                egui::ScrollArea::vertical().show(ui,|ui| {
                    ui.label("Configure a model provider, review spending limits, then choose the access your tasks need.");
                    ui.label("Save API credentials in your operating system credential store. Keys are never written to configuration or conversation history.");
                    ui.label("Meta API key");ui.add(egui::TextEdit::singleline(&mut self.meta_key).password(true));
                    ui.label("xAI API key");ui.add(egui::TextEdit::singleline(&mut self.xai_key).password(true));
                    let (saving,notice)={let s=self.shared.state.lock().unwrap_or_else(|e|e.into_inner());(s.credentials_saving,s.credentials_notice.clone())};
                    if ui.add_enabled(!saving && (!self.meta_key.is_empty() || !self.xai_key.is_empty()),egui::Button::new("Save API keys")).clicked() {
                        let meta=zeroize::Zeroizing::new(std::mem::take(&mut self.meta_key));
                        let xai=zeroize::Zeroizing::new(std::mem::take(&mut self.xai_key));
                        let shared=self.shared.clone();shared.update(|s|{s.credentials_saving=true;s.credentials_notice="Saving to the OS credential store…".into();});
                        tokio::task::spawn_blocking(move || {
                            let keys=gm_llm::KeychainSecrets::default();
                            let result=(|| {if !meta.is_empty() {keys.set_meta_key(&meta)?;}
                            if !xai.is_empty() {keys.set_xai_key(&xai)?;}Ok::<_,GmError>(())})();
                            shared.update(|s|{s.credentials_saving=false;s.credentials_notice=match result {Ok(())=>"API keys saved. Restart the daemon to use changed credentials.".into(),Err(e)=>e.to_string()};});
                        });
                    }
                    ui.label(notice);
                    ui.separator();
                    ui.strong("Models");
                    ui.label("Meta API base");ui.text_edit_singleline(&mut self.settings_config.providers.meta_api_base);
                    ui.label("Computer-use model");ui.text_edit_singleline(&mut self.settings_config.providers.meta_cu_model);
                    ui.label("Reasoning model");ui.text_edit_singleline(&mut self.settings_config.providers.meta_reasoning_model);
                    ui.label("xAI API base");ui.text_edit_singleline(&mut self.settings_config.providers.xai_api_base);
                    ui.label("xAI model");ui.text_edit_singleline(&mut self.settings_config.providers.xai_model);
                    ui.separator();ui.strong("Spending limits (USD)");
                    ui.horizontal(|ui|{ui.label("Daily");ui.add(egui::DragValue::new(&mut self.settings_config.budget.daily_spend_cap_usd).range(0.0..=10000.0).speed(0.1));});
                    ui.horizontal(|ui|{ui.label("Per task");ui.add(egui::DragValue::new(&mut self.settings_config.budget.per_job_spend_cap_usd).range(0.0..=1000.0).speed(0.05));});
                    ui.separator();ui.strong("Desktop access");
                    ui.checkbox(&mut self.settings_config.safety.allow_host_desktop,"Allow agents to observe and control this desktop");
                    ui.checkbox(&mut self.settings_config.providers.allow_screenshot_egress_meta,"Allow screenshots to Meta");
                    ui.checkbox(&mut self.settings_config.providers.allow_screenshot_egress_xai,"Allow screenshots to xAI");
                    ui.label("Linux Wayland asks for monitor, pointer and keyboard consent when a desktop task first needs capture or input. Stop sharing in your desktop controls to revoke access.");
                    ui.separator();
                    egui::widgets::global_theme_preference_buttons(ui);
                    if ui.button("Save settings").clicked() {
                        let current=std::fs::read(&self.config_path).ok();
                        let result=if current!=self.config_original {
                            Err(GmError::Config("Configuration changed outside this window. Reopen the app before saving.".into()))
                        } else { self.settings_config.save(&self.config_path) };
                        match result {
                            Ok(())=>{self.config_original=std::fs::read(&self.config_path).ok();self.settings_notice="Saved. Restart Grok Muse and its daemon to apply changes to new tasks.".into();},
                            Err(e)=>self.settings_notice=e.to_string(),
                        }
                    }
                    ui.label(&self.settings_notice);
                    ui.separator();ui.strong("Conversation history");
                    ui.label("Saved locally with your draft. Retains up to 500 messages within a bounded private file.");
                    if ui.add_enabled(!sending,egui::Button::new("Clear conversation history")).clicked() {self.shared.update(|s|s.messages.clear());}

                });
            });
            self.settings = open;
        }
        self.draw_feed_window(ctx);
        self.draw_goals_window(ctx);
        self.draw_spaces_window(ctx);
        self.draw_skills_window(ctx);
        self.draw_identity_window(ctx, &bots);
        for (id, title, details) in approvals {
            egui::Window::new(title)
                .id(egui::Id::new(&id))
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.add(egui::Label::new(details).selectable(true).wrap());
                    ui.horizontal(|ui| {
                        for (label, approved) in [("Approve this action", true), ("Deny", false)] {
                            if ui.button(label).clicked() {
                                self.send(DaemonRequest::RespondApproval {
                                    request_id: id.clone(),
                                    approved,
                                });
                                self.shared
                                    .update(|s| s.approvals.retain(|(key, _, _)| key != &id));
                            }
                        }
                    });
                });
        }
    }
}

async fn background(
    config: GmConfig,
    shared: Shared,
    mut requests: tokio::sync::mpsc::Receiver<(DaemonRequest, Option<String>)>,
) {
    let refresh_notify = Arc::new(tokio::sync::Notify::new());
    let events_config = config.clone();
    let events_shared = shared.clone();
    let events_notify = Arc::clone(&refresh_notify);
    tokio::spawn(async move {
        loop {
            if let Ok(mut connection) = connect(&events_config.daemon_socket_path).await {
                let result: Result<()> = async {
                    write_ipc_message(
                        &mut connection,
                        &IpcMessage {
                            id: 1,
                            payload: DaemonRequest::SubscribeEvents,
                        },
                    )
                    .await?;
                    let _: IpcMessage<DaemonResponse> = read_ipc_message(&mut connection).await?;
                    events_shared.update(|s| s.connected = true);
                    loop {
                        let response: IpcMessage<DaemonResponse> =
                            read_ipc_message(&mut connection).await?;
                        if let DaemonResponse::Event(event) = response.payload {
                            let wake_refresh = matches!(event, DaemonEvent::ActivityUpdated { .. });
                            events_shared.update(|s| s.event(event));
                            if wake_refresh {
                                events_notify.notify_one();
                            }
                        }
                    }
                }
                .await;
                if let Err(error) = result {
                    events_shared.update(|s| {
                        s.connected = false;
                        s.error = Some(error.to_string());
                    });
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
    let refresh_shared = shared.clone();
    let refresh_config = config.clone();
    let poll_notify = Arc::clone(&refresh_notify);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {}
                _ = poll_notify.notified() => {}
            }
            if let Ok(DaemonResponse::JsonPayload(value)) =
                rpc(&refresh_config, DaemonRequest::ListQueue { bot_id: None }).await
            {
                refresh_shared.update(|s| s.tasks = value.as_array().cloned().unwrap_or_default());
            }
            if let Ok(DaemonResponse::JsonPayload(value)) =
                rpc(&refresh_config, DaemonRequest::ListRoutines).await
            {
                refresh_shared
                    .update(|s| s.routines = value.as_array().cloned().unwrap_or_default());
            }
            if let Ok(DaemonResponse::JsonPayload(value)) =
                rpc(&refresh_config, DaemonRequest::ListChannels).await
            {
                refresh_shared
                    .update(|s| s.channels = value.as_array().cloned().unwrap_or_default());
            }
            let channel = refresh_shared
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .channel_history_id
                .clone();
            if !channel.is_empty()
                && let Ok(DaemonResponse::JsonPayload(value)) = rpc(
                    &refresh_config,
                    DaemonRequest::ChannelHistory {
                        channel_id: channel.clone(),
                    },
                )
                .await
            {
                refresh_shared.update(|s| {
                    if s.channel_history_id == channel {
                        s.channel_messages = value.as_array().cloned().unwrap_or_default();
                    }
                });
            }
            if let Ok(DaemonResponse::JsonPayload(value)) =
                rpc(&refresh_config, DaemonRequest::ListBots).await
            {
                refresh_shared.update(|s| s.bots = value.as_array().cloned().unwrap_or_default());
            }
            if let Ok(DaemonResponse::JsonPayload(value)) =
                rpc(&refresh_config, DaemonRequest::ListActivity).await
            {
                refresh_shared.update(|s| {
                    s.activity_posts = value["posts"].as_array().cloned().unwrap_or_default();
                    s.brief_prompt = value["brief_prompt"].as_str().map(str::to_string);
                });
            }
            if let Ok(DaemonResponse::JsonPayload(value)) =
                rpc(&refresh_config, DaemonRequest::ListGoals).await
            {
                refresh_shared.update(|s| s.goals = value.as_array().cloned().unwrap_or_default());
            }
            if let Ok(DaemonResponse::JsonPayload(value)) =
                rpc(&refresh_config, DaemonRequest::ListSpaces).await
            {
                refresh_shared.update(|s| s.spaces_data = value);
            }
        }
    });
    let slots = Arc::new(tokio::sync::Semaphore::new(4));
    while let Some((request, draft)) = requests.recv().await {
        let Ok(permit) = slots.clone().acquire_owned().await else {
            break;
        };
        let request_shared = shared.clone();
        let request_config = config.clone();
        let request_notify = Arc::clone(&refresh_notify);
        tokio::spawn(async move {
            let _permit = permit;
            let kind = pending_kind(&request);
            match rpc(&request_config, request).await {
                Ok(response) => {
                    let mut refresh_after = false;
                    request_shared.update(|s| {
                        if draft.is_some() {
                            s.sending = false;
                            s.clear_draft = draft.clone();
                        }
                        s.error = None;
                        match (kind, response) {
                            (PendingKind::ImportAttachment, DaemonResponse::JsonPayload(value)) => {
                                if let (Some(bot), Some(path)) =
                                    (value["bot_id"].as_str(), value["attachment_path"].as_str())
                                {
                                    s.attachment_paths.push((bot.into(), path.into()));
                                }
                            }
                            (PendingKind::ListBotFiles, DaemonResponse::JsonPayload(value)) => {
                                s.files = value;
                            }
                            (
                                PendingKind::PreviewBotFile,
                                DaemonResponse::JsonPayload(mut value),
                            ) => {
                                if let Some(text) = value["text"].as_str()
                                    && text.len() > 64 * 1024
                                {
                                    let end = text.floor_char_boundary(64 * 1024);
                                    value["text"] = Value::String(format!(
                                        "{}\n[Preview limited to 64 KiB]",
                                        &text[..end]
                                    ));
                                }
                                s.file_preview = value;
                            }
                            (
                                PendingKind::ChannelHistory(channel_id),
                                DaemonResponse::JsonPayload(value),
                            ) => {
                                s.channel_history_id = channel_id;
                                s.channel_messages = value.as_array().cloned().unwrap_or_default();
                            }
                            (PendingKind::ListActivity, DaemonResponse::JsonPayload(value)) => {
                                s.activity_posts =
                                    value["posts"].as_array().cloned().unwrap_or_default();
                                s.brief_prompt = value["brief_prompt"].as_str().map(str::to_string);
                            }
                            (PendingKind::RateActivity, DaemonResponse::JsonPayload(_)) => {
                                s.activity_notice = "Feedback recorded".into();
                                refresh_after = true;
                            }
                            (PendingKind::ListGoals, DaemonResponse::JsonPayload(value)) => {
                                s.goals = value.as_array().cloned().unwrap_or_default();
                            }
                            (PendingKind::SaveGoal, DaemonResponse::JsonPayload(_)) => {
                                s.goal_notice = "Goal updated".into();
                                refresh_after = true;
                            }
                            (PendingKind::ListSpaces, DaemonResponse::JsonPayload(value)) => {
                                s.spaces_data = value;
                            }
                            (PendingKind::CreateSpace, DaemonResponse::JsonPayload(value)) => {
                                let slug = value["space"]["slug"].as_str().unwrap_or("space");
                                s.spaces_notice = format!("Created Space '{slug}'");
                                refresh_after = true;
                            }
                            (PendingKind::SearchSkills, DaemonResponse::JsonPayload(value)) => {
                                s.skills_data = value;
                            }
                            (PendingKind::ReadRuntimeFile, DaemonResponse::JsonPayload(value)) => {
                                let file_name =
                                    value["file"].as_str().unwrap_or("SOUL.md").to_string();
                                let exists = value["exists"].as_bool().unwrap_or(false);
                                s.runtime_file_bot =
                                    value["bot_id"].as_str().unwrap_or("muse").into();
                                s.runtime_file_name = file_name.clone();
                                s.runtime_file_content =
                                    value["content"].as_str().unwrap_or("").into();
                                s.runtime_file_sha256 =
                                    value["sha256"].as_str().map(str::to_string);
                                s.runtime_file_dirty = true;
                                s.identity_notice = if exists {
                                    format!("Loaded {file_name}")
                                } else {
                                    format!("{file_name} does not exist yet (created on save)")
                                };
                            }
                            (PendingKind::SaveRuntimeFile, DaemonResponse::JsonPayload(value)) => {
                                if let Some(sha) = value["sha256"].as_str() {
                                    s.runtime_file_sha256 = Some(sha.to_string());
                                }
                                let file_name = value["file"].as_str().unwrap_or("file");
                                s.identity_notice = format!("Saved {file_name}");
                            }
                            (PendingKind::ImportMemory, DaemonResponse::JsonPayload(value)) => {
                                let applied = value["applied"].as_bool().unwrap_or(false);
                                let additions =
                                    value["additions"].as_array().map_or(0, |arr| arr.len());
                                s.memory_import_report = Some(value);
                                s.identity_notice = if applied {
                                    refresh_after = true;
                                    format!("Imported {additions} facts into memory")
                                } else {
                                    format!("Dry-run ready ({additions} facts extracted)")
                                };
                            }
                            (PendingKind::ForgetPlan, DaemonResponse::JsonPayload(value)) => {
                                s.forget_plan = Some(value);
                                s.forget_report = None;
                                s.forget_notice =
                                    "Plan prepared. Review counts and confirm to erase.".into();
                            }
                            (PendingKind::ForgetConfirm, DaemonResponse::JsonPayload(value)) => {
                                let msg = value["message"]
                                    .as_str()
                                    .unwrap_or("Forget completed")
                                    .to_string();
                                s.forget_notice = msg;
                                s.forget_report = Some(value);
                                s.forget_plan = None;
                                refresh_after = true;
                            }
                            (PendingKind::UpdateDiscretion, DaemonResponse::JsonPayload(value)) => {
                                s.discretion_policy = Some(value);
                                s.discretion_notice = "Discretion rules saved.".into();
                            }
                            (_, DaemonResponse::Accepted { id }) => {
                                s.messages.push(("Queued".into(), id))
                            }
                            (_, DaemonResponse::ChatReplyV1 { message_id, text })
                                if s.completed_chat.as_ref() != Some(&message_id) =>
                            {
                                s.finish_chat(message_id, text);
                            }
                            _ => {}
                        }
                    });
                    if refresh_after {
                        request_notify.notify_one();
                    }
                }
                Err(error) => {
                    let msg = error.to_string();
                    request_shared.update(|s| {
                        if let Some(draft) = &draft
                            && s.completed_chat != s.active_chat
                            && let Some((role, _)) = s
                                .messages
                                .iter_mut()
                                .rev()
                                .find(|(role, text)| role == "You" && text == draft)
                        {
                            *role = "Unsent".into();
                        }
                        if draft.is_some() {
                            s.sending = false;
                            if !s.streaming.is_empty() {
                                let partial = std::mem::take(&mut s.streaming);
                                s.messages.push(("Interrupted response".into(), partial));
                            }
                        }
                        match kind {
                            PendingKind::ListActivity | PendingKind::RateActivity => {
                                s.activity_notice = msg.clone();
                            }
                            PendingKind::ListGoals | PendingKind::SaveGoal => {
                                s.goal_notice = msg.clone();
                            }
                            PendingKind::ListSpaces | PendingKind::CreateSpace => {
                                s.spaces_notice = msg.clone();
                            }
                            PendingKind::ReadRuntimeFile
                            | PendingKind::SaveRuntimeFile
                            | PendingKind::ImportMemory => {
                                s.identity_notice = msg.clone();
                            }
                            PendingKind::ForgetPlan | PendingKind::ForgetConfirm => {
                                s.forget_notice = msg.clone();
                            }
                            PendingKind::UpdateDiscretion => {
                                s.discretion_notice = msg.clone();
                            }
                            _ => {}
                        }
                        s.error = Some(msg);
                    });
                }
            }
        });
    }
}

fn take_composer_shortcut(ctx: &egui::Context) -> bool {
    ctx.memory(|m| m.has_focus(egui::Id::new("main-draft")))
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter))
}

fn conversation_context(messages: &[(String, String)]) -> Vec<ChatTurn> {
    let mut bytes = 0;
    let mut turns = Vec::new();
    for (role, text) in messages.iter().rev() {
        let role = match role.as_str() {
            "You" => "user",
            "Muse" => "assistant",
            _ => continue,
        };
        if turns.len() >= 24 || bytes + text.len() > 64 * 1024 {
            break;
        }
        bytes += text.len();
        turns.push(ChatTurn {
            role: role.into(),
            content: text.clone(),
        });
    }
    turns.reverse();
    turns
}

pub fn run(config: GmConfig, config_path: std::path::PathBuf) -> Result<()> {
    let runtime = tokio::runtime::Handle::current();
    eframe::run_native(
        "Grok Muse",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1100.0, 760.0])
                .with_min_inner_size([680.0, 480.0]),
            ..Default::default()
        },
        Box::new(move |creation| {
            let mut style = (*creation.egui_ctx.style()).clone();
            style
                .text_styles
                .insert(egui::TextStyle::Body, egui::FontId::proportional(16.0));
            style
                .text_styles
                .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
            style
                .text_styles
                .insert(egui::TextStyle::Heading, egui::FontId::proportional(24.0));
            style
                .text_styles
                .insert(egui::TextStyle::Small, egui::FontId::proportional(13.0));
            style.spacing.item_spacing = egui::vec2(10.0, 8.0);
            style.spacing.button_padding = egui::vec2(12.0, 7.0);
            if style.visuals.dark_mode {
                style.visuals.panel_fill = egui::Color32::from_rgb(20, 26, 36);
                style.visuals.extreme_bg_color = egui::Color32::from_rgb(13, 18, 26);
                style.visuals.selection.bg_fill = egui::Color32::from_rgb(37, 95, 130);
                style.visuals.hyperlink_color = egui::Color32::from_rgb(108, 208, 220);
            }
            creation.egui_ctx.set_style(style);
            let (session, mut receiver, session_error) =
                match SessionStore::open(&config.storage_dir) {
                    Ok((store, receiver)) => (Some(store), Some(receiver), None),
                    Err(e) => (None, None, Some(e.to_string())),
                };
            let restored = session
                .as_ref()
                .map(|s| s.data.lock().unwrap_or_else(|e| e.into_inner()).clone())
                .unwrap_or_else(SessionData::default);
            let shared = Shared {
                state: Arc::new(Mutex::new(ViewState {
                    messages: restored.messages.clone(),
                    error: session_error,
                    ..Default::default()
                })),
                context: creation.egui_ctx.clone(),
                session: session.clone(),
            };
            if let (Some(store), Some(mut changes)) = (session, receiver.take()) {
                let writer_shared = shared.clone();
                runtime.spawn(async move {
                    while changes.changed().await.is_ok() {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        changes.borrow_and_update();
                        let next = store.clone();
                        match tokio::task::spawn_blocking(move || next.flush()).await {
                            Ok(Ok(())) => {}
                            other => writer_shared.update(|s| {
                                s.error =
                                    Some(format!("Conversation could not be saved: {other:?}"))
                            }),
                        }
                    }
                });
            }
            let settings_config = config.clone();
            let config_original = std::fs::read(&config_path).ok();
            let (sender, receiver) = tokio::sync::mpsc::channel(64);
            runtime.spawn(background(config, shared.clone(), receiver));
            let identity_bot = restored.bot.clone();
            Ok(Box::new(NativeApp {
                shared,
                requests: sender,
                draft: restored.draft,
                bot: restored.bot,
                task_mode: restored.task_mode,
                search: String::new(),
                resume_note: String::new(),
                bot_editor: None,
                channel: "general".into(),
                new_channel: String::new(),
                channel_mode: false,
                files: false,
                routines: false,
                show_feed: false,
                show_goals: false,
                show_spaces: false,
                show_skills: false,
                show_identity: false,
                liked_messages: BTreeSet::new(),
                goal_title: String::new(),
                goal_description: String::new(),
                goal_category: String::new(),
                goal_parent: String::new(),
                space_slug: String::new(),
                space_title: String::new(),
                space_html: String::new(),
                skill_query: String::new(),
                identity_bot,
                identity_file: "SOUL.md".into(),
                identity_text: String::new(),
                identity_sha256: None,
                memory_import_text: String::new(),
                forget_request: String::new(),
                forget_verify_query: String::new(),
                discretion_chat_id: "default".into(),
                discretion_rules_text: String::new(),
                discretion_sha256: None,
                routine_editor: None,
                settings: config_original.is_none(),
                settings_config,
                config_path,
                config_original,
                settings_notice: String::new(),
                meta_key: String::new(),
                xai_key: String::new(),
            }))
        }),
    )
    .map_err(|e| GmError::Platform(format!("Native UI could not start: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chat_reply_races_and_other_clients_never_duplicate_or_mix_answers() {
        for response_first in [true, false] {
            let mut state = ViewState {
                active_chat: Some("mine".into()),
                ..Default::default()
            };
            state.event(DaemonEvent::ChatTokenV1 {
                message_id: "another-client".into(),
                delta: "unrelated".into(),
                done: true,
            });
            assert!(state.messages.is_empty());
            if response_first {
                state.finish_chat("mine".into(), "one answer".into());
            }
            state.event(DaemonEvent::ChatTokenV1 {
                message_id: "mine".into(),
                delta: "one answer".into(),
                done: false,
            });
            state.event(DaemonEvent::ChatTokenV1 {
                message_id: "mine".into(),
                delta: String::new(),
                done: true,
            });
            if !response_first {
                state.finish_chat("mine".into(), "one answer".into());
            }
            assert_eq!(state.messages, vec![("Muse".into(), "one answer".into())]);
            assert!(state.streaming.is_empty());
        }
    }
    #[test]
    fn expired_or_cancelled_approvals_disappear() {
        let mut state = ViewState::default();
        state.event(DaemonEvent::ApprovalRequested {
            request_id: "one".into(),
            title: "Review".into(),
            details: "one action".into(),
        });
        state.event(DaemonEvent::ApprovalResolved {
            request_id: "one".into(),
        });
        assert!(state.approvals.is_empty());
    }
    #[test]
    fn activity_updated_event_sets_notice() {
        let mut state = ViewState::default();
        state.event(DaemonEvent::ActivityUpdated {
            reason: "feed_post_added".into(),
        });
        assert_eq!(state.activity_notice, "Updated (feed_post_added)");
    }
    #[test]
    fn pending_kind_maps_muse_ipc_requests() {
        assert_eq!(
            pending_kind(&DaemonRequest::ListActivity),
            PendingKind::ListActivity
        );
        assert_eq!(
            pending_kind(&DaemonRequest::ListGoals),
            PendingKind::ListGoals
        );
        assert_eq!(
            pending_kind(&DaemonRequest::ListSpaces),
            PendingKind::ListSpaces
        );
        assert_eq!(
            pending_kind(&DaemonRequest::SearchSkills {
                query: "goals".into()
            }),
            PendingKind::SearchSkills
        );
        assert_eq!(
            pending_kind(&DaemonRequest::ChannelHistory {
                channel_id: "general".into()
            }),
            PendingKind::ChannelHistory("general".into())
        );
    }
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;
    #[test]
    fn composer_consumes_send_shortcut_before_multiline_editor() {
        let ctx = egui::Context::default();
        let mut text = "preserve draft".to_string();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.add(egui::TextEdit::multiline(&mut text).id(egui::Id::new("main-draft")))
                    .request_focus();
            });
        });
        let modifiers = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let raw = egui::RawInput {
            modifiers,
            events: vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
            ..Default::default()
        };
        let mut submitted = false;
        let _ = ctx.run(raw, |ctx| {
            submitted = take_composer_shortcut(ctx);
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.add(egui::TextEdit::multiline(&mut text).id(egui::Id::new("main-draft")));
            });
        });
        assert!(submitted);
        assert_eq!(text, "preserve draft");
        let _ = ctx.run(Default::default(), |ctx| {
            ctx.memory_mut(|m| m.surrender_focus(egui::Id::new("main-draft")));
            assert!(!take_composer_shortcut(ctx));
        });
    }
}
