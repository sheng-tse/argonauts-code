//! One agent's goal, drawn from an [`AgentGoal`] and nothing else: the squares and the few
//! words its row carries in the workflow view, the panel under the agents there (three columns
//! in one box), and the opened agent's Goal tab, the same four parts as cards at full size.
//!
//! The four parts are the lines that must hold, every try with a bar for what still failed
//! after it, the checks as last run, and what the agent's own CLI says about its goal.

use argonauts_core::agent_goal::{AgentGoal, GoalCheck, GoalKeeper, GoalLine, GoalState, GoalTry};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::format;
use crate::kit::frame::Frame;
use crate::kit::{Drawn, Kit, bars, fit, spread};
use crate::text::{sanitize_line, spans_width, wrap_spans};
use crate::theme::{Role, Theme};

use super::columns::{self, Align, Cell, Column, Give};
use super::panes::{self, Marks};

/// Cells of a try's bar: in the panel, and on the Goal tab.
const PANEL_BAR: usize = 4;
const TAB_BAR: usize = 6;

/// The panel has three columns from this width, and two from the next.
const THREE_COLUMNS: usize = 96;
const TWO_COLUMNS: usize = 60;

/// The Goal tab puts the checks beside what the CLI says from this width.
const SIDE_BY_SIDE: usize = 100;

fn clean(text: &str) -> String {
    sanitize_line(text).trim().to_owned()
}

/// `1 line`, `3 lines`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// `45s`, `5m`, `1h12m`: time as a sentence says it, with no seconds past the first minute.
fn about(ms: u64) -> String {
    let secs = ms / 1000;
    let (hours, minutes) = (secs / 3600, secs / 60 % 60);
    if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{secs}s")
    }
}

/// `1 try`, `3 tries`.
fn tries(n: usize) -> String {
    if n == 1 {
        "1 try".to_owned()
    } else {
        format!("{n} tries")
    }
}

/// `2 of 3 lines met`.
fn lines_met(goal: &AgentGoal) -> String {
    let total = goal.lines.len();
    let noun = if total == 1 { "line" } else { "lines" };
    format!("{} of {total} {noun} met", goal.lines_met())
}

/// What an agent's row says of its goal: `try 3 · 2 of 3 lines met`, `goal met in 2 tries`.
pub fn row_words(goal: &AgentGoal) -> String {
    let made = goal.tries.len();
    let try_number = goal.current_try();
    match goal.state {
        GoalState::Met => format!("goal met in {}", tries(made.max(1))),
        GoalState::Running if try_number == 0 => format!("goal set · {}", lines_met(goal)),
        GoalState::Running => format!("try {try_number} · {}", lines_met(goal)),
        GoalState::Paused => format!("goal paused · {}", lines_met(goal)),
        GoalState::Stuck => format!("stuck on try {try_number} · {}", lines_met(goal)),
        GoalState::Cleared => "goal cleared".to_owned(),
    }
}

/// The same in a narrow pane, after the squares that already count the lines: `try 3 · 2/3`,
/// `met`.
pub fn row_words_brief(goal: &AgentGoal) -> String {
    let try_number = goal.current_try();
    let met = format!("{}/{}", goal.lines_met(), goal.lines.len());
    match goal.state {
        GoalState::Met => "met".to_owned(),
        GoalState::Running if try_number == 0 => format!("set · {met}"),
        GoalState::Running => format!("try {try_number} · {met}"),
        GoalState::Paused => format!("paused · {met}"),
        GoalState::Stuck => format!("stuck · {met}"),
        GoalState::Cleared => "cleared".to_owned(),
    }
}

/// One square per line: green once met, red while it fails, dim before its first check.
/// Without color the fill tells them apart instead.
pub fn squares(goal: &AgentGoal, theme: &Theme) -> Vec<Span<'static>> {
    let ascii = theme.glyphs.prompt.is_ascii();
    // The small squares are one cell in every font. The large ones are drawn two cells wide
    // in some, where a row of them overlaps.
    let (full, hollow) = if ascii { ("#", "-") } else { ("▪", "▫") };
    goal.lines
        .iter()
        .map(|line| match line.met {
            Some(true) => Span::styled(full, theme.style(Role::Success)),
            Some(false) if theme.has_color() => Span::styled(full, theme.style(Role::Danger)),
            Some(false) => Span::styled(hollow, theme.style(Role::Danger)),
            None => Span::styled(hollow, theme.style(Role::Dim)),
        })
        .collect()
}

/// `kept by Codex's own goal · checked by Argonauts Code`, the keeper in bold.
pub fn kept_by(goal: &AgentGoal, theme: &Theme) -> Vec<Span<'static>> {
    let dim = theme.style(Role::Dim);
    let mut spans = vec![
        Span::styled("kept by ", dim),
        Span::styled(
            clean(&goal.kept_by.words()),
            theme.style(Role::Bright).add_modifier(Modifier::BOLD),
        ),
    ];
    // Where Argonauts Code keeps the goal, saying it checks it too would say it twice.
    let checker = clean(&goal.checked_by);
    if !checker.is_empty() && checker != clean(&goal.kept_by.words()) {
        spans.push(Span::styled(format!(" · checked by {checker}"), dim));
    }
    spans
}

/// Why a goal that isn't going stopped short: `Stuck · the same failure 3 rounds running`.
fn stopped_words(goal: &AgentGoal) -> Option<String> {
    let state = match goal.state {
        GoalState::Stuck => "Stuck",
        GoalState::Paused => "Paused",
        GoalState::Cleared => "Cleared",
        GoalState::Running | GoalState::Met => return None,
    };
    let why = goal.stopped_because.as_deref().map(clean)?;
    (!why.is_empty()).then(|| format!("{state} · {why}"))
}

/// Said while the checks you asked for run beside the try under way.
const CHECKING_NOW: &str = "Its checks are running now";

/// A line's mark: `✓` met, `✗` not, `·` before its first check.
fn line_mark(line: &GoalLine, theme: &Theme) -> Span<'static> {
    let marks = Marks::for_theme(theme);
    match line.met {
        Some(true) => Span::styled(marks.done, theme.style(Role::Success)),
        Some(false) => Span::styled(marks.failed, theme.style(Role::Danger)),
        None => Span::styled(marks.phase_waiting, theme.style(Role::Dim)),
    }
}

