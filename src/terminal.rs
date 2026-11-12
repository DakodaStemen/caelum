//! PTY ownership, terminal emulation and input translation. The UI never blocks on PTY I/O.
use crate::{
    model::Preferences,
    osc::{OscEvent, OscGuard, OscScanner},
    theme::Theme,
};
use alacritty_terminal::{
    event::{Event, EventListener},
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point, Side},
    selection::{Selection, SelectionType},
    term::{ClipboardType, Config, Osc52, Term, TermMode},
    vte::ansi::{self, Color, NamedColor},
};
#[cfg_attr(not(unix), allow(unused_imports))]
use anyhow::{Context, Result, bail};
use eframe::egui::emath::GuiRounding;
use eframe::egui::{self, Color32, FontId, Key, Modifiers, Pos2, Rect, Sense, Stroke, Vec2};
use portable_pty::{CommandBuilder, MasterPty, PtySize};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
/// Terminal events forwarded from the parser thread to the UI thread.
///
/// The queue is bounded so a session whose pane is not being drawn (and therefore
/// not polled) cannot grow memory without limit. Title changes are coalesced into
/// a single latest-value slot, so a program spamming OSC 0/2 costs O(1) memory and
/// the UI only ever sees the newest title. Only events the UI acts on are queued;
/// if the queue is full, further replies are dropped rather than blocking the
/// parser (alacritty's `EventListener::send_event` must not block).
#[derive(Clone)]
pub struct Events {
    tx: SyncSender<Event>,
    title: Arc<Mutex<Option<String>>>,
    /// Whether programs may set the clipboard (OSC 52); read on the PTY thread, so a
    /// refused write is dropped before it is queued.
    osc52: Arc<AtomicBool>,
    clipboard: Arc<Mutex<Option<ClipWrite>>>,
}
/// The newest OSC 52 write waiting for the UI: the text, or the size of one refused
/// for being too large.
type ClipWrite = Result<String, usize>;
/// UI-side end of [`Events`].
struct EventQueue {
    rx: Receiver<Event>,
    title: Arc<Mutex<Option<String>>>,
    osc52: Arc<AtomicBool>,
    clipboard: Arc<Mutex<Option<ClipWrite>>>,
}
const EVENT_QUEUE: usize = 1024;
impl Events {
    fn new() -> (Self, EventQueue) {
        let (tx, rx) = mpsc::sync_channel(EVENT_QUEUE);
        let title = Arc::new(Mutex::new(None));
        // Off until the owner says otherwise: nothing is written before the preference is known.
        let osc52 = Arc::new(AtomicBool::new(false));
        let clipboard = Arc::new(Mutex::new(None));
        (
            Self {
                tx,
                title: title.clone(),
                osc52: osc52.clone(),
                clipboard: clipboard.clone(),
            },
            EventQueue {
                rx,
                title,
                osc52,
                clipboard,
            },
        )
    }
}
impl EventListener for Events {
    fn send_event(&self, event: Event) {
        match event {
            Event::Title(title) => *lock(&self.title) = Some(sanitize_title(&title)),
            Event::ResetTitle => *lock(&self.title) = Some(DEFAULT_TITLE.into()),
            Event::PtyWrite(_) | Event::ColorRequest(..) | Event::TextAreaSizeRequest(_) => {
                let _ = self.tx.try_send(event);
            }
            // OSC 52 writes only. Reads (`ClipboardLoad`) are never forwarded, so a
            // program cannot see the clipboard. A refused or oversized write never
            // reaches the queue, and only the newest accepted one is kept, so a flood
            // of them holds at most one payload.
            Event::ClipboardStore(ClipboardType::Clipboard, text) => {
                match osc52_accepts(self.osc52.load(Ordering::Relaxed), &text) {
                    Ok(true) => *lock(&self.clipboard) = Some(Ok(text)),
                    Ok(false) => {}
                    Err(n) => *lock(&self.clipboard) = Some(Err(n)),
                }
            }
            // Everything else (wakeups, bells, reads, the PRIMARY selection, ...) is ignored.
            _ => {}
        }
    }
}
/// Largest OSC 52 clipboard write that is honoured.
pub const OSC52_MAX_BYTES: usize = 1024 * 1024;
/// Whether a program's OSC 52 write of `text` may reach the clipboard: the setting
/// is on, the text is not empty and it is within [`OSC52_MAX_BYTES`].
fn osc52_accepts(enabled: bool, text: &str) -> Result<bool, usize> {
    if !enabled || text.is_empty() {
        return Ok(false);
    }
    if text.len() > OSC52_MAX_BYTES {
        return Err(text.len());
    }
    Ok(true)
}
const DEFAULT_TITLE: &str = "Terminal";
const MAX_TITLE_CHARS: usize = 100;
/// Bidirectional formatting characters that can visually reorder surrounding text.
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{200e}' | '\u{200f}' | '\u{061c}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}
/// Makes a program-supplied window title safe to show in tabs and window chrome:
/// drops C0/C1 controls and DEL plus bidi overrides/isolates, trims, and caps length.
fn sanitize_title(raw: &str) -> String {
    let clean: String = raw
        .chars()
        .filter(|&c| !c.is_control() && !is_bidi_control(c))
        .collect();
    let clean: String = clean.trim().chars().take(MAX_TITLE_CHARS).collect();
    let clean = clean.trim_end();
    if clean.is_empty() {
        DEFAULT_TITLE.into()
    } else {
        clean.into()
    }
}
/// Removes bytes a paste must never deliver to the PTY: every C0 control except
/// tab, newline and carriage return (this includes ESC, so a payload cannot end
/// bracketed paste early or inject sequences), DEL, and C1 controls.
fn sanitize_paste(text: &str) -> String {
    text.chars()
        .filter(|&c| matches!(c, '\t' | '\n' | '\r') || !c.is_control())
        .collect()
}
#[derive(Clone, Copy)]
pub struct Size {
    pub cols: usize,
    pub rows: usize,
}
impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}
/// Shell-reported state from OSC 7 / OSC 133, shared with the PTY reader thread.
#[derive(Default, Debug)]
pub struct ShellState {
    /// Last OSC 7 working directory for this host.
    pub cwd: Option<PathBuf>,
    /// Prompt starts as absolute line indexes (0 = oldest scrollback line).
    marks: Vec<i64>,
    /// Exit status of the most recent finished command, if the shell reported one.
    pub last_exit: Option<i32>,
    /// True between OSC 133;C and OSC 133;D.
    pub running: bool,
    last_history: usize,
    /// When the running command's output began (OSC 133;C).
    started: Option<Instant>,
    /// Absolute line where the running command's output began.
    output_start: Option<i64>,
    /// Absolute line span `start..end` of the last finished command's output, and, when
    /// its last line had no trailing newline, the last column that output filled (the
    /// next prompt is drawn after it on that same line and must not be copied).
    output: Option<(i64, i64, Option<usize>)>,
    /// A command that just finished, until [`Session::take_finished`] collects it.
    finished: Option<Finished>,
}
/// A command that ran from OSC 133;C to OSC 133;D.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Finished {
    pub elapsed: Duration,
    /// Exit status, if the shell reported one.
    pub code: Option<i32>,
}
impl ShellState {
    fn clear_marks(&mut self) {
        self.marks.clear();
        // Output spans are absolute line numbers too.
        self.output_start = None;
        self.output = None;
    }
}
/// Absolute index of the cursor's line (0 = oldest scrollback line).
fn cursor_abs(term: &Term<Events>) -> i64 {
    term.grid().history_size() as i64 + i64::from(term.grid().cursor.point.line.0)
}
fn apply_osc(term: &Term<Events>, shell: &mut ShellState, event: OscEvent) {
    match event {
        OscEvent::Cwd(path) => shell.cwd = Some(path),
        OscEvent::PromptStart => {
            // Marks are only meaningful on the primary screen.
            if term.mode().contains(TermMode::ALT_SCREEN) {
                return;
            }
            let abs =
                term.grid().history_size() as i64 + i64::from(term.grid().cursor.point.line.0);
            if shell.marks.last() != Some(&abs) {
                shell.marks.push(abs);
            }
        }
        OscEvent::OutputStart => {
            shell.running = true;
            shell.started = Some(Instant::now());
            // Output starts on the line below the command the user typed.
            shell.output_start =
                (!term.mode().contains(TermMode::ALT_SCREEN)).then(|| cursor_abs(term));
        }
        OscEvent::CommandEnd(code) => {
            let was_running = shell.running;
            shell.running = false;
            shell.last_exit = code;
            if let Some(start) = shell.output_start.take()
                && !term.mode().contains(TermMode::ALT_SCREEN)
            {
                // The cursor sits at the start of the line after the output, unless the
                // output did not end in a newline; then its last line is the cursor's.
                let cursor = cursor_abs(term);
                let column = term.grid().cursor.point.column.0;
                let (end, last_col) = if column == 0 {
                    (cursor, None)
                } else {
                    (cursor + 1, Some(column - 1))
                };
                shell.output = (end > start).then_some((start, end, last_col));
            }
            if let Some(started) = shell.started.take()
                && was_running
            {
                shell.finished = Some(Finished {
                    elapsed: started.elapsed(),
                    code,
                });
            }
        }
    }
}
/// Where the shell integration scripts live and how to start `shell` with them, or
/// `None` when the shell is unknown or the scripts cannot be written (the terminal
/// then simply starts without integration).
fn shell_injection(shell: &str) -> Option<crate::shell_integration::Injection> {
    let root = crate::shell_integration::dir_in(&crate::platform::default_profile_dir());
    crate::shell_integration::install(&root).ok()?;
    crate::shell_integration::plan(shell, &root, &|k| std::env::var(k).ok())
}
/// Text of the absolute line span `start..end` (end exclusive), if it is still on the
/// grid. With `last_col`, the last line is cut after that column.
fn output_text(
    term: &Term<Events>,
    start: i64,
    end: i64,
    last_col: Option<usize>,
) -> Option<String> {
    let history = term.grid().history_size() as i64;
    let (first, last) = (start - history, end - 1 - history);
    if end <= start || first < -history || last >= term.screen_lines() as i64 {
        return None;
    }
    let text = term.bounds_to_string(
        Point::new(Line(first as i32), Column(0)),
        Point::new(
            Line(last as i32),
            last_col.map_or(term.last_column(), |c| Column(c).min(term.last_column())),
        ),
    );
    Some(text.trim_end_matches('\n').to_string())
}
/// Absolute indexes drift once Alacritty starts discarding old scrollback, so marks
/// are dropped when history is truncated or full instead of pointing at wrong lines.
fn reconcile_marks(term: &Term<Events>, shell: &mut ShellState) {
    let history = term.grid().history_size();
    if history < shell.last_history || history >= Config::default().scrolling_history {
        shell.clear_marks();
    }
    shell.last_history = history;
}
pub struct Session {
    pub term: Arc<Mutex<Term<Events>>>,
    pub title: String,
    pub cwd: PathBuf,
    pub exited: bool,
    pub error: Option<String>,
    pub shell: Arc<Mutex<ShellState>>,
    // `Option` only so `Drop` can hand them to the cleanup thread; always `Some` while alive.
    master: Option<Box<dyn MasterPty + Send>>,
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    input: Option<SyncSender<Vec<u8>>>,
    events: EventQueue,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    size: Size,
    search_origin: Option<SearchOrigin>,
    pending_input: Vec<u8>,
    /// Multi-line paste held for user confirmation (already sanitised).
    pending_paste: Option<String>,
    /// Pointer state carried between frames (clicks, drags, reports).
    mouse: Mouse,
    /// Grid geometry from the last frame the pane was drawn.
    metrics: Option<Metrics>,
    /// Focus state last seen, for focus in/out reports (mode 1004).
    focus_reported: Option<bool>,
    /// Text was copied to the clipboard since the app last asked (for the toast).
    copied: bool,
    /// Output arrived while the view was scrolled back (for the scroll-to-bottom pill).
    unseen_output: bool,
    /// Last history size and display offset seen, to tell new output from scrolling.
    last_history: usize,
    /// Directories may be opened from a Ctrl+click (a file browser pane exists).
    pub dir_links: bool,
    /// Background lookup of path text hovered with Ctrl held.
    probe: PathProbe,
    /// Launched as `ssh`/`codex`/`claude` rather than a shell.
    runs_program: bool,
    wake: egui::Context,
    /// Test hook: when set, the cleanup thread waits for a message before doing any
    /// work, so a test can prove `drop` returns while cleanup has not yet run.
    #[cfg(test)]
    cleanup_gate: Option<std::sync::mpsc::Receiver<()>>,
}
impl Session {
    pub fn spawn(cwd: &Path, argv: Option<&[String]>, ctx: egui::Context) -> Result<Self> {
        Self::spawn_with(cwd, argv, ctx, true, true)
    }
    /// Starts a session. With `integrate`, the user's shell (when `argv` is `None`) is
    /// launched so it emits OSC 7 / OSC 133 without any dotfile edits; see
    /// [`crate::shell_integration`]. `osc52` is the clipboard-write preference in force
    /// from the first byte of output; keep it current with [`Session::set_osc52_write`].
    pub fn spawn_with(
        cwd: &Path,
        argv: Option<&[String]>,
        ctx: egui::Context,
        integrate: bool,
        osc52: bool,
    ) -> Result<Self> {
        let cwd = if cwd.is_dir() {
            cwd.to_path_buf()
        } else {
            crate::model::home()
        };
        let size = Size { cols: 80, rows: 24 };
        let pair = portable_pty::native_pty_system().openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let shell = crate::platform::resolve_shell(std::env::var("SHELL").ok().as_deref());
        let mut cmd = if let Some(argv) = argv {
            let first = argv.first().context("Empty terminal command")?;
            let mut c = CommandBuilder::new(first);
            c.args(&argv[1..]);
            c
        } else {
            let mut c = CommandBuilder::new(&shell);
            let injection = integrate.then(|| shell_injection(&shell)).flatten();
            match injection.as_ref().and_then(|i| i.args.as_ref()) {
                Some(args) => c.args(args),
                None => c.args(crate::platform::shell_args(&shell)),
            };
            if let Some(i) = injection {
                for (k, v) in i.env {
                    c.env(k, v);
                }
            }
            c
        };
        // These panes are the program itself, not a shell: closing one stops real work.
        let runs_program = argv
            .and_then(|a| a.first())
            .is_some_and(|p| matches!(p.as_str(), "ssh" | "codex" | "claude"));
        cmd.cwd(&cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "Caelum Rust");
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        // Lets shell rc files enable the integration only inside Caelum.
        cmd.env("CAELUM_RS", env!("CARGO_PKG_VERSION"));
        // Nonblocking descriptors let both workers honor shutdown even if a descendant holds the PTY open.
        #[cfg(unix)]
        if let Some(fd) = pair.master.as_raw_fd() {
            // SAFETY: fd is owned by the live PTY; fcntl changes flags without taking ownership.
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFL);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                    bail!("Cannot configure nonblocking PTY");
                }
            }
        }
        // The reader thread waits for readiness with poll(2) on its own duplicate of the
        // master descriptor, so it never outlives the fd it polls.
        #[cfg(unix)]
        let poll_fd = {
            use std::os::fd::{FromRawFd, OwnedFd};
            let fd = pair.master.as_raw_fd().context("PTY has no descriptor")?;
            // SAFETY: fd is a live descriptor; dup returns a new one we exclusively own.
            let dup = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
            if dup < 0 {
                bail!("Cannot duplicate PTY descriptor");
            }
            // SAFETY: dup is a freshly created, valid descriptor owned by nobody else.
            unsafe { OwnedFd::from_raw_fd(dup) }
        };
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let (listener, events) = Events::new();
        events.osc52.store(osc52, Ordering::Relaxed);
        let term = Arc::new(Mutex::new(Term::new(term_config(), &size, listener)));
        let stop = Arc::new(AtomicBool::new(false));
        let read_stop = stop.clone();
        let read_term = term.clone();
        let read_ctx = ctx.clone();
        let shell = Arc::new(Mutex::new(ShellState::default()));
        let read_shell = shell.clone();
        let read_thread = thread::spawn(move || {
            read_loop(
                reader,
                read_term,
                read_shell,
                read_stop,
                read_ctx,
                #[cfg(unix)]
                poll_fd,
            )
        });
        let (input, rx) = mpsc::sync_channel::<Vec<u8>>(32);
        let write_stop = stop.clone();
        let write_thread = thread::spawn(move || write_loop(writer, rx, write_stop));
        Ok(Self {
            term,
            title: "Terminal".into(),
            cwd,
            exited: false,
            error: None,
            shell,
            master: Some(pair.master),
            child: Some(child),
            input: Some(input),
            events,
            stop,
            threads: vec![read_thread, write_thread],
            size,
            search_origin: None,
            pending_input: Vec::new(),
            pending_paste: None,
            mouse: Mouse::default(),
            metrics: None,
            focus_reported: None,
            copied: false,
            unseen_output: false,
            last_history: 0,
            dir_links: false,
            probe: PathProbe::new(ctx.clone()),
            runs_program,
            wake: ctx,
            #[cfg(test)]
            cleanup_gate: None,
        })
    }
    pub fn write(&mut self, bytes: Vec<u8>) {
        const MAX_PENDING: usize = 8 * 1024 * 1024;
        if self.pending_input.len().saturating_add(bytes.len()) > MAX_PENDING {
            self.error = Some(
                "Terminal input buffer is full; wait for the process to read before pasting again"
                    .into(),
            );
            return;
        }
        self.pending_input.extend(bytes);
        self.flush_input();
    }
    fn flush_input(&mut self) {
        if self.pending_input.is_empty() {
            return;
        }
        if let Some(tx) = &self.input {
            let bytes = std::mem::take(&mut self.pending_input);
            match tx.try_send(bytes) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(bytes)) => {
                    self.pending_input = bytes;
                    self.wake.request_repaint_after(Duration::from_millis(8));
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    self.error = Some("Terminal input has closed".into())
                }
            }
        }
    }
    /// Writes a paste to the PTY now, bracketed if the program enabled it.
    pub fn paste(&mut self, text: &str) {
        let bracketed = self.bracketed_paste();
        self.write(paste_bytes(text, bracketed));
    }
    /// Pastes like [`Session::paste`], unless [`needs_confirm`] says the user should
    /// see it first; then it is held until [`Session::take_pending_paste`].
    /// Returns true when the paste was held.
    pub fn paste_or_hold(&mut self, text: &str, confirm_multiline: bool) -> bool {
        if needs_confirm(text, self.bracketed_paste(), confirm_multiline) {
            self.pending_paste = Some(sanitize_paste(text));
            true
        } else {
            self.paste(text);
            false
        }
    }
    /// Copies `text` to the clipboard and notes it for the "Copied" toast.
    pub fn copy_to_clipboard(&mut self, ctx: &egui::Context, text: String) {
        if !text.is_empty() {
            ctx.copy_text(text);
            self.copied = true;
        }
    }
    /// The text of the last finished command's output (OSC 133 C to D), if the shell
    /// integration reported one and it is still in the scrollback.
    pub fn last_output(&self) -> Option<String> {
        let term = lock(&self.term);
        let (start, end, last_col) = lock(&self.shell).output?;
        output_text(&term, start, end, last_col).filter(|t| !t.is_empty())
    }
    /// The command that finished since the last call, if any.
    pub fn take_finished(&mut self) -> Option<Finished> {
        lock(&self.shell).finished.take()
    }
    /// Whether closing this pane would stop something the user is running: a command
    /// the shell integration reports, a foreground job other than the shell itself,
    /// or an `ssh`/`codex`/`claude` session. An idle shell at a prompt is not busy.
    pub fn is_busy(&self) -> bool {
        if self.exited {
            return false;
        }
        if self.runs_program || lock(&self.shell).running {
            return true;
        }
        #[cfg(unix)]
        if let (Some(master), Some(pid)) = (&self.master, self.pid())
            && let Some(fg) = master.process_group_leader()
        {
            return fg > 0 && fg as u32 != pid;
        }
        false
    }
    /// Whether programs in this pane may set the clipboard (OSC 52). Takes effect for
    /// output the reader has not yet parsed; call it for every session before polling.
    pub fn set_osc52_write(&self, on: bool) {
        self.events.osc52.store(on, Ordering::Relaxed);
    }
    /// Whether text was copied since the last call.
    pub fn take_copied(&mut self) -> bool {
        std::mem::take(&mut self.copied)
    }
    /// The held multi-line paste, if any, for the app to confirm.
    pub fn take_pending_paste(&mut self) -> Option<String> {
        self.pending_paste.take()
    }
    fn bracketed_paste(&self) -> bool {
        lock(&self.term).mode().contains(TermMode::BRACKETED_PASTE)
    }
    pub fn poll(&mut self, theme: &Theme) {
        self.flush_input();
        if let Some(title) = lock(&self.events.title).take() {
            self.title = title;
        }
        while let Ok(event) = self.events.rx.try_recv() {
            match event {
                Event::PtyWrite(text) => self.write(text.into_bytes()),
                Event::ColorRequest(i, format) => {
                    let c = index_color(i, theme);
                    self.write(
                        format(ansi::Rgb {
                            r: c.r(),
                            g: c.g(),
                            b: c.b(),
                        })
                        .into_bytes(),
                    );
                }
                Event::TextAreaSizeRequest(format) => self.write(
                    format(alacritty_terminal::event::WindowSize {
                        num_lines: self.size.rows as u16,
                        num_cols: self.size.cols as u16,
                        cell_width: 0,
                        cell_height: 0,
                    })
                    .into_bytes(),
                ),
                _ => {}
            }
        }
        // OSC 52 is write-only: the clipboard can be set but never read back. The
        // preference is re-checked here, so a write queued just before the user turned
        // it off is dropped too.
        let pending = lock(&self.events.clipboard).take();
        match pending {
            Some(Ok(text)) if self.events.osc52.load(Ordering::Relaxed) => {
                let ctx = self.wake.clone();
                self.copy_to_clipboard(&ctx, text);
            }
            Some(Err(n)) if self.events.osc52.load(Ordering::Relaxed) => {
                self.error = Some(format!(
                    "Ignored a {} KiB clipboard write (limit {} KiB)",
                    n / 1024,
                    OSC52_MAX_BYTES / 1024
                ));
            }
            _ => {}
        }
        if !self.exited
            && let Some(child) = &mut self.child
        {
            match child.try_wait() {
                Ok(Some(_)) => self.exited = true,
                Ok(None) => {}
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        // A shell-reported OSC 7 directory wins; the platform lookup is the fallback
        // for shells without integration (it only sees the process we spawned, not subshells).
        let reported = lock(&self.shell).cwd.clone().filter(|p| p.is_dir());
        if let Some(cwd) = reported {
            self.cwd = cwd;
        } else if let Some(pid) = self.pid()
            && !self.exited
            && let Some(cwd) = crate::platform::process_cwd(pid)
        {
            self.cwd = cwd;
        }
    }
    /// Notes output that arrived while the view is scrolled back, and forgets it
    /// once the view is at the bottom again.
    fn track_unseen_output(&mut self) {
        let (history, offset) = {
            let term = lock(&self.term);
            (term.grid().history_size(), term.grid().display_offset())
        };
        if offset == 0 {
            self.unseen_output = false;
        } else if history != self.last_history {
            self.unseen_output = true;
        }
        self.last_history = history;
    }
    /// Whether the view is scrolled back from the live bottom of the terminal.
    pub fn scrolled_back(&self) -> bool {
        lock(&self.term).grid().display_offset() > 0
    }
    /// Whether output arrived while the view was scrolled back.
    pub fn has_unseen_output(&self) -> bool {
        self.unseen_output
    }
    /// Where the "scroll to bottom" pill is drawn (only while scrolled back), once
    /// the pane has been drawn.
    pub fn scroll_pill_rect(&self) -> Option<Rect> {
        let m = self.metrics.as_ref()?;
        self.scrolled_back().then(|| pill_rect(m.rect))
    }
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let size = Size {
            cols: cols.clamp(2, 1000),
            rows: rows.clamp(1, 1000),
        };
        if self.size.cols == size.cols && self.size.rows == size.rows {
            return;
        }
        let Some(master) = &self.master else { return };
        if let Err(e) = master.resize(PtySize {
            rows: size.rows as u16,
            cols: size.cols as u16,
            pixel_width: 0,
            pixel_height: 0,
        }) {
            self.error = Some(e.to_string());
            return;
        }
        if self.size.cols != size.cols {
            // Reflow changes line counts, so stored line positions are no longer valid.
            lock(&self.shell).clear_marks();
        }
        lock(&self.term).resize(size);
        self.size = size;
    }
    pub fn text(&self) -> String {
        let term = lock(&self.term);
        let mut text = String::new();
        for row in 0..term.screen_lines() {
            for col in 0..term.columns() {
                let c = &term.grid()[Point::new(Line(row as i32), Column(col))];
                if !c
                    .flags
                    .contains(alacritty_terminal::term::cell::Flags::WIDE_CHAR_SPACER)
                {
                    text.push(c.c);
                }
            }
            text.push('\n');
        }
        text
    }
    pub fn search(&mut self, query: &str, forward: bool) -> bool {
        let mut term = lock(&self.term);
        search_term(&mut term, &mut self.search_origin, query, forward)
    }
    /// Scroll to the previous/next shell prompt (needs OSC 133 integration).
    /// Returns false when there is no such prompt.
    pub fn jump_prompt(&mut self, forward: bool) -> bool {
        let mut term = lock(&self.term);
        let shell = lock(&self.shell);
        let history = term.grid().history_size() as i64;
        let top = -(term.grid().display_offset() as i64);
        let lines = shell.marks.iter().map(|abs| abs - history);
        let target = if forward {
            lines.filter(|l| *l > top).min()
        } else {
            lines.filter(|l| *l < top).max()
        };
        match target {
            Some(line) => {
                let offset = (-line).clamp(0, history) as i32;
                term.scroll_display(Scroll::Bottom);
                term.scroll_display(Scroll::Delta(offset));
                true
            }
            None if forward => {
                term.scroll_display(Scroll::Bottom);
                false
            }
            None => false,
        }
    }
    pub fn selection(&self) -> Option<String> {
        lock(&self.term).selection_to_string()
    }
    /// Selects everything from the top of the scrollback to the last line with text.
    pub fn select_all(&mut self) {
        let mut term = lock(&self.term);
        let last = term.last_column();
        let mut bottom = term.bottommost_line();
        while bottom > term.topmost_line()
            && (0..=last.0).all(|c| term.grid()[Point::new(bottom, Column(c))].c == ' ')
        {
            bottom -= 1;
        }
        let mut selection = Selection::new(
            SelectionType::Simple,
            Point::new(term.topmost_line(), Column(0)),
            Side::Left,
        );
        selection.update(Point::new(bottom, last), Side::Right);
        term.selection = Some(selection);
    }
    pub fn clear_selection(&mut self) {
        lock(&self.term).selection = None;
    }
    /// Drops the scrollback above the screen (like `clear -x` plus history).
    pub fn clear_scrollback(&mut self) {
        use alacritty_terminal::vte::ansi::{ClearMode, Handler};
        lock(&self.term).clear_screen(ClearMode::Saved);
        lock(&self.shell).clear_marks();
        self.search_origin = None;
    }
    /// The pane area the terminal was drawn in last frame (grid plus padding).
    pub fn pane_rect(&self) -> Option<Rect> {
        Some(self.metrics?.rect)
    }
    /// Where the cell at screen `col`/`row` was drawn in the last frame.
    pub fn cell_rect(&self, col: usize, row: usize) -> Option<Rect> {
        let m = self.metrics?;
        Some(Rect::from_min_size(
            m.cell_pos(col, row as i32),
            Vec2::new(m.cw, m.ch),
        ))
    }
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref()?.process_id()
    }
}
/// PTY reader thread: feeds output to the parser (applying OSC 7/133 events at
/// their exact byte positions) until EOF, an error, or the stop flag.
fn read_loop(
    mut reader: Box<dyn Read + Send>,
    term: Arc<Mutex<Term<Events>>>,
    shell: Arc<Mutex<ShellState>>,
    stop: Arc<AtomicBool>,
    ctx: egui::Context,
    #[cfg(unix)] poll_fd: std::os::fd::OwnedFd,
) {
    let mut parser: ansi::Processor = ansi::Processor::new();
    let mut scanner = OscScanner::default();
    let mut guard = OscGuard::new();
    let mut bytes = [0; 16384];
    while !stop.load(Ordering::Relaxed) {
        match reader.read(&mut bytes) {
            Ok(0) => break,
            Ok(n) => {
                {
                    let events = scanner.scan(&bytes[..n]);
                    let mut t = lock(&term);
                    let mut shell = lock(&shell);
                    let mut done = 0;
                    for (end, event) in events {
                        parser.advance(&mut *t, &guard.filter(&bytes[done..end]));
                        done = end;
                        apply_osc(&t, &mut shell, event);
                    }
                    parser.advance(&mut *t, &guard.filter(&bytes[done..n]));
                    reconcile_marks(&t, &mut shell);
                }
                ctx.request_repaint();
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                #[cfg(unix)]
                wait_readable(&poll_fd, READ_IDLE_WAIT);
                #[cfg(not(unix))]
                thread::sleep(READ_IDLE_WAIT);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    ctx.request_repaint();
}
/// PTY writer thread: drains queued input chunks, retrying partial and
/// would-block writes, until the channel closes or the stop flag is set.
fn write_loop(mut writer: Box<dyn Write + Send>, rx: Receiver<Vec<u8>>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        let bytes = match rx.recv_timeout(Duration::from_millis(30)) {
            Ok(b) => b,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        };
        let mut offset = 0;
        while offset < bytes.len() && !stop.load(Ordering::Relaxed) {
            match writer.write(&bytes[offset..]) {
                Ok(0) => return,
                Ok(n) => offset += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(4))
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return,
            }
        }
    }
}
/// Longest the reader sleeps between checks of the stop flag while the PTY is idle.
const READ_IDLE_WAIT: Duration = if cfg!(unix) {
    Duration::from_millis(100)
} else {
    Duration::from_millis(25)
};
/// Blocks until `fd` is readable, hung up, or `timeout` elapses, so an idle
/// session costs no CPU while staying responsive to its stop flag.
#[cfg(unix)]
fn wait_readable(fd: &std::os::fd::OwnedFd, timeout: Duration) {
    use std::os::fd::AsRawFd;
    let mut pfd = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: pfd is a valid pollfd for a descriptor we own for the whole call.
    // EINTR and errors simply return; the caller retries the read.
    unsafe {
        libc::poll(&mut pfd, 1, timeout.as_millis() as libc::c_int);
    }
}
/// Whether a paste must be confirmed before it reaches the PTY: without bracketed
/// paste every line break acts as Enter, so a multi-line paste can run commands the
/// user never saw. Bracketed-paste programs (and users who opted out) paste at once.
pub fn needs_confirm(text: &str, bracketed: bool, confirm_multiline: bool) -> bool {
    confirm_multiline && !bracketed && sanitize_paste(text).contains(['\n', '\r'])
}
/// Lines shown in the paste confirmation preview.
const PREVIEW_LINES: usize = 5;
const PREVIEW_LINE_CHARS: usize = 120;
/// Line count and a safe preview of a held paste: the first [`PREVIEW_LINES`]
/// lines with controls and bidi characters escaped, long lines shortened.
pub fn paste_preview(text: &str) -> (usize, String) {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let normalized = normalized.strip_suffix('\n').unwrap_or(&normalized);
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut preview: Vec<String> = lines
        .iter()
        .take(PREVIEW_LINES)
        .map(|line| {
            let escaped: String = line
                .chars()
                .flat_map(|c| {
                    if c.is_control() || is_bidi_control(c) {
                        c.escape_default().collect::<Vec<_>>()
                    } else {
                        vec![c]
                    }
                })
                .collect();
            if escaped.chars().count() > PREVIEW_LINE_CHARS {
                let head: String = escaped.chars().take(PREVIEW_LINE_CHARS).collect();
                format!("{head}…")
            } else {
                escaped
            }
        })
        .collect();
    if lines.len() > PREVIEW_LINES {
        preview.push(format!("… {} more line(s)", lines.len() - PREVIEW_LINES));
    }
    (lines.len(), preview.join("\n"))
}
/// Bytes sent to the PTY for a paste, after sanitising and optional bracketing.
fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let text = sanitize_paste(text);
    if bracketed {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.replace("\r\n", "\n").replace('\n', "\r").into_bytes()
    }
}
impl Drop for Session {
    /// Closing a pane must never stall the UI: sweeping the session's process tree
    /// (a `/proc` scan on Linux), reaping the child and joining the I/O threads
    /// (each can sit in a poll/recv timeout) all happen on a detached cleanup thread.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.input.take();
        let (Some(mut child), Some(master)) = (self.child.take(), self.master.take()) else {
            return;
        };
        let threads = std::mem::take(&mut self.threads);
        #[cfg(test)]
        let gate = self.cleanup_gate.take();
        // Decide liveness before any signal so the cleanup thread never reaps the
        // child before it has walked the child's session.
        let running = matches!(child.try_wait(), Ok(None));
        // Signal the foreground job and the shell's process group right away (a few
        // non-blocking syscalls) so they die even if the app exits before the cleanup
        // thread runs. The thread then also sweeps background jobs in the session.
        #[cfg(unix)]
        if running && let Some(pid) = child.process_id() {
            let pid = pid as libc::pid_t;
            // SAFETY: getsid/kill have no memory-safety preconditions. The child is
            // unreaped, so its pid (a session and group leader created by portable-pty)
            // is still ours, and the foreground group is only signalled if it belongs
            // to that session.
            unsafe {
                if let Some(fg) = master.process_group_leader()
                    && fg > 0
                    && libc::getsid(fg) == pid
                {
                    libc::kill(-fg, libc::SIGKILL);
                }
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        let cleanup = move || {
            #[cfg(test)]
            if let Some(gate) = gate {
                let _ = gate.recv();
            }
            if running && let Some(pid) = child.process_id() {
                crate::platform::kill_process_tree(pid, &*master);
            }
            let _ = child.kill();
            // Unix: close every master descriptor before reaping. The I/O threads
            // hold clones of it, and the writer's drop queues a newline + EOF whose
            // echo nobody reads once the reader has stopped. On macOS an exiting
            // session leader waits in the kernel (exit -> ttywait) for its tty's
            // output to drain, so reaping while any master fd is open blocks forever
            // and the child never finishes exiting. Both threads honour the stop flag
            // within one poll interval and close their clones on return.
            #[cfg(unix)]
            {
                drop(master);
                for t in threads {
                    let _ = t.join();
                }
                let _ = child.wait();
            }
            // Windows: the child is already terminated; reap it first, then let the
            // threads finish before closing the pseudoconsole (unchanged order).
            #[cfg(not(unix))]
            {
                let _ = child.wait();
                for t in threads {
                    let _ = t.join();
                }
                drop(master);
            }
        };
        if let Err(e) = thread::Builder::new()
            .name("caelum-pty-cleanup".into())
            .spawn(cleanup)
        {
            // Out of threads: the closure (and the child) was dropped unrun. Nothing
            // more can be done without blocking the UI; record it for diagnosis.
            eprintln!("caelum: could not spawn PTY cleanup thread: {e}");
        }
    }
}
const ANSI: [&str; 16] = [
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "brightBlack",
    "brightRed",
    "brightGreen",
    "brightYellow",
    "brightBlue",
    "brightMagenta",
    "brightCyan",
    "brightWhite",
];
fn index_color(i: usize, theme: &Theme) -> Color32 {
    if i < 16 {
        return theme.term(ANSI[i]);
    }
    if i < 232 {
        let n = i - 16;
        let level = |v| if v == 0 { 0 } else { 55 + 40 * v as u8 };
        return Color32::from_rgb(level(n / 36), level((n / 6) % 6), level(n % 6));
    }
    if i < 256 {
        let c = 8 + 10 * (i - 232) as u8;
        return Color32::from_gray(c);
    }
    match i {
        256 => theme.term("foreground"),
        257 => theme.term("background"),
        258 => theme.term("cursor"),
        _ => theme.term("foreground"),
    }
}
fn resolve_color(c: Color, theme: &Theme) -> Color32 {
    match c {
        Color::Spec(rgb) => Color32::from_rgb(rgb.r, rgb.g, rgb.b),
        Color::Indexed(i) => index_color(i as usize, theme),
        Color::Named(n) => match n {
            NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => {
                theme.term("foreground")
            }
            NamedColor::Background => theme.term("background"),
            NamedColor::Cursor => theme.term("cursor"),
            _ => index_color((n as usize) % 16, theme),
        },
    }
}
/// Xterm-style key sequences including application cursor mode and modifiers.
pub fn key_bytes(key: Key, m: Modifiers, mode: TermMode) -> Option<Vec<u8>> {
    let modifier = 1 + usize::from(m.shift) + 2 * usize::from(m.alt) + 4 * usize::from(m.ctrl);
    let seq: String = match key {
        Key::Enter => "\r".into(),
        Key::Tab => if m.shift { "\x1b[Z" } else { "\t" }.into(),
        Key::Backspace => if m.alt { "\x1b\x7f" } else { "\x7f" }.into(),
        Key::Escape => "\x1b".into(),
        Key::ArrowUp | Key::ArrowDown | Key::ArrowRight | Key::ArrowLeft | Key::Home | Key::End => {
            let c = match key {
                Key::ArrowUp => 'A',
                Key::ArrowDown => 'B',
                Key::ArrowRight => 'C',
                Key::ArrowLeft => 'D',
                Key::Home => 'H',
                _ => 'F',
            };
            if modifier > 1 {
                format!("\x1b[1;{modifier}{c}")
            } else if mode.contains(TermMode::APP_CURSOR) {
                format!("\x1bO{c}")
            } else {
                format!("\x1b[{c}")
            }
        }
        Key::Insert
        | Key::Delete
        | Key::PageUp
        | Key::PageDown
        | Key::F5
        | Key::F6
        | Key::F7
        | Key::F8
        | Key::F9
        | Key::F10
        | Key::F11
        | Key::F12 => {
            let n = match key {
                Key::Insert => 2,
                Key::Delete => 3,
                Key::PageUp => 5,
                Key::PageDown => 6,
                Key::F5 => 15,
                Key::F6 => 17,
                Key::F7 => 18,
                Key::F8 => 19,
                Key::F9 => 20,
                Key::F10 => 21,
                Key::F11 => 23,
                _ => 24,
            };
            if modifier > 1 {
                format!("\x1b[{n};{modifier}~")
            } else {
                format!("\x1b[{n}~")
            }
        }
        Key::F1 | Key::F2 | Key::F3 | Key::F4 => {
            let c = match key {
                Key::F1 => 'P',
                Key::F2 => 'Q',
                Key::F3 => 'R',
                _ => 'S',
            };
            if modifier > 1 {
                format!("\x1b[1;{modifier}{c}")
            } else {
                format!("\x1bO{c}")
            }
        }
        _ if m.ctrl => {
            let name = key.name();
            if name.len() == 1 {
                let c = name.as_bytes()[0].to_ascii_uppercase();
                if (b'@'..=b'_').contains(&c) {
                    return Some(vec![c & 0x1f]);
                }
            }
            if key == Key::Space {
                return Some(vec![0]);
            }
            return None;
        }
        _ => return None,
    };
    Some(seq.into_bytes())
}
/// Only web links from OSC 8 hyperlinks are ever opened.
fn openable_link(uri: &str) -> bool {
    uri.starts_with("https://") || uri.starts_with("http://")
}

/// Renders a hyperlink target for a tooltip: control characters are escaped so a
/// URI cannot hide its real destination behind newlines or bidi tricks, and very
/// long targets are shortened in the middle.
fn hover_label(uri: &str) -> String {
    let clean: String = uri
        .chars()
        .flat_map(|c| {
            if c.is_control() || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                c.escape_unicode().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect();
    let chars: Vec<char> = clean.chars().collect();
    if chars.len() > 300 {
        let head: String = chars[..200].iter().collect();
        let tail: String = chars[chars.len() - 80..].iter().collect();
        format!("{head}…{tail}")
    } else {
        clean
    }
}

/// Pixel geometry of the terminal grid for one frame.
#[derive(Clone, Copy)]
struct Metrics {
    /// Whole pane, including padding.
    rect: Rect,
    /// Padded area the cells are drawn in.
    area: Rect,
    /// Cell width and height in points.
    cw: f32,
    ch: f32,
    size: Size,
}
impl Metrics {
    /// Grid point under `pos`, clamped to the visible grid, for the given display offset.
    fn point_at(&self, pos: Pos2, offset: usize) -> Point {
        Point::new(
            Line(
                ((pos.y - self.area.top()) / self.ch)
                    .floor()
                    .clamp(0., self.size.rows as f32 - 1.) as i32
                    - offset as i32,
            ),
            Column(
                ((pos.x - self.area.left()) / self.cw)
                    .floor()
                    .clamp(0., self.size.cols as f32 - 1.) as usize,
            ),
        )
    }
    /// Grid point under `pos` for the terminal's current display offset, clamped to
    /// its grid. Reading the offset from the same locked `term` that is then indexed
    /// matters: the reader thread can switch to the alternate screen (which has no
    /// history) between two locks, and a stale scrolled-back offset would index past it.
    fn grid_point(&self, term: &Term<Events>, pos: Pos2) -> Point {
        let p = self.point_at(pos, term.grid().display_offset());
        Point::new(
            p.line.clamp(term.topmost_line(), term.bottommost_line()),
            p.column.min(term.last_column()),
        )
    }
    /// Top-left corner of the cell at grid `col`/screen `row`.
    fn cell_pos(&self, col: usize, row: i32) -> Pos2 {
        self.area.min + Vec2::new(col as f32 * self.cw, row as f32 * self.ch)
    }
    /// Zero-based screen column and row under `pos`, clamped to the grid.
    fn screen_cell(&self, pos: Pos2) -> (usize, usize) {
        let p = self.point_at(pos, 0);
        (p.column.0, p.line.0.max(0) as usize)
    }
    /// Grid point and cell half under `pos`, for selections. Past the left or right
    /// edge selects to the start or end of the row; above or below the grid, from the
    /// start of the top row or to the end of the bottom row.
    fn grid_side(&self, term: &Term<Events>, pos: Pos2) -> (Point, Side) {
        let point = self.grid_point(term, pos);
        if pos.y < self.area.top() {
            return (Point::new(point.line, Column(0)), Side::Left);
        }
        if pos.y >= self.area.top() + self.size.rows as f32 * self.ch {
            return (Point::new(point.line, term.last_column()), Side::Right);
        }
        let x = (pos.x - self.area.left()) / self.cw;
        let side = if x < 0. {
            Side::Left
        } else if x >= self.size.cols as f32 || x.fract() >= 0.5 {
            Side::Right
        } else {
            Side::Left
        };
        (point, side)
    }
}

/// Two presses this close together (and within a few points) count as a double
/// or triple click.
const MULTI_CLICK_SECONDS: f64 = 0.5;

/// Pointer state kept between frames.
#[derive(Default)]
struct Mouse {
    /// Presses in a row at about the same spot: 1, 2 (word) or 3 (line).
    clicks: u8,
    last_press: Option<(f64, Pos2)>,
    /// The selection gesture in progress while the primary button is held.
    gesture: Option<SelectionType>,
    /// Where a character or block drag started; the selection is created once the
    /// pointer leaves that cell half.
    anchor: Option<(Point, Side)>,
    /// Fractional lines of drag auto-scroll and of wheel scrolling not yet applied.
    autoscroll: f32,
    wheel: f32,
    /// Button whose press was reported to the program, until its release.
    reported: Option<u8>,
    /// Cell of the last motion report, so each cell is reported once.
    last_motion: Option<(usize, usize)>,
    /// Link under the pointer when the context menu opened.
    menu_link: Option<String>,
    menu_open: bool,
}

/// Terminal settings: Alacritty's defaults, with `:` dropped from the word
/// separators so a double click selects whole URLs, `host:port` and `file:line`.
fn term_config() -> Config {
    Config {
        semantic_escape_chars: ",│`|\"' ()[]{}<>\t".into(),
        // Programs may set the clipboard but never read it back.
        osc52: Osc52::OnlyCopy,
        ..Config::default()
    }
}

/// How pointer input is interpreted this frame.
#[derive(Clone, Copy)]
struct PointerMode {
    /// The program asked for mouse reports.
    reporting: bool,
    /// Pointer input selects text (no reporting, or Shift is held).
    selecting: bool,
}
impl PointerMode {
    fn forwards_to_program(self) -> bool {
        self.reporting && !self.selecting
    }
}

/// Font families for the terminal font's bold, italic and bold italic faces,
/// registered by the app only when the font has them.
pub const STYLED_FAMILIES: [&str; 3] = ["mono-bold", "mono-italic", "mono-bold-italic"];

/// The fonts cells are drawn with: regular, then the bold, italic and bold italic
/// faces where the font has them.
struct Faces {
    regular: FontId,
    styled: [Option<FontId>; 3],
}
impl Faces {
    fn new(ui: &egui::Ui, size: f32) -> Self {
        let families = ui.fonts(|f| f.families());
        let styled = STYLED_FAMILIES.map(|name| {
            let family = egui::FontFamily::Name(name.into());
            families
                .contains(&family)
                .then(|| FontId::new(size, family))
        });
        Self {
            regular: FontId::monospace(size),
            styled,
        }
    }
    /// The font for a cell, and whether bold must be faked (no bold face).
    fn pick(&self, bold: bool, italic: bool) -> (&FontId, bool) {
        let [b, i, bi] = &self.styled;
        match (bold, italic) {
            (true, true) => match (bi, i, b) {
                (Some(f), _, _) => (f, false),
                (None, Some(f), _) => (f, true),
                (None, None, Some(f)) => (f, false),
                (None, None, None) => (&self.regular, true),
            },
            (true, false) => b.as_ref().map_or((&self.regular, true), |f| (f, false)),
            (false, true) => (i.as_ref().unwrap_or(&self.regular), false),
            (false, false) => (&self.regular, false),
        }
    }
}

/// What a terminal pane asks of the app after a frame.
#[derive(Default)]
pub struct Shown {
    /// The pane was clicked, or a drag started on it: focus it.
    pub clicked: bool,
    /// A mouse selection just ended (or Select All ran) with this text, for the
    /// PRIMARY selection and copy-on-select.
    pub selected: Option<String>,
    /// A middle click asked to paste the PRIMARY selection.
    pub paste_primary: bool,
    /// A Ctrl+click on a local file or directory named in the output.
    pub open_file: Option<FileRef>,
}

pub fn show(
    ui: &mut egui::Ui,
    s: &mut Session,
    theme: &Theme,
    prefs: &Preferences,
    focused: bool,
    search: &str,
) -> Shown {
    let font = FontId::monospace(prefs.font_size);
    let (response, m) = layout(ui, s, theme, prefs, &font);
    s.metrics = Some(m);
    let pointer = pointer_mode(ui, s);
    // Not while a modal dialog (preferences, about, an alert) is over the window:
    // its keys, Tab included, are not the shell's.
    let blocked = !ui.memory(|mem| mem.allows_interaction(ui.layer_id()));
    if blocked {
        response.surrender_focus();
    } else {
        manage_focus(ui, &response, focused);
    }
    report_focus(ui, s, focused && !blocked);
    let mut shown = Shown {
        clicked: response.clicked() || response.drag_started(),
        ..Shown::default()
    };
    if pointer.forwards_to_program() {
        report_mouse(ui, s, &response, &m);
    } else {
        s.mouse.reported = None;
    }
    shown.selected = handle_selection(ui, s, &response, &m, pointer);
    shown.paste_primary = !pointer.forwards_to_program() && response.middle_clicked();
    let (link, open_file) = handle_links(ui, s, &response, &m, pointer);
    shown.open_file = open_file;
    handle_scroll(ui, s, &response, &m, pointer);
    if focused && !blocked && (response.has_focus() || !ui.ctx().wants_keyboard_input()) {
        handle_events(ui, s, prefs);
    }
    handle_drop(ui, s, &response, focused);
    let painter = ui.painter_at(m.rect);
    let faces = Faces::new(ui, prefs.font_size);
    paint_grid(
        ui, s, theme, prefs, &painter, &m, &faces, focused, search, link,
    );
    paint_exit_badge(s, theme, prefs, &painter, m.rect);
    paint_scroll_indicator(ui, s, theme, &painter, &response, m.rect);
    scroll_pill(ui, s, theme, m.rect);
    if ui.input(|i| !i.raw.hovered_files.is_empty()) && response.contains_pointer() {
        painter.rect_stroke(
            m.rect.shrink(1.),
            0.,
            Stroke::new(2_f32, theme.ui("accent")),
            egui::StrokeKind::Inside,
        );
    }
    context_menu(s, &response, &m, pointer, &mut shown);
    shown
}

/// Allocates the pane, paints its background, sizes the PTY to fit and drains
/// pending session events.
fn layout(
    ui: &mut egui::Ui,
    s: &mut Session,
    theme: &Theme,
    prefs: &Preferences,
    font: &FontId,
) -> (egui::Response, Metrics) {
    let cw = ui.fonts_mut(|f| f.glyph_width(font, 'M'));
    let ch = prefs.font_size * prefs.line_height;
    let (rect, response) = ui.allocate_exact_size(
        ui.available_size().max(Vec2::splat(1.)),
        Sense::click_and_drag(),
    );
    let area = rect.shrink(8.);
    ui.painter_at(rect)
        .rect_filled(rect, 0., theme.term("background"));
    s.resize((area.width() / cw) as usize, (area.height() / ch) as usize);
    s.set_osc52_write(prefs.osc52_write);
    s.poll(theme);
    s.track_unseen_output();
    let m = Metrics {
        rect,
        area,
        cw,
        ch,
        size: s.size,
    };
    (response, m)
}

fn pointer_mode(ui: &egui::Ui, s: &Session) -> PointerMode {
    let reporting = lock(&s.term).mode().intersects(TermMode::MOUSE_MODE);
    let selecting = !reporting || ui.input(|i| i.modifiers.shift);
    PointerMode {
        reporting,
        selecting,
    }
}

/// Mouse selection. A press clears the selection; dragging selects characters
/// (Ctrl+drag a block), a double click selects a word and a triple click a line,
/// and dragging after them extends by words or lines. Shift+click extends the
/// current selection. Dragging past the top or bottom edge scrolls the scrollback.
/// Returns the selected text when a gesture ends, for the PRIMARY selection.
///
/// Button and move events are replayed in order: a quick click can arrive with its
/// press and release in the same frame, and the last move with the release.
fn handle_selection(
    ui: &egui::Ui,
    s: &mut Session,
    response: &egui::Response,
    m: &Metrics,
    p: PointerMode,
) -> Option<String> {
    let (events, down, pos, time, dt) = ui.input(|i| {
        (
            i.events.clone(),
            i.pointer.primary_down(),
            i.pointer.interact_pos(),
            i.time,
            i.stable_dt,
        )
    });
    let ours =
        |pos: Pos2| m.rect.contains(pos) && owns_press(response, egui::PointerButton::Primary);
    let mut selected = None;
    for event in events {
        match event {
            egui::Event::PointerMoved(pos) if s.mouse.gesture.is_some() => {
                extend_gesture(s, m, pos, 0.);
            }
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers,
            } => {
                if (!p.reporting || modifiers.shift) && ours(pos) {
                    start_gesture(s, m, pos, time, modifiers);
                }
            }
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                ..
            } if s.mouse.gesture.is_some() => {
                extend_gesture(s, m, pos, 0.);
                selected = end_gesture(s);
            }
            _ => {}
        }
    }
    if s.mouse.gesture.is_some() {
        if !down {
            // The release never reached us (outside the window, say).
            selected = end_gesture(s);
        } else if let Some(pos) = pos
            && extend_gesture(s, m, pos, dt)
        {
            // Keep scrolling while the pointer rests past the edge.
            ui.ctx().request_repaint();
        }
    }
    selected
}

/// Whether egui gave this pane the press: the button is held on it, or was already
/// released on it in the same frame as a click or the end of a drag. Unlike
/// `contains_pointer`, this is false for a press on a widget on top of the pane,
/// such as a split divider's grab area, a menu or a dialog.
fn owns_press(response: &egui::Response, button: egui::PointerButton) -> bool {
    response.is_pointer_button_down_on()
        || response.clicked_by(button)
        || response.drag_stopped_by(button)
}

/// Ends the selection gesture; returns the selected text, if any.
fn end_gesture(s: &mut Session) -> Option<String> {
    s.mouse.gesture = None;
    s.mouse.anchor = None;
    s.mouse.autoscroll = 0.;
    s.selection().filter(|text| !text.is_empty())
}

/// A primary-button press on the grid: counts clicks and starts the gesture.
fn start_gesture(s: &mut Session, m: &Metrics, pos: Pos2, time: f64, mods: Modifiers) {
    let mouse = &mut s.mouse;
    let again = mouse
        .last_press
        .is_some_and(|(t, at)| time - t < MULTI_CLICK_SECONDS && at.distance(pos) < 4.);
    mouse.clicks = if again { mouse.clicks % 3 + 1 } else { 1 };
    mouse.last_press = Some((time, pos));
    let mut term = lock(&s.term);
    let (point, side) = m.grid_side(&term, pos);
    if mouse.clicks == 1
        && mods.shift
        && let Some(selection) = &mut term.selection
    {
        selection.update(point, side);
        mouse.gesture = Some(selection.ty);
        mouse.anchor = None;
        return;
    }
    let ty = match mouse.clicks {
        2 => SelectionType::Semantic,
        3 => SelectionType::Lines,
        _ if mods.ctrl || mods.command => SelectionType::Block,
        _ => SelectionType::Simple,
    };
    mouse.gesture = Some(ty);
    if matches!(ty, SelectionType::Simple | SelectionType::Block) {
        // Nothing is selected until the pointer moves off the press position.
        term.selection = None;
        mouse.anchor = Some((point, side));
    } else {
        term.selection = Some(Selection::new(ty, point, side));
        mouse.anchor = None;
    }
}

/// Moves the selection end to `pos`, scrolling when it is past the top or bottom
/// of the grid. Returns true while auto-scrolling.
fn extend_gesture(s: &mut Session, m: &Metrics, pos: Pos2, dt: f32) -> bool {
    let Some(ty) = s.mouse.gesture else {
        return false;
    };
    let mut term = lock(&s.term);
    let past = if pos.y < m.area.top() {
        m.area.top() - pos.y
    } else if pos.y > m.area.bottom() {
        m.area.bottom() - pos.y
    } else {
        0.
    };
    let scrolling = past != 0. && !term.mode().contains(TermMode::ALT_SCREEN);
    if scrolling {
        // About 15 lines a second at the edge, faster the further out.
        let rate = 15. * (1. + past.abs() / m.ch);
        s.mouse.autoscroll += dt.min(0.1) * rate * past.signum();
        let lines = s.mouse.autoscroll.trunc();
        s.mouse.autoscroll -= lines;
        if lines != 0. {
            term.scroll_display(Scroll::Delta(lines as i32));
        }
    } else {
        s.mouse.autoscroll = 0.;
    }
    let (point, side) = m.grid_side(&term, pos);
    if let Some(selection) = &mut term.selection {
        selection.update(point, side);
    } else if let Some((at, at_side)) = s.mouse.anchor
        && (at, at_side) != (point, side)
    {
        let mut selection = Selection::new(ty, at, at_side);
        selection.update(point, side);
        term.selection = Some(selection);
    }
    scrolling
}

/// Focus in/out reports (mode 1004), sent when the pane gains or loses focus.
fn report_focus(ui: &egui::Ui, s: &mut Session, focused: bool) {
    let active = focused && ui.input(|i| i.viewport().focused.unwrap_or(true));
    let before = s.focus_reported.replace(active);
    if before.is_some_and(|b| b != active) && lock(&s.term).mode().contains(TermMode::FOCUS_IN_OUT)
    {
        s.write(if active { b"\x1b[I" } else { b"\x1b[O" }.to_vec());
    }
}

/// Mouse button code for a report: left, middle, right.
fn button_code(button: egui::PointerButton) -> Option<u8> {
    match button {
        egui::PointerButton::Primary => Some(0),
        egui::PointerButton::Middle => Some(1),
        egui::PointerButton::Secondary => Some(2),
        _ => None,
    }
}

/// Modifier bits of a mouse report. Shift is never reported: it bypasses
/// reporting to select text instead.
fn modifier_bits(m: Modifiers) -> u8 {
    (if m.alt { 8 } else { 0 }) + (if m.ctrl { 16 } else { 0 })
}

/// One mouse report for `code` (button plus modifier and motion bits) at the
/// zero-based screen cell, in the encoding the program asked for: SGR (1006),
/// UTF-8 (1005) or the X10 default. None when the encoding cannot express it.
fn mouse_report(
    code: u8,
    col: usize,
    row: usize,
    pressed: bool,
    mode: TermMode,
) -> Option<Vec<u8>> {
    let (x, y) = (col + 1, row + 1);
    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if pressed { 'M' } else { 'm' };
        return Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes());
    }
    // Legacy encodings report every release as button 3.
    let code = if pressed { code } else { (code & !0b11) | 3 };
    let mut out = b"\x1b[M".to_vec();
    out.push(32 + code);
    for v in [x, y] {
        if mode.contains(TermMode::UTF8_MOUSE) {
            let c = char::from_u32(32 + u32::try_from(v).ok()?).filter(|_| v <= 2015)?;
            out.extend(c.encode_utf8(&mut [0; 4]).as_bytes());
        } else {
            out.push(32 + u8::try_from(v).ok().filter(|v| *v <= 223)?);
        }
    }
    Some(out)
}

