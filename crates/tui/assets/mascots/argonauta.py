"""Builds the mode mascots in their four states: normal amber, a darker amber for light
terminals, Claude Code's ultracode in purple and Codex's ultra in blue. In the ultra states each
mascot turns something of its own to gold and moves its own way: the argonaut holds up a gold
argonaut shell and ripples with colour, the medusa's rim lights up gold as it pulses and glows
down its tentacles, and the hydra's strong head grows gold horns, the immortal head of the myth,
as light climbs the necks.

Limbs taper from body to tip, so each is one filled path: two curves offset from a centre line,
closed by a round tip. Colours are written as MAIN, DARK, RIM and MARK and filled in per state.
Every animation is a function of t, the time through one cycle from 0 to 1, so a still frame and
the animated file come from the same numbers.

Each mascot's story is a comment at the top of its SVG and beside its cells, so a redraw keeps it.

Usage: python3 argonauta.py              writes svg/a-*.svg, then the pictures and the cells the
                                         welcome header reads (pictures.py, cells.py)
       python3 argonauta.py frames DIR N  also writes N still frames of each ultra cycle
"""

import math
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
OUT = HERE / "svg"
EYE = "#2B2B2B"
GOLD = "#E8A33D"
GOLD_DARK = "#B07020"
GOLD_LIGHT = "#FFD98A"
STATES = {
    "normal": ("#E8A33D", "#B07020"),
    # The darker amber of the block cells' light palette, so the picture reads on a light terminal.
    "light": ("#B07020", "#7A4A12"),
    "ultracode": ("#AF87FF", "#6F45C8"),
    "codex": ("#6AA6FF", "#2E6FD1"),
}
# The states that take the ultra look: the mark of excellence and the motion.
ULTRA = ("ultracode", "codex")
# Who each mascot is, said at the top of its SVG and beside its cells.
STORIES = {
    "a-captain-argonaut": (
        "An argonaut, the real octopus named after Jason's crew, with one arm raised the way "
        "Aristotle believed it sailed the sea: it steers itself and does the work itself, as the "
        "captain does."
    ),
    "a-orchestrator-medusa": (
        "A medusa, as a jellyfish is called after the Gorgon: one beat of its bell sets every "
        "tentacle stroking in time, as the orchestrator hands out the parts and keeps the crew in "
        "step."
    ),
    "a-council-hydra": (
        "The hydra that Heracles, one of the Argonauts, fought, with three heads arguing over one "
        "body: one strong, one overwhelmed, and one goofy, like whichever model everyone is "
        "teasing that day, so the other two catch its mistakes."
    ),
}
# Keyframes per cycle in the animated files.
SAMPLES = 36


# Shapes -------------------------------------------------------------------------------------

def limb(p0, p1, p2, p3, w0, w1, fill="MAIN"):
    """A tapering limb along the cubic p0..p3, w0 wide at p0 and w1 at the round tip."""
    pts = [p0, p1, p2, p3]
    tangents = [sub(p1, p0), sub(p2, p0), sub(p3, p1), sub(p3, p2)]
    widths = [w0, w0 + (w1 - w0) / 3, w0 + 2 * (w1 - w0) / 3, w1]
    a, b = [], []
    for p, t, w in zip(pts, tangents, widths):
        n = unit((-t[1], t[0]))
        a.append(add(p, scale(n, w / 2)))
        b.append(add(p, scale(n, -w / 2)))
    r = w1 / 2
    d = (
        f"M{f(a[0])} C{f(a[1])} {f(a[2])} {f(a[3])} "
        f"A{r:.2f} {r:.2f} 0 0 0 {f(b[3])} "
        f"C{f(b[2])} {f(b[1])} {f(b[0])} Z"
    )
    return f'<path d="{d}" fill="{fill}"/>'


def sub(p, q):
    return (p[0] - q[0], p[1] - q[1])


def add(p, q):
    return (p[0] + q[0], p[1] + q[1])


def scale(p, k):
    return (p[0] * k, p[1] * k)


def unit(v):
    n = math.hypot(*v) or 1.0
    return (v[0] / n, v[1] / n)


def f(p):
    return f"{p[0]:.2f} {p[1]:.2f}".replace(".00", "")


