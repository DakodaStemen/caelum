//! The shell snippets must percent-encode the OSC 7 cwd so a hostile directory name
//! cannot terminate the sequence early and inject OSC 133 marks of its own.
use caelum_rs::osc::{OscEvent, OscScanner};
use std::{path::Path, process::Command};

/// BEL ends the OSC 7 early; `ESC ] 133;D;99 BEL` would then be a forged CommandEnd.
const HOSTILE: &str = "evil\x07\x1b]133;D;99\x07 tail%41\u{e9}";

fn hostile_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join(HOSTILE);
    std::fs::create_dir(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    (base, dir)
}

fn find(shell: &str) -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os(format!("CAELUM_TEST_{}", shell.to_uppercase())) {
        return Some(p.into());
    }
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|d| d.join(shell))
            .find(|p| p.is_file())
    });
    assert!(
        found.is_some() || std::env::var("CAELUM_REQUIRE_SHELLS").as_deref() != Ok("1"),
        "{shell} not found; required by CAELUM_REQUIRE_SHELLS=1"
    );
    found
}

fn snippet(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("shell")
        .join(name)
        .display()
        .to_string()
}

fn assert_single_cwd(output: &[u8], host: String, dir: &Path) {
    assert_eq!(
        output.iter().filter(|&&b| b == 0x07).count(),
        1,
        "raw BEL leaked into the OSC 7 payload: {output:?}"
    );
    let events: Vec<OscEvent> = OscScanner::new(host)
        .scan(output)
        .into_iter()
        .map(|(_, e)| e)
        .collect();
    assert_eq!(events, vec![OscEvent::Cwd(dir.to_path_buf())], "{output:?}");
}

fn run(cmd: &mut Command) -> Vec<u8> {
    let out = cmd
        .env("CAELUM_RS", "1")
        .env("LANG", "C.UTF-8")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

#[test]
fn bash_osc7_encodes_hostile_directory_names() {
    let Some(bash) = find("bash") else {
        eprintln!("bash not found; skipping");
        return;
    };
    let (_base, dir) = hostile_dir();
    let out = run(Command::new(bash)
        .arg("--norc")
        .arg("-c")
        .arg(format!(
            "source '{}'; HOSTNAME=box; cd -- \"$1\" && __caelum_osc7",
            snippet("caelum.bash")
        ))
        .arg("_")
        .arg(&dir));
    assert_single_cwd(&out, "box".into(), &dir);
}

#[test]
fn zsh_osc7_encodes_hostile_directory_names() {
    let Some(zsh) = find("zsh") else {
        eprintln!("zsh not found; skipping");
        return;
    };
    let (_base, dir) = hostile_dir();
    let out = run(Command::new(zsh)
        .arg("-f")
        .arg("-c")
        .arg(format!(
            "source '{}' 2>/dev/null; HOST=box; cd -- \"$1\" && __caelum_osc7",
            snippet("caelum.zsh")
        ))
        .arg("_")
        .arg(&dir));
    assert_single_cwd(&out, "box".into(), &dir);
}

#[test]
fn fish_osc7_encodes_hostile_directory_names() {
    let Some(fish) = find("fish") else {
        eprintln!("fish not found; skipping");
        return;
    };
    let (_base, dir) = hostile_dir();
    let out = run(Command::new(fish)
        .arg("--no-config")
        .arg("-c")
        .arg(format!(
            "source '{}'; cd $argv[1]; and __caelum_osc7",
            snippet("caelum.fish")
        ))
        .arg(&dir));
    // fish always reports its own $hostname; accept whatever host it printed.
    let text = String::from_utf8_lossy(&out);
    let host = text
        .strip_prefix("\x1b]7;file://")
        .and_then(|r| r.split('/').next())
        .unwrap_or_default()
        .to_string();
    assert_single_cwd(&out, host, &dir);
}

/// Bytes >= 0x80 must encode as exactly one `%XX` (bash 3.2 sign-extends them to
/// `%FFFFFFFFFFFFFFC3` unless the value is masked to a byte).
#[cfg(unix)]
const HIGH_BYTES: &[u8] = b"a\x80\xc3\xa9\xff z";
#[cfg(unix)]
const HIGH_BYTES_ENCODED: &[u8] = b"a%80%C3%A9%FF%20z";

#[cfg(unix)]
fn high_bytes_arg() -> &'static std::ffi::OsStr {
    std::os::unix::ffi::OsStrExt::from_bytes(HIGH_BYTES)
}

