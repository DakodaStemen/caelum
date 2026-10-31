# Third-party notices

Caelum Rust is MIT licensed. Its dependencies retain their own licenses.
`Cargo.lock` records the exact dependency graph.

Core components include:

- Alacritty terminal core: Apache-2.0 — https://github.com/alacritty/alacritty
- egui/eframe: MIT OR Apache-2.0 — https://github.com/emilk/egui
- portable-pty: MIT — https://github.com/wezterm/wezterm
- serde/serde_json: MIT OR Apache-2.0 — https://github.com/serde-rs
- syntect (syntax parsing and bundled Sublime syntaxes): MIT — https://github.com/trishume/syntect
- image: MIT OR Apache-2.0 — https://github.com/image-rs/image
- egui_extras (SVG loading via resvg/usvg): MIT OR Apache-2.0 — https://github.com/emilk/egui
- Adwaita symbolic icons (`assets/icons`, fallbacks when the desktop has no icon theme):
  CC-BY-SA-3.0 or LGPL-3.0, by the GNOME Project —
  https://gitlab.gnome.org/GNOME/adwaita-icon-theme

The Caelum mark is a hand-written SVG redraw (three nested crescent arcs) of the supplied icon, without lettering. Source SVGs
are in `assets/brand`; `mark-app.svg` adds weight for small-size readability, and
`icon.png` is its application rendering. No fonts are bundled; default fonts come from
eframe and system fontconfig. Rust does not remove licensing obligations
for bundled assets, dependencies or third-party code.