def eye(x, y, r=3.6):
    """The mascots' eye: a dark dot with a small glint in the body colour."""
    return (
        f'<circle cx="{x}" cy="{y}" r="{r}" fill="{EYE}"/>'
        f'<circle cx="{x + r * 0.33:.2f}" cy="{y - r * 0.33:.2f}" r="{r * 0.31:.2f}" fill="MAIN"/>'
    )


def lidded(x, y, r, outer_lid, inner_lid, side, glint=True):
    """An eye cut by a straight lid. Lid heights are in radii above the centre, at the outer
    and inner corners; side is -1 for a left eye and 1 for a right one."""
    left, right = (outer_lid, inner_lid) if side < 0 else (inner_lid, outer_lid)
    y1, y2 = y - left * r, y - right * r
    pts = []
    steps = 400
    for i in range(steps + 1):
        px = x - r + 2 * r * i / steps
        py = y1 + (y2 - y1) * i / steps
        if (px - x) ** 2 + (py - y) ** 2 <= r * r:
            pts.append((px, py))
    p1, p2 = pts[0], pts[-1]
    large = 1 if (p1[1] + p2[1]) / 2 < y else 0
    out = f'<path d="M{p1[0]:.2f} {p1[1]:.2f} A{r} {r} 0 {large} 0 {p2[0]:.2f} {p2[1]:.2f} Z" fill="{EYE}"/>'
    if glint:
        out += f'<circle cx="{x + r * 0.3:.2f}" cy="{y + r * 0.2:.2f}" r="{r * 0.28:.2f}" fill="MAIN"/>'
    return out


def googly(x, y, look, r, spin=("", "")):
    """A googly eye: a dark ring around the body colour, with a small pupil pushed to one side.
    spin is a transform for the pupil, so it can roll around the eye."""
    attr, child = spin
    return (
        f'<circle cx="{x}" cy="{y}" r="{r}" fill="MAIN" stroke="{EYE}" stroke-width="1.8"/>'
        f'<g {attr}>{child}<circle cx="{x + look[0]}" cy="{y + look[1]}" r="{max(1.6, r * 0.32):.2f}" fill="{EYE}"/></g>'
    )


def spiral_eye(x, y, spin=("", "")):
    """A dizzy eye: a dark spiral, which can turn."""
    attr, child = spin
    return (
        f'<g {attr}>{child}<path d="M{x} {y} a0.9 0.9 0 0 1 1.8 0 a1.8 1.8 0 0 1 -3.6 0 '
        f'a2.7 2.7 0 0 1 5.4 0 a3.6 3.6 0 0 1 -7.2 0" fill="none" stroke="{EYE}" stroke-width="1.3" '
        f'stroke-linecap="round"/></g>'
    )


def scallop_edge(centres, r):
    """The lower edge of a row of overlapping circles of radius r, as one path."""
    def low_crossing(c1, c2):
        (x1, y1), (x2, y2) = c1, c2
        dx, dy = x2 - x1, y2 - y1
        d = math.hypot(dx, dy)
        h = math.sqrt(max(r * r - (d / 2) ** 2, 0))
        mx, my = (x1 + x2) / 2, (y1 + y2) / 2
        a = (mx - dy / d * h, my + dx / d * h)
        b = (mx + dy / d * h, my - dx / d * h)
        return a if a[1] > b[1] else b
    x0, y0 = centres[0]
    path = f"M{x0 - r:.2f} {y0:.2f}"
    for c1, c2 in zip(centres, centres[1:]):
        px, py = low_crossing(c1, c2)
        path += f" A{r} {r} 0 0 0 {px:.2f} {py:.2f}"
    xn, yn = centres[-1]
    return path + f" A{r} {r} 0 0 0 {xn + r:.2f} {yn:.2f}"


