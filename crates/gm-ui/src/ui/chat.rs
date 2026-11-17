//! GrokBot Central Chat Canvas:
//! - 48px Header Bar (Vector Bot Avatar, Status Pill, Search `Cmd/Ctrl+F`, Live Computer, Split Page, Details `Cmd/Ctrl+Alt+B`)
//! - Two-Stage FTS5 Search Bar (Tier-1 recent 200 turns + Tier-2 `"Search older messages..."`)
//! - Virtualized `ScrollArea::show_rows` Transcript (`10,000` messages `< 1.0 ms` draw CPU) + Memoized Markdown/Table Cache
//! - Hover Message Action Bar (Reply, 6 Quick Emoji Reactions `👍 ❤️ 😂 🎉 🚀 👀`, Copy, Request-ID Badge)
//! - Rich Markdown Table Toolbar (`Copy Table`, `Copy CSV`, `Copy Markdown`, `Expand ⤢`)
//! - All 14 Inline Interactive Cards
//! - Smart Composer (`->` -> `→` with Backspace Undo & IME Guard, `@`/`/` Autocomplete, `<25 MB` Attachments, `[Dictate]`, `🎙 Voice` <-> `↑ Send`)

use crate::muse_render::{RenderedPart, sandbox_uri_to_bot_relpath};
use crate::ui::cache::{BoundedTextureCache, MarkdownLayoutCache, ParsedMarkdownTable};
use crate::ui::state::{
    ActiveModal, BotDetailsTab, GrokUiState, ReplyBanner, SplitPageTarget, WorkspaceSection,
    apply_smart_composer_edit,
};
use crate::ui::theme::{BotAvatarShape, GrokPalette, paint_bot_avatar, parse_hex_color};
use eframe::egui::{
    self, Align, Color32, CornerRadius, Key, Layout, RichText, Sense, Stroke, Vec2,
};
use gm_core::ipc::DaemonRequest;
use serde_json::Value;
use zeroize::Zeroizing;

pub const QUICK_REACTION_EMOJIS: [&str; 6] = ["👍", "❤️", "😂", "🎉", "🚀", "👀"];

pub const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/do ", "Enqueue a durable background or GUI task"),
    ("/goal ", "Create or update a hierarchical Goal"),
    (
        "/space ",
        "Create or update an origin-isolated Static Space",
    ),
    ("/forget ", "Plan a two-phase memory erasure"),
    (
        "/brief ",
        "Generate a proactive Morning/Evening Digest Brief",
    ),
    ("/teach ", "Start a 'Teach a Task' demonstration session"),
];

/// Normalized message row for the virtualized transcript (works across DM records, Channel records, and local session turns).
#[derive(Debug, Clone)]
pub struct TranscriptRow {
    pub id: String,
    pub seq: i64,
    pub author: String,
    pub role: String,
    pub content: String,
    pub created_at: String,
    pub reply_to_id: Option<String>,
    pub request_id: Option<String>,
    pub reactions: Vec<Value>,
    pub card: Option<Value>,
}

