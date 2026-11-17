//! 48px Left Workspace Rail (`Bots`, `Feed`, `Goals`, `Spaces`, `Audit`)
//! and Bottom Utility Strip (`Connect Apps`, `Studio`, `Shortcuts`, `Settings`, `Emergency Kill`).
//! Automatically adapts to a compact bottom strip when viewport width `< 768px`.

use crate::ui::state::{ActiveModal, GrokUiState, WorkspaceSection};
use crate::ui::theme::GrokPalette;
use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Sense, Stroke, Vec2};
use gm_core::ipc::DaemonRequest;

pub fn draw_workspace_rail(
    ctx: &egui::Context,
    ui_state: &mut GrokUiState,
    palette: &GrokPalette,
    unread_feed: usize,
    paused: bool,
    mut send: impl FnMut(DaemonRequest),
) {
    let screen_width = ctx.content_rect().width();
    if screen_width < 768.0 {
        egui::TopBottomPanel::bottom("grok_mobile_rail")
            .exact_height(46.0)
            .frame(
                egui::Frame::NONE
                    .fill(palette.bg_sidebar)
                    .stroke(Stroke::new(1.0_f32, palette.border_subtle))
                    .inner_margin(egui::Margin::symmetric(8, 4)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    for sec in WorkspaceSection::ALL {
                        let active = ui_state.section == sec;
                        let label = format!("{} {}", sec.icon(), sec.label());
                        let btn = egui::Button::new(RichText::new(label).size(12.0).color(
                            if active {
                                palette.accent_text
                            } else {
                                palette.text_secondary
                            },
                        ))
                        .fill(if active {
                            palette.accent
                        } else {
                            Color32::TRANSPARENT
                        });
                        if ui.add(btn).clicked() {
                            select_section(ui_state, sec, &mut send);
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .button(RichText::new("⛔").color(palette.status_danger))
                            .on_hover_text("Emergency Kill (Stop all tasks)")
                            .clicked()
                        {
                            send(DaemonRequest::EmergencyKill);
                        }
                        if ui.button("⚙").on_hover_text("Settings (Cmd+,)").clicked() {
                            ui_state.active_modal = Some(ActiveModal::Settings);
                        }
                        if ui
                            .button("⌘")
                            .on_hover_text("Command Palette (Cmd+K)")
                            .clicked()
                        {
                            ui_state.active_modal = Some(ActiveModal::CommandPalette);
                        }
                    });
                });
            });
        return;
    }

    egui::SidePanel::left("grok_workspace_rail")
        .exact_width(48.0)
        .resizable(false)
        .frame(
            egui::Frame::NONE
                .fill(palette.bg_sidebar)
                .stroke(Stroke::new(1.0_f32, palette.border_subtle))
                .inner_margin(egui::Margin::symmetric(6, 8)),
        )
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                // Top: Command Palette quick trigger
                if rail_icon_button(
                    ui,
                    palette,
                    "⌘",
                    "Command Palette (Cmd/Ctrl+K)",
                    ui_state.active_modal == Some(ActiveModal::CommandPalette),
                    0,
                )
                .clicked()
                {
                    ui_state.active_modal = Some(ActiveModal::CommandPalette);
                }
                ui.add_space(6.0);

                // Primary 5 Workspace Sections
                for sec in WorkspaceSection::ALL {
                    let active = ui_state.section == sec;
                    let badge = if sec == WorkspaceSection::Feed {
                        unread_feed
                    } else {
                        0
                    };
                    if rail_icon_button(ui, palette, sec.icon(), sec.label(), active, badge)
                        .clicked()
                    {
                        select_section(ui_state, sec, &mut send);
                    }
                    ui.add_space(4.0);
                }

                // Bottom utility icons
                ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
                    // Emergency Kill
                    let kill_btn = egui::Button::new(
                        RichText::new("⛔").size(15.0).color(palette.status_danger),
                    )
                    .min_size(Vec2::new(34.0, 34.0))
                    .corner_radius(CornerRadius::same(8))
                    .fill(palette.status_danger.gamma_multiply(0.12));
                    if ui
                        .add(kill_btn)
                        .on_hover_text("Emergency Kill — Stop all active bots & tasks")
                        .clicked()
                    {
                        send(DaemonRequest::EmergencyKill);
                    }
                    ui.add_space(4.0);

                    // Pause / Resume runtime toggle
                    let pause_icon = if paused { "▶" } else { "⏸" };
                    let pause_tip = if paused {
                        "Resume task admission"
                    } else {
                        "Pause task admission"
                    };
                    if rail_icon_button(ui, palette, pause_icon, pause_tip, paused, 0).clicked() {
                        send(DaemonRequest::SetPaused { paused: !paused });
                    }
                    ui.add_space(4.0);

                    // Settings (Cmd/Ctrl+,)
                    if rail_icon_button(
                        ui,
                        palette,
                        "⚙",
                        "Settings (Cmd/Ctrl+,)",
                        ui_state.active_modal == Some(ActiveModal::Settings),
                        0,
                    )
                    .clicked()
                    {
                        ui_state.active_modal = Some(ActiveModal::Settings);
                    }
                    ui.add_space(4.0);

                    // Keyboard Shortcuts (Cmd/Ctrl+/)
                    if rail_icon_button(
                        ui,
                        palette,
                        "⌨",
                        "Keyboard Shortcuts (Cmd/Ctrl+/)",
                        ui_state.active_modal == Some(ActiveModal::Shortcuts),
                        0,
                    )
                    .clicked()
                    {
                        ui_state.active_modal = Some(ActiveModal::Shortcuts);
                    }
                    ui.add_space(4.0);

                    // Studio (Cmd/Ctrl+Shift+M)
                    if rail_icon_button(
                        ui,
                        palette,
                        "✦",
                        "Studio — Templates, Skills & Routines (Cmd/Ctrl+Shift+M)",
                        ui_state.active_modal == Some(ActiveModal::Studio),
                        0,
                    )
                    .clicked()
                    {
                        ui_state.active_modal = Some(ActiveModal::Studio);
                    }
                    ui.add_space(4.0);

                    // Connect Apps
                    if rail_icon_button(
                        ui,
                        palette,
                        "🔌",
                        "Connect Apps & Custom MCP/Webhook Connectors",
                        ui_state.active_modal == Some(ActiveModal::ConnectApps),
                        0,
                    )
                    .clicked()
                    {
                        send(DaemonRequest::ListCustomConnectors);
                        ui_state.active_modal = Some(ActiveModal::ConnectApps);
                    }
                });
            });
        });
}

