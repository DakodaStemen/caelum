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
/// Narrowest a tab gets before the tab bar scrolls.
const MIN_TAB: f32 = 140.;

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
                    Icon::Find,
                    size,
                    16.,
                    pal,
                    &self.tip("Find", "search"),
                    self.search_open,
                )
                .clicked()
                {
                    self.action(Action::Search, ctx);
                }
                let left_edge = left.min_rect().right();
                // Right cluster, laid out from the window edge inwards.
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
                let menu = icons::flat(
                    &mut right,
                    Icon::Menu,
                    size,
                    16.,
                    pal,
                    "Main Menu (F10)",
                    false,
                );
                // F10 opens the primary menu with its first row focused, as in GTK.
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F10)) {
                    egui::Popup::toggle_id(ctx, egui::Popup::default_response_id(&menu));
                    self.menu_by_keyboard = true;
                }
                self.main_menu(&menu, theme);
                if icons::flat(
                    &mut right,
                    Icon::NewTab,
                    size,
                    16.,
                    pal,
                    &self.tip("New Tab", "new_tab"),
                    false,
                )
                .clicked()
                {
                    self.action(Action::NewTab, ctx);
                }
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
                // Centered on the window, else pushed off whichever cluster it meets.
                let lo = left_edge + 12. + width / 2.;
                let cx = bar
                    .center()
                    .x
                    .max(lo)
                    .min((right_edge - 12. - width / 2.).max(lo));
                let top = bar.center().y - height / 2.;
                let painter = ui.painter();
                let ppp = painter.pixels_per_point();
                let pos = Pos2::new(cx - galley.size().x / 2., top).round_to_pixels(ppp);
                // Even the ellipsis does not fit: leave the title out rather than draw it
                // over the buttons.
                if width > room {
                    return;
                }
                super::adw::paint_bold(painter, pos, galley.clone(), pal.fg);
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
                .show(ctx, |ui| self.tab_bar(ui, theme));
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
    /// The primary menu, laid out like a GNOME app's: a zoom row, then sections.
    fn main_menu(&mut self, anchor: &egui::Response, theme: &Theme) {
        let mut chosen = None;
        egui::Popup::menu(anchor)
            .align(egui::RectAlign::BOTTOM_END)
            .gap(4.)
            .show(|ui| {
                // Scrolls when the window is too short for it (small window, big zoom).
                fit_height(ui, |ui| {
                    ui.set_width(260.);
                    menu_spacing(ui);
                    zoom_row(ui, theme);
                    ui.separator();
                    let mut rows = vec![];
                    let mut item = |ui: &mut egui::Ui, label: &str, key: &str, action: Action| {
                        let shortcut = self.keymap.get(key).map(binding_label);
                        let r = menu_item(ui, label, shortcut);
                        if r.clicked() {
                            chosen = Some(action);
                        }
                        rows.push(r);
                    };
                    item(ui, "New Tab", "new_tab", Action::NewTab);
                    item(ui, "New Workspace", "", Action::NewWorkspace);
                    item(ui, "Open File…", "open_file", Action::Open);
                    let mut sub_rows = vec![];
                    let new_pane = submenu(ui, "New Pane", |ui| {
                        ui.set_width(200.);
                        menu_spacing(ui);
                        for (label, action) in [
                            ("Terminal", Action::Split(Direction::Right)),
                            ("Files", Action::Files),
                            ("System Monitor", Action::System),
                            ("Processes", Action::Processes),
                            ("SSH Connection…", Action::Connect),
                            ("Codex", Action::Assistant("codex".into())),
                            ("Claude", Action::Assistant("claude".into())),
                        ] {
                            let r = menu_item(ui, label, None);
                            if r.clicked() {
                                chosen = Some(action);
                            }
                            sub_rows.push(r);
                        }
                        menu_keys(ui, &sub_rows);
                    });
                    rows.push(new_pane);
                    ui.separator();
                    for (label, key, action) in [
                        ("Find…", "search", Action::Search),
                        ("Balance Panes", "balance_panes", Action::Equalize),
                        ("Zoom Pane", "zoom_pane", Action::Zoom),
                        ("Command Palette", "command_palette", Action::Palette),
                    ] {
                        let shortcut = self.keymap.get(key).map(binding_label);
                        let r = menu_item(ui, label, shortcut);
                        if r.clicked() {
                            chosen = Some(action);
                        }
                        rows.push(r);
                    }
                    ui.separator();
                    for (label, action) in [
                        ("Preferences", Action::Settings),
                        ("About Caelum", Action::About),
                    ] {
                        let r = menu_item(ui, label, None);
                        if r.clicked() {
                            chosen = Some(action);
                        }
                        rows.push(r);
                    }
                    if std::mem::take(&mut self.menu_by_keyboard) {
                        rows[0].request_focus();
                    }
                    menu_keys(ui, &rows);
                });
            });
        if chosen.is_some() {
            // A row activated from the keyboard does not close the menu by itself.
            egui::Popup::close_id(&anchor.ctx, egui::Popup::default_response_id(anchor));
            self.pending_action = chosen;
        }
    }
    /// An `AdwTabBar`: equal-width tabs filling the bar, the selected one raised, thin
    /// separators between the others, a close button on the selected or hovered tab.
    fn tab_bar(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        let chrome = theme.ui("chrome");
        let text = theme.ui("text");
        let tabs: Vec<_> = self
            .workspace()
            .tabs
            .iter()
            .map(|t| (t.id.clone(), self.tab_label(t)))
            .collect();
        let active_id = self.workspace().active_tab.clone();
        let area = Rect::from_min_max(
            ui.max_rect().min,
            Pos2::new(ui.max_rect().right(), ui.max_rect().top() + 34.),
        );
        let gap = 4.;
        let n = tabs.len() as f32;
        // Tabs share the bar down to a minimum width; past that the bar scrolls
        // (wheel or the selected tab), like `AdwTabBar`.
        let width = ((area.width() - gap * (n - 1.)) / n).max(MIN_TAB);
        let total = n * width + gap * (n - 1.);
        let overflow = (total - area.width()).max(0.);
        if overflow > 0. && ui.rect_contains_pointer(area) {
            let delta = ui.input(|i| i.smooth_scroll_delta);
            self.tab_scroll -= if delta.x != 0. { delta.x } else { delta.y };
        }
        let key = (active_id.clone(), tabs.len(), area.width() as i32);
        if let Some(i) = tabs.iter().position(|(id, _)| *id == active_id)
            && self.tab_scroll_key != key
        {
            self.tab_scroll_key = key;
            let left = i as f32 * (width + gap);
            // Scroll just far enough to show the whole tab (its start, if it is wider
            // than the bar).
            self.tab_scroll = self.tab_scroll.max(left + width - area.width()).min(left);
        }
        self.tab_scroll = self.tab_scroll.clamp(0., overflow).round();
        let scroll = self.tab_scroll;
        let ppp = ui.pixels_per_point();
        let rects: Vec<Rect> = (0..tabs.len())
            .map(|i| {
                Rect::from_min_size(
                    Pos2::new(area.left() + i as f32 * (width + gap) - scroll, area.top()),
                    Vec2::new(width, area.height()),
                )
                .round_to_pixels(ppp)
            })
            .collect();
        ui.set_clip_rect(area.intersect(ui.clip_rect()));
        let hovered: Vec<bool> = rects
            .iter()
            .map(|r| ui.rect_contains_pointer(r.intersect(area)))
            .collect();
        let raised = |i: usize| tabs[i].0 == active_id || hovered[i];
        for (i, rect) in rects.iter().enumerate().skip(1) {
            if !raised(i - 1) && !raised(i) {
                let x = (rect.left() - gap / 2.).round_to_pixel_center(ppp);
                ui.painter().vline(
                    x,
                    egui::Rangef::new(area.top() + 4., area.bottom() - 4.),
                    Stroke::new(1. / ppp, theme::mix(chrome, text, 0.15)),
                );
            }
        }
        for (i, (id, name)) in tabs.into_iter().enumerate() {
            let rect = rects[i];
            if !rect.intersects(area) {
                continue;
            }
            let active = active_id == id;
            let response = ui.interact(
                rect.intersect(area),
                egui::Id::new(("tab", &id)),
                Sense::click(),
            );
            response
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &name));
            let fill = if active {
                theme::mix(chrome, text, 0.10)
            } else if hovered[i] {
                theme::mix(chrome, text, 0.06)
            } else {
                Color32::TRANSPARENT
            };
            ui.painter().rect_filled(rect, 8, fill);
            let close_room = 30.;
            let galley = truncated(
                ui,
                &name,
                egui::TextStyle::Body,
                text,
                rect.width() - 2. * close_room,
            );
            let pos = rect.center() - galley.size() / 2.;
            // A name cut short shows in full on hover.
            let response = if galley.elided {
                response.on_hover_text(&name)
            } else {
                response
            };
            ui.painter().galley(pos.round_to_pixels(ppp), galley, text);
            let mut close = response.middle_clicked();
            if active || hovered[i] {
                let button = Rect::from_center_size(
                    Pos2::new(rect.right() - 17., rect.center().y),
                    Vec2::splat(24.),
                );
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(button));
                let pal = Palette {
                    base: fill_or(fill, chrome),
                    fg: text,
                };
                close |= icons::flat(
                    &mut child,
                    Icon::Close,
                    Vec2::splat(24.),
                    16.,
                    pal,
                    "Close Tab",
                    false,
                )
                .clicked();
            }
            if close {
                self.workspace_mut().active_tab = id;
                self.request_close_tab();
                continue;
            }
            if response.clicked() {
                self.workspace_mut().active_tab = id.clone();
                self.zoom = None;
                self.ensure_focus();
            }
            if response.double_clicked() {
                self.action(Action::RenameTab, ui.ctx());
            }
            response.context_menu(|ui| {
                menu_spacing(ui);
                ui.set_width(180.);
                if menu_item(ui, "Rename…", None).clicked() {
                    self.workspace_mut().active_tab = id.clone();
                    self.action(Action::RenameTab, ui.ctx());
                }
                if menu_item(ui, "Close", None).clicked() {
                    self.workspace_mut().active_tab = id.clone();
                    self.request_close_tab();
                }
            });
        }
        // Fade the tabs cut off at a scrolled edge into the bar.
        let fade = 24.;
        if scroll > 0. {
            edge_fade(
                ui,
                Rect::from_min_size(area.min, Vec2::new(fade, area.height())),
                chrome,
                true,
            );
        }
        if scroll < overflow {
            edge_fade(
                ui,
                Rect::from_min_size(
                    Pos2::new(area.right() - fade, area.top()),
                    Vec2::new(fade, area.height()),
                ),
                chrome,
                false,
            );
        }
    }
    /// The find bar, a `GtkSearchBar` under the headerbar: an entry with a find icon,
    /// then previous and next. Enter finds the next match, Shift+Enter the previous,
    /// Escape closes it.
    pub(super) fn search_bar(&mut self, ctx: &egui::Context, theme: &Theme) {
        if !self.search_open {
            return;
        }
        let c = super::adw::Colors::new(theme);
        // Part of the content area, like Console's search bar over the terminal.
        let chrome = theme.term("background");
        egui::TopBottomPanel::top("searchbar")
            .exact_height(46.)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(chrome))
            .show(ctx, |ui| {
                let bar = ui.max_rect();
                let width = (bar.width() - 24. - 2. * 40.).clamp(120., 380.);
                let total = width + 2. * 40.;
                let row = Rect::from_center_size(bar.center(), Vec2::new(total, 34.));
                let mut row_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(row)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                let ui = &mut row_ui;
                ui.spacing_mut().item_spacing.x = 6.;
                let miss = self.search_miss && !self.search_query.is_empty();
                let edit = egui::TextEdit::singleline(&mut self.search_query)
                    .id(egui::Id::new("find-entry"))
                    .hint_text(RichText::new("Find text").color(c.muted))
                    .margin(egui::Margin {
                        left: 32,
                        right: 8,
                        top: 7,
                        bottom: 7,
                    })
                    .desired_width(width - 46.)
                    .text_color(if miss { c.danger } else { c.text });
                let response = ui.add(edit);
                let icon = Rect::from_center_size(
                    Pos2::new(response.rect.left() + 17., response.rect.center().y),
                    Vec2::splat(16.),
                );
                icons::paint(ui, icon, Icon::Find, 16., c.muted);
                if response.changed() {
                    self.search_miss = false;
                }
                let pal = Palette {
                    base: chrome,
                    fg: theme.ui("text"),
                };
                let mut find = None;
                if icons::flat(
                    ui,
                    Icon::Arrow(Direction::Up),
                    Vec2::splat(34.),
                    16.,
                    pal,
                    "Previous Match (Shift+Enter)",
                    false,
                )
                .clicked()
                {
                    find = Some(false);
                }
                if icons::flat(
                    ui,
                    Icon::Arrow(Direction::Down),
                    Vec2::splat(34.),
                    16.,
                    pal,
                    "Next Match (Enter)",
                    false,
                )
                .clicked()
                {
                    find = Some(true);
                }
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    find = Some(!ui.input(|i| i.modifiers.shift));
                    response.request_focus();
                }
                self.search_focus = response.has_focus();
                if let Some(forward) = find
                    && let Some(session) = self.terminals.get_mut(&self.focused)
                {
                    self.search_miss = !session.search(&self.search_query, forward);
                }
            });
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
    /// An `AdwAboutDialog`: the app icon, name, developer and version, then links.
    pub(super) fn about_dialog(&mut self, ctx: &egui::Context, theme: &Theme) {
        if !self.about {
            return;
        }
        let c = super::adw::Colors::new(theme);
        let close = super::adw::dialog(ctx, "about", "", Vec2::new(360., 470.), theme, |ui| {
            ui.vertical_centered(|ui| {
                ui.add(
                    egui::Image::from_bytes(
                        "bytes://caelum-icon.png",
                        include_bytes!("../../assets/brand/icon.png"),
                    )
                    .fit_to_exact_size(Vec2::splat(128.)),
                );
                ui.add_space(10.);
                let size = egui::TextStyle::Body.resolve(ui.style()).size * 1.6;
                super::adw::bold(ui, "Caelum", Some(size), c.text);
                ui.label(RichText::new("Dakoda Stemen").color(c.text));
                ui.add_space(6.);
                // The version, in an accent-tinted pill.
                let version = env!("CARGO_PKG_VERSION").to_string();
                let galley = ui.painter().layout_no_wrap(
                    version,
                    egui::TextStyle::Body.resolve(ui.style()),
                    c.accent,
                );
                let (rect, _) =
                    ui.allocate_exact_size(galley.size() + Vec2::new(24., 10.), Sense::hover());
                ui.painter()
                    .rect_filled(rect, 99, c.accent.gamma_multiply(0.18));
                ui.painter().galley(
                    (rect.center() - galley.size() / 2.).round_to_pixels(ui.pixels_per_point()),
                    galley,
                    c.accent,
                );
                ui.add_space(4.);
                ui.label(RichText::new("A native workspace terminal.").color(c.muted));
            });
            super::adw::group(ui, theme, "", None, |g| {
                if g.link_row("Website", None).clicked() {
                    g.ui.ctx()
                        .open_url(egui::OpenUrl::new_tab(env!("CARGO_PKG_REPOSITORY")));
                }
            });
        });
        if close {
            self.about = false;
        }
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

/// GTK popover menus: 32-point rows, 12 points of padding, no gap between rows.
pub(super) fn menu_spacing(ui: &mut egui::Ui) {
    ui.spacing_mut().item_spacing.y = 0.;
    ui.spacing_mut().button_padding = Vec2::new(12., 0.);
}

/// One row of a popover menu, with its shortcut dimmed on the right.
pub(super) fn menu_item(
    ui: &mut egui::Ui,
    label: &str,
    shortcut: Option<String>,
) -> egui::Response {
    let mut button = egui::Button::new(label)
        .frame_when_inactive(false)
        .min_size(Vec2::new(ui.available_width(), 32.));
    if let Some(s) = shortcut {
        button = button.shortcut_text(s);
    }
    ui.add(button)
}

/// Keyboard navigation inside a popover menu: Up/Down (and Tab/Shift+Tab) move
/// between `rows` and wrap around, instead of egui's spatial focus search
/// wandering out of the popover; Right opens a focused submenu row.
pub(super) fn menu_keys(ui: &egui::Ui, rows: &[egui::Response]) {
    let Some(i) = rows.iter().position(egui::Response::has_focus) else {
        return;
    };
    ui.memory_mut(|m| {
        m.set_focus_lock_filter(
            rows[i].id,
            egui::EventFilter {
                tab: true,
                vertical_arrows: true,
                horizontal_arrows: false,
                escape: false,
            },
        );
    });
    let (down, up) = ui.input_mut(|inp| {
        let tab = inp.consume_key(egui::Modifiers::NONE, egui::Key::Tab);
        let back = inp.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab);
        (
            inp.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) || tab,
            inp.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) || back,
        )
    });
    let n = rows.len();
    if down {
        rows[(i + 1) % n].request_focus();
    } else if up {
        rows[(i + n - 1) % n].request_focus();
    }
}

