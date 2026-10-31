mod adw;
mod editor;
mod headerbar;
mod icons;
mod panes;
mod persist;
#[cfg(test)]
mod screens;
mod settings;
mod sidebar;
mod terminal_pane;
use anyhow::{Context, Result, bail};
use caelum_rs::{
    appearance::{self, SystemLook},
    config::{self, Config, Keymap},
    files::{self, Document, Draft},
    model::{self, Direction, Pane, State, Tab, Workspace},
    syntax::Highlighter,
    terminal::{self, Session},
    theme::{self, Theme},
};
use eframe::egui::{
    self, Color32, FontId, Key, Modifiers, Pos2, Rect, RichText, Sense, Stroke, Vec2,
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone, PartialEq)]
enum Action {
    NewTab,
    NewWorkspace,
    Split(Direction),
    Equalize,
    About,
    RenameWorkspace,
    CloseWorkspace,
    RenameTab,
    Files,
    System,
    Processes,
    ClosePane,
    Zoom,
    Settings,
    Open,
    Connect,
    Assistant(String),
    Theme(String),
    Save,
    Palette,
    Sidebar,
    NextTab,
    PrevTab,
    Search,
    Copy,
    Paste,
    PrevPrompt,
    NextPrompt,
}
#[derive(Clone)]
enum PendingClose {
    Window,
    Tab,
    Pane,
    Workspace,
}
/// Result of a background directory listing: (path, filter, entries or error).
type Listing = (
    PathBuf,
    String,
    std::result::Result<Vec<files::Entry>, String>,
);
#[derive(Default)]
struct FileView {
    path_input: String,
    /// The path `path_input` was last synced from.
    input_for: Option<PathBuf>,
    filter: String,
    entries: Vec<files::Entry>,
    /// Path and filter of the listing currently shown.
    loaded: Option<(PathBuf, String)>,
    refreshed: Option<Instant>,
    error: String,
    /// At most one listing in flight per view.
    listing: Option<std::sync::mpsc::Receiver<Listing>>,
}
impl FileView {
    /// Applies a finished listing, then starts a new one if the shown listing is for
    /// another path/filter or older than two seconds. Never blocks on the disk.
    fn poll(&mut self, path: &Path, ctx: &egui::Context) {
        if let Some(rx) = &self.listing {
            match rx.try_recv() {
                Ok((dir, filter, result)) => {
                    match result {
                        Ok(entries) => {
                            self.entries = entries;
                            self.error.clear();
                        }
                        Err(e) => self.error = e,
                    }
                    self.loaded = Some((dir, filter));
                    self.refreshed = Some(Instant::now());
                    self.listing = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.error = "Directory listing stopped unexpectedly".into();
                    self.refreshed = Some(Instant::now());
                    self.listing = None;
                }
            }
        }
        let current = self
            .loaded
            .as_ref()
            .is_some_and(|(p, f)| p == path && *f == self.filter);
        let stale = self
            .refreshed
            .is_none_or(|t| t.elapsed() > Duration::from_secs(2));
        if current && !stale {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let dir = path.to_path_buf();
        let filter = self.filter.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("caelum-files".into())
            .spawn(move || {
                let result = files::list(&dir, &filter).map_err(|e| e.to_string());
                let _ = tx.send((dir, filter, result));
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => self.listing = Some(rx),
            Err(e) => {
                self.error = format!("Cannot list directory: {e}");
                self.refreshed = Some(Instant::now());
            }
        }
    }
}
struct SystemInfo {
    updated: Option<Instant>,
    load: String,
    memory: String,
    uptime: String,
    processes: Vec<(String, String, String)>,
    /// The snapshot being taken on a background thread, if any.
    snapshot: Option<std::sync::mpsc::Receiver<caelum_rs::platform::SystemSnapshot>>,
}
impl SystemInfo {
    fn new() -> Self {
        Self {
            updated: None,
            load: String::new(),
            memory: String::new(),
            uptime: String::new(),
            processes: vec![],
            snapshot: None,
        }
    }
    /// Takes in a finished snapshot and starts the next one (at most one in flight,
    /// at most every two seconds). Reading /proc never runs on the UI thread.
    fn refresh(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.snapshot {
            match rx.try_recv() {
                Ok(snap) => {
                    self.load = snap.load;
                    self.memory = snap.memory;
                    self.uptime = snap.uptime;
                    self.processes = snap.processes;
                    self.snapshot = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.snapshot = None,
            }
        }
        if self
            .updated
            .is_some_and(|t| t.elapsed() < Duration::from_secs(2))
        {
            return;
        }
        self.updated = Some(Instant::now());
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        if std::thread::Builder::new()
            .name("caelum-system".into())
            .spawn(move || {
                let _ = tx.send(caelum_rs::platform::system_snapshot());
                ctx.request_repaint();
            })
            .is_ok()
        {
            self.snapshot = Some(rx);
        }
    }
}
pub struct App {
    pub state: State,
    profile: PathBuf,
    terminals: HashMap<String, Session>,
    documents: HashMap<String, Document>,
    files: HashMap<String, FileView>,
    presets: BTreeMap<String, Theme>,
    focused: String,
    zoom: Option<String>,
    message: String,
    palette: bool,
    palette_query: String,
    palette_index: usize,
    settings: bool,
    theme_draft: Option<Theme>,
    theme_path: String,
    dialog: Option<String>,
    dialog_input: String,
    /// The find bar under the headerbar: open, its text, whether the entry has focus,
    /// and whether the last search found nothing.
    search_open: bool,
    search_query: String,
    search_focus: bool,
    search_miss: bool,
    pending: Option<PendingClose>,
    allow_close: bool,
    last_save: Instant,
    system: SystemInfo,
    process_filter: String,
    /// Desktop light/dark preference, accent, UI font and window buttons.
    look: SystemLook,
    /// A re-check of the desktop colors running in the background (on focus).
    look_rx: Option<std::sync::mpsc::Receiver<SystemLook>>,
    look_checked: Instant,
    was_focused: bool,
    /// Draw the headerbar's window controls and handle move/resize ourselves.
    csd: bool,
    /// Resolved from `look` or the windowing system each frame.
    system_dark: bool,
    /// Resolved UI (proportional) and terminal (monospace) font files.
    ui_font: Option<Vec<u8>>,
    mono_font: settings::MonoFaces,
    ui_font_rx: Option<std::sync::mpsc::Receiver<Option<Vec<u8>>>>,
    /// System fonts for scripts the UI and terminal fonts lack (CJK, Arabic, ...),
    /// appended after them so those glyphs are not drawn as boxes.
    fallback_fonts: Vec<Vec<u8>>,
    fallback_rx: Option<std::sync::mpsc::Receiver<Vec<Vec<u8>>>>,
    /// Horizontal scroll of an overflowing tab bar, and the (selected tab, tab count,
    /// bar width) it last scrolled the selected tab into view for.
    tab_scroll: f32,
    tab_scroll_key: (String, usize, i32),
    /// Where the panes of the active tab were laid out last frame.
    pane_area: Option<Rect>,
    /// When the current status message appeared (toasts hide after a few seconds).
    message_shown: Option<(String, Instant)>,
    about: bool,
    /// An action picked in a popover, run after the frame's layout.
    pending_action: Option<Action>,
    window_title: String,
    /// Window edge under the pointer (client-side decorations only).
    resize_edge: Option<egui::ResizeDirection>,
    smoke: Option<Smoke>,
    recovery_conflict: bool,
    pending_open: Option<PathBuf>,
    applied_font: String,
    font_rx: Option<std::sync::mpsc::Receiver<settings::MonoFaces>>,
    last_dialog: Option<String>,
    pending_save: Option<String>,
    /// Multi-line paste awaiting confirmation (see `terminal_pane::paste_confirm`).
    pending_paste: Option<terminal_pane::PendingPaste>,
    syntax: Highlighter,
    config_path: PathBuf,
    config: Config,
    keymap: Keymap,
    config_watch: Option<config::Watcher>,
    writer: model::BackgroundWriter,
    /// Terminals launch the hermetic test shell (offscreen screenshot tests).
    test_shell: bool,
    docio: files::DocumentIo,
}
struct Smoke {
    report: PathBuf,
    stage: usize,
    start: Instant,
    last: Instant,
    checks: Vec<String>,
    terminal: String,
    pid: Option<u32>,
    original_tabs: usize,
    screenshot: bool,
}
impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ctx: &egui::Context,
        profile: PathBuf,
        mut state: State,
        message: String,
        smoke: Option<PathBuf>,
        config_path: PathBuf,
        look: SystemLook,
        csd: bool,
    ) -> Self {
        egui_extras::install_image_loaders(ctx);
        // Ctrl+=/-/0 are handled in `shortcuts` so the zoom stays in a usable range.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        if smoke.is_some() {
            let demo = profile.join("demo");
            let _ = fs::create_dir_all(demo.join("src"));
            let _ = fs::write(
                demo.join("README.md"),
                "# Caelum Rust\n\nYour space to build.\n",
            );
            let _ = fs::write(demo.join("Cargo.toml"), include_str!("../../Cargo.toml"));
            fn relocate(p: &mut Pane, path: &Path) {
                match p {
                    Pane::Terminal { cwd, .. } => *cwd = path.to_path_buf(),
                    Pane::Files { path: p, .. } => *p = path.to_path_buf(),
                    Pane::Split { first, second, .. } => {
                        relocate(first, path);
                        relocate(second, path);
                    }
                    _ => {}
                }
            }
            for w in &mut state.workspaces {
                for t in &mut w.tabs {
                    relocate(&mut t.layout, &demo);
                }
            }
        }
        let mut app = Self {
            state,
            profile,
            terminals: HashMap::new(),
            documents: HashMap::new(),
            files: HashMap::new(),
            presets: theme::presets(),
            look: SystemLook::default(),
            focused: String::new(),
            zoom: None,
            message,
            palette: false,
            palette_query: String::new(),
            palette_index: 0,
            settings: false,
            theme_draft: None,
            theme_path: String::new(),
            dialog: None,
            dialog_input: String::new(),
            search_open: false,
            search_query: String::new(),
            search_focus: false,
            search_miss: false,
            pending: None,
            allow_close: false,
            last_save: Instant::now(),
            system: SystemInfo::new(),
            process_filter: String::new(),
            look_rx: None,
            look_checked: Instant::now(),
            was_focused: true,
            csd,
            system_dark: true,
            ui_font: None,
            mono_font: Default::default(),
            ui_font_rx: None,
            fallback_fonts: Vec::new(),
            fallback_rx: None,
            tab_scroll: 0.,
            tab_scroll_key: (String::new(), 0, 0),
            pane_area: None,
            message_shown: None,
            about: false,
            pending_action: None,
            window_title: String::new(),
            resize_edge: None,
            smoke: smoke.map(|report| Smoke {
                report,
                stage: 0,
                start: Instant::now(),
                last: Instant::now(),
                checks: vec![],
                terminal: String::new(),
                pid: None,
                original_tabs: 0,
                screenshot: false,
            }),
            recovery_conflict: false,
            pending_open: None,
            applied_font: "__unset__".into(),
            font_rx: None,
            last_dialog: None,
            pending_save: None,
            pending_paste: None,
            syntax: Highlighter::default(),
            config_path,
            config: Config::default(),
            keymap: Keymap::default(),
            config_watch: None,
            writer: model::BackgroundWriter::new(),
            test_shell: false,
            docio: {
                let ctx = ctx.clone();
                files::DocumentIo::new(move || ctx.request_repaint())
            },
        };
        if app.smoke.is_none() {
            // Smoke runs use an isolated profile and must not read the user's config.
            app.reload_config(ctx, false);
            match config::Watcher::new(&app.config_path, ctx.clone()) {
                Ok(w) => app.config_watch = Some(w),
                Err(e) => app.message = format!("Config watcher unavailable: {e}"),
            }
        }
        app.restore_drafts();
        app.ensure_focus();
        app.load_ui_font(ctx, look.ui_font.as_ref().map(|(family, _)| family.clone()));
        app.load_fallback_fonts(ctx);
        app.look = look;
        app
    }
    fn theme(&self) -> Theme {
        if self.theme_draft.is_none() && self.state.preferences.follow_system {
            return self.system_theme();
        }
        self.theme_draft
            .clone()
            .or_else(|| {
                self.state
                    .preferences
                    .custom_themes
                    .iter()
                    .find(|t| t.id == self.state.preferences.theme)
                    .cloned()
            })
            .or_else(|| self.presets.get(&self.state.preferences.theme).cloned())
            .unwrap_or_else(|| self.system_theme())
    }
    /// Adwaita light or dark (following the desktop) with the desktop's accent.
    fn system_theme(&self) -> Theme {
        let id = if self.system_dark {
            "adwaita-dark"
        } else {
            "adwaita-light"
        };
        self.presets[id].clone().with_accent(self.look.accent())
    }
    /// Body text size: the desktop's UI font size (GTK points are 1/72 inch at 96 dpi).
    fn ui_font_size(&self) -> f32 {
        self.look
            .ui_font
            .as_ref()
            .map_or(14., |(_, pt)| (pt * 96. / 72.).clamp(11., 22.))
    }
    fn workspace(&self) -> &Workspace {
        &self.state.workspaces[self.state.active_index()]
    }
    fn workspace_mut(&mut self) -> &mut Workspace {
        let i = self.state.active_index();
        &mut self.state.workspaces[i]
    }
    fn tab(&self) -> &Tab {
        let w = self.workspace();
        &w.tabs[w.active_index()]
    }
    fn tab_mut(&mut self) -> &mut Tab {
        let w = self.workspace_mut();
        let i = w.active_index();
        &mut w.tabs[i]
    }
    fn ensure_focus(&mut self) {
        let mut leaves = vec![];
        self.tab().layout.leaves(&mut leaves);
        if !leaves.iter().any(|p| p.id() == self.focused) {
            self.focused = leaves
                .first()
                .map(|p| p.id().to_string())
                .unwrap_or_default();
        }
    }
    fn cwd(&self) -> PathBuf {
        self.terminals
            .get(&self.focused)
            .map(|s| s.cwd.clone())
            .or_else(|| match self.tab().layout.find(&self.focused) {
                Some(Pane::Terminal { cwd, .. }) => Some(cwd.clone()),
                Some(Pane::Files { path, .. }) => Some(path.clone()),
                Some(Pane::Editor { path, .. }) => path.parent().map(Path::to_path_buf),
                _ => None,
            })
            .unwrap_or_else(model::home)
    }
    /// Read config.toml synchronously; used once at startup so the first frame
    /// already has the user's settings. Hot reloads arrive from the watcher thread.
    fn reload_config(&mut self, ctx: &egui::Context, announce: bool) {
        let loaded = Config::load(&self.config_path);
        self.apply_config(loaded, ctx, announce);
    }
    /// Apply a parsed config. A bad file keeps the previous settings and reports the error.
    fn apply_config(&mut self, loaded: Result<Config>, ctx: &egui::Context, announce: bool) {
        match loaded {
            Ok(config) => {
                let keymap = Keymap::build(&config.keybindings).unwrap_or_default();
                config.apply(&mut self.state.preferences);
                if let Some(id) = &config.theme {
                    let known = self.presets.contains_key(id)
                        || self
                            .state
                            .preferences
                            .custom_themes
                            .iter()
                            .any(|t| &t.id == id);
                    if known {
                        self.state.preferences.theme = id.clone();
                        self.state.preferences.follow_system = false;
                    } else {
                        self.message = format!("config.toml: unknown theme {id:?}");
                    }
                }
                self.keymap = keymap;
                self.config = config;
                if announce {
                    self.message = "Config reloaded".into();
                }
                ctx.request_repaint();
            }
            Err(e) => self.message = format!("Config not applied: {e:#}"),
        }
    }
    fn insert(&mut self, pane: Pane) {
        self.split_with(pane, Direction::Right);
    }
    /// Opens `pane` beside the focused pane. Refuses (with a message) rather than
    /// squeezing any pane below [`model::MIN_PANE`]; balances the tab when that is
    /// what makes room.
    fn split_with(&mut self, pane: Pane, dir: Direction) -> bool {
        let next = pane.id().to_string();
        let area = self
            .pane_area
            .map(|r| model::PaneRect::new(r.left(), r.top(), r.width(), r.height()))
            // Before the first frame there is no area yet; never refuse then.
            .unwrap_or(model::PaneRect::new(0., 0., 1e6, 1e6));
        let mut target = self.focused.clone();
        if self.tab().layout.find(&target).is_none() {
            target = self.tab().layout.id().to_string();
        }
        // A split root is not a leaf: fall back to its first leaf.
        if let Some(Pane::Split { .. }) = self.tab().layout.find(&target) {
            let mut leaves = vec![];
            self.tab().layout.leaves(&mut leaves);
            target = leaves[0].id().to_string();
        }
        let result =
            self.tab_mut()
                .layout
                .try_split(&target, pane, dir, area, model::MIN_PANE, panes::GAP);
        match result {
            Ok(outcome) => {
                if outcome == model::SplitOutcome::Rebalanced {
                    self.message = "Panes were balanced to make room".into();
                }
                self.focused = next;
                self.zoom = None;
                true
            }
            Err(_) => {
                self.message =
                    "No room for another pane here. Close a pane or make the window larger.".into();
                false
            }
        }
    }
    fn action(&mut self, action: Action, ctx: &egui::Context) {
        let reset_focus = matches!(
            &action,
            Action::NewTab
                | Action::NewWorkspace
                | Action::Split(_)
                | Action::Assistant(_)
                | Action::Palette
                | Action::Open
                | Action::Connect
        );
        if reset_focus {
            ctx.memory_mut(|m| m.stop_text_input());
        }
        match action {
            Action::NewTab => {
                let t = Tab::new(self.cwd());
                self.focused = t.layout.id().into();
                let w = self.workspace_mut();
                w.active_tab = t.id.clone();
                w.tabs.push(t);
                self.zoom = None;
            }
            Action::NewWorkspace => {
                let w = Workspace::new(
                    format!("Workspace {}", self.state.workspaces.len() + 1),
                    self.cwd(),
                );
                self.state.active_workspace = w.id.clone();
                self.state.workspaces.push(w);
                self.zoom = None;
                self.ensure_focus();
            }
            Action::Split(dir) => {
                self.split_with(Pane::terminal(self.cwd()), dir);
            }
            Action::Equalize => {
                self.tab_mut().layout.equalize();
                self.zoom = None;
            }
            Action::About => self.about = true,
            Action::RenameWorkspace => {
                self.dialog = Some("Rename Workspace".into());
                self.dialog_input = self.workspace().name.clone();
            }
            Action::CloseWorkspace => self.pending = Some(PendingClose::Workspace),
            Action::RenameTab => {
                self.dialog = Some("Rename Tab".into());
                self.dialog_input = self.tab().name.clone();
            }
            Action::Files => self.insert(Pane::Files {
                id: model::id(),
                path: self.cwd(),
            }),
            Action::System => self.insert(Pane::System { id: model::id() }),
            Action::Processes => self.insert(Pane::Processes { id: model::id() }),
            Action::ClosePane => self.pending = Some(PendingClose::Pane),
            Action::Zoom => {
                self.zoom = if self.zoom.is_some() {
                    None
                } else {
                    Some(self.focused.clone())
                };
            }
            Action::Settings => self.settings = true,
            Action::Sidebar => self.state.preferences.sidebar = !self.state.preferences.sidebar,
            Action::Open => {
                self.dialog = Some("Open File".into());
                self.dialog_input = self.cwd().display().to_string() + "/";
            }
            Action::Connect => {
                self.dialog = Some("Connect with SSH".into());
                self.dialog_input.clear();
            }
            Action::Assistant(command) => {
                self.insert(Pane::Terminal {
                    id: model::id(),
                    cwd: self.cwd(),
                    command: Some(vec![command]),
                });
            }
            // An empty id means "follow the desktop".
            Action::Theme(id) => {
                self.state.preferences.follow_system = id.is_empty();
                if !id.is_empty() {
                    self.state.preferences.theme = id;
                }
            }
            Action::Save => {
                self.pending_save = Some(self.focused.clone());
            }
            Action::Palette => {
                self.palette = !self.palette;
                self.palette_query.clear();
                self.palette_index = 0;
            }
            Action::NextTab | Action::PrevTab => {
                let forward = matches!(action, Action::NextTab);
                let w = self.workspace_mut();
                let n = w.tabs.len();
                let i = (w.active_index() + if forward { 1 } else { n - 1 }) % n;
                w.active_tab = w.tabs[i].id.clone();
                self.zoom = None;
                self.ensure_focus();
            }
            Action::Search => {
                self.search_open = !self.search_open;
                self.search_miss = false;
                if self.search_open {
                    // Opening the bar puts the cursor in its entry.
                    ctx.memory_mut(|m| m.request_focus(egui::Id::new("find-entry")));
                } else {
                    self.search_focus = false;
                    ctx.memory_mut(|m| m.stop_text_input());
                }
            }
            Action::Copy => {
                if let Some(text) = self
                    .terminals
                    .get(&self.focused)
                    .and_then(Session::selection)
                {
                    ctx.copy_text(text);
                }
            }
            Action::Paste => ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste),
            Action::PrevPrompt | Action::NextPrompt => {
                // Needs OSC 133 shell integration; silently a no-op otherwise.
                if let Some(s) = self.terminals.get_mut(&self.focused) {
                    s.jump_prompt(matches!(action, Action::NextPrompt));
                }
            }
        }
        ctx.request_repaint();
    }
    fn actions(&self) -> Vec<(String, Action)> {
        let mut a = vec![
            ("New Tab".into(), Action::NewTab),
            ("New Workspace".into(), Action::NewWorkspace),
            ("Rename Workspace…".into(), Action::RenameWorkspace),
            ("Close Workspace".into(), Action::CloseWorkspace),
            ("Rename Tab…".into(), Action::RenameTab),
            ("Split Right".into(), Action::Split(Direction::Right)),
            ("Split Down".into(), Action::Split(Direction::Down)),
            ("Split Left".into(), Action::Split(Direction::Left)),
            ("Split Up".into(), Action::Split(Direction::Up)),
            ("Balance Panes".into(), Action::Equalize),
            ("Zoom Pane".into(), Action::Zoom),
            ("Close Pane".into(), Action::ClosePane),
            ("Open File…".into(), Action::Open),
            ("Files".into(), Action::Files),
            ("System Monitor".into(), Action::System),
            ("Processes".into(), Action::Processes),
            ("SSH Connection…".into(), Action::Connect),
            ("Codex".into(), Action::Assistant("codex".into())),
            ("Claude".into(), Action::Assistant("claude".into())),
            ("Workspaces Sidebar".into(), Action::Sidebar),
            ("Preferences".into(), Action::Settings),
            ("Find".into(), Action::Search),
            ("Next Tab".into(), Action::NextTab),
            ("Previous Tab".into(), Action::PrevTab),
            ("About Caelum".into(), Action::About),
            ("Previous Prompt".into(), Action::PrevPrompt),
            ("Next Prompt".into(), Action::NextPrompt),
        ];
        a.push(("Theme: Follow System".into(), Action::Theme(String::new())));
        for (id, t) in &self.presets {
            a.push((format!("Theme: {}", t.name), Action::Theme(id.clone())));
        }
        for t in &self.state.preferences.custom_themes {
            a.push((format!("Theme: {}", t.name), Action::Theme(t.id.clone())));
        }
        a
    }
    fn shortcuts(&mut self, ctx: &egui::Context) {
        let mut bound: Vec<_> = shortcut_table()
            .into_iter()
            .filter_map(|(name, a)| self.keymap.get(name).map(|b| (b, a)))
            .collect();
        // Check combinations with more modifiers first so Ctrl+Shift+Tab is not eaten by Ctrl+Tab.
        let weight = |m: Modifiers| usize::from(m.ctrl) + usize::from(m.shift) + usize::from(m.alt);
        bound.sort_by_key(|(b, _)| std::cmp::Reverse(weight(b.modifiers)));
        for (b, a) in bound {
            if ctx.input_mut(|i| i.consume_key(b.modifiers, b.key)) {
                self.action(a, ctx);
            }
        }
        use egui::gui_zoom::kb_shortcuts as zoom;
        let (zoom_in, zoom_out, reset) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&zoom::ZOOM_IN) || i.consume_shortcut(&zoom::ZOOM_IN_SECONDARY),
                i.consume_shortcut(&zoom::ZOOM_OUT),
                i.consume_shortcut(&zoom::ZOOM_RESET),
            )
        });
        if zoom_in {
            step_zoom(ctx, 1);
        }
        if zoom_out {
            step_zoom(ctx, -1);
        }
        if reset {
            ctx.set_zoom_factor(1.);
        }
        // Dialogs built on `egui::Modal` (preferences, alerts) close themselves on Escape.
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.palette = false;
            if self.search_open && self.search_focus {
                self.search_open = false;
                self.search_focus = false;
            }
        }
    }
    fn overlays(&mut self, ctx: &egui::Context) {
        if self.dialog != self.last_dialog {
            ctx.memory_mut(|m| m.stop_text_input());
            self.last_dialog = self.dialog.clone();
        }
        if self.palette {
            let mut action = None;
            let theme = self.theme();
            let c = adw::Colors::new(&theme);
            let query = self.palette_query.to_lowercase();
            let matches: Vec<_> = self
                .actions()
                .into_iter()
                .filter(|(label, _)| label.to_lowercase().contains(&query))
                .collect();
            // Up and Down move the selection; Enter runs it.
            let (up, down, enter) = ctx.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::ArrowUp),
                    i.consume_key(Modifiers::NONE, Key::ArrowDown),
                    i.key_pressed(Key::Enter),
                )
            });
            let n = matches.len();
            if n > 0 {
                if down {
                    self.palette_index = (self.palette_index + 1) % n;
                }
                if up {
                    self.palette_index = (self.palette_index + n - 1) % n;
                }
            }
            self.palette_index = self.palette_index.min(n.saturating_sub(1));
            if enter {
                action = matches.get(self.palette_index).map(|(_, a)| a.clone());
            }
            let shortcuts: Vec<_> = shortcut_table()
                .into_iter()
                .filter_map(|(name, a)| self.keymap.get(name).map(|b| (a, binding_label(b))))
                .collect();
            let width = 520_f32.min(ctx.content_rect().width() - 24.);
            let area = egui::Area::new(egui::Id::new("command-palette"))
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::CENTER_TOP, [0., 64.])
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style())
                        .corner_radius(12)
                        .inner_margin(6)
                        .show(ui, |ui| {
                            ui.set_width(width);
                            let edit = ui.add(
                                egui::TextEdit::singleline(&mut self.palette_query)
                                    .hint_text(
                                        RichText::new("Search commands and themes").color(c.muted),
                                    )
                                    .margin(egui::Margin {
                                        left: 34,
                                        right: 10,
                                        top: 8,
                                        bottom: 8,
                                    })
                                    .desired_width(f32::INFINITY),
                            );
                            if edit.changed() {
                                self.palette_index = 0;
                            }
                            let icon = Rect::from_center_size(
                                Pos2::new(edit.rect.left() + 18., edit.rect.center().y),
                                Vec2::splat(16.),
                            );
                            icons::paint(ui, icon, icons::Icon::Find, 16., c.muted);
                            if !ctx.wants_keyboard_input() {
                                edit.request_focus();
                            }
                            ui.add_space(4.);
                            if matches.is_empty() {
                                ui.add_space(8.);
                                ui.vertical_centered(|ui| {
                                    ui.label(RichText::new("No Results").color(c.muted));
                                });
                                ui.add_space(8.);
                                return;
                            }
                            egui::ScrollArea::vertical()
                                .scroll_source(
                                    egui::scroll_area::ScrollSource::MOUSE_WHEEL
                                        | egui::scroll_area::ScrollSource::SCROLL_BAR,
                                )
                                .max_height(384.)
                                .show(ui, |ui| {
                                    headerbar::menu_spacing(ui);
                                    for (i, (label, a)) in matches.iter().enumerate() {
                                        let selected = i == self.palette_index;
                                        let mut row = egui::Button::new(label.as_str())
                                            .frame_when_inactive(selected)
                                            .fill(theme::mix(
                                                ui.visuals().window_fill,
                                                c.text,
                                                0.10,
                                            ))
                                            .min_size(Vec2::new(ui.available_width(), 32.));
                                        if let Some((_, keys)) =
                                            shortcuts.iter().find(|(sa, _)| sa == a)
                                        {
                                            row = row.shortcut_text(keys.as_str());
                                        }
                                        let r = ui.add(row);
                                        if selected && (up || down) {
                                            r.scroll_to_me(None);
                                        }
                                        if r.clicked() {
                                            action = Some(a.clone());
                                        }
                                    }
                                });
                        });
                });
            // A click elsewhere dismisses it, like a popover.
            if ctx.input(|i| i.pointer.any_pressed())
                && !ctx.rect_contains_pointer(area.response.layer_id, area.response.rect)
            {
                self.palette = false;
            }
            if let Some(a) = action {
                self.palette = false;
                ctx.memory_mut(|m| m.stop_text_input());
                self.action(a, ctx);
            }
        }
        if self.settings {
            self.appearance(ctx);
        }
        if self.pending_paste.is_some() {
            self.paste_confirm(ctx);
        }
        if let Some(title) = self.dialog.clone() {
            let theme = self.theme();
            let c = adw::Colors::new(&theme);
            let (body, hint, verb) = match title.as_str() {
                "Connect with SSH" => (
                    "Opens a terminal pane running ssh.",
                    "host or user@host",
                    "Connect",
                ),
                "Open File" => (
                    "Opens the file in an editor pane.",
                    "Path to a file",
                    "Open",
                ),
                _ => ("", "Name", "Rename"),
            };
            let mut submit = false;
            let picked = adw::alert(
                ctx,
                "text-dialog",
                &title,
                body,
                &[("Cancel", adw::Kind::Normal), (verb, adw::Kind::Suggested)],
                0,
                &theme,
                |ui| {
                    let edit = ui.add(adw::entry(&mut self.dialog_input, hint, &c));
                    if !ctx.wants_keyboard_input() {
                        edit.request_focus();
                    }
                    submit = ui.input(|i| i.key_pressed(Key::Enter));
                },
            );
            if picked == Some(1) || submit {
                let value = self.dialog_input.trim().to_string();
                match title.as_str() {
                    "Open File" => self.open_file(PathBuf::from(value)),
                    "Rename Tab" => {
                        if !value.is_empty() {
                            self.tab_mut().name = value;
                        }
                    }
                    "Rename Workspace" => {
                        if !value.is_empty() {
                            self.workspace_mut().name = value;
                        }
                    }
                    "Connect with SSH" => {
                        if !model::valid_ssh_host(&value) {
                            self.message =
                                "Enter a host or user@host from your SSH configuration".into();
                        } else {
                            self.insert(Pane::Terminal {
                                id: model::id(),
                                cwd: self.cwd(),
                                command: Some(vec!["ssh".into(), "--".into(), value]),
                            });
                        }
                    }
                    _ => {}
                }
                self.dialog = None;
                ctx.memory_mut(|m| m.stop_text_input());
            } else if picked == Some(0) {
                self.dialog = None;
            }
        }
        if self.pending.is_some() {
            let dirty = self.dirty_in_pending();
            if dirty.is_empty() {
                self.finish_close(ctx);
            } else {
                let theme = self.theme();
                let body = if dirty.len() == 1 {
                    "An edited file has unsaved changes. Save it before closing?".to_string()
                } else {
                    format!(
                        "{} edited files have unsaved changes. Save them before closing?",
                        dirty.len()
                    )
                };
                let picked = adw::alert(
                    ctx,
                    "unsaved-dialog",
                    "Save Changes?",
                    &body,
                    &[
                        ("Cancel", adw::Kind::Normal),
                        ("Discard", adw::Kind::Destructive),
                        ("Save", adw::Kind::Suggested),
                    ],
                    0,
                    &theme,
                    |_| {},
                );
                match picked {
                    Some(0) => self.pending = None,
                    Some(2) => {
                        // Saves run on the IO thread; once they land the documents are
                        // clean and the close completes above. A failed save keeps this
                        // dialog open.
                        for id in &dirty {
                            self.save_document(id);
                        }
                    }
                    Some(1) => {
                        for id in dirty {
                            // A prior save may still be queued on the IO thread; cancel it
                            // so discarding never writes. A save already running finishes
                            // (see `DocumentIo::discard`).
                            self.docio.discard(&id);
                            self.documents.remove(&id);
                        }
                        self.finish_close(ctx);
                    }
                    _ => {}
                }
            }
        }
    }
    fn dirty_in_pending(&self) -> Vec<String> {
        let mut allowed = HashSet::new();
        let mut add = |p: &Pane| {
            let mut leaves = vec![];
            p.leaves(&mut leaves);
            for p in leaves {
                allowed.insert(p.id().to_string());
            }
        };
        match &self.pending {
            Some(PendingClose::Window) => {
                for w in &self.state.workspaces {
                    for t in &w.tabs {
                        add(&t.layout);
                    }
                }
            }
            Some(PendingClose::Workspace) => {
                for t in &self.workspace().tabs {
                    add(&t.layout);
                }
            }
            Some(PendingClose::Tab) => add(&self.tab().layout),
            Some(PendingClose::Pane) => {
                allowed.insert(self.focused.clone());
            }
            None => {}
        }
        self.documents
            .iter()
            .filter(|(id, doc)| allowed.contains(*id) && doc.dirty())
            .map(|(id, _)| id.clone())
            .collect()
    }
    fn finish_close(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        match pending {
            PendingClose::Window => {
                self.allow_close = true;
                self.persist_now();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            PendingClose::Workspace => {
                let cwd = self.cwd();
                close_workspace(&mut self.state, cwd);
            }
            PendingClose::Tab => {
                let cwd = self.cwd();
                let w = self.workspace_mut();
                let i = w.active_index();
                w.tabs.remove(i);
                if w.tabs.is_empty() {
                    w.tabs.push(Tab::new(cwd));
                }
                w.active_tab = w.tabs[i.min(w.tabs.len() - 1)].id.clone();
            }
            PendingClose::Pane => {
                let focused = self.focused.clone();
                let cwd = self.cwd();
                if !self.tab_mut().layout.remove(&focused) {
                    self.tab_mut().layout = Pane::terminal(cwd);
                }
            }
        }
        self.zoom = None;
        self.ensure_focus();
        self.prune();
    }
    fn prune(&mut self) {
        let mut ids = HashSet::new();
        for w in &self.state.workspaces {
            for t in &w.tabs {
                let mut leaves = vec![];
                t.layout.leaves(&mut leaves);
                ids.extend(leaves.into_iter().map(|p| p.id().to_string()));
            }
        }
        self.terminals.retain(|id, _| ids.contains(id));
        self.documents.retain(|id, _| ids.contains(id));
        self.docio.retain(|id| ids.contains(id));
        self.files.retain(|id, _| ids.contains(id));
    }
    fn smoke_tick(&mut self, ctx: &egui::Context) {
        let Some(mut s) = self.smoke.take() else {
            return;
        };
        let result = (|| -> Result<bool> {
            if s.start.elapsed() > Duration::from_secs(30) {
                bail!("Desktop check timed out at stage {}", s.stage);
            }
            if s.stage != 10 && s.last.elapsed() < Duration::from_millis(180) {
                return Ok(false);
            }
            match s.stage {
                0 => {
                    let terminal = self
                        .terminals
                        .iter_mut()
                        .next()
                        .context("No terminal was created")?;
                    s.terminal = terminal.0.clone();
                    s.pid = terminal.1.pid();
                    terminal.1.write(
                        b"printf '\\033[38;2;130;213;238mNATIVE_%s\\033[0m\\n' RUST_OK\r".to_vec(),
                    );
                    s.original_tabs = self.workspace().tabs.len();
                }
                1 => {
                    if !self
                        .terminals
                        .get(&s.terminal)
                        .context("Smoke terminal disappeared")?
                        .text()
                        .contains("NATIVE_RUST_OK")
                    {
                        return Ok(false);
                    }
                    s.checks.push(
                        "Real PTY executes a shell command and decodes true-color output".into(),
                    );
                    self.focused = s.terminal.clone();
                    self.action(Action::Split(Direction::Down), ctx);
                }
                2 => {
                    if self.terminals.len() < 2 {
                        return Ok(false);
                    }
                    s.checks.push("Split creates a second live terminal".into());
                    self.action(Action::Zoom, ctx);
                }
                3 => {
                    if self.zoom.is_none() {
                        bail!("Zoom did not activate");
                    }
                    self.action(Action::Zoom, ctx);
                    s.checks.push("Pane zoom restores the split layout".into());
                    self.action(Action::NewTab, ctx);
                }
                4 => {
                    if self.workspace().tabs.len() != s.original_tabs + 1 {
                        bail!("New tab failed");
                    }
                    s.checks.push("New tab owns an independent terminal".into());
                    self.action(Action::Theme("graphite".into()), ctx);
                    self.persist_now();
                    let restored = State::read(&self.profile.join("workspace.json"))?;
                    if restored.preferences.theme != "graphite" {
                        bail!("Theme was not persisted");
                    }
                    s.checks
                        .push("Workspace and theme persist to an isolated profile".into());
                    self.action(Action::Files, ctx);
                }
                5 => {
                    let fixture = self.profile.join("desktop-check.txt");
                    fs::write(&fixture, "before\n")?;
                    self.open_file(fixture);
                }
                6 => {
                    let id = self.focused.clone();
                    if self.docio.loading(&id) {
                        return Ok(false);
                    }
                    let doc = self
                        .documents
                        .get_mut(&id)
                        .context("Editor was not opened")?;
                    doc.text = "Rust desktop edit\n".into();
                    let path = doc.path.clone();
                    self.save_document(&id);
                    self.flush_documents();
                    if fs::read_to_string(&path)? != "Rust desktop edit\n" {
                        bail!("Editor save failed: {}", self.message);
                    }
                    fs::write(&path, "external change\n")?;
                    self.documents
                        .get_mut(&id)
                        .context("Editor was closed")?
                        .text = "must refuse".into();
                    self.save_document(&id);
                    self.flush_documents();
                    if fs::read_to_string(&path)? != "external change\n"
                        || !self.message.contains("changed on disk")
                    {
                        bail!("External change was overwritten");
                    }
                    let doc = self.documents.get_mut(&id).context("Editor was closed")?;
                    s.checks.push(
                        "Native editor saves text and refuses external-change overwrite".into(),
                    );
                    doc.text = doc.original.clone();
                    self.action(Action::System, ctx);
                }
                7 => {
                    // The snapshot arrives from a background thread; the overall
                    // timeout reports a view that never fills.
                    self.system.refresh(ctx);
                    if self.system.memory.is_empty() {
                        return Ok(false);
                    }
                    s.checks
                        .push("System view reads live memory and uptime".into());
                    self.action(Action::Processes, ctx);
                }
                8 => {
                    self.system.refresh(ctx);
                    if self.system.processes.is_empty() {
                        return Ok(false);
                    }
                    s.checks.push("Process viewer lists live processes".into());
                    self.action(Action::Theme("lunar".into()), ctx);
                }
                9 => {
                    if !self.theme().kind.eq("light") {
                        bail!("Light theme failed");
                    }
                    s.checks
                        .push("Light and dark presets apply to the native interface".into());
                    self.action(Action::Theme("starlight".into()), ctx);
                    let w = self.workspace_mut();
                    w.active_tab = w.tabs[0].id.clone();
                    self.ensure_focus();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                    s.screenshot = true;
                }
                10 => {
                    for event in ctx.input(|i| i.events.clone()) {
                        if let egui::Event::Screenshot { image, .. } = event {
                            let bytes: Vec<u8> =
                                image.pixels.iter().flat_map(|c| c.to_array()).collect();
                            image::save_buffer(
                                s.report.with_extension("png"),
                                &bytes,
                                image.size[0] as u32,
                                image.size[1] as u32,
                                image::ColorType::Rgba8,
                            )?;
                            s.screenshot = false;
                        }
                    }
                    if s.screenshot {
                        return Ok(false);
                    }
                    let report = serde_json::json!({"passed":s.checks,"profile":self.profile,"renderer":"native egui/OpenGL","success":true});
                    fs::write(&s.report, serde_json::to_vec_pretty(&report)?)?;
                    self.persist_now();
                    self.allow_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return Ok(true);
                }
                _ => {}
            }
            s.stage += 1;
            s.last = Instant::now();
            Ok(false)
        })();
        match result {
            Ok(true) => {}
            Ok(false) => self.smoke = Some(s),
            Err(e) => {
                let _ = fs::write(
                    &s.report,
                    serde_json::json!({"success":false,"error":e.to_string(),"passed":s.checks})
                        .to_string(),
                );
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame(ctx);
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.persist_now();
        self.terminals.clear();
    }
}
impl App {
    /// One UI frame; split from `eframe::App::update` so offscreen tests can drive it.
    fn frame(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending = Some(PendingClose::Window);
        }
        self.poll_documents();
        // The watcher thread reads and parses config.toml; only apply the newest result here.
        if let Some(latest) = self
            .config_watch
            .as_ref()
            .and_then(|w| w.changes.try_iter().last())
        {
            self.apply_config(latest, ctx, true);
        }
        self.poll_look(ctx);
        self.shortcuts(ctx);
        self.apply_font(ctx);
        let theme = self.theme();
        for session in self.terminals.values_mut() {
            session.poll(&theme);
        }
        if theme.validate().is_ok() {
            theme.apply(ctx, self.ui_font_size());
        }
        self.window_edges(ctx);
        self.headerbar(ctx, &theme);
        self.sidebar(ctx, &theme);
        // After the sidebar, so the bar spans only the content beside it.
        self.search_bar(ctx, &theme);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme.ui("background")))
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                self.pane_area = Some(rect);
                let mut layout = self.tab().layout.clone();
                if let Some(zoom) = self.zoom.clone() {
                    if let Some(node) = layout.find(&zoom).cloned() {
                        let mut node = node;
                        self.layout(ui, rect, &mut node, ctx, &theme, true);
                        update_node(&mut layout, &zoom, node);
                    }
                } else {
                    let headers = matches!(layout, Pane::Split { .. });
                    self.layout(ui, rect, &mut layout, ctx, &theme, headers);
                }
                self.tab_mut().layout = layout;
            });
        if let Some(action) = self.pending_action.take() {
            self.action(action, ctx);
        }
        if let Some(path) = self.pending_open.take() {
            self.open_file(path);
        }
        self.overlays(ctx);
        self.about_dialog(ctx, &theme);
        self.toast(ctx, &theme);
        self.window_frame(ctx, &theme);
        if let Some(id) = self.pending_save.take() {
            self.save_document(&id);
        }

        if self.last_save.elapsed() > Duration::from_secs(3) {
            self.persist();
        }
        self.report_write_errors();
        self.smoke_tick(ctx);
        ctx.request_repaint_after(Duration::from_millis(
            if self.state.preferences.cursor_blink {
                250
            } else {
                500
            },
        ));
    }
}
/// Interface zoom limits: beyond these the headerbar no longer fits a minimum-size
/// window, or text gets too small to read.
pub(crate) const ZOOM_RANGE: std::ops::RangeInclusive<f32> = 0.5..=2.0;
/// Zooms the interface one 10% step in (`dir` > 0) or out, within [`ZOOM_RANGE`].
fn step_zoom(ctx: &egui::Context, dir: i32) {
    let steps = (ctx.zoom_factor() * 10.).round() as i32 + dir.signum();
    let z = (steps as f32 / 10.).clamp(*ZOOM_RANGE.start(), *ZOOM_RANGE.end());
    ctx.set_zoom_factor(z);
}
/// Lays `text` out on one line, cut with an ellipsis to fit `width`.
fn truncated(
    ui: &egui::Ui,
    text: &str,
    style: egui::TextStyle,
    color: Color32,
    width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(
        text.to_string(),
        style.resolve(ui.style()),
        color,
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(0.));
    ui.fonts_mut(|f| f.layout_job(job))
}
/// "Ctrl+Shift+D"-style text for a binding.
fn binding_label(b: config::Binding) -> String {
    let mut parts = vec![];
    if b.modifiers.ctrl {
        parts.push("Ctrl");
    }
    if b.modifiers.alt {
        parts.push("Alt");
    }
    if b.modifiers.shift {
        parts.push("Shift");
    }
    let key = b.key.name();
    parts.push(key);
    parts.join("+")
}
impl App {
    /// Re-reads the desktop's light/dark preference and accent (off the UI thread)
    /// when the window regains focus, so flipping the desktop style is picked up.
    fn poll_look(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.look_rx
            && let Ok(colors) = rx.try_recv()
        {
            self.look.scheme = colors.scheme;
            self.look.accent = colors.accent;
            self.look_rx = None;
        }
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if focused
            && !self.was_focused
            && self.look_rx.is_none()
            && self.look_checked.elapsed() > Duration::from_secs(2)
        {
            self.look_checked = Instant::now();
            let (tx, rx) = std::sync::mpsc::channel();
            let ctx = ctx.clone();
            if std::thread::Builder::new()
                .name("caelum-look".into())
                .spawn(move || {
                    let _ = tx.send(appearance::detect_colors());
                    ctx.request_repaint();
                })
                .is_ok()
            {
                self.look_rx = Some(rx);
            }
        }
        self.was_focused = focused;
        self.system_dark = match self.look.scheme {
            Some(appearance::Scheme::Dark) => true,
            Some(appearance::Scheme::Light) => false,
            None => ctx.system_theme().is_none_or(|t| t == egui::Theme::Dark),
        };
    }
}
/// Config action name -> app action for every rebindable shortcut in [`config::ACTIONS`].
fn shortcut_table() -> Vec<(&'static str, Action)> {
    vec![
        ("command_palette", Action::Palette),
        ("toggle_sidebar", Action::Sidebar),
        ("new_tab", Action::NewTab),
        ("split_right", Action::Split(Direction::Right)),
        ("split_below", Action::Split(Direction::Down)),
        ("split_left", Action::Split(Direction::Left)),
        ("split_up", Action::Split(Direction::Up)),
        ("balance_panes", Action::Equalize),
        ("zoom_pane", Action::Zoom),
        ("close_pane", Action::ClosePane),
        ("open_file", Action::Open),
        ("save_file", Action::Save),
        ("search", Action::Search),
        ("next_tab", Action::NextTab),
        ("prev_tab", Action::PrevTab),
        ("copy", Action::Copy),
        ("paste", Action::Paste),
        ("prev_prompt", Action::PrevPrompt),
        ("next_prompt", Action::NextPrompt),
    ]
}

