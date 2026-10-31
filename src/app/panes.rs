use super::icons::{self, Icon, Palette};
use super::*;
use egui::emath::GuiRounding;

/// Space between neighbouring panes: a one-point divider line.
pub(super) const GAP: f32 = 1.;
/// Height of the bar above each pane when a tab shows more than one.
const HEADER: f32 = 28.;

fn to_rect(r: model::PaneRect) -> Rect {
    Rect::from_min_size(Pos2::new(r.x, r.y), Vec2::new(r.w, r.h))
}
fn to_pane_rect(r: Rect) -> model::PaneRect {
    model::PaneRect::new(r.left(), r.top(), r.width(), r.height())
}

impl App {
    /// Lays out `node` in `rect`. `headers`: show the per-pane bar (more than one
    /// pane, or zoomed).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn layout(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        node: &mut Pane,
        ctx: &egui::Context,
        theme: &Theme,
        headers: bool,
    ) {
        if let Pane::Split {
            id,
            axis,
            ratio,
            first,
            second,
        } = node
        {
            let horizontal = *axis == model::Axis::Horizontal;
            let (a, b) = to_pane_rect(rect).split(*axis, *ratio, GAP);
            let (a, b) = (to_rect(a), to_rect(b));
            let line = if horizontal {
                Rect::from_min_max(
                    Pos2::new(a.right(), rect.top()),
                    Pos2::new(b.left(), rect.bottom()),
                )
            } else {
                Rect::from_min_max(
                    Pos2::new(rect.left(), a.bottom()),
                    Pos2::new(rect.right(), b.top()),
                )
            };
            // Paint children first so the divider and its wider grab area sit on top.
            self.layout(ui, a, first, ctx, theme, headers);
            self.layout(ui, b, second, ctx, theme, headers);
            ui.painter().rect_filled(line, 0., theme.ui("border"));
            let grab = line.expand2(if horizontal {
                Vec2::new(3., 0.)
            } else {
                Vec2::new(0., 3.)
            });
            let response = ui.interact(grab, egui::Id::new(id.clone()), Sense::drag());
            let length = if horizontal {
                rect.width()
            } else {
                rect.height()
            };
            if response.dragged() && length > 0. {
                let delta = response.drag_delta();
                let moved = *ratio + if horizontal { delta.x } else { delta.y } / length;
                // Never drag a side below the space its own panes need.
                let lo = (first.min_extent(*axis, model::MIN_PANE, GAP) + GAP / 2.) / length;
                let hi = 1. - (second.min_extent(*axis, model::MIN_PANE, GAP) + GAP / 2.) / length;
                *ratio = if lo < hi { moved.clamp(lo, hi) } else { *ratio }.clamp(0.1, 0.9);
            }
            if response.double_clicked() {
                node.equalize();
            }
            if response.hovered() || response.dragged() {
                ui.ctx().set_cursor_icon(if horizontal {
                    egui::CursorIcon::ResizeHorizontal
                } else {
                    egui::CursorIcon::ResizeVertical
                });
            }
            return;
        }
        let id = node.id().to_string();
        let selected = id == self.focused;
        let mut focus_requested = false;
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(&id)
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        child
            .painter()
            .rect_filled(rect, 0., theme.term("background"));
        let body = if headers {
            let bar = Rect::from_min_size(rect.min, Vec2::new(rect.width(), HEADER));
            if self.pane_header(&mut child, bar, node, &id, selected, theme) {
                focus_requested = true;
            }
            Rect::from_min_max(Pos2::new(rect.left(), bar.bottom()), rect.max)
        } else {
            rect
        };
        let mut content = child.new_child(
            egui::UiBuilder::new()
                .max_rect(body)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        content.set_clip_rect(body.intersect(child.clip_rect()));
        let ui = &mut content;
        // Non-terminal panes get the usual content margins.
        let padded = !matches!(node, Pane::Terminal { .. });
        if padded {
            let inner = body.shrink2(Vec2::new(12., 10.));
            *ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(inner)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
        }
        match node {
            Pane::Terminal { cwd, command, .. } => {
                focus_requested |= self.terminal_pane(ui, ctx, theme, &id, selected, cwd, command);
            }
            Pane::Files { path, .. } => self.files_pane(ui, theme, &id, path),
            Pane::Editor { path, .. } => {
                focus_requested |= self.editor_pane(ui, theme, &id, path);
            }
            Pane::System { .. } => self.system_pane(ui, theme),
            Pane::Processes { .. } => self.processes_pane(ui, &id, theme),
            Pane::Browser { url, .. } => Self::browser_pane(ui, ctx, url),
            _ => {}
        }
        if padded
            && ui.ctx().input(|i| i.pointer.primary_pressed())
            && ui.rect_contains_pointer(body)
        {
            focus_requested = true;
        }
        if focus_requested {
            self.focused = id;
        }
    }
    /// The slim bar above a pane: its title, then split, zoom and close buttons.
    pub(super) fn pane_header(
        &mut self,
        ui: &mut egui::Ui,
        bar: Rect,
        node: &Pane,
        id: &str,
        selected: bool,
        theme: &Theme,
    ) -> bool {
        let mut focus_requested = false;
        let base = theme::mix(theme.term("background"), theme.ui("text"), 0.045);
        ui.painter().rect_filled(bar, 0., base);
        let label = match node {
            Pane::Terminal { .. } => self
                .terminals
                .get(id)
                .map(|s| s.title.clone())
                .unwrap_or_else(|| "Terminal".into()),
            Pane::Files { path, .. } => path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string()),
            Pane::Editor { path, .. } => path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            Pane::System { .. } => "System".into(),
            Pane::Processes { .. } => "Processes".into(),
            Pane::Browser { .. } => "Link".into(),
            _ => String::new(),
        };
        let dirty = matches!(node, Pane::Editor { .. })
            && self.documents.get(id).is_some_and(Document::dirty);
        let fg = if selected {
            theme.ui("text")
        } else {
            theme.ui("muted")
        };
        let pal = Palette { base, fg };
        let mut bar_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(bar.shrink2(Vec2::new(4., 2.)))
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        let ui = &mut bar_ui;
        ui.spacing_mut().item_spacing.x = 2.;
        let button = Vec2::splat(24.);
        // Buttons show on the focused pane and the one under the pointer, keeping the
        // other headers to just their titles.
        let show_buttons = selected || ui.rect_contains_pointer(bar);
        if show_buttons
            && icons::flat(ui, Icon::Close, button, 16., pal, "Close Pane", false).clicked()
        {
            self.focused = id.to_string();
            self.pending = Some(PendingClose::Pane);
        }
        let zoomed = self.zoom.is_some();
        let roomy = show_buttons && bar.width() > 150.;
        if roomy {
            let (icon, tip) = if zoomed {
                (Icon::Restore, "Restore Pane")
            } else {
                (Icon::Maximize, "Zoom Pane")
            };
            if icons::flat(ui, icon, button, 16., pal, tip, zoomed).clicked() {
                self.focused = id.to_string();
                self.zoom = if zoomed { None } else { Some(id.to_string()) };
            }
            let split = icons::flat(ui, Icon::Split, button, 16., pal, "Split Pane", false);
            if split.clicked() {
                self.focused = id.to_string();
            }
            if let Some(action) = self.split_popover(&split, theme) {
                self.focused = id.to_string();
                self.pending_action = Some(action);
            }
        }
        let title_rect = Rect::from_min_max(
            Pos2::new(bar.left() + 12., bar.top()),
            Pos2::new(ui.cursor().right() - 4., bar.bottom()),
        );
        if title_rect.width() > 12. {
            let text = if dirty {
                format!("{label} \u{2022}")
            } else {
                label
            };
            let galley = truncated(ui, &text, egui::TextStyle::Small, fg, title_rect.width());
            let pos = Pos2::new(title_rect.left(), bar.center().y - galley.size().y / 2.);
            ui.painter()
                .with_clip_rect(title_rect)
                .galley(pos, galley, fg);
        }
        // Clicking anywhere on the bar (outside the buttons) focuses the pane.
        let hit = ui.interact(
            title_rect,
            egui::Id::new(("pane-title", id)),
            Sense::click(),
        );
        if hit.clicked() || hit.drag_started() {
            focus_requested = true;
        }
        if hit.double_clicked() {
            self.zoom = if zoomed { None } else { Some(id.to_string()) };
        }
        focus_requested
    }
    /// A popover with four arrows around a balance button, and a caption naming the
    /// hovered choice and its shortcut; returns the chosen action.
    pub(super) fn split_popover(&self, anchor: &egui::Response, theme: &Theme) -> Option<Action> {
        let mut chosen = None;
        egui::Popup::menu(anchor)
            .align(egui::RectAlign::BOTTOM_END)
            .gap(4.)
            .show(|ui| {
                let pal = Palette {
                    base: ui.visuals().window_fill,
                    fg: theme.ui("text"),
                };
                let cell = Vec2::splat(40.);
                let gap = 4.;
                let shortcut =
                    |name: &str| self.keymap.get(name).map(binding_label).unwrap_or_default();
                let choices = [
                    (Some(Icon::Arrow(Direction::Up)), "Split Up", "split_up"),
                    (None, "", ""),
                    (
                        Some(Icon::Arrow(Direction::Left)),
                        "Split Left",
                        "split_left",
                    ),
                    (Some(Icon::Balance), "Balance Panes", "balance_panes"),
                    (
                        Some(Icon::Arrow(Direction::Right)),
                        "Split Right",
                        "split_right",
                    ),
                    (None, "", ""),
                    (
                        Some(Icon::Arrow(Direction::Down)),
                        "Split Down",
                        "split_below",
                    ),
                ];
                let width = cell.x * 3. + gap * 2.;
                let mut caption = ("Split Pane".to_string(), String::new());
                ui.spacing_mut().item_spacing = Vec2::splat(gap);
                ui.vertical_centered(|ui| {
                    ui.set_width(width + 40.);
                    // Rows: [up], [left balance right], [down].
                    for row in [&choices[0..1], &choices[2..5], &choices[6..7]] {
                        let w = row.len() as f32 * cell.x + (row.len() as f32 - 1.) * gap;
                        ui.allocate_ui_with_layout(
                            Vec2::new(w, cell.y),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                for (icon, label, key) in row {
                                    let Some(icon) = icon else { continue };
                                    let r = icons::tile(ui, *icon, cell, pal, label);
                                    if r.hovered() {
                                        caption = (label.to_string(), shortcut(key));
                                    }
                                    if r.clicked() {
                                        chosen = Some(match icon {
                                            Icon::Arrow(dir) => Action::Split(*dir),
                                            _ => Action::Equalize,
                                        });
                                    }
                                }
                            },
                        );
                    }
                    ui.add_space(2.);
                    ui.label(RichText::new(&caption.0).color(theme.ui("text")));
                    let keys = if caption.1.is_empty() {
                        " ".to_string()
                    } else {
                        caption.1.clone()
                    };
                    ui.label(RichText::new(keys).small().color(theme.ui("muted")));
                });
            });
        chosen
    }
    pub(super) fn files_pane(
        &mut self,
        ui: &mut egui::Ui,
        theme: &Theme,
        id: &str,
        path: &mut PathBuf,
    ) {
        let view = self.files.entry(id.to_string()).or_default();
        if view.input_for.as_ref() != Some(&*path) {
            view.path_input = path.display().to_string();
            view.input_for = Some(path.clone());
        }
        // Shows the last listing until the background one for this path arrives.
        view.poll(path, ui.ctx());
        let base = theme.term("background");
        ui.horizontal(|ui| {
            let pal = Palette {
                base,
                fg: theme.ui("text"),
            };
            if icons::flat(
                ui,
                Icon::Arrow(Direction::Up),
                Vec2::splat(28.),
                16.,
                pal,
                "Parent folder",
                false,
            )
            .clicked()
                && let Some(parent) = path.parent()
            {
                *path = parent.to_path_buf();
            }
            let edit = ui.add(
                egui::TextEdit::singleline(&mut view.path_input)
                    .margin(Vec2::new(8., 5.))
                    .desired_width(ui.available_width()),
            );
            if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                let requested = PathBuf::from(&view.path_input);
                *path = if requested.is_absolute() {
                    requested
                } else {
                    path.join(requested)
                };
            }
        });
        ui.add(
            egui::TextEdit::singleline(&mut view.filter)
                .hint_text(RichText::new("Filter").color(ui.visuals().weak_text_color()))
                .margin(Vec2::new(8., 5.))
                .desired_width(ui.available_width()),
        );
        ui.add_space(2.);
        let shown = view
            .loaded
            .as_ref()
            .is_some_and(|(p, f)| p == path && *f == view.filter);
        if !view.error.is_empty() {
            // "Permission denied (os error 13)" -> "Permission denied".
            let reason = view.error.split(" (os error").next().unwrap_or_default();
            status_page(ui, theme, Icon::Folder, "Cannot Open Folder", reason);
            return;
        }
        if shown && view.entries.is_empty() {
            let (title, text) = if view.filter.is_empty() {
                ("Folder Is Empty", "")
            } else {
                ("No Results Found", "Try a different filter")
            };
            status_page(ui, theme, Icon::Folder, title, text);
            return;
        }
        let mut open = None;
        egui::ScrollArea::vertical()
            .scroll_source(
                egui::scroll_area::ScrollSource::MOUSE_WHEEL
                    | egui::scroll_area::ScrollSource::SCROLL_BAR,
            )
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.;
                for entry in &view.entries {
                    let response = file_row(ui, theme, base, &entry.name, entry.directory);
                    if response.double_clicked() {
                        if entry.directory {
                            *path = entry.path.clone();
                        } else {
                            open = Some(entry.path.clone());
                        }
                    }
                }
            });
        if let Some(path) = open {
            self.pending_open = Some(path);
        }
    }
    pub(super) fn system_pane(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        self.system.refresh(ui.ctx());
        let cpus = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        let rows = [
            ("Load Average", self.system.load.clone()),
            ("Memory", self.system.memory.clone()),
            ("Uptime", self.system.uptime.clone()),
            ("Processors", format!("{cpus}")),
            ("System", std::env::consts::OS.to_string()),
        ];
        egui::ScrollArea::vertical()
            .scroll_source(
                egui::scroll_area::ScrollSource::MOUSE_WHEEL
                    | egui::scroll_area::ScrollSource::SCROLL_BAR,
            )
            .show(ui, |ui| {
                ui.add_space(6.);
                for (name, value) in rows {
                    ui.label(RichText::new(name).small().color(theme.ui("muted")));
                    ui.label(RichText::new(value).size(20.).color(theme.ui("text")));
                    ui.add_space(14.);
                }
            });
    }
    pub(super) fn processes_pane(&mut self, ui: &mut egui::Ui, id: &str, theme: &Theme) {
        self.system.refresh(ui.ctx());
        ui.add(
            egui::TextEdit::singleline(&mut self.process_filter)
                .hint_text(RichText::new("Filter by name or PID").color(theme.ui("muted")))
                .margin(Vec2::new(8., 5.))
                .desired_width(ui.available_width()),
        );
        ui.add_space(4.);
        let filter = self.process_filter.to_lowercase();
        let muted = theme.ui("muted");
        let text = theme.ui("text");
        let base = theme.term("background");
        // A plain list: PID, name (cut to fit), resident memory on the right.
        let row = |ui: &mut egui::Ui, pid: &str, name: &str, rss: &str, color: Color32| {
            let (rect, response) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.), Sense::hover());
            if response.hovered() {
                ui.painter()
                    .rect_filled(rect, 6, theme::mix(base, text, 0.05));
            }
            let y = rect.center().y;
            // A narrow pane drops the PID column so names keep some room.
            let pid_w = if rect.width() < 240. { 0. } else { 72. };
            let rss_g = truncated(ui, rss, egui::TextStyle::Body, color, 90.);
            let name_w = (rect.width() - pid_w - rss_g.size().x - 24.).max(0.);
            let name_g = truncated(ui, name, egui::TextStyle::Body, color, name_w);
            let p = ui.painter();
            if pid_w > 0. {
                let pid_g = truncated(ui, pid, egui::TextStyle::Body, muted, 64.);
                p.galley(
                    Pos2::new(rect.left() + 6., y - pid_g.size().y / 2.),
                    pid_g,
                    muted,
                );
            }
            p.galley(
                Pos2::new(rect.left() + 6. + pid_w, y - name_g.size().y / 2.),
                name_g,
                color,
            );
            let rx = rect.right() - 6. - rss_g.size().x;
            p.galley(Pos2::new(rx, y - rss_g.size().y / 2.), rss_g, color);
        };
        row(ui, "PID", "Process", "Memory", muted);
        egui::ScrollArea::vertical()
            .id_salt(("processes", id))
            .scroll_source(
                egui::scroll_area::ScrollSource::MOUSE_WHEEL
                    | egui::scroll_area::ScrollSource::SCROLL_BAR,
            )
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.;
                for (pid, name, rss) in &self.system.processes {
                    if name.to_lowercase().contains(&filter) || pid.contains(&filter) {
                        row(ui, pid, name, rss, text);
                    }
                }
            });
    }
    pub(super) fn browser_pane(ui: &mut egui::Ui, ctx: &egui::Context, url: &mut String) {
        ui.label("Imported browser link");
        ui.text_edit_singleline(url);
        if ui.button("Open in system browser").clicked()
            && (url.starts_with("https://") || url.starts_with("http://"))
        {
            ctx.open_url(egui::OpenUrl::new_tab(url.clone()));
        }
        ui.label("Embedded browsing is pending. The link is preserved in this workspace.");
    }
}
/// One row of the file list: a folder or document icon and the name.
fn file_row(
    ui: &mut egui::Ui,
    theme: &Theme,
    base: Color32,
    name: &str,
    directory: bool,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
    let mut cut = false;
    if ui.is_rect_visible(rect) {
        if response.hovered() {
            ui.painter()
                .rect_filled(rect, 6, theme::mix(base, theme.ui("text"), 0.06));
        }
        let icon = Rect::from_center_size(
            Pos2::new(rect.left() + 16., rect.center().y),
            Vec2::splat(16.),
        );
        let muted = theme.ui("muted");
        icons::paint(
            ui,
            icon,
            if directory { Icon::Folder } else { Icon::File },
            16.,
            muted,
        );
        let galley = truncated(
            ui,
            name,
            egui::TextStyle::Body,
            theme.ui("text"),
            rect.width() - 40.,
        );
        cut = galley.elided;
        let pos = Pos2::new(rect.left() + 32., rect.center().y - galley.size().y / 2.);
        ui.painter().galley(pos, galley, theme.ui("text"));
    }
    // The whole name of one cut short.
    if cut {
        response.on_hover_text(name)
    } else {
        response
    }
}

