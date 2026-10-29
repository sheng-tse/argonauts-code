//! The one drawer every mod goes through: a mod's file and its numbers in, rows from the kit
//! out. A built-in card and a file of yours draw the same way, so a mod is always colour, bars
//! and marks, and never a block of text.
//!
//! | Body | Draws |
//! |---|---|
//! | stack | a stacked bar, and in the full form its legend |
//! | meters | one line meter a row |
//! | list | a mark, a name, and a number a row |
//! | columns | a two-row column chart over a letter each |
//! | sparkline | one row of columns |
//! | marks | a square per item, and in the full form its legend |
//! | text | the header's words alone |

use argonauts_core::logos::logo_for;
use argonauts_core::settings::mods::{
    BuiltinSource, ModBody, ModColor, ModFile, ModForm, ModLevel, ModPart, ModReading, ModSource,
};
use ratatui::text::Span;

use super::rows::PageGlyphs;
use crate::kit::bars::{self, Stack};
use crate::kit::bodies::{self, Chart, ListRow, ListSlots, MeterRow, MeterSlots};
use crate::kit::context::CLAUDE_CODE_PARTS;
use crate::kit::legend::{self, Entry};
use crate::kit::{
    Badge, ContextCard, ContextPart, Drawn, Form, Frame, Header, Kit, Level, Limit, LimitKind,
    State, States, Toggle, spread,
};
use crate::text::{sanitize_line, spans_width, truncate_spans};
use crate::theme::Role;

/// The legend's columns at most, as the context card has them.
const LEGEND_COLUMNS: usize = 3;
/// The folded line's small bar.
const LINE_CELLS: usize = 12;
/// The squares a marks body draws at most, so one huge count can't fill the screen.
const MOST_MARKS: usize = 400;

/// The colors parts take in turn when neither the data nor the file gives one.
const PALETTE: [Role; 6] = [
    Role::AgentBlue,
    Role::AgentPurple,
    Role::AgentCyan,
    Role::AgentOrange,
    Role::AgentPink,
    Role::AgentGreen,
];

pub fn role_of(color: ModColor) -> Role {
    match color {
        ModColor::Green => Role::Success,
        ModColor::Amber => Role::Warn,
        ModColor::Red => Role::Danger,
        ModColor::Blue => Role::AgentBlue,
        ModColor::Purple => Role::AgentPurple,
        ModColor::Cyan => Role::AgentCyan,
        ModColor::Orange => Role::AgentOrange,
        ModColor::Pink => Role::AgentPink,
        ModColor::Gold => Role::Accent,
        // What's empty or idle, such as the free room in a window: the bar's own unused color.
        ModColor::Grey => Role::CatFree,
    }
}

pub fn level_of(level: ModLevel) -> Level {
    match level {
        ModLevel::Ok => Level::Ok,
        ModLevel::Warn => Level::Warn,
        ModLevel::Danger => Level::Danger,
    }
}

/// A part's color: the file's for its label, then its CLI's hue, then the data's own, then
/// its turn in the palette.
fn part_role(file: &ModFile, part: &ModPart, index: usize) -> Role {
    if let Some(color) = file.colors.get(&part.label) {
        return role_of(*color);
    }
    if let Some(cli) = &part.cli {
        return Role::for_cli(cli);
    }
    part.color.map_or(PALETTE[index % PALETTE.len()], role_of)
}

fn clean(text: &str) -> String {
    sanitize_line(text).trim().to_owned()
}

/// `7 of 9 passed` as the bold `7` and the dim ` of 9 passed`.
fn amount_and_tail(summary: &str) -> (Option<String>, Option<String>) {
    let summary = clean(summary);
    if summary.is_empty() {
        return (None, None);
    }
    match summary.split_once(' ') {
        Some((amount, tail)) => (Some(amount.to_owned()), Some(format!(" {tail}"))),
        None => (Some(summary), None),
    }
}