/// The most that still failed after any try, which a full bar stands for.
fn worst(goal: &AgentGoal) -> u32 {
    goal.tries
        .iter()
        .filter_map(|a_try| a_try.failing)
        .max()
        .unwrap_or(0)
}

/// How a try's count reads: red while nothing has improved on the worst, amber once fewer
/// fail, green at none.
fn failing_role(failing: u32, worst: u32) -> Role {
    match failing {
        0 => Role::Success,
        n if n >= worst => Role::Danger,
        _ => Role::Warn,
    }
}

/// A try's bar, `cells` wide: what still failed after it against the worst try. A line meter,
/// since the tries sit on rows one under another.
fn try_bar(a_try: &GoalTry, worst: u32, cells: usize, kit: &Kit<'_>) -> Vec<Span<'static>> {
    match a_try.failing {
        Some(failing) => {
            let ratio = f64::from(failing) / f64::from(worst.max(1));
            bars::line_meter(kit, ratio, cells, failing_role(failing, worst), None)
        }
        None => vec![Span::raw(" ".repeat(cells))],
    }
}

/// `no limit on tries`, or `up to 5 tries`.
fn tries_limit(goal: &AgentGoal) -> String {
    match goal.max_tries {
        Some(max) => format!("up to {}", tries(max as usize)),
        None => "no limit on tries".to_owned(),
    }
}

/// What the tries' counts count, such as `failing tests`.
fn counting(goal: &AgentGoal) -> String {
    match clean(&goal.counting) {
        counting if counting.is_empty() => "lines not met".to_owned(),
        counting => counting,
    }
}

/// A check's result: the outcome in its color, then the rest dim.
fn result_spans(check: &GoalCheck, theme: &Theme) -> Vec<Span<'static>> {
    let result = clean(&check.result);
    let (outcome, rest) = match result.split_once(" · ") {
        Some((outcome, rest)) => (outcome.to_owned(), format!(" · {rest}")),
        None => (result, String::new()),
    };
    let role = match check.passed {
        Some(true) => Role::Success,
        Some(false) => Role::Danger,
        None => Role::Dim,
    };
    let outcome = match (outcome.is_empty(), check.passed) {
        (false, _) => outcome,
        (true, Some(true)) => "passed".to_owned(),
        (true, Some(false)) => "failed".to_owned(),
        (true, None) => "running".to_owned(),
    };
    vec![
        Span::styled(outcome, theme.style(role)),
        Span::styled(rest, theme.style(Role::Dim)),
    ]
}

fn wrapped(spans: &[Span<'static>], width: usize, indent: usize) -> Vec<Line<'static>> {
    let pad = [Span::raw(" ".repeat(indent))];
    wrap_spans(spans, width.max(indent + 1), &pad, &pad)
}

fn dim_line(text: impl Into<String>, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(text.into(), theme.style(Role::Dim)))
}

/// The panel's title on its box: `◆ Goal of build:cli`.
pub fn panel_title(goal: &AgentGoal, theme: &Theme) -> String {
    format!("{} Goal of {}", theme.glyphs.question, clean(&goal.label))
}

/// The panel's rows, `width` cells wide and at most `height` of them: the lines, the tries, and
/// the last check with what the CLI says, side by side. A narrow panel keeps the lines and the
/// tries, and a narrower one the lines alone.
pub fn panel(goal: &AgentGoal, width: usize, height: usize, theme: &Theme) -> Vec<Line<'static>> {
    let gap = 3;
    let widths: Vec<usize> = if width >= THREE_COLUMNS {
        // The tries' heading, `Tries · failing tests after each`, is what sets their share.
        let lines = (width - 2 * gap) * 37 / 100;
        let tries = (width - 2 * gap) * 33 / 100;
        vec![lines, tries, width - 2 * gap - lines - tries]
    } else if width >= TWO_COLUMNS {
        let lines = (width - gap) * 11 / 20;
        vec![lines, width - gap - lines]
    } else {
        vec![width]
    };
    let mut columns = vec![lines_column(goal, widths[0], theme)];
    if let Some(width) = widths.get(1) {
        columns.push(tries_column(goal, *width, height, theme));
    }
    if let Some(width) = widths.get(2) {
        columns.push(check_column(goal, *width, theme));
    }
    let rows = columns.iter().map(Vec::len).max().unwrap_or(0).min(height);
    // A column cut short says where the rest is.
    for column in &mut columns {
        if column.len() > height && height > 0 {
            column.truncate(height);
            column[height - 1] = dim_line("… the rest is on its Goal tab", theme);
        }
    }
    (0..rows)
        .map(|row| {
            let mut spans = Vec::new();
            for (i, (column, width)) in columns.iter().zip(&widths).enumerate() {
                if i > 0 {
                    spans.push(Span::raw(" ".repeat(gap)));
                }
                let cell = column.get(row).map(|line| line.spans.clone());
                spans.extend(fit(cell.unwrap_or_default(), *width));
            }
            Line::from(spans)
        })
        .collect()
}

/// `Until all of these hold`, then each line with its mark, and after it what checks it or
/// what still fails.
fn lines_column(goal: &AgentGoal, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut rows = vec![dim_line("Until all of these hold", theme)];
    for line in &goal.lines {
        let mut text = vec![Span::styled(clean(&line.text), theme.style(Role::Bright))];
        match (line.met, line.failing.as_deref().map(clean)) {
            (Some(false), Some(failing)) if !failing.is_empty() => text.push(Span::styled(
                format!(" · {failing}"),
                theme.style(Role::Danger),
            )),
            _ if !line.checked_by.trim().is_empty() => text.push(Span::styled(
                format!(" · {}", clean(&line.checked_by)),
                theme.style(Role::Dim),
            )),
            _ => {}
        }
        let first = [line_mark(line, theme), Span::raw("  ")];
        let rest = [Span::raw("   ")];
        rows.extend(wrap_spans(&text, width.max(4), &first, &rest));
    }
    if let Some(stopped) = stopped_words(goal) {
        let words = [Span::styled(stopped, theme.style(Role::Warn))];
        rows.extend(wrapped(&words, width, 0));
    }
    rows
}

