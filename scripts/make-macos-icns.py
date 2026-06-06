#!/usr/bin/env python3
"""Generate the macOS app icon (icons/icon.icns) from the full-bleed source.

macOS app icons follow Apple's icon grid: the artwork sits in a centered
~80% content area with transparent margins, a large continuous-corner
("squircle") radius, and a soft drop shadow, so it renders at the same
visual size as other native icons in the Dock and app switcher. Our base
artwork (icons/icon.png) is a full-bleed rounded square, which is the
right shape for Windows (.ico) and Linux (the PNGs) but renders oversized
on macOS.

This script applies the macOS framing and writes ONLY icons/icon.icns, so
the Windows/Linux assets keep their full-bleed look. Do NOT regenerate the
.icns with a plain `tauri icon` run: that would re-derive it from the
full-bleed source and bring back the oversized-on-macOS look. Run this
instead:

    python3 scripts/make-macos-icns.py

Requires Pillow (`pip install Pillow`).
"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

ICONS = Path(__file__).resolve().parent.parent / "crates" / "zkv" / "icons"
SRC = ICONS / "icon.png"
DST = ICONS / "icon.icns"

CANVAS = 1024
MARGIN = 100                   # Apple macOS icon-grid margin
CONTENT = CANVAS - 2 * MARGIN  # 824
RADIUS = 185                   # ~22.4% of the content side, the macOS squircle
SHADOW_DROP = 12               # downward shadow offset (px)
SHADOW_BLUR = 18
SHADOW_ALPHA = 90
SS = 4                         # supersample factor for a crisp corner mask


def rounded_mask(size: int, radius: int, ss: int = SS) -> Image.Image:
    big = Image.new("L", (size * ss, size * ss), 0)
    ImageDraw.Draw(big).rounded_rectangle(
        [0, 0, size * ss - 1, size * ss - 1], radius=radius * ss, fill=255
    )
    return big.resize((size, size), Image.LANCZOS)


def main() -> None:
    # Scale the full-bleed artwork down into the content area and re-cut its
    # corners to the larger macOS radius (a subset of the existing fill, so
    # the logo is untouched: only the background corners are trimmed).
    orig = Image.open(SRC).convert("RGBA").resize((CONTENT, CONTENT), Image.LANCZOS)
    mask = rounded_mask(CONTENT, RADIUS)
    body = Image.new("RGBA", (CONTENT, CONTENT), (0, 0, 0, 0))
    body.paste(orig, (0, 0), mask)

    master = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))

    # Soft drop shadow so the icon floats, matching Apple's template.
    shmask = Image.new("L", (CANVAS, CANVAS), 0)
    shmask.paste(mask, (MARGIN, MARGIN + SHADOW_DROP))
    sh = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
    sh.paste(Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, SHADOW_ALPHA)), (0, 0), shmask)
    master = Image.alpha_composite(master, sh.filter(ImageFilter.GaussianBlur(SHADOW_BLUR)))

    master.paste(body, (MARGIN, MARGIN), body)

    # Pillow derives the full icns resolution set (16..1024) from the master.
    master.save(DST, format="ICNS")
    print(f"wrote {DST}")


if __name__ == "__main__":
    main()
