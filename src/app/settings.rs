use super::adw;
use super::*;
use egui::emath::GuiRounding;

impl App {
    /// Installs resolved fonts when they arrive and starts resolving a changed
    /// terminal font on a worker thread so `fc-match` never blocks the UI.
    pub(super) fn apply_font(&mut self, ctx: &egui::Context) {
        let mut changed = false;
        if let Some(rx) = &self.font_rx {
            match rx.try_recv() {
                Ok(bytes) => {
                    self.font_rx = None;
                    self.mono_font = bytes;
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.font_rx = None,
            }
        }
        if let Some(rx) = &self.ui_font_rx {
            match rx.try_recv() {
                Ok(bytes) => {
                    self.ui_font_rx = None;
                    self.ui_font = bytes;
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.ui_font_rx = None,
            }
        }
        if let Some(rx) = &self.fallback_rx
            && let Ok(list) = rx.try_recv()
        {
            self.fallback_rx = None;
            self.fallback_fonts = list;
            changed = true;
        }
        if changed {
            // egui's bundled fonts stay as fallbacks for missing glyphs, then the
            // system's fonts for other scripts.
            let mut fonts = egui::FontDefinitions::default();
            for (i, bytes) in self.fallback_fonts.iter().enumerate() {
                let name = format!("system-fallback-{i}");
                fonts.font_data.insert(
                    name.clone(),
                    egui::FontData::from_owned(bytes.clone()).into(),
                );
                for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                    fonts.families.entry(family).or_default().push(name.clone());
                }
            }
            let [mono, styled @ ..] = &self.mono_font;
            for (name, bytes, family) in [
                ("system-ui", &self.ui_font, egui::FontFamily::Proportional),
                ("system-mono", mono, egui::FontFamily::Monospace),
            ] {
                if let Some(bytes) = bytes {
                    fonts.font_data.insert(
                        name.into(),
                        egui::FontData::from_owned(bytes.clone()).into(),
                    );
                    fonts
                        .families
                        .entry(family)
                        .or_default()
                        .insert(0, name.into());
                }
            }
            // The terminal's bold and italic faces, each falling back like the
            // regular one; only registered when the font has them.
            let mono_list = fonts.families[&egui::FontFamily::Monospace].clone();
            for (family, bytes) in caelum_rs::terminal::STYLED_FAMILIES.iter().zip(styled) {
                if let Some(bytes) = bytes {
                    fonts.font_data.insert(
                        (*family).into(),
                        egui::FontData::from_owned(bytes.clone()).into(),
                    );
                    let mut list = vec![(*family).to_string()];
                    list.extend(mono_list.iter().cloned());
                    fonts
                        .families
                        .insert(egui::FontFamily::Name((*family).into()), list);
                }
            }
            ctx.set_fonts(fonts);
        }
        let name = self.state.preferences.terminal_font.clone();
        if name == self.applied_font {
            return;
        }
        self.applied_font = name.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.font_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(resolve_mono(&name));
            ctx.request_repaint();
        });
    }
    /// Resolves the desktop's interface font (e.g. Adwaita Sans) in the background.
    pub(super) fn load_ui_font(&mut self, ctx: &egui::Context, family: Option<String>) {
        let Some(family) = family.filter(|f| !f.is_empty()) else {
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.ui_font_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(resolve_family(&family, false));
            ctx.request_repaint();
        });
    }
    /// Resolves fonts for scripts egui's bundled fonts lack, in the background.
    pub(super) fn load_fallback_fonts(&mut self, ctx: &egui::Context) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.fallback_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(resolve_fallbacks());
            ctx.request_repaint();
        });
    }
    /// The preferences dialog, an `AdwPreferencesDialog`: grouped boxed lists of rows.
    pub(super) fn appearance(&mut self, ctx: &egui::Context) {
        let theme = self.theme();
        let mut draft = self.theme_draft.take();
        let mut save_draft = false;
        let mut cancel_draft = false;
        let title = if draft.is_some() {
            "Customize Theme"
        } else {
            "Preferences"
        };
        let close = adw::dialog(
            ctx,
            "preferences",
            title,
            Vec2::new(640., 760.),
            &theme,
            |ui| {
                if let Some(t) = &mut draft {
                    theme_editor(ui, &theme, t, &mut save_draft, &mut cancel_draft);
                } else {
                    self.preferences_page(ui, &theme, &mut draft);
                }
            },
        );
        if save_draft && let Some(t) = draft.take() {
            self.state.preferences.theme = t.id.clone();
            self.state.preferences.follow_system = false;
            self.state.preferences.custom_themes.push(t);
        }
        if cancel_draft {
            draft = None;
        }
        // Closing the editor returns to the preferences; closing those closes the dialog.
        if close {
            if draft.is_some() {
                draft = None;
            } else {
                self.settings = false;
            }
        }
        self.theme_draft = draft;
    }

    fn preferences_page(&mut self, ui: &mut egui::Ui, theme: &Theme, draft: &mut Option<Theme>) {
        let c = adw::Colors::new(theme);
        let prefs = &mut self.state.preferences;
        adw::group(ui, theme, "Appearance", None, |g| {
            g.row(
                "Follow System Style",
                Some("Adwaita light or dark, with the desktop's accent color"),
                |ui| {
                    adw::switch(ui, &mut prefs.follow_system, &c);
                },
            );
            g.content(|ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(10.);
                // As many columns of at least 96 points as fit, filling the row.
                let avail = ui.available_width();
                let columns = ((avail + 10.) / 106.).floor().max(1.);
                let width = (avail - 10. * (columns - 1.)) / columns - 0.5;
                ui.horizontal_wrapped(|ui| {
                    let customs = prefs.custom_themes.iter().map(|t| (&t.id, t));
                    let mut picked = None;
                    for (id, t) in self.presets.iter().chain(customs) {
                        let selected = !prefs.follow_system && prefs.theme == *id;
                        if swatch(ui, t, selected, width, &c).clicked() {
                            picked = Some(id.clone());
                        }
                    }
                    if let Some(id) = picked {
                        prefs.theme = id;
                        prefs.follow_system = false;
                    }
                });
            });
            if g.link_row(
                "Customize Current Theme",
                Some("Edit every interface and terminal color"),
            )
            .clicked()
            {
                let mut t = theme.clone();
                t.id = model::id();
                t.name = format!("{} Custom", t.name);
                *draft = Some(t);
            }
        });
        adw::group(ui, theme, "Terminal Font", None, |g| {
            g.row("Font", None, |ui| {
                let mut fonts = vec!["System Default", "DejaVu Sans Mono", "Liberation Mono"];
                let current = match prefs.terminal_font.as_str() {
                    "" | "monospace" => "System Default",
                    other => other,
                };
                if !fonts.contains(&current) {
                    fonts.push(current);
                }
                if let Some(i) = adw::dropdown(ui, "font", current, &fonts, &c) {
                    prefs.terminal_font = match fonts[i] {
                        "System Default" => String::new(),
                        name => name.to_string(),
                    };
                }
            });
            g.row("Size", None, |ui| {
                adw::spin(ui, &mut prefs.font_size, 10. ..=28., 1., 0, &c);
            });
            g.row("Line Height", None, |ui| {
                adw::spin(ui, &mut prefs.line_height, 1. ..=2., 0.05, 2, &c);
            });
        });
        adw::group(ui, theme, "Cursor", None, |g| {
            g.row("Shape", None, |ui| {
                let shapes = ["Block", "Bar", "Underline"];
                let current = shapes
                    .iter()
                    .find(|s| s.eq_ignore_ascii_case(&prefs.cursor_style))
                    .copied()
                    .unwrap_or("Block");
                if let Some(i) = adw::dropdown(ui, "cursor", current, &shapes, &c) {
                    prefs.cursor_style = shapes[i].to_lowercase();
                }
            });
            g.row("Blink", None, |ui| {
                adw::switch(ui, &mut prefs.cursor_blink, &c);
            });
        });
        adw::group(ui, theme, "Behavior", None, |g| {
            g.row(
                "Confirm Multi-line Paste",
                Some("Ask before pasting several lines into a program without bracketed paste"),
                |ui| {
                    adw::switch(ui, &mut prefs.confirm_multiline_paste, &c);
                },
            );
            g.row("Show Workspaces Sidebar", None, |ui| {
                adw::switch(ui, &mut prefs.sidebar, &c);
            });
        });
        let mut export = false;
        let mut import = false;
        adw::group(
            ui,
            theme,
            "Share Themes",
            Some("Export the current theme as JSON, or import one from a file."),
            |g| {
                g.content(|ui| {
                    ui.add(adw::entry(
                        &mut self.theme_path,
                        "Absolute path to a theme JSON file",
                        &c,
                    ));
                    ui.add_space(4.);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.;
                        let w = (ui.available_width() - 12.) / 2.;
                        export = adw::button(ui, "Export", adw::Kind::Normal, w, &c).clicked();
                        import = adw::button(ui, "Import", adw::Kind::Normal, w, &c).clicked();
                    });
                });
            },
        );
        if export {
            let result = (|| -> Result<()> {
                let path = PathBuf::from(&self.theme_path);
                if !path.is_absolute() {
                    bail!("Choose an absolute file path");
                }
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)?;
                file.write_all(&serde_json::to_vec_pretty(theme)?)?;
                Ok(())
            })();
            self.message = match result {
                Ok(()) => "Theme exported".into(),
                Err(e) => e.to_string(),
            };
        }
        if import {
            let result = (|| -> Result<Theme> {
                let mut t: Theme = serde_json::from_slice(&fs::read(&self.theme_path)?)?;
                t.validate()?;
                t.id = model::id();
                Ok(t)
            })();
            match result {
                Ok(t) => {
                    self.state.preferences.theme = t.id.clone();
                    self.state.preferences.follow_system = false;
                    self.state.preferences.custom_themes.push(t);
                }
                Err(e) => self.message = e.to_string(),
            }
        }
        ui.add_space(14.);
        ui.label(
            RichText::new(format!("Settings are stored in {}", self.profile.display()))
                .small()
                .color(c.muted),
        );
    }
}