/// `Tries · failing tests after each`, each try with its bar and count, and under them the
/// work so far. Where the tries don't fit in `height` rows the newest stay, the running one
/// among them, and the earlier ones fold into one row.
fn tries_column(
    goal: &AgentGoal,
    width: usize,
    height: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let kit = Kit::new(theme);
    let worst = worst(goal);
    let mut rows = vec![dim_line(
        format!("Tries · {} after each", counting(goal)),
        theme,
    )];
    // The number, the words, the bar, and the count: `3  handling tasks…  ━━── 1`.
    let count_cells = worst.to_string().len();
    let fixed = 3 + 2 + PANEL_BAR + 1 + count_cells;
    let words_width = width.saturating_sub(fixed).max(8);
    let mut tries: Vec<Vec<Line<'static>>> = Vec::new();
    for a_try in &goal.tries {
        let current = a_try.running;
        let number_role = if current { Role::Question } else { Role::Dim };
        let words_role = if current {
            Role::Question
        } else {
            Role::Bright
        };
        let words = [Span::styled(clean(&a_try.did), theme.style(words_role))];
        let first = [Span::styled(
            format!("{:<3}", a_try.number),
            theme.style(number_role),
        )];
        let rest = [Span::raw("   ")];
        let mut wrapped = wrap_spans(&words, words_width + 3, &first, &rest);
        if let Some(top) = wrapped.first_mut() {
            let mut spans = fit(std::mem::take(&mut top.spans), words_width + 3);
            spans.push(Span::raw("  "));
            spans.extend(try_bar(a_try, worst, PANEL_BAR, &kit));
            if let Some(failing) = a_try.failing {
                spans.push(Span::styled(
                    format!(" {failing}"),
                    theme.style(failing_role(failing, worst)),
                ));
            }
            top.spans = spans;
        }
        tries.push(wrapped);
    }
    let so_far = format!(
        "{} of work · {} tokens · {}",
        about(goal.work_ms),
        format::tokens(goal.tokens),
        tries_limit(goal)
    );
    let so_far = wrapped_dim(&so_far, width, theme);
    if tries.is_empty() {
        rows.push(dim_line("No try yet", theme));
    }
    let room = height.saturating_sub(rows.len() + so_far.len());
    let all_rows: usize = tries.iter().map(Vec::len).sum();
    if all_rows > room && tries.len() > 1 {
        // The newest that fit beside the row that names the rest, and always the newest one.
        let mut kept = 0;
        let mut used = 1;
        for lines in tries.iter().rev() {
            if kept > 0 && used + lines.len() > room {
                break;
            }
            used += lines.len();
            kept += 1;
        }
        let earlier = tries.len() - kept;
        if earlier > 0 {
            let noun = if earlier == 1 { "try" } else { "tries" };
            rows.push(dim_line(
                format!("… {earlier} earlier {noun} on its Goal tab"),
                theme,
            ));
            tries.drain(..earlier);
        }
    }
    rows.extend(tries.into_iter().flatten());
    rows.extend(so_far);
    rows
}

fn wrapped_dim(text: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    wrapped(
        &[Span::styled(text.to_owned(), theme.style(Role::Dim))],
        width,
        0,
    )
}

/// `Last check · cargo test overdue` with its result and what it printed, then what the CLI
/// says of its goal, or that Argonauts Code keeps it.
fn check_column(goal: &AgentGoal, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    if goal.checking {
        rows.extend(wrapped_dim(CHECKING_NOW, width, theme));
    }
    match goal.telling_check() {
        Some(check) => {
            rows.extend(wrapped_dim(
                &format!("Last check · {}", clean(&check.name)),
                width,
                theme,
            ));
            rows.extend(wrapped(&result_spans(check, theme), width, 0));
            // One row each, cut when long: the Goal tab has every word.
            for detail in &check.detail {
                rows.push(dim_line(super::layout::cut(&clean(detail), width), theme));
            }
        }
        None if goal.checking => {}
        None => rows.push(dim_line("Not checked yet", theme)),
    }
    rows.push(Line::default());
    rows.extend(cli_rows(goal, width, theme));
    rows
}

/// `Codex says` and its own account, in the panel's one column.
fn cli_rows(goal: &AgentGoal, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let Some(says) = &goal.cli_says else {
        return wrapped_dim(&no_account(goal), width, theme);
    };
    let mut rows = vec![dim_line(format!("{} says", clean(&says.cli)), theme)];
    let mut words = vec![Span::styled(
        format!("goal {}", clean(&says.state)),
        theme.style(Role::Bright).add_modifier(Modifier::BOLD),
    )];
    if let Some(note) = says.note.as_deref().map(clean).filter(|n| !n.is_empty()) {
        words.push(Span::styled(
            format!(" · \"{}\"", note.trim_matches('"')),
            theme.style(Role::Dim),
        ));
    }
    rows.extend(wrapped(&words, width, 0));
    rows
}

/// Why there is no word from the CLI: it has no goal of its own, or it reports none.
fn no_account(goal: &AgentGoal) -> String {
    let how = "it runs the checks after each turn and sends back what still fails";
    let why = goal.kept_note.as_deref().map(clean).unwrap_or_default();
    match &goal.kept_by {
        GoalKeeper::Argonauts if why.is_empty() => format!("Kept by Argonauts Code: {how}"),
        GoalKeeper::Argonauts => format!("{why}, so Argonauts Code keeps it: {how}"),
        GoalKeeper::Cli { name } => format!("{} reports nothing about its goal", clean(name)),
    }
}

/// A card's first row: `◆ title` in bold at the left, and `right` at the right end.
fn card_head(
    title: &str,
    glyph: Role,
    right: Vec<Span<'static>>,
    width: usize,
    kit: &Kit<'_>,
) -> Vec<Span<'static>> {
    let left = vec![
        kit.span(kit.glyphs.the_mod, glyph),
        Span::raw(" "),
        kit.strong(title.to_owned()),
    ];
    let room = width.saturating_sub(spans_width(&left) + 2);
    spread(left, crate::text::truncate_spans(&right, room), width)
}

