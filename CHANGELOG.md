# Changelog

## Unreleased

- Review fixes for the friendlier-terminal work: the injected bash starts with the
  same options as a plain `bash -l` (`inherit_errexit` and the history defaults are no
  longer left from POSIX mode); OSC 52 writes are refused on the reader thread when the
  setting is off or the payload is over 1 MiB, only the newest pending write is kept,
  the preference reaches every tab (also hidden ones) before it is polled, and OSC
  strings longer than about 1.4 MB are cut off before they reach the terminal parser;
  file-path links are looked up only while Ctrl is held, on a background thread with a
  short cache, so a stalled network mount cannot freeze the window; **Discard** in the
  unsaved-edits dialog now goes on to the running-command confirmation instead of
  closing at once; "Copy Last Command Output" no longer includes the next prompt
  when the output had no trailing newline.
- Ctrl+click (hover with Ctrl to see the underline) a local file path in terminal output (`path`, `path:line`, `path:line:col`,
  as printed by compilers, `rg` and Python tracebacks) to open it in Caelum's editor
  pane scrolled to that line; relative paths resolve against the shell's directory and
  only existing files are links. Folders open in the file browser pane when one exists.
  An editor already showing the file is reused.
- Directional pane focus: Ctrl+Alt+Left/Right/Up/Down (rebindable as `focus_left`,
  `focus_right`, `focus_up`, `focus_down`; also in the command palette) focuses the
  neighbouring split pane and leaves zoom. Alt+Arrow is not used: shells bind it to
  word movement.
- A floating "Jump to bottom" pill appears at the bottom right of a terminal only
  while it is scrolled back; clicking it returns to the live output, and its outline
  takes the accent colour when new output arrived meanwhile.
- OSC 52 clipboard writes from programs now reach the clipboard (with the "Copied to
  clipboard" toast), capped at 1 MiB and switchable in Preferences (**Programs Can
  Copy**, on by default) or `[terminal] osc52_write`. Reading the clipboard stays blocked.
- Minimum contrast: text that a program colors nearly the same as its background is
  nudged lighter or darker to reach a contrast ratio of 1.5 (`[terminal] min_contrast`,
  1 to 21; 1 disables). Bundled themes' own colors are unaffected.
- Automatic shell integration: bash, zsh and fish start with Caelum's OSC 7 / OSC 133
  integration loaded, with no dotfile edits (bash via an `ENV` shim that runs the login
  files, zsh via a `ZDOTDIR` wrapper that restores yours, fish via `vendor_conf.d`).
  Your own startup files run first and are never modified, a manual `source caelum.*`
  stays harmless, and the hand-off variables are not inherited by child shells. The
  scripts are embedded in the binary and written to the profile directory. Turn it off
  with `shell_integration = false` in `config.toml`.
- A command that ran 10 s or longer and finishes while its pane is not the focused one
  (or the window is in the background) shows a toast ("Command finished in 18s",
  "Command failed (exit 1) after 12s") and puts a dot on its tab until you look at the
  pane. `long_command_seconds` in `config.toml` changes the threshold; 0 turns it off.
- "Copy Last Command Output" in the terminal context menu, the command palette and as
  the rebindable `copy_last_output` shortcut (default Ctrl+Shift+O); needs shell
  integration.
- Closing a pane, tab, workspace or window asks first only when a terminal in it is
  running something (an integrated shell's command, any foreground job, or an
  ssh/codex/claude session); idle shells at a prompt close silently.
- Copy on Select is now on by default: finishing a selection copies it to the
  clipboard and shows a "Copied to clipboard" toast (also for Ctrl+Shift+C, Ctrl+Insert
  and the menu Copy). Existing profiles pick up the new default (the preference is
  stored under a new key); turning it off in Preferences is kept. While a program has
  mouse reporting on, hold Shift to select.
- Terminal mouse: drag selection is anchored where the button went down and follows
  cell halves; triple-click selects a line, double/triple-click then drag extends by
  words/lines, Shift+click extends, Ctrl+drag selects a block, and dragging past the
  edge auto-scrolls. Selections go to the PRIMARY selection (middle-click pastes it on
  X11 and Wayland); optional Copy on Select. Ctrl+Shift+A selects all, Ctrl+Insert /
  Shift+Insert copy and paste, and the context menu adds Select All, Open/Copy Link
  and Clear Scrollback. Plain-text http(s) URLs underline on hover and open with
  Ctrl+click; double-click keeps URLs and `file:line` whole. Dropped files are typed
  as quoted paths.
- Mouse reporting for TUIs: drags (1002) and motion (1003) are now reported, in SGR,
  UTF-8 or X10 encoding as requested, with Alt/Ctrl modifiers; right-click goes to the
  program (Shift+right-click opens the menu). Focus in/out reports (1004), wheel
  scrolling by lines (3 per notch), and arrow keys for the wheel on the alternate
  screen (`less`, `man`).

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