/// A theme preview tile: its window color, name, and a strip of its terminal colors;
/// the selected one gets an accent ring and check.
fn swatch(
    ui: &mut egui::Ui,
    t: &Theme,
    selected: bool,
    width: f32,
    c: &adw::Colors,
) -> egui::Response {
    let size = Vec2::new(width, 64.);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, selected, &t.name)
    });
    let ppp = ui.pixels_per_point();
    let rect = rect.round_to_pixels(ppp);
    let painter = ui.painter();
    if selected {
        painter.rect_stroke(
            rect.expand(3.),
            13,
            Stroke::new(2_f32, c.accent),
            egui::StrokeKind::Inside,
        );
    } else if response.hovered() {
        painter.rect_stroke(
            rect.expand(3.),
            13,
            Stroke::new(2_f32, theme::mix(c.card, c.text, 0.25)),
            egui::StrokeKind::Inside,
        );
    }
    let inner = rect.shrink(1.);
    painter.rect_filled(inner, 10, t.term("background"));
    painter.rect_stroke(
        inner,
        10,
        Stroke::new(1_f32, Color32::from_white_alpha(18)),
        egui::StrokeKind::Inside,
    );
    let name = super::truncated(
        ui,
        &t.name,
        egui::TextStyle::Small,
        t.term("foreground"),
        inner.width() - 12.,
    );
    ui.painter().galley(
        Pos2::new(inner.left() + 8., inner.top() + 9.).round_to_pixels(ppp),
        name,
        t.term("foreground"),
    );
    let colors = ["red", "green", "yellow", "blue", "magenta", "cyan"];
    for (i, key) in colors.iter().enumerate() {
        let center = Pos2::new(inner.left() + 13. + i as f32 * 13., inner.bottom() - 15.);
        ui.painter().circle_filled(center, 4.5, t.term(key));
    }
    response.on_hover_text(&t.name)
}