/// The Goal tab: the goal, its tries, and the checks beside what the CLI says, each a card.
pub fn tab(goal: &AgentGoal, width: usize, theme: &Theme) -> Drawn {
    let kit = Kit::new(theme);
    let whole = Frame {
        left: 0,
        width,
        toggle: None,
        active: false,
    };
    let mut cards = vec![
        whole.draw(&kit, goal_card(goal, whole.inner(), &kit)),
        whole.draw(&kit, tries_card(goal, whole.inner(), &kit)),
    ];
    if width >= SIDE_BY_SIDE {
        let left = Frame {
            width: (width - 1) * 3 / 5,
            ..whole
        };
        let right = Frame {
            width: width - 1 - left.width,
            ..whole
        };
        cards.push(beside(
            left.draw(&kit, checks_card(goal, left.inner(), &kit)),
            right.draw(&kit, cli_card(goal, right.inner(), &kit)),
            left.width,
        ));
    } else {
        cards.push(whole.draw(&kit, checks_card(goal, whole.inner(), &kit)));
        cards.push(whole.draw(&kit, cli_card(goal, whole.inner(), &kit)));
    }
    crate::kit::stack_cards(cards)
}

/// Two cards on the same rows, one cell apart. The shorter one's column stays blank under it.
fn beside(left: Drawn, right: Drawn, left_width: usize) -> Drawn {
    let rows = left.lines.len().max(right.lines.len());
    let mut out = Drawn::new();
    for row in 0..rows {
        let mut spans = fit(
            left.lines
                .get(row)
                .map(|line| line.spans.clone())
                .unwrap_or_default(),
            left_width,
        );
        if let Some(line) = right.lines.get(row) {
            spans.push(Span::raw(" "));
            spans.extend(line.spans.iter().cloned());
        }
        out.push(spans);
    }
    out
}

/// `◆ the goal`, who keeps it and since when, and each line with what checks it and when it
/// was first met, or what still fails.
fn goal_card(goal: &AgentGoal, width: usize, kit: &Kit<'_>) -> Drawn {
    let theme = kit.theme;
    let dim = theme.style(Role::Dim);
    let mut right = kept_by(goal, theme);
    // The card says who checks beside each line, so the header leaves it out.
    right.truncate(2);
    if let Some(ago) = goal.set_ms_ago {
        right.push(Span::styled(format!(" · set {} ago", about(ago)), dim));
    }
    right.push(Span::styled(format!(" · {}", lines_met(goal)), dim));
    let mut card = Drawn::new();
    card.push(card_head("the goal", Role::Question, right, width, kit));

    let text = |line: &GoalLine| -> Cell { vec![kit.span(clean(&line.text), Role::Bright)] };
    let how = |line: &GoalLine| -> Cell {
        let checker = clean(&line.checked_by);
        let failing = line.failing.as_deref().map(clean).unwrap_or_default();
        match (line.met, line.met_on_try) {
            (Some(false), _) => {
                let words: Vec<String> = [checker, failing]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect();
                vec![kit.span(words.join(" · "), Role::Danger)]
            }
            (Some(true), Some(n)) if !checker.is_empty() => {
                vec![kit.dim(format!("{checker} · met on try {n}"))]
            }
            (Some(true), Some(n)) => vec![kit.dim(format!("met on try {n}"))],
            _ => vec![kit.dim(checker)],
        }
    };
    let texts: Vec<Cell> = goal.lines.iter().map(text).collect();
    let hows: Vec<Cell> = goal.lines.iter().map(how).collect();
    let mut fitted = [
        Column::widest(Align::Left, &texts),
        Column::widest(Align::Left, &hows),
    ];
    // The mark and its three spaces come before the two columns.
    let room = width.saturating_sub(4);
    // Beside each other while both fit whole; else how it's checked goes under its line.
    let side_by_side = columns::used(&fitted) + 2 <= room;
    // A little air between the two, while there's room for it.
    if side_by_side {
        fitted[0].width = fitted[0]
            .width
            .max(room * 11 / 20)
            .min(room - fitted[1].width - 2);
    }
    for ((line, text), how) in goal.lines.iter().zip(&texts).zip(&hows) {
        let opening = vec![line_mark(line, theme), Span::raw("   ")];
        if side_by_side {
            let cells = [text.clone(), how.clone()];
            card.lines
                .push(columns::line(opening, &fitted, &cells, None, width));
        } else {
            let rest = [Span::raw("    ")];
            card.lines.extend(wrap_spans(text, width, &opening, &rest));
            card.lines.extend(wrap_spans(how, width, &rest, &rest));
        }
    }
    if goal.lines.is_empty() {
        card.push(vec![kit.dim("No lines yet")]);
    }
    if let Some(stopped) = stopped_words(goal) {
        card.lines
            .extend(wrapped(&[kit.span(stopped, Role::Warn)], width, 0));
    }
    card
}

