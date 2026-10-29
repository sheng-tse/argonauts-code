//! The sixteen wonders, one drawing each, in units of the wonder's size. Small
//! details show only when there is room for them: `fine` from 36 px, `finest` from 56.

use std::f64::consts::PI;

use argonauts_core::behavior_world::Wonder;

use super::hand::{EDGE, Hand, Look, Normal, Step, facing as n};
use crate::world::map::fields::Rgb;
use crate::world::raster::Cap;

/// How much room a drawing has for detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detail {
    pub fine: bool,
    pub finest: bool,
}

impl Detail {
    pub fn at(size: f64) -> Self {
        Self {
            fine: size >= 36.0,
            finest: size >= 56.0,
        }
    }
}

/// The colours the wonders are made of.
pub mod colours {
    use crate::world::map::fields::Rgb;

    const fn hex(rgb: u32) -> Rgb {
        [
            ((rgb >> 16) & 0xff) as f64,
            ((rgb >> 8) & 0xff) as f64,
            (rgb & 0xff) as f64,
        ]
    }

    pub const STONE: Rgb = hex(0xd8c8a6);
    pub const MARBLE: Rgb = hex(0xe6e0d2);
    pub const BRICK: Rgb = hex(0xc7a274);
    pub const BRONZE: Rgb = hex(0xb47c42);
    pub const BRONZE_DARK: Rgb = hex(0x8d5c34);
    pub const GOLD: Rgb = hex(0xe0b44c);
    pub const IVORY: Rgb = hex(0xefe5cf);
    pub const WOOD: Rgb = hex(0x7a5034);
    pub const WOOD_DARK: Rgb = hex(0x4e3424);
    pub const SAIL: Rgb = hex(0xece2c8);
    pub const OCHRE: Rgb = hex(0xb5563a);
    pub const LEAF: Rgb = hex(0x6f9e55);
    pub const LEAF_DARK: Rgb = hex(0x4d7b45);
    pub const ROCK: Rgb = hex(0x8a8e9c);
    pub const ROCK_DARK: Rgb = hex(0x6a6c7a);
    pub const FOAM: Rgb = hex(0xdbe7f2);
    pub const FLAME: Rgb = hex(0xffcf6a);
    pub const SERPENT: Rgb = hex(0x4f8f66);
    pub const BELLY: Rgb = hex(0xb7c98a);
    pub const HARPY: Rgb = hex(0x6e5a6c);
    pub const SIREN: Rgb = hex(0xdcb9a6);
    pub const WATER: Rgb = hex(0x2f6c86);
    pub const EBONY: Rgb = hex(0x3c3644);
    pub const HAIR: Rgb = hex(0x3a2c26);
    pub const SKY: Rgb = hex(0x9cc8e8);
    pub const CURL: Rgb = hex(0xfbe7a6);
    pub const CURL_DEEP: Rgb = hex(0x9c7424);
    pub const HORN: Rgb = hex(0xc8962e);
    pub const THRONE_TOP: Rgb = hex(0x4a4456);
}

use colours::*;

/// Shading across a part, left to right.
const SIDES: [Normal; 3] = [[-0.8, 0.0, 0.6], [0.0, 0.0, 1.0], [0.8, 0.0, 0.6]];
const TORSO: [Normal; 3] = [[-0.8, 0.0, 0.6], [-0.1, 0.0, 1.0], [0.7, 0.0, 0.7]];

fn m(u: f64, v: f64) -> Step {
    Step::Move(u, v)
}
fn l(u: f64, v: f64) -> Step {
    Step::Line(u, v)
}
fn q(a: f64, b: f64, u: f64, v: f64) -> Step {
    Step::Quad(a, b, u, v)
}
const Z: Step = Step::Close;

/// Draws `wonder` with `hand`.
pub fn draw(hand: &mut Hand<'_>, wonder: Wonder, detail: Detail) {
    match wonder {
        Wonder::Argo => argo(hand, detail),
        Wonder::Fleece => fleece(hand, detail),
        Wonder::Symplegades => symplegades(hand, detail),
        Wonder::Talos => talos(hand, detail),
        Wonder::Sirens => sirens(hand),
        Wonder::Dragon => dragon(hand, detail),
        Wonder::Harpies => harpies(hand, detail),
        Wonder::Dodona => dodona(hand, detail),
        Wonder::Pyramid => pyramid(hand, detail),
        Wonder::Gardens => gardens(hand, detail),
        Wonder::Colossus => colossus(hand, detail),
        Wonder::Pharos => pharos(hand, detail),
        Wonder::Artemis => artemis(hand, detail),
        Wonder::Zeus => zeus(hand, detail),
        Wonder::Mausoleum => mausoleum(hand, detail),
        Wonder::Atlantis => atlantis(hand),
    }
}

