//! The colours Tokens per Day draws its lines in. A CLI's lines take its own hues: its own
//! colour from the theme, a step deeper, then a step lighter with a little hue, told apart by
//! lightness so colour-blind readers can tell them apart too.
//!
//! Distances are OKLab distances times 100, measured under normal vision and under
//! protanopia, deuteranopia and tritanopia (Machado, Oliveira and Fernandes 2009, at full
//! severity, in linear RGB). Under [`APART`] two thin lines read as one colour.

use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};

use crate::base::kit::Kit;
use crate::base::theme::{Brightness, ColorLevel, Role, downgrade};

/// The steps of a CLI's hues: the CLI's own colour, deeper, lighter.
pub const STEPS: usize = 3;

/// The distance any two lines of one chart keep, for every reader.
pub const APART: f64 = 8.0;

/// A CLI's hues on a dark and on a light terminal. The first step draws in the theme's
/// colour for the role; the table holds its default, which is what the distances measure.
struct Hue {
    role: Role,
    dark: [u32; STEPS],
    light: [u32; STEPS],
}

const HUES: [Hue; 5] = [
    // Deeper than Anthropic's logo terracotta (#d97757), which sits only 10.4 from the salmon.
    Hue {
        role: Role::CliClaude,
        dark: [0xf7a07f, 0xc15f3c, 0xfed397],
        light: [0x9e320c, 0x701200, 0xc85e2c],
    },
    Hue {
        role: Role::CliCodex,
        dark: [0x6a9ee6, 0x5575bf, 0x8ac9fe],
        light: [0x375cc0, 0x063a98, 0x6880ec],
    },
    Hue {
        role: Role::CliAgy,
        dark: [0x6cd1cf, 0x2f9b99, 0xacf3f7],
        light: [0x1f7f85, 0x0a5a5c, 0x469da8],
    },
    // OpenCode runs many makers' models; its lines keep its own hues, since each maker's
    // colour would put Mistral's orange beside Claude Code's.
    Hue {
        role: Role::CliOpenCode,
        dark: [0xc27cb4, 0x9c5c95, 0xe7a5d1],
        light: [0x7f3e76, 0x581d57, 0xac6297],
    },
    // Any other CLI or model server.
    Hue {
        role: Role::CatOther,
        dark: [0x8a8fa8, 0x6d7189, 0xb3b8cd],
        light: [0x6b6f80, 0x494e5d, 0x9092a4],
    },
];

const COLOURS: usize = HUES.len() * STEPS;

/// How each kind of colour blindness sees linear RGB.
const DEFICIENCIES: [[[f64; 3]; 3]; 3] = [
    [
        [0.152286, 1.052583, -0.204868],
        [0.114503, 0.786281, 0.099216],
        [-0.003882, -0.048116, 1.051998],
    ],
    [
        [0.367322, 0.860646, -0.227968],
        [0.280085, 0.672501, 0.047413],
        [-0.011820, 0.042940, 0.968881],
    ],
    [
        [1.255528, -0.076749, -0.178779],
        [-0.078411, 0.930809, 0.147602],
        [0.004733, 0.691367, 0.303900],
    ],
];

/// Each charted line's step among its CLI's hues, for `lines` given in rank order as the role of
/// each line's CLI and its model's place among that CLI's models.
///
/// A model among its CLI's top three takes its place's step, so a line keeps its colour
/// whatever the dates; one further down takes the first step its CLI's other lines leave
/// free. Where two lines would come under [`APART`] for some reader, a line moves to another
/// step of its CLI's hues: of every way to give each line a step, the one that keeps every pair
/// apart and moves the lower-ranked lines first wins.
pub fn chart_steps(brightness: Brightness, lines: &[(Role, usize)]) -> Vec<usize> {
    let hues: Vec<usize> = lines.iter().map(|(role, _)| hue_of(*role)).collect();
    let mut wanted: Vec<usize> = Vec::with_capacity(lines.len());
    for (at, (_, place)) in lines.iter().enumerate() {
        let held = |step: usize| {
            (0..at).any(|k| hues[k] == hues[at] && wanted[k] == step)
                || (0..lines.len()).any(|k| hues[k] == hues[at] && lines[k].1 == step)
        };
        let step = if *place < STEPS {
            *place
        } else {
            (0..STEPS).find(|step| !held(*step)).unwrap_or(STEPS - 1)
        };
        wanted.push(step);
    }
    // Five lines are 243 ways; the chart draws three.
    if lines.len() > 5 {
        return wanted;
    }
    let mut best: Option<(bool, Vec<bool>, f64, Vec<usize>)> = None;
    for code in 0..STEPS.pow(lines.len() as u32) {
        let steps: Vec<usize> = (0..lines.len())
            .map(|i| code / STEPS.pow((lines.len() - 1 - i) as u32) % STEPS)
            .collect();
        let pairs = || (0..lines.len()).flat_map(|i| (i + 1..lines.len()).map(move |j| (i, j)));
        if pairs().any(|(i, j)| hues[i] == hues[j] && steps[i] == steps[j]) {
            continue;
        }
        let colour = |i: usize| hues[i] * STEPS + steps[i];
        let low = pairs()
            .map(|(i, j)| closest(brightness, colour(i), colour(j)))
            .fold(f64::INFINITY, f64::min);
        let kept: Vec<bool> = steps.iter().zip(&wanted).map(|(s, w)| s == w).collect();
        let better = best.as_ref().is_none_or(|(apart, was_kept, was_low, _)| {
            (low >= APART, &kept)
                .cmp(&(*apart, was_kept))
                .then(low.total_cmp(was_low))
                .is_gt()
        });
        if better {
            best = Some((low >= APART, kept, low, steps));
        }
    }
    best.map_or(wanted, |(.., steps)| steps)
}

