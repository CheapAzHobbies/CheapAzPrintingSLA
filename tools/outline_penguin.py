#!/usr/bin/env python3
"""Add a white sticker outline to the save-indicator sprite sheet.

The sheet is a grid of frames, so the dilation is done per cell: a frame that
happens to reach a cell edge must not bleed its outline into its neighbour.

    python3 tools/outline_penguin.py assets/penguin_saving.png

Writes in place after making a .orig copy the first time it is run, so the
outline is never applied twice on top of itself.
"""

import shutil
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageFilter

COLS, ROWS = 12, 12
FRAME_W, FRAME_H = 248, 240
# The indicator is drawn at about 88 pixels from a 248 pixel cell, so a border
# that reads as roughly 2 pixels on screen needs about 6 in sheet space.
RADIUS = 7
# Ignore the near-transparent fringe of the silhouette, or the outline traces
# the antialiasing instead of the shape.
SOLID = 170


def outline_cell(cell: Image.Image) -> Image.Image:
    alpha = cell.getchannel("A")
    solid = alpha.point(lambda a: 255 if a >= SOLID else 0)
    if not solid.getbbox():
        return cell
    # A square max filter gives square corners. Blurring the dilated mask and
    # thresholding it back rounds them off, which is what a die-cut sticker
    # edge actually looks like.
    grown = solid.filter(ImageFilter.MaxFilter(RADIUS * 2 + 1))
    grown = grown.filter(ImageFilter.GaussianBlur(RADIUS / 2))
    grown = grown.point(lambda a: 255 if a >= 110 else 0)
    # One light blur back for an antialiased edge rather than a stepped one.
    grown = grown.filter(ImageFilter.GaussianBlur(0.8))

    border = Image.new("RGBA", cell.size, (255, 255, 255, 255))
    border.putalpha(grown)
    return Image.alpha_composite(border, cell)


def main() -> int:
    path = Path(sys.argv[1] if len(sys.argv) > 1 else "assets/penguin_saving.png")
    backup = path.with_suffix(path.suffix + ".orig")
    if not backup.exists():
        shutil.copy2(path, backup)
    source = Image.open(backup).convert("RGBA")

    want = (FRAME_W * COLS, FRAME_H * ROWS)
    if source.size != want:
        print(f"expected a {want[0]}x{want[1]} sheet, got {source.size[0]}x{source.size[1]}")
        return 1

    out = Image.new("RGBA", source.size, (0, 0, 0, 0))
    for row in range(ROWS):
        for col in range(COLS):
            box = (col * FRAME_W, row * FRAME_H, (col + 1) * FRAME_W, (row + 1) * FRAME_H)
            out.paste(outline_cell(source.crop(box)), box)

    out.save(path)
    added = int((np.array(out)[:, :, 3] > 0).sum() - (np.array(source)[:, :, 3] > 0).sum())
    print(f"wrote {path} ({path.stat().st_size // 1024} KB, {added} pixels of border)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
