#!/usr/bin/env python3
"""Generate every platform icon from the two master SVGs.

Usage: gen_icons.py <icon.svg> <icon-small.svg> <app-id> <out-dir>

Masters are 1024x1024 full-bleed tiles. icon-small.svg is a simplified,
bolder master used for 16-32 px so small sizes stay crisp. Every PNG is
rendered directly at its final pixel size with rsvg-convert (no
downscaling), then packed into .ico / .icns with Pillow.

Output layout (out-dir):
  linux/hicolor/<N>x<N>/apps/<app-id>.png   N in 16 22 24 32 48 64 128 256 512
  linux/hicolor/scalable/apps/<app-id>.svg
  windows/<app-id>.ico                      16 20 24 32 40 48 64 96 128 256
  macos/<app-id>.icns                       16..1024 incl. @2x
  macos/png/icon_<N>.png                    the icns members, for inspection
  window-icon-256.png                       runtime window icon (Linux padding)
  window-icon-64.png
"""
import os
import re
import subprocess
import sys

from PIL import Image

SMALL_MAX = 32  # sizes <= this use the small master
# Tile scale inside the canvas, per platform.
LINUX_SMALL = 0.9375  # 1 px margin at 16 px; small icons need every pixel
LINUX = 0.875  # 6.25% margin, matches the visual size of other square app icons
MACOS = 824 / 1024  # Apple's macOS 11+ grid: 824 px tile on a 1024 canvas
WINDOWS = 0.9375

LINUX_SIZES = [16, 22, 24, 32, 48, 64, 128, 256, 512]
ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]
ICNS_SIZES = [16, 32, 64, 128, 256, 512, 1024]


def wrap(svg: str, scale: float, shadow: bool = False) -> str:
    """Scale the master's content about the canvas centre, optionally with a drop shadow."""
    m = re.match(r"(?s)\s*(<\?xml[^>]*>\s*)?<svg[^>]*>(.*)</svg>\s*$", svg)
    if not m:
        raise SystemExit("master is not a single <svg> element")
    body = m.group(2)
    off = 512 * (1 - scale)
    defs = ""
    attr = ""
    if shadow:
        defs = (
            '<defs><filter id="gi_shadow" x="-20%" y="-20%" width="140%" height="140%">'
            '<feDropShadow dx="0" dy="10" stdDeviation="12" flood-color="#000" flood-opacity="0.30"/>'
            "</filter></defs>"
        )
        attr = ' filter="url(#gi_shadow)"'
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" width="1024" height="1024">'
        f'{defs}<g{attr}><g transform="translate({off:g} {off:g}) scale({scale:g})">{body}</g></g></svg>'
    )


def render(svg: str, size: int, path: str) -> Image.Image:
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = path + ".src.svg"
    with open(tmp, "w") as f:
        f.write(svg)
    subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size), "-o", path, tmp], check=True)
    os.remove(tmp)
    img = Image.open(path).convert("RGBA")
    if img.size != (size, size):
        raise SystemExit(f"{path}: rendered {img.size}, wanted {size}")
    return img


def main() -> None:
    if len(sys.argv) != 5:
        raise SystemExit(__doc__)
    big_p, small_p, app_id, out = sys.argv[1:]
    big = open(big_p).read()
    small = open(small_p).read()

    def master(size: int) -> str:
        return small if size <= SMALL_MAX else big

    # Linux hicolor
    for n in LINUX_SIZES:
        scale = LINUX_SMALL if n <= SMALL_MAX else LINUX
        render(wrap(master(n), scale), n, f"{out}/linux/hicolor/{n}x{n}/apps/{app_id}.png")
    os.makedirs(f"{out}/linux/hicolor/scalable/apps", exist_ok=True)
    with open(f"{out}/linux/hicolor/scalable/apps/{app_id}.svg", "w") as f:
        f.write(wrap(big, LINUX))

    # Runtime window icons
    render(wrap(big, LINUX), 256, f"{out}/window-icon-256.png")
    render(wrap(big, LINUX), 64, f"{out}/window-icon-64.png")

    # Windows .ico, every member rendered at its own size
    ico = [render(wrap(master(n), WINDOWS), n, f"{out}/windows/png/icon_{n}.png") for n in ICO_SIZES]
    largest = ico[-1]
    largest.save(
        f"{out}/windows/{app_id}.ico",
        format="ICO",
        sizes=[(n, n) for n in ICO_SIZES],
        append_images=ico[:-1],
    )

    # macOS .icns (Apple grid + shadow; small master below 33 px)
    icns = {n: render(wrap(master(n), MACOS, shadow=n > SMALL_MAX), n, f"{out}/macos/png/icon_{n}.png") for n in ICNS_SIZES}
    icns[1024].save(f"{out}/macos/{app_id}.icns", format="ICNS", append_images=[icns[n] for n in ICNS_SIZES[:-1]])

    # Self-check: read the containers back.
    got_ico = sorted(Image.open(f"{out}/windows/{app_id}.ico").info.get("sizes", []))
    got_icns = sorted(Image.open(f"{out}/macos/{app_id}.icns").info.get("sizes", []))
    print(f"{app_id}: linux {len(LINUX_SIZES)}+svg, ico {got_ico}, icns {got_icns}")


if __name__ == "__main__":
    main()
