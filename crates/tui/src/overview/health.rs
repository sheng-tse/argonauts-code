//! The Status tab's health card: every CLI with its models, each model's state on its own, why,
//! and when it's looked at again, then what the states mean. An overload or a server error is
//! about one model, so its row alone turns red while the CLI's other models stay green.

use argonauts_core::status_tab::{CliHealth, HealthRows, ModelStanding, Standing};
use ratatui::text::Span;

use super::cards::cli_role;
use super::shades::ModelColors;
use crate::format::WallClock;
use crate::kit::{Drawn, Frame, Kit, spread};
use crate::text::{spans_width, truncate_spans, wrap_spans};
use crate::theme::Role;

/// The columns' widths in a card with room for all of them: the CLI, its model, the state, and
/// why. When it's checked next takes the rest.
const FULL_COLUMNS: [usize; 4] = [19, 18, 15, 40];

/// Blank cells kept between a column's words and the next column.
const GAP: usize = 2;

/// A test task nobody finished counts as failed after this long, in words.
const SILENT_LIMIT: &str = "out if silent an hour";

const COUNTING_RULE: &str = "An overload or server error counts against the model it came from; \
                             two models out at once take the CLI down.";

/// The card inside `frame`. Times read as `clock` gives them. Each model draws in the shade
/// `shades` ranks it at, the one it has on every tab, and one the ranking hasn't met takes the
/// next after its CLI's others, in the card's own order.
pub fn card(
    health: &HealthRows,
    kit: &Kit<'_>,
    frame: Frame,
    clock: &WallClock,
    shades: ModelColors<'_>,
) -> Drawn {
    let listed = health.clis.iter().flat_map(|cli| {
        let names = cli.models.iter().map(|model| model.name.as_str());
        names.map(|name| (cli.id.as_str(), name))
    });
    let shades = &shades.with_named(listed);
    let inner = frame.inner();
    let heads = Row::of(
        [
            vec![kit.dim("CLI")],
            vec![kit.dim("model")],
            vec![kit.dim("state")],
            vec![kit.dim("why")],
        ],
        vec![kit.dim("next check")],
    );
    let clis: Vec<Vec<Row>> = health
        .clis
        .iter()
        .map(|cli| cli_rows(cli, shades, kit, clock))
        .collect();
    let legend: Vec<Row> = Standing::ALL
        .into_iter()
        .enumerate()
        .map(|(at, state)| legend_row(kit, state, health, at == 0))
        .collect();
    let table = std::iter::once(&heads)
        .chain(clis.iter().flatten())
        .chain(&legend);
    let widths = columns(table, inner);
    let drawn = |row: &Row| truncate_spans(&row.draw(widths), inner);

    let mut body = Drawn::new();
    body.push(spread(
        vec![kit.strong("CLI health")],
        vec![kit.dim("each model on its own · kept across runs")],
        inner,
    ));
    body.push(Vec::new());
    body.push(drawn(&heads));
    if health.clis.is_empty() {
        body.push(Vec::new());
        body.push(vec![
            kit.dim("Doctor hasn't checked the CLIs yet; /doctor checks them."),
        ]);
    }
    for cli in &clis {
        body.push(Vec::new());
        for row in cli {
            body.push(drawn(row));
        }
    }
    body.push(Vec::new());
    body.push(vec![kit.span("─".repeat(inner), Role::Faint)]);
    body.push(Vec::new());
    for row in &legend {
        body.push(drawn(row));
    }
    body.push(Vec::new());
    // A sentence, so a card narrower than it gives it a second row.
    for line in wrap_spans(&[kit.dim(COUNTING_RULE)], inner, &[], &[]) {
        body.push(line.spans);
    }
    frame.draw(kit, body)
}

/// A row of the table before its columns have their widths: each cell with the columns it
/// takes, then the words that follow them. In a row that fills every column those words are
/// when the model is checked next.
struct Row {
    cells: Vec<(Vec<Span<'static>>, usize)>,
    rest: Vec<Span<'static>>,
}

impl Row {
    /// A cell in each column, then what's checked next.
    fn of(cells: [Vec<Span<'static>>; 4], rest: Vec<Span<'static>>) -> Self {
        Self {
            cells: cells.into_iter().map(|spans| (spans, 1)).collect(),
            rest,
        }
    }

    /// The row in columns `widths` wide, each cell cut so [`GAP`] blank cells follow it.
    fn draw(&self, widths: [usize; 4]) -> Vec<Span<'static>> {
        let mut line = Vec::new();
        let mut at = 0;
        for (spans, across) in &self.cells {
            let width: usize = widths.iter().skip(at).take(*across).sum();
            let cut = truncate_spans(spans, width.saturating_sub(GAP));
            let blank = width.saturating_sub(spans_width(&cut));
            line.extend(cut);
            line.push(Span::raw(" ".repeat(blank)));
            at += across;
        }
        line.extend(self.rest.iter().cloned());
        line
    }
}

/// The columns' widths in a card `inner` cells across: [`FULL_COLUMNS`], which leave the last
/// column room for its longest words in a card with room for all of them. A narrower card takes
/// the cells that column is short from the blank ones the others have past their own longest
/// words, the right-most first, so a row is cut only once none are left. The CLIs' names stay
/// where they are.
fn columns<'a>(table: impl Iterator<Item = &'a Row>, inner: usize) -> [usize; 4] {
    let mut longest = [0; 4];
    let mut last = 0;
    for row in table {
        let mut at = 0;
        for (spans, across) in &row.cells {
            if *across == 1 {
                longest[at] = longest[at].max(spans_width(spans));
            }
            at += across;
        }
        if at == FULL_COLUMNS.len() {
            last = last.max(spans_width(&row.rest));
        }
    }
    let mut widths = FULL_COLUMNS;
    let mut short = (FULL_COLUMNS.iter().sum::<usize>() + last).saturating_sub(inner);
    for at in (1..widths.len()).rev() {
        let spare = widths[at].saturating_sub(longest[at] + GAP).min(short);
        widths[at] -= spare;
        short -= spare;
    }
    widths
}

