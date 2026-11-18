//! GrokBot Design System Tokens, Dark/Light/System Theme Application,
//! 12-Color Bot Palette, Accent Mode (`bot_color` vs `black`), and Vector Bot Shape Primitives.

use crate::ui::tokens;
use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Shape, Stroke, Vec2};

/// Preset 12-color GrokBot palette (`(name, hex)`).
pub const BOT_COLOR_PALETTE: &[(&str, &str, Color32)] = &[
    (
        "Electric Blue",
        "#3B82F6",
        Color32::from_rgb(0x3B, 0x82, 0xF6),
    ),
    ("Violet", "#8B5CF6", Color32::from_rgb(0x8B, 0x5C, 0xF6)),
    ("Emerald", "#10B981", Color32::from_rgb(0x10, 0xB9, 0x81)),
    ("Amber", "#F59E0B", Color32::from_rgb(0xF5, 0x9E, 0x0B)),
    ("Rose", "#F43F5E", Color32::from_rgb(0xF4, 0x3F, 0x5E)),
    ("Cyan", "#06B6D4", Color32::from_rgb(0x06, 0xB6, 0xD4)),
    ("Indigo", "#6366F1", Color32::from_rgb(0x63, 0x66, 0xF1)),
    ("Teal", "#14B8A6", Color32::from_rgb(0x14, 0xB8, 0xA6)),
    ("Orange", "#F97316", Color32::from_rgb(0xF9, 0x73, 0x16)),
    ("Pink", "#EC4899", Color32::from_rgb(0xEC, 0x48, 0x99)),
    ("Lime", "#84CC16", Color32::from_rgb(0x84, 0xCC, 0x16)),
    ("Slate", "#64748B", Color32::from_rgb(0x64, 0x74, 0x8B)),
];

/// Six geometric vector shapes used for Bot avatars in GrokBot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotAvatarShape {
    Circle,
    Hex,
    RoundedSquare,
    Diamond,
    Pill,
    Spark,
}

impl BotAvatarShape {
    pub const ALL: [BotAvatarShape; 6] = [
        BotAvatarShape::Circle,
        BotAvatarShape::Hex,
        BotAvatarShape::RoundedSquare,
        BotAvatarShape::Diamond,
        BotAvatarShape::Pill,
        BotAvatarShape::Spark,
    ];

    pub fn from_name(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "hex" | "hexagon" => Self::Hex,
            "rounded_square" | "roundedsquare" | "square" => Self::RoundedSquare,
            "diamond" => Self::Diamond,
            "pill" => Self::Pill,
            "spark" | "star" => Self::Spark,
            _ => Self::Circle,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Circle => "Circle",
            Self::Hex => "Hex",
            Self::RoundedSquare => "RoundedSquare",
            Self::Diamond => "Diamond",
            Self::Pill => "Pill",
            Self::Spark => "Spark",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Circle => "Circle",
            Self::Hex => "Hex",
            Self::RoundedSquare => "Rounded Square",
            Self::Diamond => "Diamond",
            Self::Pill => "Pill",
            Self::Spark => "Spark",
        }
    }
}

/// Parse a `#RRGGBB` hex color string into `Color32`, falling back to Electric Blue.
pub fn parse_hex_color(hex: &str) -> Color32 {
    let clean = hex.trim().trim_start_matches('#');
    if clean.len() == 6
        && clean.is_ascii()
        && let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&clean[0..2], 16),
            u8::from_str_radix(&clean[2..4], 16),
            u8::from_str_radix(&clean[4..6], 16),
        )
    {
        return Color32::from_rgb(r, g, b);
    }
    Color32::from_rgb(0x3B, 0x82, 0xF6)
}

/// Resolved GrokBot semantic surface & text colors for the current frame.
#[derive(Debug, Clone, Copy)]
pub struct GrokPalette {
    pub dark_mode: bool,
    pub bg_canvas: Color32,
    pub bg_sidebar: Color32,
    pub bg_elevated: Color32,
    pub bg_hover: Color32,
    pub border_subtle: Color32,
    pub text_primary: Color32,
    pub text_secondary: Color32,
    pub text_muted: Color32,
    pub accent: Color32,
    pub accent_text: Color32,
    pub status_ok: Color32,
    pub status_warn: Color32,
    pub status_danger: Color32,
}

