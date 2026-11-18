//! Small vector controls shared by the desktop shell.
use eframe::egui::{self, Color32, Sense, Stroke, Vec2};

#[derive(Clone, Copy)]
pub enum Icon {
    Search,
    Activity,
    Plus,
    Sidebar,
    Settings,
    More,
    Pages,
    Close,
    Trash,
    Refresh,
}

pub fn button(ui: &mut egui::Ui, icon: Icon, label: &str, color: Color32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    if response.hovered() || response.has_focus() {
        ui.painter()
            .rect_filled(rect, 8, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    paint(ui.painter(), rect.shrink(7.0), icon, color);
    response.on_hover_text(label)
}

pub fn labeled_button(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    color: Color32,
) -> egui::Response {
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_string(), egui::FontId::proportional(14.0), color);
    let width = 16.0 + 6.0 + galley.size().x + 16.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    if response.hovered() || response.has_focus() {
        ui.painter()
            .rect_filled(rect, 8, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 14.0, rect.center().y),
        Vec2::splat(14.0),
    );
    paint(ui.painter(), icon_rect, icon, color);
    ui.painter().galley(
        egui::pos2(rect.left() + 26.0, rect.center().y - galley.size().y * 0.5),
        galley,
        color,
    );
    response.on_hover_text(label)
}

pub fn paint(p: &egui::Painter, rect: egui::Rect, icon: Icon, color: Color32) {
    let xy = |x: f32, y: f32| {
        egui::pos2(
            rect.left() + rect.width() * x,
            rect.top() + rect.height() * y,
        )
    };
    let stroke = Stroke::new(1.5_f32, color);
    let line = |a, b| {
        p.line_segment([a, b], stroke);
    };
    match icon {
        Icon::Search => {
            p.circle_stroke(xy(0.43, 0.43), rect.width() * 0.32, stroke);
            line(xy(0.68, 0.68), xy(0.95, 0.95));
        }
        Icon::Plus => {
            line(xy(0.5, 0.1), xy(0.5, 0.9));
            line(xy(0.1, 0.5), xy(0.9, 0.5));
        }
        Icon::Sidebar => {
            p.rect_stroke(rect.shrink(1.0), 3, stroke, egui::StrokeKind::Inside);
            line(xy(0.35, 0.1), xy(0.35, 0.9));
        }
        Icon::Activity => {
            p.circle_stroke(rect.center(), rect.width() * 0.42, stroke);
            line(xy(0.5, 0.22), xy(0.5, 0.52));
            line(xy(0.5, 0.52), xy(0.72, 0.65));
        }
        Icon::More => {
            for x in [0.2, 0.5, 0.8] {
                p.circle_filled(xy(x, 0.5), 1.5, color);
            }
        }
        Icon::Settings => {
            p.circle_stroke(rect.center(), rect.width() * 0.23, stroke);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                let v = Vec2::angled(a);
                line(
                    rect.center() + v * rect.width() * 0.34,
                    rect.center() + v * rect.width() * 0.47,
                );
            }
        }
        Icon::Pages => {
            p.rect_stroke(
                rect.shrink2(Vec2::new(3.0, 0.0)),
                3,
                stroke,
                egui::StrokeKind::Inside,
            );
            line(xy(0.35, 0.4), xy(0.65, 0.4));
            line(xy(0.35, 0.65), xy(0.65, 0.65));
        }
        Icon::Close => {
            line(xy(0.2, 0.2), xy(0.8, 0.8));
            line(xy(0.8, 0.2), xy(0.2, 0.8));
        }
        Icon::Trash => {
            line(xy(0.2, 0.25), xy(0.8, 0.25));
            line(xy(0.38, 0.15), xy(0.62, 0.15));
            p.rect_stroke(
                egui::Rect::from_min_max(xy(0.25, 0.28), xy(0.75, 0.88)),
                2,
                stroke,
                egui::StrokeKind::Inside,
            );
            line(xy(0.42, 0.4), xy(0.42, 0.76));
            line(xy(0.58, 0.4), xy(0.58, 0.76));
        }
        Icon::Refresh => {
            p.circle_stroke(rect.center(), rect.width() * 0.36, stroke);
            line(xy(0.72, 0.15), xy(0.9, 0.35));
            line(xy(0.62, 0.38), xy(0.9, 0.35));
        }
    }
}