def shell(x, y, r):
    """A gold argonaut shell, the paper-thin coil the animal is named for: a round coil with a
    flared mouth, a spiral groove and a few ribs."""
    k = r / 6
    outline = (
        f"M{x - 5.6 * k:.2f} {y + 1.4 * k:.2f} "
        f"C{x - 6.4 * k:.2f} {y - 4.2 * k:.2f} {x - 1.2 * k:.2f} {y - 6.6 * k:.2f} {x + 3.2 * k:.2f} {y - 4.6 * k:.2f} "
        f"C{x + 7.6 * k:.2f} {y - 2.6 * k:.2f} {x + 8.2 * k:.2f} {y + 3.4 * k:.2f} {x + 4.6 * k:.2f} {y + 5.6 * k:.2f} "
        f"C{x + 1.6 * k:.2f} {y + 7.4 * k:.2f} {x - 4.6 * k:.2f} {y + 6.4 * k:.2f} {x - 5.6 * k:.2f} {y + 1.4 * k:.2f} Z"
    )
    groove = (
        f"M{x - 1.6 * k:.2f} {y + 0.4 * k:.2f} a{1.2 * k:.2f} {1.2 * k:.2f} 0 0 1 {2.4 * k:.2f} 0 "
        f"a{2.2 * k:.2f} {2.2 * k:.2f} 0 0 1 {-4.4 * k:.2f} 0 a{3.3 * k:.2f} {3.3 * k:.2f} 0 0 1 {6.6 * k:.2f} 0"
    )
    ribs = " ".join(
        f"M{x + a * k:.2f} {y + b * k:.2f} L{x + c * k:.2f} {y + d * k:.2f}"
        for a, b, c, d in ((3.6, -1.4, 6.2, -1.6), (3.4, 2.0, 5.6, 3.6), (0.6, 3.6, 1.0, 5.8))
    )
    return [
        f'<path d="{outline}" fill="{GOLD}" stroke="{EYE}" stroke-width="0.9" stroke-linejoin="round"/>',
        f'<path d="{groove} {ribs}" fill="none" stroke="{GOLD_DARK}" stroke-width="1.1" stroke-linecap="round"/>',
    ]


# Motion -------------------------------------------------------------------------------------

def ease(x):
    return x * x * (3 - 2 * x)


def segment(t, a, b):
    """0 before a, 1 after b, easing in between."""
    if t <= a:
        return 0.0
    if t >= b:
        return 1.0
    return ease((t - a) / (b - a))


def bump(t, centre, width):
    return math.exp(-(((t - centre) / width) ** 2))


class Motion:
    """How to draw a moving part: not at all, still at one moment t, or animated over a cycle."""

    def __init__(self, kind, t=0.0, cycle=3.4):
        self.kind, self.t, self.cycle = kind, t, cycle

    @property
    def on(self):
        return self.kind != "off"

    def _values(self, fn, fmt):
        values = ";".join(fmt(fn(i / SAMPLES)) for i in range(SAMPLES + 1))
        times = ";".join(f"{i / SAMPLES:.3f}" for i in range(SAMPLES + 1))
        return values, times

    def transform(self, kind, fn):
        """The attribute and child element that move a group: kind is translate, rotate or scale,
        and fn(t) gives its numbers."""
        fmt = lambda v: " ".join(f"{x:.2f}" for x in v)
        if self.kind == "smil":
            values, times = self._values(fn, fmt)
            return "", (
                f'<animateTransform attributeName="transform" type="{kind}" values="{values}" '
                f'keyTimes="{times}" dur="{self.cycle}s" repeatCount="indefinite"/>'
            )
        t = self.t if self.kind == "frame" else 0.0
        return f'transform="{kind}({fmt(fn(t))})"', ""

    def fade(self, fn):
        """The attribute and child element that set a group's opacity from fn(t)."""
        if self.kind == "smil":
            values, times = self._values(fn, lambda v: f"{v:.2f}")
            return 'opacity="0"', (
                f'<animate attributeName="opacity" values="{values}" keyTimes="{times}" '
                f'dur="{self.cycle}s" repeatCount="indefinite"/>'
            )
        t = self.t if self.kind == "frame" else 0.0
        return f'opacity="{fn(t):.2f}"', ""

    def group(self, kind, fn, body):
        attr, child = self.transform(kind, fn)
        return [f"<g {attr}>{child}"] + body + ["</g>"]

    def glow(self, fn, body):
        attr, child = self.fade(fn)
        return [f"<g {attr}>{child}"] + body + ["</g>"]

    def spin(self, fn):
        return self.transform("rotate", fn) if self.on else ("", "")