fn argo(g: &mut Hand<'_>, d: Detail) {
    for (u, r, a) in [(-0.06, 0.5, 0.45), (0.42, 0.18, 0.5), (-0.42, 0.14, 0.35)] {
        g.ripple(u, -0.07, r, a);
    }
    // Mast, yard and sail.
    g.line(
        &[(0.0, 0.05), (0.0, 0.62)],
        0.024,
        WOOD_DARK,
        Some(n::FRONT),
        Look::default().min(1.0),
    );
    const SAIL_ACROSS: [Normal; 3] = [[-0.6, 0.2, 0.8], [-0.1, 0.0, 1.0], [0.5, 0.0, 0.85]];
    let sail = [
        m(-0.22, 0.56),
        q(0.0, 0.53, 0.22, 0.56),
        q(0.25, 0.36, 0.23, 0.17),
        q(0.0, 0.2, -0.23, 0.17),
        q(-0.25, 0.36, -0.22, 0.56),
        Z,
    ];
    g.shape(
        &sail,
        SAIL,
        None,
        Look::default().across((-0.24, 0.4), (0.24, 0.36), &SAIL_ACROSS),
    );
    if d.fine {
        let stripe = [
            m(-0.235, 0.34),
            q(0.0, 0.31, 0.235, 0.34),
            l(0.24, 0.27),
            q(0.0, 0.24, -0.24, 0.27),
            Z,
        ];
        g.shape(&stripe, OCHRE, Some(n::FRONT), Look::flat());
    }
    g.line(
        &[(-0.25, 0.565), (0.25, 0.565)],
        0.02,
        WOOD_DARK,
        Some(n::FRONT),
        Look::default().min(1.0),
    );
    // The hull, its stern curling up behind and its prow ending in a ram.
    const HULL_ACROSS: [Normal; 3] = [[0.0, 0.8, 0.6], [0.0, 0.1, 1.0], [0.0, -0.5, 0.85]];
    let hull = [
        m(-0.47, 0.34),
        q(-0.5, 0.2, -0.38, 0.06),
        q(-0.3, -0.04, -0.12, -0.05),
        l(0.3, -0.05),
        l(0.48, -0.02),
        l(0.38, 0.03),
        q(0.42, 0.16, 0.45, 0.24),
        l(0.4, 0.25),
        q(0.36, 0.14, 0.3, 0.1),
        l(-0.3, 0.1),
        q(-0.4, 0.16, -0.42, 0.33),
        Z,
    ];
    g.shape(
        &hull,
        WOOD,
        None,
        Look::default().across((0.0, 0.12), (0.0, -0.06), &HULL_ACROSS),
    );
    if d.fine {
        g.line(
            &[(-0.34, 0.06), (0.33, 0.06)],
            0.014,
            SAIL,
            None,
            Look::raw(0.35),
        );
        for i in 0..7 {
            let u = -0.24 + f64::from(i) * 0.08;
            g.line(
                &[(u, 0.04), (u - 0.07, -0.11)],
                0.012,
                WOOD_DARK,
                Some(n::FRONT),
                Look::default().no_cast(),
            );
        }
    }
    if d.finest {
        g.ellipse(
            0.34,
            0.035,
            0.018,
            0.012,
            IVORY,
            Some(n::FRONT),
            Look::default().no_rim(),
        );
        g.ellipse(
            0.343,
            0.035,
            0.008,
            0.008,
            EDGE,
            None,
            Look::default().no_rim().unshaded(),
        );
    }
}

fn fleece(g: &mut Hand<'_>, d: Detail) {
    g.poly(
        &[(-0.37, 0.0), (-0.23, 0.0), (-0.25, 0.45), (-0.35, 0.48)],
        WOOD,
        None,
        Look::default().across((-0.37, 0.2), (-0.23, 0.2), &SIDES),
    );
    // The bough casts no shade of its own: a long thin one would read as a stick lying on the
    // ground.
    g.line(
        &[(-0.28, 0.43), (0.0, 0.53), (0.35, 0.55)],
        0.05,
        WOOD,
        Some(n::TOP),
        Look::default().min(1.2).no_cast(),
    );
    g.ball(-0.35, 0.56, 0.12, LEAF_DARK, Look::default());
    g.ball(-0.21, 0.62, 0.1, LEAF, Look::default());
    g.ball(0.36, 0.59, 0.09, LEAF, Look::default().no_cast());
    // The fleece hung over the bough: forelegs from its top corners, hind legs below, a wavy
    // hem.
    const PELT_ACROSS: [Normal; 3] = [[-0.75, 0.5, 0.6], [-0.15, 0.2, 1.0], [0.6, -0.3, 0.75]];
    let pelt = [
        m(-0.17, 0.51),
        q(0.05, 0.58, 0.24, 0.53),
        l(0.28, 0.41),
        l(0.24, 0.4),
        q(0.25, 0.29, 0.23, 0.21),
        l(0.28, 0.08),
        l(0.19, 0.11),
        q(0.12, 0.05, 0.04, 0.1),
        q(-0.03, 0.05, -0.1, 0.11),
        l(-0.18, 0.08),
        l(-0.14, 0.21),
        q(-0.16, 0.31, -0.16, 0.4),
        l(-0.23, 0.39),
        Z,
    ];
    g.shape(
        &pelt,
        GOLD,
        None,
        Look::default().across((-0.2, 0.52), (0.3, 0.08), &PELT_ACROSS),
    );
    // The ram's head hanging at its side, with its curled horn.
    let head = [
        m(0.22, 0.52),
        q(0.33, 0.52, 0.34, 0.4),
        q(0.34, 0.33, 0.3, 0.3),
        q(0.26, 0.33, 0.25, 0.4),
        q(0.22, 0.46, 0.22, 0.52),
        Z,
    ];
    g.shape(&head, GOLD, Some(n::SHADE_RIGHT), Look::default());
    if d.fine {
        let horn = [
            m(0.26, 0.5),
            q(0.37, 0.53, 0.37, 0.45),
            q(0.36, 0.4, 0.32, 0.42),
            q(0.3, 0.45, 0.33, 0.46),
        ];
        g.curve(&horn, 0.02, HORN, Some(n::FRONT), Look::default().no_cast());
        for r in 0..4 {
            let columns = if r == 3 { 3 } else { 4 };
            for c in 0..columns {
                let u = -0.09 + f64::from(c) * 0.09 + f64::from(r % 2) * 0.045;
                let v = 0.44 - f64::from(r) * 0.09;
                let tuft = [m(u - 0.028, v), q(u, v + 0.033, u + 0.028, v)];
                let colour = if r < 2 { CURL } else { CURL_DEEP };
                g.curve(&tuft, 0.012, colour, None, Look::raw(0.7));
            }
        }
    }
    if d.finest {
        g.line(
            &[(-0.09, 0.47), (-0.09, 0.47)],
            0.035,
            [255.0, 250.0, 232.0],
            None,
            Look::raw(1.0),
        );
    }
}