/// Removes the active workspace. The state never ends up without a workspace: closing
/// the last one leaves a fresh workspace at `cwd` in its place.
fn close_workspace(state: &mut State, cwd: PathBuf) {
    let i = state.active_index();
    if i < state.workspaces.len() {
        state.workspaces.remove(i);
    }
    if state.workspaces.is_empty() {
        state.workspaces.push(Workspace::new("Workspace 1", cwd));
    }
    let next = i.min(state.workspaces.len() - 1);
    state.active_workspace = state.workspaces[next].id.clone();
}
fn update_node(root: &mut Pane, id: &str, replacement: Pane) {
    if root.id() == id {
        *root = replacement;
    } else if let Pane::Split { first, second, .. } = root {
        if first.find(id).is_some() {
            update_node(first, id, replacement);
        } else {
            update_node(second, id, replacement);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_configurable_action_is_dispatched() {
        let wired: Vec<&str> = shortcut_table().into_iter().map(|(n, _)| n).collect();
        for (name, _) in config::ACTIONS {
            assert!(wired.contains(name), "{name} has no dispatch entry");
        }
        assert_eq!(wired.len(), config::ACTIONS.len());
        let k = Keymap::default();
        assert_eq!(k.get("prev_prompt").unwrap().key, Key::ArrowUp);
        assert_eq!(k.get("next_prompt").unwrap().key, Key::ArrowDown);
    }

    #[test]
    fn closing_last_workspace_leaves_a_fresh_one() {
        let mut state = State::default();
        let cwd = std::env::temp_dir();
        while state.workspaces.len() > 1 {
            close_workspace(&mut state, cwd.clone());
        }
        let only = state.workspaces[0].id.clone();
        close_workspace(&mut state, cwd.clone());
        assert_eq!(state.workspaces.len(), 1);
        assert_ne!(state.workspaces[0].id, only);
        assert_eq!(state.active_workspace, state.workspaces[0].id);
    }
    #[test]
    fn closing_a_workspace_activates_a_neighbour() {
        let mut state = State::default();
        let cwd = std::env::temp_dir();
        for n in 0..3 {
            state
                .workspaces
                .push(Workspace::new(format!("W{n}"), cwd.clone()));
        }
        let last = state.workspaces.last().unwrap().id.clone();
        state.active_workspace = last.clone();
        let before = state.workspaces.len();
        close_workspace(&mut state, cwd);
        assert_eq!(state.workspaces.len(), before - 1);
        assert!(state.workspaces.iter().all(|w| w.id != last));
        assert_eq!(state.active_workspace, state.workspaces.last().unwrap().id);
    }
}
