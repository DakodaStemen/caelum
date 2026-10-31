//! libadwaita-style building blocks drawn with egui: dialogs with a headerbar, boxed-list
//! preference groups and rows, switches, spin buttons, drop-downs and alert dialogs.
use super::icons::{self, Icon, Palette};
use caelum_rs::theme::{Theme, mix};
use eframe::egui::{
    self, Align, Color32, CornerRadius, Layout, Margin, Pos2, Rect, Response, RichText, Sense,
    Stroke, Vec2, emath::GuiRounding,
};
use std::sync::Arc;

/// Text drawn twice a physical pixel apart: GTK's bold weight, which egui cannot select
/// from the desktop's variable font.
pub fn paint_bold(painter: &egui::Painter, pos: Pos2, galley: Arc<egui::Galley>, color: Color32) {
    let px = 1. / painter.pixels_per_point();
    painter.galley(pos, galley.clone(), color);
    painter.galley(pos + Vec2::new(px, 0.), galley, color);
}

/// A bold label (heading of a group, dialog title).
pub fn bold(ui: &mut egui::Ui, text: &str, size: Option<f32>, color: Color32) -> Response {
    let font = size.map_or_else(
        || egui::TextStyle::Body.resolve(ui.style()),
        egui::FontId::proportional,
    );
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, color);
    let (rect, response) =
        ui.allocate_exact_size(galley.size() + Vec2::new(1., 0.), Sense::hover());
    let pos = rect.min.round_to_pixels(ui.pixels_per_point());
    paint_bold(ui.painter(), pos, galley, color);
    response
}

/// Colors shared by the widgets in this module.
#[derive(Clone, Copy)]
pub struct Colors {
    pub dialog: Color32,
    pub card: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub danger: Color32,
    pub line: Color32,
}
impl Colors {
    pub fn new(theme: &Theme) -> Self {
        let dialog = theme.ui("background");
        let text = theme.ui("text");
        let light = theme.kind == "light";
        Self {
            dialog,
            card: if light {
                theme.ui("surface")
            } else {
                mix(dialog, text, 0.08)
            },
            text,
            muted: theme.ui("muted"),
            accent: theme.ui("accent"),
            danger: theme.ui("danger"),
            line: if light {
                mix(dialog, Color32::BLACK, 0.07)
            } else {
                mix(dialog, Color32::BLACK, 0.12)
            },
        }
    }
}

/// An `AdwDialog`: dims the window, a headerbar with a bold title and a round close
/// button, scrolling content. Returns whether it should close (button, Escape or a
/// click outside).
pub fn dialog(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    size: Vec2,
    theme: &Theme,
    content: impl FnOnce(&mut egui::Ui),
) -> bool {
    let c = Colors::new(theme);
    let screen = ctx.content_rect();
    let size = Vec2::new(
        size.x.min(screen.width() - 24.),
        size.y.min(screen.height() - 24.),
    );
    let mut close = false;
    let response = egui::Modal::new(egui::Id::new(id))
        .backdrop_color(Color32::from_black_alpha(if theme.kind == "light" {
            40
        } else {
            90
        }))
        .frame(
            egui::Frame::new()
                .fill(c.dialog)
                .corner_radius(CornerRadius::same(14))
                .stroke(ctx.style().visuals.window_stroke)
                .shadow(ctx.style().visuals.window_shadow),
        )
        .show(ctx, |ui| {
            ui.set_width(size.x);
            ui.set_height(size.y);
            let top = ui.max_rect().top();
            let header = Rect::from_min_size(ui.max_rect().min, Vec2::new(size.x, 47.));
            let galley = ui.painter().layout_no_wrap(
                title.to_string(),
                egui::TextStyle::Body.resolve(ui.style()),
                c.text,
            );
            paint_bold(
                ui.painter(),
                (header.center() - galley.size() / 2.).round_to_pixels(ui.pixels_per_point()),
                galley,
                c.text,
            );
            let button = Rect::from_center_size(
                Pos2::new(header.right() - 24., header.center().y),
                Vec2::splat(34.),
            );
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(button));
            let pal = Palette {
                base: c.dialog,
                fg: c.text,
            };
            close |= icons::window_control(&mut child, Icon::Close, pal, "Close").clicked();
            let body = Rect::from_min_max(
                Pos2::new(header.left(), top + 47.),
                ui.max_rect().max - Vec2::new(4., 0.),
            );
            let mut inner = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(body)
                    .layout(Layout::top_down(Align::Min)),
            );
            // Keep scrolled content inside the dialog's rounded bottom.
            inner.set_clip_rect(
                body.with_max_y(body.bottom() - 6.)
                    .intersect(ui.clip_rect()),
            );
            // A dialog opens scrolled to the top, like a new AdwDialog.
            let frame_nr = ctx.cumulative_frame_nr();
            let seen = egui::Id::new((id, "shown"));
            let reopened = ctx.data_mut(|d| {
                let last = d.get_temp::<u64>(seen);
                d.insert_temp(seen, frame_nr);
                last.is_none_or(|f| f + 1 < frame_nr)
            });
            let mut scroll = egui::ScrollArea::vertical()
                .scroll_source(
                    egui::scroll_area::ScrollSource::MOUSE_WHEEL
                        | egui::scroll_area::ScrollSource::SCROLL_BAR,
                )
                .id_salt((id, "scroll"))
                .auto_shrink([false, false]);
            if reopened {
                scroll = scroll.vertical_scroll_offset(0.);
            }
            scroll.show(&mut inner, |ui| {
                // Content is a centered column, like AdwPreferencesPage's clamp.
                let width = (size.x - 48.).min(560.);
                let pad = ((size.x - width) / 2.).max(12.);
                egui::Frame::new()
                    .inner_margin(Margin {
                        left: pad as i8,
                        right: pad as i8,
                        top: 12,
                        bottom: 24,
                    })
                    .show(ui, |ui| {
                        ui.set_width(width);
                        ui.spacing_mut().item_spacing.y = 6.;
                        content(ui);
                    });
            });
        });
    close || response.should_close()
}