fn symplegades(g: &mut Hand<'_>, d: Detail) {
    for (u, r, a) in [(0.0, 0.2, 0.7), (-0.32, 0.22, 0.4), (0.34, 0.2, 0.4)] {
        g.ripple(u, -0.05, r, a);
    }
    // The left rock leans right, the right rock leans left: two facets each.
    g.poly(
        &[
            (-0.47, 0.0),
            (-0.44, 0.3),
            (-0.34, 0.6),
            (-0.3, 0.3),
            (-0.3, 0.0),
        ],
        ROCK,
        Some(n::UP_LEFT),
        Look::default(),
    );
    g.poly(
        &[
            (-0.3, 0.0),
            (-0.3, 0.3),
            (-0.34, 0.6),
            (-0.22, 0.52),
            (-0.07, 0.28),
            (-0.05, 0.0),
        ],
        ROCK,
        Some(n::SHADE_RIGHT),
        Look::default(),
    );
    g.poly(
        &[
            (0.05, 0.0),
            (0.04, 0.3),
            (0.16, 0.62),
            (0.22, 0.3),
            (0.22, 0.0),
        ],
        ROCK,
        Some(n::LIT_LEFT),
        Look::default(),
    );
    g.poly(
        &[
            (0.22, 0.0),
            (0.22, 0.3),
            (0.16, 0.62),
            (0.32, 0.52),
            (0.45, 0.28),
            (0.47, 0.0),
        ],
        ROCK_DARK,
        Some(n::UP_RIGHT),
        Look::default(),
    );
    if d.fine {
        for (u, v, w) in [
            (-0.38, 0.38, 0.06),
            (-0.2, 0.3, 0.05),
            (0.2, 0.44, 0.05),
            (0.35, 0.26, 0.06),
        ] {
            g.line(
                &[(u - w, v), (u + w, v - 0.04)],
                0.012,
                EDGE,
                None,
                Look::raw(0.35),
            );
        }
    }
    for (u, r) in [(-0.06, 0.035), (0.05, 0.03), (-0.42, 0.03), (0.44, 0.03)] {
        g.ball(u, 0.01, r, FOAM, Look::flat().squash(0.6));
    }
    if d.fine {
        // The dove flying through.
        let dove = [
            m(-0.05, 0.44),
            q(-0.02, 0.48, 0.0, 0.43),
            q(0.02, 0.48, 0.05, 0.45),
        ];
        g.curve(
            &dove,
            0.018,
            [250.0, 250.0, 245.0],
            None,
            Look::default().unshaded().min(1.0),
        );
    }
}

fn talos(g: &mut Hand<'_>, d: Detail) {
    g.poly(
        &[
            (-0.4, 0.0),
            (-0.28, 0.08),
            (-0.05, 0.11),
            (0.18, 0.1),
            (0.34, 0.05),
            (0.42, 0.0),
        ],
        ROCK,
        Some(n::UP_LEFT),
        Look::default(),
    );
    g.poly(
        &[(0.18, 0.1), (0.34, 0.05), (0.42, 0.0), (0.08, 0.0)],
        ROCK_DARK,
        Some(n::UP_RIGHT),
        Look::default().no_rim(),
    );
    // A bronze giant, broad in the shoulder, legs apart, a boulder held over his head.
    g.poly(
        &[(-0.19, 0.09), (-0.1, 0.09), (-0.01, 0.31), (-0.09, 0.32)],
        BRONZE,
        Some(n::LEFT),
        Look::default(),
    );
    g.poly(
        &[(0.1, 0.09), (0.19, 0.09), (0.09, 0.32), (0.01, 0.31)],
        BRONZE_DARK,
        Some(n::RIGHT),
        Look::default(),
    );
    g.poly(
        &[(-0.1, 0.3), (0.1, 0.3), (0.19, 0.49), (-0.19, 0.49)],
        BRONZE,
        None,
        Look::default().across((-0.19, 0.4), (0.19, 0.4), &TORSO),
    );
    g.ball(0.0, 0.535, 0.055, BRONZE, Look::default());
    g.line(
        &[(-0.16, 0.47), (-0.23, 0.56), (-0.12, 0.62)],
        0.06,
        BRONZE,
        Some(n::LEFT),
        Look::default().min(1.4),
    );
    g.line(
        &[(0.16, 0.47), (0.23, 0.56), (0.12, 0.62)],
        0.06,
        BRONZE_DARK,
        Some(n::RIGHT),
        Look::default().min(1.4),
    );
    g.ball(0.0, 0.66, 0.135, ROCK, Look::default().squash(0.76));
    if d.fine {
        g.line(
            &[(-0.1, 0.32), (0.1, 0.32)],
            0.014,
            BRONZE_DARK,
            None,
            Look::flat(),
        );
        g.line(
            &[(-0.04, 0.66), (0.06, 0.7)],
            0.01,
            EDGE,
            None,
            Look::raw(0.25),
        );
    }
}

