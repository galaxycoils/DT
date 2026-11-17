//! Adaptive Bot / Group / Section Sidebar (`AutoCollapsed` `<= 1`, `CompactRail` `60px` for `2..=4`,
//! `FullList` `240px` for `5+`), Vector Bot Avatars, Unread Badges, Context Menus, and `Hidden Bots (N)`.

use crate::ui::cache::BoundedTextureCache;
use crate::ui::state::{ActiveModal, GrokUiState, SidebarMode};
use crate::ui::theme::{BotAvatarShape, GrokPalette, paint_bot_avatar, parse_hex_color};
use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Sense, Stroke, Vec2};
use gm_core::ipc::DaemonRequest;
use serde_json::Value;

#[allow(clippy::too_many_arguments)]
pub fn draw_adaptive_sidebar(
    ctx: &egui::Context,
    ui_state: &mut GrokUiState,
    palette: &GrokPalette,
    textures: &mut BoundedTextureCache,
    bots: &[Value],
    sections: &[Value],
    channels: &[Value],
    unread_counts: &Value,
    active_bot: &mut String,
    active_channel: &mut String,
    mut send: impl FnMut(DaemonRequest),
) {
    let visible_bots: Vec<&Value> = bots
        .iter()
        .filter(|b| !b["hidden_from_sidebar"].as_bool().unwrap_or(false))
        .collect();
    let hidden_bots: Vec<&Value> = bots
        .iter()
        .filter(|b| b["hidden_from_sidebar"].as_bool().unwrap_or(false))
        .collect();

    let mode =
        SidebarMode::from_visible_bot_count(visible_bots.len(), ui_state.sidebar_manual_override);

    if mode == SidebarMode::AutoCollapsed {
        return;
    }

    if mode == SidebarMode::CompactRail {
        egui::SidePanel::left("grok_compact_bot_rail")
            .exact_width(mode.width_px())
            .resizable(false)
            .frame(
                egui::Frame::NONE
                    .fill(palette.bg_sidebar)
                    .stroke(Stroke::new(1.0_f32, palette.border_subtle))
                    .inner_margin(egui::Margin::symmetric(8, 8)),
            )
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    if ui
                        .small_button("»")
                        .on_hover_text("Expand full bot sidebar")
                        .clicked()
                    {
                        ui_state.sidebar_manual_override = Some(true);
                    }
                    ui.add_space(6.0);
                    for bot in &visible_bots {
                        let id = bot["id"].as_str().unwrap_or("muse");
                        let name = bot["display_name"].as_str().unwrap_or(id);
                        let color = parse_hex_color(
                            bot["avatar_color"]
                                .as_str()
                                .or_else(|| bot["color"].as_str())
                                .unwrap_or("#2F7CF6"),
                        );
                        let shape = BotAvatarShape::from_name(
                            bot["avatar_shape"].as_str().unwrap_or("Circle"),
                        );
                        let is_selected =
                            ui_state.active_chat_kind == "dm" && active_bot.as_str() == id;
                        let (rect, resp) =
                            ui.allocate_exact_size(Vec2::new(38.0, 38.0), Sense::click());
                        if is_selected {
                            ui.painter().rect_stroke(
                                rect.expand(2.0),
                                CornerRadius::same(10),
                                Stroke::new(2.0_f32, palette.accent),
                                egui::StrokeKind::Outside,
                            );
                        }
                        let tex = textures.get(&format!("avatar:{id}")).map(|(t, _)| t);
                        paint_bot_avatar(
                            ui.painter(),
                            rect.shrink(3.0),
                            shape,
                            color,
                            name,
                            tex,
                            Some("online"),
                            palette.bg_sidebar,
                        );
                        if resp.clicked() {
                            *active_bot = id.to_string();
                            ui_state.active_chat_kind = "dm".into();
                            send(DaemonRequest::GetDirectChatHistory {
                                bot_id: id.to_string(),
                                before_seq: None,
                                limit: Some(100),
                            });
                            send(DaemonRequest::MarkChatRead {
                                chat_kind: "dm".into(),
                                chat_id: id.to_string(),
                            });
                        }
                        resp.on_hover_text(name);
                        ui.add_space(6.0);
                    }
                    if ui.small_button("＋").on_hover_text("New Bot").clicked() {
                        ui_state.active_modal = Some(ActiveModal::CreateBot);
                    }
                });
            });
        return;
    }

    // Full 240px Sidebar
    egui::SidePanel::left("grok_full_bot_sidebar")
        .exact_width(mode.width_px())
        .resizable(false)
        .frame(
            egui::Frame::NONE
                .fill(palette.bg_sidebar)
                .stroke(Stroke::new(1.0_f32, palette.border_subtle))
                .inner_margin(egui::Margin::symmetric(10, 10)),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.strong(RichText::new("Workspace").color(palette.text_primary));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .small_button("«")
                        .on_hover_text("Collapse sidebar")
                        .clicked()
                    {
                        ui_state.sidebar_manual_override = Some(false);
                    }
                    if ui
                        .small_button("＋ Bot")
                        .on_hover_text("Create a new specialist Bot")
                        .clicked()
                    {
                        ui_state.active_modal = Some(ActiveModal::CreateBot);
                    }
                });
            });
            ui.add_space(4.0);
            ui.add(
                egui::TextEdit::singleline(&mut ui_state.sidebar_search)
                    .desired_width(ui.available_width())
                    .hint_text("Filter bots or #channels…"),
            );
            ui.add_space(6.0);

            let needle = ui_state.sidebar_search.trim().to_ascii_lowercase();

            egui::ScrollArea::vertical()
                .id_salt("grok_sidebar_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // Pinned & Primary Bots
                    let pinned: Vec<&&Value> = visible_bots
                        .iter()
                        .filter(|b| {
                            (b["is_primary"].as_bool().unwrap_or(false)
                                || b["pinned"].as_bool().unwrap_or(false))
                                && matches_bot_filter(b, &needle)
                        })
                        .collect();
                    if !pinned.is_empty() {
                        ui.small(RichText::new("PINNED").color(palette.text_muted));
                        for bot in pinned {
                            draw_bot_row(
                                ui,
                                ui_state,
                                palette,
                                textures,
                                bot,
                                sections,
                                unread_counts,
                                active_bot,
                                &mut send,
                            );
                        }
                        ui.add_space(6.0);
                    }

                    // Custom Sections
                    for sec in sections {
                        let sec_id = sec["id"].as_str().unwrap_or("");
                        let sec_name = sec["name"].as_str().unwrap_or("Section");
                        let collapsed = sec["collapsed"].as_bool().unwrap_or(false);
                        let sec_bots: Vec<&&Value> = visible_bots
                            .iter()
                            .filter(|b| {
                                !b["is_primary"].as_bool().unwrap_or(false)
                                    && !b["pinned"].as_bool().unwrap_or(false)
                                    && b["section_id"].as_str() == Some(sec_id)
                                    && matches_bot_filter(b, &needle)
                            })
                            .collect();
                        ui.horizontal(|ui| {
                            let arrow = if collapsed { "▸" } else { "▾" };
                            if ui
                                .small_button(format!("{arrow} {}", sec_name.to_uppercase()))
                                .clicked()
                            {
                                send(DaemonRequest::ManageBotSection {
                                    op: "collapse".into(),
                                    id: Some(sec_id.to_string()),
                                    name: None,
                                    collapsed: Some(!collapsed),
                                    sort_order: None,
                                });
                            }
                        });
                        if !collapsed {
                            for bot in sec_bots {
                                draw_bot_row(
                                    ui,
                                    ui_state,
                                    palette,
                                    textures,
                                    bot,
                                    sections,
                                    unread_counts,
                                    active_bot,
                                    &mut send,
                                );
                            }
                        }
                        ui.add_space(4.0);
                    }

                    // Unsectioned Bots
                    let unsectioned: Vec<&&Value> = visible_bots
                        .iter()
                        .filter(|b| {
                            !b["is_primary"].as_bool().unwrap_or(false)
                                && !b["pinned"].as_bool().unwrap_or(false)
                                && b["section_id"].as_str().map(str::is_empty).unwrap_or(true)
                                && matches_bot_filter(b, &needle)
                        })
                        .collect();
                    if !unsectioned.is_empty() {
                        ui.small(RichText::new("BOTS").color(palette.text_muted));
                        for bot in unsectioned {
                            draw_bot_row(
                                ui,
                                ui_state,
                                palette,
                                textures,
                                bot,
                                sections,
                                unread_counts,
                                active_bot,
                                &mut send,
                            );
                        }
                        ui.add_space(6.0);
                    }

                    // Group Channels (`#channels`)
                    ui.horizontal(|ui| {
                        ui.small(RichText::new("GROUPS").color(palette.text_muted));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .small_button("＋")
                                .on_hover_text("Create group channel")
                                .clicked()
                            {
                                ui_state.active_modal = Some(ActiveModal::CreateChannel);
                            }
                        });
                    });
                    for ch in channels {
                        let ch_id = ch["id"].as_str().unwrap_or("general");
                        if !needle.is_empty() && !ch_id.to_ascii_lowercase().contains(&needle) {
                            continue;
                        }
                        let selected =
                            ui_state.active_chat_kind == "channel" && active_channel == ch_id;
                        let unread = unread_counts[format!("channel:{ch_id}")]
                            .as_u64()
                            .unwrap_or(0);
                        let label = if unread > 0 {
                            format!("# {ch_id}  ({unread})")
                        } else {
                            format!("# {ch_id}")
                        };
                        let btn = egui::Button::new(RichText::new(label).color(if selected {
                            palette.accent_text
                        } else {
                            palette.text_primary
                        }))
                        .min_size(Vec2::new(ui.available_width(), 28.0))
                        .fill(if selected {
                            palette.accent
                        } else {
                            Color32::TRANSPARENT
                        });
                        if ui.add(btn).clicked() {
                            *active_channel = ch_id.to_string();
                            ui_state.active_chat_kind = "channel".into();
                            send(DaemonRequest::ChannelHistory {
                                channel_id: ch_id.to_string(),
                            });
                            send(DaemonRequest::MarkChatRead {
                                chat_kind: "channel".into(),
                                chat_id: ch_id.to_string(),
                            });
                        }
                    }

                    // Hidden Bots (N) expandable footer
                    if !hidden_bots.is_empty() {
                        ui.add_space(10.0);
                        ui.separator();
                        let toggle_label = format!(
                            "{} Hidden Bots ({})",
                            if ui_state.show_hidden_bots {
                                "▾"
                            } else {
                                "▸"
                            },
                            hidden_bots.len()
                        );
                        if ui.small_button(toggle_label).clicked() {
                            ui_state.show_hidden_bots = !ui_state.show_hidden_bots;
                        }
                        if ui_state.show_hidden_bots {
                            for bot in &hidden_bots {
                                draw_bot_row(
                                    ui,
                                    ui_state,
                                    palette,
                                    textures,
                                    bot,
                                    sections,
                                    unread_counts,
                                    active_bot,
                                    &mut send,
                                );
                            }
                        }
                    }
                });
        });
}

