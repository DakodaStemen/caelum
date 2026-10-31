//! Symbolic icons from the desktop's icon theme, drawn in the text colour like GTK draws
//! them, plus the flat buttons that host them.
//!
//! Each icon is looked up in the user's icon theme, the themes it inherits, then Adwaita.
//! A copy of the Adwaita icon ships in `assets/icons` for systems without one (macOS,
//! Windows, minimal Linux installs). SVGs are rasterized by `egui_extras` at the physical
//! size they are shown, so they stay sharp at every zoom level.
use caelum_rs::model::Direction;
use eframe::egui::{self, Color32, Image, Rect, Response, Sense, Vec2, emath::GuiRounding};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Menu,
    NewTab,
    Close,
    Sidebar,
    Split,
    Arrow(Direction),
    Balance,
    Maximize,
    Restore,
    WindowMinimize,
    WindowMaximize,
    WindowRestore,
    WindowClose,
    Folder,
    File,
    Find,
    Add,
    PanEnd,
    PanDown,
    ZoomIn,
    ZoomOut,
    Remove,
    Check,
}

macro_rules! bundled {
    ($name:literal) => {
        (
            $name,
            include_bytes!(concat!("../../assets/icons/", $name, "-symbolic.svg")) as &[u8],
        )
    };
}

impl Icon {
    #[cfg(test)]
    pub const ALL: [Icon; 26] = [
        Icon::Menu,
        Icon::NewTab,
        Icon::Close,
        Icon::Sidebar,
        Icon::Split,
        Icon::Arrow(Direction::Up),
        Icon::Arrow(Direction::Down),
        Icon::Arrow(Direction::Left),
        Icon::Arrow(Direction::Right),
        Icon::Balance,
        Icon::Maximize,
        Icon::Restore,
        Icon::WindowMinimize,
        Icon::WindowMaximize,
        Icon::WindowRestore,
        Icon::WindowClose,
        Icon::Folder,
        Icon::File,
        Icon::Find,
        Icon::Add,
        Icon::PanEnd,
        Icon::ZoomIn,
        Icon::ZoomOut,
        Icon::PanDown,
        Icon::Remove,
        Icon::Check,
    ];

    /// The freedesktop icon name, without `-symbolic`, and the bundled Adwaita copy.
    fn source(self) -> (&'static str, &'static [u8]) {
        match self {
            Icon::Menu => bundled!("open-menu"),
            Icon::NewTab => bundled!("tab-new"),
            Icon::Close | Icon::WindowClose => bundled!("window-close"),
            Icon::Sidebar => bundled!("sidebar-show"),
            Icon::Split => bundled!("view-dual"),
            Icon::Arrow(Direction::Up) => bundled!("go-up"),
            Icon::Arrow(Direction::Down) => bundled!("go-down"),
            Icon::Arrow(Direction::Left) => bundled!("go-previous"),
            Icon::Arrow(Direction::Right) => bundled!("go-next"),
            Icon::Balance => bundled!("view-grid"),
            Icon::Maximize => bundled!("view-fullscreen"),
            Icon::Restore => bundled!("view-restore"),
            Icon::WindowMinimize => bundled!("window-minimize"),
            Icon::WindowMaximize => bundled!("window-maximize"),
            Icon::WindowRestore => bundled!("window-restore"),
            Icon::Folder => bundled!("folder"),
            Icon::File => bundled!("text-x-generic"),
            Icon::Find => bundled!("edit-find"),
            Icon::Add => bundled!("list-add"),
            Icon::PanEnd => bundled!("pan-end"),
            Icon::ZoomIn => bundled!("zoom-in"),
            Icon::ZoomOut => bundled!("zoom-out"),
            Icon::PanDown => bundled!("pan-down"),
            Icon::Remove => bundled!("list-remove"),
            Icon::Check => bundled!("object-select"),
        }
    }

    pub fn name(self) -> &'static str {
        self.source().0
    }
}

/// The icon at `size` points, tinted `color`.
pub fn image(icon: Icon, size: f32, color: Color32) -> Image<'static> {
    Image::from_bytes(format!("bytes://icon/{}.svg", icon.name()), svg(icon))
        .fit_to_exact_size(Vec2::splat(size))
        .tint(color)
}

/// Paints `icon` at `size` points (16 = the symbolic size) centered in `rect`, on whole
/// physical pixels so it stays crisp at any zoom.
pub fn paint(ui: &egui::Ui, rect: Rect, icon: Icon, size: f32, color: Color32) {
    let ppp = ui.pixels_per_point();
    let square = Rect::from_center_size(rect.center(), Vec2::splat(size)).round_to_pixels(ppp);
    image(icon, size, color).paint_at(ui, square);
}