fn sirens(g: &mut Hand<'_>) {
    for (u, r, a) in [(0.0, 0.44, 0.55), (0.08, 0.3, 0.35)] {
        g.ripple(u, -0.05, r, a);
    }
    g.poly(
        &[
            (-0.4, 0.0),
            (-0.3, 0.12),
            (-0.14, 0.2),
            (0.0, 0.19),
            (0.0, 0.0),
        ],
        ROCK,
        Some(n::UP_LEFT),
        Look::default(),
    );
    g.poly(
        &[
            (0.0, 0.0),
            (0.0, 0.19),
            (0.16, 0.21),
            (0.3, 0.12),
            (0.4, 0.0),
        ],
        ROCK_DARK,
        Some(n::UP_RIGHT),
        Look::default(),
    );
    // One siren perched with her wings raised: broad feathered wings, a bird's body, a woman's
    // head.
    let wing = |k: f64| {
        [
            m(0.02 * k, 0.38),
            q(0.2 * k, 0.48, 0.38 * k, 0.68),
            l(0.34 * k, 0.56),
            l(0.4 * k, 0.53),
            l(0.29 * k, 0.47),
            l(0.34 * k, 0.42),
            l(0.22 * k, 0.39),
            l(0.25 * k, 0.33),
            l(0.07 * k, 0.28),
            Z,
        ]
    };
    g.shape(&wing(-1.0), SIREN, Some(n::UP_LEFT), Look::default());
    g.shape(&wing(1.0), SIREN, Some(n::UP_RIGHT), Look::default());
    const BODY_ACROSS: [Normal; 3] = [[-0.7, 0.3, 0.6], [0.0, 0.0, 1.0], [0.7, 0.0, 0.6]];
    let body = [
        m(0.0, 0.43),
        q(0.1, 0.38, 0.08, 0.27),
        q(0.07, 0.2, 0.12, 0.17),
        l(0.02, 0.19),
        q(-0.08, 0.22, -0.07, 0.32),
        q(-0.06, 0.4, 0.0, 0.43),
        Z,
    ];
    g.shape(
        &body,
        SIREN,
        None,
        Look::default().across((-0.08, 0.36), (0.1, 0.28), &BODY_ACROSS),
    );
    let claw = Look::default().unshaded().no_cast();
    g.line(&[(-0.02, 0.22), (-0.03, 0.19)], 0.015, EDGE, None, claw);
    g.line(&[(0.03, 0.22), (0.04, 0.19)], 0.015, EDGE, None, claw);
    let hair = [
        m(-0.055, 0.5),
        q(0.0, 0.57, 0.06, 0.5),
        q(0.085, 0.43, 0.08, 0.36),
        q(0.04, 0.41, 0.025, 0.43),
        q(-0.03, 0.44, -0.055, 0.5),
        Z,
    ];
    g.shape(&hair, HAIR, Some(n::FRONT), Look::default());
    g.ball(-0.005, 0.48, 0.05, IVORY, Look::default());
}

fn dragon(g: &mut Hand<'_>, d: Detail) {
    g.poly(
        &[
            (-0.42, 0.0),
            (-0.3, 0.08),
            (0.0, 0.11),
            (0.26, 0.08),
            (0.42, 0.0),
        ],
        ROCK,
        Some(n::UP_LEFT),
        Look::default(),
    );
    // The body: a coil on the rock, then an S rising to the head, thinner toward the tail.
    type P = (f64, f64);
    let bezier = |t: f64, p0: P, p1: P, p2: P, p3: P| {
        let m = 1.0 - t;
        (
            m * m * m * p0.0 + 3.0 * m * m * t * p1.0 + 3.0 * m * t * t * p2.0 + t * t * t * p3.0,
            m * m * m * p0.1 + 3.0 * m * m * t * p1.1 + 3.0 * m * t * t * p2.1 + t * t * t * p3.1,
        )
    };
    let segments: [[P; 4]; 4] = [
        [(-0.38, 0.09), (-0.2, 0.03), (0.2, 0.04), (0.32, 0.13)],
        [(0.32, 0.13), (0.42, 0.24), (0.2, 0.32), (0.02, 0.3)],
        [(0.02, 0.3), (-0.16, 0.28), (-0.2, 0.4), (-0.08, 0.47)],
        [(-0.08, 0.47), (0.02, 0.52), (-0.02, 0.58), (-0.12, 0.6)],
    ];
    let mut points: Vec<P> = Vec::new();
    for s in &segments {
        for i in 0..12 {
            points.push(bezier(f64::from(i) / 12.0, s[0], s[1], s[2], s[3]));
        }
    }
    points.push(segments[3][3]);
    let last = (points.len() - 1) as f64;
    let width = |i: usize| 0.03 + 0.065 * (PI * ((i as f64 / last) * 1.15).min(1.0)).sin();
    for pass in 0..2 {
        for i in 0..points.len() - 1 {
            let (p, q2, w) = (points[i], points[i + 1], width(i));
            if pass == 0 {
                g.line(
                    &[p, q2],
                    w,
                    SERPENT,
                    Some([0.0, 0.0, 1.0]),
                    Look::default().min(1.6),
                );
            } else {
                let belly = [
                    (p.0 - w * 0.18, p.1 + w * 0.22),
                    (q2.0 - w * 0.18, q2.1 + w * 0.22),
                ];
                g.line(
                    &belly,
                    w * 0.38,
                    BELLY,
                    Some([-0.5, 0.5, 0.7]),
                    Look::flat().min(0.6),
                );
            }
        }
    }
    // The head, facing left, jaw open.
    let head = [
        m(-0.1, 0.63),
        q(-0.2, 0.66, -0.27, 0.62),
        l(-0.2, 0.595),
        l(-0.27, 0.57),
        q(-0.18, 0.54, -0.1, 0.57),
        Z,
    ];
    g.shape(&head, SERPENT, Some(n::LIT_LEFT), Look::default());
    if d.fine {
        let mut i = 30;
        while i < points.len() - 3 {
            let p = points[i];
            let spine = [
                (p.0 + 0.025, p.1 + 0.04),
                (p.0 + 0.05, p.1 + 0.09),
                (p.0 + 0.07, p.1 + 0.03),
            ];
            g.poly(
                &spine,
                LEAF_DARK,
                Some(n::UP_RIGHT),
                Look::default().no_cast(),
            );
            i += 4;
        }
        g.ellipse(-0.17, 0.615, 0.014, 0.011, FLAME, None, Look::raw(1.0));
    }
}