/// A menu row that opens a submenu, marked with GTK's `pan-end` arrow.
pub(super) fn submenu(
    ui: &mut egui::Ui,
    label: &str,
    content: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let color = ui.visuals().text_color();
    let button = egui::Button::new(label)
        .frame_when_inactive(false)
        .min_size(Vec2::new(ui.available_width(), 32.))
        .right_text(icons::image(Icon::PanEnd, 16., color));
    egui::containers::menu::SubMenuButton::from_button(button)
        .ui(ui, |ui| {
            fit_height(ui, content);
        })
        .0
}

/// Shows `content` as is, or in a scroll area once it was taller than the window
/// (so a popover in a short window can still reach every row, without a scroll
/// bar the rest of the time).
pub(super) fn fit_height<R>(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let id = ui.id().with("fit-height");
    let max = ui.ctx().content_rect().height() - 16.;
    let last: f32 = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(0.);
    if last > max {
        let out = egui::ScrollArea::vertical()
            .max_height(max)
            .show(ui, content);
        let height = out.content_size.y;
        ui.ctx().data_mut(|d| d.insert_temp(id, height));
        out.inner
    } else {
        let top = ui.cursor().top();
        let inner = content(ui);
        let height = ui.min_rect().bottom() - top;
        ui.ctx().data_mut(|d| d.insert_temp(id, height));
        inner
    }
}

