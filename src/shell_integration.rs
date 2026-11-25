//! Automatic shell integration: makes the user's shell emit OSC 7 / OSC 133 without
//! any dotfile edits, in the style of Ghostty and Kitty.
//!
//! * bash: started as `bash --posix` with `ENV` pointing at a shim (bash reads `ENV`
//!   only in POSIX mode). The shim restores `ENV`, leaves POSIX mode, runs the login
//!   files a `-l` shell would, then loads `caelum.bash`.
//! * zsh: `ZDOTDIR` points at a directory whose `.zshenv` restores the user's
//!   `ZDOTDIR`, runs their `.zshenv`, and loads `caelum.zsh` before the first prompt
//!   (after their `.zshrc`).
//! * fish: the data directory is prepended to `XDG_DATA_DIRS` so fish loads its
//!   `vendor_conf.d/caelum.fish`, which removes that entry again.
//!
//! The scripts are embedded in the binary and written to the profile directory, so an
//! installed binary and a dev checkout behave the same. Every script is idempotent
//! (a user who already sources `caelum.*` by hand gets no duplicate hooks) and unsets
//! the variables used for the hand-off so child shells never see them.
use std::path::{Path, PathBuf};

const CAELUM_BASH: &str = include_str!("../shell/caelum.bash");
const CAELUM_ZSH: &str = include_str!("../shell/caelum.zsh");
const CAELUM_FISH: &str = include_str!("../shell/caelum.fish");
const BASH_INJECT: &str = include_str!("../shell/inject/bash-inject.bash");
const ZSH_ENV: &str = include_str!("../shell/inject/zdotdir/.zshenv");
const FISH_INJECT: &str = include_str!("../shell/inject/fish/vendor_conf.d/caelum.fish");

/// Where each embedded script lives under the integration directory.
const FILES: &[(&str, &str)] = &[
    ("caelum.bash", CAELUM_BASH),
    ("caelum.zsh", CAELUM_ZSH),
    ("caelum.fish", CAELUM_FISH),
    ("bash/inject.bash", BASH_INJECT),
    ("zsh/.zshenv", ZSH_ENV),
    ("fish/vendor_conf.d/caelum.fish", FISH_INJECT),
];

/// What to change about the shell's command line and environment.
#[derive(Debug, PartialEq, Eq)]
pub struct Injection {
    /// Replaces the shell's default arguments when set.
    pub args: Option<Vec<String>>,
    pub env: Vec<(String, String)>,
}

/// The directory the embedded scripts are written to for a given profile directory.
pub fn dir_in(profile: &Path) -> PathBuf {
    profile.join("shell-integration")
}