/// `borderStrong` → `Border Strong`, for theme color keys.
fn title_case(key: &str) -> String {
    let mut out = String::new();
    for (i, ch) in key.chars().enumerate() {
        if i == 0 {
            out.extend(ch.to_uppercase());
        } else if ch.is_uppercase() {
            out.push(' ');
            out.push(ch);
        } else {
            out.push(ch);
        }
    }
    out
}

/// The theme editor page: name and style, then every color as a row.
fn theme_editor(
    ui: &mut egui::Ui,
    theme: &Theme,
    t: &mut Theme,
    save: &mut bool,
    cancel: &mut bool,
) {
    let c = adw::Colors::new(theme);
    ui.label(RichText::new("Changes preview live.").color(c.muted));
    adw::group(ui, theme, "Theme", None, |g| {
        g.content(|ui| {
            ui.add(adw::entry(&mut t.name, "Name", &c));
        });
        g.row("Style", None, |ui| {
            let kinds = ["Dark", "Light"];
            let current = if t.kind == "light" { "Light" } else { "Dark" };
            if let Some(i) = adw::dropdown(ui, "kind", current, &kinds, &c) {
                t.kind = kinds[i].to_lowercase();
            }
        });
    });
    for (name, values) in [
        ("Interface", &mut t.ui),
        ("Terminal", &mut t.terminal),
        ("Code", &mut t.syntax),
    ] {
        if values.is_empty() {
            continue;
        }
        adw::group(ui, theme, name, None, |g| {
            for (key, value) in values.iter_mut() {
                g.row(&title_case(key), None, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(value)
                            .desired_width(96.)
                            .margin(Vec2::new(8., 6.)),
                    );
                    if let Some(color) = theme::color(value) {
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(22.), Sense::hover());
                        ui.painter().rect_filled(rect, 6, color);
                        ui.painter().rect_stroke(
                            rect,
                            6,
                            Stroke::new(1_f32, theme::mix(c.card, c.text, 0.3)),
                            egui::StrokeKind::Inside,
                        );
                    }
                });
            }
        });
    }
    ui.add_space(12.);
    if let Err(e) = t.validate() {
        ui.colored_label(c.danger, e.to_string());
    }
    ui.add_space(6.);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.;
        let w = (ui.available_width() - 12.) / 2.;
        *cancel = adw::button(ui, "Cancel", adw::Kind::Normal, w, &c).clicked();
        let valid = t.validate().is_ok();
        let r = ui.add_enabled_ui(valid, |ui| {
            adw::button(ui, "Save Theme", adw::Kind::Suggested, w, &c)
        });
        *save = valid && r.inner.clicked();
    });
}

