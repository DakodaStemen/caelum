#!/usr/bin/env bash
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source_binary="$project_dir/target/release/caelum-rs"
if [[ ! -x "$source_binary" ]]; then
  echo 'Build first: cargo build --release --locked' >&2
  exit 1
fi
install -Dm755 "$source_binary" "$HOME/.local/bin/caelum-rs"
data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
icon_theme="$data_home/icons/hicolor"
icon_src="$project_dir/assets/app-icons/linux/hicolor"
installed_icons=0
for icon in "$icon_src"/*/apps/caelum-rs.*; do
  rel="${icon#"$icon_src"/}"
  install -Dm644 "$icon" "$icon_theme/$rel"
  installed_icons=$((installed_icons + 1))
done
if [[ $installed_icons -eq 0 ]]; then echo "No icons found in $icon_src" >&2; exit 1; fi
mkdir -p "$data_home/applications"
# Desktop Entry values use their own quoting rules, not shell interpolation.
binary_path="$HOME/.local/bin/caelum-rs"
escaped_binary="${binary_path//\\/\\\\}"
escaped_binary="${escaped_binary//\"/\\\"}"
escaped_binary="${escaped_binary//\`/\\\`}"
escaped_binary="${escaped_binary//\$/\\\$}"
escaped_binary="${escaped_binary//%/%%}"
cat > "$data_home/applications/io.github.DakodaStemen.CaelumRust.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Caelum Rust
Comment=Native Rust workspace terminal
Exec="$escaped_binary"
Icon=caelum-rs
Terminal=false
Categories=System;TerminalEmulator;
StartupWMClass=io.github.DakodaStemen.CaelumRust
DESKTOP
# Cache refreshes are best effort: a missing or failing tool must not fail the install.
if command -v gtk-update-icon-cache >/dev/null; then gtk-update-icon-cache -f -t "$icon_theme" || true; fi
if command -v update-desktop-database >/dev/null; then update-desktop-database "$data_home/applications" || true; fi
for kbuild in kbuildsycoca6 kbuildsycoca5; do
  if command -v "$kbuild" >/dev/null; then "$kbuild" >/dev/null 2>&1 || true; break; fi
done
printf 'Installed %s and %s icon files\n' "$binary_path" "$installed_icons"