/// The recoloured SVG for `icon`, loaded once.
fn svg(icon: Icon) -> &'static [u8] {
    static CACHE: OnceLock<Mutex<HashMap<&'static str, &'static [u8]>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (name, fallback) = icon.source();
    cache.entry(name).or_insert_with(|| {
        let from_theme = theme_index()
            .get(&format!("{name}-symbolic.svg"))
            .and_then(|path| std::fs::read_to_string(path).ok());
        let text = from_theme.unwrap_or_else(|| String::from_utf8_lossy(fallback).into_owned());
        // A handful of small icons, kept for the life of the process.
        Box::leak(whiten(&text).into_bytes().into_boxed_slice())
    })
}

/// Makes every colour in a symbolic icon white so a tint gives it any colour, keeping
/// opacity.
pub fn whiten(svg: &str) -> String {
    const PREFIXES: [&str; 6] = [
        "fill=\"#",
        "stroke=\"#",
        "fill:#",
        "stroke:#",
        "stop-color:#",
        "stop-color=\"#",
    ];
    let mut out = String::with_capacity(svg.len() + 16);
    let mut rest = svg;
    'scan: while !rest.is_empty() {
        for prefix in PREFIXES {
            if let Some(after) = rest.strip_prefix(prefix) {
                let hex = after
                    .bytes()
                    .take_while(u8::is_ascii_hexdigit)
                    .take(8)
                    .count();
                if matches!(hex, 3 | 4 | 6 | 8) {
                    out.push_str(prefix);
                    out.push_str("ffffff");
                    rest = &after[hex..];
                    continue 'scan;
                }
            }
        }
        let next = rest.chars().next().map_or(1, char::len_utf8);
        out.push_str(&rest[..next]);
        rest = &rest[next..];
    }
    // Shapes without a fill default to black; give the root a white one they inherit.
    if let Some(start) = out.find("<svg") {
        let tag_end = out[start..].find('>').map_or(out.len(), |i| start + i);
        if !out[start..tag_end].contains(" fill=") {
            out.insert_str(start + 4, " fill=\"#ffffff\"");
        }
    }
    out
}

/// Every `*-symbolic.svg` in the user's icon theme and its fallbacks, by file name.
/// Themes earlier in the search order win.
fn theme_index() -> &'static HashMap<String, PathBuf> {
    static INDEX: OnceLock<HashMap<String, PathBuf>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut index = HashMap::new();
        if cfg!(any(target_os = "windows", target_os = "macos")) {
            return index;
        }
        let roots = icon_roots();
        let mut themes: Vec<String> = Vec::new();
        let mut queue: Vec<String> = current_theme().into_iter().collect();
        queue.push("Adwaita".to_owned());
        while !queue.is_empty() {
            let theme = queue.remove(0);
            if themes.contains(&theme) || theme == "hicolor" || themes.len() > 8 {
                continue;
            }
            for root in &roots {
                if let Ok(index_file) =
                    std::fs::read_to_string(root.join(&theme).join("index.theme"))
                {
                    queue.extend(inherits(&index_file));
                }
            }
            themes.push(theme);
        }
        for theme in &themes {
            for root in &roots {
                collect(&root.join(theme), 0, &mut index);
            }
        }
        index
    })
}

/// The themes named on an `index.theme`'s `Inherits=` line.
pub fn inherits(index_file: &str) -> Vec<String> {
    index_file
        .lines()
        .find_map(|line| line.trim().strip_prefix("Inherits="))
        .map(|list| {
            list.split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn collect(dir: &Path, depth: u32, index: &mut HashMap<String, PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth < 3 {
                collect(&path, depth + 1, index);
            }
        } else if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.ends_with("-symbolic.svg")
        {
            index.entry(name.to_owned()).or_insert(path);
        }
    }
}

/// Where icon themes live, in freedesktop search order.
fn icon_roots() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut roots = Vec::new();
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.as_ref().map(|h| h.join(".local/share")));
    if let Some(dir) = data_home {
        roots.push(dir.join("icons"));
    }
    if let Some(home) = &home {
        roots.push(home.join(".icons"));
    }
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    roots.extend(
        data_dirs
            .split(':')
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .map(|p| p.join("icons")),
    );
    roots
}

/// The desktop's icon theme, from GNOME's settings.
fn current_theme() -> Option<String> {
    let out = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "icon-theme"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let value = String::from_utf8(out.stdout).ok()?;
    let name = value.trim().trim_matches('\'');
    (!name.is_empty()).then(|| name.to_owned())
}