fn matches_bot_filter(bot: &Value, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let id = bot["id"].as_str().unwrap_or("").to_ascii_lowercase();
    let name = bot["display_name"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    let role = bot["role_title"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    id.contains(needle) || name.contains(needle) || role.contains(needle)
}

#[allow(clippy::too_many_arguments)]
fn draw_bot_row(
    ui: &mut egui::Ui,
    ui_state: &mut GrokUiState,
    palette: &GrokPalette,
    textures: &mut BoundedTextureCache,
    bot: &Value,
    sections: &[Value],
    unread_counts: &Value,
    active_bot: &mut String,
    send: &mut impl FnMut(DaemonRequest),
) {
    let id = bot["id"].as_str().unwrap_or("muse").to_string();
    let name = bot["display_name"].as_str().unwrap_or(&id);
    let role = bot["role_title"].as_str().unwrap_or("");
    let is_primary = bot["is_primary"].as_bool().unwrap_or(false);
    let pinned = bot["pinned"].as_bool().unwrap_or(false);
    let hidden = bot["hidden_from_sidebar"].as_bool().unwrap_or(false);
    let color = parse_hex_color(
        bot["avatar_color"]
            .as_str()
            .or_else(|| bot["color"].as_str())
            .unwrap_or("#2F7CF6"),
    );
    let shape = BotAvatarShape::from_name(bot["avatar_shape"].as_str().unwrap_or("Circle"));
    let selected = ui_state.active_chat_kind == "dm" && active_bot.as_str() == id;
    let unread = unread_counts[format!("dm:{id}")].as_u64().unwrap_or(0);

    let (row_rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::click());
    let bg = if selected {
        palette.bg_hover
    } else if resp.hovered() {
        palette.bg_elevated
    } else {
        Color32::TRANSPARENT
    };
    ui.painter()
        .rect_filled(row_rect, CornerRadius::same(7), bg);
    if selected {
        let bar = egui::Rect::from_min_size(row_rect.min, Vec2::new(3.0, row_rect.height()));
        ui.painter()
            .rect_filled(bar, CornerRadius::same(2), palette.accent);
    }

    let avatar_rect = egui::Rect::from_center_size(
        egui::Pos2::new(row_rect.min.x + 18.0, row_rect.center().y),
        Vec2::new(22.0, 22.0),
    );
    let tex = textures.get(&format!("avatar:{id}")).map(|(t, _)| t);
    paint_bot_avatar(
        ui.painter(),
        avatar_rect,
        shape,
        color,
        name,
        tex,
        Some("online"),
        palette.bg_sidebar,
    );

    let title_str = if is_primary {
        format!("{name} ★")
    } else {
        name.to_string()
    };
    ui.painter().text(
        egui::Pos2::new(row_rect.min.x + 36.0, row_rect.center().y - 5.0),
        egui::Align2::LEFT_CENTER,
        title_str,
        egui::FontId::proportional(13.0),
        palette.text_primary,
    );
    ui.painter().text(
        egui::Pos2::new(row_rect.min.x + 36.0, row_rect.center().y + 7.0),
        egui::Align2::LEFT_CENTER,
        role,
        egui::FontId::proportional(10.5),
        palette.text_muted,
    );

    if unread > 0 {
        let badge_pos = egui::Pos2::new(row_rect.max.x - 14.0, row_rect.center().y);
        ui.painter().circle_filled(badge_pos, 8.0, palette.accent);
        ui.painter().text(
            badge_pos,
            egui::Align2::CENTER_CENTER,
            unread.to_string(),
            egui::FontId::proportional(10.0),
            palette.accent_text,
        );
    }

    if resp.clicked() {
        *active_bot = id.clone();
        ui_state.active_chat_kind = "dm".into();
        send(DaemonRequest::GetDirectChatHistory {
            bot_id: id.clone(),
            before_seq: None,
            limit: Some(100),
        });
        send(DaemonRequest::MarkChatRead {
            chat_kind: "dm".into(),
            chat_id: id.clone(),
        });
    }

    // Right-click context menu with full GrokBot bot operations
    resp.context_menu(|ui| {
        ui.strong(format!("@{id}"));
        ui.separator();
        if ui.button("★ Set as Primary Bot").clicked() {
            send(DaemonRequest::SetBotUiPrefs {
                bot_id: id.clone(),
                prefs: serde_json::json!({ "is_primary": true }),
            });
            ui.close();
        }
        if ui
            .button(if pinned { "Unpin Bot" } else { "Pin Bot" })
            .clicked()
        {
            send(DaemonRequest::SetBotUiPrefs {
                bot_id: id.clone(),
                prefs: serde_json::json!({ "pinned": !pinned }),
            });
            ui.close();
        }
        if ui
            .button(if hidden { "Unhide Bot" } else { "Hide Bot" })
            .clicked()
        {
            send(DaemonRequest::SetBotUiPrefs {
                bot_id: id.clone(),
                prefs: serde_json::json!({ "hidden_from_sidebar": !hidden }),
            });
            ui.close();
        }
        if ui.button("🎨 Customize Shape & Color…").clicked() {
            ui_state.avatar_shape_draft =
                bot["avatar_shape"].as_str().unwrap_or("Circle").to_string();
            ui_state.avatar_color_draft = bot["avatar_color"]
                .as_str()
                .or_else(|| bot["color"].as_str())
                .unwrap_or("#2F7CF6")
                .to_string();
            ui_state.active_modal = Some(ActiveModal::AvatarPicker { bot_id: id.clone() });
            ui.close();
        }
        if ui.button("⎘ Duplicate Bot").clicked() {
            send(DaemonRequest::ManageSystem {
                op: "duplicate_bot".into(),
                payload: serde_json::json!({ "source_bot_id": id }),
            });
            ui.close();
        }
        if !sections.is_empty() {
            ui.menu_button("Move to Section", |ui| {
                if ui.button("(None)").clicked() {
                    send(DaemonRequest::SetBotUiPrefs {
                        bot_id: id.clone(),
                        prefs: serde_json::json!({ "section_id": "" }),
                    });
                    ui.close();
                }
                for sec in sections {
                    let s_id = sec["id"].as_str().unwrap_or("");
                    let s_name = sec["name"].as_str().unwrap_or(s_id);
                    if ui.button(s_name).clicked() {
                        send(DaemonRequest::SetBotUiPrefs {
                            bot_id: id.clone(),
                            prefs: serde_json::json!({ "section_id": s_id }),
                        });
                        ui.close();
                    }
                }
            });
        }
        if !is_primary && id != "muse" {
            ui.separator();
            if ui
                .button(RichText::new("🗑 Delete Bot").color(palette.status_danger))
                .clicked()
            {
                send(DaemonRequest::ManageSystem {
                    op: "delete_bot".into(),
                    payload: serde_json::json!({ "bot_id": id }),
                });
                ui.close();
            }
        }
    });
}