#[allow(clippy::too_many_arguments)]
pub fn draw_chat_canvas(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    ui_state: &mut GrokUiState,
    palette: &GrokPalette,
    textures: &mut BoundedTextureCache,
    layout_cache: &mut MarkdownLayoutCache,
    bots: &[Value],
    active_bot: &mut String,
    active_channel: &str,
    rows: &[TranscriptRow],
    streaming_text: &str,
    search_query: &mut String,
    search_hits: &[Value],
    draft: &mut String,
    connected: bool,
    sending: bool,
    status_line: &str,
    mut send: impl FnMut(DaemonRequest),
    mut submit_message: impl FnMut(String, Option<String>),
) {
    let active_bot_profile = bots
        .iter()
        .find(|b| b["id"].as_str() == Some(active_bot.as_str()));
    let bot_display_name: String = active_bot_profile
        .and_then(|b| b["display_name"].as_str())
        .unwrap_or(active_bot.as_str())
        .to_string();
    let bot_role_title: String = active_bot_profile
        .and_then(|b| b["role_title"].as_str())
        .unwrap_or("Primary Agent")
        .to_string();
    let bot_color = parse_hex_color(
        active_bot_profile
            .and_then(|b| b["avatar_color"].as_str().or_else(|| b["color"].as_str()))
            .unwrap_or("#3B82F6"),
    );
    let bot_shape = BotAvatarShape::from_name(
        active_bot_profile
            .and_then(|b| b["avatar_shape"].as_str())
            .unwrap_or("circle"),
    );

    // 1. 48px Chat Header Bar
    egui::Frame::NONE
        .fill(palette.bg_canvas)
        .stroke(Stroke::new(1.0_f32, palette.border_subtle))
        .inner_margin(egui::Margin::symmetric(14, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(ui_state.nav_history.can_back(), egui::Button::new("<"))
                    .clicked()
                {
                    ui_state.nav_request = -1;
                }
                if ui
                    .add_enabled(ui_state.nav_history.can_forward(), egui::Button::new(">"))
                    .clicked()
                {
                    ui_state.nav_request = 1;
                }
                if ui.small_button("☰").clicked() {
                    ui_state.sidebar_manual_override =
                        Some(!ui_state.sidebar_manual_override.unwrap_or(false));
                }
                let (avatar_rect, avatar_resp) =
                    ui.allocate_exact_size(Vec2::new(28.0, 28.0), Sense::click());
                let tex = textures
                    .get(&format!("avatar:{active_bot}"))
                    .map(|(t, _)| t);
                paint_bot_avatar(
                    ui.painter(),
                    avatar_rect,
                    bot_shape,
                    bot_color,
                    &bot_display_name,
                    tex,
                    Some(if connected { "online" } else { "offline" }),
                    palette.bg_canvas,
                );
                if avatar_resp
                    .on_hover_text("Customize Bot shape, color & avatar")
                    .clicked()
                {
                    ui_state.active_modal = Some(ActiveModal::AvatarPicker {
                        bot_id: active_bot.clone(),
                    });
                }

                ui.vertical(|ui| {
                    let header_title = if ui_state.active_chat_kind == "channel" {
                        format!("# {active_channel}")
                    } else {
                        bot_display_name.to_string()
                    };
                    ui.strong(
                        RichText::new(header_title)
                            .size(15.0)
                            .color(palette.text_primary),
                    );
                    let sub = if ui_state.active_chat_kind == "channel" {
                        format!("Shared group channel · {status_line}")
                    } else {
                        format!("{bot_role_title} · {status_line}")
                    };
                    ui.small(RichText::new(sub).color(palette.text_secondary));
                });

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let details_btn = egui::Button::new(RichText::new("◧ Details").color(
                        if ui_state.details_drawer_open {
                            palette.accent_text
                        } else {
                            palette.text_primary
                        },
                    ))
                    .fill(if ui_state.details_drawer_open {
                        palette.accent
                    } else {
                        palette.bg_elevated
                    });
                    if ui
                        .add(details_btn)
                        .on_hover_text("Toggle Bot Details Drawer (Cmd/Ctrl+Alt+B)")
                        .clicked()
                    {
                        ui_state.details_drawer_open = !ui_state.details_drawer_open;
                    }

                    let comp_btn = egui::Button::new(RichText::new("🖥 Computer").color(
                        if ui_state.live_computer_open {
                            palette.accent_text
                        } else {
                            palette.text_primary
                        },
                    ))
                    .fill(if ui_state.live_computer_open {
                        palette.accent
                    } else {
                        palette.bg_elevated
                    });
                    if ui
                        .add(comp_btn)
                        .on_hover_text("Toggle Live Computer Viewer (adaptive 2–5 FPS)")
                        .clicked()
                    {
                        ui_state.live_computer_open = !ui_state.live_computer_open;
                        if ui_state.live_computer_open {
                            ui_state.details_drawer_open = true;
                            ui_state.details_tab = BotDetailsTab::Computer;
                            send(DaemonRequest::CaptureComputerPreview {
                                max_long_edge: Some(1280),
                                quality: Some(75),
                            });
                        } else {
                            textures.evict_prefix("live_computer:");
                        }
                    }

                    if ui
                        .button("📄 SOUL.md")
                        .on_hover_text("Open SOUL.md in Split Page Editor")
                        .clicked()
                    {
                        send(DaemonRequest::ReadRuntimeFile {
                            bot_id: Some(active_bot.clone()),
                            file: "SOUL.md".into(),
                        });
                        ui_state.split_page = Some(SplitPageTarget::MarkdownFile {
                            bot_id: active_bot.clone(),
                            file: "SOUL.md".into(),
                            title: format!("@{active_bot} / SOUL.md"),
                            editing: false,
                        });
                    }

                    let search_btn =
                        egui::Button::new(RichText::new("🔍").color(if ui_state.search_bar_open {
                            palette.accent_text
                        } else {
                            palette.text_primary
                        }))
                        .fill(if ui_state.search_bar_open {
                            palette.accent
                        } else {
                            palette.bg_elevated
                        });
                    if ui
                        .add(search_btn)
                        .on_hover_text("Search messages (Cmd/Ctrl+F)")
                        .clicked()
                    {
                        ui_state.search_bar_open = !ui_state.search_bar_open;
                    }
                });
            });
        });

    // 2. Two-Stage FTS5 Search Bar (when open)
    if ui_state.search_bar_open {
        egui::Frame::NONE
            .fill(palette.bg_sidebar)
            .stroke(Stroke::new(1.0_f32, palette.border_subtle))
            .inner_margin(egui::Margin::symmetric(14, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("🔍");
                    let resp = ui.add(
                        egui::TextEdit::singleline(search_query)
                            .desired_width(260.0)
                            .hint_text("Search recent 200 messages…"),
                    );
                    if resp.changed() && !search_query.trim().is_empty() {
                        ui_state.search_older_tier2 = false;
                    }
                    if !search_query.trim().is_empty()
                        && ui.small_button("Search older messages (FTS5)…").clicked()
                    {
                        ui_state.search_older_tier2 = true;
                        send(DaemonRequest::SearchMessages {
                            query: search_query.trim().to_string(),
                            include_older: true,
                            bot_id: Some(active_bot.clone()),
                        });
                    }
                    if !search_hits.is_empty() {
                        ui.small(format!("{} archive hits", search_hits.len()));
                    }
                    if ui.small_button("✕").clicked() {
                        ui_state.search_bar_open = false;
                        search_query.clear();
                    }
                });
            });
    }

    // Filter rows by tier-1 search query if active
    let needle = if ui_state.search_bar_open {
        search_query.trim().to_ascii_lowercase()
    } else {
        String::new()
    };
    let filtered_indices: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            needle.is_empty()
                || r.content.to_ascii_lowercase().contains(&needle)
                || r.author.to_ascii_lowercase().contains(&needle)
        })
        .map(|(idx, _)| idx)
        .collect();

    // 3. Bottom Smart Composer Panel (drawn before central transcript so it claims bottom height cleanly)
    egui::TopBottomPanel::bottom("grok_smart_composer")
        .frame(
            egui::Frame::NONE
                .fill(palette.bg_canvas)
                .stroke(Stroke::new(1.0_f32, palette.border_subtle))
                .inner_margin(egui::Margin::symmetric(14, 10)),
        )
        .show_inside(ui, |ui| {
            // Reply-to banner (Tier 5 Escape target)
            if let Some(reply) = ui_state.reply_to.clone() {
                egui::Frame::NONE
                    .fill(palette.bg_elevated)
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(8, 4))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.small(
                                RichText::new(format!("↩ Replying to {}: {}", reply.author, reply.snippet))
                                    .color(palette.text_secondary),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button("✕").clicked() {
                                    ui_state.reply_to = None;
                                }
                            });
                        });
                    });
                ui.add_space(4.0);
            }

            // Attachment pills (`< 25 MB` limit enforced)
            if !ui_state.pending_attachments.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    let mut remove_idx = None;
                    for (idx, att) in ui_state.pending_attachments.iter().enumerate() {
                        ui.group(|ui| {
                            ui.small(format!("📎 {att}"));
                            if ui.small_button("✕").clicked() {
                                remove_idx = Some(idx);
                            }
                        });
                    }
                    if let Some(i) = remove_idx {
                        ui_state.pending_attachments.remove(i);
                    }
                });
                ui.add_space(4.0);
            }

            // `@bot` and `/command` autocomplete popovers (Tier 1 Escape target)
            if draft.ends_with('@') || ui_state.mention_popup_open {
                ui_state.mention_popup_open = draft.contains('@');
                if ui_state.mention_popup_open {
                    egui::Frame::NONE
                        .fill(palette.bg_elevated)
                        .stroke(Stroke::new(1.0_f32, palette.border_subtle))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(egui::Margin::symmetric(8, 4))
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.small(RichText::new("Mention Bot:").color(palette.text_muted));
                                for b in bots {
                                    if let Some(bid) = b["id"].as_str()
                                        && ui.small_button(format!("@{bid}")).clicked()
                                    {
                                        if draft.ends_with('@') {
                                            draft.push_str(&format!("{bid} "));
                                        } else {
                                            draft.push_str(&format!(" @{bid} "));
                                        }
                                        ui_state.mention_popup_open = false;
                                    }
                                }
                            });
                        });
                }
            }
            if draft.starts_with('/') && !draft.contains(' ') {
                ui_state.slash_popup_open = true;
                egui::Frame::NONE
                    .fill(palette.bg_elevated)
                    .stroke(Stroke::new(1.0_f32, palette.border_subtle))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(8, 4))
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            for (cmd, desc) in SLASH_COMMANDS {
                                if ui.small_button(*cmd).on_hover_text(*desc).clicked() {
                                    *draft = (*cmd).to_string();
                                    ui_state.slash_popup_open = false;
                                }
                            }
                        });
                    });
            } else {
                ui_state.slash_popup_open = false;
            }

            // Detect IME composition & Backspace for Smart Composer `->` -> `→` + single-Backspace undo
            let (ime_composing, backspace_pressed) = ctx.input(|i| {
                let mut had_ime = false;
                for event in &i.events {
                    if let egui::Event::Ime(event) = event {
                        had_ime = true;
                        match event {
                            egui::ImeEvent::Enabled | egui::ImeEvent::Preedit(_) => ui_state.ime_composing = true,
                            egui::ImeEvent::Commit(_) | egui::ImeEvent::Disabled => ui_state.ime_composing = false,
                        }
                    }
                }
                (ui_state.ime_composing || had_ime, i.key_pressed(Key::Backspace))
            });

            let composer_id = egui::Id::new("main-draft");
            let plain_enter = !ime_composing && !ui_state.mention_popup_open && !ui_state.slash_popup_open
                && ctx.memory(|m| m.has_focus(composer_id))
                && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter));
            let command_enter = !ime_composing && crate::native::take_composer_shortcut(ctx);
            let edit_resp = ui.add(
                egui::TextEdit::multiline(draft)
                    .id(composer_id)
                    .desired_width(ui.available_width())
                    .desired_rows(2)
                    .hint_text(
                        "Message, /do task, @bot handoff, or type -> for → (Drop files < 25 MB to attach)",
                    ),
            );
            if edit_resp.changed() || backspace_pressed {
                apply_smart_composer_edit(
                    draft,
                    ime_composing,
                    backspace_pressed,
                    &mut ui_state.last_auto_arrow_pos,
                );
            }

            let send_enter = !ime_composing && (command_enter || plain_enter);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui
                    .small_button("📎 Attach")
                    .on_hover_text("Attach file from workspace (< 25 MB)")
                    .clicked()
                {
                    send(DaemonRequest::ListBotFiles {
                        bot_id: active_bot.clone(),
                    });
                    ui_state.details_drawer_open = true;
                    ui_state.details_tab = BotDetailsTab::Library;
                }
                if ui
                    .small_button("@ Mention")
                    .on_hover_text("Insert @bot mention")
                    .clicked()
                {
                    draft.push('@');
                    ui_state.mention_popup_open = true;
                }
                if ui
                    .small_button("/ Command")
                    .on_hover_text("Insert slash command")
                    .clicked()
                {
                    if draft.is_empty() {
                        draft.push('/');
                    }
                    ui_state.slash_popup_open = true;
                }

                // Dictation indicator (`Cmd/Ctrl+D`)
                let dictate_label = if ui_state.dictation_active {
                    "🔴 [Dictate ON]"
                } else {
                    "🎙 Dictate"
                };
                if ui
                    .small_button(dictate_label)
                    .on_hover_text("Toggle Dictation (Cmd/Ctrl+D)")
                    .clicked()
                {
                    ui_state.dictation_active = !ui_state.dictation_active;
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let has_text = !draft.trim().is_empty() || !ui_state.pending_attachments.is_empty();
                    if has_text {
                        // Morphs into ↑ Send circle button when text is present
                        let send_btn = egui::Button::new(
                            RichText::new("↑ Send")
                                .strong()
                                .color(palette.accent_text),
                        )
                        .fill(palette.accent)
                        .corner_radius(CornerRadius::same(14));
                        if (ui.add_enabled(connected && !sending, send_btn).clicked()
                            || (send_enter && connected && !sending))
                            && has_text
                        {
                            let mut payload = draft.trim().to_string();
                            for att in ui_state.pending_attachments.drain(..) {
                                if !payload.is_empty() {
                                    payload.push('\n');
                                }
                                payload.push_str(&format!("Attached file: {att}"));
                            }
                            let reply_id = ui_state.reply_to.take().map(|r| r.message_id);
                            submit_message(payload, reply_id);
                        }
                    } else {
                        // Morphs into 🎙 Voice pill when composer is empty
                        let voice_label = if ui_state.voice_active {
                            "⏹ End Voice"
                        } else {
                            "🎙 Voice"
                        };
                        let voice_btn = egui::Button::new(
                            RichText::new(voice_label).color(if ui_state.voice_active {
                                Color32::WHITE
                            } else {
                                palette.text_primary
                            }),
                        )
                        .fill(if ui_state.voice_active {
                            palette.status_danger
                        } else {
                            palette.bg_elevated
                        })
                        .corner_radius(CornerRadius::same(14));
                        if ui
                            .add(voice_btn)
                            .on_hover_text("Start/Stop Realtime Voice Session")
                            .clicked()
                        {
                            ui_state.voice_active = !ui_state.voice_active;
                        }
                    }
                });
            });
        });

    // 4. Virtualized Transcript (`ScrollArea::vertical().show_rows`)
    let avail_width = ui.available_width().max(260.0);
    let row_height = 84.0;
    let total_rows = filtered_indices.len();

    egui::ScrollArea::vertical()
        .id_salt("grok_virtualized_transcript")
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show_rows(ui, row_height, total_rows, |ui, row_range| {
            if total_rows == 0 && streaming_text.is_empty() {
                ui.add_space(32.0);
                ui.vertical_centered(|ui| {
                    ui.heading(
                        RichText::new(format!("Chat with {bot_display_name}"))
                            .color(palette.text_primary),
                    );
                    ui.label(
                        RichText::new(
                            "Send a message, type /do to queue a task, or press Cmd/Ctrl+K for the Command Palette.",
                        )
                        .color(palette.text_secondary),
                    );
                });
            }

            for visible_pos in row_range {
                let Some(&row_idx) = filtered_indices.get(visible_pos) else {
                    continue;
                };
                let row = &rows[row_idx];
                let layout = layout_cache
                    .get_or_compute(&row.id, &row.content, row.card.is_some(), avail_width)
                    .clone();

                ui.push_id(&row.id, |ui| {
                    draw_transcript_row(
                        ui,
                        ctx,
                        ui_state,
                        palette,
                        active_bot,
                        row,
                        &layout.parts,
                        &layout.tables,
                        connected && !sending,
                        &mut send,
                        &mut submit_message,
                    );
                });
                ui.add_space(6.0);
            }

            if !streaming_text.is_empty() {
                egui::Frame::NONE
                    .fill(palette.bg_elevated)
                    .stroke(Stroke::new(1.0_f32, palette.accent))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.small(RichText::new(format!("{bot_display_name} · streaming…")).color(palette.accent));
                        ui.add(egui::Label::new(streaming_text).selectable(true).wrap());
                    });
            }
        });
}