/// The zoom row at the top of the main menu: out, the current level (resets), in.
fn zoom_row(ui: &mut egui::Ui, theme: &Theme) {
    let ctx = ui.ctx().clone();
    let pal = Palette {
        base: ui.visuals().window_fill,
        fg: theme.ui("text"),
    };
    let width = ui.available_width();
    ui.allocate_ui_with_layout(
        Vec2::new(width, 40.),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = 0.;
            let side = Vec2::new(width / 3., 34.);
            let zoom = ctx.zoom_factor();
            let (min, max) = (*super::ZOOM_RANGE.start(), *super::ZOOM_RANGE.end());
            let out = ui.add_enabled_ui(zoom > min + 0.01, |ui| {
                icons::flat(
                    ui,
                    Icon::ZoomOut,
                    side,
                    16.,
                    pal,
                    "Zoom Out (Ctrl+-)",
                    false,
                )
            });
            if out.inner.clicked() {
                super::step_zoom(&ctx, -1);
            }
            let level = format!("{:.0}%", ctx.zoom_factor() * 100.);
            let reset = ui
                .add_sized(
                    side,
                    egui::Button::new(RichText::new(level).strong()).frame_when_inactive(false),
                )
                .on_hover_text("Reset Zoom (Ctrl+0)");
            if reset.clicked() {
                ctx.set_zoom_factor(1.);
            }
            let zoom_in = ui.add_enabled_ui(zoom < max - 0.01, |ui| {
                icons::flat(ui, Icon::ZoomIn, side, 16., pal, "Zoom In (Ctrl+=)", false)
            });
            if zoom_in.inner.clicked() {
                super::step_zoom(&ctx, 1);
            }
        },
    );
}

/// A horizontal gradient over `rect` from `color` (at the left edge when
/// `from_left`) to transparent.
fn edge_fade(ui: &egui::Ui, rect: Rect, color: Color32, from_left: bool) {
    let (l, r) = if from_left {
        (color, Color32::TRANSPARENT)
    } else {
        (Color32::TRANSPARENT, color)
    };
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), l);
    mesh.colored_vertex(rect.right_top(), r);
    mesh.colored_vertex(rect.right_bottom(), r);
    mesh.colored_vertex(rect.left_bottom(), l);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(mesh);
}

fn fill_or(fill: Color32, base: Color32) -> Color32 {
    if fill == Color32::TRANSPARENT {
        base
    } else {
        fill
    }
}