/// Forwards presses, releases, drags and motion to a program that enabled mouse
/// reporting (1000 clicks, 1002 drags, 1003 all motion).
fn report_mouse(ui: &egui::Ui, s: &mut Session, response: &egui::Response, m: &Metrics) {
    let mode = *lock(&s.term).mode();
    let (events, mods) = ui.input(|i| (i.events.clone(), i.modifiers));
    for event in events {
        match event {
            egui::Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers,
            } => {
                let Some(code) = button_code(button) else {
                    continue;
                };
                if pressed {
                    if !m.rect.contains(pos) || !owns_press(response, button) {
                        continue;
                    }
                    s.mouse.reported = Some(code);
                } else if s.mouse.reported != Some(code) {
                    continue;
                } else {
                    s.mouse.reported = None;
                }
                let (col, row) = m.screen_cell(pos);
                s.mouse.last_motion = Some((col, row));
                let code = code + modifier_bits(modifiers);
                if let Some(bytes) = mouse_report(code, col, row, pressed, mode) {
                    s.write(bytes);
                }
            }
            egui::Event::PointerMoved(pos) => {
                let held = s.mouse.reported;
                let wanted = mode.contains(TermMode::MOUSE_MOTION)
                    || (held.is_some() && mode.contains(TermMode::MOUSE_DRAG));
                if !wanted || (held.is_none() && !(response.hovered() && m.rect.contains(pos))) {
                    continue;
                }
                let cell = m.screen_cell(pos);
                if s.mouse.last_motion == Some(cell) {
                    continue;
                }
                s.mouse.last_motion = Some(cell);
                let code = held.unwrap_or(3) + 32 + modifier_bits(mods);
                if let Some(bytes) = mouse_report(code, cell.0, cell.1, true, mode) {
                    s.write(bytes);
                }
            }
            _ => {}
        }
    }
}