def gradient(gid, colour, peak, vertical=False):
    x2, y2 = ("0", "1") if vertical else ("1", "0")
    return [
        f'<linearGradient id="{gid}" x1="0" y1="0" x2="{x2}" y2="{y2}">',
        f'<stop offset="0" stop-color="{colour}" stop-opacity="0"/>',
        f'<stop offset="0.5" stop-color="{colour}" stop-opacity="{peak}"/>',
        f'<stop offset="1" stop-color="{colour}" stop-opacity="0"/>',
        "</linearGradient>",
    ]


# Mascots ------------------------------------------------------------------------------------

def argonaut(motion, stem, ultra):
    arm = dict(w0=9.6, w1=3.0)
    clouds = []
    if motion.on:
        # Passing clouds: dark bands rippling over the skin, the way an octopus flashes colour.
        bands = [f'<rect x="{x}" y="-60" width="20" height="220" fill="url(#{stem}-cloud)"/>' for x in (-26, 4, 34)]
        clouds = ['<g transform="rotate(-28 50 50)">'] + motion.group(
            "translate", lambda t: (-80 + 190 * segment(t, 0.0, 0.72), 0), bands
        ) + ["</g>"]
    mark = []
    if ultra:
        # The gold shell, held up at the top of the raised arm, glinting once a cycle.
        sx, sy = 86.5, 7.2
        held = shell(sx, sy, 6.2)
        if motion.on:
            held = [f'<g transform="translate({sx} {sy})">'] + motion.group(
                "scale", lambda t: (1 + 0.22 * bump(t, 0.12, 0.06),), [f'<g transform="translate({-sx} {-sy})">'] + held + ["</g>"]
            ) + ["</g>"]
        mark = [("mark of excellence: a gold argonaut shell, held up", held)]
    return {
        "name": "a-captain-argonaut",
        "title": "Captain: the argonaut, one arm raised.",
        "cycle": 3.4,
        "defs": gradient(f"{stem}-cloud", "DARK", 0.75),
        "body": [
            ("raised arm, curling over at the tip", [
                limb((57, 48), (64, 38), (66, 24), (79, 14), 9.4, 4.4),
                limb((79, 14), (85, 9.5), (91, 14), (87.5, 20.5), 4.4, 2.5),
            ]),
            ("arms, four in a skirt, the outer ones curling up", [
                limb((37, 56), (27, 63), (15, 65), (12, 56), **arm),
                limb((42, 60), (40, 72), (34, 82), (28, 80), **arm),
                limb((50, 60), (52, 72), (58, 82), (64, 80), **arm),
                limb((55, 56), (65, 63), (77, 65), (80, 56), **arm),
            ]),
            ("crown, where the arms meet under the mantle", '<ellipse cx="46" cy="57" rx="12.5" ry="7.5" fill="MAIN"/>'),
            ("mantle, the head", '<path d="M46 12 C59 12 66 23 66 35.5 C66 48 58 56.5 46 56.5 C34 56.5 26 48 26 35.5 C26 23 33 12 46 12 Z" fill="MAIN"/>'),
        ],
        "overlay": clouds,
        "eyes": [("eyes", [eye(39.5, 42), eye(52.5, 42)])],
        "mark": mark,
    }


def baton(grip, tip):
    """A gold conductor's baton: a thin gold stick with a round grip."""
    (gx, gy), (tx, ty) = grip, tip
    return [
        f'<path d="M{gx} {gy} L{tx} {ty}" stroke="{EYE}" stroke-width="3.8" stroke-linecap="round"/>',
        f'<path d="M{gx} {gy} L{tx} {ty}" stroke="{GOLD}" stroke-width="2.2" stroke-linecap="round"/>',
        f'<circle cx="{gx}" cy="{gy}" r="2.7" fill="{GOLD}" stroke="{EYE}" stroke-width="0.8"/>',
    ]