/// An `AdwPreferencesGroup`: a bold title, an optional description, then its rows in a
/// rounded card with lines between them.
pub fn group(
    ui: &mut egui::Ui,
    theme: &Theme,
    title: &str,
    description: Option<&str>,
    rows: impl FnOnce(&mut Group),
) {
    let c = Colors::new(theme);
    ui.add_space(18.);
    if !title.is_empty() {
        bold(ui, title, None, c.text);
    }
    if let Some(d) = description {
        ui.label(RichText::new(d).color(c.muted));
    }
    ui.add_space(6.);
    // Light cards get libadwaita's faint outline; dark ones stand out by color.
    let outline = if theme.kind == "light" {
        Stroke::new(1_f32, mix(c.dialog, Color32::BLACK, 0.09))
    } else {
        Stroke::NONE
    };
    let frame = egui::Frame::new()
        .fill(c.card)
        .stroke(outline)
        .corner_radius(CornerRadius::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.;
            let mut g = Group {
                ui,
                colors: c,
                rows: 0,
            };
            rows(&mut g);
        });
    let _ = frame;
}

pub struct Group<'a> {
    pub ui: &'a mut egui::Ui,
    pub colors: Colors,
    rows: usize,
}
impl Group<'_> {
    fn separator(&mut self) {
        if self.rows > 0 {
            let ui = &mut *self.ui;
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.), Sense::hover());
            ui.painter().rect_filled(rect, 0., self.colors.line);
        }
        self.rows += 1;
    }
    /// An `AdwActionRow`: title (and subtitle) on the left, `suffix` widgets on the right.
    /// The returned response covers the row, for activatable rows.
    pub fn row(
        &mut self,
        title: &str,
        subtitle: Option<&str>,
        suffix: impl FnOnce(&mut egui::Ui),
    ) -> Response {
        self.separator();
        let c = self.colors;
        let ui = &mut *self.ui;
        let height = if subtitle.is_some() { 64. } else { 52. };
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
        let inner = rect.shrink2(Vec2::new(12., 0.));
        // The suffix first, so the title can be cut short to fit beside it.
        let mut right = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(Layout::right_to_left(Align::Center)),
        );
        right.spacing_mut().item_spacing.x = 8.;
        suffix(&mut right);
        let room = (right.min_rect().left() - inner.left() - 12.).max(20.);
        let title_g = super::truncated(ui, title, egui::TextStyle::Body, c.text, room);
        let sub_g =
            subtitle.map(|s| super::truncated(ui, s, egui::TextStyle::Small, c.muted, room));
        let h = title_g.size().y + sub_g.as_ref().map_or(0., |g| g.size().y + 2.);
        let mut y = rect.center().y - h / 2.;
        let ppp = ui.pixels_per_point();
        let ty = title_g.size().y;
        ui.painter().galley(
            Pos2::new(inner.left(), y).round_to_pixels(ppp),
            title_g,
            c.text,
        );
        y += ty + 2.;
        if let Some(g) = sub_g {
            ui.painter()
                .galley(Pos2::new(inner.left(), y).round_to_pixels(ppp), g, c.muted);
        }
        let label_area =
            Rect::from_min_max(rect.min, Pos2::new(right.min_rect().left(), rect.max.y));
        ui.interact(label_area, ui.id().with(("row", title)), Sense::click())
    }
    /// A row whose whole surface is a button, with a `go-next` chevron (opens a subpage
    /// or runs an action).
    pub fn link_row(&mut self, title: &str, subtitle: Option<&str>) -> Response {
        let fg = self.colors.text;
        let card = self.colors.card;
        let response = self.row(title, subtitle, |ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(16.), Sense::hover());
            icons::paint(
                ui,
                rect,
                Icon::Arrow(caelum_rs::model::Direction::Right),
                16.,
                fg,
            );
        });
        if response.hovered() {
            self.ui.painter().rect_filled(
                response
                    .rect
                    .expand2(Vec2::new(1000., 0.))
                    .intersect(response.rect),
                0,
                mix(card, fg, 0.04),
            );
        }
        // Around the whole row, chevron included.
        let row = response.rect.with_max_x(self.ui.max_rect().right());
        icons::focus_ring(self.ui, &response, row, 8);
        response
    }
    /// A row holding arbitrary content (an entry, a grid of swatches).
    pub fn content(&mut self, add: impl FnOnce(&mut egui::Ui)) {
        self.separator();
        egui::Frame::new()
            .inner_margin(Margin::same(12))
            .show(self.ui, |ui| {
                ui.set_width(ui.available_width());
                add(ui);
            });
    }
}