fn harpies(g: &mut Hand<'_>, d: Detail) {
    g.poly(
        &[
            (-0.42, 0.0),
            (-0.3, 0.16),
            (-0.16, 0.32),
            (-0.06, 0.24),
            (-0.06, 0.0),
        ],
        ROCK,
        Some(n::UP_LEFT),
        Look::default(),
    );
    g.poly(
        &[
            (-0.06, 0.0),
            (-0.06, 0.24),
            (0.06, 0.3),
            (0.2, 0.16),
            (0.42, 0.0),
        ],
        ROCK_DARK,
        Some(n::UP_RIGHT),
        Look::default(),
    );
    // Wings out in a wide M, a small body, a pale face, claws below.
    let mut harpy = |u: f64, v: f64, k: f64| {
        let left = [
            m(u, v),
            q(u - 0.1 * k, v + 0.14 * k, u - 0.24 * k, v + 0.1 * k),
            q(u - 0.13 * k, v + 0.06 * k, u - 0.03 * k, v - 0.04 * k),
            Z,
        ];
        g.shape(&left, HARPY, Some(n::UP_LEFT), Look::default());
        let right = [
            m(u, v),
            q(u + 0.1 * k, v + 0.14 * k, u + 0.24 * k, v + 0.12 * k),
            q(u + 0.13 * k, v + 0.06 * k, u + 0.03 * k, v - 0.04 * k),
            Z,
        ];
        g.shape(&right, HARPY, Some(n::SHADE_RIGHT), Look::default());
        g.ellipse(
            u,
            v - 0.02 * k,
            0.035 * k,
            0.05 * k,
            HARPY,
            Some(n::FRONT),
            Look::default(),
        );
        g.ball(u, v + 0.04 * k, 0.028 * k, IVORY, Look::default());
        if d.fine {
            let claw = Look::default().unshaded();
            g.line(
                &[(u - 0.015 * k, v - 0.07 * k), (u - 0.03 * k, v - 0.11 * k)],
                0.01 * k,
                EDGE,
                None,
                claw,
            );
            g.line(
                &[(u + 0.015 * k, v - 0.07 * k), (u + 0.03 * k, v - 0.11 * k)],
                0.01 * k,
                EDGE,
                None,
                claw,
            );
        }
    };
    harpy(-0.06, 0.5, 1.15);
    harpy(0.25, 0.4, 0.85);
}

fn dodona(g: &mut Hand<'_>, d: Detail) {
    g.poly(
        &[
            (-0.09, 0.0),
            (0.09, 0.0),
            (0.05, 0.08),
            (0.05, 0.27),
            (-0.05, 0.27),
            (-0.05, 0.08),
        ],
        WOOD,
        None,
        Look::default().across((-0.09, 0.1), (0.09, 0.1), &SIDES),
    );
    if d.fine {
        g.line(
            &[(0.0, 0.24), (-0.14, 0.34)],
            0.025,
            WOOD,
            Some(n::LEFT),
            Look::default(),
        );
        g.line(
            &[(0.0, 0.24), (0.15, 0.33)],
            0.025,
            WOOD_DARK,
            Some(n::RIGHT),
            Look::default(),
        );
    }
    for (u, v, r, c) in [
        (0.0, 0.31, 0.14, LEAF_DARK),
        (-0.22, 0.36, 0.14, LEAF_DARK),
        (0.21, 0.36, 0.14, LEAF_DARK),
        (-0.12, 0.5, 0.15, LEAF),
        (0.12, 0.5, 0.15, LEAF),
        (0.0, 0.58, 0.13, LEAF),
    ] {
        g.ball(u, v, r, c, Look::default());
    }
    if d.fine {
        // A white dove of the oracle on a low bough.
        g.ellipse(
            0.27,
            0.24,
            0.035,
            0.022,
            [250.0, 248.0, 240.0],
            None,
            Look::default().unshaded(),
        );
        g.ball(
            0.3,
            0.26,
            0.016,
            [250.0, 248.0, 240.0],
            Look::default().no_cast(),
        );
    }
}

fn pyramid(g: &mut Hand<'_>, d: Detail) {
    let w = 0.92;
    let dp = w * 0.13;
    let (left, front, right, apex) = ((-w / 2.0, 0.0), (0.04, -dp), (w / 2.0, 0.02), (-0.02, 0.6));
    g.poly(
        &[left, front, apex],
        STONE,
        Some(n::UP_LEFT),
        Look::default(),
    );
    g.poly(
        &[front, right, apex],
        STONE,
        Some(n::UP_RIGHT),
        Look::default(),
    );
    let toward = |p: (f64, f64), t: f64| (p.0 + (apex.0 - p.0) * t, p.1 + (apex.1 - p.1) * t);
    if d.fine {
        // Courses of stone, a line every few.
        let n = if d.finest { 9 } else { 6 };
        for i in 1..n {
            let t = f64::from(i) / f64::from(n);
            let course = [toward(left, t), toward(front, t), toward(right, t)];
            g.line(&course, 0.008, EDGE, None, Look::raw(0.16).min(0.6));
        }
    }
    // The capstone, gilded.
    let t = 0.86;
    let (cl, cf, cr) = (toward(left, t), toward(front, t), toward(right, t));
    g.poly(&[cl, cf, apex], GOLD, Some(n::UP_LEFT), Look::flat());
    g.poly(&[cf, cr, apex], GOLD, Some(n::UP_RIGHT), Look::flat());
}

fn gardens(g: &mut Hand<'_>, d: Detail) {
    let tiers = [(0.92, 0.0, 0.15), (0.64, 0.15, 0.14), (0.38, 0.29, 0.13)];
    for (i, (w, v0, h)) in tiers.into_iter().enumerate() {
        let b = g.cuboid(0.0, v0, w, h, BRICK, Some(LEAF_DARK), 0.16, Look::default());
        // Greenery along the front edges, and vines hanging down the faces.
        let k = if d.fine { 5 } else { 3 };
        for j in 0..=k {
            let t = f64::from(j) / f64::from(k);
            let pl = (
                b.left.0 + (b.front.0 - b.left.0) * t,
                b.left.1 + (b.front.1 - b.left.1) * t + h,
            );
            let pr = (
                b.front.0 + (b.right.0 - b.front.0) * t,
                b.front.1 + (b.right.1 - b.front.1) * t + h,
            );
            let bigger = if i == 0 { 0.005 } else { 0.0 };
            g.ball(
                pl.0,
                pl.1 + 0.02,
                0.042 + bigger,
                LEAF,
                Look::default().no_cast(),
            );
            g.ball(
                pr.0,
                pr.1 + 0.02,
                0.042,
                LEAF_DARK,
                Look::default().no_cast(),
            );
            if d.fine && j % 2 == 1 {
                g.line(
                    &[pl, (pl.0, pl.1 - h * 0.6)],
                    0.014,
                    LEAF,
                    Some(n::LEFT),
                    Look::flat(),
                );
                g.line(
                    &[pr, (pr.0, pr.1 - h * 0.5)],
                    0.014,
                    LEAF_DARK,
                    Some(n::RIGHT),
                    Look::flat(),
                );
            }
        }
    }
    g.ellipse(
        -0.07,
        0.5,
        0.045,
        0.1,
        LEAF_DARK,
        Some(n::LIT_LEFT),
        Look::default(),
    );
    g.ellipse(
        0.08,
        0.48,
        0.04,
        0.085,
        LEAF_DARK,
        Some(n::SHADE_RIGHT),
        Look::default(),
    );
    if d.fine {
        g.line(
            &[(0.16, 0.42), (0.2, 0.3), (0.28, 0.16), (0.36, 0.02)],
            0.014,
            SKY,
            None,
            Look::raw(0.85),
        );
    }
}