def medusa(motion, stem, ultra):
    tentacle = lambda x, length: limb((x, 55), (x - 6, 55 + length / 3), (x + 6, 55 + 2 * length / 3), (x, 55 + length), 6.4, 3.2)
    lobes = [21, 30.7, 40.3, 50, 59.7, 69.3, 79]
    sag = lambda x: 54 + 2.2 * (1 - ((x - 50) / 29) ** 2)
    glow, raised, mark = [], [], []
    if motion.on:
        # A glow that runs from the top of the bell down every tentacle, on the beat.
        band = [f'<rect x="-10" y="0" width="120" height="30" fill="url(#{stem}-glow)"/>']
        glow = motion.group("translate", lambda t: (0, -34 + 142 * segment(t, 0.06, 0.66)), band)
    if ultra:
        # In ultra it conducts: one tentacle rises beside the bell with a gold baton, and the
        # baton flicks down on every beat.
        raised = [("conducting tentacle, raised beside the bell", limb((79, 52), (88, 46), (92, 34), (88, 25), 6.4, 3.4))]
        grip, tip = (86.2, 28.2), (95.5, 8.5)
        stick = baton(grip, tip)
        if motion.on:
            stick = motion.group("rotate", lambda t: (16 * bump(t, 0.1, 0.07), *grip), stick)
        mark = [("mark of excellence: a gold conductor's baton", stick)]
    beat = (lambda t: (1 + 0.045 * bump(t, 0.1, 0.055),)) if motion.on else None
    return {
        "name": "a-orchestrator-medusa",
        "title": "Orchestrator: the medusa, every tentacle on one beat.",
        "cycle": 1.8,
        "beat": ((50, 52), beat),
        "defs": gradient(f"{stem}-glow", "#fff", 0.6, vertical=True),
        "body": raised + [
            ("tentacles, five, all swaying in the same phase", [
                tentacle(26, 30), tentacle(38, 35), tentacle(50, 38), tentacle(62, 35), tentacle(74, 30),
            ]),
            ("bell", '<path d="M16 54 C16 30 31.5 15 50 15 C68.5 15 84 30 84 54 Z" fill="MAIN"/>'),
            ("margin of the bell, a row of small lobes", [f'<circle cx="{x}" cy="{sag(x):.2f}" r="5.3" fill="MAIN"/>' for x in lobes]),
        ],
        "overlay": glow,
        "eyes": [("eyes", [eye(41, 35), eye(59, 35)])],
        "mark": mark,
    }


