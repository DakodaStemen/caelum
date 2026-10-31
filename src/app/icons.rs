//! Symbolic icons drawn as vector strokes (no icon font, no emoji), modelled on
//! Adwaita's 16px symbolic set, plus the flat buttons that host them.
//!
//! Every icon is laid out on a 16-unit grid around a center that is snapped to the
//! physical pixel grid, and straight strokes are snapped too, so icons stay crisp and
//! centered at any zoom / pixels-per-point (the old text "×" drifted and blurred).
use caelum_rs::model::Direction;
use eframe::egui::{
    self, Color32, CornerRadius, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, Vec2,
    emath::GuiRounding,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Menu,
    Plus,
    Close,
    Sidebar,
    Split,
    Arrow(Direction),
    Balance,
    Maximize,
    Restore,
    WindowMinimize,
    WindowMaximize,
    WindowRestore,
    WindowClose,
    Folder,
    File,
}

/// Paints `icon` at `size` points (16 = Adwaita's symbolic size) centered in `rect`.
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, size: f32, color: Color32) {
    let ppp = painter.pixels_per_point();
    let u = size / 16.;
    // Stroke width: ~1.75 units, rounded to whole physical pixels (at least one).
    let px = (1.75 * u * ppp).round().max(1.);
    let width = px / ppp;
    let odd = px as i32 % 2 == 1;
    let snap = |v: f32| {
        if odd {
            v.round_to_pixel_center(ppp)
        } else {
            v.round_to_pixels(ppp)
        }
    };
    let c = rect.center().round_to_pixels(ppp);
    let p = |x: f32, y: f32| Pos2::new(c.x + x * u, c.y + y * u);
    let stroke = Stroke::new(width, color);
    let line = |a: Pos2, b: Pos2| {
        painter.line_segment([a, b], stroke);
    };
    let hline = |x0: f32, x1: f32, y: f32| {
        let y = snap(c.y + y * u);
        line(Pos2::new(c.x + x0 * u, y), Pos2::new(c.x + x1 * u, y));
    };
    let vline = |x: f32, y0: f32, y1: f32| {
        let x = snap(c.x + x * u);
        line(Pos2::new(x, c.y + y0 * u), Pos2::new(x, c.y + y1 * u));
    };
    let frame = |x0: f32, y0: f32, x1: f32, y1: f32, r: f32| {
        let r_box = Rect::from_min_max(
            Pos2::new(snap(c.x + x0 * u), snap(c.y + y0 * u)),
            Pos2::new(snap(c.x + x1 * u), snap(c.y + y1 * u)),
        );
        painter.rect_stroke(
            r_box,
            CornerRadius::same((r * u).round() as u8),
            stroke,
            egui::StrokeKind::Middle,
        );
    };
    let cross = |r: f32| {
        line(p(-r, -r), p(r, r));
        line(p(-r, r), p(r, -r));
    };
    match icon {
        Icon::Menu => {
            for y in [-4.5, 0., 4.5] {
                hline(-6., 6., y);
            }
        }
        Icon::Plus => {
            hline(-6., 6., 0.);
            vline(0., -6., 6.);
        }
        Icon::Close => cross(4.5),
        Icon::WindowClose => cross(4.),
        Icon::Sidebar => {
            frame(-7., -6., 7., 6., 2.);
            vline(-2., -6., 6.);
        }
        Icon::Split => {
            frame(-7., -6., 7., 6., 2.);
            vline(0., -6., 6.);
        }
        Icon::Balance => {
            frame(-7., -6., 7., 6., 2.);
            vline(-2.33, -6., 6.);
            vline(2.33, -6., 6.);
        }
        Icon::Arrow(dir) => {
            // Drawn pointing right, then rotated.
            let rot = |x: f32, y: f32| match dir {
                Direction::Right => p(x, y),
                Direction::Left => p(-x, y),
                Direction::Down => p(y, x),
                Direction::Up => p(y, -x),
            };
            line(rot(-5.5, 0.), rot(5.5, 0.));
            painter.add(Shape::line(
                vec![rot(1., -4.5), rot(5.5, 0.), rot(1., 4.5)],
                stroke,
            ));
        }
        Icon::Maximize | Icon::Restore => {
            // Four corner brackets, pointing out (maximize) or in (restore).
            let (a, b) = if icon == Icon::Maximize {
                (6., 2.)
            } else {
                (2., 6.)
            };
            for (sx, sy) in [(-1., -1.), (1., -1.), (-1., 1.), (1., 1.)] {
                let corner = p(sx * a, sy * a);
                painter.add(Shape::line(
                    vec![p(sx * a, sy * b), corner, p(sx * b, sy * a)],
                    stroke,
                ));
            }
        }
        Icon::WindowMinimize => hline(-4., 4., 3.5),
        Icon::WindowMaximize => frame(-4., -4., 4., 4., 1.),
        Icon::WindowRestore => {
            frame(-4., -2., 2., 4., 1.);
            hline(-2., 4., -4.);
            vline(4., -4., 2.);
        }
        Icon::Folder => {
            let thin = Stroke::new(width.min(1.5 / ppp.max(1.)).max(1. / ppp), color);
            painter.add(Shape::closed_line(
                vec![
                    p(-7., -5.),
                    p(-2., -5.),
                    p(0., -3.),
                    p(7., -3.),
                    p(7., 5.5),
                    p(-7., 5.5),
                ],
                thin,
            ));
        }
        Icon::File => {
            let thin = Stroke::new(width.min(1.5 / ppp.max(1.)).max(1. / ppp), color);
            painter.add(Shape::closed_line(
                vec![p(-5., -7.), p(2., -7.), p(5., -4.), p(5., 7.), p(-5., 7.)],
                thin,
            ));
        }
    }
}

/// Colors for a flat button on a surface of `base` color with foreground `fg`.
#[derive(Clone, Copy)]
pub struct Palette {
    pub base: Color32,
    pub fg: Color32,
}
impl Palette {
    fn fill(self, response: &Response, on: bool) -> Color32 {
        let t = if response.is_pointer_button_down_on() {
            0.20
        } else if response.hovered() || on {
            0.10
        } else {
            return Color32::TRANSPARENT;
        };
        caelum_rs::theme::mix(self.base, self.fg, t)
    }
}

/// A flat (frameless until hovered) icon button, like a button in a GNOME headerbar.
/// `label` is both the tooltip and the accessible name.
pub fn flat(
    ui: &mut egui::Ui,
    icon: Icon,
    size: Vec2,
    icon_size: f32,
    pal: Palette,
    label: &str,
    on: bool,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let fill = pal.fill(&response, on);
        if fill != Color32::TRANSPARENT {
            painter.rect_filled(rect.round_to_pixels(painter.pixels_per_point()), 6, fill);
        }
        paint(painter, rect, icon, icon_size, pal.fg);
    }
    response.on_hover_text(label)
}

/// A round window-control button (libadwaita: 24pt circle, always tinted).
pub fn window_control(ui: &mut egui::Ui, icon: Icon, pal: Palette, label: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(34., 34.), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    if ui.is_rect_visible(rect) {
        let t = if response.is_pointer_button_down_on() {
            0.28
        } else if response.hovered() {
            0.16
        } else {
            0.10
        };
        let painter = ui.painter();
        let center = rect.center().round_to_pixels(painter.pixels_per_point());
        painter.circle_filled(center, 12., caelum_rs::theme::mix(pal.base, pal.fg, t));
        paint(painter, rect, icon, 16., pal.fg);
    }
    response
}
