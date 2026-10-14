//! How the Overview writes its numbers and picks its colours: big token counts, dollars with
//! thousands, each CLI's word, and each CLI's ramp from faint to full for the year of days.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::overview::OverviewData;
use ratatui::style::{Color, Style};

use crate::overview::cards::cli_role;
use crate::overview::stats::draw;
use crate::theme::{Brightness, ColorLevel, Role, Theme, nearest_ansi256};
use crate::widgets::Kit;

/// `812`, `12.3k`, `4.5M`, `118.4b`, `3b`: one decimal, dropped when it's nought.
pub fn big(count: u64) -> String {
    let amount = amount(count);
    match amount.strip_suffix(".0b") {
        Some(whole) => format!("{whole}b"),
        None => amount.replace(".0M", "M").replace(".0k", "k"),
    }
}

/// As [`big`], but a nought decimal stays, so a column of amounts lines up: `26.0b`.
pub fn amount(count: u64) -> String {
    let (value, unit) = match count {
        0..1_000 => return count.to_string(),
        1_000..999_950 => (count as f64 / 1e3, "k"),
        999_950..999_950_000 => (count as f64 / 1e6, "M"),
        _ => (count as f64 / 1e9, "b"),
    };
    format!("{value:.1}{unit}")
}

/// `~$9,640`, `~$60`, `~$4.20`: whole dollars from ten, with thousands marked. The value is
/// always a reckoning at list prices, so it carries the `~`.
pub fn dollars(usd: f64) -> String {
    if usd < 10.0 {
        return format!("~${usd:.2}");
    }
    format!("~${}", thousands(usd.round() as u64))
}

/// `1,086`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// The share of `part` in `whole` as whole percents, `<1%` for a sliver.
pub fn share(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "0%".to_owned();
    }
    let percent = part as f64 / whole as f64 * 100.0;
    if part > 0 && percent < 1.0 {
        return "<1%".to_owned();
    }
    format!("{percent:.0}%")
}

/// How a CLI is named on the Overview: Antigravity CLI is `agy`, as its command is.
pub fn cli_word(names: &DisplayNames, agent: &str) -> String {
    match agent {
        "antigravity-cli" => "agy".to_owned(),
        _ => names.harness(agent),
    }
}

/// `Opus 5.5`, or the CLI's own word when no model is named.
pub fn model_word(names: &DisplayNames, agent: &str, model: Option<&str>) -> String {
    names
        .model(agent, model)
        .unwrap_or_else(|| format!("{} default", cli_word(names, agent)))
}

/// The colour a model draws in: its CLI's, in the shade of its place among that CLI's
/// models, and faint once a CLI has more models than shades.
pub fn model_style(kit: &Kit<'_>, data: &OverviewData, agent: &str, model: Option<&str>) -> Style {
    let nth = data
        .models
        .iter()
        .filter(|row| row.agent == agent)
        .position(|row| row.model.as_deref() == model)
        .unwrap_or(0);
    if nth >= 4 {
        return kit.fg(Role::Faint);
    }
    draw::shade(kit, cli_role(agent), nth)
}

/// How far each step of a ramp is from the screen's paper toward the full colour: the first
/// step is a quarter of the way, the last the colour itself.
const RAMP: [f64; 4] = [0.3, 0.5, 0.75, 1.0];

/// The `level` step, 1 to 4, of `style`'s colour, fainter for less. Without true or 256
/// colours there are no steps, and the glyph tells them apart instead.
pub fn ramp(kit: &Kit<'_>, style: Style, level: usize) -> Style {
    let theme = kit.theme;
    let Some(Color::Rgb(r, g, b)) = style.fg.or_else(|| theme.color(Role::Accent)) else {
        return style;
    };
    if matches!(theme.level, ColorLevel::Ansi16 | ColorLevel::None) {
        return style;
    }
    let paper = paper(theme);
    let mix = RAMP[level.clamp(1, 4) - 1];
    let toward = |full: u8, paper: u8| {
        (f64::from(paper) + (f64::from(full) - f64::from(paper)) * mix).round() as u8
    };
    let mixed = (toward(r, paper.0), toward(g, paper.1), toward(b, paper.2));
    let color = match theme.level {
        ColorLevel::TrueColor => Color::Rgb(mixed.0, mixed.1, mixed.2),
        _ => Color::Indexed(nearest_ansi256(mixed)),
    };
    Style::new().fg(color)
}

/// The colour a day with nothing draws in: just off the paper.
pub fn empty_day(kit: &Kit<'_>) -> Style {
    kit.fg(Role::BarTrack)
}

/// The screen's background, near enough for mixing.
fn paper(theme: &Theme) -> (u8, u8, u8) {
    match theme.brightness {
        Brightness::Dark => (0x1f, 0x22, 0x33),
        Brightness::Light => (0xff, 0xff, 0xff),
    }
}

/// Each CLI's colour as text, from the theme even where the palette has none.
pub fn cli_style(kit: &Kit<'_>, agent: &str) -> Style {
    let role = cli_role(agent);
    let own = kit.fg(role);
    if own.fg.is_some() {
        return own;
    }
    match Theme::for_brightness(kit.theme.brightness).color(role) {
        Some(color) if kit.has_color() => Style::new().fg(color),
        _ => own,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_numbers_read_short() {
        assert_eq!(big(812), "812");
        assert_eq!(big(12_340), "12.3k");
        assert_eq!(big(4_500_000), "4.5M");
        assert_eq!(big(118_400_000_000), "118.4b");
        assert_eq!(big(3_000_000_000), "3b");
        assert_eq!(amount(26_000_000_000), "26.0b");
        assert_eq!(dollars(60.0), "~$60");
        assert_eq!(dollars(9_640.4), "~$9,640");
        assert_eq!(dollars(4.2), "~$4.20");
        assert_eq!(thousands(1_086), "1,086");
        assert_eq!(share(1, 1_000), "<1%");
        assert_eq!(share(62, 100), "62%");
    }

    #[test]
    fn a_ramp_runs_from_faint_to_the_full_colour() {
        let theme = Theme::dark().with_level(ColorLevel::TrueColor);
        let kit = Kit::new(&theme);
        let full = cli_style(&kit, "claude-code");
        assert_eq!(ramp(&kit, full, 4), full);
        assert_ne!(ramp(&kit, full, 1), full);
        assert_ne!(ramp(&kit, full, 1), ramp(&kit, full, 2));
    }
}