/// The card's first row. A badge the reading only takes from one of its parts stays off a
/// full card whose legend already names that part beside its share of the bar.
fn header(kit: &Kit<'_>, file: &ModFile, reading: &ModReading, form: ModForm) -> Header {
    let (amount, tail) = amount_and_tail(&reading.summary);
    let legend = form == ModForm::Full && matches!(file.body, ModBody::Stack | ModBody::Marks);
    let badge = if legend && reading.badge.is_none() {
        None
    } else {
        reading.badge_words()
    };
    Header {
        amount,
        tail: tail.into_iter().map(|tail| kit.dim(tail)).collect(),
        badge: badge.map(|words| Badge::words(clean(&words), level_of(reading.level))),
        ..Header::new(clean(&file.title), Role::Accent)
    }
}

/// The parts as a stacked bar. Values keep three decimals, since the kit counts in whole
/// numbers.
fn stack(file: &ModFile, reading: &ModReading) -> Stack {
    let scaled = |value: f64| (value.max(0.0) * 1000.0).round() as u64;
    let parts: Vec<(u64, Role)> = reading
        .parts
        .iter()
        .enumerate()
        .map(|(index, part)| (scaled(part.value), part_role(file, part, index)))
        .collect();
    let total = parts
        .iter()
        .map(|(value, _)| *value)
        .fold(0_u64, u64::saturating_add)
        .max(1);
    Stack {
        parts,
        total,
        tick: None,
        held_from: None,
        track: Role::BarTrack,
    }
}

fn legend_entries(file: &ModFile, reading: &ModReading) -> Vec<Entry> {
    let total = reading.total();
    reading
        .parts
        .iter()
        .enumerate()
        .map(|(index, part)| {
            let entry = Entry::new(
                part_role(file, part, index),
                clean(&part.label),
                clean(&part.amount()),
            );
            let entry = match &part.cli {
                Some(cli) => entry.with_logo(logo_for(cli, None)),
                None => entry,
            };
            if total > 0.0 {
                entry.shared(format!("{:.0}%", part.value / total * 100.0))
            } else {
                entry
            }
        })
        .collect()
}

fn meter_rows(file: &ModFile, reading: &ModReading) -> Vec<MeterRow> {
    let largest = reading
        .parts
        .iter()
        .map(|part| part.value)
        .fold(0.0, f64::max);
    reading
        .parts
        .iter()
        .map(|part| {
            let against = part.max.unwrap_or(largest);
            let ratio = if against > 0.0 {
                part.value / against
            } else {
                0.0
            };
            MeterRow::Meter {
                label: clean(&part.label),
                ratio,
                level: level_of(file.levels.level(ratio * 100.0)),
                unit: String::new(),
                note: part.note.as_deref().map(clean).unwrap_or_default(),
                tick: None,
            }
        })
        .collect()
}

/// A list row's mark, from its color: green is done, amber waits for you, red failed.
fn state_of(part: &ModPart) -> State {
    match part.color {
        Some(ModColor::Green) => State::Done,
        Some(ModColor::Amber) => State::NeedsYou,
        Some(ModColor::Red) => State::Failed,
        Some(ModColor::Grey) => State::Stopped,
        Some(ModColor::Blue | ModColor::Purple | ModColor::Cyan) => State::Working,
        _ => State::Waiting,
    }
}

fn list_rows(reading: &ModReading) -> Vec<ListRow> {
    reading
        .parts
        .iter()
        .map(|part| ListRow {
            state: state_of(part),
            name: clean(&part.label),
            word: String::new(),
            note: part.note.as_deref().map(clean).unwrap_or_default(),
            ratio: None,
            numbers: vec![clean(&part.amount())],
        })
        .collect()
}

fn values(reading: &ModReading) -> Vec<f64> {
    reading.parts.iter().map(|part| part.value).collect()
}