/// `◆ tries`, the count and the work so far, and a row per try: what it did, what it changed,
/// a bar and a count for what still failed, and its time and tokens.
fn tries_card(goal: &AgentGoal, width: usize, kit: &Kit<'_>) -> Drawn {
    let worst = worst(goal);
    let limit = match goal.max_tries {
        Some(max) => format!("of up to {max}"),
        None => "no limit set".to_owned(),
    };
    let right = vec![kit.dim(format!(
        "{} so far · {limit} · {} of work · {} tokens",
        goal.tries.len(),
        about(goal.work_ms),
        format::tokens(goal.tokens)
    ))];
    let mut card = Drawn::new();
    card.push(card_head("tries", Role::Question, right, width, kit));
    if goal.tries.is_empty() {
        card.push(vec![kit.dim("No try yet")]);
        return card;
    }
    let noun = counting(goal);
    // `failing tests` counts as `4 failing`: the first word says enough beside a number.
    let noun = noun.split(' ').next().unwrap_or("failing").to_owned();
    let rows: Vec<(Vec<Cell>, Cell)> = goal
        .tries
        .iter()
        .map(|a_try| {
            let role = if a_try.running {
                Role::Question
            } else {
                Role::Bright
            };
            let changed = a_try.changed.filter(|diff| !diff.is_empty()).map(|diff| {
                format!(
                    "+{} -{} in {}",
                    diff.insertions,
                    diff.deletions,
                    count(diff.files_changed as usize, "file")
                )
            });
            let failing = match a_try.failing {
                Some(0) => vec![kit.span("none", Role::Success)],
                Some(n) => vec![kit.span(format!("{n} {noun}"), failing_role(n, worst))],
                None => Vec::new(),
            };
            let time = panes::elapsed(a_try.elapsed_ms);
            let spent = if a_try.running {
                vec![kit.span(format!("{time} · running"), Role::Question)]
            } else {
                vec![kit.dim(format!("{time} · {}", format::tokens(a_try.tokens)))]
            };
            let number_role = if a_try.running {
                Role::Question
            } else {
                Role::Dim
            };
            let cells = vec![
                vec![kit.span(a_try.number.to_string(), number_role)],
                vec![kit.span(clean(&a_try.did), role)],
                vec![kit.dim(changed.unwrap_or_default())],
                try_bar(a_try, worst, TAB_BAR, kit),
                failing,
            ];
            (cells, spent)
        })
        .collect();
    let mut fitted: Vec<Column> = (0..5)
        .map(|i| Column::widest(Align::Left, rows.iter().map(|(cells, _)| &cells[i])))
        .collect();
    // Each try's time and tokens end at the card's edge, padded to the widest of them.
    let spent_width = rows
        .iter()
        .map(|(_, spent)| spans_width(spent))
        .max()
        .unwrap_or(0);
    // The number's column is as wide as the mark's in the goal card, so the words line up.
    fitted[0].width = fitted[0].width.max(2);
    let natural = fitted[1].width;
    let wanted = natural.max(width * 2 / 5);
    fitted[1].width = wanted;
    let room = width.saturating_sub(spent_width + columns::GAP);
    columns::fit(
        &mut fitted,
        room,
        &[
            Give::Cut {
                column: 1,
                min: natural.min(24),
            },
            Give::Drop(2),
            Give::Drop(3),
            Give::Cut { column: 1, min: 8 },
        ],
    );
    // What a column freed by going is the words' again, which were cut before it went.
    let spare = room.saturating_sub(columns::used(&fitted));
    fitted[1].width = (fitted[1].width + spare).min(wanted);
    for (cells, spent) in &rows {
        let spent = fit(spent.clone(), spent_width);
        card.lines.push(columns::line(
            Vec::new(),
            &fitted,
            cells,
            Some(spent),
            width,
        ));
    }
    card
}

/// `◆ checks`, who runs them, and each check with its result and what it printed.
fn checks_card(goal: &AgentGoal, width: usize, kit: &Kit<'_>) -> Drawn {
    let theme = kit.theme;
    let checker = match clean(&goal.checked_by) {
        _ if goal.checking => "running now, as you asked".to_owned(),
        checker if checker.is_empty() => "after each try".to_owned(),
        checker => format!("run by {checker} after each try"),
    };
    let mut card = Drawn::new();
    card.push(card_head(
        "checks",
        Role::Success,
        vec![kit.dim(checker)],
        width,
        kit,
    ));
    if goal.checks.is_empty() {
        let none = if goal.checking {
            CHECKING_NOW
        } else {
            "Not checked yet"
        };
        card.push(vec![kit.dim(none)]);
        return card;
    }
    let names: Vec<Cell> = goal
        .checks
        .iter()
        .map(|check| vec![kit.span(clean(&check.name), Role::Bright)])
        .collect();
    let name = Column::widest(Align::Left, &names).width.min(width / 2);
    let indent = name + 2;
    for (check, cell) in goal.checks.iter().zip(&names) {
        let mut first = fit(cell.clone(), name);
        first.push(Span::raw("  "));
        let rest = [Span::raw(" ".repeat(indent))];
        card.lines.extend(wrap_spans(
            &result_spans(check, theme),
            width,
            &first,
            &rest,
        ));
        for detail in &check.detail {
            card.lines
                .extend(wrapped(&[kit.dim(clean(detail))], width, indent));
        }
    }
    card
}

/// `◆ Codex says`, its own account of its goal: the state, its note, and its turns. Where the
/// CLI has no goal of its own, the card says who keeps it instead.
fn cli_card(goal: &AgentGoal, width: usize, kit: &Kit<'_>) -> Drawn {
    let mut card = Drawn::new();
    let Some(says) = &goal.cli_says else {
        let title = match &goal.kept_by {
            GoalKeeper::Argonauts => "kept by Argonauts Code".to_owned(),
            GoalKeeper::Cli { name } => format!("{} says", clean(name)),
        };
        card.push(card_head(&title, Role::Question, Vec::new(), width, kit));
        card.lines
            .extend(wrapped(&[kit.dim(no_account(goal))], width, 0));
        return card;
    };
    card.push(card_head(
        &format!("{} says", clean(&says.cli)),
        Role::Question,
        vec![kit.dim("its own account")],
        width,
        kit,
    ));
    let mut row = |label: &str, value: Span<'static>| {
        let first = [kit.dim(format!("{label:<8}"))];
        let rest = [Span::raw(" ".repeat(8))];
        card.lines
            .extend(wrap_spans(&[value], width, &first, &rest));
    };
    row(
        "goal",
        Span::styled(clean(&says.state), kit.bold(Role::Bright)),
    );
    if let Some(note) = says.note.as_deref().map(clean).filter(|n| !n.is_empty()) {
        row(
            "note",
            kit.span(format!("\"{}\"", note.trim_matches('"')), Role::Bright),
        );
    }
    if let Some(turns) = says.turns {
        row(
            "turns",
            kit.span(format!("{turns} in this goal"), Role::Bright),
        );
    }
    card
}

/// The goal as a screen reader says it: where it stands and who keeps it, then each line with
/// whether it holds, and the last check.
pub fn spoken(goal: &AgentGoal) -> Vec<String> {
    let mut lines = vec![format!(
        "goal of {}: {}, kept by {}",
        clean(&goal.label),
        row_words(goal),
        clean(&goal.kept_by.words())
    )];
    lines.extend(goal.lines.iter().map(|line| {
        let state = match line.met {
            Some(true) => "met",
            Some(false) => "not met",
            None => "not checked yet",
        };
        let mut said = format!("goal line, {state}: {}", clean(&line.text));
        if let Some(failing) = line.failing.as_deref().filter(|_| line.met == Some(false)) {
            said.push_str(&format!(", {}", clean(failing)));
        }
        said
    }));
    if let Some(stopped) = stopped_words(goal) {
        lines.push(format!(
            "goal {}",
            stopped.replace(" · ", ": ").to_lowercase()
        ));
    }
    if goal.checking {
        lines.push(CHECKING_NOW.to_lowercase());
    }
    if let Some(check) = goal.telling_check() {
        lines.push(format!(
            "last check {}: {}",
            clean(&check.name),
            clean(&check.result)
        ));
    }
    lines
}