#[cfg(unix)]
#[test]
fn bash_urlencode_masks_high_bytes() {
    let Some(bash) = find("bash") else {
        eprintln!("bash not found; skipping");
        return;
    };
    let out = run(Command::new(bash)
        .arg("--norc")
        .arg("-c")
        .arg(format!(
            "source '{}'; __caelum_urlencode \"$1\"; printf '%s' \"$REPLY\"",
            snippet("caelum.bash")
        ))
        .arg("_")
        .arg(high_bytes_arg()));
    assert_eq!(out, HIGH_BYTES_ENCODED, "{}", String::from_utf8_lossy(&out));
}

#[cfg(unix)]
#[test]
fn zsh_urlencode_masks_high_bytes() {
    let Some(zsh) = find("zsh") else {
        eprintln!("zsh not found; skipping");
        return;
    };
    let out = run(Command::new(zsh)
        .arg("-f")
        .arg("-c")
        .arg(format!(
            "source '{}' 2>/dev/null; __caelum_urlencode \"$1\"; print -rn -- \"$REPLY\"",
            snippet("caelum.zsh")
        ))
        .arg("_")
        .arg(high_bytes_arg()));
    assert_eq!(out, HIGH_BYTES_ENCODED, "{}", String::from_utf8_lossy(&out));
}

// ---- Automatic injection, through a real PTY (ignored: needs the shells installed) ----

use caelum_rs::shell_integration::{dir_in, install, plan};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};

struct Run {
    output: String,
    root: std::path::PathBuf,
}

/// Starts `shell` through the same plan Caelum uses, with a scratch HOME holding the
/// given dotfiles, types `input`, and returns everything the PTY produced.
fn inject(shell: &str, dotfiles: &[(&str, String)], input: &str) -> Option<Run> {
    let path = find(shell)?;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let root = dir_in(&tmp.path().join("data"));
    install(&root).unwrap();
    for (rel, text) in dotfiles {
        let file = home.join(rel);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text.replace("@ROOT@", root.to_str().unwrap())).unwrap();
    }
    std::fs::create_dir_all(&home).unwrap();
    let inherited = |k: &str| std::env::var(k).ok().filter(|_| k == "XDG_DATA_DIRS");
    let injection = plan(path.to_str().unwrap(), &root, &inherited).expect("known shell");
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(&path);
    match &injection.args {
        Some(args) => cmd.args(args),
        None => cmd.args(["-l"]),
    };
    for k in ["ENV", "ZDOTDIR", "HISTFILE", "XDG_CONFIG_HOME"] {
        cmd.env_remove(k);
    }
    cmd.env("HOME", &home);
    cmd.env("TERM", "xterm-256color");
    cmd.env("CAELUM_RS", "1");
    cmd.cwd(&home);
    for (k, v) in &injection.env {
        cmd.env(k, v);
    }
    let mut child = pty.slave.spawn_command(cmd).unwrap();
    drop(pty.slave);
    let mut reader = pty.master.try_clone_reader().unwrap();
    let mut writer = pty.master.take_writer().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut all = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            all.extend_from_slice(&buf[..n]);
            let _ = tx.send(all.clone());
        }
    });
    std::thread::sleep(std::time::Duration::from_millis(1500));
    writer.write_all(input.as_bytes()).unwrap();
    writer.flush().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut output = Vec::new();
    while std::time::Instant::now() < deadline {
        if let Ok(all) = rx.recv_timeout(std::time::Duration::from_millis(200)) {
            output = all;
        }
        if child.try_wait().ok().flatten().is_some() {
            while let Ok(all) = rx.recv_timeout(std::time::Duration::from_millis(300)) {
                output = all;
            }
            break;
        }
    }
    let _ = child.kill();
    Some(Run {
        output: String::from_utf8_lossy(&output).into_owned(),
        root,
    })
}

