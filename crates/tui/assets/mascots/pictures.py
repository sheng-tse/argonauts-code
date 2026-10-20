"""The pictures the welcome header sends where the terminal can draw one: each mascot in each state
as a PNG that fills its 9 x 3 cells, and each ultra look as the frames its animation swaps
through. They are drawn for the cell sizes listed here; the header sends the one drawn for the
terminal's own cell size, and scales the largest to fit any other.

A mascot keeps one drawn area in every state, its ultra mark included, so it doesn't move when
the look changes. The area is fitted to the 9 x 3 cells by its height, or by its width when it
would be too wide, and centred.

pictures.json lists what was written, with each mascot's area and cycle, for the header to read.

Needs rsvg-convert and Pillow. Usage: python3 pictures.py   (argonauta.py runs it too)
"""

import io
import json
import re
import subprocess
from pathlib import Path

from PIL import Image

import argonauta

HERE = Path(__file__).resolve().parent
OUT = HERE / "pictures"
# Cell sizes in pixels: Iosevka at 17 px, and the same on a Retina screen.
CELLS = [(9, 21), (18, 42)]
COLUMNS, ROWS = 9, 3
FRAMES = 18
STILL = ("normal", "light")


def viewbox(svg_text):
    return tuple(map(float, re.search(r'viewBox="([^"]+)"', svg_text).group(1).split()))


def render(svg_text, width, height):
    return subprocess.run(
        ["rsvg-convert", "-w", str(width), "-h", str(height)],
        input=svg_text.encode(), check=True, capture_output=True,
    ).stdout


def bounds(svg_text):
    """The drawn area in viewBox units, from a large render."""
    x, y, w, h = viewbox(svg_text)
    big = Image.open(io.BytesIO(render(svg_text, 1000, round(1000 * h / w)))).convert("RGBA")
    left, top, right, bottom = big.getchannel("A").point(lambda a: 255 if a > 40 else 0).getbbox()
    sx, sy = w / big.width, h / big.height
    return (x + left * sx, y + top * sy, (right - left) * sx, (bottom - top) * sy)


def area(name):
    """The drawn area with the ultra mark included, so a mascot keeps its size in every state."""
    bx, by, bw, bh = bounds((argonauta.OUT / f"{name}-ultracode.svg").read_text())
    pad = 0.04 * max(bw, bh)
    return (round(bx - pad, 2), round(by - pad, 2), round(bw + 2 * pad, 2), round(bh + 2 * pad, 2))


def png(svg_text, box, size):
    """The mascot's area fitted into a transparent picture of `size` pixels."""
    bx, by, bw, bh = box
    box_w, box_h = size
    height = box_h
    width = height * bw / bh
    if width > box_w:
        width, height = box_w, box_w * bh / bw
    cropped = re.sub(r'viewBox="[^"]+"', f'viewBox="{bx} {by} {bw} {bh}"', svg_text, count=1)
    art = Image.open(io.BytesIO(render(cropped, round(width), round(height))))
    canvas = Image.new("RGBA", (box_w, box_h), (0, 0, 0, 0))
    canvas.alpha_composite(art, ((box_w - art.width) // 2, (box_h - art.height) // 2))
    out = io.BytesIO()
    canvas.save(out, "PNG", optimize=True)
    return out.getvalue()


def main():
    OUT.mkdir(exist_ok=True)
    for old in OUT.glob("*.png"):
        old.unlink()
    listing = {"cells": CELLS, "frames": FRAMES, "mascots": {}}
    for make in argonauta.MASCOTS:
        probe = make(argonauta.Motion("off"), "x", False)
        name = probe["name"]
        box = area(name)
        listing["mascots"][name] = {"area": box, "cycle": probe["cycle"]}
        for cw, ch in CELLS:
            size = (COLUMNS * cw, ROWS * ch)
            for state in argonauta.STATES:
                if state in STILL:
                    stem, svg = argonauta.build(make, state)
                    (OUT / f"{stem}-{cw}x{ch}.png").write_bytes(png(svg, box, size))
                    continue
                for k in range(FRAMES):
                    stem, svg = argonauta.build(make, state, "frame", k / FRAMES)
                    (OUT / f"{stem}-{cw}x{ch}-{k:02d}.png").write_bytes(png(svg, box, size))
    (HERE / "pictures.json").write_text(json.dumps(listing, indent=1) + "\n")
    print("wrote", len(list(OUT.glob("*.png"))), "pictures")


if __name__ == "__main__":
    main()