/// A `GtkSwitch`, accent-filled when on.
pub fn switch(ui: &mut egui::Ui, on: &mut bool, c: &Colors) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(Vec2::new(48., 26.), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, *on, ""));
    let t = ui.ctx().animate_bool_responsive(response.id, *on);
    let off = mix(c.card, c.text, if response.hovered() { 0.22 } else { 0.16 });
    let track = lerp_color(off, c.accent, t);
    let painter = ui.painter();
    let rect = rect.round_to_pixels(ui.pixels_per_point());
    painter.rect_filled(rect, 13, track);
    let x = egui::lerp(rect.left() + 13.0..=rect.right() - 13., t);
    painter.circle_filled(Pos2::new(x, rect.center().y), 10., Color32::WHITE);
    icons::focus_ring(ui, &response, rect.expand(3.), 16);
    response
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    mix(a, b, t)
}

/// A spin button: the value, then round − and + buttons.
pub fn spin(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f32,
    decimals: usize,
    c: &Colors,
) -> Response {
    let pal = Palette {
        base: c.card,
        fg: c.text,
    };
    // Right to left: +, −, value.
    let plus = round_button(ui, Icon::Add, pal, "Increase");
    let minus = round_button(ui, Icon::Remove, pal, "Decrease");
    let mut changed = false;
    if plus.clicked() {
        *value = (*value + step).min(*range.end());
        changed = true;
    }
    if minus.clicked() {
        *value = (*value - step).max(*range.start());
        changed = true;
    }
    // Snap away float drift from repeated steps.
    let scale = 10_f32.powi(decimals as i32);
    *value = (*value * scale).round() / scale;
    let mut r = ui.label(RichText::new(format!("{:.*}", decimals, *value)).color(c.text));
    if changed {
        r.mark_changed();
    }
    r
}

/// A circular flat button (`.circular`), tinted at rest like libadwaita's.
pub fn round_button(ui: &mut egui::Ui, icon: Icon, pal: Palette, label: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(34.), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    let t = if response.is_pointer_button_down_on() {
        0.24
    } else if response.hovered() {
        0.16
    } else {
        0.10
    };
    let center = rect.center().round_to_pixels(ui.pixels_per_point());
    ui.painter()
        .circle_filled(center, 17., mix(pal.base, pal.fg, t));
    icons::paint(ui, rect, icon, 16., pal.fg);
    if response.has_focus() {
        ui.painter().circle_stroke(
            center,
            16.,
            Stroke::new(2_f32, ui.visuals().hyperlink_color.gamma_multiply(0.5)),
        );
    }
    response.on_hover_text(label)
}

/// A `GtkDropDown`: the selected item and a `pan-down` arrow; the popover lists the
/// choices with a check beside the selected one. Returns the index picked this frame.
pub fn dropdown(
    ui: &mut egui::Ui,
    id: &str,
    selected: &str,
    choices: &[&str],
    c: &Colors,
) -> Option<usize> {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let galley = ui
        .painter()
        .layout_no_wrap(selected.to_string(), font, c.text);
    let width = (galley.size().x + 12. + 8. + 16. + 12.).max(120.);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 34.), Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::Default);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, selected));
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
    let t = if response.is_pointer_button_down_on() || open {
        0.16
    } else if response.hovered() {
        0.13
    } else {
        0.09
    };
    let rect = rect.round_to_pixels(ui.pixels_per_point());
    ui.painter().rect_filled(rect, 8, mix(c.card, c.text, t));
    icons::focus_ring(ui, &response, rect, 8);
    ui.painter().galley(
        Pos2::new(rect.left() + 12., rect.center().y - galley.size().y / 2.)
            .round_to_pixels(ui.pixels_per_point()),
        galley,
        c.text,
    );
    let arrow = Rect::from_center_size(
        Pos2::new(rect.right() - 20., rect.center().y),
        Vec2::splat(16.),
    );
    icons::paint(ui, arrow, Icon::PanDown, 16., c.text);
    let mut picked = None;
    egui::Popup::menu(&response)
        .id(egui::Id::new(("dropdown", id)))
        .align(egui::RectAlign::BOTTOM_START)
        .gap(4.)
        .show(|ui| {
            super::headerbar::menu_spacing(ui);
            let menu_width = width.max(160.);
            ui.set_width(menu_width);
            for (i, choice) in choices.iter().enumerate() {
                let check = *choice == selected;
                let mut b = egui::Button::new(*choice)
                    .frame_when_inactive(false)
                    .min_size(Vec2::new(menu_width, 32.));
                if check {
                    b = b.right_text(icons::image(Icon::Check, 16., c.text));
                }
                if ui.add(b).clicked() {
                    picked = Some(i);
                }
            }
        });
    picked
}