/// Writes the embedded scripts under `root`, replacing any that differ (so an
/// upgraded binary refreshes them). Each file is written to a temporary name and
/// renamed, so a concurrently starting second Caelum never sees a partial script.
pub fn install(root: &Path) -> std::io::Result<()> {
    for (rel, text) in FILES {
        let path = root.join(rel);
        if std::fs::read_to_string(&path).is_ok_and(|have| have == *text) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(())
}

/// The injection for `shell`, or `None` for a shell Caelum does not know (including
/// PowerShell and cmd on Windows). `env` reads the environment the shell would
/// inherit, so tests need not touch the real one.
pub fn plan(shell: &str, root: &Path, env: &dyn Fn(&str) -> Option<String>) -> Option<Injection> {
    let name = Path::new(shell).file_name()?.to_str()?;
    let name = name.strip_suffix(".exe").unwrap_or(name);
    let root_str = root.to_str()?;
    let set = |k: &str, v: &str| (k.to_string(), v.to_string());
    let non_empty = |k: &str| env(k).filter(|v| !v.is_empty());
    match name {
        "bash" => {
            let mut vars = vec![
                set("ENV", &format!("{root_str}/bash/inject.bash")),
                set("CAELUM_BASH_INJECT", "1"),
            ];
            if let Some(orig) = non_empty("ENV") {
                vars.push(set("CAELUM_BASH_ENV", &orig));
            }
            Some(Injection {
                args: Some(vec!["--posix".into()]),
                env: vars,
            })
        }
        "zsh" => {
            let mut vars = vec![set("ZDOTDIR", &format!("{root_str}/zsh"))];
            if let Some(orig) = non_empty("ZDOTDIR") {
                vars.push(set("CAELUM_ZDOTDIR", &orig));
            }
            Some(Injection {
                args: None,
                env: vars,
            })
        }
        "fish" => {
            // XDG_DATA_DIRS is colon separated: a directory containing one cannot be listed.
            if root_str.contains(':') {
                return None;
            }
            let rest =
                non_empty("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into());
            Some(Injection {
                args: None,
                env: vec![set("XDG_DATA_DIRS", &format!("{root_str}:{rest}"))],
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }
    fn var<'a>(i: &'a Injection, k: &str) -> Option<&'a str> {
        i.env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }

    #[test]
    fn bash_goes_through_a_posix_env_shim_and_remembers_the_users_env() {
        let root = Path::new("/data/si");
        let i = plan("/bin/bash", root, &lookup(&[])).unwrap();
        assert_eq!(i.args.as_deref(), Some(&["--posix".to_string()][..]));
        assert_eq!(var(&i, "ENV"), Some("/data/si/bash/inject.bash"));
        assert_eq!(var(&i, "CAELUM_BASH_INJECT"), Some("1"));
        assert_eq!(var(&i, "CAELUM_BASH_ENV"), None);
        let i = plan("/usr/bin/bash", root, &lookup(&[("ENV", "/home/u/.env")])).unwrap();
        assert_eq!(var(&i, "CAELUM_BASH_ENV"), Some("/home/u/.env"));
    }
    #[test]
    fn zsh_gets_a_zdotdir_wrapper_and_keeps_the_users_zdotdir() {
        let root = Path::new("/data/si");
        let i = plan("/bin/zsh", root, &lookup(&[])).unwrap();
        assert_eq!(i.args, None);
        assert_eq!(var(&i, "ZDOTDIR"), Some("/data/si/zsh"));
        assert_eq!(var(&i, "CAELUM_ZDOTDIR"), None);
        let i = plan("/bin/zsh", root, &lookup(&[("ZDOTDIR", "/home/u/zd")])).unwrap();
        assert_eq!(var(&i, "CAELUM_ZDOTDIR"), Some("/home/u/zd"));
    }
    #[test]
    fn fish_prepends_the_data_dir_to_xdg_data_dirs() {
        let root = Path::new("/data/si");
        let i = plan("/usr/bin/fish", root, &lookup(&[])).unwrap();
        assert_eq!(
            var(&i, "XDG_DATA_DIRS"),
            Some("/data/si:/usr/local/share:/usr/share")
        );
        let i = plan(
            "/usr/bin/fish",
            root,
            &lookup(&[("XDG_DATA_DIRS", "/a:/b")]),
        )
        .unwrap();
        assert_eq!(var(&i, "XDG_DATA_DIRS"), Some("/data/si:/a:/b"));
        assert!(plan("/usr/bin/fish", Path::new("/da:ta"), &lookup(&[])).is_none());
    }
    #[test]
    fn other_shells_are_left_alone() {
        for s in ["/bin/sh", "/usr/bin/nu", "pwsh.exe", "cmd.exe", ""] {
            assert!(plan(s, Path::new("/d"), &lookup(&[])).is_none(), "{s}");
        }
        assert!(plan("bash.exe", Path::new("/d"), &lookup(&[])).is_some());
    }
    #[test]
    fn install_writes_every_script_and_is_idempotent() {
        let d = tempfile::tempdir().unwrap();
        let root = dir_in(d.path());
        install(&root).unwrap();
        for (rel, text) in FILES {
            assert_eq!(&std::fs::read_to_string(root.join(rel)).unwrap(), text);
        }
        // A stale copy is refreshed; an identical one is not rewritten.
        std::fs::write(root.join("caelum.bash"), "old").unwrap();
        install(&root).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("caelum.bash")).unwrap(),
            CAELUM_BASH
        );
        let before = std::fs::metadata(root.join("caelum.zsh"))
            .unwrap()
            .modified()
            .unwrap();
        install(&root).unwrap();
        let after = std::fs::metadata(root.join("caelum.zsh"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after);
        assert!(
            !std::fs::read_dir(&root)
                .unwrap()
                .any(|e| { e.unwrap().file_name().to_string_lossy().contains(".tmp") })
        );
    }
}