/// Takes keyboard focus when the pane is the active one, and keeps Tab, arrows and
/// Escape for the terminal while it has focus.
fn manage_focus(ui: &egui::Ui, response: &egui::Response, focused: bool) {
    if focused && !ui.ctx().wants_keyboard_input() {
        response.request_focus();
    }
    if response.has_focus() {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
    }
    if response.clicked() || response.drag_started() {
        response.request_focus();
    }
}

/// URI of the OSC 8 hyperlink in the cell under `pos`, if any.
fn link_at(term: &Term<Events>, m: &Metrics, pos: Pos2) -> Option<String> {
    term.grid()[m.grid_point(term, pos)]
        .hyperlink()
        .map(|link| link.uri().to_string())
}

/// A local file or directory named in terminal text, as `path`, `path:line` or
/// `path:line:col` (compiler errors, `rg`, tracebacks), resolved against the
/// session's working directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRef {
    pub path: PathBuf,
    /// 1-based line and column to scroll to, when the text gave them.
    pub line: Option<usize>,
    pub col: Option<usize>,
    pub is_dir: bool,
}

/// A link under the pointer: an OSC 8 hyperlink, a URL written as plain text, or a
/// path to an existing local file, together with the cells it covers (to underline it).
struct Link {
    uri: String,
    span: Option<(Point, Point)>,
    file: Option<FileRef>,
}