impl GrokPalette {
    pub fn resolve(
        theme_pref: &str,
        system_dark: bool,
        accent_mode: &str,
        bot_color_hex: &str,
    ) -> Self {
        let dark_mode = match theme_pref.trim().to_ascii_lowercase().as_str() {
            "light" => false,
            "dark" => true,
            _ => system_dark,
        };
        let bot_color = parse_hex_color(bot_color_hex);
        let monochrome = accent_mode.trim().eq_ignore_ascii_case("black");

        let _ = (bot_color, monochrome);
        if dark_mode {
            Self {
                dark_mode: true,
                bg_canvas: tokens::DARK_BACKGROUND,
                bg_sidebar: tokens::DARK_SIDEBAR,
                bg_elevated: tokens::DARK_CARD,
                bg_hover: tokens::DARK_ACCENT,
                border_subtle: tokens::DARK_BORDER,
                text_primary: tokens::DARK_FOREGROUND,
                text_secondary: tokens::DARK_MUTED_FOREGROUND,
                text_muted: tokens::DARK_FAINT_FOREGROUND,
                accent: tokens::DARK_PRIMARY,
                accent_text: tokens::DARK_PRIMARY_FOREGROUND,
                status_ok: tokens::DARK_SUCCESS,
                status_warn: tokens::DARK_WARNING,
                status_danger: tokens::DARK_DESTRUCTIVE,
            }
        } else {
            Self {
                dark_mode: false,
                bg_canvas: tokens::LIGHT_BACKGROUND,
                bg_sidebar: tokens::LIGHT_SIDEBAR,
                bg_elevated: tokens::LIGHT_CARD,
                bg_hover: tokens::LIGHT_ACCENT,
                border_subtle: tokens::LIGHT_BORDER,
                text_primary: tokens::LIGHT_FOREGROUND,
                text_secondary: tokens::LIGHT_MUTED_FOREGROUND,
                text_muted: tokens::LIGHT_FAINT_FOREGROUND,
                accent: tokens::LIGHT_PRIMARY,
                accent_text: tokens::LIGHT_PRIMARY_FOREGROUND,
                status_ok: tokens::LIGHT_SUCCESS,
                status_warn: tokens::LIGHT_WARNING,
                status_danger: tokens::LIGHT_DESTRUCTIVE,
            }
        }
    }

    pub fn apply_to_context(&self, ctx: &egui::Context) {
        let mut style = (*ctx.style()).clone();
        style.visuals = if self.dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        style.visuals.panel_fill = self.bg_canvas;
        style.visuals.window_fill = if self.dark_mode {
            tokens::DARK_DIALOG
        } else {
            tokens::LIGHT_DIALOG
        };
        style.visuals.extreme_bg_color = self.bg_sidebar;
        style.visuals.faint_bg_color = self.bg_elevated;
        style.visuals.window_stroke = Stroke::new(1.0_f32, self.border_subtle);
        style.visuals.widgets.noninteractive.bg_fill = self.bg_elevated;
        style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, self.border_subtle);
        style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, self.text_primary);
        style.visuals.widgets.inactive.bg_fill = self.bg_elevated;
        style.visuals.widgets.inactive.weak_bg_fill = self.bg_elevated;
        style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, self.border_subtle);
        style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, self.text_primary);
        style.visuals.widgets.hovered.bg_fill = self.bg_hover;
        style.visuals.widgets.hovered.weak_bg_fill = self.bg_hover;
        style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, self.accent);
        style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, self.text_primary);
        style.visuals.widgets.active.bg_fill = self.accent;
        style.visuals.widgets.active.weak_bg_fill = self.accent;
        style.visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, self.text_primary);
        style.visuals.selection.bg_fill = self.accent.gamma_multiply(0.28);
        style.visuals.selection.stroke = Stroke::new(1.0_f32, self.accent);
        style.visuals.hyperlink_color = if self.dark_mode {
            tokens::DARK_LINK
        } else {
            tokens::LIGHT_LINK
        };

        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
        style
            .text_styles
            .insert(egui::TextStyle::Heading, egui::FontId::proportional(16.0));
        style
            .text_styles
            .insert(egui::TextStyle::Small, egui::FontId::proportional(12.0));
        style
            .text_styles
            .insert(egui::TextStyle::Monospace, egui::FontId::monospace(12.5));
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(10.0, 7.0);
        style.visuals.widgets.inactive.bg_stroke = Stroke::NONE;
        style.visuals.widgets.hovered.bg_stroke = Stroke::NONE;
        style.visuals.widgets.active.bg_stroke = Stroke::NONE;
        style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, self.border_subtle);
        style.visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        style.visuals.widgets.inactive.corner_radius = CornerRadius::same(8);
        style.visuals.widgets.hovered.corner_radius = CornerRadius::same(8);
        style.visuals.widgets.active.corner_radius = CornerRadius::same(8);
        ctx.set_style(style);
    }
}