#[cfg(test)]
pub(crate) mod fixture {
    use argonauts_core::DiffStat;
    use argonauts_core::agent_goal::CliGoal;
    use argonauts_core::names::SessionKey;

    use super::*;

    fn line(
        text: &str,
        met: bool,
        by: &str,
        on_try: Option<u32>,
        failing: Option<&str>,
    ) -> GoalLine {
        GoalLine {
            text: text.into(),
            met: Some(met),
            checked_by: by.into(),
            met_on_try: on_try,
            failing: failing.map(str::to_owned),
        }
    }

    pub(crate) fn try_number(
        number: u32,
        did: &str,
        diff: (u32, u32, u32),
        failing: u32,
    ) -> GoalTry {
        GoalTry {
            number,
            did: did.into(),
            changed: Some(DiffStat {
                insertions: diff.0,
                deletions: diff.1,
                files_changed: diff.2,
            }),
            failing: Some(failing),
            elapsed_ms: 0,
            tokens: 0,
            running: false,
        }
    }

    /// `build:cli` on its third try: two of three lines met,
    /// one test still failing, kept by Codex's own goal and checked by Argonauts Code.
    pub(crate) fn build_cli(session: &str) -> AgentGoal {
        let mut tries = vec![
            try_number(1, "wrote the command and its tests", (142, 0, 3), 4),
            try_number(2, "fixed the date comparison", (9, 6, 1), 2),
            try_number(3, "handling tasks with no due date", (14, 2, 2), 1),
        ];
        (tries[0].elapsed_ms, tries[0].tokens) = (125_000, 61_000);
        (tries[1].elapsed_ms, tries[1].tokens) = (100_000, 38_000);
        (tries[2].elapsed_ms, tries[2].running) = (137_000, true);
        AgentGoal {
            session: SessionKey::parse(session).unwrap(),
            label: "build:cli".into(),
            role: Some("worker".into()),
            task: None,
            kept_by: GoalKeeper::Cli {
                name: "Codex".into(),
            },
            kept_note: None,
            checked_by: "Argonauts Code".into(),
            state: GoalState::Running,
            stopped_because: None,
            set_ms_ago: Some(362_000),
            lines: vec![
                line(
                    "tasks overdue lists tasks due before today",
                    true,
                    "the judge",
                    Some(2),
                    None,
                ),
                line(
                    "tasks add --due rejects a date that isn't ISO",
                    true,
                    "cargo test due::parse",
                    Some(1),
                    None,
                ),
                line(
                    "every test in tests/overdue.rs passes",
                    false,
                    "cargo test overdue",
                    None,
                    Some("1 of 12 fails"),
                ),
            ],
            tries,
            counting: "failing tests".into(),
            max_tries: None,
            work_ms: 300_000,
            tokens: 143_000,
            checks: vec![
                GoalCheck {
                    name: "cargo test overdue".into(),
                    passed: Some(false),
                    result: "1 failed · 11 passed · 4.1s".into(),
                    detail: vec![
                        "overdue::skips_tasks_without_a_date:".into(),
                        "expected 2 tasks, got 3".into(),
                    ],
                },
                GoalCheck {
                    name: "cargo test due::parse".into(),
                    passed: Some(true),
                    result: "passed · 6 tests".into(),
                    detail: Vec::new(),
                },
                GoalCheck {
                    name: "the judge".into(),
                    passed: Some(true),
                    result: "1 of 1 line met · Astra".into(),
                    detail: Vec::new(),
                },
            ],
            checking: false,
            cli_says: Some(CliGoal {
                cli: "Codex".into(),
                state: "active".into(),
                note: Some("One test left: tasks without a date are still listed.".into()),
                turns: Some(14),
            }),
        }
    }

    /// A goal already met, in two tries.
    pub(crate) fn met(session: &str, label: &str) -> AgentGoal {
        let mut goal = build_cli(session);
        goal.label = label.into();
        goal.state = GoalState::Met;
        goal.lines.truncate(2);
        goal.tries.truncate(2);
        goal.tries[1].failing = Some(0);
        goal
    }

    /// A goal on its first try with nothing met yet, kept by Argonauts Code.
    pub(crate) fn starting(session: &str, label: &str) -> AgentGoal {
        let mut goal = build_cli(session);
        goal.label = label.into();
        goal.kept_by = GoalKeeper::Argonauts;
        goal.cli_says = None;
        goal.lines.truncate(2);
        for line in &mut goal.lines {
            line.met = Some(false);
            line.met_on_try = None;
        }
        goal.tries.truncate(1);
        goal.tries[0].running = true;
        goal.tries[0].failing = None;
        goal.checks.clear();
        goal
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{build_cli, met, starting, try_number};
    use super::*;
    use crate::stability::assert_lines;
    use crate::text::{plain_text, str_width};
    use crate::theme::Glyphs;

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    fn squares_text(goal: &AgentGoal, theme: &Theme) -> String {
        squares(goal, theme)
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    /// Twenty tries in a short box: the running one shows, and the earlier ones fold into one
    /// row that says where they are. With no try yet the column says so.
    #[test]
    fn the_running_try_shows_however_many_came_before() {
        let theme = theme();
        let mut goal = build_cli("s-002");
        goal.tries = (1..=20)
            .map(|number| try_number(number, &format!("try number {number}"), (1, 1, 1), 1))
            .collect();
        goal.tries[19].running = true;
        let text: Vec<String> = panel(&goal, 110, 12, &theme)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        let all = text.join("\n");
        assert!(all.contains("try number 20"), "{all}");
        assert!(all.contains("earlier tries on its Goal tab"), "{all}");
        assert!(!all.contains("try number 1 "), "{all}");

        goal.tries.clear();
        let none: String = panel(&goal, 110, 12, &theme)
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.to_string()))
            .collect();
        assert!(none.contains("No try yet"), "{none}");
    }