def hydra(motion, stem, ultra):
    slumped = 'transform="rotate(22 21 48)"'
    tipped = 'transform="translate(5.5 2) rotate(-26 80 37)"'
    rise, flare = [], []
    horns = [
        limb((42.5, 13.5), (35.5, 11.5), (28.5, 12), (25, 6), 6.2, 1.5, fill="MARK"),
        limb((57.5, 13.5), (64.5, 11.5), (71.5, 12), (75, 6), 6.2, 1.5, fill="MARK"),
    ]
    if motion.on:
        # Light climbing from the body up all three necks.
        band = [f'<rect x="-10" y="0" width="120" height="30" fill="url(#{stem}-rise)"/>']
        rise = motion.group("translate", lambda t: (0, 104 - 144 * segment(t, 0.0, 0.62)), band)
        # When it reaches the top, the gold horns of the immortal head flare.
        lit = [h.replace('fill="MARK"', f'fill="{GOLD_LIGHT}"') for h in horns]
        flare = motion.glow(lambda t: 0.9 * bump(t, 0.6, 0.07), lit)
    # In ultra the goofy head's pupils roll round once, and the dizzy spirals keep turning.
    roll = lambda cx, cy: motion.spin(lambda t: (360 * segment(t, 0.18, 0.5), cx, cy))
    turn = lambda cx, cy, way: motion.spin(lambda t: (way * 360 * t, cx, cy))
    head = lambda cx, cy, rx, ry: f'<ellipse cx="{cx}" cy="{cy}" rx="{rx}" ry="{ry}" fill="MAIN"/>'
    return {
        "name": "a-council-hydra",
        "title": "Council: the hydra. One head strong, one overwhelmed, one goofy.",
        "cycle": 3.6,
        "shift": (-2.5, 5),
        "defs": gradient(f"{stem}-rise", "#fff", 0.55, vertical=True),
        "body": [
            ("necks, widening into the body: the strong one rising straight, the overwhelmed one "
             "sagging, the goofy one curving out", [
                limb((36, 77), (26, 70), (15, 66), (19, 56), 15.5, 9.2),
                limb((48, 77), (49, 61), (51.5, 45), (50, 32), 16.5, 10.6),
                limb((60, 77), (71.5, 71), (80.5, 60), (84, 49), 15.5, 9.2),
            ]),
            ("body, one shape that flows into the tail, which curls up at the tip",
             '<path d="M30 71 C23 72 19 78 21 83 C23 88 32 91 46 91 C58 91 66 89 73 86 '
             'C80 83 87 80 91 75 C94 71 94 67 92 64.5 A1.6 1.6 0 0 0 89 66 C90 69 89 73 85 75.5 '
             'C80 78.5 74 79 69 77 C66 73 60 70 50 69.5 C41 69 35 70 30 71 Z" fill="MAIN"/>'),
            ("the overwhelmed head, slumped low, its horns wilting", [
                f"<g {slumped}>",
                limb((15, 40), (11.5, 38), (8, 40), (7, 45), 4.6, 1.2, fill="DARK"),
                limb((27, 40), (30.5, 38), (34, 40), (35, 45), 4.6, 1.2, fill="DARK"),
                head(21, 46, 11.5, 10.5),
                "</g>",
            ]),
            ("the strong head, biggest and upright, horns swept back; in ultra they turn gold, "
             "the immortal head of the myth", horns + flare + [head(50, 21, 13, 12)]),
            ("the goofy head, tipped sideways, one horn up and one flopped over", [
                f"<g {tipped}>",
                limb((75, 27), (74, 21), (73.5, 16), (75, 11.5), 4.4, 1.3, fill="DARK"),
                limb((86, 27.5), (90.5, 23.5), (96, 24.5), (97.5, 30), 4.4, 1.3, fill="DARK"),
                head(80, 36, 12.5, 12),
                "</g>",
            ]),
        ],
        "overlay": rise,
        "eyes": [
            ("overwhelmed: dizzy spiral eyes", [
                f"<g {slumped}>",
                spiral_eye(16.5, 45.5, turn(16.5, 45.5, -1)),
                spiral_eye(25.5, 45.5, turn(25.5, 45.5, 1)),
                "</g>",
            ]),
            ("strong: heavy brows over a narrowed glare", [
                f'<path d="M40.5 14.8 L47.5 18.2 M59.5 14.8 L52.5 18.2" stroke="{EYE}" stroke-width="2.6" stroke-linecap="round"/>',
                lidded(44.5, 21, 3.7, outer_lid=0.85, inner_lid=0.15, side=-1),
                lidded(55.5, 21, 3.7, outer_lid=0.85, inner_lid=0.15, side=1),
            ]),
            ("goofy: huge mismatched googly eyes looking two ways", [
                f"<g {tipped}>",
                googly(73.8, 34, (-3, -3.2), 7, roll(73.8, 34)),
                googly(87.6, 36.2, (1.8, 2), 4.2, roll(87.6, 36.2)),
                "</g>",
            ]),
        ],
        "mark": [],
    }


# Assembly -----------------------------------------------------------------------------------

def flat(parts):
    out = []
    for name, body in parts:
        out.append(f"<!-- {name} -->")
        out += body if isinstance(body, list) else [body]
    return out


def silhouette(lines):
    """The same shapes in white, for the mask that keeps light and colour on the body."""
    out = []
    for line in lines:
        if line.startswith("<!--"):
            continue
        while "<animate" in line:
            start = line.index("<animate")
            end = line.index("/>", start) + 2
            line = line[:start] + line[end:]
        for colour in ("MAIN", "DARK", "RIM", "MARK", EYE, GOLD_LIGHT):
            line = line.replace(f'fill="{colour}"', 'fill="#fff"').replace(f'stroke="{colour}"', 'stroke="#fff"')
        line = line.replace('opacity="0"', 'opacity="1"')
        out.append(line)
    return out


