//! Adaptive Bot / Group / Section Sidebar (`AutoCollapsed` `<= 1`, `CompactRail` `60px` for `2..=4`,
//! `FullList` `240px` for `5+`), Vector Bot Avatars, Unread Badges, Context Menus, and `Hidden Bots (N)`.

use crate::ui::cache::BoundedTextureCache;
use crate::ui::state::{ActiveModal, GrokUiState};
use crate::ui::theme::{BotAvatarShape, GrokPalette, paint_bot_avatar, parse_hex_color};
use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Sense, Vec2};
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
    use crate::ui::{
        icons::{self, Icon},
        rail::select_section,
        state::{SettingsPane, WorkspaceSection},
    };
    let collapsed = ui_state.sidebar_manual_override == Some(false);
    let narrow = ctx.content_rect().width() < 760.0;
    let mini = collapsed || narrow;
    let response = egui::SidePanel::left("grok_conversation_sidebar")
        .default_width(if mini { 72.0 } else { ui_state.sidebar_width })
        .width_range(if mini { 72.0..=72.0 } else { 240.0..=480.0 })
        .resizable(!mini)
        .frame(
            egui::Frame::NONE
                .fill(palette.bg_sidebar)
                .inner_margin(egui::Margin::symmetric(12, 14)),
        )
        .show(ctx, |ui| {
            if mini {
                ui.vertical_centered(|ui| {
                    if icons::button(ui, Icon::Sidebar, "Expand sidebar", palette.text_secondary)
                        .clicked()
                    {
                        ui_state.sidebar_manual_override = Some(true);
                        send(DaemonRequest::SaveUiSettings {
                            ui: serde_json::json!({"sidebar_collapsed": false}),
                        });
                    }
                    ui.add_space(4.0);
                    let plus = icons::button(ui, Icon::Plus, "Create", palette.text_secondary);
                    egui::Popup::menu(&plus).show(|ui| {
                        for (label, modal) in [
                            ("New bot", ActiveModal::CreateBot),
                            ("New group", ActiveModal::CreateChannel),
                            ("Studio · skills & routines", ActiveModal::Studio),
                        ] {
                            if ui.button(label).clicked() {
                                ui_state.active_modal = Some(modal);
                                ui.close();
                            }
                        }
                    });
                    if icons::button(
                        ui,
                        Icon::Search,
                        "Search conversations",
                        palette.text_secondary,
                    )
                    .clicked()
                    {
                        ui_state.sidebar_manual_override = Some(true);
                        ui_state.sidebar_search_open = true;
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    if icons::button(
                        ui,
                        Icon::Sidebar,
                        "Collapse sidebar",
                        palette.text_secondary,
                    )
                    .clicked()
                    {
                        ui_state.sidebar_manual_override = Some(false);
                        send(DaemonRequest::SaveUiSettings {
                            ui: serde_json::json!({"sidebar_collapsed": true}),
                        });
                    }
                    ui.add_space(4.0);
                    ui.strong("DT");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let plus = icons::button(ui, Icon::Plus, "Create", palette.text_secondary);
                        egui::Popup::menu(&plus).show(|ui| {
                            for (label, modal) in [
                                ("New bot", ActiveModal::CreateBot),
                                ("New group", ActiveModal::CreateChannel),
                                ("Studio · skills & routines", ActiveModal::Studio),
                            ] {
                                if ui.button(label).clicked() {
                                    ui_state.active_modal = Some(modal);
                                    ui.close();
                                }
                            }
                        });
                        if icons::button(
                            ui,
                            Icon::Search,
                            "Search conversations",
                            palette.text_secondary,
                        )
                        .clicked()
                        {
                            ui_state.sidebar_search_open = !ui_state.sidebar_search_open;
                        }
                    });
                });
            }
            ui.add_space(12.0);
            if ui_state.sidebar_search_open && !mini {
                ui.add(
                    egui::TextEdit::singleline(&mut ui_state.sidebar_search)
                        .hint_text("Search conversations…")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(10.0);
            }
            if mini {
                ui.vertical_centered(|ui| {
                    let activity =
                        icons::button(ui, Icon::Activity, "Activity", palette.text_secondary);
                    egui::Popup::menu(&activity).show(|ui| {
                        for section in [
                            WorkspaceSection::Feed,
                            WorkspaceSection::Goals,
                            WorkspaceSection::Audit,
                        ] {
                            if ui
                                .selectable_label(ui_state.section == section, section.label())
                                .clicked()
                            {
                                select_section(ui_state, section, &mut send);
                                ui.close();
                            }
                        }
                        ui.separator();
                        let paused = ui_state.server_snapshot["status"]["paused"]
                            .as_bool()
                            .unwrap_or(false);
                        if ui
                            .button(if paused {
                                "Resume tasks"
                            } else {
                                "Pause tasks"
                            })
                            .clicked()
                        {
                            send(DaemonRequest::SetPaused { paused: !paused });
                            ui.close();
                        }
                        if ui
                            .button(RichText::new("Stop all tasks").color(palette.status_danger))
                            .clicked()
                        {
                            send(DaemonRequest::EmergencyKill);
                            ui.close();
                        }
                    });
                    if icons::button(ui, Icon::Pages, "Pages", palette.text_secondary).clicked() {
                        select_section(ui_state, WorkspaceSection::Spaces, &mut send);
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    let activity = icons::labeled_button(
                        ui,
                        Icon::Activity,
                        "Activity",
                        palette.text_secondary,
                    );
                    egui::Popup::menu(&activity).show(|ui| {
                        for section in [
                            WorkspaceSection::Feed,
                            WorkspaceSection::Goals,
                            WorkspaceSection::Audit,
                        ] {
                            if ui
                                .selectable_label(ui_state.section == section, section.label())
                                .clicked()
                            {
                                select_section(ui_state, section, &mut send);
                                ui.close();
                            }
                        }
                        ui.separator();
                        let paused = ui_state.server_snapshot["status"]["paused"]
                            .as_bool()
                            .unwrap_or(false);
                        if ui
                            .button(if paused {
                                "Resume tasks"
                            } else {
                                "Pause tasks"
                            })
                            .clicked()
                        {
                            send(DaemonRequest::SetPaused { paused: !paused });
                            ui.close();
                        }
                        if ui
                            .button(RichText::new("Stop all tasks").color(palette.status_danger))
                            .clicked()
                        {
                            send(DaemonRequest::EmergencyKill);
                            ui.close();
                        }
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icons::labeled_button(ui, Icon::Pages, "Pages", palette.text_secondary)
                            .clicked()
                        {
                            select_section(ui_state, WorkspaceSection::Spaces, &mut send);
                        }
                    });
                });
            }
            ui.add_space(10.0);
            // Footer remains visible while conversations scroll.
            egui::TopBottomPanel::bottom("grok_sidebar_account")
                .frame(egui::Frame::NONE)
                .show_inside(ui, |ui| {
                    ui.add_space(8.0);
                    if mini {
                        ui.vertical_centered(|ui| {
                            if icons::button(
                                ui,
                                Icon::Plus,
                                "Connect apps & marketplace",
                                palette.text_secondary,
                            )
                            .clicked()
                            {
                                ui_state.active_modal = Some(ActiveModal::ConnectApps);
                            }
                            if icons::button(ui, Icon::Settings, "Settings", palette.text_secondary)
                                .clicked()
                            {
                                ui_state.active_modal = Some(ActiveModal::Settings);
                            }
                            let ws_btn =
                                icons::button(ui, Icon::More, "Workspace", palette.text_secondary);
                            egui::Popup::menu(&ws_btn).show(|ui| {
                                if ui.button("Providers & models").clicked() {
                                    ui_state.settings_pane = SettingsPane::Providers;
                                    ui_state.active_modal = Some(ActiveModal::Settings);
                                    ui.close();
                                }
                                if ui.button("Local components & updates").clicked() {
                                    ui_state.settings_pane = SettingsPane::SystemAndUpdates;
                                    ui_state.active_modal = Some(ActiveModal::Settings);
                                    ui.close();
                                }
                                if ui.button("Keyboard shortcuts").clicked() {
                                    ui_state.active_modal = Some(ActiveModal::Shortcuts);
                                    ui.close();
                                }
                                if ui.button("Command palette").clicked() {
                                    ui_state.active_modal = Some(ActiveModal::CommandPalette);
                                    ui.close();
                                }
                            });
                        });
                    } else {
                        if ui
                            .add_sized(
                                [ui.available_width(), 36.0],
                                egui::Button::new("Connect apps")
                                    .fill(palette.bg_elevated)
                                    .corner_radius(12),
                            )
                            .clicked()
                        {
                            ui_state.active_modal = Some(ActiveModal::ConnectApps);
                        }
                        ui.horizontal(|ui| {
                            if icons::labeled_button(
                                ui,
                                Icon::Settings,
                                "Settings",
                                palette.text_secondary,
                            )
                            .clicked()
                            {
                                ui_state.active_modal = Some(ActiveModal::Settings);
                            }
                            ui.menu_button("Workspace", |ui| {
                                if ui.button("Providers & models").clicked() {
                                    ui_state.settings_pane = SettingsPane::Providers;
                                    ui_state.active_modal = Some(ActiveModal::Settings);
                                    ui.close();
                                }
                                if ui.button("Local components & updates").clicked() {
                                    ui_state.settings_pane = SettingsPane::SystemAndUpdates;
                                    ui_state.active_modal = Some(ActiveModal::Settings);
                                    ui.close();
                                }
                                if ui.button("Keyboard shortcuts").clicked() {
                                    ui_state.active_modal = Some(ActiveModal::Shortcuts);
                                    ui.close();
                                }
                                if ui.button("Command palette").clicked() {
                                    ui_state.active_modal = Some(ActiveModal::CommandPalette);
                                    ui.close();
                                }
                            });
                        });
                    }
                });
            let needle = ui_state.sidebar_search.trim().to_ascii_lowercase();
            let mut visible: Vec<_> = bots
                .iter()
                .filter(|b| {
                    !b["hidden_from_sidebar"].as_bool().unwrap_or(false)
                        && matches_bot_filter(b, &needle)
                })
                .collect();
            visible.sort_by_key(|bot| !bot["pinned"].as_bool().unwrap_or(false));
            egui::ScrollArea::vertical()
                .id_salt("grok_sidebar_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if !mini {
                        ui.label(
                            RichText::new("Personal")
                                .size(12.0)
                                .color(palette.text_muted),
                        );
                        ui.add_space(8.0);
                    }
                    for bot in &visible {
                        if !mini
                            && !bot["pinned"].as_bool().unwrap_or(false)
                            && bot["section_id"].as_str().is_some_and(|id| {
                                sections
                                    .iter()
                                    .any(|section| section["id"].as_str() == Some(id))
                            })
                        {
                            continue;
                        }

                        if mini {
                            let id = bot["id"].as_str().unwrap_or("muse");
                            let name = bot["display_name"].as_str().unwrap_or(id);
                            let (rect, resp) =
                                ui.allocate_exact_size(Vec2::splat(46.0), Sense::click());
                            if active_bot == id && ui_state.section == WorkspaceSection::Bots {
                                ui.painter().rect_filled(rect, 14, palette.bg_hover);
                            }
                            paint_bot_avatar(
                                ui.painter(),
                                rect.shrink(7.0),
                                BotAvatarShape::from_name(
                                    bot["avatar_shape"].as_str().unwrap_or("circle"),
                                ),
                                parse_hex_color(
                                    bot["avatar_color"]
                                        .as_str()
                                        .or_else(|| bot["color"].as_str())
                                        .unwrap_or("#D8B4FE"),
                                ),
                                name,
                                textures.get(&format!("avatar:{id}")).map(|(t, _)| t),
                                None,
                                palette.bg_sidebar,
                            );
                            resp.widget_info(|| {
                                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name)
                            });
                            if resp.on_hover_text(name).clicked() {
                                *active_bot = id.into();
                                ui_state.active_chat_kind = "dm".into();
                                ui_state.section = WorkspaceSection::Bots;
                                send(DaemonRequest::GetDirectChatHistory {
                                    bot_id: id.into(),
                                    before_seq: None,
                                    limit: Some(100),
                                });
                                send(DaemonRequest::MarkChatRead {
                                    chat_kind: "dm".into(),
                                    chat_id: id.into(),
                                });
                            }
                        } else {
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
                    if !mini {
                        for section in sections {
                            let id = section["id"].as_str().unwrap_or_default();
                            let name = section["name"].as_str().unwrap_or("Section");
                            let collapsed = section["collapsed"].as_bool().unwrap_or(false);
                            ui.add_space(12.0);
                            if ui
                                .small_button(format!(
                                    "{} {name}",
                                    if collapsed { "▸" } else { "▾" }
                                ))
                                .clicked()
                            {
                                send(DaemonRequest::ManageBotSection {
                                    op: "collapse".into(),
                                    id: Some(id.into()),
                                    name: None,
                                    collapsed: Some(!collapsed),
                                    sort_order: None,
                                });
                            }
                            if !collapsed {
                                for bot in visible.iter().filter(|bot| {
                                    !bot["pinned"].as_bool().unwrap_or(false)
                                        && bot["section_id"].as_str() == Some(id)
                                }) {
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
                    }
                    if mini && !channels.is_empty() {
                        ui.separator();
                        for ch in channels {
                            let id = ch["id"].as_str().unwrap_or("general");
                            let name = ch["name"].as_str().unwrap_or(id).trim_start_matches('#');
                            let count =
                                unread_counts[format!("channel:{id}")].as_u64().unwrap_or(0);
                            let selected = ui_state.section == WorkspaceSection::Bots
                                && ui_state.active_chat_kind == "channel"
                                && active_channel == id;
                            let (rect, resp) =
                                ui.allocate_exact_size(Vec2::splat(44.0), Sense::click());
                            if selected || resp.hovered() {
                                ui.painter().rect_filled(rect, 12, palette.bg_hover);
                            }
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                format!(
                                    "#{}",
                                    name.chars().next().unwrap_or('g').to_ascii_uppercase()
                                ),
                                egui::FontId::proportional(14.0),
                                if selected {
                                    palette.accent
                                } else {
                                    palette.text_secondary
                                },
                            );
                            if count > 0 {
                                ui.painter().circle_filled(
                                    rect.right_top() + Vec2::new(-7.0, 7.0),
                                    4.0,
                                    palette.accent,
                                );
                            }
                            resp.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    true,
                                    format!("#{name}"),
                                )
                            });
                            if resp.on_hover_text(format!("#{name}")).clicked() {
                                *active_channel = id.into();
                                ui_state.active_chat_kind = "channel".into();
                                ui_state.section = WorkspaceSection::Bots;
                                send(DaemonRequest::ChannelHistory {
                                    channel_id: id.into(),
                                });
                                send(DaemonRequest::MarkChatRead {
                                    chat_kind: "channel".into(),
                                    chat_id: id.into(),
                                });
                            }
                        }
                    }
                    if !mini {
                        if !channels.is_empty() {
                            ui.add_space(18.0);
                            ui.label(RichText::new("Groups").size(12.0).color(palette.text_muted));
                            ui.add_space(8.0);
                        }
                        for ch in channels {
                            let id = ch["id"].as_str().unwrap_or("general");
                            if !needle.is_empty() && !id.to_lowercase().contains(&needle) {
                                continue;
                            }
                            let count =
                                unread_counts[format!("channel:{id}")].as_u64().unwrap_or(0);
                            let name = ch["name"].as_str().unwrap_or(id).trim_start_matches('#');
                            let preview =
                                ui_state.server_snapshot["channel_previews"][id]["preview"]
                                    .as_str()
                                    .or_else(|| ch["topic"].as_str())
                                    .unwrap_or("Shared conversation");
                            let (rect, response) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), 64.0),
                                Sense::click(),
                            );
                            let selected =
                                ui_state.active_chat_kind == "channel" && active_channel == id;
                            response.widget_info(|| {
                                egui::WidgetInfo::selected(
                                    egui::WidgetType::Button,
                                    true,
                                    selected,
                                    name,
                                )
                            });
                            if selected || response.hovered() {
                                ui.painter().rect_filled(rect, 14, palette.bg_hover);
                            }
                            let participants = ch["participants"].as_array();
                            for index in 0..3 {
                                let bot = participants
                                    .and_then(|ids| ids.get(index))
                                    .and_then(|value| value.as_str())
                                    .and_then(|id| {
                                        bots.iter().find(|b| b["id"].as_str() == Some(id))
                                    });
                                let color = parse_hex_color(
                                    bot.and_then(|b| {
                                        b["avatar_color"].as_str().or_else(|| b["color"].as_str())
                                    })
                                    .unwrap_or(["#4D9BFF", "#D85AA6", "#35BFA5"][index]),
                                );
                                let center = rect.left_top()
                                    + Vec2::new(
                                        18.0 + (index % 2) as f32 * 16.0,
                                        24.0 + (index / 2) as f32 * 16.0,
                                    );
                                paint_bot_avatar(
                                    ui.painter(),
                                    egui::Rect::from_center_size(center, Vec2::splat(19.0)),
                                    BotAvatarShape::Circle,
                                    color,
                                    name,
                                    None,
                                    None,
                                    palette.bg_sidebar,
                                );
                            }
                            let painter = ui.painter().with_clip_rect(egui::Rect::from_min_max(
                                rect.left_top() + Vec2::new(54.0, 0.0),
                                rect.right_bottom() - Vec2::new(24.0, 0.0),
                            ));
                            painter.text(
                                rect.left_top() + Vec2::new(54.0, 20.0),
                                egui::Align2::LEFT_CENTER,
                                name,
                                egui::FontId::proportional(14.0),
                                palette.text_primary,
                            );
                            painter.text(
                                rect.left_top() + Vec2::new(54.0, 43.0),
                                egui::Align2::LEFT_CENTER,
                                preview,
                                egui::FontId::proportional(12.0),
                                palette.text_muted,
                            );
                            if count > 0 {
                                ui.painter().circle_filled(
                                    rect.right_center() - Vec2::new(12.0, 0.0),
                                    8.0,
                                    palette.accent,
                                );
                                ui.painter().text(
                                    rect.right_center() - Vec2::new(12.0, 0.0),
                                    egui::Align2::CENTER_CENTER,
                                    count.to_string(),
                                    egui::FontId::proportional(10.0),
                                    palette.accent_text,
                                );
                            }
                            if response.clicked() {
                                *active_channel = id.into();
                                ui_state.active_chat_kind = "channel".into();
                                ui_state.section = WorkspaceSection::Bots;
                                send(DaemonRequest::ChannelHistory {
                                    channel_id: id.into(),
                                });
                                send(DaemonRequest::MarkChatRead {
                                    chat_kind: "channel".into(),
                                    chat_id: id.into(),
                                });
                            }
                        }
                        let hidden: Vec<_> = bots
                            .iter()
                            .filter(|b| b["hidden_from_sidebar"].as_bool().unwrap_or(false))
                            .collect();
                        if !hidden.is_empty() {
                            ui.add_space(16.0);
                            ui.checkbox(
                                &mut ui_state.show_hidden_bots,
                                format!("Hidden bots ({})", hidden.len()),
                            );
                            if ui_state.show_hidden_bots {
                                for bot in hidden {
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
                    }
                });
        });
    if !mini {
        ui_state.sidebar_width = response.response.rect.width();
        if ctx.input(|i| i.pointer.any_released())
            && (ui_state.sidebar_saved_width - ui_state.sidebar_width).abs() > 0.5
        {
            ui_state.sidebar_saved_width = ui_state.sidebar_width;
            send(DaemonRequest::SaveUiSettings {
                ui: serde_json::json!({"sidebar_width":ui_state.sidebar_width.round() as u16}),
            });
        }
    }
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
    let status = &ui_state.server_snapshot["bot_statuses"][&id];
    let phrase = status["phrase"]
        .as_str()
        .or_else(|| bot["status_phrase"].as_str())
        .unwrap_or(role);
    let attention = status["needs_attention"]
        .as_bool()
        .or_else(|| bot["needs_attention"].as_bool())
        .unwrap_or(false);

    let (row_rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 64.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, name));
    let bg = if selected {
        palette.bg_hover
    } else if resp.hovered() {
        palette.bg_elevated
    } else {
        Color32::TRANSPARENT
    };
    ui.painter()
        .rect_filled(row_rect, CornerRadius::same(14), bg);

    let avatar_rect = egui::Rect::from_center_size(
        egui::Pos2::new(row_rect.min.x + 26.0, row_rect.center().y),
        Vec2::new(34.0, 34.0),
    );
    let tex = textures.get(&format!("avatar:{id}")).map(|(t, _)| t);
    paint_bot_avatar(
        ui.painter(),
        avatar_rect,
        shape,
        color,
        name,
        tex,
        None,
        palette.bg_sidebar,
    );

    let title_str = short_preview(name, ((row_rect.width() - 120.0) / 7.0).max(6.0) as usize);
    if let Some(date) = ui_state.server_snapshot["conversation_previews"][&id]["created_at"]
        .as_str()
        .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
    {
        ui.painter().text(
            egui::pos2(row_rect.right() - 8.0, row_rect.top() + 17.0),
            egui::Align2::RIGHT_CENTER,
            date.with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string(),
            egui::FontId::proportional(10.0),
            palette.text_muted,
        );
    }
    ui.painter().text(
        egui::Pos2::new(row_rect.min.x + 54.0, row_rect.center().y - 10.0),
        egui::Align2::LEFT_CENTER,
        title_str,
        egui::FontId::proportional(14.0),
        palette.text_primary,
    );
    ui.painter().text(
        egui::Pos2::new(row_rect.min.x + 54.0, row_rect.center().y + 11.0),
        egui::Align2::LEFT_CENTER,
        ui_state.server_snapshot["conversation_previews"][&id]["preview"]
            .as_str()
            .unwrap_or(phrase)
            .chars()
            .take(((row_rect.width() - 85.0) / 6.5).max(5.0) as usize)
            .collect::<String>(),
        egui::FontId::proportional(12.0),
        palette.text_muted,
    );

    if attention {
        ui.painter().circle_filled(
            egui::pos2(row_rect.max.x - 6.0, row_rect.min.y + 6.0),
            4.0,
            palette.status_warn,
        );
    }
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
        ui_state.section = crate::ui::state::WorkspaceSection::Bots;
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
        if ui.button("Set as Primary Bot").clicked() {
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
        if ui.button("Customize Shape & Color…").clicked() {
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
        if ui.button("Duplicate Bot").clicked() {
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
                .button(RichText::new("Delete Bot").color(palette.status_danger))
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

fn short_preview(value: &str, limit: usize) -> String {
    if value.chars().count() > limit {
        value
            .chars()
            .take(limit.saturating_sub(1))
            .collect::<String>()
            + "…"
    } else {
        value.into()
    }
}