/// `paths` is `Some` only when the user asked for file links (Ctrl held); without it
/// no path text is ever looked up, so plain hovering never costs a filesystem call.
fn link_under(
    term: &Term<Events>,
    m: &Metrics,
    pos: Pos2,
    paths: Option<&PathScan>,
) -> Option<Link> {
    if let Some(uri) = link_at(term, m, pos) {
        return Some(Link {
            uri,
            span: None,
            file: None,
        });
    }
    let point = m.grid_point(term, pos);
    if let Some((uri, start, end)) = url_at(term, point) {
        return Some(Link {
            uri,
            span: Some((start, end)),
            file: None,
        });
    }
    let (file, start, end) = path_at(term, point, paths?)?;
    Some(Link {
        uri: file.path.display().to_string(),
        span: Some((start, end)),
        file: Some(file),
    })
}

/// What a background lookup of path text is keyed by.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ProbeKey {
    cwd: PathBuf,
    home: PathBuf,
    text: String,
    dirs: bool,
}
/// `(path, is_dir)` of an existing file, or `None`.
type Probed = Option<(PathBuf, bool)>;
type Resolver = Arc<dyn Fn(&ProbeKey) -> Probed + Send + Sync>;
/// Result of asking the probe about a path.
#[derive(Debug, PartialEq, Eq)]
enum Probe {
    Ready(Probed),
    /// Not known yet; the lookup runs on a worker thread and repaints when done.
    Pending,
}
const PROBE_TTL: Duration = Duration::from_secs(3);
const PROBE_MAX_INFLIGHT: usize = 4;
const PROBE_MAX_CACHED: usize = 256;
#[derive(Default)]
struct ProbeState {
    done: std::collections::HashMap<ProbeKey, (Instant, Probed)>,
    inflight: std::collections::HashSet<ProbeKey>,
}
/// Answers "does this path text name an existing file?" without ever touching the
/// filesystem on the calling (UI) thread: lookups run on short-lived worker threads
/// (at most [`PROBE_MAX_INFLIGHT`], so a stalled NFS/FUSE mount can hold only a few
/// and the window never waits for it), and results are cached for a few seconds, per
/// (directory, text), so hovering the same token costs one lookup.
#[derive(Clone)]
struct PathProbe {
    resolver: Resolver,
    state: Arc<Mutex<ProbeState>>,
    wake: egui::Context,
}
impl PathProbe {
    fn new(wake: egui::Context) -> Self {
        Self::with_resolver(
            wake,
            Arc::new(|k: &ProbeKey| {
                let files = FileLookup {
                    cwd: &k.cwd,
                    home: k.home.clone(),
                    dirs: k.dirs,
                };
                resolve_file_ref(&k.text, None, None, &files).map(|f| (f.path, f.is_dir))
            }),
        )
    }
    fn with_resolver(wake: egui::Context, resolver: Resolver) -> Self {
        Self {
            resolver,
            state: Arc::default(),
            wake,
        }
    }
    /// Never blocks on the resolver. An expired cached answer is returned while a
    /// refresh runs, so a link does not flicker off every few seconds.
    fn lookup(&self, key: &ProbeKey) -> Probe {
        let mut st = lock(&self.state);
        let cached = st.done.get(key).cloned();
        if let Some((at, r)) = &cached
            && at.elapsed() < PROBE_TTL
        {
            return Probe::Ready(r.clone());
        }
        let stale = cached.map(|(_, r)| Probe::Ready(r));
        if st.inflight.contains(key) || st.inflight.len() >= PROBE_MAX_INFLIGHT {
            return stale.unwrap_or(Probe::Pending);
        }
        st.inflight.insert(key.clone());
        drop(st);
        let (resolver, state, wake, k) = (
            self.resolver.clone(),
            self.state.clone(),
            self.wake.clone(),
            key.clone(),
        );
        let spawned = thread::Builder::new()
            .name("link-probe".into())
            .spawn(move || {
                let result = resolver(&k);
                let mut st = lock(&state);
                st.inflight.remove(&k);
                if st.done.len() >= PROBE_MAX_CACHED {
                    st.done.clear();
                }
                st.done.insert(k, (Instant::now(), result));
                drop(st);
                wake.request_repaint();
            });
        if spawned.is_err() {
            lock(&self.state).inflight.remove(key);
        }
        stale.unwrap_or(Probe::Pending)
    }
}
/// Everything needed to turn path text under the pointer into a [`FileRef`].
struct PathScan<'a> {
    probe: &'a PathProbe,
    cwd: &'a Path,
    home: PathBuf,
    dirs: bool,
}
impl PathScan<'_> {
    fn resolve(&self, text: &str, line: Option<usize>, col: Option<usize>) -> Option<FileRef> {
        let key = ProbeKey {
            cwd: self.cwd.to_path_buf(),
            home: self.home.clone(),
            text: text.to_string(),
            dirs: self.dirs,
        };
        let Probe::Ready(Some((path, is_dir))) = self.probe.lookup(&key) else {
            return None;
        };
        Some(FileRef {
            path,
            line: line.map(|n| n.max(1)),
            col: col.map(|n| n.max(1)),
            is_dir,
        })
    }
}

/// What paths in terminal text are resolved against.
struct FileLookup<'a> {
    cwd: &'a Path,
    home: PathBuf,
    /// Whether a directory is worth linking (a file browser pane can show it).
    dirs: bool,
}

/// The cells of the (soft-wrapped) text around `point`, without wide-character
/// spacers, bounded so scanning a wrapped line every frame stays cheap.
fn text_cells(term: &Term<Events>, point: Point) -> Vec<(char, Point)> {
    use alacritty_terminal::term::cell::Flags;
    let grid = term.grid();
    let last = term.last_column();
    let wraps = |line: Line| grid[Point::new(line, last)].flags.contains(Flags::WRAPLINE);
    let (mut top, mut bottom) = (point.line, point.line);
    while top > term.topmost_line() && wraps(top - 1) && point.line - top < 16 {
        top -= 1;
    }
    while bottom < term.bottommost_line() && wraps(bottom) && bottom - point.line < 16 {
        bottom += 1;
    }
    let mut cells = Vec::new();
    for line in top.0..=bottom.0 {
        for col in 0..=last.0 {
            let p = Point::new(Line(line), Column(col));
            let cell = &grid[p];
            if !cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                cells.push((cell.c, p));
            }
        }
    }
    cells
}

/// The web URL written in the text at `point` (following wrapped lines), with its
/// first and last cells.
fn url_at(term: &Term<Events>, point: Point) -> Option<(String, Point, Point)> {
    let cells = text_cells(term, point);
    let at = cells.iter().rposition(|(_, p)| *p <= point)?;
    let chars: Vec<char> = cells.iter().map(|(c, _)| *c).collect();
    let (from, to) = find_urls(&chars)
        .into_iter()
        .find(|(from, to)| (*from..*to).contains(&at))?;
    Some((
        chars[from..to].iter().collect(),
        cells[from].1,
        cells[to - 1].1,
    ))
}

/// The existing local file written in the text at `point`, with its first and last cells.
fn path_at(term: &Term<Events>, point: Point, paths: &PathScan) -> Option<(FileRef, Point, Point)> {
    let cells = text_cells(term, point);
    let at = cells.iter().rposition(|(_, p)| *p <= point)?;
    let chars: Vec<char> = cells.iter().map(|(c, _)| *c).collect();
    // Only the token under the pointer is looked at (and statted), not the line.
    let found = find_path_tokens(&chars)
        .into_iter()
        .find(|t| (t.from..t.to).contains(&at))?;
    let file = paths.resolve(&found.text, found.line, found.col)?;
    Some((file, cells[found.from].1, cells[found.to - 1].1))
}

/// A candidate path in a run of text: its character range, the path text and any
/// line and column that followed it.
#[derive(Debug, PartialEq, Eq)]
struct PathToken {
    from: usize,
    to: usize,
    text: String,
    line: Option<usize>,
    col: Option<usize>,
}

/// Characters that end a path token: space, quotes, brackets and box drawing.
fn ends_path(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(
            c,
            '"' | '\''
                | '`'
                | '<'
                | '>'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '|'
                | ','
                | ';'
                | '│'
        )
}

/// Splits `text` into tokens that might name a file. The existence check, which
/// needs the disk and a working directory, is [`resolve_file_ref`]'s job.
fn find_path_tokens(text: &[char]) -> Vec<PathToken> {
    let mut found = Vec::new();
    let mut i = 0;
    while i < text.len() {
        if ends_path(text[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < text.len() && !ends_path(text[i]) {
            i += 1;
        }
        let mut end = i;
        // Prose punctuation after the path is not part of it.
        while end > start && matches!(text[end - 1], '.' | ':' | '!' | '?' | '*' | '=') {
            end -= 1;
        }
        let token: String = text[start..end].iter().collect();
        let Some((path, mut line, col, used)) = split_line_col(&token) else {
            continue;
        };
        let end = start + token[..used].chars().count();
        if token.starts_with("http://") || token.starts_with("https://") {
            continue;
        }
        if !looks_like_path(&path, line.is_some()) {
            continue;
        }
        // Python tracebacks: `File "x.py", line 12`.
        if line.is_none() && start > 0 && text[start - 1] == '"' {
            let rest: String = text[i..text.len().min(i + 24)].iter().collect();
            if let Some(n) = rest.strip_prefix("\", line ") {
                let digits: String = n.chars().take_while(char::is_ascii_digit).collect();
                line = digits.parse().ok();
            }
        }
        found.push(PathToken {
            from: start,
            to: end,
            text: path,
            line,
            col,
        });
    }
    found
}

/// `path`, `path:12` and `path:12:5` split into the path and its 1-based numbers,
/// and the length in bytes of the part of `token` they came from. Text after the
/// numbers (`path:12:text`, as `rg` prints) is not part of the link.
fn split_line_col(token: &str) -> Option<(String, Option<usize>, Option<usize>, usize)> {
    let number = |rest: &str| -> Option<(usize, usize)> {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let ends = rest[digits..].is_empty() || rest[digits..].starts_with(':');
        (digits > 0 && digits <= 9 && ends).then(|| (rest[..digits].parse().unwrap_or(0), digits))
    };
    for (i, _) in token.match_indices(':') {
        // A Windows drive letter (`C:\x`, `C:/x`) is part of the path.
        if i == 1 && matches!(token.as_bytes().get(2), Some(b'\\' | b'/')) {
            continue;
        }
        let Some((line, n)) = number(&token[i + 1..]) else {
            continue;
        };
        if i == 0 {
            return None;
        }
        let mut used = i + 1 + n;
        let mut col = None;
        if let Some(rest) = token[used..].strip_prefix(':')
            && let Some((c, n)) = number(rest)
        {
            col = Some(c);
            used += 1 + n;
        }
        return Some((token[..i].to_string(), Some(line), col, used));
    }
    (!token.is_empty()).then(|| (token.to_string(), None, None, token.len()))
}

/// Cheap filter before touching the disk: something with a separator, a dot, a
/// home prefix or an explicit line number, and not just dots.
fn looks_like_path(path: &str, has_line: bool) -> bool {
    if path.chars().all(|c| c == '.') || path.contains('\0') {
        return false;
    }
    has_line || path.starts_with('~') || path.contains(['/', '\\', '.'])
}

/// Resolves path text against the session's directory; `None` unless it exists
/// (and, for a directory, `files.dirs` allows it).
fn resolve_file_ref(
    text: &str,
    line: Option<usize>,
    col: Option<usize>,
    files: &FileLookup,
) -> Option<FileRef> {
    let path = if text == "~" {
        files.home.clone()
    } else if let Some(rest) = text.strip_prefix("~/") {
        files.home.join(rest)
    } else {
        let p = PathBuf::from(text);
        if p.is_absolute() {
            p
        } else {
            files.cwd.join(p)
        }
    };
    let meta = std::fs::metadata(&path).ok()?;
    if meta.is_dir() && !files.dirs {
        return None;
    }
    Some(FileRef {
        path,
        line: line.map(|n| n.max(1)),
        col: col.map(|n| n.max(1)),
        is_dir: meta.is_dir(),
    })
}

/// Character ranges of the `http://` and `https://` URLs in `text`. Trailing
/// punctuation and unbalanced closing brackets are left out, as in prose.
fn find_urls(text: &[char]) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let rest: String = text[i..text.len().min(i + 8)].iter().collect();
        let rest = rest.to_ascii_lowercase();
        let scheme = if rest.starts_with("https://") {
            8
        } else if rest.starts_with("http://") {
            7
        } else {
            0
        };
        if scheme == 0 || (i > 0 && text[i - 1].is_alphanumeric()) {
            i += 1;
            continue;
        }
        let mut end = i + scheme;
        while end < text.len()
            && !text[end].is_whitespace()
            && !text[end].is_control()
            && !matches!(
                text[end],
                '<' | '>' | '"' | '\'' | '`' | '│' | '{' | '}' | '\\' | '^'
            )
        {
            end += 1;
        }
        while let Some(&c) = text[i..end].last() {
            let unbalanced = |open: char| {
                let body = &text[i..end];
                body.iter().filter(|x| **x == c).count()
                    > body.iter().filter(|x| **x == open).count()
            };
            let trim = match c {
                '.' | ',' | ';' | ':' | '!' | '?' | '*' => true,
                ')' => unbalanced('('),
                ']' => unbalanced('['),
                _ => false,
            };
            if !trim {
                break;
            }
            end -= 1;
        }
        if end > i + scheme {
            found.push((i, end));
        }
        i = end.max(i + 1);
    }
    found
}

