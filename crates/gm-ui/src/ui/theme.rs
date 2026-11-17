//! GrokBot Design System Tokens, Dark/Light/System Theme Application,
//! 12-Color Bot Palette, Accent Mode (`bot_color` vs `black`), and Vector Bot Shape Primitives.

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

        if dark_mode {
            let accent = if monochrome {
                Color32::from_rgb(0xF0, 0xF4, 0xF8)
            } else {
                bot_color
            };
            let accent_text = if monochrome {
                Color32::from_rgb(0x0B, 0x0D, 0x10)
            } else {
                Color32::WHITE
            };
            Self {
                dark_mode: true,
                bg_canvas: Color32::from_rgb(0x0B, 0x0D, 0x10),
                bg_sidebar: Color32::from_rgb(0x11, 0x15, 0x1B),
                bg_elevated: Color32::from_rgb(0x16, 0x1B, 0x22),
                bg_hover: Color32::from_rgb(0x1C, 0x23, 0x2D),
                border_subtle: Color32::from_rgb(0x23, 0x2A, 0x35),
                text_primary: Color32::from_rgb(0xF0, 0xF4, 0xF8),
                text_secondary: Color32::from_rgb(0x9B, 0xA3, 0xB0),
                text_muted: Color32::from_rgb(0x64, 0x6E, 0x7D),
                accent,
                accent_text,
                status_ok: Color32::from_rgb(0x22, 0xC5, 0x5E),
                status_warn: Color32::from_rgb(0xF5, 0x9E, 0x0B),
                status_danger: Color32::from_rgb(0xEF, 0x44, 0x44),
            }
        } else {
            let accent = if monochrome {
                Color32::from_rgb(0x0F, 0x17, 0x2A)
            } else {
                bot_color
            };
            Self {
                dark_mode: false,
                bg_canvas: Color32::from_rgb(0xF8, 0xFA, 0xFC),
                bg_sidebar: Color32::from_rgb(0xF1, 0xF5, 0xF9),
                bg_elevated: Color32::from_rgb(0xFF, 0xFF, 0xFF),
                bg_hover: Color32::from_rgb(0xE2, 0xE8, 0xF0),
                border_subtle: Color32::from_rgb(0xE2, 0xE8, 0xF0),
                text_primary: Color32::from_rgb(0x0F, 0x17, 0x2A),
                text_secondary: Color32::from_rgb(0x47, 0x55, 0x69),
                text_muted: Color32::from_rgb(0x94, 0xA3, 0xB8),
                accent,
                accent_text: Color32::WHITE,
                status_ok: Color32::from_rgb(0x16, 0xA3, 0x4A),
                status_warn: Color32::from_rgb(0xD9, 0x77, 0x06),
                status_danger: Color32::from_rgb(0xDC, 0x26, 0x26),
            }
        }
    }

    /// Apply this palette to `egui::Context` only when the theme/accent parameters change.
    pub fn apply_to_context(&self, ctx: &egui::Context) {
        let mut style = (*ctx.style()).clone();
        style.visuals = if self.dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        style.visuals.panel_fill = self.bg_canvas;
        style.visuals.window_fill = self.bg_elevated;
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
        style.visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, self.accent_text);
        style.visuals.selection.bg_fill = self.accent.gamma_multiply(0.28);
        style.visuals.selection.stroke = Stroke::new(1.0_f32, self.accent);
        style.visuals.hyperlink_color = self.accent;

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
        style.spacing.button_padding = Vec2::new(10.0, 5.0);
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

        let initials: String = monogram.chars().take(2).collect();
        if !initials.is_empty() {
            painter.text(
                center,
                egui::Align2::CENTER_CENTER,
                initials.to_uppercase(),
                egui::FontId::proportional((radius * 0.85).clamp(9.0, 16.0)),
                Color32::WHITE,
            );
        }
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
