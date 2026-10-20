"""The block version of each mascot, for terminals that can't draw a picture: 3 rows of 9 cells,
drawn on a grid of 18 x 6 quadrants.

Each grid is six strings of eighteen letters, one per quadrant: A the body, D its darker parts,
K the eyes, and . the terminal's own background. The ultra looks turn the quadrants in STARS to
G, the gold mark. Every cell takes two quadrant rows and two columns and holds one foreground
and one background colour, so a cell mixes at most two letters, and `.` can only be the
background. `cells()` turns a grid into block characters.

The grids are drawn by hand after the SVGs, at this size a shape rather than a likeness: the
hydra's after its final drawing, the overwhelmed head slumped low and the tail curling up.

Writes cells.json: each mascot's story, grid, cells and ultra cells, and the palettes, each
letter's colour per look. The header reads that file. Usage: python3 cells.py
"""

import json
from pathlib import Path

import argonauta

HERE = Path(__file__).resolve().parent

PALETTES = {
    "normal": {"A": "#E8A33D", "D": "#B07020", "K": "#2B2B2B", "G": "#E8A33D"},
    "light": {"A": "#B07020", "D": "#7A4A12", "K": "#2B2B2B", "G": "#B07020"},
    "ultracode": {"A": "#AF87FF", "D": "#6F45C8", "K": "#2B2B2B", "G": "#E8A33D"},
    "codex": {"A": "#6AA6FF", "D": "#2E6FD1", "K": "#2B2B2B", "G": "#E8A33D"},
}
# The light that sweeps across the body in the ultra looks, as Claude Code's own shimmer is lit.
SHIMMER = {"ultracode": "#D7C2FF", "codex": "#BFD8FF"}
# The gold mark of each ultra look, by quadrant: the shell at the top of the argonaut's raised
# arm, the medusa's baton held up beside its bell, and the gold horns on top of the hydra's
# strong head. (x, y), or (x, y, letter) for a quadrant of another colour, such as the tentacle
# that holds the baton.
STARS = {
    "a-captain-argonaut": [(14, 0), (15, 0)],
    "a-orchestrator-medusa": [(15, 1, "A"), (16, 1), (17, 0)],
    "a-council-hydra": [(8, 0), (9, 0)],
}
GRIDS = {
    "a-captain-argonaut": [
        "....AAAAAA...AAA..",
        "...AAAAAAAA..A..A.",
        "...AAKAAKAA..A....",
        "...AAAAAAAAAAA....",
        "...A.A..A.A.......",
        "..A..A..A..A......",
    ],
    "a-orchestrator-medusa": [
        "....AAAAAAAAAA....",
        "...AAKAAAAAAKAA...",
        "..AAAAAAAAAAAAAA..",
        "..AAAAAAAAAAAAAA..",
        "...A..A.AA.A..A...",
        "...A..A.AA.A..A...",
    ],
    "a-council-hydra": [
        "......AAAAAA..AAKA",
        "......AKAAKA..AKAA",
        "AAA.....AA....AAD.",
        "AKAA....AA...AA...",
        "..AAAAAAAAAAAA..A.",
        "....AAAAAAAAAAAA..",
    ],
}

# Bits for the four quadrants of a cell.
UL, UR, LL, LR = 8, 4, 2, 1
CHARS = {
    0: " ", UL: "▘", UR: "▝", LL: "▖", LR: "▗",
    UL | UR: "▀", LL | LR: "▄", UL | LL: "▌", UR | LR: "▐",
    UL | LR: "▚", UR | LL: "▞",
    UL | UR | LL: "▛", UL | UR | LR: "▜", UL | LL | LR: "▙", UR | LL | LR: "▟",
    15: "█",
}
# Characters some terminals draw two cells wide when set to treat ambiguous width as wide.
AMBIGUOUS = set("▀▄█▌")


def problems(grid):
    """Cells that mix three colours, or put the terminal's background in front."""
    bad = []
    for cy in range(3):
        for cx in range(9):
            quad = {grid[2 * cy + dy][2 * cx + dx] for dy in (0, 1) for dx in (0, 1)}
            if len(quad) > 2:
                bad.append(f"cell {cx},{cy} mixes {sorted(quad)}")
    return bad


def cells(grid):
    """Rows of (char, fg, bg) for a 6 x 18 letter grid; bg None is the terminal's own."""
    assert len(grid) == 6 and all(len(row) == 18 for row in grid), grid
    rows = []
    for cy in range(3):
        row = []
        for cx in range(9):
            quad = [
                grid[2 * cy][2 * cx], grid[2 * cy][2 * cx + 1],
                grid[2 * cy + 1][2 * cx], grid[2 * cy + 1][2 * cx + 1],
            ]
            row.append(cell(quad, cx, cy))
        rows.append(row)
    return rows


def cell(quad, cx, cy):
    colours = sorted(set(quad))
    mask = lambda c: sum(bit for bit, q in zip((UL, UR, LL, LR), quad) if q == c)
    if colours == ["."]:
        return (" ", None, None)
    if "." in colours:
        assert len(colours) == 2, f"cell {cx},{cy} mixes {colours}"
        fg = colours[1]
        return (CHARS[mask(fg)], fg, None)
    if len(colours) == 1:
        return ("█", colours[0], None)
    assert len(colours) == 2, f"cell {cx},{cy} mixes {colours}"
    a, b = colours
    options = [(CHARS[mask(a)], a, b), (CHARS[mask(b)], b, a)]
    # Prefer a glyph no terminal widens, then the smaller shape in the foreground.
    options.sort(key=lambda o: (o[0] in AMBIGUOUS, quad.count(o[1])))
    return options[0]


def starred(name, grid):
    out = [list(row) for row in grid]
    for x, y, *letter in STARS[name]:
        out[y][x] = letter[0] if letter else "G"
    return ["".join(row) for row in out]


def main():
    mascots = {}
    for name, grid in GRIDS.items():
        bad = problems(grid) + problems(starred(name, grid))
        assert not bad, f"{name}: {bad}"
        mascots[name] = {
            "story": argonauta.STORIES[name],
            "grid": grid,
            "cells": cells(grid),
            "ultra_cells": cells(starred(name, grid)),
        }
    data = {"palettes": PALETTES, "shimmer": SHIMMER, "mascots": mascots}
    (HERE / "cells.json").write_text(json.dumps(data, ensure_ascii=False, indent=1) + "\n")
    print("wrote cells.json")


if __name__ == "__main__":
    main()