fn chart(reading: &ModReading) -> Chart {
    let top = values(reading).into_iter().fold(0.0, f64::max);
    Chart {
        values: values(reading),
        label: argonauts_core::settings::mods::reading::plain_number(top),
        letters: reading
            .parts
            .iter()
            .map(|part| clean(&part.label).chars().take(1).collect())
            .collect(),
        role: Role::BarFill,
        last_role: Role::Accent,
    }
}

/// One row of columns, the last in the accent color.
fn sparkline(kit: &Kit<'_>, reading: &ModReading) -> Vec<Span<'static>> {
    bars::columns(kit, &values(reading), 1, Role::BarFill, Some(Role::Accent))
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// A square per item in its part's color, wrapped to `width`.
fn marks(kit: &Kit<'_>, file: &ModFile, reading: &ModReading, width: usize) -> Drawn {
    // Without color the squares would all look alike anyway, so they take the plain mark.
    let square = if kit.has_color() {
        PageGlyphs::for_kit(kit).square
    } else {
        "#"
    };
    let mut squares: Vec<Span<'static>> = Vec::new();
    for (index, part) in reading.parts.iter().enumerate() {
        let role = part_role(file, part, index);
        let count =
            (part.value.max(0.0).round() as usize).min(MOST_MARKS - squares.len().min(MOST_MARKS));
        squares.extend((0..count).map(|_| Span::styled(square, kit.fg(role))));
    }
    // A square and the space after it.
    let per_row = (width / 2).max(1);
    let mut body = Drawn::new();
    for row in squares.chunks(per_row) {
        let mut line = Vec::with_capacity(row.len() * 2);
        for square in row {
            line.push(square.clone());
            line.push(Span::raw(" "));
        }
        line.pop();
        body.push(line);
    }
    body
}

/// What goes under the header in a card `inner` cells wide.
fn body_rows(
    kit: &Kit<'_>,
    file: &ModFile,
    reading: &ModReading,
    form: ModForm,
    inner: usize,
) -> Drawn {
    let full = form == ModForm::Full;
    // The legend sits a blank row under the start of the bar, packed to the left, and not
    // spread across the card.
    let legend_rows = |body: &mut Drawn| {
        if full {
            body.push(Vec::new());
            let entries = legend_entries(file, reading);
            let width = legend::packed_width(kit, &entries, inner, LEGEND_COLUMNS);
            for row in legend::legend(kit, &entries, width, LEGEND_COLUMNS) {
                body.push(row);
            }
        }
    };
    match file.body {
        ModBody::Stack => {
            let mut body = bodies::stack(kit, &stack(file, reading), &[], inner, 1, LEGEND_COLUMNS);
            legend_rows(&mut body);
            body
        }
        ModBody::Meters => {
            let rows = meter_rows(file, reading);
            let rows = if full {
                &rows[..]
            } else {
                &rows[..rows.len().min(1)]
            };
            bodies::meters(kit, rows, inner, MeterSlots::default())
        }
        ModBody::List => {
            let rows = list_rows(reading);
            let rows = if full {
                &rows[..]
            } else {
                &rows[..rows.len().min(3)]
            };
            bodies::list(kit, &States::default(), rows, inner, &ListSlots::default())
        }
        ModBody::Columns => bodies::columns(kit, &chart(reading)),
        ModBody::Sparkline => {
            let mut body = Drawn::new();
            body.push(sparkline(kit, reading));
            body
        }
        ModBody::Marks => {
            let mut body = marks(kit, file, reading, inner);
            legend_rows(&mut body);
            body
        }
        ModBody::Text => Drawn::new(),
    }
}

/// The rows inside a frame: the header, then the body.
pub fn body(
    kit: &Kit<'_>,
    file: &ModFile,
    reading: &ModReading,
    form: ModForm,
    inner: usize,
) -> Drawn {
    let mut out = Drawn::new();
    out.push(header(kit, file, reading, form).draw(kit, inner));
    out.append(body_rows(kit, file, reading, form, inner));
    out
}

/// The full or compact card inside `frame`. The built-in context mod is the context card,
/// wherever it shows.
pub fn card(
    kit: &Kit<'_>,
    file: &ModFile,
    reading: &ModReading,
    form: ModForm,
    frame: Frame,
) -> Drawn {
    if let Some(card) = context_card(file, reading) {
        let form = match form {
            ModForm::Compact => Form::Compact,
            ModForm::Full | ModForm::Line => Form::Full,
        };
        return card.card(kit, form, frame);
    }
    frame.draw(kit, body(kit, file, reading, form, frame.inner()))
}

/// The context card of the built-in context mod, from its reading: a part for each category
/// in the CLI's order, each measured against the window, and the free space in grey, which
/// leaves out the room held back for compacting. `None` for any other mod.
pub fn context_card(file: &ModFile, reading: &ModReading) -> Option<ContextCard> {
    let ModSource::Builtin {
        source: BuiltinSource::Context,
    } = &file.data
    else {
        return None;
    };
    let tokens = |part: &ModPart| part.value.max(0.0).round() as u64;
    let is_free = |part: &&ModPart| part.color == Some(ModColor::Grey);
    let parts: Vec<ContextPart> = reading
        .parts
        .iter()
        .enumerate()
        .filter(|(_, part)| !is_free(part))
        .map(|(index, part)| {
            let name = clean(&part.label);
            match CLAUDE_CODE_PARTS.iter().find(|(known, _)| *known == name) {
                Some((_, color)) => ContextPart::named(name, *color, Some(tokens(part))),
                None => ContextPart::new(name, part_role(file, part, index), Some(tokens(part))),
            }
        })
        .collect();
    let window = reading
        .parts
        .iter()
        .filter_map(|part| part.max)
        .map(|max| max.max(0.0).round() as u64)
        .max()
        .filter(|window| *window > 0);
    let used: u64 = parts.iter().filter_map(|part| part.tokens).sum();
    let free = reading.parts.iter().find(is_free).map(tokens);
    let limit = free
        .map(|free| Limit {
            at: used + free,
            kind: LimitKind::Compacts,
        })
        .filter(|limit| window.is_some_and(|window| limit.at < window));
    Some(ContextCard {
        used: window.map(|_| used),
        window,
        limit,
        ..ContextCard::with_parts(parts, false)
    })
}

/// A small picture of the numbers, for the one-row form and for `/mods`' list.
pub fn small(kit: &Kit<'_>, file: &ModFile, reading: &ModReading) -> Vec<Span<'static>> {
    match file.body {
        ModBody::Stack | ModBody::Marks => {
            bars::stacked_line(kit, &stack(file, reading), LINE_CELLS)
        }
        ModBody::Meters => match meter_rows(file, reading).first() {
            Some(MeterRow::Meter { ratio, level, .. }) => {
                bars::pills(kit, *ratio, LINE_CELLS / 2, level.fill_role())
            }
            _ => Vec::new(),
        },
        ModBody::Columns | ModBody::Sparkline => sparkline(kit, reading),
        ModBody::List | ModBody::Text => Vec::new(),
    }
}

/// One row, `width` cells: `▸ ◆ checks  ━━━━━━━━━━── 7 of 9 passed   1 failed  [+]`. The key
/// gives way first, then the words are cut; the badge stays.
pub fn line(
    kit: &Kit<'_>,
    file: &ModFile,
    reading: &ModReading,
    width: usize,
    toggle: Option<Toggle>,
) -> Vec<Span<'static>> {
    if let Some(card) = context_card(file, reading) {
        return card.line(kit, width, toggle);
    }
    let mut left = vec![
        Span::raw(" "),
        kit.dim(format!("{} ", kit.glyphs.folded)),
        kit.span(kit.glyphs.the_mod, Role::Accent),
        Span::raw(" "),
        kit.strong(clean(&file.title)),
    ];
    let picture = small(kit, file, reading);
    if !picture.is_empty() {
        left.push(Span::raw("  "));
        left.extend(picture);
    }
    let (amount, tail) = amount_and_tail(&reading.summary);
    if let Some(amount) = amount {
        left.push(Span::raw("  "));
        left.push(kit.strong(amount));
    }
    if let Some(tail) = tail {
        left.push(kit.dim(tail));
    }
    let badge: Vec<Span<'static>> = reading
        .badge_words()
        .map(|words| Badge::words(clean(&words), level_of(reading.level)).spans(kit))
        .unwrap_or_default();
    let end: Vec<Span<'static>> = toggle
        .map(|toggle| vec![Span::raw(" "), kit.dim(toggle.text())])
        .unwrap_or_default();
    let key: Vec<Span<'static>> = file
        .key
        .as_deref()
        .map(|key| vec![kit.dim(clean(key)), Span::raw("  ")])
        .unwrap_or_default();
    let fixed = spans_width(&badge) + spans_width(&end);
    let mut right = if spans_width(&left) + spans_width(&key) + fixed < width {
        key
    } else {
        Vec::new()
    };
    right.extend(badge);
    right.extend(end);
    let room = width.saturating_sub(spans_width(&right) + 1);
    let left = if spans_width(&left) > room {
        truncate_spans(&left, room)
    } else {
        left
    };
    spread(left, right, width)
}

/// A few words for the bottom line: `checks 7 of 9 passed`, in the level's color once it
/// isn't ok. A text mod is its words alone, as `main` is for the branch.
pub fn chip(kit: &Kit<'_>, file: &ModFile, reading: &ModReading) -> Vec<Span<'static>> {
    if let Some(card) = context_card(file, reading) {
        return card.chip(kit);
    }
    let words = clean(&reading.summary);
    let role = level_of(reading.level).number_role();
    if file.body == ModBody::Text {
        return vec![kit.span(words, role)];
    }
    let mut out = vec![kit.dim(format!("{} ", clean(&file.title)))];
    let picture = match file.body {
        ModBody::Columns | ModBody::Sparkline => sparkline(kit, reading),
        _ => Vec::new(),
    };
    if !picture.is_empty() {
        out.extend(picture);
        out.push(Span::raw(" "));
    }
    out.push(kit.span(words, role));
    out
}

/// The mod above the input on a screen `screen` cells wide, in the form its file names.
pub fn above(kit: &Kit<'_>, file: &ModFile, reading: &ModReading, screen: usize) -> Drawn {
    above_as(kit, file, reading, file.form, screen)
}

/// The mod above the input in `form`, which is its file's until the rows run short.
pub fn above_as(
    kit: &Kit<'_>,
    file: &ModFile,
    reading: &ModReading,
    form: ModForm,
    screen: usize,
) -> Drawn {
    match form {
        ModForm::Full | ModForm::Compact => card(kit, file, reading, form, Frame::band(screen)),
        ModForm::Line => {
            let mut out = Drawn::new();
            out.push(line(kit, file, reading, screen, Some(Toggle::Folded)));
            out
        }
    }
}

/// The mod as a page previews it: in its own form, `width` cells wide, with no toggle since
/// nothing folds there.
pub fn preview(kit: &Kit<'_>, file: &ModFile, reading: &ModReading, width: usize) -> Drawn {
    match file.form {
        ModForm::Full | ModForm::Compact => {
            let frame = Frame {
                left: 0,
                width,
                toggle: None,
                active: false,
            };
            card(kit, file, reading, file.form, frame)
        }
        ModForm::Line => {
            let mut out = Drawn::new();
            out.push(line(kit, file, reading, width, None));
            out
        }
    }
}

/// The mod as a card on a tab of the usage panel.
pub fn on_tab(kit: &Kit<'_>, file: &ModFile, reading: &ModReading, screen: usize) -> Drawn {
    let form = match file.form {
        ModForm::Line => ModForm::Compact,
        form => form,
    };
    card(kit, file, reading, form, Frame::view(screen))
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::mods::{builtin, check};

    use super::*;
    use crate::theme::{ColorLevel, Theme};

    fn text(drawn: &Drawn) -> String {
        drawn
            .lines
            .iter()
            .map(|line| {
                let row: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                row.trim_end().to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Parts too large to add up never stop the drawing, even ones no reading lets through.
    #[test]
    fn a_stack_of_huge_parts_still_draws() {
        let reading: ModReading = serde_json::from_str(
            r#"{"summary": "big", "parts": [{"label": "a", "value": 1e19},
                {"label": "b", "value": 1e19}]}"#,
        )
        .unwrap();
        let stack = stack(&checks(), &reading);
        assert_eq!(stack.total, u64::MAX);
    }

    fn checks() -> ModFile {
        let (name, file) = builtin::STARTERS
            .iter()
            .find(|(name, _)| *name == "checks")
            .unwrap();
        check::read(name, file).unwrap()
    }

    fn with_body(body: ModBody) -> ModFile {
        ModFile { body, ..checks() }
    }

    #[test]
    fn the_starter_checks_card_draws_as_the_proposal_shows_it() {
        let theme = Theme::dark().with_level(ColorLevel::None);
        let kit = Kit::new(&theme);
        let reading = ModReading::sample(ModBody::Stack);
        let drawn = above(&kit, &checks(), &reading, 66);
        insta::assert_snapshot!(text(&drawn), @r"
         ╭──────────────────────────────────────────────────────────╮ [-]
         │ ◆ checks                                   7 of 9 passed │
         │ ████████████████████████████████████████████████████████ │
         │                                                          │
         │ ▌ passed   7   78%      ▌ running   1   11%              │
         │ ▌ failed   1   11%                                       │
         ╰──────────────────────────────────────────────────────────╯
        ");
    }

    #[test]
    fn every_body_draws_in_every_form_at_any_width() {
        let theme = Theme::dark();
        let kit = Kit::new(&theme);
        for body in ModBody::ALL {
            let file = with_body(body);
            let reading = ModReading::sample(body);
            for form in ModForm::ALL {
                for screen in [24, 40, 80, 120, 200] {
                    let file = ModFile {
                        form,
                        ..file.clone()
                    };
                    let drawn = above(&kit, &file, &reading, screen);
                    assert!(!drawn.lines.is_empty(), "{body} {form} {screen}");
                    assert!(
                        drawn.width() <= screen,
                        "{body} {form} {screen}: {}",
                        drawn.width()
                    );
                    let tab = on_tab(&kit, &file, &reading, screen);
                    assert!(tab.width() <= screen, "{body} {form} {screen}");
                }
            }
        }
    }

    #[test]
    fn a_reading_with_nothing_in_it_still_draws() {
        let theme = Theme::dark();
        let kit = Kit::new(&theme);
        for body in ModBody::ALL {
            let drawn = above(&kit, &with_body(body), &ModReading::default(), 60);
            assert!(drawn.height() >= 3, "{body}");
        }
    }

    #[test]
    fn the_file_s_color_wins_over_the_data_s() {
        let mut file = checks();
        file.colors.insert("passed".to_owned(), ModColor::Blue);
        let reading = ModReading::sample(ModBody::Stack);
        assert_eq!(part_role(&file, &reading.parts[0], 0), Role::AgentBlue);
        assert_eq!(part_role(&file, &reading.parts[2], 2), Role::Danger);
        let plain = ModPart::new("other", 1.0);
        assert_eq!(part_role(&file, &plain, 1), Role::AgentPurple);
    }

    #[test]
    fn one_row_keeps_the_badge_and_drops_the_key_first() {
        let theme = Theme::dark().with_level(ColorLevel::None);
        let kit = Kit::new(&theme);
        let reading = ModReading::sample(ModBody::Stack);
        let row = |width| -> String {
            line(&kit, &checks(), &reading, width, Some(Toggle::Folded))
                .iter()
                .map(|s| s.content.as_ref())
                .collect()
        };
        assert!(row(90).contains("ctrl+x k"), "{}", row(90));
        assert!(row(90).ends_with("[1 failed] [+]"), "{}", row(90));
        assert!(!row(50).contains("ctrl+x k"), "{}", row(50));
        assert!(row(50).ends_with("[1 failed] [+]"), "{}", row(50));
        assert!(row(30).ends_with("[1 failed] [+]"), "{}", row(30));
        for width in [30, 50, 90] {
            assert_eq!(crate::text::str_width(&row(width)), width);
        }
    }

    #[test]
    fn a_text_mod_on_the_bottom_line_is_its_words() {
        let theme = Theme::dark().with_level(ColorLevel::None);
        let kit = Kit::new(&theme);
        let words = |file: &ModFile, reading: &ModReading| -> String {
            chip(&kit, file, reading)
                .iter()
                .map(|s| s.content.as_ref())
                .collect()
        };
        let branch = ModReading::from_output("main\n").unwrap();
        assert_eq!(words(&with_body(ModBody::Text), &branch), "main");
        assert_eq!(
            words(&checks(), &ModReading::sample(ModBody::Stack)),
            "checks 7 of 9 passed"
        );
    }

    #[test]
    fn marks_draw_a_square_per_item_and_stop_at_a_screenful() {
        let theme = Theme::dark().with_level(ColorLevel::None);
        let kit = Kit::new(&theme);
        let file = with_body(ModBody::Marks);
        let drawn = marks(&kit, &file, &ModReading::sample(ModBody::Marks), 40);
        assert_eq!(text(&drawn), "# # # # # # # # #");
        let huge = ModReading {
            parts: vec![ModPart::new("all", 1e9)],
            ..ModReading::default()
        };
        let squares: usize = marks(&kit, &file, &huge, 40)
            .lines
            .iter()
            .map(|line| line.spans.iter().filter(|s| s.content == "#").count())
            .sum();
        assert_eq!(squares, MOST_MARKS);
    }

    #[test]
    fn what_a_command_printed_is_cleaned_before_it_is_drawn() {
        let theme = Theme::dark();
        let kit = Kit::new(&theme);
        let reading = ModReading {
            summary: "ok".to_owned(),
            parts: vec![ModPart::new("a\u{1b}[31mb", 1.0)],
            ..ModReading::default()
        };
        let drawn = above(&kit, &checks(), &reading, 60);
        assert!(!text(&drawn).contains('\u{1b}'));
    }

    /// The README's pictures are what the drawer draws, so the page can't drift from the
    /// screen.
    #[test]
    fn the_readme_s_pictures_are_what_the_drawer_draws() {
        let theme = Theme::dark().with_level(ColorLevel::None);
        let kit = Kit::new(&theme);
        let readme = builtin::README;
        let picture_after = |marker: &str| -> String {
            let rest = readme
                .split_once(marker)
                .unwrap_or_else(|| panic!("{marker} isn't in the README"))
                .1;
            let block = rest.split_once("```\n").unwrap().1;
            block.split_once("\n```").unwrap().0.to_owned()
        };
        for body in ModBody::ALL {
            let file = ModFile {
                body,
                title: body.word().to_owned(),
                key: None,
                ..checks()
            };
            let drawn = preview(&kit, &file, &ModReading::sample(body), 62);
            assert_eq!(
                text(&drawn),
                picture_after(&format!("\n`{body}`: ")),
                "{body}"
            );
        }
        let line = ModFile {
            title: "stack".to_owned(),
            key: None,
            form: ModForm::Line,
            ..checks()
        };
        let drawn = preview(&kit, &line, &ModReading::sample(ModBody::Stack), 62);
        assert_eq!(
            text(&drawn),
            picture_after("Any body in the `line` form is one row:")
        );
    }
}
