//! Window chrome: a GNOME-style headerbar, a tab bar that only appears with more than
//! one tab, the main menu, toasts, and (with client-side decorations) window
//! controls, moving and resizing.
use super::icons::{self, Icon, Palette};
use super::*;
use caelum_rs::appearance::WindowButton;
use egui::{CornerRadius, Margin, ResizeDirection, emath::GuiRounding};

/// libadwaita headerbar height (47 incl. its bottom shade line).
const BAR: f32 = 46.;
const TAB_BAR: f32 = 40.;

impl App {
    pub(super) fn headerbar(&mut self, ctx: &egui::Context, theme: &Theme) {
        let chrome = theme.ui("chrome");
        let pal = Palette {
            base: chrome,
            fg: theme.ui("text"),
        };
        let maximized = ctx.input(|i| {
            i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false)
        });
        egui::TopBottomPanel::top("headerbar")
            .exact_height(BAR)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(chrome)
                    .inner_margin(Margin::symmetric(6, 0)),
            )
            .show(ctx, |ui| {
                let bar = ui.max_rect();
                if self.csd {
                    // Registered first so every button drawn later wins the pointer.
                    let drag = ui.interact(
                        bar,
                        egui::Id::new("headerbar-move"),
                        Sense::click_and_drag(),
                    );
                    if drag.drag_started_by(egui::PointerButton::Primary)
                        && self.resize_edge.is_none()
                    {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    if drag.double_clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                    }
                }
                let buttons = self.look.buttons.clone();
                // Left cluster.
                let mut left = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(bar)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                left.spacing_mut().item_spacing.x = 6.;
                if self.csd {
                    self.window_controls(&mut left, &buttons.left, pal, maximized, ctx);
                }
                let size = Vec2::splat(34.);
                if icons::flat(
                    &mut left,
                    Icon::Sidebar,
                    size,
                    16.,
                    pal,
                    &self.tip("Workspaces", "toggle_sidebar"),
                    self.state.preferences.sidebar,
                )
                .clicked()
                {
                    self.action(Action::Sidebar, ctx);
                }
                if icons::flat(
                    &mut left,
                    Icon::Plus,
                    size,
                    16.,
                    pal,
                    &self.tip("New tab", "new_tab"),
                    false,
                )
                .clicked()
                {
                    self.action(Action::NewTab, ctx);
                }
                let left_edge = left.min_rect().right();
                // Right cluster.
                let mut right = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(bar)
                        .layout(egui::Layout::right_to_left(egui::Align::Center)),
                );
                right.spacing_mut().item_spacing.x = 6.;
                if self.csd {
                    let mut order = buttons.right.clone();
                    order.reverse();
                    self.window_controls(&mut right, &order, pal, maximized, ctx);
                }
                let menu = icons::flat(&mut right, Icon::Menu, size, 16., pal, "Main menu", false);
                self.main_menu(&menu);
                let split = icons::flat(&mut right, Icon::Split, size, 16., pal, "Split", false);
                if let Some(a) = self.split_popover(&split, theme) {
                    self.pending_action = Some(a);
                }
                let right_edge = right.min_rect().left();
                // Title, centered on the window when it fits between the clusters.
                let room = (right_edge - left_edge - 24.).max(0.);
                let title = self.window_title_text();
                let subtitle =
                    (self.state.workspaces.len() > 1).then(|| self.workspace().name.clone());
                let galley = truncated(ui, &title, egui::TextStyle::Body, pal.fg, room);
                let sub = subtitle
                    .map(|s| truncated(ui, &s, egui::TextStyle::Small, theme.ui("muted"), room));
                let height = galley.size().y + sub.as_ref().map_or(0., |g| g.size().y);
                let width = galley.size().x.max(sub.as_ref().map_or(0., |g| g.size().x));
                let cx = bar.center().x.clamp(
                    left_edge + 12. + width / 2.,
                    (right_edge - 12. - width / 2.).max(left_edge),
                );
                let top = bar.center().y - height / 2.;
                let painter = ui.painter();
                let ppp = painter.pixels_per_point();
                painter.galley(
                    Pos2::new(cx - galley.size().x / 2., top).round_to_pixels(ppp),
                    galley.clone(),
                    pal.fg,
                );
                if let Some(sub) = sub {
                    painter.galley(
                        Pos2::new(cx - sub.size().x / 2., top + galley.size().y)
                            .round_to_pixels(ppp),
                        sub,
                        theme.ui("muted"),
                    );
                }
            });
        if self.workspace().tabs.len() > 1 {
            egui::TopBottomPanel::top("tabbar")
                .exact_height(TAB_BAR)
                .show_separator_line(false)
                .frame(
                    egui::Frame::new()
                        .fill(chrome)
                        .inner_margin(Margin::symmetric(6, 0)),
                )
                .show(ctx, |ui| self.tab_bar(ui, ctx, theme));
        }
        // The headerbar's bottom shade line.
        egui::TopBottomPanel::top("headerbar-shade")
            .exact_height(1.)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(theme.ui("border")))
            .show(ctx, |_| {});
        let title = self.window_title_text();
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }
    /// What the window is called: the focused terminal's title, else the tab name.
    fn window_title_text(&self) -> String {
        self.terminals
            .get(&self.focused)
            .map(|s| s.title.clone())
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| self.tab_label(self.tab()))
    }
    /// A tab shows its name once renamed, else its first terminal's title.
    fn tab_label(&self, tab: &Tab) -> String {
        if tab.name != "Terminal" {
            return tab.name.clone();
        }
        let mut leaves = vec![];
        tab.layout.leaves(&mut leaves);
        leaves
            .iter()
            .find_map(|p| self.terminals.get(p.id()))
            .map(|s| s.title.clone())
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| tab.name.clone())
    }
    /// `name` plus its shortcut, for tooltips.
    pub(super) fn tip(&self, name: &str, action: &str) -> String {
        match self.keymap.get(action) {
            Some(b) => format!("{name} ({})", binding_label(b)),
            None => name.to_string(),
        }
    }
    fn window_controls(
        &mut self,
        ui: &mut egui::Ui,
        buttons: &[WindowButton],
        pal: Palette,
        maximized: bool,
        ctx: &egui::Context,
    ) {
        for b in buttons {
            let (icon, label) = match b {
                WindowButton::Minimize => (Icon::WindowMinimize, "Minimize"),
                WindowButton::Maximize if maximized => (Icon::WindowRestore, "Restore"),
                WindowButton::Maximize => (Icon::WindowMaximize, "Maximize"),
                WindowButton::Close => (Icon::WindowClose, "Close"),
            };
            if icons::window_control(ui, icon, pal, label).clicked() {
                ctx.send_viewport_cmd(match b {
                    WindowButton::Minimize => egui::ViewportCommand::Minimized(true),
                    WindowButton::Maximize => egui::ViewportCommand::Maximized(!maximized),
                    WindowButton::Close => egui::ViewportCommand::Close,
                });
            }
        }
    }
    fn main_menu(&mut self, anchor: &egui::Response) {
        let mut chosen = None;
        egui::Popup::menu(anchor)
            .align(egui::RectAlign::BOTTOM_END)
            .gap(4.)
            .show(|ui| {
                ui.set_width(250.);
                let item = |ui: &mut egui::Ui, label: &str, key: &str, action: Action| {
                    let mut button = egui::Button::new(label)
                        .frame_when_inactive(false)
                        .min_size(Vec2::new(ui.max_rect().width(), 30.));
                    if let Some(b) = self.keymap.get(key) {
                        button = button.shortcut_text(binding_label(b));
                    }
                    ui.add(button).clicked().then_some(action)
                };
                let mut picks = vec![
                    item(ui, "New Tab", "new_tab", Action::NewTab),
                    item(ui, "New Workspace", "", Action::NewWorkspace),
                ];
                ui.separator();
                picks.push(item(ui, "Open File…", "open_file", Action::Open));
                ui.menu_button("New Pane", |ui| {
                    ui.set_width(190.);
                    for (label, action) in [
                        ("Files", Action::Files),
                        ("System Monitor", Action::System),
                        ("Processes", Action::Processes),
                        ("SSH Connection…", Action::Connect),
                        ("Codex", Action::Assistant("codex".into())),
                        ("Claude", Action::Assistant("claude".into())),
                    ] {
                        picks.push(item(ui, label, "", action));
                    }
                });
                ui.separator();
                picks.push(item(ui, "Balance Panes", "balance_panes", Action::Equalize));
                picks.push(item(ui, "Zoom Pane", "zoom_pane", Action::Zoom));
                picks.push(item(ui, "Find…", "search", Action::Search));
                ui.separator();
                picks.push(item(
                    ui,
                    "Command Palette",
                    "command_palette",
                    Action::Palette,
                ));
                picks.push(item(ui, "Preferences", "", Action::Settings));
                picks.push(item(ui, "About Caelum", "", Action::About));
                chosen = picks.into_iter().flatten().next();
            });
        if chosen.is_some() {
            self.pending_action = chosen;
        }
    }
    fn tab_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, theme: &Theme) {
        let chrome = theme.ui("chrome");
        let tabs: Vec<_> = self
            .workspace()
            .tabs
            .iter()
            .map(|t| (t.id.clone(), self.tab_label(t)))
            .collect();
        let area = ui.max_rect().shrink2(Vec2::new(0., 4.));
        let gap = 4.;
        let n = tabs.len() as f32;
        let width = ((area.width() - gap * (n - 1.)) / n).max(60.);
        let mut x = area.left();
        for (id, name) in tabs {
            let rect =
                Rect::from_min_size(Pos2::new(x, area.top()), Vec2::new(width, area.height()));
            x += width + gap;
            let active = self.workspace().active_tab == id;
            let response = ui.interact(rect, egui::Id::new(("tab", &id)), Sense::click());
            response
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &name));
            let fill = if active {
                theme::mix(chrome, theme.ui("text"), 0.10)
            } else if response.hovered() {
                theme::mix(chrome, theme.ui("text"), 0.05)
            } else {
                Color32::TRANSPARENT
            };
            let painter = ui.painter();
            painter.rect_filled(rect.round_to_pixels(painter.pixels_per_point()), 6, fill);
            let fg = if active {
                theme.ui("text")
            } else {
                theme.ui("textSecondary")
            };
            let close_room = 30.;
            let galley = truncated(
                ui,
                &name,
                egui::TextStyle::Body,
                fg,
                rect.width() - 2. * close_room,
            );
            let pos = rect.center() - galley.size() / 2.;
            ui.painter()
                .galley(pos.round_to_pixels(ui.pixels_per_point()), galley, fg);
            let mut close = response.middle_clicked();
            if active || response.hovered() || ui.rect_contains_pointer(rect) {
                let button = Rect::from_center_size(
                    Pos2::new(rect.right() - 17., rect.center().y),
                    Vec2::splat(24.),
                );
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(button));
                let pal = Palette {
                    base: fill_or(fill, chrome),
                    fg,
                };
                close |= icons::flat(
                    &mut child,
                    Icon::Close,
                    Vec2::splat(24.),
                    14.,
                    pal,
                    "Close tab",
                    false,
                )
                .clicked();
            }
            if close {
                self.workspace_mut().active_tab = id;
                self.pending = Some(PendingClose::Tab);
                continue;
            }
            if response.clicked() {
                self.workspace_mut().active_tab = id.clone();
                self.zoom = None;
                self.ensure_focus();
            }
            if response.double_clicked() {
                self.dialog = Some("Rename tab".into());
                self.dialog_input = self.tab().name.clone();
            }
        }
        let _ = ctx;
    }
    /// The status message as a libadwaita-style toast at the bottom of the window.
    pub(super) fn toast(&mut self, ctx: &egui::Context, theme: &Theme) {
        if self.message.is_empty() {
            self.message_shown = None;
            return;
        }
        let now = Instant::now();
        let shown = match &self.message_shown {
            Some((m, t)) if *m == self.message => *t,
            _ => {
                self.message_shown = Some((self.message.clone(), now));
                now
            }
        };
        let mut hovered = false;
        let mut dismiss = false;
        egui::Area::new(egui::Id::new("toast"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_BOTTOM, Vec2::new(0., -18.))
            .interactable(true)
            .show(ctx, |ui| {
                let fill = theme::mix(theme.ui("surface"), theme.ui("text"), 0.06);
                let r = egui::Frame::new()
                    .fill(fill)
                    .corner_radius(CornerRadius::same(20))
                    .inner_margin(Margin {
                        left: 18,
                        right: 6,
                        top: 4,
                        bottom: 4,
                    })
                    .shadow(ctx.style().visuals.popup_shadow)
                    .show(ui, |ui| {
                        ui.set_max_width(560.);
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&self.message).color(theme.ui("text")),
                                )
                                .wrap(),
                            );
                            let pal = Palette {
                                base: fill,
                                fg: theme.ui("text"),
                            };
                            dismiss = icons::flat(
                                ui,
                                Icon::Close,
                                Vec2::splat(30.),
                                14.,
                                pal,
                                "Dismiss",
                                false,
                            )
                            .clicked();
                        });
                    });
                hovered = r.response.contains_pointer();
            });
        if hovered {
            self.message_shown = Some((self.message.clone(), now));
        }
        let age = now.duration_since(shown);
        if dismiss || age > Duration::from_secs(6) {
            self.message.clear();
            self.message_shown = None;
        } else {
            ctx.request_repaint_after(Duration::from_secs(6) - age);
        }
    }
    pub(super) fn about_dialog(&mut self, ctx: &egui::Context, theme: &Theme) {
        let mut open = self.about;
        egui::Window::new("About Caelum")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size([300., 0.])
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(8.);
                    ui.label(
                        RichText::new("Caelum")
                            .heading()
                            .color(theme.ui("textStrong")),
                    );
                    ui.label(
                        RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                            .color(theme.ui("muted")),
                    );
                    ui.add_space(6.);
                    ui.label("A native workspace terminal.");
                    ui.hyperlink_to("Website", env!("CARGO_PKG_REPOSITORY"));
                    ui.add_space(6.);
                    if ui.button("Close").clicked() {
                        self.about = false;
                    }
                });
            });
        self.about &= open;
    }
    /// With client-side decorations the window has no frame to grab, so the outer few
    /// points resize it. Call before the panels so the headerbar can tell a resize
    /// from a move; returns nothing, but sets the cursor and starts the resize.
    pub(super) fn window_edges(&mut self, ctx: &egui::Context) {
        self.resize_edge = None;
        if !self.csd {
            return;
        }
        let maximized = ctx.input(|i| {
            i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false)
        });
        let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) else {
            return;
        };
        if maximized {
            return;
        }
        let screen = ctx.content_rect();
        let (edge, corner) = (5., 14.);
        let left = pos.x < screen.left() + edge;
        let right = pos.x > screen.right() - edge;
        let top = pos.y < screen.top() + edge;
        let bottom = pos.y > screen.bottom() - edge;
        let near_left = pos.x < screen.left() + corner;
        let near_right = pos.x > screen.right() - corner;
        let near_top = pos.y < screen.top() + corner;
        let near_bottom = pos.y > screen.bottom() - corner;
        let dir = if (top && near_left) || (left && near_top) {
            Some(ResizeDirection::NorthWest)
        } else if (top && near_right) || (right && near_top) {
            Some(ResizeDirection::NorthEast)
        } else if (bottom && near_left) || (left && near_bottom) {
            Some(ResizeDirection::SouthWest)
        } else if (bottom && near_right) || (right && near_bottom) {
            Some(ResizeDirection::SouthEast)
        } else if top {
            Some(ResizeDirection::North)
        } else if bottom {
            Some(ResizeDirection::South)
        } else if left {
            Some(ResizeDirection::West)
        } else if right {
            Some(ResizeDirection::East)
        } else {
            None
        };
        self.resize_edge = dir;
        if let Some(dir) = dir
            && ctx.input(|i| i.pointer.primary_pressed())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
        }
    }
    /// Sets the resize cursor over the window edge, after widgets set theirs; and
    /// outlines the undecorated window, which GNOME draws no border around.
    pub(super) fn window_frame(&self, ctx: &egui::Context, theme: &Theme) {
        if !self.csd {
            return;
        }
        if let Some(dir) = self.resize_edge {
            ctx.set_cursor_icon(match dir {
                ResizeDirection::North => egui::CursorIcon::ResizeNorth,
                ResizeDirection::South => egui::CursorIcon::ResizeSouth,
                ResizeDirection::East => egui::CursorIcon::ResizeEast,
                ResizeDirection::West => egui::CursorIcon::ResizeWest,
                ResizeDirection::NorthEast => egui::CursorIcon::ResizeNorthEast,
                ResizeDirection::NorthWest => egui::CursorIcon::ResizeNorthWest,
                ResizeDirection::SouthEast => egui::CursorIcon::ResizeSouthEast,
                ResizeDirection::SouthWest => egui::CursorIcon::ResizeSouthWest,
            });
        }
        let maximized = ctx.input(|i| {
            i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false)
        });
        if !maximized {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("window-outline"),
            ));
            painter.rect_stroke(
                ctx.content_rect(),
                0.,
                Stroke::new(1_f32, theme.ui("border")),
                egui::StrokeKind::Inside,
            );
        }
    }
}

fn fill_or(fill: Color32, base: Color32) -> Color32 {
    if fill == Color32::TRANSPARENT {
        base
    } else {
        fill
    }
}