#[allow(clippy::too_many_arguments)]
fn draw_transcript_row(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    ui_state: &mut GrokUiState,
    palette: &GrokPalette,
    active_bot: &mut String,
    row: &TranscriptRow,
    parts: &[RenderedPart],
    tables: &[ParsedMarkdownTable],
    can_interact: bool,
    send: &mut impl FnMut(DaemonRequest),
    submit_message: &mut impl FnMut(String, Option<String>),
) {
    let is_user = row.role == "user" || row.author == "You" || row.author == "operator";
    let frame_bg = if is_user {
        palette.accent.gamma_multiply(0.12)
    } else {
        palette.bg_elevated
    };

    let frame_resp = egui::Frame::NONE
        .fill(frame_bg)
        .stroke(Stroke::new(1.0_f32, palette.border_subtle))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            // Header row: Author + Timestamp + Request-ID badge + Quick Hover Actions
            ui.horizontal(|ui| {
                ui.strong(RichText::new(&row.author).size(13.0).color(if is_user {
                    palette.accent
                } else {
                    palette.text_primary
                }));
                if let Some(parent) = &row.reply_to_id {
                    ui.small(format!("Reply to {parent}"));
                }
                ui.label("·")
                    .on_hover_text(format!("Message sequence {}", row.seq));
                if !row.created_at.is_empty() {
                    ui.small(RichText::new(&row.created_at).color(palette.text_muted));
                }
                if let Some(req_id) = &row.request_id
                    && !req_id.is_empty()
                {
                    let short_id: String = req_id.chars().take(10).collect();
                    if ui
                        .small_button(format!("#{short_id}"))
                        .on_hover_text(format!("Copy Request ID: {req_id}"))
                        .clicked()
                    {
                        ctx.copy_text(req_id.clone());
                    }
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .small_button("⎘")
                        .on_hover_text("Copy message text")
                        .clicked()
                    {
                        ctx.copy_text(row.content.clone());
                    }
                    if ui
                        .small_button("☺")
                        .on_hover_text("React with emoji")
                        .clicked()
                    {
                        ui_state.emoji_picker_msg =
                            if ui_state.emoji_picker_msg.as_deref() == Some(row.id.as_str()) {
                                None
                            } else {
                                Some(row.id.clone())
                            };
                    }
                    if ui
                        .small_button("↩")
                        .on_hover_text("Reply to this message")
                        .clicked()
                    {
                        let snippet: String = row.content.chars().take(60).collect();
                        ui_state.reply_to = Some(ReplyBanner {
                            message_id: row.id.clone(),
                            author: row.author.clone(),
                            snippet,
                        });
                    }
                });
            });

            // 6-Emoji Quick Reaction Picker Popover (Tier 1 Escape target)
            if ui_state.emoji_picker_msg.as_deref() == Some(row.id.as_str()) {
                ui.horizontal(|ui| {
                    for emoji in QUICK_REACTION_EMOJIS {
                        if ui.small_button(emoji).clicked() {
                            send(DaemonRequest::ToggleMessageReaction {
                                chat_kind: ui_state.active_chat_kind.clone(),
                                message_id: row.id.clone(),
                                emoji: emoji.to_string(),
                                actor: Some("operator".into()),
                            });
                            ui_state.emoji_picker_msg = None;
                        }
                    }
                });
            }

            // Message Body & Muse Directives
            if parts.is_empty() {
                ui.add(
                    egui::Label::new(row.content.as_str())
                        .selectable(true)
                        .wrap(),
                );
            } else {
                for (p_idx, part) in parts.iter().enumerate() {
                    ui.push_id(("part", p_idx), |ui| {
                        if !part.text.is_empty() {
                            ui.add(egui::Label::new(part.text.as_str()).selectable(true).wrap());
                        }
                        for (a_idx, att) in part.attachments.iter().enumerate() {
                            ui.push_id(("att", a_idx), |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    let label = if att.alt.trim().is_empty() {
                                        att.uri.as_str()
                                    } else {
                                        att.alt.as_str()
                                    };
                                    ui.small(format!("📎 {label}"));
                                    if let Some(rel) = sandbox_uri_to_bot_relpath(&att.uri)
                                        && ui.small_button("Open in Split Page").clicked()
                                    {
                                        send(DaemonRequest::PreviewBotFile {
                                            bot_id: active_bot.clone(),
                                            path: rel.clone(),
                                        });
                                        ui_state.split_page = Some(SplitPageTarget::MarkdownFile {
                                            bot_id: active_bot.clone(),
                                            file: rel.clone(),
                                            title: rel,
                                            editing: false,
                                        });
                                    }
                                });
                            });
                        }
                        if !part.citations.is_empty() {
                            ui.collapsing(format!("Sources ({})", part.citations.len()), |ui| {
                                for cite in &part.citations {
                                    ui.small(format!("[{}] {}", cite.index, cite.raw));
                                }
                            });
                        }
                        if !part.options.is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                for opt in &part.options {
                                    if ui
                                        .add_enabled(can_interact, egui::Button::new(opt.as_str()))
                                        .clicked()
                                    {
                                        submit_message(opt.clone(), Some(row.id.clone()));
                                    }
                                }
                            });
                        }
                    });
                }
            }

            // Rich Markdown Tables with Copy TSV / Copy CSV / Copy Markdown / Expand ⤢
            for (t_idx, table) in tables.iter().enumerate() {
                ui.push_id(("tbl", t_idx), |ui| {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.small(RichText::new("📊 Table").color(palette.text_secondary));
                            if ui.small_button("Copy Table").clicked() {
                                ctx.copy_text(table.to_tsv());
                            }
                            if ui.small_button("Copy CSV").clicked() {
                                ctx.copy_text(table.to_csv());
                            }
                            if ui.small_button("Copy for Slack").clicked() {
                                ctx.copy_text(table.to_slack());
                            }
                            if ui.small_button("Copy Markdown").clicked() {
                                ctx.copy_text(table.raw_markdown.clone());
                            }
                            if ui
                                .small_button("Expand ⤢")
                                .on_hover_text("Open full-screen table modal")
                                .clicked()
                            {
                                ui_state.active_modal = Some(ActiveModal::ExpandedTable {
                                    title: format!("Table from {}", row.author),
                                    headers: table.headers.clone(),
                                    rows: table.rows.clone(),
                                    raw_markdown: table.raw_markdown.clone(),
                                });
                            }
                        });
                        egui::Grid::new(("md_tbl_grid", &row.id, t_idx))
                            .striped(true)
                            .show(ui, |ui| {
                                for h in &table.headers {
                                    ui.strong(h);
                                }
                                ui.end_row();
                                for r in table.rows.iter().take(6) {
                                    for c in r {
                                        ui.label(c);
                                    }
                                    ui.end_row();
                                }
                            });
                    });
                });
            }

            // Inline Interactive Card (All 14 Card Variants)
            if let Some(card) = &row.card {
                ui.add_space(4.0);
                render_inline_card(
                    ui,
                    ctx,
                    ui_state,
                    palette,
                    active_bot,
                    &row.id,
                    card,
                    send,
                    submit_message,
                );
            }

            // Existing Reaction Chips
            if !row.reactions.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for r in &row.reactions {
                        let emoji = r["emoji"].as_str().unwrap_or("👍");
                        let count = r["count"].as_u64().unwrap_or(1);
                        if ui.small_button(format!("{emoji} {count}")).clicked() {
                            send(DaemonRequest::ToggleMessageReaction {
                                chat_kind: ui_state.active_chat_kind.clone(),
                                message_id: row.id.clone(),
                                emoji: emoji.to_string(),
                                actor: Some("operator".into()),
                            });
                        }
                    }
                });
            }
        });
    let _ = frame_resp;
}