/// An `AdwStatusPage` in miniature: a dimmed icon, a title and an optional line of
/// explanation, centered in the space left.
fn status_page(ui: &mut egui::Ui, theme: &Theme, icon: Icon, title: &str, text: &str) {
    let muted = theme.ui("muted");
    let area = ui.available_rect_before_wrap();
    let width = area.width();
    let title_g = truncated(ui, title, egui::TextStyle::Heading, theme.ui("text"), width);
    let text_g = truncated(ui, text, egui::TextStyle::Body, muted, width);
    let icon_size = 48.;
    let height = icon_size + 12. + title_g.size().y + 4. + text_g.size().y;
    // Centered, but never above the top of the space.
    let top = (area.center().y - height / 2.).max(area.top()).round();
    let x = area.center().x;
    icons::paint(
        ui,
        Rect::from_center_size(Pos2::new(x, top + icon_size / 2.), Vec2::splat(icon_size)),
        icon,
        icon_size,
        muted.gamma_multiply(0.6),
    );
    let ppp = ui.pixels_per_point();
    let mut y = top + icon_size + 12.;
    let title_pos = Pos2::new(x - title_g.size().x / 2., y).round_to_pixels(ppp);
    y += title_g.size().y + 4.;
    super::adw::paint_bold(ui.painter(), title_pos, title_g, theme.ui("text"));
    let text_pos = Pos2::new(x - text_g.size().x / 2., y).round_to_pixels(ppp);
    ui.painter().galley(text_pos, text_g, muted);
}