fn colossus(g: &mut Hand<'_>, d: Detail) {
    g.ripple(0.0, -0.04, 0.16, 0.6);
    g.cuboid(
        -0.28,
        0.0,
        0.2,
        0.13,
        MARBLE,
        Some(MARBLE),
        0.16,
        Look::default(),
    );
    g.cuboid(
        0.28,
        0.0,
        0.2,
        0.13,
        MARBLE,
        Some(MARBLE),
        0.16,
        Look::default(),
    );
    // Legs astride the harbour mouth, torso, head with its crown of rays, a torch held high.
    g.poly(
        &[(-0.34, 0.13), (-0.23, 0.13), (0.0, 0.38), (-0.1, 0.4)],
        BRONZE,
        Some(n::LEFT),
        Look::default(),
    );
    g.poly(
        &[(0.23, 0.13), (0.34, 0.13), (0.1, 0.4), (0.0, 0.38)],
        BRONZE_DARK,
        Some(n::RIGHT),
        Look::default(),
    );
    g.poly(
        &[(-0.1, 0.37), (0.1, 0.37), (0.115, 0.54), (-0.115, 0.54)],
        BRONZE,
        None,
        Look::default().across((-0.115, 0.45), (0.115, 0.45), &TORSO),
    );
    g.ball(0.0, 0.585, 0.05, BRONZE, Look::default());
    g.line(
        &[(0.1, 0.53), (0.16, 0.61), (0.19, 0.68)],
        0.045,
        BRONZE_DARK,
        Some(n::RIGHT),
        Look::default().min(1.2),
    );
    g.line(
        &[(-0.1, 0.53), (-0.16, 0.45), (-0.17, 0.37)],
        0.042,
        BRONZE,
        Some(n::LEFT),
        Look::default().min(1.2),
    );
    if d.fine {
        for i in 0..7 {
            let a = PI * (0.12 + 0.76 * f64::from(i) / 6.0);
            let ray = [
                (a.cos() * 0.058, 0.59 + a.sin() * 0.058),
                (a.cos() * 0.095, 0.59 + a.sin() * 0.095),
            ];
            g.line(
                &ray,
                0.012,
                GOLD,
                None,
                Look::default().unshaded().no_cast(),
            );
        }
    }
    let torch = [
        m(0.195, 0.68),
        q(0.155, 0.72, 0.2, 0.79),
        q(0.235, 0.72, 0.195, 0.68),
        Z,
    ];
    g.shape(&torch, FLAME, None, Look::default().unshaded().no_cast());
}

fn pharos(g: &mut Hand<'_>, d: Detail) {
    g.cuboid(
        0.0,
        0.0,
        0.52,
        0.05,
        STONE,
        Some(STONE),
        0.16,
        Look::default(),
    );
    g.cuboid(
        0.0,
        0.05,
        0.3,
        0.32,
        STONE,
        Some(STONE),
        0.16,
        Look::default(),
    );
    g.cuboid(
        0.0,
        0.37,
        0.2,
        0.15,
        STONE,
        Some(STONE),
        0.16,
        Look::default(),
    );
    g.cylinder(0.0, 0.52, 0.6, 0.058, STONE, Some(STONE), Look::default());
    if d.fine {
        for v in [0.14, 0.24, 0.32] {
            let window = Look::raw(0.55);
            g.poly(
                &[
                    (-0.085, v - 0.012),
                    (-0.065, v - 0.02),
                    (-0.065, v + 0.015),
                    (-0.085, v + 0.023),
                ],
                EDGE,
                None,
                window,
            );
            g.poly(
                &[
                    (0.065, v - 0.02),
                    (0.085, v - 0.012),
                    (0.085, v + 0.023),
                    (0.065, v + 0.015),
                ],
                EDGE,
                None,
                window,
            );
        }
    }
    let fire = [
        m(0.0, 0.6),
        q(-0.05, 0.65, 0.0, 0.73),
        q(0.05, 0.65, 0.0, 0.6),
        Z,
    ];
    g.shape(&fire, FLAME, None, Look::default().unshaded().no_cast());
    g.glow(0.0, 0.66, 0.2, FLAME, 0.5);
}

fn artemis(g: &mut Hand<'_>, d: Detail) {
    let front = |g: &mut Hand<'_>, u0: f64, u1: f64, v0: f64, v1: f64, rgb: Rgb, normal: Normal| {
        g.poly(
            &[(u0, v0), (u1, v0), (u1, v1), (u0, v1)],
            rgb,
            Some(normal),
            Look::default(),
        );
    };
    front(g, -0.48, 0.48, 0.0, 0.035, MARBLE, [0.0, 0.4, 1.0]);
    front(g, -0.45, 0.45, 0.035, 0.07, MARBLE, [0.0, 0.4, 1.0]);
    front(g, -0.42, 0.42, 0.07, 0.1, MARBLE, [0.0, 0.4, 1.0]);
    front(g, -0.39, 0.39, 0.1, 0.36, EBONY, [0.0, 0.0, 1.0]);
    let n_columns = if d.fine { 8 } else { 5 };
    let r = if d.fine { 0.026 } else { 0.032 };
    for i in 0..n_columns {
        let u = -0.35 + 0.7 * f64::from(i) / f64::from(n_columns - 1);
        g.cylinder(u, 0.1, 0.355, r, MARBLE, None, Look::default().no_rim());
    }
    front(g, -0.43, 0.43, 0.355, 0.41, MARBLE, [0.0, 0.1, 1.0]);
    g.poly(
        &[(-0.45, 0.41), (0.45, 0.41), (0.0, 0.54)],
        MARBLE,
        Some([0.0, 0.25, 1.0]),
        Look::default(),
    );
    g.line(
        &[(-0.46, 0.405), (0.0, 0.545), (0.46, 0.405)],
        0.016,
        MARBLE,
        Some([0.0, 1.0, 0.3]),
        Look::default().no_cast().min(0.8),
    );
    if d.fine {
        g.poly(
            &[(-0.3, 0.43), (0.3, 0.43), (0.0, 0.515)],
            EBONY,
            None,
            Look::raw(0.25),
        );
    }
}