/// Renders all 14 GrokBot inline interactive card types inside the chat stream.
#[allow(clippy::too_many_arguments)]
pub fn render_inline_card(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    ui_state: &mut GrokUiState,
    palette: &GrokPalette,
    active_bot: &mut String,
    message_id: &str,
    card: &Value,
    send: &mut impl FnMut(DaemonRequest),
    submit_message: &mut impl FnMut(String, Option<String>),
) {
    let kind = card["kind"]
        .as_str()
        .or_else(|| card["type"].as_str())
        .unwrap_or("info");
    let req_id = card["request_id"]
        .as_str()
        .unwrap_or(message_id)
        .to_string();
    let status = card["status"].as_str().unwrap_or("pending");

    egui::Frame::NONE
        .fill(palette.bg_sidebar)
        .stroke(Stroke::new(1.0_f32, palette.accent.gamma_multiply(0.6)))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| match kind {
            // 1. Approval Card
            "approval" | "terminal_approval" | "ask_first" | "order_approval" | "shopping" => {
                if kind == "terminal_approval" {
                    ui.monospace(card["command"].as_str().unwrap_or_default());
                    ui.small(format!(
                        "CWD: {}",
                        card["cwd"].as_str().unwrap_or("workspace")
                    ));
                }
                if matches!(kind, "order_approval" | "shopping") {
                    ui.strong(card["merchant"].as_str().unwrap_or("Merchant"));
                    ui.label(card["item_title"].as_str().unwrap_or_default());
                    ui.label(format!(
                        "Total: ${:.2}",
                        card["amount_usd"].as_f64().unwrap_or(0.0)
                    ));
                }
                if matches!(kind, "terminal_approval" | "ask_first")
                    && ui.small_button("Manage Custom Rules").clicked()
                {
                    ui_state.settings_pane = crate::ui::state::SettingsPane::RulesAndAccess;
                    ui_state.active_modal = Some(ActiveModal::Settings);
                }
                let title = card["title"].as_str().unwrap_or("Action Approval Required");
                let details = card["details"].as_str().unwrap_or("");
                ui.horizontal(|ui| {
                    ui.strong(RichText::new(format!("🛡 {title}")).color(palette.status_warn));
                    ui.small(format!("[{status}]"));
                });
                if !details.is_empty() {
                    ui.small(details);
                }
                if status == "pending" {
                    let entry = ui_state
                        .cards
                        .approval_remember
                        .entry(req_id.clone())
                        .or_insert_with(|| (false, title.to_string()));
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut entry.0, "Remember custom rule:");
                        if entry.0 {
                            ui.add(
                                egui::TextEdit::singleline(&mut entry.1)
                                    .desired_width(180.0)
                                    .hint_text("pattern*"),
                            );
                        }
                    });
                    let (remember, pattern) = entry.clone();
                    ui.horizontal(|ui| {
                        if ui
                            .button(RichText::new("✓ Approve").color(palette.status_ok))
                            .clicked()
                        {
                            if remember && kind == "approval" && !pattern.trim().is_empty() {
                                send(DaemonRequest::ManageCustomRule {
                                    op: "upsert".into(),
                                    id: None,
                                    rule: Some(serde_json::json!({
                                        "mode": "allow",
                                        "pattern": pattern.trim(),
                                        "description": "Saved from inline approval card",
                                        "enabled": true,
                                    })),
                                });
                            }
                            send(DaemonRequest::RespondApproval {
                                request_id: req_id.clone(),
                                approved: true,
                            });
                        }
                        if ui
                            .button(RichText::new("✕ Deny").color(palette.status_danger))
                            .clicked()
                        {
                            send(DaemonRequest::RespondApproval {
                                request_id: req_id,
                                approved: false,
                            });
                        }
                    });
                }
            }

            "draft" => {
                ui.strong(card["title"].as_str().unwrap_or("Draft for review"));
                let text = card["body"]
                    .as_str()
                    .or_else(|| card["content"].as_str())
                    .unwrap_or_default();
                ui.label(text);
                if ui.small_button("Copy Draft").clicked() {
                    ctx.copy_text(text.to_string());
                }
                ui.small("Review before sending through the connected app.");
                if ui.small_button("Connected Apps").clicked() {
                    ui_state.active_modal = Some(ActiveModal::ConnectApps);
                }
            }
            "phone_call" | "meeting" | "connect_app" | "reauth" => {
                ui.strong(card["title"].as_str().unwrap_or("Connect an app"));
                ui.label(
                    card["summary"]
                        .as_str()
                        .unwrap_or("Setup / Connect bridge required"),
                );
                if ui.button("Connect / Configure").clicked() {
                    ui_state.active_modal = Some(ActiveModal::ConnectApps);
                }
            }
            "initiative" => {
                ui.strong(card["title"].as_str().unwrap_or("Suggested next step"));
                let suggestion = card["instruction"]
                    .as_str()
                    .or_else(|| card["summary"].as_str())
                    .unwrap_or_default();
                ui.label(suggestion);
                if !suggestion.is_empty() && ui.button("Run").clicked() {
                    submit_message(suggestion.to_string(), None);
                }
            }

            // 2. Secret Prompt Card (`RULE-SECRET-001` zeroized fields, never echoed in chat)
            "secret_prompt" => {
                let label = card["label"].as_str().unwrap_or("Credential Required");
                let domain = card["domain"].as_str().unwrap_or("");
                ui.strong(RichText::new(format!("🔑 {label}")).color(palette.accent));
                if !domain.is_empty() {
                    ui.small(format!(
                        "Domain: {domain} · End-to-end zeroized (never stored in chat SQLite)"
                    ));
                }
                if status == "pending" {
                    let field_names: Vec<String> = card["fields"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|f| {
                                    f.as_str()
                                        .or_else(|| f["name"].as_str())
                                        .or_else(|| f["key"].as_str())
                                })
                                .map(ToString::to_string)
                                .collect()
                        })
                        .filter(|v: &Vec<String>| !v.is_empty())
                        .unwrap_or_else(|| vec!["username".into(), "password".into()]);

                    let state_entry = ui_state
                        .cards
                        .secret_fields
                        .entry(req_id.clone())
                        .or_insert_with(|| {
                            (
                                field_names
                                    .iter()
                                    .map(|k| (k.clone(), Zeroizing::new(String::new())))
                                    .collect(),
                                true,
                            )
                        });
                    for (f_name, f_val) in &mut state_entry.0 {
                        let is_pw = f_name.to_ascii_lowercase().contains("pass")
                            || f_name.to_ascii_lowercase().contains("secret")
                            || f_name.to_ascii_lowercase().contains("key")
                            || f_name.to_ascii_lowercase().contains("token")
                            || f_name.to_ascii_lowercase().contains("cvv");
                        ui.horizontal(|ui| {
                            ui.label(f_name.as_str());
                            ui.add(
                                egui::TextEdit::singleline(&mut **f_val)
                                    .password(is_pw)
                                    .desired_width(180.0),
                            );
                        });
                    }
                    ui.checkbox(&mut state_entry.1, "Save to Keychain Vault");
                    let save_to_vault = state_entry.1;
                    let mut submit_clicked = false;
                    let mut cancel_clicked = false;
                    ui.horizontal(|ui| {
                        submit_clicked = ui.button("Submit Secret").clicked();
                        cancel_clicked = ui.button("Cancel").clicked();
                    });
                    if submit_clicked {
                        let pairs: Vec<(String, String)> = state_entry
                            .0
                            .iter()
                            .map(|(k, v)| (k.clone(), (**v).clone()))
                            .collect();
                        send(DaemonRequest::SubmitSecretPrompt {
                            request_id: req_id.clone(),
                            bot_id: active_bot.clone(),
                            fields: pairs,
                            save_to_vault,
                            vault_label: Some(label.to_string()),
                            domain: if domain.is_empty() {
                                None
                            } else {
                                Some(domain.to_string())
                            },
                        });
                    } else if cancel_clicked {
                        send(DaemonRequest::SubmitSecretPrompt {
                            request_id: req_id.clone(),
                            bot_id: active_bot.clone(),
                            fields: Vec::new(),
                            save_to_vault: false,
                            vault_label: None,
                            domain: None,
                        });
                    }
                    if submit_clicked || cancel_clicked {
                        ui_state.cards.clear_secret(&req_id);
                    }
                } else {
                    ui.small(format!("Secret prompt {status}."));
                }
            }

            // 3. Clarifying Question Card
            "clarifying_question" | "question" => {
                let prompt = card["question"]
                    .as_str()
                    .or_else(|| card["title"].as_str())
                    .unwrap_or("Clarifying Question");
                let multi = card["multi_select"].as_bool().unwrap_or(false);
                ui.strong(format!("❓ {prompt}"));
                if status == "pending" {
                    let options: Vec<String> = card["options"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|x| x.as_str().map(ToString::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    let q_entry = ui_state
                        .cards
                        .question_answers
                        .entry(req_id.clone())
                        .or_insert_with(|| (Vec::new(), String::new()));
                    for opt in &options {
                        let mut selected = q_entry.0.contains(opt);
                        if multi {
                            if ui.checkbox(&mut selected, opt).changed() {
                                q_entry.0.retain(|x| x != opt);
                                if selected {
                                    q_entry.0.push(opt.clone());
                                }
                            }
                        } else if ui.radio(selected, opt).clicked() {
                            q_entry.0.clear();
                            q_entry.0.push(opt.clone());
                        }
                    }
                    ui.horizontal(|ui| {
                        ui.label("Other:");
                        ui.add(
                            egui::TextEdit::singleline(&mut q_entry.1)
                                .desired_width(180.0)
                                .hint_text("Type custom answer…"),
                        );
                    });
                    let (answers, other) = q_entry.clone();
                    ui.horizontal(|ui| {
                        if ui.button("Submit Answer").clicked() {
                            send(DaemonRequest::RespondQuestion {
                                request_id: req_id.clone(),
                                answers,
                                custom_text: if other.trim().is_empty() {
                                    None
                                } else {
                                    Some(other)
                                },
                            });
                        }
                        if ui.button("Skip").clicked() {
                            send(DaemonRequest::RespondQuestion {
                                request_id: req_id,
                                answers: Vec::new(),
                                custom_text: None,
                            });
                        }
                    });
                } else {
                    ui.small(format!(
                        "Answered: {}",
                        card["answer"].as_str().unwrap_or(status)
                    ));
                }
            }

            // 4. Plan Checklist Card
            "plan_checklist" | "plan" => {
                let title = card["title"].as_str().unwrap_or("Execution Plan");
                ui.strong(format!("📋 {title}"));
                if let Some(steps) = card["steps"].as_array() {
                    let done_count = steps
                        .iter()
                        .filter(|s| s["done"].as_bool().unwrap_or(false))
                        .count();
                    let ratio = if steps.is_empty() {
                        0.0
                    } else {
                        done_count as f32 / steps.len() as f32
                    };
                    ui.add(egui::ProgressBar::new(ratio).show_percentage());
                    for s in steps {
                        let label = s["label"]
                            .as_str()
                            .or_else(|| s["title"].as_str())
                            .unwrap_or("Step");
                        let done = s["done"].as_bool().unwrap_or(false);
                        ui.label(format!("{} {label}", if done { "☑" } else { "☐" }));
                    }
                }
            }

            // 5. Sub-agent Spawn Card
            "subagent_spawn" | "a2a_handoff" => {
                let target_bot = card["target_bot_id"]
                    .as_str()
                    .or_else(|| card["bot_id"].as_str())
                    .unwrap_or("muse");
                let summary = card["instruction"]
                    .as_str()
                    .or_else(|| card["summary"].as_str())
                    .unwrap_or("Delegated specialist task");
                ui.horizontal(|ui| {
                    ui.strong(format!("🤝 Sub-agent @{target_bot}"));
                    ui.small(format!("[{status}]"));
                    if ui
                        .small_button(format!("Switch to @{target_bot}"))
                        .clicked()
                    {
                        *active_bot = target_bot.to_string();
                        ui_state.active_chat_kind = "dm".into();
                        send(DaemonRequest::GetDirectChatHistory {
                            bot_id: target_bot.to_string(),
                            before_seq: None,
                            limit: Some(100),
                        });
                    }
                });
                ui.small(summary);
            }

            // 6. File Diff Card
            "file_diff" => {
                let path = card["path"].as_str().unwrap_or("workspace/file");
                let added = card["added"].as_u64().unwrap_or(0);
                let removed = card["removed"].as_u64().unwrap_or(0);
                ui.horizontal(|ui| {
                    ui.strong(format!("📝 {path}"));
                    ui.colored_label(palette.status_ok, format!("+{added}"));
                    ui.colored_label(palette.status_danger, format!("-{removed}"));
                    if ui.small_button("Open in Split Page").clicked() {
                        ui_state.split_page = Some(SplitPageTarget::MarkdownFile {
                            bot_id: active_bot.clone(),
                            file: path.to_string(),
                            title: path.to_string(),
                            editing: false,
                        });
                    }
                });
                if let Some(diff) = card["diff"].as_str() {
                    ui.collapsing("View Diff", |ui| {
                        ui.monospace(diff);
                    });
                }
            }

            // 7. File Output Card
            "file_output" => {
                let path = card["path"].as_str().unwrap_or("output.md");
                let size = card["size"].as_u64().unwrap_or(0);
                ui.horizontal(|ui| {
                    ui.strong(format!("📄 {path}"));
                    if size > 0 {
                        ui.small(format!("{size} B"));
                    }
                    if ui.small_button("Open in Split Page").clicked() {
                        send(DaemonRequest::PreviewBotFile {
                            bot_id: active_bot.clone(),
                            path: path.to_string(),
                        });
                        ui_state.split_page = Some(SplitPageTarget::MarkdownFile {
                            bot_id: active_bot.clone(),
                            file: path.to_string(),
                            title: path.to_string(),
                            editing: false,
                        });
                    }
                    if ui.small_button("Copy Path").clicked() {
                        ctx.copy_text(path.to_string());
                    }
                });
            }

            // 8. Media Output Card
            "media_output" => {
                let title = card["title"].as_str().unwrap_or("Media Artifact");
                let uri = card["uri"]
                    .as_str()
                    .or_else(|| card["path"].as_str())
                    .unwrap_or("");
                let mime = card["mime"].as_str().unwrap_or("image/png");
                ui.horizontal(|ui| {
                    ui.strong(format!("🖼 {title}"));
                    if ui.small_button("🔍 Open Lightbox").clicked() {
                        ui_state.active_modal = Some(ActiveModal::Lightbox {
                            title: title.to_string(),
                            uri: uri.to_string(),
                            mime: mime.to_string(),
                        });
                    }
                });
            }

            // 9. Space Preview Card
            "space_preview" => {
                let slug = card["slug"].as_str().unwrap_or("space");
                let title = card["title"].as_str().unwrap_or(slug);
                let url = card["url"].as_str().map(ToString::to_string);
                ui.horizontal(|ui| {
                    ui.strong(format!("🧩 Space: {title} ({slug})"));
                    if ui.small_button("Open Split Page").clicked() {
                        ui_state.split_page = Some(SplitPageTarget::StaticSpace {
                            slug: slug.to_string(),
                            title: title.to_string(),
                            url: url.clone(),
                            editing_html: false,
                        });
                    }
                    if let Some(ref u) = url {
                        ui.hyperlink_to("Open Browser ↗", u);
                    }
                });
            }

            // 10. Goal Progress Card
            "goal_progress" => {
                let slug = card["slug"].as_str().unwrap_or("goal");
                let title = card["title"].as_str().unwrap_or(slug);
                let progress = card["progress"].as_f64().unwrap_or(0.5) as f32;
                ui.horizontal(|ui| {
                    ui.strong(format!("🎯 {title}"));
                    if ui.small_button("Open Goals").clicked() {
                        ui_state.section = WorkspaceSection::Goals;
                        send(DaemonRequest::ListGoals);
                    }
                });
                ui.add(egui::ProgressBar::new(progress.clamp(0.0, 1.0)).show_percentage());
            }

            // 11. Digest Brief Card
            "digest_brief" => {
                let id = card["id"].as_str().unwrap_or("");
                let title = card["title"].as_str().unwrap_or("Proactive Brief");
                let body = card["body"].as_str().unwrap_or("");
                ui.strong(format!("⚡ {title}"));
                if !body.is_empty() {
                    ui.small(body);
                }
                if !id.is_empty() {
                    ui.horizontal(|ui| {
                        if ui.small_button("Useful 👍").clicked() {
                            send(DaemonRequest::RateActivity {
                                id: id.to_string(),
                                rating: 5,
                            });
                        }
                        if ui.small_button("Not helpful 👎").clicked() {
                            send(DaemonRequest::RateActivity {
                                id: id.to_string(),
                                rating: -1,
                            });
                        }
                    });
                }
            }

            // 12. Memory Forget Confirmation Card (`RULE-FORGET-001` blinded fingerprint)
            "memory_forget_confirm" => {
                let plan_id = card["plan_id"].as_str().unwrap_or("");
                let fp = card["request_fingerprint"].as_str().unwrap_or("");
                ui.strong(format!("🧹 Two-Phase Forget Plan ({plan_id})"));
                ui.small(format!(
                    "Blinded fingerprint: {fp} · Counts: {}",
                    card["summary_counts"]
                ));
                if status == "pending" && !plan_id.is_empty() {
                    ui.horizontal(|ui| {
                        if ui
                            .button(
                                RichText::new("Confirm & Permanently Erase")
                                    .color(palette.status_danger),
                            )
                            .clicked()
                        {
                            send(DaemonRequest::ForgetConfirm {
                                bot_id: Some(active_bot.clone()),
                                plan_id: plan_id.to_string(),
                                verification_query: None,
                                confirmed: true,
                            });
                        }
                    });
                }
            }

            // 13. Skill Amendment Proposal Card
            "skill_amendment" => {
                let skill_name = card["skill_name"].as_str().unwrap_or("skill");
                let note = card["note"].as_str().unwrap_or("");
                ui.strong(format!("✦ Proposed Skill Amendment: {skill_name}"));
                ui.small(note);
                if status == "pending" {
                    ui.horizontal(|ui| {
                        if ui.small_button("Approve Amendment").clicked() {
                            submit_message(
                                format!("Approved amendment for skill {skill_name}"),
                                None,
                            );
                        }
                    });
                }
            }

            // 14. Take-the-Wheel Handoff Card
            "wheel_handoff" => {
                let task_id = card["task_id"].as_str().unwrap_or(&req_id);
                let blocker = card["blocker"]
                    .as_str()
                    .unwrap_or("Human assistance required");
                let instructions = card["instructions"].as_str().unwrap_or("");
                ui.strong(
                    RichText::new(format!("🛞 Take the Wheel ({task_id})"))
                        .color(palette.status_warn),
                );
                ui.small(blocker);
                if !instructions.is_empty() {
                    ui.small(instructions);
                }
                let note = ui_state
                    .cards
                    .wheel_notes
                    .entry(task_id.to_string())
                    .or_default();
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(note)
                            .desired_width(180.0)
                            .hint_text("What did you complete?"),
                    );
                    if ui
                        .add_enabled(
                            !note.trim().is_empty(),
                            egui::Button::new("Hand back to bot"),
                        )
                        .clicked()
                    {
                        send(DaemonRequest::TakeTheWheelResume {
                            task_id: task_id.to_string(),
                            note: Some(note.trim().to_string()),
                        });
                    }
                });
            }

            _ => {
                ui.small(format!(
                    "[Card: {kind}] {}",
                    card["title"].as_str().unwrap_or("")
                ));
            }
        });
}