/// The colour of `step` among the hues of the CLI drawn as `role`, fitted to the terminal.
pub fn step_style(kit: &Kit<'_>, role: Role, step: usize) -> Style {
    if step == 0 {
        return kit.fg(role);
    }
    if kit.theme.level == ColorLevel::Ansi16 {
        return sixteen_colour_step(kit.fg(role), step);
    }
    let hue = &HUES[hue_of(role)];
    let style = Style::new().fg(rgb(hex(hue, kit.theme.brightness, step)));
    downgrade(role, style, kit.theme.level)
}

/// Sixteen colours give each CLI one named colour, and the nearest named colour to a deeper
/// blue is that same blue, so the deeper step dims the CLI's colour and the lighter one takes
/// its bright variant.
fn sixteen_colour_step(own: Style, step: usize) -> Style {
    if step == 1 {
        return own.add_modifier(Modifier::DIM);
    }
    let bright = match own.fg {
        Some(Color::Red) => Color::LightRed,
        Some(Color::Green) => Color::LightGreen,
        Some(Color::Yellow) => Color::LightYellow,
        Some(Color::Blue) => Color::LightBlue,
        Some(Color::Magenta) => Color::LightMagenta,
        Some(Color::Cyan) => Color::LightCyan,
        Some(Color::DarkGray) => Color::Gray,
        _ => Color::White,
    };
    own.fg(bright)
}

fn hue_of(role: Role) -> usize {
    HUES.iter()
        .position(|hue| hue.role == role)
        .unwrap_or(HUES.len() - 1)
}

fn hex(hue: &Hue, brightness: Brightness, step: usize) -> u32 {
    match brightness {
        Brightness::Dark => hue.dark[step.min(STEPS - 1)],
        Brightness::Light => hue.light[step.min(STEPS - 1)],
    }
}

fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// The smallest distance between two of the table's colours, by their index, for any reader.
fn closest(brightness: Brightness, a: usize, b: usize) -> f64 {
    static DARK: OnceLock<Distances> = OnceLock::new();
    static LIGHT: OnceLock<Distances> = OnceLock::new();
    let table = match brightness {
        Brightness::Dark => DARK.get_or_init(|| distances(Brightness::Dark)),
        Brightness::Light => LIGHT.get_or_init(|| distances(Brightness::Light)),
    };
    table[a][b]
}

type Distances = [[f64; COLOURS]; COLOURS];

fn distances(brightness: Brightness) -> Distances {
    let colour = |at: usize| hex(&HUES[at / STEPS], brightness, at % STEPS);
    let mut table = [[0.0; COLOURS]; COLOURS];
    for (a, row) in table.iter_mut().enumerate() {
        for (b, cell) in row.iter_mut().enumerate() {
            *cell = least_apart(colour(a), colour(b));
        }
    }
    table
}

fn least_apart(a: u32, b: u32) -> f64 {
    let normal = distance(oklab(linear(a)), oklab(linear(b)));
    DEFICIENCIES
        .iter()
        .map(|seen| {
            distance(
                oklab(through(seen, linear(a))),
                oklab(through(seen, linear(b))),
            )
        })
        .fold(normal, f64::min)
}

fn linear(hex: u32) -> [f64; 3] {
    [16, 8, 0].map(|shift| {
        let c = f64::from((hex >> shift) as u8) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    })
}

