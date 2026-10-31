"""The pictures the welcome header sends where the terminal can draw one: each mascot in each state
as a PNG that fills its 9 x 3 cells, and each ultra look as the frames its animation swaps
through, one every 50 ms of its cycle. They are drawn for one cell size; the header sends them
as drawn for a terminal of that size, and scales them to fit any other.

A mascot keeps one size in every state, so it doesn't move when the look changes. Its own
height fills the 3 rows; across, its area keeps the room its ultra marks take at the side. An
ultra look's picture is one row taller and starts in the blank row above the header, so the
mark's top rises into it. Where the area would be too wide, its width fits the 9 cells instead.

pictures.json lists what was written, with each mascot's area, cycle and frames, for the header
to read.

Needs rsvg-convert and Pillow. Usage: python3 pictures.py [DIR...]   (argonauta.py runs it too)
Each DIR is one that `argonauta.py frames DIR N` wrote; a mascot's frames come from the one
whose N is its own count, and from argonauta.py's own drawing where none is given, which is the
same.
"""

import io
import json
import re
import subprocess
import sys
from pathlib import Path

from PIL import Image

import argonauta

HERE = Path(__file__).resolve().parent
OUT = HERE / "pictures"
# The cell size in pixels: Iosevka at 17 px on a Retina screen. Any other size
# is scaled from it.
CELLS = [(18, 42)]
COLUMNS, ROWS = 9, 3
# The rows an ultra look's picture takes above the mascot's own.
ABOVE = 1
# Seconds between an ultra look's frames, quicker than Claude Code's own 64 ms shimmer step.
STEP = 0.05
STILL = ("normal", "light")


def frames_of(cycle):
    """How many frames one cycle takes, one every STEP seconds."""
    return round(cycle / STEP)


def frame_svg(make, state, k, n, given):
    """Frame k of n of an ultra look: from a directory `argonauta.py frames` wrote with n frames,
    or drawn here, which comes to the same."""
    stem, svg = argonauta.build(make, state, "frame", k / n)
    for folder in given:
        drawn = folder / f"{stem}-{k:02d}.svg"
        if len(list(folder.glob(f"{stem}-*.svg"))) == n and drawn.exists():
            return stem, drawn.read_text()
    return stem, svg


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


def area(name, ultra_svgs):
    """The mascot's own height, down to the lowest any frame reaches, and across, the room its
    ultra looks take at the side: the still and every ultra frame, so nothing moving is cut."""
    nx, ny, nw, nh = bounds((argonauta.OUT / f"{name}.svg").read_text())
    top, left, right, bottom = ny, nx, nx + nw, ny + nh
    for svg in ultra_svgs:
        x, y, w, h = bounds(svg)
        top, left, right, bottom = min(top, y), min(left, x), max(right, x + w), max(bottom, y + h)
    rise = (ny - top) / (bottom - ny) * ROWS
    if rise > ABOVE:
        sys.exit(f"{name}: an ultra mark rises {rise:.2f} rows, more than the {ABOVE} above")
    return (round(left, 2), round(ny, 2), round(right - left, 2), round(bottom - ny, 2))


def png(svg_text, box, size, above=0):
    """The mascot's area fitted into the bottom ROWS rows of a transparent picture of `size`
    pixels, by its height or by its width when that would be too wide, and centred there. A
    picture `above` rows taller shows as much more of the drawing over the area."""
    bx, by, bw, bh = box
    box_w, box_h = size
    lift = box_h * above // (ROWS + above)
    rows_h = box_h - lift
    height = rows_h
    width = height * bw / bh
    if width > box_w:
        width, height = box_w, box_w * bh / bw
    width, height = round(width), round(height)
    rise = lift * bh / height
    cropped = re.sub(
        r'viewBox="[^"]+"', f'viewBox="{bx} {by - rise} {bw} {bh + rise}"', svg_text, count=1
    )
    art = Image.open(io.BytesIO(render(cropped, width, height + lift)))
    canvas = Image.new("RGBA", (box_w, box_h), (0, 0, 0, 0))
    canvas.alpha_composite(art, ((box_w - width) // 2, (rows_h - height) // 2))
    out = io.BytesIO()
    canvas.save(out, "PNG", optimize=True)
    return out.getvalue()


def main(given=()):
    given = [Path(folder) for folder in given]
    OUT.mkdir(exist_ok=True)
    for old in OUT.glob("*.png"):
        old.unlink()
    listing = {"cells": CELLS, "step": STEP, "above": ABOVE, "mascots": {}}
    for make in argonauta.MASCOTS:
        probe = make(argonauta.Motion("off"), "x", False)
        name = probe["name"]
        frames = frames_of(probe["cycle"])
        ultra = {
            state: [frame_svg(make, state, k, frames, given) for k in range(frames)]
            for state in argonauta.STATES
            if state not in STILL
        }
        box = area(name, [svg for drawn in ultra.values() for _, svg in drawn])
        listing["mascots"][name] = {"area": box, "cycle": probe["cycle"], "frames": frames}
        for cw, ch in CELLS:
            size = (COLUMNS * cw, ROWS * ch)
            tall = (COLUMNS * cw, (ROWS + ABOVE) * ch)
            for state in STILL:
                stem, svg = argonauta.build(make, state)
                (OUT / f"{stem}-{cw}x{ch}.png").write_bytes(png(svg, box, size))
            for drawn in ultra.values():
                for k, (stem, svg) in enumerate(drawn):
                    picture = png(svg, box, tall, ABOVE)
                    (OUT / f"{stem}-{cw}x{ch}-{k:02d}.png").write_bytes(picture)
    (HERE / "pictures.json").write_text(json.dumps(listing, indent=1) + "\n")
    write_list(listing)
    print("wrote", len(list(OUT.glob("*.png"))), "pictures")


def write_list(listing):
    """The Rust list of every picture, which the header builds in, in pictures.json's order."""
    (cw, ch), = CELLS
    lines = [
        "//! Every picture the header builds in, by mascot and look, as `assets/mascots/pictures.py`",
        "//! writes them with `pictures.json`. Run that script to draw them again; it writes this",
        "//! list too.",
        "",
        "/// A mascot's pictures: its two stills, and the frames of its ultra looks.",
        "pub(super) struct Drawn {",
        "    pub normal: &'static [u8],",
        "    pub light: &'static [u8],",
        "    pub ultracode: &'static [&'static [u8]],",
        "    pub codex: &'static [&'static [u8]],",
        "}",
        "",
        "macro_rules! picture {",
        "    ($name:literal) => {",
        '        include_bytes!(concat!("../../../assets/mascots/pictures/", $name)).as_slice()',
        "    };",
        "}",
    ]
    for name, listed in listing["mascots"].items():
        constant = name.rsplit("-", 1)[1].upper()
        lines += ["", f"pub(super) static {constant}: Drawn = Drawn {{"]
        for state in STILL:
            stem = name if state == "normal" else f"{name}-{state}"
            lines.append(f'    {state}: picture!("{stem}-{cw}x{ch}.png"),')
        for state in ("ultracode", "codex"):
            lines.append(f"    {state}: &[")
            for k in range(listed["frames"]):
                lines.append(f'        picture!("{name}-{state}-{cw}x{ch}-{k:02d}.png"),')
            lines.append("    ],")
        lines.append("};")
    (HERE.parent.parent / "src" / "base" / "mascot" / "frames.rs").write_text("\n".join(lines) + "\n")


if __name__ == "__main__":
    main(sys.argv[1:])
