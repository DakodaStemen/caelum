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

The Caelum app icon is a hand-written SVG (three nested crescent arcs and a cursor pill, no lettering). Master SVGs
are in `assets/brand` (`icon.svg`, bolder `icon-small.svg` for 32 px and below, `mark.svg`, `lockup.svg`); every PNG/ICO/ICNS
in `assets/app-icons` is generated from them by `scripts/gen-icons.py`. No fonts are bundled; default fonts come from
eframe and system fontconfig. Rust does not remove licensing obligations
for bundled assets, dependencies or third-party code.