/// The terminal font's regular, bold, italic and bold italic faces.
pub(super) type MonoFaces = [Option<Vec<u8>>; 4];

/// Resolves the terminal font family (fontconfig first, then standard font
/// directories) and, through fontconfig, its bold and italic faces. A style the
/// family lacks (fontconfig answers with the regular file) is left out.
fn resolve_mono(name: &str) -> MonoFaces {
    let regular = resolve_family(name, true);
    let styled = |style: &str| -> Option<Vec<u8>> {
        let regular = regular.as_ref()?;
        let family = if name.is_empty() { "monospace" } else { name };
        let output = std::process::Command::new("fc-match")
            .args(["-f", "%{file}", "--", &format!("{family}:{style}")])
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        fs::read(String::from_utf8_lossy(&output.stdout).trim())
            .ok()
            .filter(|b| b != regular && ab_glyph::FontRef::try_from_slice(b).is_ok())
    };
    let bold = styled("weight=bold");
    let italic = styled("slant=italic");
    let bold_italic = styled("weight=bold:slant=italic")
        .filter(|b| Some(b) != bold.as_ref() && Some(b) != italic.as_ref());
    [regular, bold, italic, bold_italic]
}
/// `mono`: an empty name means the system monospace font, and the platform
/// fallbacks are monospace families.
fn resolve_family(name: &str, mono: bool) -> Option<Vec<u8>> {
    // A leading '-' would be parsed by fc-match as an option, not a font pattern.
    if name.starts_with('-') || (!mono && name.is_empty()) {
        return None;
    }
    let parse = |bytes: Vec<u8>| {
        ab_glyph::FontArc::try_from_vec(bytes.clone())
            .ok()
            .map(|_| bytes)
    };
    if let Ok(output) = std::process::Command::new("fc-match")
        .args([
            "-f",
            "%{file}",
            "--",
            if name.is_empty() { "monospace" } else { name },
        ])
        .output()
        && output.status.success()
        && let Some(bytes) = fs::read(String::from_utf8_lossy(&output.stdout).trim())
            .ok()
            .and_then(parse)
    {
        return Some(bytes);
    }
    // macOS/Windows (or Linux without fontconfig): scan the standard font directories.
    let wanted: Vec<&str> = if name.is_empty() && mono {
        vec!["Menlo", "Consolas", "Courier New", "DejaVuSansMono"]
    } else {
        vec![name]
    };
    let dirs = font_dirs();
    wanted.into_iter().find_map(|n| {
        find_font_file(&dirs, n)
            .and_then(|p| fs::read(p).ok())
            .and_then(parse)
    })
}