fn select_section(
    ui_state: &mut GrokUiState,
    sec: WorkspaceSection,
    send: &mut impl FnMut(DaemonRequest),
) {
    ui_state.section = sec;
    match sec {
        WorkspaceSection::Bots => {}
        WorkspaceSection::Feed => send(DaemonRequest::ListActivity),
        WorkspaceSection::Goals => send(DaemonRequest::ListGoals),
        WorkspaceSection::Spaces => send(DaemonRequest::ListSpaces),
        WorkspaceSection::Audit => send(DaemonRequest::ListQueue { bot_id: None }),
    }
}

fn rail_icon_button(
    ui: &mut egui::Ui,
    palette: &GrokPalette,
    icon: &str,
    tooltip: &str,
    active: bool,
    badge: usize,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(34.0, 34.0), Sense::click());
    let bg = if active {
        palette.accent
    } else if resp.hovered() {
        palette.bg_hover
    } else {
        Color32::TRANSPARENT
    };
    let fg = if active {
        palette.accent_text
    } else if resp.hovered() {
        palette.text_primary
    } else {
        palette.text_secondary
    };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(8), bg);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(16.0),
        fg,
    );
    if badge > 0 {
        let dot = egui::Pos2::new(rect.max.x - 5.0, rect.min.y + 5.0);
        ui.painter().circle_filled(dot, 4.0, palette.accent);
    }
    resp.on_hover_text(tooltip)
}