/// The shell started with injection, read the user's own rc, emitted OSC 7 and OSC 133,
/// loaded the integration once, and did not leak the hand-off variables.
///
/// `with_c`: the shell sends the command-start mark (133;C). bash before 4.4 has no PS0
/// and sends none; Caelum then detects the start from Enter, so the script must not.
fn check(run: &Run, rc_marker: &str, with_c: bool) {
    let out = &run.output;
    if std::env::var_os("CAELUM_TEST_VERBOSE").is_some() {
        eprintln!("{out:?}");
    }
    assert!(out.contains(rc_marker), "user rc not sourced:\n{out:?}");
    assert!(out.contains("\x1b]133;A\x07"), "no OSC 133;A:\n{out:?}");
    assert_eq!(
        out.contains("\x1b]133;C\x07"),
        with_c,
        "OSC 133;C present={}, expected {with_c}:\n{out:?}",
        !with_c
    );
    assert!(out.contains("\x1b]133;D;0\x07"), "no OSC 133;D:\n{out:?}");
    assert!(out.contains("\x1b]7;file://"), "no OSC 7:\n{out:?}");
    // One prompt per command plus the first: each prompt emits exactly one A, so a
    // duplicated hook (manual source plus injection) would double the A marks.
    assert_eq!(
        out.matches("\x1b]133;A\x07").count(),
        out.matches(if with_c {
            "\x1b]133;C\x07"
        } else {
            "\x1b]133;D;"
        })
        .count(),
        "integration loaded twice:\n{out:?}"
    );
    assert!(
        out.contains("LEAK=[]"),
        "hand-off variables leaked:\n{out:?}"
    );
    assert!(
        !out.contains(run.root.to_str().unwrap()),
        "injection dir leaked into the environment:\n{out:?}"
    );
}

/// bash has `PS0` (and so sends 133;C) from 4.4.
fn bash_has_ps0() -> bool {
    let Some(bash) = find("bash") else {
        return true;
    };
    let out = Command::new(bash)
        .args(["-c", "echo $((BASH_VERSINFO[0] * 100 + BASH_VERSINFO[1]))"])
        .output()
        .expect("run bash");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u32>()
        .unwrap()
        >= 404
}

#[test]
#[ignore = "needs bash; run with --ignored"]
fn bash_injection_sources_user_rc_and_emits_marks() {
    for manual in [false, true] {
        let rc = if manual {
            "export MYRC=bashrc_ok\nsource @ROOT@/caelum.bash\n"
        } else {
            "export MYRC=bashrc_ok\n"
        };
        let Some(run) = inject(
            "bash",
            &[
                (".bash_profile", ". ~/.bashrc\n".into()),
                (".bashrc", format!("PS1='$ '\n{rc}")),
            ],
            "echo RC=$MYRC; cd /; echo LEAK=[$ENV$CAELUM_BASH_INJECT$CAELUM_BASH_ENV]\ntrue\nexit\n",
        ) else {
            return;
        };
        check(&run, "RC=bashrc_ok", bash_has_ps0());
    }
}

#[test]
#[ignore = "needs zsh; run with --ignored"]
fn zsh_injection_sources_user_rc_and_emits_marks() {
    for manual in [false, true] {
        let rc = if manual {
            "export MYRC=zshrc_ok\nsource @ROOT@/caelum.zsh\n"
        } else {
            "export MYRC=zshrc_ok\n"
        };
        let Some(run) = inject(
            "zsh",
            &[
                (
                    ".zshenv",
                    // Test hook for a relocated zsh whose modules are not in the
                    // compiled-in module path (CAELUM_TEST_ZSH_MODULES) or function path
                    // (CAELUM_TEST_ZSH_FUNCTIONS, the `functions` directory of zsh-common).
                    "export MYENV=zshenv_ok\n[ -n \"$CAELUM_TEST_ZSH_MODULES\" ] && module_path=($CAELUM_TEST_ZSH_MODULES $module_path)\n[ -n \"$CAELUM_TEST_ZSH_FUNCTIONS\" ] && fpath=($CAELUM_TEST_ZSH_FUNCTIONS/Misc $fpath)\n"
                        .into(),
                ),
                (".zshrc", format!("PROMPT='$ '\n{rc}")),
            ],
            "echo RC=$MYRC$MYENV; cd /; echo \"LEAK=[$ZDOTDIR$CAELUM_ZDOTDIR]\"\ntrue\nexit\n",
        ) else {
            return;
        };
        check(&run, "RC=zshrc_okzshenv_ok", true);
    }
}

#[test]
#[ignore = "needs fish; run with --ignored"]
fn fish_injection_sources_user_config_and_emits_marks() {
    for manual in [false, true] {
        let rc = if manual {
            "set -gx MYRC fish_ok\nsource @ROOT@/caelum.fish\n"
        } else {
            "set -gx MYRC fish_ok\n"
        };
        let Some(run) = inject(
            "fish",
            &[(".config/fish/config.fish", rc.into())],
            "echo RC=$MYRC; cd /; echo LEAK=[] XDG=$XDG_DATA_DIRS\ntrue\nexit\n",
        ) else {
            return;
        };
        check(&run, "RC=fish_ok", true);
    }
}

// ---- bash: injected shell must behave like Caelum's plain `bash -l` ----