/// OSC 8 link text can differ from its target, so the real URI is always revealed
/// on hover before a Ctrl+click opens it. Plain-text URLs and paths to existing
/// local files are underlined on hover and open the same way. Returns the cells of
/// a hovered plain-text link and the file a Ctrl+click asked to open.
fn handle_links(
    ui: &egui::Ui,
    s: &Session,
    response: &egui::Response,
    m: &Metrics,
    p: PointerMode,
) -> (Option<(Point, Point)>, Option<FileRef>) {
    let mut span = None;
    let ctrl = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
    // File links are only looked for while Ctrl is held (the key that opens them).
    let paths = ctrl.then(|| PathScan {
        probe: &s.probe,
        cwd: &s.cwd,
        home: crate::model::home(),
        dirs: s.dir_links,
    });
    if let Some(pos) = response.hover_pos()
        && s.mouse.gesture.is_none()
    {
        let link = link_under(&lock(&s.term), m, pos, paths.as_ref());
        if let Some(link) = link {
            let tip = if link.file.is_some() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                "Ctrl+click to open".to_string()
            } else if openable_link(&link.uri) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                if link.span.is_some() {
                    "Ctrl+click to open".to_string()
                } else {
                    format!("{}\nCtrl+click to open", hover_label(&link.uri))
                }
            } else {
                format!(
                    "{}\n(not opened: only http and https links)",
                    hover_label(&link.uri)
                )
            };
            response.clone().on_hover_text_at_pointer(tip);
            span = link.span;
        } else if p.selecting {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }
    }
    let mut open = None;
    if response.clicked()
        && ctrl
        && let Some(pos) = response.interact_pointer_pos()
        && let Some(link) = link_under(&lock(&s.term), m, pos, paths.as_ref())
    {
        if link.file.is_some() {
            open = link.file;
        } else if openable_link(&link.uri) {
            ui.ctx().open_url(egui::OpenUrl::new_tab(link.uri));
        }
    }
    (span, open)
}

/// Wheel and touchpad scrolling: reported to a program that asked for mouse
/// events, sent as arrow keys on the alternate screen (`less`, `man`), and
/// otherwise moving through the scrollback, three lines per wheel notch.
fn handle_scroll(
    ui: &egui::Ui,
    s: &mut Session,
    response: &egui::Response,
    m: &Metrics,
    p: PointerMode,
) {
    if !response.hovered() {
        return;
    }
    let (events, hover) = ui.input(|i| (i.events.clone(), i.pointer.hover_pos()));
    let mut mods = Modifiers::NONE;
    let mut lines = 0.;
    for event in events {
        if let egui::Event::MouseWheel {
            unit,
            delta,
            modifiers,
        } = event
        {
            // Ctrl+wheel zooms the text.
            if modifiers.ctrl || modifiers.command {
                continue;
            }
            mods = modifiers;
            lines += match unit {
                egui::MouseWheelUnit::Line => delta.y * 3.,
                egui::MouseWheelUnit::Point => delta.y / m.ch,
                egui::MouseWheelUnit::Page => delta.y * m.size.rows as f32,
            };
        }
    }
    if lines == 0. {
        return;
    }
    s.mouse.wheel += lines;
    let whole = s.mouse.wheel.trunc();
    s.mouse.wheel -= whole;
    let n = whole as i32;
    if n == 0 {
        return;
    }
    let count = n.unsigned_abs().min(100) as usize;
    let mode = *lock(&s.term).mode();
    if p.forwards_to_program() {
        if let Some(pos) = hover {
            let (col, row) = m.screen_cell(pos);
            let code = if n > 0 { 64 } else { 65 } + modifier_bits(mods);
            if let Some(bytes) = mouse_report(code, col, row, true, mode) {
                s.write(bytes.repeat(count));
            }
        }
    } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
        let key = if n > 0 { 'A' } else { 'B' };
        let seq = if mode.contains(TermMode::APP_CURSOR) {
            format!("\x1bO{key}")
        } else {
            format!("\x1b[{key}")
        };
        s.write(seq.repeat(count).into_bytes());
    } else {
        lock(&s.term).scroll_display(Scroll::Delta(n));
    }
}

/// Translates this frame's keyboard, clipboard and IME events into PTY input.
fn handle_events(ui: &egui::Ui, s: &mut Session, prefs: &Preferences) {
    let mode = *lock(&s.term).mode();
    let events = ui.input(|i| i.events.clone());
    for event in events {
        match event {
            egui::Event::Text(text) => {
                if !ui.input(|i| i.modifiers.ctrl || i.modifiers.command) {
                    let text = if ui.input(|i| i.modifiers.alt) {
                        format!("\x1b{text}")
                    } else {
                        text
                    };
                    s.write(text.into_bytes());
                    lock(&s.term).scroll_display(Scroll::Bottom);
                }
            }
            egui::Event::Copy => {
                if ui.input(|i| i.modifiers.shift) {
                    if let Some(text) = s.selection() {
                        s.copy_to_clipboard(ui.ctx(), text);
                    }
                } else {
                    s.write(vec![3]);
                }
            }
            egui::Event::Cut => s.write(vec![24]),
            egui::Event::Paste(text) => {
                s.paste_or_hold(&text, prefs.confirm_multiline_paste);
            }
            // Ctrl+Insert copies and Shift+Insert pastes, as in GTK and Windows.
            egui::Event::Key {
                key: Key::Insert,
                pressed: true,
                modifiers,
                ..
            } if modifiers.ctrl != modifiers.shift && !modifiers.alt => {
                if modifiers.shift {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                } else if let Some(text) = s.selection() {
                    s.copy_to_clipboard(ui.ctx(), text);
                }
            }
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => handle_key(s, key, modifiers, mode),
            egui::Event::Ime(egui::ImeEvent::Commit(text)) => s.write(text.into_bytes()),
            _ => {}
        }
    }
}

/// Shift+PageUp/PageDown/Home/End scroll the scrollback; other keys go to the PTY.
fn handle_key(s: &mut Session, key: Key, modifiers: Modifiers, mode: TermMode) {
    if modifiers.shift
        && !modifiers.ctrl
        && matches!(key, Key::PageUp | Key::PageDown | Key::Home | Key::End)
    {
        lock(&s.term).scroll_display(match key {
            Key::PageUp => Scroll::PageUp,
            Key::PageDown => Scroll::PageDown,
            Key::Home => Scroll::Top,
            _ => Scroll::Bottom,
        });
        return;
    }
    if !(modifiers.ctrl && modifiers.shift)
        && let Some(bytes) = key_bytes(key, modifiers, mode)
    {
        s.write(bytes);
        lock(&s.term).scroll_display(Scroll::Bottom);
    }
}

/// Files dropped on the pane are typed as shell-quoted paths.
fn handle_drop(ui: &egui::Ui, s: &mut Session, response: &egui::Response, focused: bool) {
    let (files, hover) = ui.input(|i| (i.raw.dropped_files.clone(), i.pointer.hover_pos()));
    if files.is_empty() {
        return;
    }
    let here = match hover {
        Some(pos) => response.rect.contains(pos),
        None => focused,
    };
    let paths: Vec<String> = files
        .iter()
        .filter_map(|f| f.path.as_ref())
        .map(|p| shell_quote(&p.to_string_lossy()))
        .collect();
    if here && !paths.is_empty() {
        s.paste(&format!("{} ", paths.join(" ")));
    }
}

/// Quotes a path for the shell only when it needs it: POSIX single quotes, or on
/// Windows PowerShell's (the default shell there) literal single quotes, so `$`,
/// backticks and `$(...)` in a file name are never expanded.
fn shell_quote(path: &str) -> String {
    quote_path(path, cfg!(windows))
}
fn quote_path(path: &str, powershell: bool) -> String {
    // PowerShell also treats `,` `@` `%` and `=` specially, so it gets fewer bare characters.
    let bare = if powershell { "\\/._-:" } else { "/._-+,:@%=" };
    let plain = !path.is_empty()
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || bare.contains(c));
    if plain {
        path.to_string()
    } else if powershell {
        format!("'{}'", path.replace('\'', "''"))
    } else {
        format!("'{}'", path.replace('\'', r"'\''"))
    }
}

/// Start of the last search match, with the grid shape it was found in. Columns and
/// the alternate screen change what an absolute point refers to (and how many cells a
/// row has), so the origin is only reused while both are unchanged.
struct SearchOrigin {
    query: String,
    point: Point,
    columns: usize,
    alt_screen: bool,
}

