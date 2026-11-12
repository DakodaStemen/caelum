# Shell integration

Caelum sets `CAELUM_RS` in every shell it spawns. Snippets in [`shell/`](../shell) emit:

- **OSC 7** (`file://host/path`) after each prompt, so new panes/tabs open in the shell's current directory (including inside subshells; `/proc` is the fallback without integration). URIs naming a different host (e.g. over SSH) are ignored.
- **OSC 133** A/C/D prompt (`C` only from bash 4.4, see below) and command marks, enabling **Ctrl+Shift+Up / Ctrl+Shift+Down** to jump between prompts in scrollback (rebindable as `prev_prompt` / `next_prompt` in config.toml). Marks are dropped if scrollback is truncated rather than pointing at wrong lines.

## Automatic setup

Nothing to configure: when Caelum starts your shell (`$SHELL`, if it is bash, zsh or
fish) it loads these snippets itself, after your own startup files. The scripts are
embedded in the binary and written to `shell-integration/` in the profile directory
(`~/.local/share/caelum-rs/` on Linux), so installed and development builds work alike.

| Shell | How it is loaded |
| --- | --- |
| bash | Started as `bash --posix` with `ENV` set to a shim (bash only reads `ENV` in POSIX mode). The shim restores `ENV`, leaves POSIX mode (and switches back off the options POSIX mode turns on, such as `inherit_errexit`, so `set -e` behaves as in a plain shell), sources `/etc/profile` and the first of `~/.bash_profile`, `~/.bash_login`, `~/.profile` (exactly what the plain `bash -l` Caelum starts without integration does: `~/.bashrc` is read only if your profile sources it), then loads `caelum.bash`. `~/.bash_logout` is not run. |
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

## bash before 4.4 (macOS `/bin/bash` 3.2)

bash only has `PS0`, the hook that sends the command-start mark (`C`), from 4.4. On
older bash `caelum.bash` sends OSC 7 and `A`/`D` only; it installs no `DEBUG` trap and
does not touch the end of `PROMPT_COMMAND`, because a trap armed from there breaks
`PROMPT_COMMAND` values ending in `;`, `&` or a comment, is restored around `source`
(losing bash-preexec/iTerm2 chaining), and fires inside `bind -x` handlers such as fzf
and atuin. [Ghostty takes the same stance](https://ghostty.org/docs/features/shell-integration#shell-integration-injection):
its automatic integration does not support macOS `/bin/bash`.

Instead Caelum notices the command start itself, for any shell that sends `A` and `D`
but no `C`: when you press Enter (or paste several lines without bracketed paste) at a
prompt, the output is taken to begin on the line after the cursor, and the run time
starts then. A real `C`, when the shell sends one (bash 4.4+, zsh, fish), takes over.
Enter inside a full-screen program, or while a command is running, is ignored, and
bracketed-paste newlines are text, not Enter. Limits: a command typed over several
lines (a `PS2` continuation) or edited with the cursor away from the end of the line
starts its output a line or two early, and a command started by a prompt hook rather
than Enter is not seen.

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
