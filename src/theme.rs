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
#[cfg(test)]
mod tests {
    use super::*;
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
