# Shell integration

Caelum sets `CAELUM_RS` in every shell it spawns. Snippets in [`shell/`](../shell) emit:

- **OSC 7** (`file://host/path`) after each prompt, so new panes/tabs open in the shell's current directory (including inside subshells; `/proc` is the fallback without integration). URIs naming a different host (e.g. over SSH) are ignored.
- **OSC 133** A/C/D prompt and command marks, enabling **Ctrl+Shift+Up / Ctrl+Shift+Down** to jump between prompts in scrollback (rebindable as `prev_prompt` / `next_prompt` in config.toml). Marks are dropped if scrollback is truncated rather than pointing at wrong lines.

## Automatic setup

Nothing to configure: when Caelum starts your shell (`$SHELL`, if it is bash, zsh or
fish) it loads these snippets itself, after your own startup files. The scripts are
embedded in the binary and written to `shell-integration/` in the profile directory
(`~/.local/share/caelum-rs/` on Linux), so installed and development builds work alike.

| Shell | How it is loaded |
| --- | --- |
| bash | Started as `bash --posix` with `ENV` set to a shim (bash only reads `ENV` in POSIX mode). The shim restores `ENV`, leaves POSIX mode, sources `/etc/profile` and the first of `~/.bash_profile`, `~/.bash_login`, `~/.profile` (what `bash -l` does), then loads `caelum.bash`. `~/.bash_logout` is not run. |
| zsh | `ZDOTDIR` points at a wrapper whose `.zshenv` restores your `ZDOTDIR` (or unsets it), runs your `.zshenv`, and loads `caelum.zsh` just before the first prompt, after your `.zshrc`. If your `.zshrc` replaces `precmd_functions` wholesale the hook is lost; source `caelum.zsh` yourself then. |
| fish | The data directory is prepended to `XDG_DATA_DIRS` so fish sources its `vendor_conf.d/caelum.fish`, which removes that entry again. fish 4 also emits its own OSC 7/133 marks; Caelum tolerates the duplicates. |

The variables used for the hand-off (`ENV`, `ZDOTDIR`, `XDG_DATA_DIRS` changes,
`CAELUM_BASH_*`, `CAELUM_ZDOTDIR`) are restored or removed before your startup files
finish, so child shells and programs do not see them. The snippets do nothing a second
time, so keeping an old `source caelum.*` line in your dotfiles is harmless. Other
shells, and PowerShell/cmd on Windows, start untouched.

Opt out in `config.toml`:

```toml
shell_integration = false
```

## What it enables

- Long commands: when a command that ran at least `long_command_seconds` (default 10)
  finishes while its pane is not the focused one or the window is in the background, a
  toast shows its run time and exit status and its tab gets a dot until you look.
- **Copy Last Command Output** (right-click menu, command palette, `copy_last_output`,
  default Ctrl+Shift+O) copies the text between the last OSC 133 `C` and `D` that is
  still in the scrollback.
- Closing a pane, tab or window asks for confirmation only while a command is running.
  Without integration Caelum falls back to checking for a foreground job in the PTY.

## Manual setup

Only needed if automatic setup is off or unsupported for your shell (guarded so other
terminals are unaffected):

```sh
# ~/.bashrc
[ -n "$CAELUM_RS" ] && source /path/to/caelum-rs/shell/caelum.bash
# ~/.zshrc
[[ -n $CAELUM_RS ]] && source /path/to/caelum-rs/shell/caelum.zsh
# ~/.config/fish/config.fish
test -n "$CAELUM_RS"; and source /path/to/caelum-rs/shell/caelum.fish
```

Automatic injection is covered by `cargo test --test shell_integration -- --ignored`
(real PTYs; needs bash, zsh and fish on `PATH`, or `CAELUM_TEST_<SHELL>` paths).
