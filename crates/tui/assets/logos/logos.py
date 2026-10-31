"""The logos drawn before a CLI's or a model's name, as PNGs made from the SVGs in svg/.

Each logo is written at four sizes:
- NAME-cells.png, 36 x 42: two terminal cells of 18 x 42 px, the cell size the app's other
  pictures are drawn for, with the logo 28 px square in their middle;
- NAME-cells-2x.png, 72 x 84: the same for cells twice that size, so a terminal with larger cells
  scales a picture down rather than up;
- NAME-64.png and NAME-128.png: the logo filling a square, for pictures that draw it themselves.

A logo drawn in one colour takes the colour of the screen's text. It is written twice: in a light
ink for a dark screen, and with -on-light before .png in a dark ink for a light one. A coloured
logo keeps its own colours on both.

Each logo is drawn at three times its size and scaled down, which keeps thin strokes even.

The SVGs come from LobeHub Icons, @lobehub/icons-static-svg 1.95.1 (MIT; the notice is in the
crate's NOTICE). Each logo is a trademark of the maker or project it names, used only to
name the programs and models the app works with.

Needs rsvg-convert and Pillow. Usage: python3 logos.py
"""

import io
import subprocess
from pathlib import Path

from PIL import Image

HERE = Path(__file__).resolve().parent
SVG = HERE / "svg"

LOGOS = [
    "claudecode", "codex", "antigravity", "opencode", "ollama", "qwen", "kimi", "openai",
    "mistral", "meta", "openrouter", "claude", "gemini", "gemma",
]

# Name, canvas width and height, and the logo's square size inside it, centred.
SIZES = [
    ("cells", 36, 42, 28),
    ("cells-2x", 72, 84, 56),
    ("64", 64, 64, 64),
    ("128", 128, 128, 128),
]

# One-colour logos draw in currentColor. On a dark screen that is the text colour of the
# terminal the app is drawn for (#c8d3f5, a pale blue); on a light one, that terminal's
# background colour, #222436, a near black.
INKS = [("", "#c8d3f5"), ("-on-light", "#222436")]

RSVG = "rsvg-convert"
SUPERSAMPLE = 3


def drawn(svg_text, side):
    """The SVG drawn into a square `side` px wide, as an RGBA image."""
    big = side * SUPERSAMPLE
    png = subprocess.run(
        [RSVG, "--width", str(big), "--height", str(big), "--format", "png"],
        input=svg_text.encode(), capture_output=True, check=True,
    ).stdout
    image = Image.open(io.BytesIO(png)).convert("RGBA")
    return image.resize((side, side), Image.LANCZOS)


def write(image, path):
    # 8-bit RGBA without interlacing: what the app's own PNG reader takes.
    image.save(path, format="PNG", optimize=True)


def main():
    written = []
    for name in LOGOS:
        text = (SVG / f"{name}.svg").read_text()
        inks = INKS if "currentColor" in text else INKS[:1]
        for suffix, ink in inks:
            svg_text = text.replace("currentColor", ink)
            for size, width, height, side in SIZES:
                canvas = Image.new("RGBA", (width, height), (0, 0, 0, 0))
                logo = drawn(svg_text, side)
                canvas.alpha_composite(logo, ((width - side) // 2, (height - side) // 2))
                path = HERE / f"{name}-{size}{suffix}.png"
                write(canvas, path)
                written.append(path.name)
    for stale in HERE.glob("*.png"):
        if stale.name not in written:
            stale.unlink()
    print(f"{len(written)} pictures in {HERE}")


if __name__ == "__main__":
    main()