/// fontconfig's choice of font for a few scripts (Han/kana/Hangul, Arabic, Hebrew,
/// Devanagari, Thai), each file once. Empty without fontconfig.
fn resolve_fallbacks() -> Vec<Vec<u8>> {
    let mut seen = std::collections::HashSet::new();
    let mut out = vec![];
    for lang in ["ja", "ko", "zh-cn", "ar", "he", "hi", "th"] {
        let Ok(output) = std::process::Command::new("fc-match")
            .args(["-f", "%{file}", "--", &format!("sans-serif:lang={lang}")])
            .output()
        else {
            return out;
        };
        let file = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !output.status.success() || file.is_empty() || !seen.insert(file.clone()) {
            continue;
        }
        if let Some(bytes) = fs::read(&file)
            .ok()
            .filter(|b| ab_glyph::FontRef::try_from_slice(b).is_ok())
        {
            out.push(bytes);
        }
    }
    out
}

fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/System/Library/Fonts"),
        PathBuf::from("/Library/Fonts"),
        caelum_rs::platform::home().join("Library/Fonts"),
    ];
    if let Some(w) = std::env::var_os("WINDIR").or_else(|| std::env::var_os("SystemRoot")) {
        dirs.push(PathBuf::from(w).join("Fonts"));
    }
    if let Some(l) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(l).join("Microsoft/Windows/Fonts"));
    }
    dirs
}

/// Finds a font file whose name matches `family` (case, spaces and dashes ignored):
/// an exact stem match wins, otherwise the shortest stem starting with the family.
fn find_font_file(dirs: &[PathBuf], family: &str) -> Option<PathBuf> {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let want = norm(family);
    if want.is_empty() {
        return None;
    }
    let mut best: Option<(usize, PathBuf)> = None;
    let mut stack: Vec<(PathBuf, u8)> = dirs.iter().cloned().map(|d| (d, 0)).collect();
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                if depth < 2 {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            let ext = path
                .extension()
                .and_then(|x| x.to_str())
                .map(str::to_lowercase);
            if !matches!(ext.as_deref(), Some("ttf" | "otf" | "ttc")) {
                continue;
            }
            let stem = norm(path.file_stem().and_then(|x| x.to_str()).unwrap_or(""));
            if stem == want {
                return Some(path);
            }
            if stem.starts_with(&want) && best.as_ref().is_none_or(|(l, _)| stem.len() < *l) {
                best = Some((stem.len(), path));
            }
        }
    }
    best.map(|(_, p)| p)
}

#[cfg(test)]
mod font_tests {
    use super::*;
    #[test]
    fn finds_font_by_family_name() {
        let d = tempfile::tempdir().unwrap();
        let sub = d.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("Menlo-Bold.ttf"), b"x").unwrap();
        fs::write(sub.join("Menlo.ttc"), b"x").unwrap();
        fs::write(d.path().join("notes.txt"), b"x").unwrap();
        let dirs = [d.path().to_path_buf()];
        assert_eq!(find_font_file(&dirs, "menlo"), Some(sub.join("Menlo.ttc")));
        assert_eq!(find_font_file(&dirs, "Nope"), None);
        assert_eq!(find_font_file(&dirs, ""), None);
    }
}