/// How a dialog button looks.
#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Normal,
    Suggested,
    Destructive,
}

/// A raised dialog button: tinted, accent-filled when suggested, red text when
/// destructive.
pub fn button(ui: &mut egui::Ui, label: &str, kind: Kind, width: f32, c: &Colors) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 40.), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    let rest = match kind {
        Kind::Suggested => c.accent,
        _ => mix(c.dialog, c.text, 0.10),
    };
    let fill = if response.is_pointer_button_down_on() {
        mix(rest, c.text, 0.18)
    } else if response.hovered() {
        mix(rest, c.text, 0.08)
    } else {
        rest
    };
    let fg = match kind {
        Kind::Suggested => Color32::WHITE,
        Kind::Destructive => c.danger,
        Kind::Normal => c.text,
    };
    let rect = rect.round_to_pixels(ui.pixels_per_point());
    ui.painter().rect_filled(rect, 10, fill);
    icons::focus_ring(ui, &response, rect, 10);
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::TextStyle::Body.resolve(ui.style()),
        fg,
    );
    paint_bold(
        ui.painter(),
        (rect.center() - galley.size() / 2.).round_to_pixels(ui.pixels_per_point()),
        galley,
        fg,
    );
    response
}

/// An `AdwAlertDialog`: bold heading, body, optional extra content (an entry), and a
/// row of equal buttons. Returns the index of the clicked button; `cancel` (the index
/// Escape or a click outside choose) is returned for those.
#[allow(clippy::too_many_arguments)]
pub fn alert(
    ctx: &egui::Context,
    id: &str,
    heading: &str,
    body: &str,
    buttons: &[(&str, Kind)],
    cancel: usize,
    theme: &Theme,
    extra: impl FnOnce(&mut egui::Ui),
) -> Option<usize> {
    let c = Colors::new(theme);
    let width = 372_f32.min(ctx.content_rect().width() - 24.);
    let mut picked = None;
    let response = egui::Modal::new(egui::Id::new(id))
        .backdrop_color(Color32::from_black_alpha(if theme.kind == "light" {
            40
        } else {
            90
        }))
        .frame(
            egui::Frame::new()
                .fill(c.dialog)
                .corner_radius(CornerRadius::same(14))
                .stroke(ctx.style().visuals.window_stroke)
                .shadow(ctx.style().visuals.window_shadow)
                .inner_margin(Margin::same(24)),
        )
        .show(ctx, |ui| {
            let inner = width - 48.;
            ui.set_width(inner);
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = 10.;
                bold(ui, heading, Some(ui_size(ui) * 1.25), c.text);
                if !body.is_empty() {
                    ui.add(
                        egui::Label::new(RichText::new(body).color(c.text))
                            .wrap_mode(egui::TextWrapMode::Wrap),
                    );
                }
            });
            ui.add_space(4.);
            extra(ui);
            ui.add_space(14.);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.;
                let n = buttons.len() as f32;
                let w = (inner - 12. * (n - 1.)) / n;
                for (i, (label, kind)) in buttons.iter().enumerate() {
                    if button(ui, label, *kind, w, &c).clicked() {
                        picked = Some(i);
                    }
                }
            });
        });
    if picked.is_none() && response.should_close() {
        picked = Some(cancel);
    }
    picked
}

fn ui_size(ui: &egui::Ui) -> f32 {
    egui::TextStyle::Body.resolve(ui.style()).size
}

/// A single-line entry styled like `GtkEntry` (tinted, rounded, accent focus ring).
pub fn entry<'t>(text: &'t mut String, hint: &str, c: &Colors) -> egui::TextEdit<'t> {
    egui::TextEdit::singleline(text)
        .hint_text(RichText::new(hint).color(c.muted))
        .margin(Vec2::new(10., 8.))
        .desired_width(f32::INFINITY)
}