    #[test]
    fn a_row_carries_a_square_per_line_its_try_and_the_lines_met() {
        let theme = theme();
        let goal = build_cli("s-002");
        assert_eq!(squares_text(&goal, &theme), "▪▪▪");
        let colors: Vec<_> = squares(&goal, &theme)
            .iter()
            .map(|span| span.style.fg)
            .collect();
        assert_eq!(colors[0], theme.style(Role::Success).fg);
        assert_eq!(colors[2], theme.style(Role::Danger).fg);
        assert_eq!(row_words(&goal), "try 3 · 2 of 3 lines met");
        assert_eq!(
            row_words(&met("s-001", "build:store")),
            "goal met in 2 tries"
        );
        let starting = starting("s-003", "build:docs");
        assert_eq!(row_words(&starting), "try 1 · 0 of 2 lines met");
        assert_eq!(squares_text(&starting, &theme), "▪▪");
    }

    #[test]
    fn a_screen_reader_hears_where_the_goal_stands_and_each_line() {
        assert_eq!(
            spoken(&build_cli("s-002")),
            [
                "goal of build:cli: try 3 · 2 of 3 lines met, kept by Codex's own goal",
                "goal line, met: tasks overdue lists tasks due before today",
                "goal line, met: tasks add --due rejects a date that isn't ISO",
                "goal line, not met: every test in tests/overdue.rs passes, 1 of 12 fails",
                "last check cargo test overdue: 1 failed · 11 passed · 4.1s",
            ]
        );
    }

    #[test]
    fn every_state_of_a_goal_has_its_own_words() {
        let mut goal = build_cli("s-002");
        assert_eq!(row_words_brief(&goal), "try 3 · 2/3");
        goal.state = GoalState::Paused;
        assert_eq!(row_words(&goal), "goal paused · 2 of 3 lines met");
        assert_eq!(row_words_brief(&goal), "paused · 2/3");
        goal.state = GoalState::Stuck;
        assert_eq!(row_words(&goal), "stuck on try 3 · 2 of 3 lines met");
        assert_eq!(row_words_brief(&goal), "stuck · 2/3");
        goal.state = GoalState::Cleared;
        assert_eq!(row_words(&goal), "goal cleared");
        assert_eq!(row_words_brief(&goal), "cleared");
        goal.state = GoalState::Running;
        goal.tries.clear();
        goal.lines.truncate(1);
        assert_eq!(row_words(&goal), "goal set · 1 of 1 line met");
        assert_eq!(row_words_brief(&goal), "set · 1/1");
        goal.state = GoalState::Met;
        assert_eq!(row_words(&goal), "goal met in 1 try");
        assert_eq!(row_words_brief(&goal), "met");
    }

    #[test]
    fn without_color_a_failing_line_is_a_hollow_square() {
        let plain = theme().with_level(crate::theme::ColorLevel::None);
        assert_eq!(squares_text(&build_cli("s-002"), &plain), "▪▪▫");
    }

    #[test]
    fn the_panel_has_the_lines_the_tries_and_the_last_check_side_by_side() {
        let theme = theme();
        let lines = panel(&build_cli("s-002"), 110, 12, &theme);
        assert_lines(&lines, 110);
        let text = plain_text(&lines);
        let rows: Vec<&str> = text.lines().collect();
        assert!(rows[0].starts_with("Until all of these hold"), "{text}");
        assert!(
            rows[0].contains("Tries · failing tests after each"),
            "{text}"
        );
        assert!(
            rows[0].contains("Last check · cargo test overdue"),
            "{text}"
        );
        assert!(
            rows[1].starts_with("✓  tasks overdue lists tasks due"),
            "{text}"
        );
        assert!(rows[2].starts_with("   before today · the judge"), "{text}");
        assert!(rows[1].contains("1  wrote the command"), "{text}");
        assert!(rows[1].contains("━━━━ 4"), "{text}");
        assert!(rows[1].contains("1 failed · 11 passed · 4.1s"), "{text}");
        assert!(text.contains("✗  every test in tests/overdue.rs"), "{text}");
        assert!(text.contains("passes · 1 of 12 fails"), "{text}");
        assert!(text.contains("━─── 1"), "{text}");
        assert!(text.contains("5m of work · 143k tokens · no"), "{text}");
        assert!(text.contains("overdue::skips_tasks_without_a_…"), "{text}");
        assert!(text.contains("expected 2 tasks, got 3"), "{text}");
        assert!(text.contains("Codex says"), "{text}");
        assert!(text.contains("goal active · \"One test left"), "{text}");
    }

    #[test]
    fn a_narrow_panel_keeps_the_lines_and_a_short_one_says_where_the_rest_is() {
        let theme = theme();
        let goal = build_cli("s-002");
        for width in [40, 59, 60, 80, 95, 96, 140] {
            let lines = panel(&goal, width, 20, &theme);
            assert_lines(&lines, width as u16);
            for line in &lines {
                assert!(spans_width(&line.spans) <= width, "{width}");
            }
            let text = plain_text(&lines);
            assert!(text.contains("Until all of these hold"), "{width}: {text}");
            assert_eq!(text.contains("Tries ·"), width >= TWO_COLUMNS, "{width}");
            assert_eq!(
                text.contains("Last check"),
                width >= THREE_COLUMNS,
                "{width}"
            );
        }
        let short = plain_text(&panel(&goal, 110, 3, &theme));
        assert_eq!(short.lines().count(), 3, "{short}");
        assert!(short.contains("… the rest is on its Goal tab"), "{short}");
    }

    #[test]
    fn a_goal_argonauts_keeps_says_so_where_the_cli_would_speak() {
        let theme = theme();
        let goal = starting("s-003", "build:docs");
        let text = plain_text(&panel(&goal, 120, 12, &theme));
        assert!(text.contains("Not checked yet"), "{text}");
        assert!(
            text.contains("Kept by Argonauts Code: it runs the"),
            "{text}"
        );
        let kept: String = kept_by(&goal, &theme)
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(kept, "kept by Argonauts Code");
        let tab = plain_text(&tab(&goal, 120, &theme).lines);
        assert!(tab.contains("◆ kept by Argonauts Code"), "{tab}");
        assert!(tab.contains("Not checked yet"), "{tab}");
    }