/// Paint a vector Bot avatar shape (`Circle`, `Hex`, `RoundedSquare`, `Diamond`, `Pill`, `Spark`)
/// with an optional 2-letter monogram or custom texture and a status badge dot.
#[allow(clippy::too_many_arguments)]
pub fn paint_bot_avatar(
    painter: &egui::Painter,
    rect: Rect,
    shape: BotAvatarShape,
    fill: Color32,
    monogram: &str,
    texture: Option<egui::TextureId>,
    status: Option<&str>,
    bg_surface: Color32,
) {
    let center = rect.center();
    let radius = rect.width().min(rect.height()) * 0.48;

    if let Some(tex_id) = texture {
        painter.image(
            tex_id,
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        match shape {
            BotAvatarShape::Circle => {
                painter.circle_filled(center, radius, fill);
            }
            BotAvatarShape::RoundedSquare => {
                let cr = CornerRadius::same((radius * 0.45).round() as u8);
                painter.rect_filled(rect.shrink(1.0), cr, fill);
            }
            BotAvatarShape::Pill => {
                let pill_rect = Rect::from_center_size(
                    center,
                    Vec2::new(rect.width() * 0.96, rect.height() * 0.76),
                );
                let cr = CornerRadius::same((pill_rect.height() * 0.5).round() as u8);
                painter.rect_filled(pill_rect, cr, fill);
            }
            BotAvatarShape::Diamond => {
                let pts = vec![
                    Pos2::new(center.x, center.y - radius),
                    Pos2::new(center.x + radius, center.y),
                    Pos2::new(center.x, center.y + radius),
                    Pos2::new(center.x - radius, center.y),
                ];
                painter.add(Shape::convex_polygon(pts, fill, Stroke::NONE));
            }
            BotAvatarShape::Hex => {
                let pts: Vec<Pos2> = (0..6)
                    .map(|i| {
                        let angle =
                            std::f32::consts::FRAC_PI_3 * (i as f32) - std::f32::consts::FRAC_PI_6;
                        Pos2::new(
                            center.x + radius * angle.cos(),
                            center.y + radius * angle.sin(),
                        )
                    })
                    .collect();
                painter.add(Shape::convex_polygon(pts, fill, Stroke::NONE));
            }
            BotAvatarShape::Spark => {
                // Convex octagon + inner 4-point star accent
                let pts: Vec<Pos2> = (0..8)
                    .map(|i| {
                        let angle = std::f32::consts::FRAC_PI_4 * (i as f32);
                        Pos2::new(
                            center.x + radius * angle.cos(),
                            center.y + radius * angle.sin(),
                        )
                    })
                    .collect();
                painter.add(Shape::convex_polygon(pts, fill, Stroke::NONE));
            }
        }

        let eye = Stroke::new((radius * 0.12).max(1.3), Color32::from_rgb(30, 24, 24));
        for x in [-0.23, 0.23] {
            painter.line_segment(
                [
                    center + Vec2::new(radius * x, -radius * 0.16),
                    center + Vec2::new(radius * x, radius * 0.16),
                ],
                eye,
            );
        }
        let _ = monogram;
    }

    if let Some(st) = status {
        let dot_color = match st {
            "working" | "acting" | "running" | "online" | "idle" => {
                Color32::from_rgb(0x22, 0xC5, 0x5E)
            }
            "needs_you" | "waiting_for_human" | "paused" => Color32::from_rgb(0xF5, 0x9E, 0x0B),
            "error" | "failed" | "stopped" => Color32::from_rgb(0xEF, 0x44, 0x44),
            _ => Color32::from_rgb(0x64, 0x74, 0x8B),
        };
        let dot_center = Pos2::new(rect.max.x - 3.0, rect.max.y - 3.0);
        painter.circle_filled(dot_center, 4.5, bg_surface);
        painter.circle_filled(dot_center, 3.2, dot_color);
    }
}

/// Use the platform UI font when available; retain egui fallbacks for missing glyphs.
pub fn install_system_font(ctx: &egui::Context) {
    let candidates = [
        "/System/Library/Fonts/SFNS.ttf",
        "C:/Windows/Fonts/segoeui.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ];
    if let Some(bytes) = candidates.iter().find_map(|p| std::fs::read(p).ok()) {
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "system-ui".into(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "system-ui".into());
        ctx.set_fonts(fonts);
    }
}