def build(make, state, motion_kind="off", t=0.0):
    """One SVG: make is a mascot, motion_kind is off (a still), frame (still at t) or smil."""
    main, dark = STATES[state]
    ultra = state in ULTRA
    probe = make(Motion("off"), "x", ultra)
    animated = ultra and motion_kind == "smil"
    stem = probe["name"] + ("" if state == "normal" else f"-{state}") + ("-animated" if animated else "")
    motion = Motion(motion_kind if ultra else "off", t, probe["cycle"])
    mascot = make(motion, stem, ultra)
    dx, dy = mascot.get("shift", (0, 0))
    shift = dx or dy
    body, eyes, mark = flat(mascot["body"]), flat(mascot["eyes"]), flat(mascot["mark"])
    label = {
        "normal": "Normal",
        "light": "Light, darker amber",
        "ultracode": "Ultracode, purple",
        "codex": "Codex ultra, blue",
    }[state]
    lines = ['<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">']
    lines.append(f"  <!-- {STORIES[probe['name']]} -->")
    lines.append(f"  <!-- Argonauta. {mascot['title']} {label}{', animated' if animated else ''}. -->")
    moving = motion.on and mascot["overlay"]
    if moving:
        lines.append("  <defs>")
        lines += [f"    {l}" for l in mascot["defs"]]
        lines.append("    <!-- the body's outline, so the effect only falls on the mascot -->")
        lines.append(f'    <mask id="{stem}-body" maskUnits="userSpaceOnUse" x="0" y="0" width="100" height="100">')
        inner = silhouette(body + eyes)
        if shift:
            inner = [f'<g transform="translate({dx} {dy})">'] + inner + ["</g>"]
        lines += [f"      {l}" for l in inner]
        lines += ["    </mask>", "  </defs>"]
    wrap_open, wrap_close = [], []
    beat_centre, beat = mascot.get("beat", (None, None))
    if motion.on and beat:
        cx, cy = beat_centre
        attr, child = motion.transform("scale", beat)
        wrap_open = [f'<g transform="translate({cx} {cy})">', f"<g {attr}>{child}", f'<g transform="translate({-cx} {-cy})">']
        wrap_close = ["</g>", "</g>", "</g>"]
    if shift:
        wrap_open.append(f'<g transform="translate({dx} {dy})">')
        wrap_close.insert(0, "</g>")
    lines += [f"  {l}" for l in wrap_open]
    lines += [f"  {l}" for l in body]
    if moving:
        lines.append("  <!-- the effect, kept to the body by its outline -->")
        unshift = f'<g transform="translate({-dx} {-dy})">' if shift else ""
        lines.append(f'  {unshift}<g mask="url(#{stem}-body)">')
        lines += [f"    {l}" for l in mascot["overlay"]]
        lines.append("  </g>" + ("</g>" if shift else ""))
    lines += [f"  {l}" for l in eyes]
    lines += [f"  {l}" for l in mark]
    lines += [f"  {l}" for l in wrap_close]
    lines += ["</svg>", ""]
    text = "\n".join(lines)
    colours = {
        '"MAIN"': main, '"DARK"': dark,
        '"RIM"': GOLD if ultra else main,
        '"MARK"': GOLD if ultra else dark,
    }
    for token, colour in colours.items():
        text = text.replace(token, f'"{colour}"')
    return stem, text


MASCOTS = [argonaut, medusa, hydra]


def main():
    OUT.mkdir(exist_ok=True)
    for make in MASCOTS:
        for state in STATES:
            stem, text = build(make, state)
            (OUT / f"{stem}.svg").write_text(text)
            if state in ULTRA:
                stem, text = build(make, state, "smil")
                (OUT / f"{stem}.svg").write_text(text)
    print("wrote", len(list(OUT.glob("a-*.svg"))), "files")
    if len(sys.argv) == 1:
        import cells
        import pictures

        pictures.main()
        cells.main()
    if len(sys.argv) > 3 and sys.argv[1] == "frames":
        out, n = Path(sys.argv[2]), int(sys.argv[3])
        out.mkdir(parents=True, exist_ok=True)
        for make in MASCOTS:
            for state in ("ultracode", "codex"):
                for i in range(n):
                    stem, text = build(make, state, "frame", i / n)
                    (out / f"{stem}-{i:02d}.svg").write_text(text)
        print("wrote frames to", out)


if __name__ == "__main__":
    main()
