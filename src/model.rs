use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub fn id() -> String {
    Uuid::new_v4().to_string()
}
pub use crate::platform::home;
/// Creates `path` (and parents); on unix new directories get mode 0700. When
/// `fix_existing` is set, an already-existing `path` is tightened to 0700 (best effort).
pub fn create_private_dir(path: &Path, fix_existing: bool) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        if fix_existing {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = fix_existing;
        fs::create_dir_all(path)?;
    }
    Ok(())
}
/// Accepts an SSH destination (`host`, `user@host`, `[v6]:port`) that cannot be
/// mistaken for an option: non-empty, no leading `-`, and only `[A-Za-z0-9_.@:\-\[\]]`.
/// Shared by the SSH dialog and workspace validation so both enforce the same rule.
pub fn valid_ssh_host(host: &str) -> bool {
    !host.is_empty()
        && !host.starts_with('-')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.@:-[]".contains(c))
}
/// Terminal panes may only launch the user's shell (`None`), `ssh -- <host>`,
/// `codex` or `claude`. Anything else in a workspace file is untrusted and must
/// never reach `Session::spawn`.
pub fn allowed_terminal_command(command: Option<&[String]>) -> bool {
    match command {
        None => true,
        Some([program]) => program == "codex" || program == "claude",
        Some([ssh, dashes, host]) => ssh == "ssh" && dashes == "--" && valid_ssh_host(host),
        Some(_) => false,
    }
}
/// Refuses a profile another user could have tampered with: on unix the profile
/// directory and any existing `workspace.json` / `drafts.json` must be owned by the
/// current user and not group/world-writable (and not symlinks). The directory is
/// tightened to 0700 first when we own it. No-op elsewhere.
pub fn check_profile_trust(profile: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // SAFETY: geteuid has no preconditions and cannot fail.
        let uid = unsafe { libc::geteuid() };
        let check = |path: &Path, meta: &fs::Metadata| -> Result<()> {
            if meta.uid() != uid {
                bail!(
                    "Refusing to load {}: it is owned by uid {}, not the current user (uid {uid})",
                    path.display(),
                    meta.uid()
                );
            }
            if meta.mode() & 0o022 != 0 {
                bail!(
                    "Refusing to load {}: it is group- or world-writable (mode {:o}); run chmod go-w on it",
                    path.display(),
                    meta.mode() & 0o777
                );
            }
            Ok(())
        };
        let dir = fs::symlink_metadata(profile)
            .with_context(|| format!("Reading {}", profile.display()))?;
        if dir.file_type().is_symlink() {
            // Follow the link but judge the real directory.
            let real = fs::metadata(profile)?;
            check(profile, &real)?;
        } else {
            if dir.uid() == uid && dir.mode() & 0o077 != 0 {
                fs::set_permissions(profile, fs::Permissions::from_mode(0o700))?;
            }
            check(profile, &fs::metadata(profile)?)?;
        }
        for name in ["workspace.json", "drafts.json"] {
            let path = profile.join(name);
            match fs::symlink_metadata(&path) {
                Ok(meta) => {
                    if meta.file_type().is_symlink() {
                        bail!("Refusing to load {}: it is a symlink", path.display());
                    }
                    check(&path, &meta)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("Reading {}", path.display())),
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = profile;
    }
    Ok(())
}
pub fn profile() -> PathBuf {
    std::env::var_os("CAELUM_RS_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(crate::platform::default_profile_dir)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Pane {
    Terminal {
        id: String,
        #[serde(default)]
        cwd: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<Vec<String>>,
    },
    Files {
        id: String,
        path: PathBuf,
    },
    Editor {
        id: String,
        path: PathBuf,
    },
    System {
        id: String,
    },
    Processes {
        id: String,
    },
    #[serde(alias = "web")]
    Browser {
        id: String,
        #[serde(default = "default_url")]
        url: String,
    },
    Split {
        id: String,
        axis: Axis,
        ratio: f32,
        first: Box<Pane>,
        second: Box<Pane>,
    },
}
fn default_url() -> String {
    "https://www.rust-lang.org".into()
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    Horizontal,
    Vertical,
}
/// Which side of the focused pane a new pane appears on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}
impl Direction {
    pub const ALL: [Direction; 4] = [Self::Up, Self::Left, Self::Right, Self::Down];
    pub fn axis(self) -> Axis {
        match self {
            Self::Left | Self::Right => Axis::Horizontal,
            Self::Up | Self::Down => Axis::Vertical,
        }
    }
    /// The new pane is the split's `first` child.
    pub fn before(self) -> bool {
        matches!(self, Self::Left | Self::Up)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Left => "Split left",
            Self::Right => "Split right",
            Self::Up => "Split up",
            Self::Down => "Split down",
        }
    }
}
/// Layouts nest at most this deep (also enforced when loading a workspace).
pub const MAX_DEPTH: usize = 32;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneSize {
    pub width: f32,
    pub height: f32,
}
/// The smallest a pane may become through splitting or dragging a divider.
pub const MIN_PANE: PaneSize = PaneSize {
    width: 160.,
    height: 96.,
};
/// A plain rectangle (x, y, width, height) so layout math stays testable without egui.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}
impl PaneRect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    /// The two halves of a split: `first` gets `ratio` of the space left after `gap`.
    pub fn split(self, axis: Axis, ratio: f32, gap: f32) -> (Self, Self) {
        let length = match axis {
            Axis::Horizontal => self.w,
            Axis::Vertical => self.h,
        };
        // Each side gives up half the gap, so nested equal shares stay equal.
        let a = (length * ratio - gap / 2.)
            .round()
            .clamp(0., (length - gap).max(0.));
        let b = (length - gap - a).max(0.);
        match axis {
            Axis::Horizontal => (
                Self::new(self.x, self.y, a, self.h),
                Self::new(self.x + a + gap, self.y, b, self.h),
            ),
            Axis::Vertical => (
                Self::new(self.x, self.y, self.w, a),
                Self::new(self.x, self.y + a + gap, self.w, b),
            ),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitOutcome {
    Split,
    /// The split only fit after balancing every pane in the tab.
    Rebalanced,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitError {
    NotFound,
    /// A pane would end up smaller than [`MIN_PANE`].
    NoRoom,
}
impl Pane {
    pub fn terminal(cwd: PathBuf) -> Self {
        Self::Terminal {
            id: id(),
            cwd,
            command: None,
        }
    }
    pub fn id(&self) -> &str {
        match self {
            Self::Terminal { id, .. }
            | Self::Files { id, .. }
            | Self::Editor { id, .. }
            | Self::System { id }
            | Self::Processes { id }
            | Self::Browser { id, .. }
            | Self::Split { id, .. } => id,
        }
    }
    pub fn leaves<'a>(&'a self, out: &mut Vec<&'a Pane>) {
        match self {
            Self::Split { first, second, .. } => {
                first.leaves(out);
                second.leaves(out);
            }
            _ => out.push(self),
        }
    }
    /// Splits `target` along `axis`, putting `new` after it (right / below).
    pub fn split(&mut self, target: &str, new: Pane, axis: Axis) -> bool {
        let dir = match axis {
            Axis::Horizontal => Direction::Right,
            Axis::Vertical => Direction::Down,
        };
        self.split_toward(target, new, dir)
    }
    /// Splits `target`, placing `new` on the `dir` side of it, then gives every tile
    /// in the same-direction run the new pane joined an equal share (splitting right
    /// four times yields five equal columns). Returns false if `target` is not a leaf here.
    pub fn split_toward(&mut self, target: &str, new: Pane, dir: Direction) -> bool {
        let mut new = Some(new);
        self.insert(target, &mut new, dir).is_some()
    }
    /// Returns `None` if `target` was not found, else whether this node still belongs
    /// to the run of `dir.axis()` splits that starts at the inserted split.
    fn insert(&mut self, target: &str, new: &mut Option<Pane>, dir: Direction) -> Option<bool> {
        if self.id() == target {
            if matches!(self, Self::Split { .. }) {
                return None;
            }
            let new = Box::new(new.take()?);
            let old = Box::new(std::mem::replace(self, Self::System { id: String::new() }));
            let (first, second) = if dir.before() { (new, old) } else { (old, new) };
            *self = Self::Split {
                id: id(),
                axis: dir.axis(),
                ratio: 0.5,
                first,
                second,
            };
            return Some(true);
        }
        let Self::Split {
            axis,
            first,
            second,
            ..
        } = self
        else {
            return None;
        };
        let axis = *axis;
        let open = first
            .insert(target, new, dir)
            .or_else(|| second.insert(target, new, dir))?;
        if open && axis == dir.axis() {
            self.equalize_run(axis);
            Some(true)
        } else {
            Some(false)
        }
    }
    /// How many tiles this subtree shows side by side along `axis` (a nested split on
    /// the other axis counts as one tile).
    fn weight(&self, axis: Axis) -> f32 {
        match self {
            Self::Split {
                axis: a,
                first,
                second,
                ..
            } if *a == axis => first.weight(axis) + second.weight(axis),
            _ => 1.,
        }
    }
    /// Equal shares for the run of `axis` splits rooted here; other-axis subtrees keep
    /// their own ratios.
    fn equalize_run(&mut self, axis: Axis) {
        if let Self::Split {
            axis: a,
            ratio,
            first,
            second,
            ..
        } = self
            && *a == axis
        {
            let (f, s) = (first.weight(axis), second.weight(axis));
            *ratio = f / (f + s);
            first.equalize_run(axis);
            second.equalize_run(axis);
        }
    }
    /// Balances every split in the tree: tiles that sit side by side in the same
    /// direction all get the same size.
    pub fn equalize(&mut self) {
        if let Self::Split {
            axis,
            ratio,
            first,
            second,
            ..
        } = self
        {
            let (f, s) = (first.weight(*axis), second.weight(*axis));
            *ratio = f / (f + s);
            first.equalize();
            second.equalize();
        }
    }
    pub fn depth(&self) -> usize {
        match self {
            Self::Split { first, second, .. } => 1 + first.depth().max(second.depth()),
            _ => 0,
        }
    }
    /// The smallest size this subtree can be laid out in along `axis` while every
    /// leaf keeps at least `min` and splits are `gap` apart.
    pub fn min_extent(&self, axis: Axis, min: PaneSize, gap: f32) -> f32 {
        match self {
            Self::Split {
                axis: a,
                first,
                second,
                ..
            } => {
                let (f, s) = (
                    first.min_extent(axis, min, gap),
                    second.min_extent(axis, min, gap),
                );
                if *a == axis { f + gap + s } else { f.max(s) }
            }
            _ => match axis {
                Axis::Horizontal => min.width,
                Axis::Vertical => min.height,
            },
        }
    }
    /// The rectangle of every leaf when this tree fills `area`.
    pub fn leaf_rects(&self, area: PaneRect, gap: f32) -> Vec<(String, PaneRect)> {
        let mut out = vec![];
        self.collect_rects(area, gap, &mut out);
        out
    }
    fn collect_rects(&self, area: PaneRect, gap: f32, out: &mut Vec<(String, PaneRect)>) {
        match self {
            Self::Split {
                axis,
                ratio,
                first,
                second,
                ..
            } => {
                let (a, b) = area.split(*axis, *ratio, gap);
                first.collect_rects(a, gap, out);
                second.collect_rects(b, gap, out);
            }
            _ => out.push((self.id().to_string(), area)),
        }
    }
    /// Whether every leaf is at least `min` when the tree fills `area`.
    pub fn fits(&self, area: PaneRect, min: PaneSize, gap: f32) -> bool {
        self.leaf_rects(area, gap)
            .iter()
            .all(|(_, r)| r.w + 0.5 >= min.width && r.h + 0.5 >= min.height)
    }
    /// Splits like [`Pane::split_toward`] but refuses when the result would leave a
    /// pane smaller than `min` in `area` (or nest too deeply). If the split only fits
    /// once the whole tree is balanced, it balances instead of refusing.
    pub fn try_split(
        &mut self,
        target: &str,
        new: Pane,
        dir: Direction,
        area: PaneRect,
        min: PaneSize,
        gap: f32,
    ) -> Result<SplitOutcome, SplitError> {
        let mut next = self.clone();
        if !next.split_toward(target, new, dir) {
            return Err(SplitError::NotFound);
        }
        if next.depth() > MAX_DEPTH {
            return Err(SplitError::NoRoom);
        }
        if next.fits(area, min, gap) {
            *self = next;
            return Ok(SplitOutcome::Split);
        }
        next.equalize();
        if next.fits(area, min, gap) {
            *self = next;
            return Ok(SplitOutcome::Rebalanced);
        }
        Err(SplitError::NoRoom)
    }
    pub fn remove(&mut self, target: &str) -> bool {
        if let Self::Split { first, second, .. } = self {
            if first.id() == target {
                *self = *second.clone();
                return true;
            }
            if second.id() == target {
                *self = *first.clone();
                return true;
            }
            return first.remove(target) || second.remove(target);
        }
        false
    }
    pub fn find(&self, target: &str) -> Option<&Pane> {
        if self.id() == target {
            return Some(self);
        }
        if let Self::Split { first, second, .. } = self {
            first.find(target).or_else(|| second.find(target))
        } else {
            None
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tab {
    pub id: String,
    pub name: String,
    pub layout: Pane,
}
impl Tab {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            id: id(),
            name: "Terminal".into(),
            layout: Pane::terminal(cwd),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub tabs: Vec<Tab>,
    pub active_tab: String,
}
impl Workspace {
    pub fn new(name: impl Into<String>, cwd: PathBuf) -> Self {
        let t = Tab::new(cwd);
        Self {
            id: id(),
            name: name.into(),
            active_tab: t.id.clone(),
            tabs: vec![t],
        }
    }
    pub fn active_index(&self) -> usize {
        self.tabs
            .iter()
            .position(|t| t.id == self.active_tab)
            .unwrap_or(0)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Preferences {
    /// Use Adwaita light/dark with the desktop's accent instead of `theme`. Profiles
    /// saved before this existed lack the key and so start following the desktop.
    pub follow_system: bool,
    pub theme: String,
    pub font_size: f32,
    pub terminal_font: String,
    pub line_height: f32,
    pub cursor_style: String,
    pub cursor_blink: bool,
    /// Whether the workspace sidebar is shown; hidden by default for a minimal window.
    pub sidebar: bool,
    /// Ask before a multi-line paste reaches a program without bracketed paste.
    pub confirm_multiline_paste: bool,
    pub custom_themes: Vec<crate::theme::Theme>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            follow_system: true,
            theme: "adwaita-dark".into(),
            font_size: 15.,
            terminal_font: String::new(),
            line_height: 1.2,
            cursor_style: "block".into(),
            cursor_blink: false,
            sidebar: false,
            confirm_multiline_paste: true,
            custom_themes: vec![],
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub version: u32,
    pub workspaces: Vec<Workspace>,
    pub active_workspace: String,
    #[serde(default)]
    pub preferences: Preferences,
    /// Non-fatal problems found while validating (e.g. a dropped terminal command).
    #[serde(skip)]
    pub warnings: Vec<String>,
}
impl Default for State {
    fn default() -> Self {
        let mut w = Workspace::new("Mission Control", home());
        let target = w.tabs[0].layout.id().to_string();
        w.tabs[0].layout.split(
            &target,
            Pane::Files {
                id: id(),
                path: home(),
            },
            Axis::Horizontal,
        );
        Self {
            version: 1,
            active_workspace: w.id.clone(),
            workspaces: vec![w],
            preferences: Preferences::default(),
            warnings: vec![],
        }
    }
}
impl State {
    pub fn active_index(&self) -> usize {
        self.workspaces
            .iter()
            .position(|w| w.id == self.active_workspace)
            .unwrap_or(0)
    }
    pub fn validate(&mut self) -> Result<()> {
        if self.version != 1 {
            bail!("Unsupported workspace version {}", self.version);
        }
        if self.workspaces.is_empty() {
            bail!("Workspace file contains no workspaces");
        }
        let mut ids = HashSet::new();
        let mut warnings = vec![];
        fn node(
            p: &mut Pane,
            ids: &mut HashSet<String>,
            warnings: &mut Vec<String>,
            depth: usize,
        ) -> Result<()> {
            if depth > MAX_DEPTH {
                bail!("Workspace layout is too deeply nested");
            }
            if !ids.insert(p.id().to_string()) {
                bail!("Duplicate workspace identifier");
            }
            if let Pane::Terminal { id, command, .. } = p
                && !allowed_terminal_command(command.as_deref())
            {
                // Drop rather than reject so an existing profile still opens; the pane
                // falls back to the user's shell and the command is never executed.
                let warning = format!(
                    "Terminal pane {id} had an unsupported launch command; it will open your shell instead"
                );
                eprintln!("caelum-rs: {warning}");
                warnings.push(warning);
                *command = None;
            }
            if let Pane::Split {
                ratio,
                first,
                second,
                ..
            } = p
            {
                *ratio = if ratio.is_finite() {
                    ratio.clamp(0.1, 0.9)
                } else {
                    0.5
                };
                node(first, ids, warnings, depth + 1)?;
                node(second, ids, warnings, depth + 1)?;
            }
            Ok(())
        }
        for w in &mut self.workspaces {
            if !ids.insert(w.id.clone()) {
                bail!("Duplicate workspace identifier");
            }
            if w.tabs.is_empty() {
                bail!("Workspace contains no tabs");
            }
            for t in &mut w.tabs {
                if !ids.insert(t.id.clone()) {
                    bail!("Duplicate tab identifier");
                }
                node(&mut t.layout, &mut ids, &mut warnings, 0)?;
            }
            if !w.tabs.iter().any(|t| t.id == w.active_tab) {
                w.active_tab = w.tabs[0].id.clone();
            }
        }
        if !self
            .workspaces
            .iter()
            .any(|w| w.id == self.active_workspace)
        {
            self.active_workspace = self.workspaces[0].id.clone();
        }
        self.preferences.font_size = self.preferences.font_size.clamp(10., 28.);
        self.preferences.line_height = self.preferences.line_height.clamp(1., 2.);
        for theme in &self.preferences.custom_themes {
            theme.validate()?;
        }
        self.warnings.extend(warnings);
        Ok(())
    }
    pub fn read(path: &Path) -> Result<Self> {
        let mut s: Self = serde_json::from_slice(
            &fs::read(path).with_context(|| format!("Reading {}", path.display()))?,
        )?;
        s.validate()?;
        Ok(s)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.write_to(path, false)
    }
    /// Serialises the workspace; cheap enough for the UI thread. Pair with
    /// [`write_private`] (or a [`BackgroundWriter`]) to put the bytes on disk.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec_pretty(self)?)
    }
    /// Writes atomically; with `no_clobber` the final link fails if `path` exists,
    /// closing the exists()-then-write race.
    fn write_to(&self, path: &Path, no_clobber: bool) -> Result<()> {
        write_private_inner(path, &self.to_bytes()?, no_clobber)
    }
    /// Imports a copy. Unsupported legacy pane kinds are rejected rather than silently discarded.
    pub fn import_legacy(source: &Path, destination: &Path) -> Result<()> {
        if destination.exists() {
            bail!("A Rust workspace already exists; it will not be overwritten");
        }
        let mut value: serde_json::Value = serde_json::from_slice(&fs::read(source)?)?;
        fn convert(value: &mut serde_json::Value) -> Result<()> {
            if let Some(object) = value.as_object_mut() {
                if object.get("kind").and_then(|v| v.as_str()) == Some("terminal") {
                    // Never trust a launch command from the legacy file; derive it only
                    // from the validated `ssh` / `assistant` fields.
                    object.remove("command");
                    if let Some(host) = object.get("ssh").and_then(|v| v.as_str()) {
                        if !valid_ssh_host(host) {
                            bail!("Unsupported SSH host in legacy workspace");
                        }
                        object.insert("command".into(), serde_json::json!(["ssh", "--", host]));
                    } else if let Some(assistant) = object.get("assistant").and_then(|v| v.as_str())
                    {
                        if !["codex", "claude"].contains(&assistant) {
                            bail!("Unsupported assistant in legacy workspace");
                        }
                        object.insert("command".into(), serde_json::json!([assistant]));
                    }
                }
                for child in object.values_mut() {
                    convert(child)?;
                }
            } else if let Some(array) = value.as_array_mut() {
                for child in array {
                    convert(child)?;
                }
            }
            Ok(())
        }
        convert(&mut value)?;
        let mut state: Self = serde_json::from_value(value)?;
        state.validate()?;
        if !state.warnings.is_empty() {
            bail!("Legacy workspace contains an unsupported terminal command");
        }
        state.write_to(destination, true)
    }
}

/// Atomically replaces `path` with `bytes`: the parent is created private (0700 on
/// unix), the temp file is 0600, synced, then renamed over `path`.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    write_private_inner(path, bytes, false)
}
fn write_private_inner(path: &Path, bytes: &[u8], no_clobber: bool) -> Result<()> {
    let parent = path.parent().context("Path has no parent directory")?;
    create_private_dir(parent, false)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    if no_clobber {
        temp.persist_noclobber(path).map_err(|e| {
            if e.error.kind() == std::io::ErrorKind::AlreadyExists {
                anyhow::anyhow!("A Rust workspace already exists; it will not be overwritten")
            } else {
                anyhow::Error::from(e.error)
            }
        })?;
    } else {
        temp.persist(path)?;
    }
    Ok(())
}

/// One batch of files written in order; the batch stops at the first failure.
pub type WriteBatch = Vec<(PathBuf, Vec<u8>)>;

#[derive(Default)]
struct WriterInner {
    pending: Option<WriteBatch>,
    submitted: u64,
    done: u64,
    errors: Vec<String>,
    shutdown: bool,
}
#[derive(Default)]
struct WriterShared {
    inner: std::sync::Mutex<WriterInner>,
    changed: std::sync::Condvar,
}
impl WriterShared {
    fn lock(&self) -> std::sync::MutexGuard<'_, WriterInner> {
        // A panic while holding the lock cannot leave WriterInner inconsistent.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}
fn write_batch(batch: &WriteBatch) -> Option<String> {
    for (path, bytes) in batch {
        if let Err(e) = write_private(path, bytes) {
            return Some(format!("{} was not saved: {e}", path.display()));
        }
    }
    None
}

/// A single background thread that writes [`WriteBatch`]es with [`write_private`].
/// Latest wins: a batch submitted while another is still queued replaces it, and
/// batches are written one at a time in submission order, so an older snapshot can
/// never land after a newer one. Dropping the writer drains the queue.
pub struct BackgroundWriter {
    shared: std::sync::Arc<WriterShared>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Default for BackgroundWriter {
    fn default() -> Self {
        Self::new()
    }
}
impl BackgroundWriter {
    pub fn new() -> Self {
        let shared = std::sync::Arc::new(WriterShared::default());
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("caelum-persist".into())
            .spawn(move || {
                loop {
                    let mut inner = worker.lock();
                    while inner.pending.is_none() && !inner.shutdown {
                        inner = worker
                            .changed
                            .wait(inner)
                            .unwrap_or_else(|e| e.into_inner());
                    }
                    let Some(batch) = inner.pending.take() else {
                        return; // shutdown with nothing queued
                    };
                    let generation = inner.submitted;
                    drop(inner);
                    let error = write_batch(&batch);
                    let mut inner = worker.lock();
                    inner.done = generation;
                    inner.errors.extend(error);
                    drop(inner);
                    worker.changed.notify_all();
                }
            })
            .ok();
        Self { shared, thread }
    }
    /// Queues `batch`, replacing any batch not yet started.
    pub fn submit(&self, batch: WriteBatch) {
        if self.thread.is_none() {
            // No worker thread could be started: write synchronously instead.
            let error = write_batch(&batch);
            let mut inner = self.shared.lock();
            inner.submitted += 1;
            inner.done = inner.submitted;
            inner.errors.extend(error);
            return;
        }
        let mut inner = self.shared.lock();
        inner.submitted += 1;
        inner.pending = Some(batch);
        drop(inner);
        self.shared.changed.notify_all();
    }
    /// Blocks until every batch submitted so far is on disk (or failed).
    pub fn flush(&self) {
        let mut inner = self.shared.lock();
        let target = inner.submitted;
        while inner.done < target && self.thread.is_some() {
            inner = self
                .shared
                .changed
                .wait(inner)
                .unwrap_or_else(|e| e.into_inner());
        }
    }
    /// Errors from finished batches since the last call.
    pub fn take_errors(&self) -> Vec<String> {
        std::mem::take(&mut self.shared.lock().errors)
    }
}
impl Drop for BackgroundWriter {
    fn drop(&mut self) {
        self.shared.lock().shutdown = true;
        self.shared.changed.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn background_writer_flushes_latest_batch() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("profile/workspace.json");
        let writer = BackgroundWriter::new();
        for i in 0..50 {
            writer.submit(vec![(file.clone(), format!("{i}").into_bytes())]);
        }
        writer.flush();
        assert_eq!(fs::read_to_string(&file).unwrap(), "49");
        assert!(writer.take_errors().is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // Drop drains a batch that was never flushed.
        writer.submit(vec![(file.clone(), b"last".to_vec())]);
        drop(writer);
        assert_eq!(fs::read_to_string(&file).unwrap(), "last");
    }
    #[test]
    fn background_writer_reports_errors_and_stops_batch() {
        let d = tempfile::tempdir().unwrap();
        let blocker = d.path().join("file");
        fs::write(&blocker, "x").unwrap();
        let after = d.path().join("after");
        let writer = BackgroundWriter::new();
        writer.submit(vec![
            (blocker.join("child"), b"a".to_vec()),
            (after.clone(), b"b".to_vec()),
        ]);
        writer.flush();
        let errors = writer.take_errors();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(!after.exists());
        assert!(writer.take_errors().is_empty());
    }
    #[test]
    fn state_bytes_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("workspace.json");
        let state = State::default();
        write_private(&file, &state.to_bytes().unwrap()).unwrap();
        assert_eq!(
            State::read(&file).unwrap().workspaces.len(),
            state.workspaces.len()
        );
    }
    #[cfg(unix)]
    #[test]
    fn private_dir_and_workspace_file_modes() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let d = tempfile::tempdir().unwrap();
        let profile = d.path().join("a/profile");
        create_private_dir(&profile, true).unwrap();
        assert_eq!(fs::metadata(&profile).unwrap().mode() & 0o777, 0o700);
        let file = profile.join("workspace.json");
        State::default().save(&file).unwrap();
        assert_eq!(fs::metadata(&file).unwrap().mode() & 0o777, 0o600);
        // Re-saving over an existing file keeps 0600.
        State::default().save(&file).unwrap();
        assert_eq!(fs::metadata(&file).unwrap().mode() & 0o777, 0o600);
        // Existing loose dir is tightened when requested.
        let loose = d.path().join("loose");
        fs::create_dir(&loose).unwrap();
        fs::set_permissions(&loose, fs::Permissions::from_mode(0o755)).unwrap();
        create_private_dir(&loose, false).unwrap();
        assert_eq!(fs::metadata(&loose).unwrap().mode() & 0o777, 0o755);
        create_private_dir(&loose, true).unwrap();
        assert_eq!(fs::metadata(&loose).unwrap().mode() & 0o777, 0o700);
    }
    #[test]
    fn nested_layout_collapses_without_losing_sibling() {
        let mut p = Pane::terminal(home());
        let original = p.id().to_string();
        let b = Pane::terminal(home());
        let bid = b.id().to_string();
        p.split(&original, b, Axis::Horizontal);
        p.split(&bid, Pane::System { id: id() }, Axis::Vertical);
        assert!(p.remove(&bid));
        assert!(p.find(&original).is_some());
        let mut leaves = vec![];
        p.leaves(&mut leaves);
        assert_eq!(leaves.len(), 2);
    }
    #[test]
    fn saved_workspace_roundtrip_and_import_never_overwrite() {
        let d = tempfile::tempdir().unwrap();
        let old = d.path().join("old.json");
        let new = d.path().join("new.json");
        State::default().save(&old).unwrap();
        State::import_legacy(&old, &new).unwrap();
        let bytes = fs::read(&old).unwrap();
        assert!(State::import_legacy(&old, &new).is_err());
        assert_eq!(bytes, fs::read(&old).unwrap());
        assert_eq!(State::read(&new).unwrap().workspaces.len(), 1);
    }
    #[test]
    fn import_never_clobbers_a_destination_created_after_the_check() {
        let d = tempfile::tempdir().unwrap();
        let dest = d.path().join("dest.json");
        fs::write(&dest, "racer").unwrap();
        // Bypass the early exists() check to exercise the atomic no-clobber link.
        assert!(State::default().write_to(&dest, true).is_err());
        assert_eq!(fs::read_to_string(&dest).unwrap(), "racer");
        let fresh = d.path().join("fresh.json");
        State::default().write_to(&fresh, true).unwrap();
        assert!(State::read(&fresh).is_ok());
    }
    #[cfg(unix)]
    #[test]
    fn profile_trust_rejects_writable_files_and_tightens_dir() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let d = tempfile::tempdir().unwrap();
        let profile = d.path().join("p");
        fs::create_dir(&profile).unwrap();
        fs::set_permissions(&profile, fs::Permissions::from_mode(0o775)).unwrap();
        check_profile_trust(&profile).unwrap();
        assert_eq!(
            fs::metadata(&profile).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let ws = profile.join("workspace.json");
        State::default().save(&ws).unwrap();
        check_profile_trust(&profile).unwrap();
        fs::set_permissions(&ws, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(check_profile_trust(&profile).is_err());
        fs::set_permissions(&ws, fs::Permissions::from_mode(0o600)).unwrap();
        let drafts = profile.join("drafts.json");
        fs::write(&drafts, "[]").unwrap();
        fs::set_permissions(&drafts, fs::Permissions::from_mode(0o620)).unwrap();
        assert!(check_profile_trust(&profile).is_err());
        fs::remove_file(&drafts).unwrap();
        symlink(&ws, &drafts).unwrap();
        assert!(check_profile_trust(&profile).is_err());
        fs::remove_file(&drafts).unwrap();
        check_profile_trust(&profile).unwrap();
    }
    fn term() -> Pane {
        Pane::terminal(PathBuf::from("/tmp"))
    }
    fn leaf_ids(p: &Pane) -> Vec<String> {
        let mut leaves = vec![];
        p.leaves(&mut leaves);
        leaves.iter().map(|l| l.id().to_string()).collect()
    }
    const AREA: PaneRect = PaneRect {
        x: 0.,
        y: 0.,
        w: 1200.,
        h: 800.,
    };
    #[test]
    fn repeated_splits_make_equal_columns() {
        let mut p = term();
        let mut focus = p.id().to_string();
        for _ in 0..4 {
            let n = term();
            let next = n.id().to_string();
            assert!(p.split_toward(&focus, n, Direction::Right));
            focus = next;
        }
        let rects = p.leaf_rects(AREA, 1.);
        assert_eq!(rects.len(), 5);
        let usable = AREA.w - 4.;
        for (_, r) in &rects {
            assert!((r.w - usable / 5.).abs() <= 1., "{rects:?}");
            assert_eq!(r.h, AREA.h);
        }
        // Splitting a middle column re-balances the whole row, not just its half.
        let middle = rects[2].0.clone();
        p.split_toward(&middle, term(), Direction::Left);
        let rects = p.leaf_rects(AREA, 1.);
        for (_, r) in &rects {
            assert!((r.w - (AREA.w - 5.) / 6.).abs() <= 1., "{rects:?}");
        }
    }
    #[test]
    fn rebalance_leaves_other_axis_alone() {
        let mut p = term();
        let a = p.id().to_string();
        let b = term();
        let bid = b.id().to_string();
        p.split_toward(&a, b, Direction::Down);
        if let Pane::Split { ratio, .. } = &mut p {
            *ratio = 0.7;
        }
        // A row inside the bottom pane does not touch the custom vertical ratio.
        p.split_toward(&bid, term(), Direction::Right);
        assert!(matches!(&p, Pane::Split { ratio, .. } if (*ratio - 0.7).abs() < 1e-6));
    }
    #[test]
    fn repeated_splits_stay_within_bounds() {
        let area = PaneRect::new(0., 0., 1000., 600.);
        let mut p = term();
        let mut focus = p.id().to_string();
        let mut refused = false;
        for _ in 0..40 {
            let n = term();
            let next = n.id().to_string();
            match p.try_split(&focus, n, Direction::Right, area, MIN_PANE, 1.) {
                Ok(_) => focus = next,
                Err(e) => {
                    assert_eq!(e, SplitError::NoRoom);
                    refused = true;
                    break;
                }
            }
        }
        assert!(refused, "a 1000pt wide area cannot hold 40 columns");
        let rects = p.leaf_rects(area, 1.);
        assert_eq!(rects.len(), (1000. / (MIN_PANE.width + 1.)) as usize);
        for (_, r) in rects {
            assert!(r.w >= MIN_PANE.width && r.x >= 0. && r.x + r.w <= area.w + 0.01);
        }
        // Same for rows.
        let mut p = term();
        let mut focus = p.id().to_string();
        loop {
            let n = term();
            let next = n.id().to_string();
            if p.try_split(&focus, n, Direction::Down, area, MIN_PANE, 1.)
                .is_err()
            {
                break;
            }
            focus = next;
        }
        for (_, r) in p.leaf_rects(area, 1.) {
            assert!(r.h >= MIN_PANE.height && r.y + r.h <= area.h + 0.01);
        }
    }
    #[test]
    fn try_split_rebalances_when_that_makes_room() {
        let area = PaneRect::new(0., 0., 700., 400.);
        let mut p = term();
        let a = p.id().to_string();
        let b = term();
        let bid = b.id().to_string();
        p.split_toward(&a, b, Direction::Right);
        let c = term();
        let cid = c.id().to_string();
        p.split_toward(&bid, c, Direction::Down);
        // Squash the right column so splitting `c` sideways does not fit as-is.
        if let Pane::Split { ratio, .. } = &mut p {
            *ratio = 0.75;
        }
        let out = p.try_split(&cid, term(), Direction::Right, area, MIN_PANE, 1.);
        assert_eq!(out, Ok(SplitOutcome::Rebalanced));
        assert!(p.fits(area, MIN_PANE, 1.));
        assert_eq!(leaf_ids(&p).len(), 4);
        let before = p.clone();
        assert_eq!(
            p.try_split("missing", term(), Direction::Up, area, MIN_PANE, 1.),
            Err(SplitError::NotFound)
        );
        assert_eq!(leaf_ids(&p), leaf_ids(&before));
    }
    #[test]
    fn all_four_directions_insert_on_the_correct_side() {
        for dir in Direction::ALL {
            let mut p = term();
            let old = p.id().to_string();
            let n = term();
            let new = n.id().to_string();
            assert!(p.split_toward(&old, n, dir));
            let rects: std::collections::HashMap<_, _> =
                p.leaf_rects(AREA, 1.).into_iter().collect();
            let (o, n) = (rects[&old], rects[&new]);
            match dir {
                Direction::Left => assert!(n.x + n.w <= o.x && n.y == o.y),
                Direction::Right => assert!(o.x + o.w <= n.x && n.y == o.y),
                Direction::Up => assert!(n.y + n.h <= o.y && n.x == o.x),
                Direction::Down => assert!(o.y + o.h <= n.y && n.x == o.x),
            }
            assert!(matches!(&p, Pane::Split { axis, .. } if *axis == dir.axis()));
        }
    }
    #[test]
    fn split_targets_only_leaves() {
        let mut p = term();
        let a = p.id().to_string();
        p.split_toward(&a, term(), Direction::Right);
        let root = p.id().to_string();
        assert!(!p.split_toward(&root, term(), Direction::Right));
        assert_eq!(leaf_ids(&p).len(), 2);
    }
    #[test]
    fn equalize_balances_every_run() {
        let mut p = term();
        let a = p.id().to_string();
        let b = term();
        let bid = b.id().to_string();
        p.split_toward(&a, b, Direction::Right);
        let c = term();
        let cid = c.id().to_string();
        p.split_toward(&bid, c, Direction::Down);
        p.split_toward(&cid, term(), Direction::Down);
        fn skew(p: &mut Pane) {
            if let Pane::Split {
                ratio,
                first,
                second,
                ..
            } = p
            {
                *ratio = 0.2;
                skew(first);
                skew(second);
            }
        }
        skew(&mut p);
        p.equalize();
        let rects: std::collections::HashMap<_, _> = p.leaf_rects(AREA, 0.).into_iter().collect();
        assert_eq!(rects[&a].w, 600.);
        // Right column: three rows of equal height.
        let rows: Vec<f32> = rects
            .iter()
            .filter(|(id, _)| **id != a)
            .map(|(_, r)| r.h)
            .collect();
        assert_eq!(rows.len(), 3);
        for h in rows {
            assert!((h - 800. / 3.).abs() <= 1.);
        }
    }
    #[test]
    fn min_extent_counts_nested_tiles() {
        let mut p = term();
        let a = p.id().to_string();
        let b = term();
        let bid = b.id().to_string();
        p.split_toward(&a, b, Direction::Right);
        p.split_toward(&bid, term(), Direction::Down);
        assert_eq!(
            p.min_extent(Axis::Horizontal, MIN_PANE, 1.),
            2. * MIN_PANE.width + 1.
        );
        assert_eq!(
            p.min_extent(Axis::Vertical, MIN_PANE, 1.),
            2. * MIN_PANE.height + 1.
        );
    }
    #[test]
    fn duplicate_panes_are_rejected() {
        let mut s = State::default();
        let t = s.workspaces[0].tabs[0].clone();
        s.workspaces[0].tabs.push(t);
        assert!(s.validate().is_err());
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    #[test]
    fn imports_legacy_browser_and_ssh_without_changing_source() {
        let d = tempfile::tempdir().unwrap();
        let source = d.path().join("source.json");
        let destination = d.path().join("dest.json");
        let original = serde_json::json!({"version":1,"activeWorkspace":"w","workspaces":[{"id":"w","name":"Legacy","activeTab":"t","tabs":[{"id":"t","name":"SSH","layout":{"id":"s","kind":"split","axis":"horizontal","ratio":0.6,"first":{"id":"terminal","kind":"terminal","cwd":"/tmp","ssh":"user@example.org"},"second":{"id":"browser","kind":"web","url":"https://example.org"}}}]}],"preferences":{"theme":"orbit","fontSize":13}});
        let bytes = serde_json::to_vec(&original).unwrap();
        fs::write(&source, &bytes).unwrap();
        State::import_legacy(&source, &destination).unwrap();
        assert_eq!(fs::read(&source).unwrap(), bytes);
        let state = State::read(&destination).unwrap();
        let mut leaves = vec![];
        state.workspaces[0].tabs[0].layout.leaves(&mut leaves);
        assert!(
            matches!(&leaves[0],Pane::Terminal{command:Some(command),..} if command==&vec!["ssh","--","user@example.org"])
        );
        assert!(matches!(&leaves[1],Pane::Browser{url,..} if url=="https://example.org"));
    }

    fn legacy(terminal: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"version":1,"activeWorkspace":"w","workspaces":[{"id":"w","name":"L","activeTab":"t","tabs":[{"id":"t","name":"T","layout":terminal}]}]})
    }
    fn only_command(state: &State) -> Option<Vec<String>> {
        match &state.workspaces[0].tabs[0].layout {
            Pane::Terminal { command, .. } => command.clone(),
            other => panic!("unexpected pane {other:?}"),
        }
    }

    #[test]
    fn legacy_import_strips_injected_command() {
        let d = tempfile::tempdir().unwrap();
        let source = d.path().join("source.json");
        let destination = d.path().join("dest.json");
        // A bare command with no ssh/assistant is discarded entirely.
        let v = legacy(
            serde_json::json!({"id":"x","kind":"terminal","cwd":"/tmp","command":["sh","-c","touch /tmp/pwned"]}),
        );
        fs::write(&source, serde_json::to_vec(&v).unwrap()).unwrap();
        State::import_legacy(&source, &destination).unwrap();
        assert_eq!(only_command(&State::read(&destination).unwrap()), None);
        // A command next to a valid ssh host is replaced by the derived ssh argv.
        fs::remove_file(&destination).unwrap();
        let v = legacy(
            serde_json::json!({"id":"x","kind":"terminal","cwd":"/tmp","ssh":"h","command":["ssh","-oProxyCommand=touch /tmp/x","h"]}),
        );
        fs::write(&source, serde_json::to_vec(&v).unwrap()).unwrap();
        State::import_legacy(&source, &destination).unwrap();
        assert_eq!(
            only_command(&State::read(&destination).unwrap()),
            Some(vec!["ssh".into(), "--".into(), "h".into()])
        );
    }

    #[test]
    fn legacy_import_rejects_ssh_option_injection() {
        let d = tempfile::tempdir().unwrap();
        let source = d.path().join("source.json");
        let destination = d.path().join("dest.json");
        for host in ["-oProxyCommand=touch /tmp/x", "", "a b", "h;id"] {
            let v = legacy(serde_json::json!({"id":"x","kind":"terminal","ssh":host}));
            fs::write(&source, serde_json::to_vec(&v).unwrap()).unwrap();
            assert!(
                State::import_legacy(&source, &destination).is_err(),
                "{host:?}"
            );
            assert!(!destination.exists());
        }
    }

    #[test]
    fn workspace_file_with_arbitrary_command_drops_it() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("workspace.json");
        for bad in [
            serde_json::json!(["sh", "-c", "touch /tmp/pwned"]),
            serde_json::json!(["ssh", "-oProxyCommand=touch /tmp/x", "h"]),
            serde_json::json!(["ssh", "--", "-oProxyCommand=x"]),
            serde_json::json!(["ssh", "--", "h", "extra"]),
            serde_json::json!(["codex", "--dangerously-bypass"]),
            serde_json::json!(["/usr/bin/claude"]),
            serde_json::json!([]),
        ] {
            let v = legacy(serde_json::json!({"id":"x","kind":"terminal","command":bad}));
            fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
            let state = State::read(&path).unwrap();
            assert_eq!(only_command(&state), None, "{bad}");
            assert_eq!(state.warnings.len(), 1);
        }
    }

    #[test]
    fn valid_ssh_codex_claude_commands_are_kept() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("workspace.json");
        for good in [
            vec!["ssh", "--", "user@example.org"],
            vec!["ssh", "--", "[::1]:22"],
            vec!["codex"],
            vec!["claude"],
        ] {
            let v = legacy(serde_json::json!({"id":"x","kind":"terminal","command":good}));
            fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
            let state = State::read(&path).unwrap();
            let want: Vec<String> = good.iter().map(|s| s.to_string()).collect();
            assert_eq!(only_command(&state), Some(want));
            assert!(state.warnings.is_empty());
        }
        // Legacy assistant/ssh fields are converted to the same allowed argv.
        let source = d.path().join("legacy.json");
        let destination = d.path().join("dest.json");
        let v = legacy(serde_json::json!({"id":"x","kind":"terminal","assistant":"claude"}));
        fs::write(&source, serde_json::to_vec(&v).unwrap()).unwrap();
        State::import_legacy(&source, &destination).unwrap();
        assert_eq!(
            only_command(&State::read(&destination).unwrap()),
            Some(vec!["claude".to_string()])
        );
    }

    /// A workspace.json written before four-direction splits (and before panes were
    /// rebalanced) still loads unchanged, and its layout can be split further.
    #[test]
    fn pre_rebalance_workspace_still_loads_and_splits() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("workspace.json");
        let v = serde_json::json!({"version":1,"activeWorkspace":"w","workspaces":[{"id":"w","name":"Mission Control","activeTab":"t","tabs":[{"id":"t","name":"Terminal","layout":{"id":"s","kind":"split","axis":"horizontal","ratio":0.83,"first":{"id":"a","kind":"terminal","cwd":"/tmp"},"second":{"id":"s2","kind":"split","axis":"horizontal","ratio":0.5,"first":{"id":"b","kind":"terminal","cwd":"/tmp"},"second":{"id":"f","kind":"files","path":"/tmp"}}}}]}],"preferences":{"theme":"graphite","fontSize":15.0,"terminalFont":"","lineHeight":1.2,"cursorStyle":"block","cursorBlink":false,"sidebar":false,"confirmMultilinePaste":true,"customThemes":[]}});
        fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        let mut state = State::read(&path).unwrap();
        // Profiles from before `followSystem` existed switch to the desktop style
        // once, keeping their chosen theme for when it is turned off again.
        assert!(state.preferences.follow_system);
        assert_eq!(state.preferences.theme, "graphite");
        let layout = &mut state.workspaces[0].tabs[0].layout;
        // Saved ratios are kept as-is on load.
        assert!(matches!(layout, Pane::Split { ratio, .. } if (*ratio - 0.83).abs() < 1e-6));
        assert!(layout.split_toward("b", Pane::System { id: "n".into() }, Direction::Left));
        let mut leaves = vec![];
        layout.leaves(&mut leaves);
        let ids: Vec<_> = leaves.iter().map(|p| p.id()).collect();
        assert_eq!(ids, ["a", "n", "b", "f"]);
        // The row the new pane joined is now four equal columns.
        let rects = layout.leaf_rects(PaneRect::new(0., 0., 1203., 500.), 1.);
        for (_, r) in rects {
            assert!((r.w - 300.).abs() <= 1., "{r:?}");
        }
        // An explicit choice to stop following the desktop is saved and kept.
        state.preferences.follow_system = false;
        state.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"followSystem\": false"), "{text}");
        assert!(!State::read(&path).unwrap().preferences.follow_system);
    }

    #[test]
    fn ssh_host_allowlist() {
        assert!(valid_ssh_host("user@host.example"));
        assert!(valid_ssh_host("[fe80::1]"));
        assert!(!valid_ssh_host(""));
        assert!(!valid_ssh_host("-oProxyCommand=x"));
        assert!(!valid_ssh_host("host name"));
        assert!(!valid_ssh_host("host$(id)"));
    }
}
