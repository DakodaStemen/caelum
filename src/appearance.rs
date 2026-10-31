//! The desktop's look: light/dark preference, accent color, UI font and whether the
//! window should draw its own GNOME-style headerbar.
//!
//! No extra crates: on Linux the settings come from the freedesktop portal via
//! `gdbus` (part of GLib, present on every GNOME/GTK desktop), falling back to
//! `gsettings`; macOS reads `defaults`, Windows the registry via `reg`. Every probe
//! is a short-lived child process with a hard timeout, so a missing tool or a hung
//! D-Bus only means the built-in defaults are used.
use eframe::egui::Color32;
use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Light,
    Dark,
}

/// Window buttons the desktop shows, and on which side.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ButtonLayout {
    pub left: Vec<WindowButton>,
    pub right: Vec<WindowButton>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowButton {
    Minimize,
    Maximize,
    Close,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemLook {
    /// `None` when the desktop states no preference.
    pub scheme: Option<Scheme>,
    pub accent: Option<Color32>,
    /// Family and size in points (e.g. "Adwaita Sans", 11.0).
    pub ui_font: Option<(String, f32)>,
    pub buttons: ButtonLayout,
}

/// libadwaita's default accent (blue 3).
pub const DEFAULT_ACCENT: Color32 = Color32::from_rgb(0x35, 0x84, 0xe4);

impl SystemLook {
    pub fn accent(&self) -> Color32 {
        self.accent.unwrap_or(DEFAULT_ACCENT)
    }
}

/// Reads everything; takes a few tens of milliseconds, so call it off the UI thread.
pub fn detect() -> SystemLook {
    let mut look = detect_colors();
    look.ui_font = detect_font();
    look.buttons = detect_buttons();
    look
}

/// Only the light/dark preference and accent (cheap enough to re-check on focus).
pub fn detect_colors() -> SystemLook {
    #[cfg(target_os = "linux")]
    {
        linux::colors()
    }
    #[cfg(target_os = "macos")]
    {
        macos::colors()
    }
    #[cfg(windows)]
    {
        windows::colors()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        SystemLook::default()
    }
}

fn detect_font() -> Option<(String, f32)> {
    #[cfg(target_os = "linux")]
    {
        linux::font()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn detect_buttons() -> ButtonLayout {
    #[cfg(target_os = "linux")]
    if let Some(layout) = run("gsettings", &["get", WM_SCHEMA, "button-layout"]) {
        return parse_button_layout(&layout);
    }
    ButtonLayout {
        left: vec![],
        right: vec![WindowButton::Close],
    }
}
#[cfg(target_os = "linux")]
const WM_SCHEMA: &str = "org.gnome.desktop.wm.preferences";

/// Whether to replace the platform title bar with an in-app headerbar. Only GNOME on
/// Wayland: it has no server-side decorations, so the alternative is winit's plain
/// fallback frame stacked on top of our toolbar. `CAELUM_RS_CSD=0/1` overrides.
pub fn client_side_decorations() -> bool {
    match std::env::var("CAELUM_RS_CSD").as_deref() {
        Ok("1") => return true,
        Ok("0") => return false,
        _ => {}
    }
    if !cfg!(target_os = "linux") {
        return false;
    }
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
        && std::env::var("XDG_SESSION_TYPE").map_or(true, |t| t != "x11");
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    wayland
        && desktop
            .split(':')
            .any(|d| d.eq_ignore_ascii_case("GNOME") || d.eq_ignore_ascii_case("ubuntu"))
}

/// Runs a probe, giving up after half a second. `None` on any failure.
pub(crate) fn run(program: &str, args: &[&str]) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(4)),
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(child.stdout.as_mut()?, &mut out).ok()?;
    Some(out.trim().to_string())
}

/// `(<uint32 1>,)` (ReadOne) or `(<<uint32 1>>,)` (Read) -> 1.
pub fn parse_portal_u32(text: &str) -> Option<u32> {
    let rest = &text[text.find("uint32 ")? + 7..];
    rest.split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}
/// `(<(0.9, 0.17, 0.25)>,)` -> that color; components outside 0..=1 mean "unset".
pub fn parse_portal_rgb(text: &str) -> Option<Color32> {
    let inner = text.trim_start_matches(['(', '<']);
    let inner = &inner[..inner.find(')')?];
    let parts: Vec<f32> = inner
        .split(',')
        .map(|p| p.trim().parse::<f32>())
        .collect::<Result<_, _>>()
        .ok()?;
    if parts.len() != 3 || parts.iter().any(|v| !(0.0..=1.0).contains(v)) {
        return None;
    }
    let c = |v: f32| (v * 255.).round() as u8;
    Some(Color32::from_rgb(c(parts[0]), c(parts[1]), c(parts[2])))
}
/// GNOME's named accents (`gsettings get org.gnome.desktop.interface accent-color`).
pub fn named_accent(name: &str) -> Option<Color32> {
    let rgb = match name.trim_matches('\'') {
        "blue" => 0x3584e4,
        "teal" => 0x2190a4,
        "green" => 0x3a944a,
        "yellow" => 0xc88800,
        "orange" => 0xed5b00,
        "red" => 0xe62d42,
        "pink" => 0xd56199,
        "purple" => 0x9141ac,
        "slate" => 0x6f8396,
        _ => return None,
    };
    Some(Color32::from_rgb(
        (rgb >> 16) as u8,
        (rgb >> 8) as u8,
        rgb as u8,
    ))
}
/// `'Adwaita Sans 11'` -> ("Adwaita Sans", 11.0).
pub fn parse_font_name(text: &str) -> Option<(String, f32)> {
    let text = text.trim().trim_matches('\'');
    let (family, size) = text.rsplit_once(' ')?;
    let size: f32 = size.parse().ok()?;
    // Drop style words GTK allows in the name; fontconfig picks regular by default.
    let family = family
        .split(' ')
        .filter(|w| !["Regular", "Medium", "Book"].contains(w))
        .collect::<Vec<_>>()
        .join(" ");
    (!family.is_empty() && (6.0..=30.0).contains(&size)).then_some((family, size))
}
/// `'appmenu:minimize,maximize,close'` -> left [], right [min, max, close].
pub fn parse_button_layout(text: &str) -> ButtonLayout {
    let text = text.trim().trim_matches('\'');
    let (left, right) = text.split_once(':').unwrap_or(("", text));
    let parse = |side: &str| {
        side.split(',')
            .filter_map(|b| match b.trim() {
                "minimize" => Some(WindowButton::Minimize),
                "maximize" => Some(WindowButton::Maximize),
                "close" => Some(WindowButton::Close),
                _ => None,
            })
            .collect()
    };
    ButtonLayout {
        left: parse(left),
        right: parse(right),
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    const IFACE: &str = "org.gnome.desktop.interface";
    fn portal(key: &str) -> Option<String> {
        let args = [
            "call",
            "--session",
            "--timeout",
            "1",
            "--dest",
            "org.freedesktop.portal.Desktop",
            "--object-path",
            "/org/freedesktop/portal/desktop",
            "--method",
        ];
        let mut read_one = args.to_vec();
        read_one.extend([
            "org.freedesktop.portal.Settings.ReadOne",
            "org.freedesktop.appearance",
            key,
        ]);
        run("gdbus", &read_one).or_else(|| {
            // Portals older than v2 only have the deprecated Read method.
            let mut read = args.to_vec();
            read.extend([
                "org.freedesktop.portal.Settings.Read",
                "org.freedesktop.appearance",
                key,
            ]);
            run("gdbus", &read)
        })
    }
    pub fn colors() -> SystemLook {
        let scheme = portal("color-scheme")
            .and_then(|t| parse_portal_u32(&t))
            .map(|v| match v {
                1 => Some(Scheme::Dark),
                // 0 is "no preference", which GNOME renders light.
                _ => Some(Scheme::Light),
            })
            .or_else(|| {
                let v = run("gsettings", &["get", IFACE, "color-scheme"])?;
                Some(match v.trim_matches('\'') {
                    "prefer-dark" => Some(Scheme::Dark),
                    _ => Some(Scheme::Light),
                })
            })
            .flatten();
        let accent = portal("accent-color")
            .and_then(|t| parse_portal_rgb(&t))
            .or_else(|| named_accent(&run("gsettings", &["get", IFACE, "accent-color"])?));
        SystemLook {
            scheme,
            accent,
            ..Default::default()
        }
    }
    pub fn font() -> Option<(String, f32)> {
        run("gsettings", &["get", IFACE, "font-name"]).and_then(|t| parse_font_name(&t))
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    pub fn colors() -> SystemLook {
        // The key only exists in dark mode.
        let dark = run("defaults", &["read", "-g", "AppleInterfaceStyle"])
            .is_some_and(|v| v.eq_ignore_ascii_case("dark"));
        let accent = run("defaults", &["read", "-g", "AppleAccentColor"])
            .and_then(|v| v.parse::<i32>().ok())
            .and_then(|v| {
                let rgb: u32 = match v {
                    -1 => 0x8c8c8c,
                    0 => 0xff5257,
                    1 => 0xf7821b,
                    2 => 0xffc600,
                    3 => 0x62ba46,
                    5 => 0xa550a7,
                    6 => 0xf74f9e,
                    _ => return None,
                };
                Some(Color32::from_rgb(
                    (rgb >> 16) as u8,
                    (rgb >> 8) as u8,
                    rgb as u8,
                ))
            })
            .or(Some(Color32::from_rgb(0x00, 0x7a, 0xff)));
        SystemLook {
            scheme: Some(if dark { Scheme::Dark } else { Scheme::Light }),
            accent,
            ..Default::default()
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    pub fn colors() -> SystemLook {
        let light = run(
            "reg",
            &[
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
                "/v",
                "AppsUseLightTheme",
            ],
        )
        .map(|out| out.contains("0x1"));
        SystemLook {
            scheme: light.map(|l| if l { Scheme::Light } else { Scheme::Dark }),
            accent: Some(Color32::from_rgb(0x00, 0x67, 0xc0)),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_portal_replies() {
        assert_eq!(parse_portal_u32("(<uint32 1>,)"), Some(1));
        assert_eq!(parse_portal_u32("(<<uint32 2>>,)"), Some(2));
        assert_eq!(parse_portal_u32("(<'x'>,)"), None);
        assert_eq!(
            parse_portal_rgb(
                "(<(0.90196079015731812, 0.17647059261798859, 0.25882354378700256)>,)"
            ),
            Some(Color32::from_rgb(230, 45, 66))
        );
        assert_eq!(
            parse_portal_rgb("(<<(0.0, 0.5, 1.0)>>,)"),
            Some(Color32::from_rgb(0, 128, 255))
        );
        // Out-of-range components mean "no accent set".
        assert_eq!(parse_portal_rgb("(<(-1.0, -1.0, -1.0)>,)"), None);
    }
    #[test]
    fn parses_gsettings_values() {
        assert_eq!(
            named_accent("'red'"),
            Some(Color32::from_rgb(0xe6, 0x2d, 0x42))
        );
        assert_eq!(named_accent("'nope'"), None);
        assert_eq!(
            parse_font_name("'Adwaita Sans 11'"),
            Some(("Adwaita Sans".into(), 11.))
        );
        assert_eq!(
            parse_font_name("'Cantarell Regular 10.5'"),
            Some(("Cantarell".into(), 10.5))
        );
        assert_eq!(parse_font_name("''"), None);
        assert_eq!(
            parse_button_layout("'appmenu:close'"),
            ButtonLayout {
                left: vec![],
                right: vec![WindowButton::Close]
            }
        );
        assert_eq!(
            parse_button_layout("'close,minimize:appmenu'"),
            ButtonLayout {
                left: vec![WindowButton::Close, WindowButton::Minimize],
                right: vec![]
            }
        );
    }
}