/// Runs `script` (fed on stdin) in an interactive bash with a scratch HOME holding
/// `dotfiles`; `injected` starts it the way Caelum does, otherwise as plain `bash -l`.
/// Returns stdout. stdin/stdout are pipes, so `-i` is forced.
fn bash_session(dotfiles: &[(&str, &str)], script: &str, injected: bool) -> Option<String> {
    let bash = find("bash")?;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    for (rel, text) in dotfiles {
        std::fs::write(home.join(rel), text).unwrap();
    }
    let root = dir_in(&tmp.path().join("data"));
    install(&root).unwrap();
    let mut cmd = Command::new(&bash);
    cmd.env_clear()
        .env("HOME", &home)
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("CAELUM_RS", "1")
        .env("LANG", "C.UTF-8")
        .current_dir(&home);
    if injected {
        let inj = plan(bash.to_str().unwrap(), &root, &|_| None).unwrap();
        cmd.args(inj.args.unwrap());
        cmd.envs(inj.env);
    } else {
        cmd.arg("-l");
    }
    cmd.arg("-i")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{script}\nexit\n").as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `s` without `ESC ] ... BEL` sequences (Caelum's own marks; a plain shell has none).
fn strip_osc(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("\x1b]") {
        out.push_str(&rest[..i]);
        rest = rest[i..].find('\x07').map_or("", |j| &rest[i + j + 1..]);
    }
    out + rest
}

/// The text from `tag` to the end of its line, for every line containing it.
fn tagged(s: &str, tag: &str) -> Vec<String> {
    strip_osc(s)
        .lines()
        .filter_map(|l| l.find(tag).map(|i| l[i..].to_string()))
        .collect()
}

const RC_PROBE: &str = "echo PROFILE=${PROFILE-} RC=${RC-} RCN=${RCN-}";

#[test]
#[ignore = "needs bash; run with --ignored"]
fn bash_injected_startup_files_match_plain_login_bash() {
    let cases: &[(&str, &[(&str, &str)])] = &[
        ("no files", &[]),
        (
            "profile does not source bashrc",
            &[
                (".bash_profile", "PROFILE=yes\n"),
                (".bashrc", "RC=yes; RCN=${RCN}x\n"),
            ],
        ),
        (
            "bashrc only (a login shell does not read it)",
            &[(".bashrc", "RC=yes; RCN=${RCN}x\n")],
        ),
        (
            "profile delegates to bashrc exactly once",
            &[
                (".bash_profile", "PROFILE=yes\n. ~/.bashrc\n"),
                (".bashrc", "RC=yes; RCN=${RCN}x\n"),
            ],
        ),
        (
            "only ~/.profile",
            &[
                (".profile", "PROFILE=yes\n. ~/.bashrc\n"),
                (".bashrc", "RC=yes; RCN=${RCN}x\n"),
            ],
        ),
    ];
    for (name, files) in cases {
        let (Some(plain), Some(inj)) = (
            bash_session(files, RC_PROBE, false),
            bash_session(files, RC_PROBE, true),
        ) else {
            return;
        };
        assert_eq!(
            tagged(&inj, "PROFILE="),
            tagged(&plain, "PROFILE="),
            "{name}"
        );
        // The shared expectation: RCN counts bashrc sourcings, 0 or 1, never 2.
        assert!(!inj.contains("RCN=xx"), "{name}: rc sourced twice: {inj:?}");
    }
}

/// POSIX mode turns `inherit_errexit` on; after the shim it must be off again, so
/// `set -e; v=$(false; printf survived)` behaves as in a plain bash.
#[test]
#[ignore = "needs bash; run with --ignored"]
fn bash_injection_restores_non_posix_option_state() {
    let snippet = "set -e; v=$(false; printf survived); echo RESULT=$v";
    let (Some(plain), Some(inj)) = (
        bash_session(&[], snippet, false),
        bash_session(&[], snippet, true),
    ) else {
        return;
    };
    assert!(plain.contains("RESULT=survived"), "{plain:?}");
    assert!(inj.contains("RESULT=survived"), "{inj:?}");
    let state = "shopt -p | sort; set -o | sort; echo HS=$HISTSIZE HF=$HISTFILESIZE POSIXLY=${POSIXLY_CORRECT-unset}";
    let (Some(plain), Some(inj)) = (
        bash_session(&[], state, false),
        bash_session(&[], state, true),
    ) else {
        return;
    };
    // `login_shell` is read-only and legitimately differs (Caelum cannot make the
    // injected shell a true login shell without losing ENV).
    let norm = |s: &str| {
        strip_osc(s)
            .lines()
            .filter(|l| !l.ends_with("login_shell"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        norm(&inj),
        norm(&plain),
        "shell options differ from plain bash -l"
    );
}
