//! The perceptual distance follows Codex's `color.rs` (Apache-2.0): CIE76, a Euclidean distance
//! in Lab space. Sixteen color terminals get a hue-first match instead, because the nearest
//! color by distance alone turns most pastel roles grey and loses what they mean.

use std::sync::OnceLock;

use argonauts_core::settings::ui::ColorChoice;
use ratatui::style::{Color, Modifier, Style};

use super::Role;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorLevel {
    #[default]
    TrueColor,
    Ansi256,
    Ansi16,
    /// `NO_COLOR` or a dumb terminal: modifiers only.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Brightness {
    #[default]
    Dark,
    Light,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColorEnv {
    /// `NO_COLOR` is set to something non-empty.
    pub no_color: bool,
    pub colorterm: Option<String>,
    pub term: Option<String>,
    /// Set by some terminals as `foreground;background` palette indexes, such as `15;0`.
    pub colorfgbg: Option<String>,
}

impl ColorEnv {
    pub fn from_process() -> Self {
        let var = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
        Self {
            no_color: var("NO_COLOR").is_some(),
            colorterm: var("COLORTERM"),
            term: var("TERM"),
            colorfgbg: var("COLORFGBG"),
        }
    }
}

impl ColorLevel {
    /// `NO_COLOR`, `TERM=dumb`, or no `TERM` means none, `COLORTERM=truecolor` or `24bit` means
    /// truecolor, a `TERM` containing `256color` means 256, and anything else means 16.
    pub fn detect(env: &ColorEnv) -> Self {
        let term = env.term.as_deref().map(str::trim).unwrap_or_default();
        if env.no_color || term.is_empty() || term == "dumb" {
            return ColorLevel::None;
        }
        let colorterm = env.colorterm.as_deref().unwrap_or_default();
        if colorterm.eq_ignore_ascii_case("truecolor") || colorterm.eq_ignore_ascii_case("24bit") {
            ColorLevel::TrueColor
        } else if term.contains("256color") {
            ColorLevel::Ansi256
        } else {
            ColorLevel::Ansi16
        }
    }
}

impl ColorLevel {
    /// The level for `ui.color`. `never` draws none, `auto` reads the environment, and `always`
    /// keeps color under `NO_COLOR` or `TERM=dumb`, as the NO_COLOR standard lets a setting do,
    /// with at least 16 colors.
    pub fn choose(choice: ColorChoice, env: &ColorEnv) -> Self {
        match choice {
            ColorChoice::Never => ColorLevel::None,
            ColorChoice::Auto => ColorLevel::detect(env),
            ColorChoice::Always => {
                let env = ColorEnv {
                    no_color: false,
                    ..env.clone()
                };
                match ColorLevel::detect(&env) {
                    ColorLevel::None => ColorLevel::Ansi16,
                    level => level,
                }
            }
        }
    }
}

impl Brightness {
    /// Reads `COLORFGBG`. Its background index 7 or 15 means light, 0 to 6 or 8 means dark.
    /// Returns `None` when nothing says.
    pub fn detect(env: &ColorEnv) -> Option<Self> {
        let background = env.colorfgbg.as_deref()?.rsplit(';').next()?.trim();
        match background.parse::<u8>().ok()? {
            7 | 15 => Some(Brightness::Light),
            0..=6 | 8 => Some(Brightness::Dark),
            _ => None,
        }
    }
}

/// 256 and 16 color terminals get the nearest palette colors. Where that would lose the
/// meaning (row backgrounds on 16 colors), the role falls back to foregrounds or modifiers.
/// With no color at all, a role gains a modifier where color carried the meaning: dim text is
/// DIM, the accent is BOLD, changed diff words are UNDERLINED. Fitting an already fitted style
/// changes nothing, so themes can be fitted again.
pub fn downgrade(role: Role, style: Style, level: ColorLevel) -> Style {
    match level {
        ColorLevel::TrueColor => style,
        ColorLevel::Ansi256 => fit_ansi256(role, style),
        ColorLevel::Ansi16 => fit_ansi16(role, style),
        ColorLevel::None => fit_no_color(role, style),
    }
}

fn fit_ansi256(role: Role, style: Style) -> Style {
    // The cube is coarse near black, so the nearest color to a dark red or green row is grey.
    // Diff rows take fixed tints instead, as Codex's diff renderer does.
    let tint = |dark: u8, light: u8| {
        move |color: Color| match color {
            Color::Rgb(r, g, b) if lab((r, g, b)).0 < 50.0 => Color::Indexed(dark),
            Color::Rgb(..) => Color::Indexed(light),
            other => other,
        }
    };
    let bg = match role {
        Role::RemovedRow => style.bg.map(tint(52, 224)),
        Role::RemovedWord => style.bg.map(tint(88, 217)),
        Role::AddedRow => style.bg.map(tint(22, 194)),
        Role::AddedWord => style.bg.map(tint(28, 157)),
        _ => style.bg.map(to_ansi256),
    };
    Style {
        fg: style.fg.map(to_ansi256),
        bg,
        ..style
    }
}

fn fit_ansi16(role: Role, style: Style) -> Style {
    let original_fg = style.fg;
    let mut style = Style {
        fg: style.fg.map(to_ansi16_fg),
        ..style
    };
    match role {
        // A dark red or green row turns into a muddy named color, so rows lose their fill and
        // the diff text carries the color instead.
        Role::RemovedRow | Role::AddedRow | Role::UserBand => style.bg = None,
        Role::RemovedWord | Role::AddedWord => {
            style.bg = None;
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        Role::BarTrack => style.bg = Some(Color::DarkGray),
        Role::BarFill => style.bg = Some(Color::Yellow),
        _ => style.bg = style.bg.map(to_ansi16),
    }
    // The nearest named color would give purple and pink, or orange and red, the same one, so
    // agents get a fixed set of eight.
    if let Some(color) = agent_ansi16(role, original_fg) {
        style.fg = Some(color);
    }
    style
}

/// The sixteen color form of an agent name color: the bright variants on a dark background,
/// where the name is drawn light, and the plain ones on a light background.
fn agent_ansi16(role: Role, fg: Option<Color>) -> Option<Color> {
    let Some(Color::Rgb(r, g, b)) = fg else {
        return None;
    };
    let on_dark = lab((r, g, b)).0 > 55.0;
    let (dark, light) = match role {
        Role::AgentRed => (Color::LightRed, Color::Red),
        Role::AgentBlue => (Color::LightBlue, Color::Blue),
        Role::AgentGreen => (Color::LightGreen, Color::Green),
        Role::AgentYellow => (Color::LightYellow, Color::Yellow),
        Role::AgentPurple => (Color::LightMagenta, Color::Magenta),
        Role::AgentOrange => (Color::Yellow, Color::LightRed),
        Role::AgentPink => (Color::Magenta, Color::LightMagenta),
        Role::AgentCyan => (Color::LightCyan, Color::Cyan),
        _ => return None,
    };
    Some(if on_dark { dark } else { light })
}

fn fit_no_color(role: Role, style: Style) -> Style {
    let style = Style {
        fg: None,
        bg: None,
        ..style
    };
    match role {
        Role::Dim | Role::Faint | Role::Rule => style.add_modifier(Modifier::DIM),
        Role::Accent | Role::Shimmer => style.add_modifier(Modifier::BOLD),
        Role::RemovedWord | Role::AddedWord => style.add_modifier(Modifier::UNDERLINED),
        _ => style,
    }
}

fn to_ansi256(color: Color) -> Color {
    match color {
        Color::Rgb(r, g, b) => Color::Indexed(nearest_ansi256((r, g, b))),
        other => other,
    }
}

fn to_ansi16(color: Color) -> Color {
    match color {
        Color::Rgb(r, g, b) => nearest_ansi16((r, g, b)),
        Color::Indexed(index) => indexed_to_ansi16(index),
        other => other,
    }
}

/// Like [`to_ansi16`], but never black or white, which vanish on one background or the other.
fn to_ansi16_fg(color: Color) -> Color {
    match to_ansi16(color) {
        Color::Black => Color::DarkGray,
        Color::White => Color::Gray,
        other => other,
    }
}

fn indexed_to_ansi16(index: u8) -> Color {
    match NAMED.get(usize::from(index)) {
        Some((color, _)) => *color,
        None => nearest_ansi16(xterm_rgb(index)),
    }
}

/// The xterm 256 color index closest to `rgb` by perceived distance. Indexes 0 to 15 are never
/// picked, because terminals theme them freely, so their real colors are unknown.
pub fn nearest_ansi256(rgb: (u8, u8, u8)) -> u8 {
    let target = lab(rgb);
    let palette = xterm_lab();
    let mut best = (f32::MAX, 16u8);
    for (offset, candidate) in palette.iter().enumerate() {
        let distance = lab_distance(target, *candidate);
        if distance < best.0 {
            // The palette holds indexes 16 to 255, so the offset always fits.
            best = (distance, (offset + 16) as u8);
        }
    }
    best.1
}

/// Greys pick among black, the two greys, and white by lightness. Anything with a clear hue
/// picks the basic color of the nearest hue, then the plain or light variant by lightness, so
/// a soft green stays green instead of becoming grey.
pub fn nearest_ansi16(rgb: (u8, u8, u8)) -> Color {
    const GREY_CHROMA: f32 = 12.0;
    let (lightness, chroma, hue) = lch(lab(rgb));
    let greys = [Color::Black, Color::DarkGray, Color::Gray, Color::White];
    let score = |candidate: &(Color, (u8, u8, u8))| -> f32 {
        let (l, _, h) = lch(lab(candidate.1));
        if chroma < GREY_CHROMA {
            (lightness - l).abs()
        } else {
            hue_distance(hue, h) + 0.25 * (lightness - l).abs()
        }
    };
    NAMED
        .iter()
        .filter(|candidate| (chroma < GREY_CHROMA) == greys.contains(&candidate.0))
        .min_by(|a, b| score(a).total_cmp(&score(b)))
        .map_or(Color::Reset, |candidate| candidate.0)
}

/// The basic colors in palette order, with xterm's default values.
const NAMED: [(Color, (u8, u8, u8)); 16] = [
    (Color::Black, (0, 0, 0)),
    (Color::Red, (205, 0, 0)),
    (Color::Green, (0, 205, 0)),
    (Color::Yellow, (205, 205, 0)),
    (Color::Blue, (0, 0, 238)),
    (Color::Magenta, (205, 0, 205)),
    (Color::Cyan, (0, 205, 205)),
    (Color::Gray, (229, 229, 229)),
    (Color::DarkGray, (127, 127, 127)),
    (Color::LightRed, (255, 0, 0)),
    (Color::LightGreen, (0, 255, 0)),
    (Color::LightYellow, (255, 255, 0)),
    (Color::LightBlue, (92, 92, 255)),
    (Color::LightMagenta, (255, 0, 255)),
    (Color::LightCyan, (0, 255, 255)),
    (Color::White, (255, 255, 255)),
];

/// The color of an xterm palette index from 16 up: a 6x6x6 cube, then 24 greys.
fn xterm_rgb(index: u8) -> (u8, u8, u8) {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match index {
        0..=15 => NAMED[usize::from(index)].1,
        16..=231 => {
            let i = usize::from(index - 16);
            (LEVELS[i / 36], LEVELS[i / 6 % 6], LEVELS[i % 6])
        }
        _ => {
            let grey = 8 + 10 * (index - 232);
            (grey, grey, grey)
        }
    }
}

/// Lab values of indexes 16 to 255, computed once.
fn xterm_lab() -> &'static [(f32, f32, f32); 240] {
    static PALETTE: OnceLock<[(f32, f32, f32); 240]> = OnceLock::new();
    PALETTE.get_or_init(|| std::array::from_fn(|i| lab(xterm_rgb((i + 16) as u8))))
}

fn lab_distance(a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
    let (dl, da, db) = (a.0 - b.0, a.1 - b.1, a.2 - b.2);
    (dl * dl + da * da + db * db).sqrt()
}

fn hue_distance(a: f32, b: f32) -> f32 {
    let d = (a - b).abs() % 360.0;
    d.min(360.0 - d)
}

/// Lightness, chroma, and hue in degrees.
fn lch((l, a, b): (f32, f32, f32)) -> (f32, f32, f32) {
    (l, a.hypot(b), b.atan2(a).to_degrees().rem_euclid(360.0))
}

/// sRGB to CIE Lab with a D65 white point.
fn lab((r, g, b): (u8, u8, u8)) -> (f32, f32, f32) {
    fn linear(c: u8) -> f32 {
        let c = f32::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    fn f(t: f32) -> f32 {
        if t > 0.008856 {
            t.cbrt()
        } else {
            7.787 * t + 16.0 / 116.0
        }
    }
    let (r, g, b) = (linear(r), linear(g), linear(b));
    let x = (r * 0.4124 + g * 0.3576 + b * 0.1805) / 0.95047;
    let y = r * 0.2126 + g * 0.7152 + b * 0.0722;
    let z = (r * 0.0193 + g * 0.1192 + b * 0.9505) / 1.08883;
    let (fx, fy, fz) = (f(x), f(y), f(z));
    (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn env(term: Option<&str>, colorterm: Option<&str>) -> ColorEnv {
        ColorEnv {
            no_color: false,
            colorterm: colorterm.map(str::to_owned),
            term: term.map(str::to_owned),
            colorfgbg: None,
        }
    }

    #[test]
    fn color_level_reads_the_environment() {
        let level = |term, colorterm| ColorLevel::detect(&env(term, colorterm));
        assert_eq!(
            level(Some("xterm-256color"), Some("truecolor")),
            ColorLevel::TrueColor
        );
        assert_eq!(
            level(Some("xterm-kitty"), Some("24bit")),
            ColorLevel::TrueColor
        );
        assert_eq!(level(Some("xterm-256color"), None), ColorLevel::Ansi256);
        assert_eq!(
            level(Some("screen-256color"), Some("yes")),
            ColorLevel::Ansi256
        );
        assert_eq!(level(Some("xterm"), None), ColorLevel::Ansi16);
        assert_eq!(level(Some("dumb"), Some("truecolor")), ColorLevel::None);
        assert_eq!(level(None, Some("truecolor")), ColorLevel::None);

        let mut no_color = env(Some("xterm-256color"), Some("truecolor"));
        no_color.no_color = true;
        assert_eq!(ColorLevel::detect(&no_color), ColorLevel::None);
    }

    #[test]
    fn the_color_setting_overrides_no_color_and_dumb_terminals() {
        let no_color = ColorEnv {
            no_color: true,
            ..env(Some("xterm-256color"), Some("truecolor"))
        };
        let dumb = env(Some("dumb"), None);
        assert_eq!(
            ColorLevel::choose(ColorChoice::Auto, &no_color),
            ColorLevel::None
        );
        assert_eq!(
            ColorLevel::choose(ColorChoice::Auto, &dumb),
            ColorLevel::None
        );
        assert_eq!(
            ColorLevel::choose(ColorChoice::Always, &no_color),
            ColorLevel::TrueColor
        );
        assert_eq!(
            ColorLevel::choose(ColorChoice::Always, &dumb),
            ColorLevel::Ansi16
        );
        let rich = env(Some("xterm-256color"), Some("truecolor"));
        assert_eq!(
            ColorLevel::choose(ColorChoice::Never, &rich),
            ColorLevel::None
        );
        assert_eq!(
            ColorLevel::choose(ColorChoice::Auto, &rich),
            ColorLevel::TrueColor
        );
    }

    #[test]
    fn brightness_reads_the_last_colorfgbg_field() {
        let brightness = |value: &str| {
            Brightness::detect(&ColorEnv {
                colorfgbg: Some(value.to_owned()),
                ..ColorEnv::default()
            })
        };
        assert_eq!(brightness("0;15"), Some(Brightness::Light));
        assert_eq!(brightness("0;7"), Some(Brightness::Light));
        assert_eq!(brightness("15;0"), Some(Brightness::Dark));
        assert_eq!(brightness("15;default;8"), Some(Brightness::Dark));
        assert_eq!(brightness("15;12"), None);
        assert_eq!(brightness("oops"), None);
        assert_eq!(Brightness::detect(&ColorEnv::default()), None);
    }

    #[test]
    fn ansi256_uses_the_cube_and_greys_only() {
        assert_eq!(nearest_ansi256((0, 0, 0)), 16);
        assert_eq!(nearest_ansi256((255, 255, 255)), 231);
        assert_eq!(nearest_ansi256((255, 0, 0)), 196);
        assert_eq!(nearest_ansi256((0x6b, 0x72, 0x80)), 243);
        assert_eq!(nearest_ansi256((128, 128, 128)), 244);
        for rgb in [(0, 0, 0), (1, 2, 3), (250, 250, 250), (0x4a, 0x20, 0x25)] {
            assert!(nearest_ansi256(rgb) >= 16, "{rgb:?}");
        }
    }

    #[test]
    fn ansi16_keeps_the_hue_of_pastel_roles() {
        assert_eq!(nearest_ansi16((0x6b, 0x72, 0x80)), Color::DarkGray);
        assert_eq!(nearest_ansi16((0xe6, 0xa2, 0x3c)), Color::Yellow);
        assert_eq!(nearest_ansi16((0x98, 0xc3, 0x79)), Color::Green);
        assert_eq!(nearest_ansi16((0xe0, 0x6c, 0x75)), Color::LightRed);
        assert_eq!(nearest_ansi16((0x8c, 0xaa, 0xee)), Color::LightBlue);
        assert_eq!(nearest_ansi16((0x56, 0xb6, 0xc2)), Color::Cyan);
        assert_eq!(nearest_ansi16((10, 10, 10)), Color::Black);
        assert_eq!(nearest_ansi16((250, 250, 250)), Color::White);
    }

    #[test]
    fn downgrade_is_idempotent_for_every_role_and_level() {
        for theme in [Theme::dark(), Theme::light()] {
            for level in [
                ColorLevel::TrueColor,
                ColorLevel::Ansi256,
                ColorLevel::Ansi16,
                ColorLevel::None,
            ] {
                for role in Role::ALL {
                    let once = downgrade(role, theme.style(role), level);
                    let twice = downgrade(role, once, level);
                    assert_eq!(once, twice, "{role:?} at {level:?}");
                }
            }
        }
    }

    #[test]
    fn ansi16_drops_row_fills_but_keeps_the_bar() {
        let theme = Theme::dark().with_level(ColorLevel::Ansi16);
        assert_eq!(theme.style(Role::RemovedRow).bg, None);
        assert_eq!(theme.style(Role::UserBand).bg, None);
        assert_eq!(theme.style(Role::AddedWord).bg, None);
        assert!(
            theme
                .style(Role::AddedWord)
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );
        assert_eq!(theme.style(Role::BarTrack).bg, Some(Color::DarkGray));
        assert_eq!(theme.style(Role::BarFill).bg, Some(Color::Yellow));
        assert_eq!(theme.style(Role::Dim).fg, Some(Color::DarkGray));
        assert_eq!(theme.style(Role::Success).fg, Some(Color::Green));
    }

    #[test]
    fn no_color_keeps_meaning_in_modifiers() {
        let theme = Theme::dark().with_level(ColorLevel::None);
        for role in Role::ALL {
            let style = theme.style(role);
            assert_eq!((style.fg, style.bg), (None, None), "{role:?}");
        }
        assert!(theme.style(Role::Dim).add_modifier.contains(Modifier::DIM));
        assert!(
            theme
                .style(Role::Accent)
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            theme
                .style(Role::Shimmer)
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            theme
                .style(Role::RemovedWord)
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );
        assert!(
            theme
                .style(Role::SyntaxComment)
                .add_modifier
                .contains(Modifier::ITALIC)
        );
    }

    #[test]
    fn ansi256_maps_every_rgb_role_to_an_index() {
        let theme = Theme::dark().with_level(ColorLevel::Ansi256);
        for role in Role::ALL {
            let style = theme.style(role);
            for color in [style.fg, style.bg].into_iter().flatten() {
                assert!(matches!(color, Color::Indexed(16..)), "{role:?}: {color:?}");
            }
        }
    }

    #[test]
    fn ansi256_diff_rows_keep_their_tint() {
        let dark = Theme::dark().with_level(ColorLevel::Ansi256);
        assert_eq!(dark.style(Role::RemovedRow).bg, Some(Color::Indexed(52)));
        assert_eq!(dark.style(Role::AddedRow).bg, Some(Color::Indexed(22)));
        let light = Theme::light().with_level(ColorLevel::Ansi256);
        assert_eq!(light.style(Role::RemovedRow).bg, Some(Color::Indexed(224)));
        assert_eq!(light.style(Role::AddedWord).bg, Some(Color::Indexed(157)));
    }

    #[test]
    fn with_accent_refits_to_the_level() {
        let theme = Theme::dark()
            .with_level(ColorLevel::Ansi16)
            .with_accent((0x40, 0x90, 0xff));
        assert_eq!(theme.style(Role::Accent).fg, Some(Color::LightBlue));
        assert_eq!(theme.style(Role::BarFill).bg, Some(Color::Yellow));
    }
}