/// Literal search from the last match (or the top of the view), selecting and
/// scrolling to the hit. `last` is updated to the new match.
fn search_term(
    term: &mut Term<Events>,
    last: &mut Option<SearchOrigin>,
    query: &str,
    forward: bool,
) -> bool {
    use alacritty_terminal::{index::Direction, term::search::RegexSearch};
    if query.is_empty() {
        return false;
    }
    let escaped: String = query
        .chars()
        .flat_map(|c| {
            if ".+*?()[]{}|^$\\".contains(c) {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect();
    let Ok(mut regex) = RegexSearch::new(&escaped) else {
        return false;
    };
    let alt_screen = term.mode().contains(TermMode::ALT_SCREEN);
    let previous = last
        .as_ref()
        .filter(|o| o.query == query && o.columns == term.columns() && o.alt_screen == alt_screen)
        .map(|o| o.point);
    let mut origin = match previous {
        // History can shrink (clear, resize) without the columns changing, so the
        // stored point is clamped into the grid before alacritty indexes with it.
        Some(p) => {
            let line = p.line.clamp(term.topmost_line(), term.bottommost_line());
            let column = p.column.min(term.last_column());
            let mut origin = Point::new(line, column);
            if forward {
                if origin.column.0 + 1 < term.columns() {
                    origin.column.0 += 1;
                } else {
                    origin.column = Column(0);
                    origin.line.0 += 1;
                    if origin.line > term.bottommost_line() {
                        origin.line = term.topmost_line();
                    }
                }
            } else if origin.column.0 > 0 {
                origin.column.0 -= 1;
            } else {
                origin.column = term.last_column();
                origin.line.0 -= 1;
                if origin.line < term.topmost_line() {
                    origin.line = term.bottommost_line();
                }
            }
            origin
        }
        None => Point::new(Line(-(term.grid().display_offset() as i32)), Column(0)),
    };
    origin.line = origin
        .line
        .clamp(term.topmost_line(), term.bottommost_line());
    let found = term.search_next(
        &mut regex,
        origin,
        if forward {
            Direction::Right
        } else {
            Direction::Left
        },
        Side::Left,
        None,
    );
    let Some(found) = found else {
        return false;
    };
    let start = *found.start();
    let end = *found.end();
    let mut selection = Selection::new(SelectionType::Simple, start, Side::Left);
    selection.update(end, Side::Right);
    term.selection = Some(selection);
    let target = (-start.line.0 + term.screen_lines() as i32 / 2).max(0);
    term.scroll_display(Scroll::Bottom);
    term.scroll_display(Scroll::Delta(target));
    *last = Some(SearchOrigin {
        query: query.to_string(),
        point: start,
        columns: term.columns(),
        alt_screen,
    });
    true
}

/// Cells (line, column) covered by literal matches of `search` on the visible screen.
fn search_cells(
    term: &Term<Events>,
    size: Size,
    offset: i32,
    search: &str,
) -> std::collections::HashSet<(i32, usize)> {
    use alacritty_terminal::term::cell::Flags;
    let mut cells = std::collections::HashSet::new();
    if search.is_empty() {
        return cells;
    }
    for row in 0..size.rows {
        let line = Line(row as i32 - offset);
        let mut text = String::new();
        let mut positions = Vec::new();
        for col in 0..size.cols {
            let point = Point::new(line, Column(col));
            let cell = &term.grid()[point];
            if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                positions.push((text.len(), point));
                text.push(cell.c);
            }
        }
        for (start, found) in text.match_indices(search) {
            for (byte, point) in &positions {
                if *byte >= start && *byte < start + found.len() {
                    cells.insert((point.line.0, point.column.0));
                }
            }
        }
    }
    cells
}

/// Paints every visible cell, search highlights and the cursor.
#[allow(clippy::too_many_arguments)]
fn paint_grid(
    ui: &egui::Ui,
    s: &Session,
    theme: &Theme,
    prefs: &Preferences,
    painter: &egui::Painter,
    m: &Metrics,
    faces: &Faces,
    focused: bool,
    search: &str,
    link: Option<(Point, Point)>,
) {
    let term = lock(&s.term);
    let content = term.renderable_content();
    let offset = content.display_offset as i32;
    let selection = content.selection;
    let cursor = content.cursor;
    let colors = content.colors;
    let highlighted = search_cells(&term, m.size, offset, search);
    for cell in content.display_iter {
        let point = cell.point;
        let row = point.line.0 + offset;
        if row < 0 || row >= m.size.rows as i32 {
            continue;
        }
        let selected = selection.is_some_and(|s| s.contains(point));
        let found = highlighted.contains(&(point.line.0, point.column.0));
        paint_cell(
            theme,
            painter,
            m,
            faces,
            colors,
            &cell,
            m.cell_pos(point.column.0, row),
            selected,
            found,
            prefs.min_contrast,
        );
    }
    // The focused pane of a focused window shows a solid (blinking) cursor; any
    // other terminal a hollow block, like VTE.
    let window_focused = ui.input(|i| i.viewport().focused.unwrap_or(true));
    let active = focused && window_focused;
    let show_cursor = term.mode().contains(TermMode::SHOW_CURSOR)
        && offset == 0
        && (!active
            || !prefs.cursor_blink
            || ui.input(|i| ((i.time * 2.) as u64).is_multiple_of(2)));
    let under = term.grid()[cursor.point].c;
    let last = term.last_column().0;
    drop(term);
    // Underline a hovered plain-text URL, row by row.
    if let Some((start, end)) = link {
        for line in start.line.0..=end.line.0 {
            let row = line + offset;
            if row < 0 || row >= m.size.rows as i32 {
                continue;
            }
            let from = if line == start.line.0 {
                start.column.0
            } else {
                0
            };
            let to = if line == end.line.0 {
                end.column.0
            } else {
                last
            };
            let left = m.cell_pos(from, row);
            let y = left.y + m.ch - 2.;
            painter.line_segment(
                [
                    Pos2::new(left.x, y),
                    Pos2::new(left.x + (to + 1 - from) as f32 * m.cw, y),
                ],
                Stroke::new(1_f32, theme.term("foreground")),
            );
        }
    }
    if show_cursor {
        let r = Rect::from_min_size(
            m.cell_pos(cursor.point.column.0, cursor.point.line.0),
            Vec2::new(m.cw, m.ch),
        );
        paint_cursor(theme, prefs, painter, r, active, under, &faces.regular);
    }
}

/// Paints one cell's background, glyph (with combining marks) and underline.
#[allow(clippy::too_many_arguments)]
fn paint_cell(
    theme: &Theme,
    painter: &egui::Painter,
    m: &Metrics,
    faces: &Faces,
    colors: &alacritty_terminal::term::color::Colors,
    cell: &alacritty_terminal::grid::Indexed<&alacritty_terminal::term::cell::Cell>,
    pos: Pos2,
    selected: bool,
    found: bool,
    min_contrast: f32,
) {
    use alacritty_terminal::term::cell::Flags;
    let (cw, ch) = (m.cw, m.ch);
    let cell_rect = Rect::from_min_size(pos, Vec2::new(cw, ch));
    let dynamic = |c: Color| {
        let index = match c {
            Color::Named(n) => Some(n as usize),
            Color::Indexed(n) => Some(n as usize),
            Color::Spec(_) => None,
        };
        index
            .and_then(|i| colors[i])
            .map(|rgb| Color32::from_rgb(rgb.r, rgb.g, rgb.b))
    };
    let mut fg = dynamic(cell.fg).unwrap_or_else(|| resolve_color(cell.fg, theme));
    let mut bg = dynamic(cell.bg).unwrap_or_else(|| resolve_color(cell.bg, theme));
    if cell.flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if selected {
        bg = theme.term("selectionBackground");
    }
    if found {
        bg = theme.ui("button");
    }
    if bg != theme.term("background") {
        painter.rect_filled(cell_rect, 0., bg);
    }
    if cell
        .flags
        .intersects(Flags::WIDE_CHAR_SPACER | Flags::HIDDEN)
    {
        return;
    }
    if cell.c != ' ' {
        // Text a program colored nearly the same as its background (dark blue on
        // black, say) is moved just far enough to be legible.
        fg = crate::theme::ensure_contrast(fg, bg, min_contrast);
    }
    if cell.flags.contains(Flags::DIM) {
        fg = fg.gamma_multiply(0.66);
    }
    if cell.c != ' ' {
        let mut text = cell.c.to_string();
        if let Some(extra) = cell.zerowidth() {
            text.extend(extra);
        }
        let (font, fake_bold) = faces.pick(
            cell.flags.contains(Flags::BOLD),
            cell.flags.contains(Flags::ITALIC),
        );
        let galley = painter.layout_no_wrap(text, font.clone(), fg);
        if fake_bold {
            // No bold face: overstrike one pixel to the right, like VTE.
            let px = 1. / painter.pixels_per_point();
            painter.galley(pos + Vec2::new(px, 0.), galley.clone(), fg);
        }
        painter.galley(pos, galley, fg);
    }
    let width = if cell.flags.contains(Flags::WIDE_CHAR) {
        2. * cw
    } else {
        cw
    };
    let line = |y: f32| {
        painter.line_segment(
            [Pos2::new(pos.x, y), Pos2::new(pos.x + width, y)],
            Stroke::new(1_f32, fg),
        );
    };
    if cell.flags.intersects(Flags::ALL_UNDERLINES) {
        line(pos.y + ch - 2.);
    }
    if cell.flags.contains(Flags::STRIKEOUT) {
        line(pos.y + ch / 2.);
    }
}

fn paint_cursor(
    theme: &Theme,
    prefs: &Preferences,
    painter: &egui::Painter,
    r: Rect,
    active: bool,
    under: char,
    font: &FontId,
) {
    let color = theme.term("cursor");
    if !active {
        painter.rect_stroke(r, 0., Stroke::new(1_f32, color), egui::StrokeKind::Inside);
        return;
    }
    match prefs.cursor_style.as_str() {
        "bar" => {
            painter.line_segment([r.left_top(), r.left_bottom()], Stroke::new(2_f32, color));
        }
        "underline" => {
            painter.line_segment(
                [r.left_bottom(), r.right_bottom()],
                Stroke::new(2_f32, color),
            );
        }
        _ => {
            // A solid block with the character under it in the background color.
            painter.rect_filled(r, 0., color);
            if !under.is_whitespace() {
                painter.text(
                    r.min,
                    egui::Align2::LEFT_TOP,
                    under,
                    font.clone(),
                    theme.term("background"),
                );
            }
        }
    }
}

/// An overlay scroll indicator like GTK's: a slim thumb at the right edge, shown
/// while the pointer is over the terminal or the view is scrolled back.
fn paint_scroll_indicator(
    ui: &egui::Ui,
    s: &Session,
    theme: &Theme,
    painter: &egui::Painter,
    response: &egui::Response,
    rect: Rect,
) {
    let (history, offset, rows) = {
        let term = lock(&s.term);
        let grid = term.grid();
        (
            grid.history_size(),
            grid.display_offset(),
            grid.screen_lines(),
        )
    };
    let scrolled = offset > 0;
    if history == 0 || !(scrolled || response.hovered()) {
        return;
    }
    let total = (history + rows) as f32;
    let track = rect.shrink2(Vec2::new(3., 4.));
    let height = (track.height() * rows as f32 / total)
        .max(24.)
        .min(track.height());
    let travel = track.height() - height;
    let top = track.top() + travel * (1. - offset as f32 / history as f32);
    let thumb = Rect::from_min_size(egui::pos2(track.right() - 4., top), Vec2::new(4., height))
        .round_to_pixels(ui.pixels_per_point());
    let alpha = if scrolled { 0.45 } else { 0.25 };
    painter.rect_filled(thumb, 2., theme.term("foreground").gamma_multiply(alpha));
}

const PILL_SIZE: Vec2 = Vec2::new(124., 24.);
/// Bottom-right of a pane, clear of the scroll indicator.
fn pill_rect(pane: Rect) -> Rect {
    Rect::from_min_size(
        Pos2::new(
            pane.right() - 16. - PILL_SIZE.x,
            pane.bottom() - 12. - PILL_SIZE.y,
        ),
        PILL_SIZE,
    )
}

/// A floating "Jump to bottom" pill, shown only while the view is scrolled back; a
/// click returns to the live output. It takes an accent outline when output has
/// arrived since the view left the bottom.
fn scroll_pill(ui: &egui::Ui, s: &mut Session, theme: &Theme, rect: Rect) {
    if !s.scrolled_back() {
        return;
    }
    let area = pill_rect(rect);
    let response = ui.interact(area, ui.id().with("scroll-pill"), Sense::click());
    let new_output = s.unseen_output;
    let accent = theme.ui("accent");
    let fg = theme.term("foreground");
    let painter = ui.painter_at(rect);
    let fill = crate::theme::mix(
        theme.term("background"),
        fg,
        if response.hovered() { 0.20 } else { 0.12 },
    );
    painter.rect_filled(area, 12., fill);
    let edge = if new_output {
        accent
    } else {
        fg.gamma_multiply(0.3)
    };
    painter.rect_stroke(
        area,
        12.,
        Stroke::new(if new_output { 1.5_f32 } else { 1. }, edge),
        egui::StrokeKind::Inside,
    );
    let ink = if new_output {
        accent
    } else {
        fg.gamma_multiply(0.85)
    };
    // A down chevron, then the label.
    let c = Pos2::new(area.left() + 16., area.center().y);
    let stroke = Stroke::new(1.6_f32, ink);
    painter.line_segment([c + Vec2::new(-4., -2.), c + Vec2::new(0., 2.)], stroke);
    painter.line_segment([c + Vec2::new(0., 2.), c + Vec2::new(4., -2.)], stroke);
    painter.text(
        Pos2::new(area.left() + 28., area.center().y),
        egui::Align2::LEFT_CENTER,
        "Jump to bottom",
        FontId::proportional(11.),
        ink,
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if response.clicked() {
        lock(&s.term).scroll_display(Scroll::Bottom);
        s.unseen_output = false;
        ui.ctx().request_repaint();
    }
}

/// Small "exit N" badge in the corner after a command reported a non-zero status.
fn paint_exit_badge(
    s: &Session,
    theme: &Theme,
    prefs: &Preferences,
    painter: &egui::Painter,
    rect: Rect,
) {
    let shell = lock(&s.shell);
    let Some(code) = shell.last_exit.filter(|c| *c != 0 && !shell.running) else {
        return;
    };
    let galley = painter.layout_no_wrap(
        format!("exit {code}"),
        FontId::monospace(prefs.font_size * 0.8),
        theme.ui("danger"),
    );
    let badge = Rect::from_min_size(
        Pos2::new(rect.right() - galley.size().x - 14., rect.top() + 4.),
        galley.size() + Vec2::new(10., 4.),
    );
    painter.rect_filled(badge, 4., theme.term("background").gamma_multiply(0.85));
    painter.rect_stroke(
        badge,
        4.,
        Stroke::new(1_f32, theme.ui("danger")),
        egui::StrokeKind::Inside,
    );
    painter.galley(badge.min + Vec2::new(5., 2.), galley, theme.ui("danger"));
}

/// Right-click menu. A program with mouse reporting gets right clicks itself;
/// Shift+right-click still opens the menu there.
fn context_menu(
    s: &mut Session,
    response: &egui::Response,
    m: &Metrics,
    p: PointerMode,
    shown: &mut Shown,
) {
    if p.forwards_to_program() && !s.mouse.menu_open {
        return;
    }
    if response.secondary_clicked() {
        // The menu only offers URLs, so path text is not looked up at all.
        s.mouse.menu_link = response
            .interact_pointer_pos()
            .and_then(|pos| link_under(&lock(&s.term), m, pos, None))
            .filter(|link| link.file.is_none())
            .map(|link| link.uri);
    }
    let mut open = false;
    response.context_menu(|ui| {
        open = true;
        shown.clicked = true;
        let selection = s.selection().filter(|t| !t.is_empty());
        if ui
            .add_enabled(selection.is_some(), egui::Button::new("Copy"))
            .clicked()
        {
            if let Some(text) = selection {
                s.copy_to_clipboard(ui.ctx(), text);
            }
            ui.close();
        }
        if ui.button("Paste").clicked() {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            ui.close();
        }
        if ui.button("Select All").clicked() {
            s.select_all();
            shown.selected = s.selection();
            ui.close();
        }
        if let Some(uri) = s.mouse.menu_link.clone() {
            ui.separator();
            if openable_link(&uri) && ui.button("Open Link").clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(&uri));
                ui.close();
            }
            if ui.button("Copy Link Address").clicked() {
                ui.ctx().copy_text(uri);
                ui.close();
            }
        }
        ui.separator();
        let has_output = lock(&s.shell).output.is_some();
        if ui
            .add_enabled(has_output, egui::Button::new("Copy Last Command Output"))
            .clicked()
        {
            if let Some(text) = s.last_output() {
                s.copy_to_clipboard(ui.ctx(), text);
            }
            ui.close();
        }
        let (history, offset) = {
            let term = lock(&s.term);
            (term.grid().history_size(), term.grid().display_offset())
        };
        if offset > 0 && ui.button("Scroll to Bottom").clicked() {
            lock(&s.term).scroll_display(Scroll::Bottom);
            ui.close();
        }
        if ui
            .add_enabled(history > 0, egui::Button::new("Clear Scrollback"))
            .clicked()
        {
            s.clear_scrollback();
            ui.close();
        }
    });
    s.mouse.menu_open = open;
}

/// Lock a mutex, recovering the guard if a previous holder panicked.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Runs `bytes` through the same scanner/parser/apply path as the reader thread.
    fn feed(
        term: &mut Term<Events>,
        shell: &mut ShellState,
        parser: &mut ansi::Processor,
        scanner: &mut OscScanner,
        bytes: &[u8],
    ) {
        let mut done = 0;
        for (end, event) in scanner.scan(bytes) {
            parser.advance(term, &bytes[done..end]);
            done = end;
            apply_osc(term, shell, event);
        }
        parser.advance(term, &bytes[done..]);
        reconcile_marks(term, shell);
    }
    fn shell_term(rows: usize) -> (Term<Events>, ShellState) {
        let (events, _queue) = Events::new();
        (
            Term::new(term_config(), &Size { cols: 20, rows }, events),
            ShellState::default(),
        )
    }
    #[test]
    fn last_output_is_the_text_between_osc_133_c_and_d() {
        let (mut term, mut shell) = shell_term(5);
        let (mut parser, mut scanner) = (ansi::Processor::new(), OscScanner::default());
        let mut go = |term: &mut Term<Events>, shell: &mut ShellState, s: &str| {
            feed(term, shell, &mut parser, &mut scanner, s.as_bytes());
        };
        go(&mut term, &mut shell, "\x1b]133;A\x07$ echo hi\r\n");
        go(&mut term, &mut shell, "\x1b]133;C\x07hi\r\nthere\r\n");
        go(&mut term, &mut shell, "\x1b]133;D;0\x07\x1b]133;A\x07$ ");
        let (s, e, c) = shell.output.expect("span recorded");
        assert_eq!(output_text(&term, s, e, c).as_deref(), Some("hi\nthere"));
        // The next command replaces it; output without a final newline keeps its last line.
        go(&mut term, &mut shell, "ls\r\n\x1b]133;C\x07a b\r\npartial");
        go(
            &mut term,
            &mut shell,
            "\x1b]133;D;1\x07\x1b]133;A\x07\r\n$ ",
        );
        let (s, e, c) = shell.output.unwrap();
        assert_eq!(output_text(&term, s, e, c).as_deref(), Some("a b\npartial"));
        assert_eq!(shell.last_exit, Some(1));
    }
    #[test]
    fn last_output_without_a_trailing_newline_excludes_the_next_prompt() {
        let (mut term, mut shell) = shell_term(5);
        let (mut parser, mut scanner) = (ansi::Processor::new(), OscScanner::default());
        let mut go = |term: &mut Term<Events>, shell: &mut ShellState, s: &str| {
            feed(term, shell, &mut parser, &mut scanner, s.as_bytes());
        };
        go(&mut term, &mut shell, "\x1b]133;A\x07$ printf foo\r\n");
        go(&mut term, &mut shell, "\x1b]133;C\x07foo");
        // The prompt is drawn right after the output, on the same line.
        go(&mut term, &mut shell, "\x1b]133;D;0\x07\x1b]133;A\x07$ ");
        let (s, e, c) = shell.output.expect("span recorded");
        assert_eq!(output_text(&term, s, e, c).as_deref(), Some("foo"));
    }
    /// The same, end to end: a real bash running `printf foo` with Caelum's own
    /// integration script, read back through the session API.
    #[cfg(unix)]
    #[test]
    fn real_shell_printf_without_newline_copies_only_its_output() {
        let Some(bash) = ["/bin/bash", "/usr/bin/bash"]
            .into_iter()
            .find(|p| Path::new(p).is_file())
        else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("shell/caelum.bash");
        let rc = dir.path().join("rc");
        std::fs::write(&rc, format!("PS1='PROMPT> '\n. '{}'\n", script.display())).unwrap();
        let argv: Vec<String> = [
            "/usr/bin/env".to_string(),
            "CAELUM_RS=1".into(),
            "HISTFILE=/dev/null".into(),
            bash.to_string(),
            "--noprofile".into(),
            "--rcfile".into(),
            rc.display().to_string(),
            "-i".into(),
        ]
        .to_vec();
        let mut s = Session::spawn(dir.path(), Some(&argv), egui::Context::default()).unwrap();
        let theme = crate::theme::presets().remove("starlight").unwrap();
        let wait = |s: &mut Session, what: &str, f: &dyn Fn(&Session) -> bool| {
            let end = Instant::now() + Duration::from_secs(10);
            while !f(s) {
                s.poll(&theme);
                assert!(Instant::now() < end, "timed out: {what}: {:?}", s.text());
                thread::sleep(Duration::from_millis(20));
            }
        };
        wait(&mut s, "prompt", &|s| s.text().contains("PROMPT>"));
        s.write(b"printf foo\r".to_vec());
        wait(&mut s, "output", &|s| s.last_output().is_some());
        wait(&mut s, "next prompt", &|s| {
            s.text().matches("PROMPT>").count() >= 2
        });
        assert_eq!(s.last_output().as_deref(), Some("foo"));
    }
    #[test]
    fn last_output_spans_scrollback_and_is_dropped_when_marks_are() {
        let (mut term, mut shell) = shell_term(5);
        let (mut parser, mut scanner) = (ansi::Processor::new(), OscScanner::default());
        let mut text = String::from("\x1b]133;C\x07");
        for i in 0..30 {
            text.push_str(&format!("line {i}\r\n"));
        }
        text.push_str("\x1b]133;D;0\x07$ ");
        feed(
            &mut term,
            &mut shell,
            &mut parser,
            &mut scanner,
            text.as_bytes(),
        );
        let (s, e, c) = shell.output.unwrap();
        assert_eq!(e - s, 30);
        let out = output_text(&term, s, e, c).unwrap();
        assert!(
            out.starts_with("line 0\n") && out.ends_with("line 29"),
            "{out}"
        );
        shell.clear_marks();
        assert_eq!(
            shell.output, None,
            "positions are invalid after a reflow or clear"
        );
    }
    #[test]
    fn empty_output_and_alternate_screen_record_no_span() {
        let (mut term, mut shell) = shell_term(5);
        let (mut parser, mut scanner) = (ansi::Processor::new(), OscScanner::default());
        feed(
            &mut term,
            &mut shell,
            &mut parser,
            &mut scanner,
            b"$ true\r\n\x1b]133;C\x07\x1b]133;D;0\x07",
        );
        assert_eq!(shell.output, None);
        feed(
            &mut term,
            &mut shell,
            &mut parser,
            &mut scanner,
            b"\x1b[?1049h\x1b]133;C\x07x\r\n\x1b]133;D;0\x07\x1b[?1049l",
        );
        assert_eq!(shell.output, None);
    }
    #[test]
    fn a_finished_command_reports_how_long_it_ran_once() {
        let (mut term, mut shell) = shell_term(5);
        let (mut parser, mut scanner) = (ansi::Processor::new(), OscScanner::default());
        feed(
            &mut term,
            &mut shell,
            &mut parser,
            &mut scanner,
            b"\x1b]133;C\x07",
        );
        std::thread::sleep(Duration::from_millis(30));
        feed(
            &mut term,
            &mut shell,
            &mut parser,
            &mut scanner,
            b"\x1b]133;D;7\x07",
        );
        let f = shell.finished.take().expect("a finished command");
        assert_eq!(f.code, Some(7));
        assert!(f.elapsed >= Duration::from_millis(30));
        assert!(shell.finished.is_none());
        // A D without a C (the very first prompt) is not a command.
        feed(
            &mut term,
            &mut shell,
            &mut parser,
            &mut scanner,
            b"\x1b]133;D;0\x07",
        );
        assert!(shell.finished.is_none());
    }
    #[test]
    fn multiline_paste_needs_confirmation_only_without_bracketed_paste() {
        // Single line, any mode: never asks.
        assert!(!needs_confirm("ls -la", false, true));
        assert!(!needs_confirm("ls -la", true, true));
        // Line breaks of every flavour ask when bracketed paste is off.
        for text in ["a\nb", "a\r\nb", "a\rb", "rm -rf ~\n"] {
            assert!(needs_confirm(text, false, true), "{text:?}");
            // Bracketed-paste programs receive it as text, so no prompt.
            assert!(!needs_confirm(text, true, true), "{text:?}");
            // "Don't ask again" turns the prompt off.
            assert!(!needs_confirm(text, false, false), "{text:?}");
        }
        // Decided on the sanitised text: stripped controls are not line breaks.
        assert!(!needs_confirm("a\x0bb\x0cc\u{85}d", false, true));
        assert!(!needs_confirm("", false, true));
    }
    #[test]
    fn paste_preview_counts_lines_and_escapes_controls() {
        assert_eq!(paste_preview("one\ntwo\n"), (2, "one\ntwo".into()));
        assert_eq!(paste_preview("a\r\nb\rc"), (3, "a\nb\nc".into()));
        let (n, preview) = paste_preview("x\ty\u{202e}z\n2\n3\n4\n5\n6\n7");
        assert_eq!(n, 7);
        let lines: Vec<&str> = preview.lines().collect();
        assert_eq!(lines.len(), 6, "{preview}");
        assert_eq!(lines[0], "x\\ty\\u{202e}z");
        assert_eq!(lines[5], "… 2 more line(s)");
        let (_, long) = paste_preview(&"a".repeat(500));
        assert_eq!(long.chars().count(), PREVIEW_LINE_CHARS + 1);
    }
    #[test]
    fn hyperlink_targets_are_shown_safely() {
        assert!(openable_link("https://example.org"));
        assert!(!openable_link("file:///etc/passwd"));
        assert!(!openable_link("javascript:alert(1)"));
        assert_eq!(hover_label("https://a.example/x"), "https://a.example/x");
        let tricky = hover_label("https://good.example\nhttps://evil.example\u{202e}");
        assert!(
            !tricky.contains('\n') && !tricky.contains('\u{202e}'),
            "{tricky}"
        );
        assert!(tricky.contains("evil.example"));
        let long = format!("https://x.example/{}", "a".repeat(1000));
        assert!(hover_label(&long).chars().count() < 300);
    }
    #[test]
    fn key_modes_and_control() {
        assert_eq!(
            key_bytes(Key::C, Modifiers::CTRL, TermMode::empty()).unwrap(),
            vec![3]
        );
        assert_eq!(
            key_bytes(Key::ArrowUp, Modifiers::NONE, TermMode::APP_CURSOR).unwrap(),
            b"\x1bOA"
        );
        assert_eq!(
            key_bytes(
                Key::ArrowLeft,
                Modifiers {
                    ctrl: true,
                    ..Modifiers::NONE
                },
                TermMode::empty()
            )
            .unwrap(),
            b"\x1b[1;5D"
        );
    }
    #[cfg(unix)]
    #[test]
    fn real_pty_decodes_colors_resizes_and_accepts_input() {
        let d = tempfile::tempdir().unwrap();
        let cmd = vec![
            "/usr/bin/env".into(),
            "HISTFILE=/dev/null".into(),
            "/bin/bash".into(),
            "--noprofile".into(),
            "--norc".into(),
        ];
        let mut s = Session::spawn(d.path(), Some(&cmd), egui::Context::default()).unwrap();
        s.resize(100, 30);
        s.write(b"printf '\\033[31mRUST_PTY_OK\\033[0m\\n'; stty size\r".to_vec());
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            if s.text().contains("30 100") && s.text().contains("RUST_PTY_OK") {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("{}", s.text());
    }
    #[cfg(unix)]
    #[test]
    fn missing_cwd_falls_back() {
        let s = Session::spawn(
            Path::new("/caelum-missing-directory"),
            Some(&["/bin/sh".into()]),
            egui::Context::default(),
        )
        .unwrap();
        assert_eq!(s.cwd, crate::model::home());
    }
    fn metrics(cols: usize, rows: usize) -> Metrics {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(cols as f32 * 10., rows as f32 * 20.));
        Metrics {
            rect,
            area: rect,
            cw: 10.,
            ch: 20.,
            size: Size { cols, rows },
        }
    }
    fn urls(text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        find_urls(&chars)
            .into_iter()
            .map(|(a, b)| chars[a..b].iter().collect())
            .collect()
    }
    #[test]
    fn plain_text_urls_leave_out_prose_punctuation() {
        assert_eq!(urls("see https://x.example/a."), ["https://x.example/a"]);
        assert_eq!(
            urls("(https://en.wikipedia.org/wiki/Rust_(language)), and"),
            ["https://en.wikipedia.org/wiki/Rust_(language)"]
        );
        assert_eq!(
            urls("<http://x.example/?q=1&b=2>"),
            ["http://x.example/?q=1&b=2"]
        );
        assert_eq!(
            urls("HTTPS://X.EXAMPLE 'http://y.example'"),
            ["HTTPS://X.EXAMPLE", "http://y.example"]
        );
        assert!(urls("xhttps://x.example http:// ftp://x.example").is_empty());
    }
    #[test]
    fn plain_text_urls_are_found_across_wrapped_lines() {
        let (events, _queue) = Events::new();
        let mut term = Term::new(term_config(), &Size { cols: 10, rows: 5 }, events);
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(&mut term, b"go https://x.example/long/path now");
        let on_second_row = Point::new(Line(1), Column(5));
        let (uri, start, end) = url_at(&term, on_second_row).unwrap();
        assert_eq!(uri, "https://x.example/long/path");
        assert_eq!(start, Point::new(Line(0), Column(3)));
        assert_eq!(end, Point::new(Line(2), Column(9)));
        assert!(url_at(&term, Point::new(Line(0), Column(0))).is_none());
    }
    #[test]
    fn mouse_reports_use_the_requested_encoding() {
        let sgr = TermMode::SGR_MOUSE;
        assert_eq!(mouse_report(0, 2, 4, true, sgr).unwrap(), b"\x1b[<0;3;5M");
        assert_eq!(
            mouse_report(16 + 32, 0, 0, true, sgr).unwrap(),
            b"\x1b[<48;1;1M"
        );
        assert_eq!(mouse_report(2, 9, 0, false, sgr).unwrap(), b"\x1b[<2;10;1m");
        // X10: 32 + code, 32 + x, 32 + y; a release is button 3.
        let none = TermMode::empty();
        assert_eq!(mouse_report(0, 0, 0, true, none).unwrap(), b"\x1b[M !!");
        assert_eq!(
            mouse_report(8 + 1, 0, 0, false, none).unwrap(),
            b"\x1b[M+!!"
        );
        assert_eq!(mouse_report(64, 1, 1, true, none).unwrap(), b"\x1b[M`\"\"");
        assert!(mouse_report(0, 300, 0, true, none).is_none());
        // UTF-8 (1005) carries larger coordinates.
        let utf8 = mouse_report(0, 300, 0, true, TermMode::UTF8_MOUSE).unwrap();
        assert_eq!(utf8, "\x1b[M \u{14d}!".as_bytes());
    }
    #[test]
    fn dropped_paths_are_quoted_for_the_shell() {
        let posix = |p| quote_path(p, false);
        assert_eq!(posix("/home/me/notes.txt"), "/home/me/notes.txt");
        assert_eq!(posix("/tmp/My File.txt"), "'/tmp/My File.txt'");
        assert_eq!(posix("/tmp/it's"), r"'/tmp/it'\''s'");
        assert_eq!(posix("/tmp/$(rm)`x`"), "'/tmp/$(rm)`x`'");
        // PowerShell: single-quoted literals, quotes doubled; `$(...)` stays text.
        let ps = |p| quote_path(p, true);
        assert_eq!(ps(r"C:\Users\me\notes.txt"), r"C:\Users\me\notes.txt");
        assert_eq!(
            ps(r"C:\drop\$(Write-Output INJECTED).txt"),
            r"'C:\drop\$(Write-Output INJECTED).txt'"
        );
        assert_eq!(ps(r"C:\it's,a@b.txt"), r"'C:\it''s,a@b.txt'");
    }
    #[test]
    fn selection_sides_follow_the_cell_halves() {
        let (events, _queue) = Events::new();
        let term = Term::new(term_config(), &Size { cols: 20, rows: 5 }, events);
        let m = metrics(20, 5);
        let p = |x: f32, y: f32| m.grid_side(&term, Pos2::new(x, y));
        assert_eq!(p(32., 5.), (Point::new(Line(0), Column(3)), Side::Left));
        assert_eq!(p(37., 5.), (Point::new(Line(0), Column(3)), Side::Right));
        assert_eq!(p(500., 5.), (Point::new(Line(0), Column(19)), Side::Right));
        assert_eq!(p(-5., 25.), (Point::new(Line(1), Column(0)), Side::Left));
        // Below the grid: to the end of the last row.
        assert_eq!(p(32., 500.), (Point::new(Line(4), Column(19)), Side::Right));
    }
    #[test]
    fn hover_uses_the_offset_of_the_grid_it_indexes() {
        let mut out = String::from("\x1b]8;;https://x.example/\x1b\\link\x1b]8;;\x1b\\\r\n");
        for i in 0..100 {
            out.push_str(&format!("line {i}\r\n"));
        }
        let (events, _queue) = Events::new();
        let mut term = Term::new(Config::default(), &Size { cols: 20, rows: 5 }, events);
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(&mut term, out.as_bytes());
        let m = metrics(20, 5);
        let top_left = Pos2::new(1., 1.);
        term.scroll_display(Scroll::Top);
        let offset = term.grid().display_offset();
        assert!(offset > 90);
        // Scrolled to the top, the link on the first line is under the pointer.
        assert_eq!(
            link_at(&term, &m, top_left).as_deref(),
            Some("https://x.example/")
        );
        // A program enters the alternate screen while the UI still holds the old
        // offset: that point is outside the alternate grid and used to be indexed.
        parser.advance(&mut term, b"\x1b[?1049h");
        assert!(m.point_at(top_left, offset).line < term.topmost_line());
        let p = m.grid_point(&term, top_left);
        assert!(p.line >= term.topmost_line() && p.line <= term.bottommost_line());
        assert_eq!(link_at(&term, &m, top_left), None);
    }
}

