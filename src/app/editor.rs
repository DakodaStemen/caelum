use super::*;

/// The id of an editor pane's text widget (to give it keyboard focus).
pub(super) fn text_id(pane: &str) -> egui::Id {
    egui::Id::new(("editor-text", pane))
}

/// Where the cursor goes for "open at line N, column M": the char index into `text`
/// and the 0-based line it is on. Lines and columns are 1-based and clamped to
/// the text, so a stale line number lands on the last line instead of nowhere.
pub(super) fn goto_offset(text: &str, line: usize, col: Option<usize>) -> (usize, usize) {
    let lines: Vec<&str> = text.split('\n').collect();
    let row = line.saturating_sub(1).min(lines.len() - 1);
    let start: usize = lines[..row].iter().map(|l| l.chars().count() + 1).sum();
    let width = lines[row].trim_end_matches('\r').chars().count();
    let col = col.map_or(0, |c| c.saturating_sub(1)).min(width);
    (start + col, row)
}

impl App {
    pub(super) fn editor_pane(
        &mut self,
        ui: &mut egui::Ui,
        theme: &Theme,
        id: &str,
        path: &Path,
    ) -> bool {
        let mut focus_requested = false;
        if !self.documents.contains_key(id) {
            // Files load on the document IO thread; show a placeholder until the
            // result arrives (or the error, which also reaches the status line).
            if let Some(error) = self.docio.open_error(id) {
                ui.colored_label(theme.ui("danger"), error);
                return false;
            }
            if !self.docio.loading(id) {
                self.docio.open(id, path, false);
            }
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Loading…").color(theme.ui("muted")));
            });
            return false;
        }
        let language = self
            .syntax
            .language(path)
            .unwrap_or_else(|| "Plain text".into());
        let Some(doc) = self.documents.get_mut(id) else {
            // Unreachable (checked above); render a placeholder rather than panic.
            ui.colored_label(
                theme.ui("danger"),
                "Document is not loaded; reopen the file",
            );
            return false;
        };
        // A slim status line, like GNOME Text Editor's: the language and length, and
        // Save only while there is something to save (Ctrl+S always works).
        ui.horizontal(|ui| {
            ui.set_min_height(28.);
            if self.docio.saving(id) {
                ui.spinner();
            }
            if doc.read_only {
                ui.colored_label(theme.ui("danger"), "Unknown encoding · read only");
            }
            let lines = doc.text.lines().count().max(1);
            ui.label(
                RichText::new(format!("{language} · {lines} lines"))
                    .small()
                    .color(theme.ui("muted")),
            )
            .on_hover_text(doc.path.display().to_string());
            if !doc.read_only && doc.dirty() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Save").on_hover_text("Ctrl+S").clicked() {
                        self.pending_save = Some(id.to_string());
                    }
                });
            }
        });
        let font = FontId::monospace(self.state.preferences.font_size);
        let doc_path = doc.path.clone();
        let goto = self
            .goto
            .remove(id)
            .map(|(line, col)| goto_offset(&doc.text, line, col));
        let tid = text_id(id);
        let syntax = &mut self.syntax;
        let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap: f32| {
            let job = syntax.job(&doc_path, text.as_str(), theme, font.clone(), wrap);
            ui.fonts_mut(|f| f.layout_job(job))
        };
        let row_height = ui.fonts_mut(|f| f.row_height(&font));
        let edit = egui::TextEdit::multiline(&mut doc.text)
            .id(tid)
            .font(egui::TextStyle::Monospace)
            .desired_width(f32::INFINITY)
            .interactive(!doc.read_only)
            .frame(false)
            .code_editor()
            .layouter(&mut layouter);
        egui::ScrollArea::both()
            .scroll_source(
                egui::scroll_area::ScrollSource::MOUSE_WHEEL
                    | egui::scroll_area::ScrollSource::SCROLL_BAR,
            )
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let size = ui.available_size().max(Vec2::new(1., 100.));
                if let Some((index, _)) = goto {
                    let ctx = ui.ctx().clone();
                    let mut state = egui::TextEdit::load_state(&ctx, tid).unwrap_or_default();
                    state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::one(
                            egui::text::CCursor::new(index),
                        )));
                    state.store(&ctx, tid);
                    ctx.memory_mut(|m| m.request_focus(tid));
                }
                let response = ui.add_sized(size, edit);
                if response.has_focus() {
                    focus_requested = true;
                }
                if let Some((_, row)) = goto {
                    let top = response.rect.top() + row_height * row as f32;
                    ui.scroll_to_rect(
                        Rect::from_min_size(
                            Pos2::new(response.rect.left(), top),
                            Vec2::new(1., row_height),
                        ),
                        Some(egui::Align::Center),
                    );
                }
            });
        focus_requested
    }
    /// Opens `path` in a new editor pane; the file loads in the background. If it
    /// cannot be opened the pane is removed again and the error is shown.
    pub(super) fn open_file(&mut self, path: PathBuf) -> String {
        let id = model::id();
        self.docio.open(&id, &path, true);
        self.insert(Pane::Editor {
            id: id.clone(),
            path,
        });
        id
    }
    /// Opens what a Ctrl+click on terminal output named: a directory in the file
    /// browser pane, a file in an editor pane scrolled to its line (reusing the
    /// editor already showing it in this tab).
    pub(super) fn open_ref(&mut self, target: terminal::FileRef) {
        if target.is_dir {
            let mut leaves = vec![];
            self.tab().layout.leaves(&mut leaves);
            let files = leaves.iter().find_map(|p| match p {
                Pane::Files { id, .. } => Some(id.clone()),
                _ => None,
            });
            if let Some(id) = files {
                update_node(
                    &mut self.tab_mut().layout,
                    &id.clone(),
                    Pane::Files {
                        id,
                        path: target.path,
                    },
                );
            }
            return;
        }
        let mut leaves = vec![];
        self.tab().layout.leaves(&mut leaves);
        let existing = leaves.iter().find_map(|p| match p {
            Pane::Editor { id, path } if *path == target.path => Some(id.clone()),
            _ => None,
        });
        let id = match existing {
            Some(id) => {
                self.zoom = None;
                self.focused = id.clone();
                self.ctx_focus = Some(text_id(&id));
                id
            }
            None => self.open_file(target.path),
        };
        // `open_file` leaves no pane behind when the split is refused.
        if let Some(line) = target.line
            && self.tab().layout.find(&id).is_some()
        {
            self.goto.insert(id, (line, target.col));
        }
    }
    /// Saves document `id` on the IO thread (coalescing with a save already running).
    pub(super) fn save_document(&mut self, id: &str) {
        let Some(doc) = self.documents.get(id) else {
            if self.docio.loading(id) {
                self.message = "File is still loading".into();
            }
            return;
        };
        self.message = match self.docio.save(id, doc) {
            files::SaveRequest::Started => "Saving…".into(),
            files::SaveRequest::Queued => "Saving… latest edits follow".into(),
        };
    }
    /// Applies finished document opens and saves.
    pub(super) fn poll_documents(&mut self) {
        let events = self.docio.poll(&mut self.documents);
        self.document_events(events);
    }
    /// Waits for every pending save to land (exit, window close, smoke checks).
    pub(super) fn flush_documents(&mut self) {
        let events = self.docio.flush(&mut self.documents);
        self.document_events(events);
    }
    fn document_events(&mut self, events: Vec<files::DocEvent>) {
        let mut remove = vec![];
        for event in events {
            match event {
                files::DocEvent::Opened { .. } => {}
                files::DocEvent::OpenFailed {
                    id,
                    error,
                    remove_pane,
                } => {
                    eprintln!("caelum-rs: {error}");
                    self.message = error;
                    if remove_pane {
                        remove.push(id);
                    }
                }
                files::DocEvent::Saved { .. } => self.message = "File saved".into(),
                files::DocEvent::SaveFailed { error, .. } => {
                    eprintln!("caelum-rs: {error}");
                    self.message = error;
                }
            }
        }
        if remove.is_empty() {
            return;
        }
        // A file that failed to open from "open file" gets no pane, as before.
        let cwd = self.cwd();
        for w in &mut self.state.workspaces {
            for t in &mut w.tabs {
                for id in &remove {
                    if t.layout.find(id).is_some() && !t.layout.remove(id) {
                        t.layout = Pane::terminal(cwd.clone());
                    }
                }
            }
        }
        if remove.iter().any(|id| self.zoom.as_ref() == Some(id)) {
            self.zoom = None;
        }
        self.ensure_focus();
        self.prune();
    }
}