fn through(seen: &[[f64; 3]; 3], rgb: [f64; 3]) -> [f64; 3] {
    seen.map(|row| {
        let sum: f64 = row.iter().zip(rgb).map(|(k, c)| k * c).sum();
        sum.clamp(0.0, 1.0)
    })
}

fn oklab([r, g, b]: [f64; 3]) -> [f64; 3] {
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    100.0
        * a.iter()
            .zip(b)
            .map(|(x, y)| (x - y).powi(2))
            .sum::<f64>()
            .sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRIGHTNESSES: [Brightness; 2] = [Brightness::Dark, Brightness::Light];

    fn contrast(a: u32, b: u32) -> f64 {
        let luminance = |hex: u32| {
            let [r, g, b] = linear(hex);
            0.2126 * r + 0.7152 * g + 0.0722 * b
        };
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn every_step_reads_on_its_background_and_apart_from_the_rest_of_its_cli() {
        for (brightness, background) in
            [(Brightness::Dark, 0x222436), (Brightness::Light, 0xffffff)]
        {
            for hue in &HUES {
                for step in 0..STEPS {
                    let colour = hex(hue, brightness, step);
                    assert!(contrast(colour, background) >= 3.0, "{colour:06x}");
                    for other in step + 1..STEPS {
                        let apart = least_apart(colour, hex(hue, brightness, other));
                        assert!(
                            apart >= APART,
                            "{colour:06x} is {apart:.1} from step {other}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn every_chart_of_two_or_three_lines_keeps_its_lines_apart() {
        let items: Vec<(Role, usize)> = HUES
            .iter()
            .flat_map(|hue| (0..5).map(|place| (hue.role, place)))
            .collect();
        for brightness in BRIGHTNESSES {
            let mut charts = 0;
            for (a, first) in items.iter().enumerate() {
                for (b, second) in items.iter().enumerate().filter(|(b, _)| *b != a) {
                    let mut check = |lines: &[(Role, usize)]| {
                        let steps = chart_steps(brightness, lines);
                        let colour =
                            |i: usize| hex(&HUES[hue_of(lines[i].0)], brightness, steps[i]);
                        for i in 0..lines.len() {
                            for j in i + 1..lines.len() {
                                let apart = least_apart(colour(i), colour(j));
                                assert!(apart >= APART, "{lines:?} as {steps:?}: {apart:.1}");
                            }
                        }
                        charts += 1;
                    };
                    check(&[*first, *second]);
                    for third in items.iter().enumerate().filter(|(c, _)| *c != a && *c != b) {
                        check(&[*first, *second, *third.1]);
                    }
                }
            }
            assert_eq!(charts, 14_400);
        }
    }

    #[test]
    fn a_line_moves_only_where_two_would_read_as_one() {
        let dark = |lines: &[(Role, usize)]| chart_steps(Brightness::Dark, lines);
        assert_eq!(
            dark(&[
                (Role::CliClaude, 0),
                (Role::CliClaude, 1),
                (Role::CliClaude, 2)
            ]),
            [0, 1, 2]
        );
        assert_eq!(
            dark(&[(Role::CliCodex, 0), (Role::CliAgy, 0), (Role::CliAgy, 1)]),
            [0, 0, 2],
            "Codex's blue and agy's deep teal are too close, so the deep teal moves"
        );
        assert_eq!(dark(&[(Role::CliAgy, 0), (Role::CliOpenCode, 2)]), [0, 1]);
    }

    #[test]
    fn a_clis_three_steps_are_different_in_sixteen_colours() {
        for theme in [
            crate::base::theme::Theme::dark(),
            crate::base::theme::Theme::light(),
        ] {
            let theme = theme.with_level(ColorLevel::Ansi16);
            let kit = Kit::new(&theme);
            for hue in &HUES {
                let steps: Vec<Style> = (0..STEPS)
                    .map(|step| step_style(&kit, hue.role, step))
                    .collect();
                for (i, a) in steps.iter().enumerate() {
                    for b in &steps[i + 1..] {
                        assert_ne!(a, b, "{:?} draws two steps alike: {steps:?}", hue.role);
                    }
                }
            }
        }
    }

    #[test]
    fn a_model_past_the_third_takes_the_step_its_cli_leaves_free() {
        let dark = |lines: &[(Role, usize)]| chart_steps(Brightness::Dark, lines);
        assert_eq!(dark(&[(Role::CliClaude, 3), (Role::CliClaude, 0)]), [1, 0]);
        assert_eq!(
            dark(&[
                (Role::CliClaude, 0),
                (Role::CliClaude, 4),
                (Role::CliClaude, 1)
            ]),
            [0, 2, 1]
        );
    }
}
