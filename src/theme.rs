use anyhow::{Result, bail};
use eframe::egui::{Color32, Context, CornerRadius, Stroke, Vec2, Visuals, epaint::Shadow};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Theme {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub kind: String,
    pub ui: BTreeMap<String, String>,
    pub terminal: BTreeMap<String, String>,
    #[serde(default)]
    pub syntax: BTreeMap<String, String>,
}
static BUILTIN: std::sync::LazyLock<BTreeMap<String, Theme>> = std::sync::LazyLock::new(|| {
    let mut themes: BTreeMap<String, Theme> =
        serde_json::from_str(include_str!("../assets/themes.json")).expect("bundled themes");
    for (id, t) in &mut themes {
        t.id = id.clone();
    }
    themes
});
pub fn presets() -> BTreeMap<String, Theme> {
    BUILTIN.clone()
}
pub fn color(hex: &str) -> Option<Color32> {
    let h = hex.strip_prefix('#')?;
    let h = if h.len() == 3 {
        h.chars().flat_map(|c| [c, c]).collect()
    } else {
        h.to_string()
    };
    if ![6, 8].contains(&h.len()) || !h.is_ascii() {
        return None;
    }
    let n = u32::from_str_radix(&h, 16).ok()?;
    Some(if h.len() == 6 {
        Color32::from_rgb((n >> 16) as u8, (n >> 8) as u8, n as u8)
    } else {
        Color32::from_rgba_unmultiplied((n >> 24) as u8, (n >> 16) as u8, (n >> 8) as u8, n as u8)
    })
}
impl Theme {
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() || !["light", "dark"].contains(&self.kind.as_str()) {
            bail!("Theme needs a name and a light/dark kind");
        }
        let reference = &BUILTIN["starlight"];
        for (values, keys) in [
            (&self.ui, &reference.ui),
            (&self.terminal, &reference.terminal),
        ] {
            for key in keys.keys() {
                if values.get(key).and_then(|v| color(v)).is_none() {
                    bail!("Invalid or missing theme color: {key}");
                }
            }
        }
        Ok(())
    }
    pub fn ui(&self, key: &str) -> Color32 {
        self.ui
            .get(key)
            .and_then(|s| color(s))
            .unwrap_or(Color32::WHITE)
    }
    pub fn term(&self, key: &str) -> Color32 {
        self.terminal
            .get(key)
            .and_then(|s| color(s))
            .unwrap_or(Color32::WHITE)
    }
    /// Code color for a token kind; themes without a `syntax` table borrow terminal colors.
    pub fn syn(&self, key: &str) -> Color32 {
        if let Some(c) = self.syntax.get(key).and_then(|s| color(s)) {
            return c;
        }
        match key {
            "comment" => self.ui("muted"),
            "keyword" => self.term("brightMagenta"),
            "string" => self.term("green"),
            "number" | "constant" | "parameter" => self.term("yellow"),
            "function" => self.term("blue"),
            "type" | "operator" | "escape" => self.term("cyan"),
            "tag" => self.term("red"),
            "attribute" => self.term("brightYellow"),
            "punctuation" => self.ui("textSecondary"),
            _ => self.term("foreground"),
        }
    }
    /// The same theme with `accent` as its accent color (used to follow the desktop).
    pub fn with_accent(mut self, accent: Color32) -> Self {
        let [r, g, b, _] = accent.to_array();
        self.ui
            .insert("accent".into(), format!("#{r:02x}{g:02x}{b:02x}"));
        self
    }
    /// Applies the theme as libadwaita-like visuals: flat surfaces, no strokes on
    /// controls, 6pt control radius, 12pt popovers and dialogs, accent only for
    /// selection, focus and links. `font_size` is the body text size in points.
    pub fn apply(&self, ctx: &Context, font_size: f32) {
        let light = self.kind == "light";
        let mut v = if light {
            Visuals::light()
        } else {
            Visuals::dark()
        };
        let text = self.ui("text");
        let bg = self.ui("background");
        let accent = self.ui("accent");
        // Controls are the text color laid over the window at low opacity.
        let control = self.ui("button");
        let hover = mix(control, text, 0.06);
        let pressed = mix(control, text, 0.16);
        v.override_text_color = Some(text);
        v.weak_text_color = Some(self.ui("muted"));
        v.panel_fill = bg;
        v.window_fill = self.ui("surface");
        v.extreme_bg_color = control;
        v.text_edit_bg_color = Some(control);
        v.code_bg_color = control;
        v.faint_bg_color = mix(bg, text, 0.03);
        v.hyperlink_color = accent;
        v.warn_fg_color = self.syn("number");
        v.error_fg_color = self.ui("danger");
        v.selection.bg_fill = accent.gamma_multiply(if light { 0.25 } else { 0.4 });
        // Also the focus ring of text fields and the text of selected rows, so it is
        // an accent tint that still reads as text.
        v.selection.stroke = Stroke::new(1.5_f32, mix(accent, self.ui("textStrong"), 0.45));
        v.handle_shape = eframe::egui::style::HandleShape::Circle;
        v.text_cursor.stroke = Stroke::new(1.5_f32, text);
        v.striped = false;
        v.interact_cursor = None;
        v.button_frame = true;
        v.collapsing_header_frame = false;
        v.indent_has_left_vline = false;
        let radius = CornerRadius::same(6);
        let quiet = Stroke::NONE;
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = bg;
        w.noninteractive.weak_bg_fill = bg;
        w.noninteractive.bg_stroke = Stroke::new(1_f32, self.ui("border"));
        w.noninteractive.fg_stroke = Stroke::new(1_f32, text);
        // Check boxes, radios and slider rails need to show up on dialogs too.
        w.inactive.bg_fill = mix(self.ui("surface"), text, 0.16);
        w.inactive.weak_bg_fill = control;
        w.inactive.bg_stroke = quiet;
        w.inactive.fg_stroke = Stroke::new(1_f32, text);
        w.hovered.bg_fill = hover;
        w.hovered.weak_bg_fill = hover;
        w.hovered.bg_stroke = quiet;
        w.hovered.fg_stroke = Stroke::new(1.5_f32, self.ui("textStrong"));
        w.active.bg_fill = pressed;
        w.active.weak_bg_fill = pressed;
        w.active.bg_stroke = quiet;
        w.active.fg_stroke = Stroke::new(1.5_f32, self.ui("textStrong"));
        w.open.bg_fill = hover;
        w.open.weak_bg_fill = hover;
        w.open.bg_stroke = quiet;
        w.open.fg_stroke = Stroke::new(1_f32, self.ui("textStrong"));
        for s in [
            &mut w.noninteractive,
            &mut w.inactive,
            &mut w.hovered,
            &mut w.active,
            &mut w.open,
        ] {
            s.corner_radius = radius;
            s.expansion = 0.;
        }
        v.window_corner_radius = CornerRadius::same(12);
        v.menu_corner_radius = CornerRadius::same(12);
        v.window_stroke = Stroke::new(
            1_f32,
            if light {
                Color32::from_black_alpha(30)
            } else {
                Color32::from_white_alpha(14)
            },
        );
        v.window_shadow = Shadow {
            offset: [0, 4],
            blur: 18,
            spread: 0,
            color: Color32::from_black_alpha(if light { 40 } else { 90 }),
        };
        v.popup_shadow = Shadow {
            offset: [0, 2],
            blur: 10,
            spread: 0,
            color: Color32::from_black_alpha(if light { 36 } else { 80 }),
        };
        v.window_highlight_topmost = false;
        ctx.set_visuals(v);
        ctx.style_mut(|st| {
            use eframe::egui::{FontFamily, FontId, TextStyle};
            st.text_styles = [
                (
                    TextStyle::Small,
                    FontId::proportional((font_size * 0.86).round()),
                ),
                (TextStyle::Body, FontId::proportional(font_size)),
                (TextStyle::Button, FontId::proportional(font_size)),
                (
                    TextStyle::Heading,
                    FontId::proportional((font_size * 1.2).round()),
                ),
                (
                    TextStyle::Monospace,
                    FontId::new(font_size, FontFamily::Monospace),
                ),
            ]
            .into();
            st.spacing.item_spacing = Vec2::new(6., 6.);
            st.spacing.button_padding = Vec2::new(10., 4.);
            st.spacing.interact_size.y = 28.;
            st.spacing.menu_margin = eframe::egui::Margin::same(6);
            st.spacing.window_margin = eframe::egui::Margin::same(14);
            st.spacing.scroll.bar_width = 6.;
            st.spacing.scroll.floating = true;
            st.spacing.slider_width = 180.;
            st.spacing.slider_rail_height = 4.;
            st.spacing.combo_width = 160.;
        });
    }
}
/// Linear blend of `a` toward `b` (opaque colors), `t` in 0..=1.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}
/// WCAG relative luminance of an opaque color (0 black ..= 1 white).
pub fn luminance(c: Color32) -> f32 {
    let lin = |v: u8| {
        let v = v as f32 / 255.;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}
/// WCAG contrast ratio between two colors, 1 (identical) ..= 21 (black on white).
pub fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}
/// `fg` moved toward white or black, as little as needed, until its contrast against
/// `bg` is at least `min`. A color that already meets `min` is returned unchanged; if
/// even the better extreme cannot reach `min`, that extreme is returned.
pub fn ensure_contrast(fg: Color32, bg: Color32, min: f32) -> Color32 {
    if min <= 1. || contrast_ratio(fg, bg) >= min {
        return fg;
    }
    let (white, black) = (Color32::WHITE, Color32::BLACK);
    // Lighten when the background is dark, darken when it is light (the direction
    // with more room), unless that direction cannot reach `min` but the other can.
    let first = if luminance(bg) < 0.18 { white } else { black };
    let second = if first == white { black } else { white };
    let target = if contrast_ratio(first, bg) >= min
        || contrast_ratio(first, bg) >= contrast_ratio(second, bg)
    {
        first
    } else {
        second
    };
    if contrast_ratio(target, bg) < min {
        return target;
    }
    let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
    for _ in 0..12 {
        let mid = (lo + hi) / 2.;
        if contrast_ratio(mix(fg, target, mid), bg) >= min {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    mix(fg, target, hi)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contrast_math_matches_wcag() {
        let r = contrast_ratio(Color32::BLACK, Color32::WHITE);
        assert!((r - 21.).abs() < 0.01, "{r}");
        assert!((contrast_ratio(Color32::RED, Color32::RED) - 1.).abs() < 1e-6);
        // Symmetric.
        let (a, b) = (
            Color32::from_rgb(10, 80, 200),
            Color32::from_rgb(240, 240, 20),
        );
        assert_eq!(contrast_ratio(a, b), contrast_ratio(b, a));
    }
    #[test]
    fn ensure_contrast_nudges_only_what_is_too_faint() {
        let bg = Color32::from_rgb(30, 30, 30);
        // Fine already: untouched, whatever the minimum.
        let good = Color32::from_rgb(220, 220, 220);
        assert_eq!(ensure_contrast(good, bg, 4.5), good);
        // Black on near-black is lifted to the minimum, no further than needed.
        let dark = Color32::from_rgb(20, 20, 20);
        let fixed = ensure_contrast(dark, bg, 1.8);
        let r = contrast_ratio(fixed, bg);
        assert!((1.8..2.0).contains(&r), "{r}");
        assert!(luminance(fixed) > luminance(bg));
        // The same on a light background goes darker instead.
        let light_bg = Color32::from_rgb(250, 250, 250);
        let faint = Color32::from_rgb(240, 240, 240);
        let fixed = ensure_contrast(faint, light_bg, 1.8);
        assert!(luminance(fixed) < luminance(light_bg));
        assert!(contrast_ratio(fixed, light_bg) >= 1.8);
        // A disabled minimum (1.0 or less) never changes anything.
        assert_eq!(ensure_contrast(dark, bg, 1.0), dark);
        assert_eq!(ensure_contrast(dark, bg, 0.0), dark);
        // Mid grey on mid grey cannot reach 21; the best extreme is returned.
        let mid = Color32::from_gray(128);
        let best = ensure_contrast(mid, mid, 21.);
        assert!(best == Color32::BLACK || best == Color32::WHITE);
    }
    #[test]
    fn bundled_themes_keep_their_text_color_at_the_default_minimum() {
        for t in presets().values() {
            let (fg, bg) = (t.term("foreground"), t.term("background"));
            assert_eq!(
                ensure_contrast(fg, bg, crate::model::DEFAULT_MIN_CONTRAST),
                fg,
                "{}",
                t.name
            );
        }
    }
    #[test]
    fn bundled_themes_complete() {
        let p = presets();
        assert_eq!(p.len(), 13);
        for t in p.values() {
            t.validate().unwrap();
            for key in crate::syntax::KINDS {
                assert!(t.syntax.contains_key(*key), "{} lacks {key}", t.name);
            }
        }
    }
    #[test]
    fn invalid_colors_rejected() {
        for s in ["red", "#xxyyzz", "#12345", "#😀abc"] {
            assert!(color(s).is_none());
        }
    }
}