/// Colors for a flat button on a surface of `base` color with foreground `fg`.
#[derive(Clone, Copy)]
pub struct Palette {
    pub base: Color32,
    pub fg: Color32,
}
impl Palette {
    fn fill(self, response: &Response, on: bool) -> Color32 {
        // libadwaita flat buttons: 7% of the text colour on hover, 10% when checked,
        // 16% while pressed.
        let t = if response.is_pointer_button_down_on() {
            0.16
        } else if on {
            0.10
        } else if response.hovered() {
            0.07
        } else {
            return Color32::TRANSPARENT;
        };
        caelum_rs::theme::mix(self.base, self.fg, t)
    }
}

/// A flat (frameless until hovered) icon button, like a button in a GNOME headerbar.
/// `label` is both the tooltip and the accessible name.
pub fn flat(
    ui: &mut egui::Ui,
    icon: Icon,
    size: Vec2,
    icon_size: f32,
    pal: Palette,
    label: &str,
    on: bool,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    if ui.is_rect_visible(rect) {
        let fill = pal.fill(&response, on);
        if fill != Color32::TRANSPARENT {
            let radius = (size.min_elem() * 0.18).round().clamp(4., 8.) as u8;
            let painter = ui.painter();
            painter.rect_filled(
                rect.round_to_pixels(painter.pixels_per_point()),
                radius,
                fill,
            );
        }
        paint(ui, rect, icon, icon_size, pal.fg);
    }
    response.on_hover_text(label)
}

/// A softly filled square icon button, for button grids in popovers.
pub fn tile(ui: &mut egui::Ui, icon: Icon, size: Vec2, pal: Palette, label: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    if ui.is_rect_visible(rect) {
        let t = if response.is_pointer_button_down_on() {
            0.20
        } else if response.hovered() {
            0.12
        } else {
            0.06
        };
        let painter = ui.painter();
        painter.rect_filled(
            rect.round_to_pixels(painter.pixels_per_point()),
            8,
            caelum_rs::theme::mix(pal.base, pal.fg, t),
        );
        paint(ui, rect, icon, 16., pal.fg);
    }
    response
}

/// A round window-control button (libadwaita: a 24px circle in a 34px slot, always
/// tinted, darker on hover).
pub fn window_control(ui: &mut egui::Ui, icon: Icon, pal: Palette, label: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(34., 34.), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    if ui.is_rect_visible(rect) {
        let t = if response.is_pointer_button_down_on() {
            0.25
        } else if response.hovered() {
            0.15
        } else {
            0.10
        };
        let painter = ui.painter();
        let center = rect.center().round_to_pixels(painter.pixels_per_point());
        painter.circle_filled(center, 12., caelum_rs::theme::mix(pal.base, pal.fg, t));
        paint(ui, rect, icon, 16., pal.fg);
    }
    response.on_hover_text(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_become_white_and_opacity_stays() {
        let svg = r##"<svg width="16"><path fill="#2e3436" d="M0"/><path style="fill:#222;opacity:.35" fill-opacity="0.34902"/></svg>"##;
        let out = whiten(svg);
        assert!(out.contains(r##"fill="#ffffff" d="M0""##));
        assert!(out.contains("fill:#ffffff;opacity:.35"));
        assert!(out.contains(r#"fill-opacity="0.34902""#));
        assert!(!out.contains("2e3436"));
    }

    #[test]
    fn shapes_without_a_fill_inherit_white() {
        let out = whiten(r#"<svg width="16"><path d="M0"/></svg>"#);
        assert!(out.starts_with(r##"<svg fill="#ffffff" width="16">"##));
        let already = whiten(r##"<svg fill="#000" width="16"/>"##);
        assert_eq!(already.matches("fill=").count(), 1);
    }

    #[test]
    fn element_ids_are_left_alone() {
        let out = whiten(r##"<svg fill="#fff"><use href="#abc"/><path fill="url(#abc)"/></svg>"##);
        assert!(out.contains(r##"href="#abc""##));
        assert!(out.contains("url(#abc)"));
    }

    #[test]
    fn inherited_themes_are_read_from_index_theme() {
        let file = "[Icon Theme]\nName=Papirus\nInherits=breeze-dark, hicolor\n";
        assert_eq!(inherits(file), ["breeze-dark", "hicolor"]);
        assert!(inherits("[Icon Theme]\n").is_empty());
    }

    #[test]
    fn every_bundled_icon_is_a_valid_symbolic_svg() {
        for icon in Icon::ALL {
            let text = std::str::from_utf8(icon.source().1).unwrap();
            assert!(text.contains("<svg"), "{icon:?}");
            assert!(!whiten(text).contains("2e3436"), "{icon:?}");
        }
    }
}