#[cfg(test)]
mod regressions {
    use super::*;
    use alacritty_terminal::term::cell::Flags;

    fn term_with(cols: usize, rows: usize, output: &str) -> Term<Events> {
        let (events, _queue) = Events::new();
        let mut term = Term::new(Config::default(), &Size { cols, rows }, events);
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(&mut term, output.as_bytes());
        term
    }
    fn feed(term: &mut Term<Events>, output: &str) {
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(term, output.as_bytes());
    }
    /// `needle` near the far right of a wide line deep in history, then filler.
    fn history_with_wide_match() -> String {
        let mut out = format!("{}needle\r\n", " ".repeat(110));
        for i in 0..200 {
            out.push_str(&format!("filler {i}\r\n"));
        }
        out
    }

    #[test]
    fn repeated_search_after_narrowing_does_not_index_past_the_row() {
        let mut term = term_with(120, 5, &history_with_wide_match());
        let mut origin = None;
        assert!(search_term(&mut term, &mut origin, "needle", false));
        assert_eq!(origin.as_ref().unwrap().point.column, Column(110));
        // Column 110 does not exist in an 80-column grid; this used to panic.
        term.resize(Size { cols: 80, rows: 5 });
        search_term(&mut term, &mut origin, "needle", false);
        search_term(&mut term, &mut origin, "needle", true);
    }
    #[test]
    fn repeated_search_on_the_alternate_screen_ignores_a_history_match() {
        let mut term = term_with(
            40,
            5,
            &history_with_wide_match().replace(&" ".repeat(110), ""),
        );
        let mut origin = None;
        assert!(search_term(&mut term, &mut origin, "needle", false));
        assert!(origin.as_ref().unwrap().point.line < Line(-100));
        // The alternate screen has no history, so Line(-200) is outside it; this used
        // to panic. The match is not on the alternate screen, so nothing is found.
        feed(&mut term, "\x1b[?1049h");
        assert!(!search_term(&mut term, &mut origin, "needle", true));
        assert!(!search_term(&mut term, &mut origin, "needle", false));
        // Back on the primary screen the history match is found again.
        feed(&mut term, "\x1b[?1049l");
        assert!(search_term(&mut term, &mut origin, "needle", false));
    }
    #[test]
    fn repeated_search_after_clearing_history_stays_in_the_grid() {
        let mut term = term_with(
            40,
            5,
            &history_with_wide_match().replace(&" ".repeat(110), ""),
        );
        let mut origin = None;
        assert!(search_term(&mut term, &mut origin, "needle", false));
        feed(&mut term, "\x1b[3J");
        assert_eq!(term.grid().history_size(), 0);
        assert!(!search_term(&mut term, &mut origin, "needle", true));
    }

    #[test]
    fn parser_preserves_truecolor_wide_cells_and_alternate_screen() {
        let (events, _queue) = Events::new();
        let mut term = Term::new(Config::default(), &Size { cols: 20, rows: 5 }, events);
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(&mut term, "\x1b[38;2;12;34;56m界\x1b[0m".as_bytes());
        let cell = &term.grid()[Point::new(Line(0), Column(0))];
        assert_eq!(cell.c, '界');
        assert_eq!(
            cell.fg,
            Color::Spec(ansi::Rgb {
                r: 12,
                g: 34,
                b: 56
            })
        );
        assert!(cell.flags.contains(Flags::WIDE_CHAR));
        assert!(
            term.grid()[Point::new(Line(0), Column(1))]
                .flags
                .contains(Flags::WIDE_CHAR_SPACER)
        );
        parser.advance(&mut term, b"\x1b[?1049h");
        assert!(term.mode().contains(TermMode::ALT_SCREEN));
        parser.advance(&mut term, b"\x1b[?1049l");
        assert_eq!(term.grid()[Point::new(Line(0), Column(0))].c, '界');
    }
    #[cfg(unix)]
    #[test]
    fn literal_search_reaches_history() {
        let argv = [
            "/usr/bin/env".into(),
            "HISTFILE=/dev/null".into(),
            "/bin/bash".into(),
            "--noprofile".into(),
            "--norc".into(),
        ];
        let mut s =
            Session::spawn(&crate::model::home(), Some(&argv), egui::Context::default()).unwrap();
        s.write(b"printf 'line-%s\\n' {1..100}\r".to_vec());
        let end = Instant::now() + Duration::from_secs(5);
        while !s.text().contains("line-100") && Instant::now() < end {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(s.search("line-5", true));
        assert!(s.term.lock().unwrap().grid().display_offset() > 0);
        assert_eq!(s.selection().as_deref(), Some("line-5"));
    }
    #[cfg(target_os = "linux")] // inspects /proc
    #[test]
    fn closing_session_stops_foreground_even_when_hangup_is_ignored() {
        let argv = [
            "/bin/bash".into(),
            "--noprofile".into(),
            "--norc".into(),
            "-c".into(),
            "trap '' HUP TERM; sleep 60 & wait".into(),
        ];
        let s =
            Session::spawn(&crate::model::home(), Some(&argv), egui::Context::default()).unwrap();
        let pid = s.pid().unwrap();
        let mut children = Vec::new();
        let end = Instant::now() + Duration::from_secs(2);
        while Instant::now() < end {
            children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
                .unwrap_or_default()
                .split_whitespace()
                .filter_map(|s| s.parse::<u32>().ok())
                .collect();
            if !children.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!children.is_empty());
        drop(s);
        // Cleanup runs on a background thread, so give it a moment to finish.
        let gone = |child: &u32| {
            let status =
                std::fs::read_to_string(format!("/proc/{child}/status")).unwrap_or_default();
            status.is_empty()
                || status
                    .lines()
                    .any(|l| l.starts_with("State:") && l.contains('Z'))
        };
        let end = Instant::now() + Duration::from_secs(5);
        while !children.iter().all(gone) && Instant::now() < end {
            thread::sleep(Duration::from_millis(10));
        }
        for child in children {
            assert!(gone(&child), "child {child} is still running");
        }
    }
    #[cfg(unix)]
    #[test]
    fn dropping_a_session_does_not_block_the_caller() {
        // Previously kill, reap, the /proc sweep and both thread joins ran here, on
        // the caller's (UI) thread. Rather than a tight wall-clock bound (a 50ms limit
        // flaked under CPU load), hold the cleanup thread on a gate and require that
        // `drop` returns while the child is still unreaped: if any blocking cleanup
        // (reap or thread join) moved back into `drop`, this deadlocks and the
        // recv_timeout below fails deterministically, independent of machine load.
        let argv = [
            "/bin/sh".into(),
            "-c".into(),
            "trap '' HUP TERM; while :; do sleep 1; done".into(),
        ];
        let mut s =
            Session::spawn(&crate::model::home(), Some(&argv), egui::Context::default()).unwrap();
        let pid = s.pid().unwrap();
        let (release, gate) = std::sync::mpsc::channel();
        s.cleanup_gate = Some(gate);
        thread::sleep(Duration::from_millis(100));
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            drop(s);
            let _ = done_tx.send(());
        });
        let returned = done_rx.recv_timeout(Duration::from_secs(10)).is_ok();
        // SAFETY: signal 0 only checks for existence; an unreaped zombie still exists.
        let unreaped = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
        let _ = release.send(());
        assert!(returned, "drop blocked on the cleanup thread");
        assert!(unreaped, "drop reaped {pid} before returning");
        // The process is still reaped in the background once cleanup runs.
        let end = Instant::now() + Duration::from_secs(5);
        // SAFETY: signal 0 only checks for existence.
        while unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 && Instant::now() < end {
            thread::sleep(Duration::from_millis(10));
        }
        assert_ne!(
            unsafe { libc::kill(pid as libc::pid_t, 0) },
            0,
            "{pid} survived"
        );
    }
    #[cfg(unix)]
    #[test]
    fn idle_wait_blocks_until_data_or_timeout() {
        use std::os::fd::{FromRawFd, OwnedFd};
        let mut fds = [0; 2];
        // SAFETY: pipe fills two fresh descriptors that we take ownership of below.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // SAFETY: both descriptors were just created and are owned by nobody else.
        let (r, mut w) = unsafe {
            (
                OwnedFd::from_raw_fd(fds[0]),
                std::fs::File::from(OwnedFd::from_raw_fd(fds[1])),
            )
        };
        // Idle: the reader sleeps in the kernel for the whole timeout, no spinning.
        let start = Instant::now();
        wait_readable(&r, Duration::from_millis(100));
        assert!(
            start.elapsed() >= Duration::from_millis(80),
            "{:?}",
            start.elapsed()
        );
        // Data available: it wakes immediately.
        w.write_all(b"x").unwrap();
        let start = Instant::now();
        wait_readable(&r, Duration::from_secs(5));
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "{:?}",
            start.elapsed()
        );
    }
    #[test]
    fn titles_are_sanitised() {
        assert_eq!(sanitize_title("vim ~/notes"), "vim ~/notes");
        assert_eq!(
            sanitize_title("a\x07b\x1b[31mc\u{9b}d\x7fe\nf\tg"),
            "ab[31mcdefg"
        );
        assert_eq!(
            sanitize_title("safe\u{202e}txt.exe\u{2066}\u{2069}\u{200e}\u{200f}\u{61c}\u{202a}"),
            "safetxt.exe"
        );
        let long = sanitize_title(&"é".repeat(500));
        assert_eq!(long.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(sanitize_title(" \x1b\u{202e} "), DEFAULT_TITLE);
    }
    #[test]
    fn title_events_are_coalesced_and_sanitised() {
        let (events, queue) = Events::new();
        for i in 0..10_000 {
            events.send_event(Event::Title(format!("t{i}\u{202e}\x07")));
        }
        events.send_event(Event::Wakeup);
        assert_eq!(lock(&queue.title).take().as_deref(), Some("t9999"));
        assert!(
            queue.rx.try_recv().is_err(),
            "titles and wakeups are not queued"
        );
        // Replies are bounded rather than unbounded when the UI is not polling.
        for _ in 0..EVENT_QUEUE * 3 {
            events.send_event(Event::PtyWrite("x".into()));
        }
        assert_eq!(queue.rx.try_iter().count(), EVENT_QUEUE);
    }
    #[test]
    fn paste_strips_controls_but_keeps_text_layout() {
        let payload = "echo hi\x1b[201~\x03\x04\x7f\u{9b}31m\x00\tok\r\nnext\n";
        assert_eq!(sanitize_paste(payload), "echo hi[201~31m\tok\r\nnext\n");
        assert_eq!(
            paste_bytes(payload, false),
            b"echo hi[201~31m\tok\rnext\r".to_vec()
        );
        assert_eq!(
            paste_bytes(payload, true),
            b"\x1b[200~echo hi[201~31m\tok\r\nnext\n\x1b[201~".to_vec()
        );
        assert_eq!(sanitize_paste("naïve 界 😀"), "naïve 界 😀");
    }
}

