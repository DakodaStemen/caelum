# Changelog

## Unreleased

- Minimal, desktop-native look: follows the system light/dark preference and accent
  color (freedesktop portal, then gsettings; `defaults` on macOS, the registry on
  Windows) with new Adwaita Dark/Light presets, uses the desktop UI font, and drops the
  logo, badge, breadcrumb, status bar and accent borders. Choosing a theme stops
  following the desktop; Preferences has a "Follow system style" option. Existing
  profiles start following the desktop once (their theme choice is kept).
- GNOME-style headerbar with drawn symbolic icons, a main menu, and a tab bar that only
  appears with more than one tab; on GNOME Wayland it replaces the title bar
  (`CAELUM_RS_CSD=0/1` overrides). Status messages are toasts.
- Pane close button: fixed the "×" that sat on the pane border and drifted off center
  at non-integer zoom levels; it is now a pixel-snapped vector icon with hover feedback.
- Splits: split left/right/up/down (popover and Ctrl+Shift+D/E,
  Ctrl+Alt+Shift+D/E), panes in the same row or column are rebalanced to equal sizes,
  splits that would shrink a pane below 160x96 are refused instead of pushing panes
  off screen, dividers cannot be dragged past that minimum, and "Balance panes"
  (Ctrl+Shift+R, or double-click a divider) equalizes the layout.

No release tag has been cut yet; `0.1.0` below is the Cargo version, not a published tag.

- In-flight fixes:
  - Title and paste sanitising
  - Non-blocking terminal close
  - Lower idle CPU
  - Disk and process work moved off the UI thread
  - Crash fixes
  - macOS and Windows cwd tracking and process-tree kill
  - Pinned release tooling
- Vibrant syntax highlighting in the editor (Rust, Python, JavaScript, JSON, Markdown, HTML,
  CSS, shell, YAML, TOML and more), colored by each theme's new `syntax` palette.
- The custom theme editor now includes the code (syntax) colors, listed first.
- Graphite, a neutral grey theme with muted terminal colors, is the new default. Orbit stays available.
- The sidebar is hidden by default for a minimal window; Ctrl+Shift+B or the header button
  slides it in and out, and the choice is remembered. The header is slimmer too.
- The Appearance window opens centered.
- All 11 themes (graphite, starlight, orbit, nova, aurora, nebula, mars, eclipse, comet, lunar, dawn) rebuilt with richer accents, clearer surfaces and brighter terminal colors.
- The editor Save button now reads just "Save" (Ctrl+S shown as a tooltip).
- Interface overhaul: breadcrumb header, command-search pill, sidebar rows with hints and an
  active marker, tab chips with inline close, focus-aware pane headers, rounded controls,
  shortcut tooltips and a quieter status bar.

## 0.1.0 — Native preview

- Native Rust desktop app using egui, Alacritty's terminal core and portable-pty.
- Workspaces, tabs, resizable nested splits, pane zoom, file browser and text editor.
- Eleven Caelum themes (graphite, starlight, orbit, nova, aurora, nebula, mars, eclipse, comet, lunar, dawn), custom color previews and import/export, font and cursor controls.
- Linux system/process views, SSH and assistant command launchers.
- Independent profile, optional legacy workspace import, recovery and safe-save checks.
- Supplied Caelum icon with the lettering removed.

This is an early migration preview. See `docs/migration.md` for remaining parity work.