fn zeus(g: &mut Hand<'_>, d: Detail) {
    g.cuboid(
        0.0,
        0.0,
        0.6,
        0.1,
        EBONY,
        Some(THRONE_TOP),
        0.16,
        Look::default(),
    );
    // The throne's tall back, dark, edged in gold.
    let back = [
        m(-0.19, 0.12),
        l(-0.19, 0.52),
        q(-0.19, 0.63, 0.0, 0.65),
        q(0.19, 0.63, 0.19, 0.52),
        l(0.19, 0.12),
    ];
    let mut closed = back.to_vec();
    closed.push(Z);
    g.shape(&closed, EBONY, Some(n::FRONT), Look::default());
    if d.fine {
        g.curve(&back, 0.014, GOLD, None, Look::raw(1.0));
    }
    // Seated, facing you: knees under a golden robe, an ivory chest, the sceptre and its eagle
    // beside.
    const ROBE: [Normal; 3] = [[-0.75, 0.3, 0.6], [-0.1, 0.1, 1.0], [0.7, 0.0, 0.7]];
    let robe = [
        m(-0.17, 0.1),
        l(0.17, 0.1),
        l(0.16, 0.28),
        q(0.0, 0.31, -0.16, 0.28),
        Z,
    ];
    g.shape(
        &robe,
        GOLD,
        None,
        Look::default().across((-0.17, 0.2), (0.17, 0.2), &ROBE),
    );
    const CHEST: [Normal; 3] = [[-0.75, 0.0, 0.66], [-0.1, 0.0, 1.0], [0.7, 0.0, 0.7]];
    let chest = [
        m(-0.095, 0.28),
        l(0.095, 0.28),
        l(0.085, 0.44),
        q(0.0, 0.47, -0.085, 0.44),
        Z,
    ];
    g.shape(
        &chest,
        IVORY,
        None,
        Look::default().across((-0.1, 0.36), (0.1, 0.36), &CHEST),
    );
    if d.fine {
        g.poly(
            &[(-0.095, 0.44), (-0.03, 0.45), (0.095, 0.29), (0.04, 0.28)],
            GOLD,
            Some(n::FRONT),
            Look::flat(),
        );
    }
    g.line(
        &[(0.085, 0.42), (0.17, 0.34), (0.21, 0.35)],
        0.04,
        IVORY,
        Some(n::SHADE_RIGHT),
        Look::default().min(1.1),
    );
    g.line(
        &[(-0.085, 0.42), (-0.17, 0.45), (-0.22, 0.48)],
        0.04,
        IVORY,
        Some(n::LIT_LEFT),
        Look::default().min(1.1),
    );
    g.ball(0.0, 0.505, 0.058, IVORY, Look::default());
    if d.fine {
        let beard = [
            m(-0.04, 0.49),
            q(0.0, 0.42, 0.04, 0.49),
            q(0.0, 0.47, -0.04, 0.49),
            Z,
        ];
        g.shape(&beard, HAIR, None, Look::raw(0.55));
        g.ball(0.225, 0.38, 0.025, GOLD, Look::default().no_cast());
    }
    g.line(
        &[(-0.235, 0.1), (-0.235, 0.7)],
        0.022,
        GOLD,
        Some(n::LIT_LEFT),
        Look::default().min(1.1),
    );
    g.ball(-0.235, 0.72, 0.034, GOLD, Look::default());
}

fn mausoleum(g: &mut Hand<'_>, d: Detail) {
    g.cuboid(
        0.0,
        0.0,
        0.7,
        0.05,
        MARBLE,
        Some(MARBLE),
        0.16,
        Look::default(),
    );
    g.cuboid(
        0.0,
        0.05,
        0.58,
        0.2,
        STONE,
        Some(STONE),
        0.16,
        Look::default(),
    );
    let c = g.cuboid(0.0, 0.25, 0.5, 0.13, MARBLE, None, 0.16, Look::default());
    if d.fine {
        // The colonnade: dark gaps between columns on both faces.
        let k = if d.finest { 5 } else { 3 };
        for i in 1..=k {
            for (a, b) in [(c.left, c.front), (c.front, c.right)] {
                let t = (f64::from(i) - 0.5) / (f64::from(k) + 0.5) + 0.03;
                let p = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
                let gap = Look::raw(0.75).cap(Cap::Butt);
                g.line(
                    &[(p.0, p.1 + 0.02), (p.0, p.1 + 0.11)],
                    0.022,
                    EBONY,
                    None,
                    gap,
                );
            }
        }
    }
    // The stepped roof and the chariot on top.
    let w = 0.52;
    let dp = w * 0.16;
    let v0 = 0.38;
    let (left, front, right, apex) = ((-w / 2.0, v0), (0.0, v0 - dp), (w / 2.0, v0), (0.0, 0.58));
    g.poly(
        &[left, front, apex],
        MARBLE,
        Some(n::UP_LEFT),
        Look::default(),
    );
    g.poly(
        &[front, right, apex],
        MARBLE,
        Some(n::UP_RIGHT),
        Look::default(),
    );
    if d.fine {
        for i in 1..4 {
            let t = f64::from(i) / 4.0;
            let step = [
                (left.0 * (1.0 - t), v0 + (apex.1 - v0) * t),
                (0.0, front.1 + (apex.1 - front.1) * t),
                (right.0 * (1.0 - t), v0 + (apex.1 - v0) * t),
            ];
            g.line(&step, 0.008, EDGE, None, Look::raw(0.2).min(0.6));
        }
    }
    g.cuboid(
        0.0,
        0.57,
        0.12,
        0.05,
        BRONZE_DARK,
        Some(BRONZE_DARK),
        0.25,
        Look::default(),
    );
    if d.fine {
        g.line(
            &[(-0.035, 0.62), (-0.035, 0.665)],
            0.018,
            BRONZE_DARK,
            Some(n::LEFT),
            Look::default(),
        );
        g.line(
            &[(0.035, 0.62), (0.035, 0.67)],
            0.018,
            BRONZE_DARK,
            Some(n::RIGHT),
            Look::default(),
        );
    }
}