#[cfg(all(test, unix))]
mod input_queue_tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn burst_of_small_key_events_is_retained_under_backpressure() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("typed");
        let argv = [
            "/usr/bin/env".into(),
            "HISTFILE=/dev/null".into(),
            "/bin/bash".into(),
            "--noprofile".into(),
            "--norc".into(),
        ];
        let mut s = Session::spawn(dir.path(), Some(&argv), egui::Context::default()).unwrap();
        let text = "x".repeat(if cfg!(target_os = "linux") { 1000 } else { 300 });
        let command = format!("printf '%s' '{text}' > {}\r", file.display());
        for byte in command.bytes() {
            s.write(vec![byte]);
        }
        let theme = crate::theme::presets().remove("starlight").unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            s.poll(&theme);
            if std::fs::read_to_string(&file).ok().as_deref() == Some(&text) {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("Input was lost: {:?}", s.error);
    }
}

#[cfg(test)]
mod lock_shell_tests {
    use super::*;

    #[test]
    fn poisoned_mutex_still_usable() {
        let m = Arc::new(Mutex::new(5));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("poison");
        })
        .join();
        assert!(m.is_poisoned());
        *lock(&m) += 1;
        assert_eq!(*lock(&m), 6);
    }

    #[cfg(unix)]
    #[test]
    fn bad_shell_falls_back() {
        for bad in [
            None,
            Some(""),
            Some("bash"),
            Some("./sh"),
            Some("/nonexistent/shell"),
            Some("/tmp"),
        ] {
            assert_eq!(
                crate::platform::resolve_shell(bad),
                crate::platform::FALLBACK_SHELL,
                "{bad:?}"
            );
        }
        assert_eq!(crate::platform::resolve_shell(Some("/bin/sh")), "/bin/sh");
    }
}

#[cfg(test)]
mod ux_tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }
    fn token(text: &str) -> Vec<(String, Option<usize>, Option<usize>)> {
        find_path_tokens(&chars(text))
            .into_iter()
            .map(|t| (t.text, t.line, t.col))
            .collect()
    }
    fn t(
        path: &str,
        line: Option<usize>,
        col: Option<usize>,
    ) -> (String, Option<usize>, Option<usize>) {
        (path.to_string(), line, col)
    }

    #[test]
    fn path_tokens_carry_line_and_column() {
        assert_eq!(token("src/main.rs"), [t("src/main.rs", None, None)]);
        assert_eq!(token("src/main.rs:42"), [t("src/main.rs", Some(42), None)]);
        assert_eq!(
            token("src/main.rs:42:7"),
            [t("src/main.rs", Some(42), Some(7))]
        );
        // Compiler output.
        assert_eq!(
            token("  --> src/lib.rs:10:5"),
            [t("src/lib.rs", Some(10), Some(5))]
        );
        // `rg` and `grep -n` print `file:line:text`; the text is not part of the path.
        assert_eq!(
            token("src/a.rs:3:fn main() {"),
            [t("src/a.rs", Some(3), None)]
        );
        // Trailing prose punctuation and wrapping brackets are not part of it.
        assert_eq!(token("see ./a/b.txt."), [t("./a/b.txt", None, None)]);
        assert_eq!(token("(src/x.rs:9)"), [t("src/x.rs", Some(9), None)]);
        assert_eq!(token("src/x.rs:9:5:"), [t("src/x.rs", Some(9), Some(5))]);
        assert_eq!(token("'~/notes.md'"), [t("~/notes.md", None, None)]);
        // Python tracebacks name the line after the quoted path.
        assert_eq!(
            token("  File \"/app/run.py\", line 27, in <module>"),
            [t("/app/run.py", Some(27), None)]
        );
        // A bare name only counts with a line number, a dot or a separator.
        assert_eq!(token("Makefile:3"), [t("Makefile", Some(3), None)]);
        assert!(token("hello world error E0432: nope").is_empty());
        assert!(token("... .. :").is_empty());
        // URLs are the URL code's business.
        assert!(token("https://example.com/a.html:80").is_empty());
    }

    #[test]
    fn path_token_ranges_cover_only_the_path() {
        let text = chars("at src/a.rs:3:9 ok");
        let tokens = find_path_tokens(&text);
        assert_eq!(tokens.len(), 1);
        let got: String = text[tokens[0].from..tokens[0].to].iter().collect();
        assert_eq!(got, "src/a.rs:3:9");
    }

    fn key(text: &str) -> ProbeKey {
        ProbeKey {
            cwd: "/w".into(),
            home: "/h".into(),
            text: text.into(),
            dirs: false,
        }
    }

    /// A resolver stuck for a second (a dead NFS mount) must cost the caller nothing,
    /// run once per path however often it is asked, and be answered from the cache after.
    #[test]
    fn slow_path_lookup_never_blocks_the_caller_and_is_cached() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = calls.clone();
        let probe = PathProbe::with_resolver(
            egui::Context::default(),
            Arc::new(move |k: &ProbeKey| {
                c.fetch_add(1, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(600));
                Some((PathBuf::from(&k.text), false))
            }),
        );
        let started = Instant::now();
        for _ in 0..1000 {
            assert_eq!(probe.lookup(&key("a.rs")), Probe::Pending);
        }
        assert!(
            started.elapsed() < Duration::from_millis(300),
            "1000 frames took {:?} against a 600 ms resolver",
            started.elapsed()
        );
        let end = Instant::now() + Duration::from_secs(5);
        let got = loop {
            if let Probe::Ready(r) = probe.lookup(&key("a.rs")) {
                break r;
            }
            assert!(Instant::now() < end, "lookup never finished");
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(got, Some((PathBuf::from("a.rs"), false)));
        assert_eq!(probe.lookup(&key("a.rs")), Probe::Ready(got));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "one filesystem call per path"
        );
    }

    /// Many different hovered tokens cannot pile up threads behind a stalled mount.
    #[test]
    fn stalled_lookups_are_capped() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = calls.clone();
        let probe = PathProbe::with_resolver(
            egui::Context::default(),
            Arc::new(move |_: &ProbeKey| {
                c.fetch_add(1, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(500));
                None
            }),
        );
        for i in 0..50 {
            assert_eq!(probe.lookup(&key(&format!("f{i}"))), Probe::Pending);
        }
        thread::sleep(Duration::from_millis(100));
        assert!(calls.load(Ordering::SeqCst) <= PROBE_MAX_INFLIGHT);
    }

    /// The pane's link scan finds a real file only through the probe, with Ctrl-style
    /// opt-in: without a `PathScan`, no path is looked at.
    #[test]
    fn path_links_need_the_scan_and_resolve_through_the_probe() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "x").unwrap();
        let probe = PathProbe::new(egui::Context::default());
        let scan = PathScan {
            probe: &probe,
            cwd: dir.path(),
            home: dir.path().to_path_buf(),
            dirs: false,
        };
        let end = Instant::now() + Duration::from_secs(5);
        let file = loop {
            if let Some(f) = scan.resolve("main.rs", Some(4), None) {
                break f;
            }
            assert!(Instant::now() < end, "never resolved");
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            (file.path, file.line, file.is_dir),
            (dir.path().join("main.rs"), Some(4), false)
        );
        assert!(scan.resolve("absent.rs", None, None).is_none());
    }

    #[test]
    fn file_refs_resolve_against_the_working_directory_and_must_exist() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("notes.md"), "x").unwrap();
        let lookup = |dirs| FileLookup {
            cwd: dir.path(),
            home: home.path().to_path_buf(),
            dirs,
        };
        let f = resolve_file_ref("src/main.rs", Some(12), Some(3), &lookup(false)).unwrap();
        assert_eq!(f.path, dir.path().join("src/main.rs"));
        assert_eq!((f.line, f.col, f.is_dir), (Some(12), Some(3), false));
        // Absolute paths and `~` need no working directory.
        let abs = dir.path().join("src/main.rs");
        let f = resolve_file_ref(abs.to_str().unwrap(), None, None, &lookup(false)).unwrap();
        assert_eq!(f.path, abs);
        let f = resolve_file_ref("~/notes.md", None, None, &lookup(false)).unwrap();
        assert_eq!(f.path, home.path().join("notes.md"));
        // Line 0 is read as line 1.
        assert_eq!(
            resolve_file_ref("src/main.rs", Some(0), None, &lookup(false))
                .unwrap()
                .line,
            Some(1)
        );
        // Missing files are not links.
        assert!(resolve_file_ref("src/missing.rs", Some(1), None, &lookup(true)).is_none());
        // Directories only when something can show them.
        assert!(resolve_file_ref("src", None, None, &lookup(false)).is_none());
        let d = resolve_file_ref("src", None, None, &lookup(true)).unwrap();
        assert!(d.is_dir);
        // `..` resolves from the working directory too.
        let sub = FileLookup {
            cwd: &dir.path().join("src"),
            home: home.path().to_path_buf(),
            dirs: false,
        };
        assert!(resolve_file_ref("../src/main.rs", None, None, &sub).is_some());
    }

    #[test]
    fn osc52_cap_and_toggle() {
        assert_eq!(osc52_accepts(true, "hello"), Ok(true));
        assert_eq!(osc52_accepts(false, "hello"), Ok(false));
        assert_eq!(osc52_accepts(true, ""), Ok(false));
        let at_cap = "x".repeat(OSC52_MAX_BYTES);
        assert_eq!(osc52_accepts(true, &at_cap), Ok(true));
        let over = "x".repeat(OSC52_MAX_BYTES + 1);
        assert_eq!(osc52_accepts(true, &over), Err(OSC52_MAX_BYTES + 1));
        // A disabled toggle ignores even oversized text quietly.
        assert_eq!(osc52_accepts(false, &over), Ok(false));
    }

    #[test]
    fn only_enabled_clipboard_writes_are_kept_and_only_the_newest() {
        let (events, queue) = Events::new();
        let store = |t: &str| Event::ClipboardStore(ClipboardType::Clipboard, t.into());
        // Disabled (the initial state): nothing is kept, however big.
        events.send_event(store("nope"));
        events.send_event(store(&"x".repeat(OSC52_MAX_BYTES + 1)));
        assert!(lock(&queue.clipboard).is_none());
        queue.osc52.store(true, Ordering::Relaxed);
        events.send_event(store("first"));
        events.send_event(Event::ClipboardStore(
            ClipboardType::Selection,
            "primary".into(),
        ));
        events.send_event(Event::ClipboardLoad(
            ClipboardType::Clipboard,
            Arc::new(|text| text.to_string()),
        ));
        events.send_event(store("second"));
        assert_eq!(lock(&queue.clipboard).take(), Some(Ok("second".into())));
        // Nothing went through the bounded event channel at all.
        assert!(queue.rx.try_iter().next().is_none());
        // Oversized: only the size is kept, not the payload.
        let big = "y".repeat(OSC52_MAX_BYTES + 1);
        events.send_event(store(&big));
        assert_eq!(lock(&queue.clipboard).take(), Some(Err(big.len())));
    }

    #[test]
    fn a_flood_of_clipboard_writes_holds_one_payload() {
        let (events, queue) = Events::new();
        queue.osc52.store(true, Ordering::Relaxed);
        for i in 0..5000 {
            events.send_event(Event::ClipboardStore(
                ClipboardType::Clipboard,
                format!("write {i}"),
            ));
        }
        assert_eq!(lock(&queue.clipboard).take(), Some(Ok("write 4999".into())));
        assert!(queue.rx.try_iter().next().is_none());
    }

    /// A 6 MiB OSC 52 through the same guard + parser the reader thread uses is refused
    /// as too big, and a small one right after it still lands.
    #[test]
    fn oversized_osc52_is_bounded_before_decoding_and_refused() {
        let (events, queue) = Events::new();
        queue.osc52.store(true, Ordering::Relaxed);
        let mut term = Term::new(term_config(), &Size { cols: 20, rows: 5 }, events);
        let mut parser: ansi::Processor = ansi::Processor::new();
        let mut guard = OscGuard::new();
        let mut stream = b"\x1b]52;c;".to_vec();
        stream.extend(std::iter::repeat_n(b'A', 6 * 1024 * 1024));
        stream.extend(b"\x07");
        for chunk in stream.chunks(16384) {
            parser.advance(&mut term, &guard.filter(chunk));
        }
        match lock(&queue.clipboard).take() {
            Some(Err(n)) => assert!(n > OSC52_MAX_BYTES && n < 6 * 1024 * 1024, "{n}"),
            other => panic!("expected a refused oversize write, got {other:?}"),
        }
        parser.advance(&mut term, &guard.filter(b"\x1b]52;c;aGVsbG8=\x07"));
        assert_eq!(lock(&queue.clipboard).take(), Some(Ok("hello".into())));
    }

    /// The preference is read when output is parsed, not when a pane is drawn: a tab
    /// that is never drawn must stop honouring a setting that was switched off.
    #[cfg(unix)]
    #[test]
    fn background_session_honours_osc52_switched_off_before_its_output() {
        let dir = tempfile::tempdir().unwrap();
        let argv: Vec<String> = [
            "/bin/sh",
            "-c",
            "sleep 1; printf '\\033]52;c;aGVsbG8=\\007'; sleep 2",
        ]
        .map(String::from)
        .into();
        let mut s = Session::spawn_with(
            dir.path(),
            Some(&argv),
            egui::Context::default(),
            false,
            true,
        )
        .unwrap();
        s.set_osc52_write(false); // the user turned it off; this tab was never drawn
        let theme = crate::theme::presets().remove("starlight").unwrap();
        let end = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < end {
            s.poll(&theme);
            assert!(!s.take_copied(), "clipboard written despite the setting");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[cfg(unix)]
    fn run_osc52(text_b64: &str, enabled: bool) -> (bool, Option<String>) {
        let dir = tempfile::tempdir().unwrap();
        let argv: Vec<String> = [
            "/bin/sh",
            "-c",
            &format!("printf '\\033]52;c;{text_b64}\\007'; sleep 2"),
        ]
        .map(String::from)
        .into();
        let ctx = egui::Context::default();
        let mut s =
            Session::spawn_with(dir.path(), Some(&argv), ctx.clone(), false, enabled).unwrap();
        let theme = crate::theme::presets().remove("starlight").unwrap();
        let end = std::time::Instant::now() + Duration::from_secs(if enabled { 8 } else { 1 });
        let mut copied = false;
        while std::time::Instant::now() < end && !copied {
            s.poll(&theme);
            copied = s.take_copied();
            thread::sleep(Duration::from_millis(10));
        }
        (copied, s.error.take())
    }

    #[cfg(unix)]
    #[test]
    fn osc52_write_reaches_the_clipboard_and_toasts() {
        let (copied, error) = run_osc52("aGVsbG8=", true);
        assert!(copied, "OSC 52 write was not forwarded: {error:?}");
    }

    #[cfg(unix)]
    #[test]
    fn osc52_write_is_ignored_when_turned_off() {
        assert!(!run_osc52("aGVsbG8=", false).0);
    }
}