/// A CLI's rows: its dot and name on the first, then a row a model. One that isn't installed is
/// a single quiet row.
fn cli_rows(
    cli: &CliHealth,
    shades: &ModelColors<'_>,
    kit: &Kit<'_>,
    clock: &WallClock,
) -> Vec<Row> {
    let dot = if cli.main { "● " } else { "○ " };
    // What is said of a CLI with no model to list runs on from its name.
    let alone = |name: Vec<Span<'static>>, said: &'static str| {
        vec![Row {
            cells: vec![(name, 1)],
            rest: vec![kit.dim(said)],
        }]
    };
    if !cli.installed {
        let name = vec![kit.dim(dot), kit.dim(cli.name.clone())];
        return alone(name, "not installed");
    }
    let name = vec![
        kit.span(dot, cli_role(&cli.id)),
        kit.strong(cli.name.clone()),
    ];
    if cli.models.is_empty() {
        return alone(name, "no model named yet");
    }
    let mut first = Some(name);
    cli.models
        .iter()
        .map(|model| {
            let style = shades.style_named(kit, &cli.id, &model.name);
            Row::of(
                [
                    first.take().unwrap_or_default(),
                    vec![Span::styled(model.name.clone(), style)],
                    state_spans(kit, model.state),
                    why_spans(model, shades, kit),
                ],
                vec![kit.dim(next_check(model, clock))],
            )
        })
        .collect()
}

/// A state with what it means and when a model in it is looked at again. The legend's title
/// stands before the first, across the CLIs' and the models' columns.
fn legend_row(kit: &Kit<'_>, state: Standing, health: &HealthRows, titled: bool) -> Row {
    let title = if titled {
        vec![kit.strong("what the states mean")]
    } else {
        Vec::new()
    };
    let (meaning, checked) = meaning(state, health);
    Row {
        cells: vec![
            (title, 2),
            (state_spans(kit, state), 1),
            (vec![kit.span(meaning, Role::Text)], 1),
        ],
        rest: vec![kit.dim(checked)],
    }
}

/// Why a model stands where it does, or else the job it does for a model that is out:
/// `↷ review:due-tests for Opus 5.5`, that model's name in the colour of its own row, or its
/// CLI's colour for one neither the card nor the ranking knows.
fn why_spans(model: &ModelStanding, shades: &ModelColors<'_>, kit: &Kit<'_>) -> Vec<Span<'static>> {
    match &model.covers {
        Some(covers) if model.why.is_empty() => vec![
            kit.dim(format!("↷ {} for ", covers.job)),
            Span::styled(
                covers.model.clone(),
                shades.style_named(kit, &covers.cli, &covers.model),
            ),
        ],
        _ => vec![kit.dim(model.why.clone())],
    }
}

/// `▪ overloaded` in the state's colour.
fn state_spans(kit: &Kit<'_>, state: Standing) -> Vec<Span<'static>> {
    let role = match state {
        Standing::Healthy => Role::Success,
        Standing::Degraded => Role::Warn,
        Standing::Overloaded | Standing::Down => Role::Danger,
        Standing::Testing => Role::Accent,
    };
    vec![kit.span(format!("▪ {}", state.word()), role)]
}

/// `2:03 PM, a test task` for a model that is out, `2:01 PM, if quiet` for one that is degraded.
fn next_check(model: &ModelStanding, clock: &WallClock) -> String {
    let Some(at) = model.next_check else {
        return String::new();
    };
    let what = match model.state {
        Standing::Overloaded | Standing::Down => "a test task",
        Standing::Degraded => "if quiet",
        Standing::Healthy | Standing::Testing => return String::new(),
    };
    format!("{}, {what}", clock.clock(at))
}

/// What a state means, and when a model in it is looked at again.
fn meaning(state: Standing, health: &HealthRows) -> (&'static str, String) {
    let test = format!("a test in {}", span_words(health.cooldown_secs));
    match state {
        Standing::Healthy => ("takes work as asked", String::new()),
        Standing::Degraded => (
            "new work goes to another model first",
            match health.quiet_secs {
                Some(quiet) => format!("after {}", quiet_words(quiet)),
                None => String::new(),
            },
        ),
        Standing::Overloaded => ("out: a fallback steps in, or it waits", test),
        Standing::Down => ("every model on the CLI is out", test),
        Standing::Testing => (
            "one test task decides: healthy or out",
            SILENT_LIMIT.to_owned(),
        ),
    }
}

/// `5 minutes`, `a minute`, `90 seconds`.
fn span_words(seconds: u64) -> String {
    match (seconds / 60, seconds % 60) {
        (1, 0) => "a minute".to_owned(),
        (minutes, 0) if minutes > 0 => format!("{minutes} minutes"),
        _ => format!("{seconds} seconds"),
    }
}

/// `2 quiet minutes`, `a quiet minute`, `90 quiet seconds`.
fn quiet_words(seconds: u64) -> String {
    match (seconds / 60, seconds % 60) {
        (1, 0) => "a quiet minute".to_owned(),
        (minutes, 0) if minutes > 0 => format!("{minutes} quiet minutes"),
        _ => format!("{seconds} quiet seconds"),
    }
}

#[cfg(test)]
#[path = "health_tests.rs"]
pub(crate) mod tests;
