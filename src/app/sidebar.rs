use super::*;

impl App {
    /// The workspace list, styled like a GNOME sidebar (hidden by default).
    pub(super) fn sidebar(&mut self, ctx: &egui::Context, theme: &Theme) {
        egui::SidePanel::left("workspaces")
            .default_width(220.)
            .width_range(180. ..=320.)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(theme.ui("panel"))
                    .inner_margin(egui::Margin::symmetric(6, 8)),
            )
            .show_animated(ctx, self.state.preferences.sidebar, |ui| {
                let panel = ui.max_rect();
                ui.spacing_mut().item_spacing.y = 2.;
                ui.add_space(2.);
                ui.horizontal(|ui| {
                    ui.add_space(12.);
                    ui.label(RichText::new("Workspaces").small().color(theme.ui("muted")));
                });
                ui.add_space(2.);
                let workspaces: Vec<_> = self
                    .state
                    .workspaces
                    .iter()
                    .map(|w| (w.id.clone(), w.name.clone()))
                    .collect();
                for (id, name) in workspaces {
                    let active = self.state.active_workspace == id;
                    let row = nav_row(ui, theme, &name, active, theme.ui("text"));
                    if row.clicked() {
                        self.state.active_workspace = id.clone();
                        self.zoom = None;
                        self.ensure_focus();
                    }
                    row.context_menu(|ui| {
                        if ui.button("Rename…").clicked() {
                            self.state.active_workspace = id.clone();
                            self.action(Action::RenameWorkspace, ctx);
                        }
                        if ui
                            .add_enabled(
                                self.state.workspaces.len() > 1,
                                egui::Button::new("Close"),
                            )
                            .clicked()
                        {
                            self.state.active_workspace = id.clone();
                            self.action(Action::CloseWorkspace, ctx);
                        }
                    });
                    if row.double_clicked() {
                        self.action(Action::RenameWorkspace, ctx);
                    }
                }
                let add = nav_row(ui, theme, "New Workspace", false, theme.ui("muted"));
                let icon = Rect::from_center_size(
                    Pos2::new(add.rect.right() - 18., add.rect.center().y),
                    Vec2::splat(16.),
                );
                super::icons::paint(ui, icon, super::icons::Icon::Add, 16., theme.ui("muted"));
                if add
                    .on_hover_text("Right-click a workspace to rename or close it")
                    .clicked()
                {
                    self.action(Action::NewWorkspace, ctx);
                }
                // The sidebar's edge, like a GtkPaned separator.
                ui.painter().vline(
                    panel.right() + 6.,
                    panel.y_range().expand(8.),
                    Stroke::new(1_f32, theme.ui("border")),
                );
            });
    }
}
/// A full-width sidebar row; the active one is tinted.
pub(super) fn nav_row(
    ui: &mut egui::Ui,
    theme: &Theme,
    label: &str,
    active: bool,
    color: Color32,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    if ui.is_rect_visible(rect) {
        let base = theme.ui("panel");
        let t = if active {
            0.10
        } else if response.hovered() {
            0.05
        } else {
            0.
        };
        if t > 0. {
            ui.painter()
                .rect_filled(rect, 6, theme::mix(base, theme.ui("text"), t));
        }
        let galley = truncated(ui, label, egui::TextStyle::Body, color, rect.width() - 24.);
        let pos = Pos2::new(rect.left() + 12., rect.center().y - galley.size().y / 2.);
        ui.painter().galley(pos, galley, color);
    }
    response
}