    /// Why Argonauts Code keeps a goal that was asked of the CLI is said where the CLI would speak,
    /// a goal that stopped short says why under its lines, and checks you asked for show as
    /// running, in the panel, on the Goal tab and to a screen reader.
    #[test]
    fn a_goal_says_why_argonauts_keeps_it_why_it_stopped_and_that_its_checks_run() {
        let theme = theme();
        let mut goal = starting("s-003", "build:docs");
        goal.kept_note = Some("Claude Code has no goal of its own".into());
        goal.checking = true;
        let said = plain_text(&panel(&goal, 120, 12, &theme));
        assert!(
            said.contains("Claude Code has no goal of its own,"),
            "{said}"
        );
        assert!(said.contains("so Argonauts Code keeps it:"), "{said}");
        assert!(said.contains("Its checks are running now"), "{said}");
        assert!(!said.contains("Not checked yet"), "{said}");
        let card = plain_text(&tab(&goal, 120, &theme).lines);
        assert!(card.contains("running now, as you asked"), "{card}");
        assert!(card.contains("Its checks are running now"), "{card}");
        assert!(
            spoken(&goal).contains(&"its checks are running now".to_owned()),
            "{:?}",
            spoken(&goal)
        );

        let mut stuck = build_cli("s-002");
        stuck.state = GoalState::Stuck;
        stuck.stopped_because = Some("the same failure 3 rounds running".into());
        let why = "Stuck · the same failure 3 rounds running";
        let said = plain_text(&panel(&stuck, 120, 14, &theme));
        assert!(said.contains("Stuck · the same failure"), "{said}");
        let card = plain_text(&tab(&stuck, 120, &theme).lines);
        assert!(card.contains(why), "{card}");
        assert!(
            spoken(&stuck).contains(&"goal stuck: the same failure 3 rounds running".to_owned()),
            "{:?}",
            spoken(&stuck)
        );
        // A goal still going says nothing of stopping, whatever was left in the field.
        stuck.state = GoalState::Running;
        assert!(!plain_text(&tab(&stuck, 120, &theme).lines).contains("Stuck"));
    }

    #[test]
    fn the_goal_tab_draws_the_four_parts_as_cards() {
        let theme = theme();
        let drawn = tab(&build_cli("s-002"), 118, &theme);
        assert_lines(&drawn.lines, 118);
        let text = plain_text(&drawn.lines);
        let rows: Vec<&str> = text.lines().collect();
        assert!(rows[0].starts_with('╭') && rows[0].ends_with('╮'), "{text}");
        assert!(rows[1].starts_with("│ ◆ the goal"), "{text}");
        assert!(
            rows[1].ends_with("kept by Codex's own goal · set 6m ago · 2 of 3 lines met │"),
            "{text}"
        );
        assert!(
            rows[2].starts_with("│ ✓   tasks overdue lists tasks due before today"),
            "{text}"
        );
        assert!(rows[2].contains("the judge · met on try 2"), "{text}");
        assert!(
            rows[3].contains("cargo test due::parse · met on try 1"),
            "{text}"
        );
        assert!(
            rows[4].starts_with("│ ✗   every test in tests/overdue.rs passes"),
            "{text}"
        );
        assert!(
            rows[4].contains("cargo test overdue · 1 of 12 fails"),
            "{text}"
        );
        // One blank row between cards, never two.
        assert!(
            rows[6].trim().is_empty() && !rows[7].trim().is_empty(),
            "{text}"
        );
        assert!(rows[8].starts_with("│ ◆ tries"), "{text}");
        assert!(
            rows[8].ends_with("3 so far · no limit set · 5m of work · 143k tokens │"),
            "{text}"
        );
        assert!(
            rows[9].contains("1   wrote the command and its tests"),
            "{text}"
        );
        assert!(rows[9].contains("+142 -0 in 3 files"), "{text}");
        assert!(rows[9].contains("━━━━━━  4 failing"), "{text}");
        assert!(rows[9].trim_end().ends_with("2m05s · 61k     │"), "{text}");
        assert!(rows[10].contains("+9 -6 in 1 file "), "{text}");
        assert!(rows[10].contains("━━━───  2 failing"), "{text}");
        assert!(rows[11].contains("━╸────  1 failing"), "{text}");
        assert!(rows[11].ends_with("2m17s · running │"), "{text}");
        // The checks and the CLI's own account share their rows.
        let heads = rows.iter().find(|row| row.contains("◆ checks")).unwrap();
        assert!(
            heads.contains("run by Argonauts Code after each try"),
            "{text}"
        );
        assert!(heads.contains("◆ Codex says"), "{text}");
        assert!(heads.trim_end().ends_with("its own account │"), "{text}");
        assert!(
            text.contains("cargo test overdue     1 failed · 11 passed · 4.1s"),
            "{text}"
        );
        assert!(
            text.contains("cargo test due::parse  passed · 6 tests"),
            "{text}"
        );
        assert!(
            text.contains("the judge              1 of 1 line met · Astra"),
            "{text}"
        );
        assert!(text.contains("goal    active"), "{text}");
        assert!(text.contains("turns   14 in this goal"), "{text}");
        for row in &rows {
            assert!(str_width(row) <= 118, "{row}");
        }
    }

    #[test]
    fn the_goal_tab_stacks_its_cards_when_narrow() {
        let theme = theme();
        for width in [50, 72, 99] {
            let drawn = tab(&build_cli("s-002"), width, &theme);
            assert_lines(&drawn.lines, width as u16);
            let text = plain_text(&drawn.lines);
            for row in text.lines() {
                assert!(str_width(row) <= width, "{width}: {row}");
            }
            let checks = text
                .lines()
                .position(|row| row.contains("◆ checks"))
                .unwrap();
            let says = text
                .lines()
                .position(|row| row.contains("◆ Codex says"))
                .unwrap();
            assert!(says > checks, "{width}: {text}");
            assert!(
                text.contains("tasks overdue lists tasks"),
                "{width}: {text}"
            );
        }
    }

    /// At 80 columns what each try changed gives way, and the room it leaves goes to what the
    /// try did, which reads whole again.
    #[test]
    fn what_a_try_did_takes_the_room_a_column_that_went_leaves() {
        let drawn = tab(&build_cli("s-002"), 80, &theme());
        let text = plain_text(&drawn.lines);
        assert!(!text.contains("in 3 files"), "{text}");
        assert!(
            text.contains("1   wrote the command and its tests  ━━━━━━  4 failing"),
            "{text}"
        );
        assert!(
            text.contains("3   handling tasks with no due date  ━╸────  1 failing"),
            "{text}"
        );
    }
}