fn atlantis(g: &mut Hand<'_>) {
    // Plato's city, drowned where the behavior's land sank: two rings of land and three of
    // water round the temple's island, and the one canal cut through them to the sea. Drawn
    // nearly round, as the map's ground is seen nearly from above, so the rings read as rings at
    // the terminal's size. The broken temple still breaks the surface; it holds still.
    let ry = 0.9;
    let stone: Rgb = [158.0, 196.0, 188.0];
    let wall: Rgb = [96.0, 134.0, 132.0];
    let canal: Rgb = [58.0, 140.0, 160.0];
    let arc = |r: f64, a0: f64, a1: f64| -> Vec<Step> {
        (1..=40)
            .map(|i| {
                let a = a0 + (a1 - a0) * f64::from(i) / 40.0;
                l(a.cos() * r, a.sin() * r * ry)
            })
            .collect()
    };
    let ring = |g: &mut Hand<'_>, r0: f64, r1: f64, rgb: Rgb, alpha: f64| {
        let mut steps = vec![m(r1, 0.0)];
        steps.extend(arc(r1, 0.0, PI * 2.0));
        steps.push(m(r0, 0.0));
        steps.extend(arc(r0, PI * 2.0, 0.0));
        steps.push(Z);
        g.shape(&steps, rgb, None, Look::raw(alpha));
    };
    let edge = |g: &mut Hand<'_>, r: f64, alpha: f64| {
        let points: Vec<(f64, f64)> = (0..=48)
            .map(|i| {
                let t = f64::from(i) / 48.0 * PI * 2.0;
                (t.cos() * r, t.sin() * r * ry)
            })
            .collect();
        g.line(&points, 0.012, wall, None, Look::raw(alpha).min(1.0));
    };
    // The deep it sank into.
    g.ellipse(
        0.0,
        0.0,
        0.58,
        0.58 * ry,
        [14.0, 40.0, 58.0],
        None,
        Look::raw(0.4),
    );
    ring(g, 0.44, 0.53, stone, 0.62);
    ring(g, 0.35, 0.44, canal, 0.55);
    ring(g, 0.25, 0.35, stone, 0.68);
    ring(g, 0.17, 0.25, canal, 0.55);
    // The drowned town on each ring of land: small walls and roofs, broken.
    for (r0, r1, count, seed) in [(0.445, 0.525, 22, 3.0), (0.255, 0.345, 14, 7.0)] {
        for i in 0..count {
            let fi = f64::from(i);
            let a = (fi + 0.5 * (fi * 12.9898 + seed).sin()) / f64::from(count) * PI * 2.0;
            let rr = (r0 + r1) / 2.0 + (r1 - r0) * 0.25 * (fi * 7.31 + seed).sin();
            // The canal cuts through here.
            if (a - PI / 2.0).abs() < 0.16 {
                continue;
            }
            let (u, v) = (a.cos() * rr, -a.sin() * rr * ry);
            let size = 0.016 + 0.01 * ((i * 37 + seed as i32) % 3) as f64;
            let house = [
                (u - size, v - size * 0.6),
                (u + size, v - size * 0.6),
                (u + size, v + size * 0.6),
                (u - size, v + size * 0.6),
            ];
            g.poly(&house, MARBLE, None, Look::raw(0.5));
        }
    }
    // The temple's island, the walls along each shore, and the canal to the sea.
    g.ellipse(0.0, 0.0, 0.17, 0.17 * ry, stone, None, Look::raw(0.9));
    for (r, a) in [
        (0.53, 0.6),
        (0.44, 0.5),
        (0.35, 0.6),
        (0.25, 0.5),
        (0.17, 0.6),
    ] {
        edge(g, r, a);
    }
    let cut = [
        (-0.032, -0.15 * ry),
        (0.032, -0.15 * ry),
        (0.032, -0.56 * ry),
        (-0.032, -0.56 * ry),
    ];
    g.poly(&cut, canal, None, Look::raw(0.85));
    // The temple's broken columns, standing above the water on its island.
    for (u, h) in [
        (-0.1, 0.2),
        (-0.05, 0.3),
        (0.0, 0.13),
        (0.05, 0.33),
        (0.1, 0.17),
    ] {
        g.cylinder(
            u,
            0.02,
            0.02 + h,
            0.022,
            MARBLE,
            Some(MARBLE),
            Look::default(),
        );
    }
    g.poly(
        &[
            (-0.075, 0.32),
            (0.075, 0.355),
            (0.075, 0.385),
            (-0.075, 0.35),
        ],
        MARBLE,
        Some(n::TOP),
        Look::default(),
    );
    for (u, r, a) in [(0.0, 0.15, 0.7), (0.0, 0.21, 0.4)] {
        g.ripple(u, 0.0, r, a);
    }
    // The sea over it all, and the light from below.
    g.ellipse(0.0, 0.0, 0.53, 0.53 * ry, WATER, None, Look::raw(0.16));
    g.glow(0.0, 0.05, 0.5, canal, 0.22);
}
