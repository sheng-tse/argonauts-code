//! What the agent panel draws: the title row with plan usage, the state tabs, the grouped list, the
//! details of the highlighted row, and the keys.

use argonauts_core::agent_panel::{
    AgentPanelRows, CrewDetails, CrewRow, CrewRun, CrewState, CrewTab, CrewTarget, RepoGroup,
};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::limits::{PlanUsageReading, PlanUsageStatus};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use time::OffsetDateTime;

use crate::bridge::status::label_for_agent;
use crate::format;
use crate::state::{AppState, binding_reading};
use crate::text::{str_width, truncate_line, wrap_text};
use crate::theme::{Role, Theme};

use super::super::layout::{cut, cut_middle, left};
use super::super::list::Column;
use super::super::panes::{self, Marks, key_names, pad_glyph};
use super::super::resume::ago;
use super::rows::clean;

/// Cells between columns.
pub(super) const GAP: usize = 2;

/// The name column keeps at least this many cells, enough for a label such as `build:add-due`.
const NAME_MIN: usize = 16;

/// A run's name keeps at least this many cells before the words after it are cut.
const RUN_NAME_MIN: usize = 12;

/// Lines of a long message the details keep before cutting.
const MESSAGE_LINES: usize = 6;
const TITLE_LINES: usize = 3;

/// The field names in the details, padded so the values line up.
const FIELD: usize = 9;

/// One list line, and the row it highlights when it's a row.
pub(super) struct BodyLine {
    pub line: Line<'static>,
    pub row: Option<usize>,
}

impl BodyLine {
    fn text(line: Line<'static>) -> Self {
        Self { line, row: None }
    }
}

pub(super) fn state_word(state: CrewState) -> &'static str {
    match state {
        CrewState::NeedsYou => "Needs you",
        CrewState::Working => "Working",
        CrewState::Waiting => "Waiting",
        CrewState::Ready => "Ready",
        CrewState::Done => "Done",
        CrewState::Stopped => "Stopped",
        CrewState::Failed => "Failed",
        CrewState::Skipped => "Skipped",
    }
}

/// A row's glyph and color: shape and color both say its state.
pub(super) fn glyph(state: CrewState, sleeping: bool, theme: &Theme) -> (&'static str, Role) {
    let marks = Marks::for_theme(theme);
    match state {
        CrewState::NeedsYou => (marks.needs_you, Role::Question),
        CrewState::Working => (marks.running, Role::Accent),
        CrewState::Waiting if sleeping => (theme.glyphs.agent_timer, Role::Dim),
        CrewState::Waiting => (marks.waiting, Role::Dim),
        // Lavender marks a conversation you can carry on.
        CrewState::Ready => (ready_glyph(theme), Role::Question),
        CrewState::Done => (marks.done, Role::Success),
        CrewState::Stopped => (marks.stopped, Role::Dim),
        CrewState::Failed => (marks.failed, Role::Danger),
        CrewState::Skipped => (marks.skipped, Role::Dim),
    }
}

/// Ready takes more input, like an idle prompt: a ring apart from waiting's `○`.
fn ready_glyph(theme: &Theme) -> &'static str {
    if theme.glyphs.prompt.is_ascii() {
        "O"
    } else {
        "◎"
    }
}

/// `argonauts › crew` at the left and the plan usage at the right, the plan usage giving way first.
pub(super) fn title_line(state: &AppState, width: usize, theme: &Theme) -> Line<'static> {
    let marks = Marks::for_theme(theme);
    let title = format!("argonauts {} crew", marks.crumb);
    let title_width = str_width(&title);
    let mut spans = vec![Span::styled(title, theme.style(Role::Label))];
    let mut parts = plan_usage_parts(state);
    let joined = |parts: &[(String, Role)]| {
        parts.iter().map(|(text, _)| str_width(text)).sum::<usize>()
            + 3 * parts.len().saturating_sub(1)
    };
    while !parts.is_empty() && title_width + GAP + joined(&parts) > width {
        parts.pop();
    }
    if parts.is_empty() {
        return Line::from(spans);
    }
    spans.push(Span::raw(" ".repeat(width - title_width - joined(&parts))));
    for (i, (text, role)) in parts.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", theme.style(Role::Dim)));
        }
        spans.push(Span::styled(text, theme.style(role)));
    }
    Line::from(spans)
}

/// `Claude 39%` for each agent with a reading, in the bottom line's order.
fn plan_usage_parts(state: &AppState) -> Vec<(String, Role)> {
    let chips: Vec<PlanUsageReading> = state
        .status
        .iter()
        .flat_map(|status| &status.plan_chips)
        .filter_map(|chip| chip.reading.clone())
        .collect();
    let mut agent_ids: Vec<String> = Vec::new();
    for agent_id in chips
        .iter()
        .map(|reading| &reading.agent_id.0)
        .chain(state.plan_windows.keys())
    {
        if !agent_ids.contains(agent_id) {
            agent_ids.push(agent_id.clone());
        }
    }
    agent_ids
        .iter()
        .filter_map(|agent_id| {
            let mut readings: Vec<PlanUsageReading> = state
                .plan_windows
                .get(agent_id)
                .cloned()
                .unwrap_or_default();
            readings.extend(
                chips
                    .iter()
                    .filter(|reading| reading.agent_id.0 == *agent_id)
                    .cloned(),
            );
            let reading = binding_reading(&readings)?;
            Some(plan_usage_part(&clean(&label_for_agent(agent_id)), reading))
        })
        .collect()
}

pub(super) fn plan_usage_part(label: &str, reading: &PlanUsageReading) -> (String, Role) {
    let warn = reading.status == PlanUsageStatus::Warning || reading.used.is_some_and(|u| u >= 0.9);
    let role = if warn { Role::Warn } else { Role::Dim };
    match (reading.status, reading.used) {
        (PlanUsageStatus::Rejected, _) if reading.wrap_up => {
            (format!("{label} wrapping up"), Role::Warn)
        }
        (PlanUsageStatus::Rejected, _) => (format!("{label} out"), Role::Danger),
        (_, Some(used)) => {
            let percent = (used.clamp(0.0, 1.0) * 100.0).round();
            (format!("{label} {percent}%"), role)
        }
        (PlanUsageStatus::Warning, None) => (format!("{label} near"), role),
        (PlanUsageStatus::Allowed, None) => (format!("{label} ok"), role),
        (PlanUsageStatus::Unknown, None) => (format!("{label} ?"), role),
    }
}

/// Every tab with its count. Each takes the same cells shown or not, so none moves when you
/// switch; when the roomy `[ All 3 ]` form doesn't fit, the tabs close up to `[All 3]`.
pub(super) fn tabs_line(
    panel: &AgentPanelRows,
    shown: CrewTab,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    let labels: Vec<(CrewTab, String)> = CrewTab::ORDER
        .iter()
        .map(|&tab| (tab, format!("{} {}", tab.label(), panel.count(tab))))
        .collect();
    let used: usize = labels.iter().map(|(_, label)| str_width(label)).sum();
    let roomy = used + 4 * labels.len() <= width;
    let spans = labels
        .into_iter()
        .map(|(tab, label)| {
            if tab == shown {
                let text = if roomy {
                    format!("[ {label} ]")
                } else {
                    format!("[{label}]")
                };
                Span::styled(text, theme.style(Role::Label).add_modifier(Modifier::BOLD))
            } else {
                let role = if tab == CrewTab::NeedsYou && panel.count(tab) > 0 {
                    Role::Question
                } else {
                    Role::Dim
                };
                let text = if roomy {
                    format!("  {label}  ")
                } else {
                    format!(" {label} ")
                };
                Span::styled(text, theme.style(role))
            }
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

/// The list: each run's heading once, then its agents by their labels and their last lines,
/// and the conversations and CLI sessions under `Sessions`. A repo's name heads its runs only
/// when another repo's are listed too. `selected` indexes the rows in order, and `sleeping`
/// says which wait on a usage reset.
#[allow(clippy::too_many_arguments)]
pub(super) fn list_lines(
    groups: &[RepoGroup<'_>],
    this_repo: &str,
    sleeping: &[bool],
    selected: usize,
    width: usize,
    now: OffsetDateTime,
    names: &DisplayNames,
    theme: &Theme,
) -> Vec<BodyLine> {
    let marks = Marks::for_theme(theme);
    // Two cells of indent, the selection mark and a space, the glyph slot and a space.
    let margin = 2 + 2 + marks.slot() + 1;
    let rows: Vec<&CrewRow> = groups
        .iter()
        .flat_map(|repo| repo.runs.iter().flat_map(|run| run.rows.iter().copied()))
        .collect();
    let columns = row_columns(&rows, width.saturating_sub(margin), now, names);
    let dim = theme.style(Role::Dim);
    let named = groups.len() > 1 || groups.iter().any(|repo| repo.repo != this_repo);
    let mut lines = Vec::new();
    let mut index = 0;
    for repo in groups {
        if named {
            if !lines.is_empty() {
                lines.push(BodyLine::text(Line::default()));
            }
            lines.push(BodyLine::text(Line::from(Span::styled(
                cut_middle(&clean(repo.repo), width),
                theme.style(Role::Bright).add_modifier(Modifier::BOLD),
            ))));
        }
        for run in &repo.runs {
            match run.run {
                // A run listed whole is its own heading, and the heading is the row.
                Some(heading) if listed_whole(&run.rows) => {
                    let chosen = index == selected;
                    lines.push(BodyLine {
                        line: run_heading(heading, chosen, width, now, theme),
                        row: Some(index),
                    });
                    index += 1;
                    continue;
                }
                Some(heading) => lines.push(BodyLine::text(run_heading(
                    heading, false, width, now, theme,
                ))),
                None => lines.push(BodyLine::text(Line::from(Span::styled("Sessions", dim)))),
            }
            for row in &run.rows {
                let chosen = index == selected;
                let asleep = sleeping.get(index).copied().unwrap_or(false);
                let (glyph, role) = glyph(row.state, asleep, theme);
                // The chosen row is lavender from its pointer to its last line, its mark too.
                let role = if chosen { Role::Question } else { role };
                let mut spans = vec![
                    Span::raw("  "),
                    if chosen {
                        Span::styled(format!("{} ", marks.selected), theme.style(Role::Question))
                    } else {
                        Span::raw("  ")
                    },
                    Span::styled(pad_glyph(glyph, marks.slot()), theme.style(role)),
                    Span::raw(" "),
                ];
                for (c, column) in columns.iter().enumerate() {
                    if c > 0 {
                        spans.push(Span::raw(" ".repeat(GAP)));
                    }
                    let cell = column.cell(&column.cells[index]);
                    let style = match column.heading {
                        "name" | "latest" if chosen => theme.style(Role::Question),
                        "name" => theme.style(Role::Text),
                        "latest" if row.state == CrewState::NeedsYou => theme.style(Role::Question),
                        "latest" if row.state == CrewState::Failed => theme.style(Role::Danger),
                        "latest" if row.run.is_some() => theme.style(Role::Text),
                        _ => dim,
                    };
                    match (column.heading, tries_mark(row)) {
                        ("name", Some(mark)) => spans.extend(with_tries(cell, &mark, style, dim)),
                        _ => spans.push(Span::styled(cell, style)),
                    }
                }
                lines.push(BodyLine {
                    line: Line::from(spans),
                    row: Some(index),
                });
                index += 1;
            }
        }
    }
    lines
}

/// A run's group is the run itself, with none of its agents listed.
fn listed_whole(rows: &[&CrewRow]) -> bool {
    matches!(rows, [row] if matches!(row.target, CrewTarget::Run { .. }))
}

/// `✓ add-due-dates · merged 13m ago · ~$0.23`: the run's glyph and name, then, dim, where it
/// stands and what it spent. A run that ended says how long ago. Highlighted, as the row of a
/// run listed whole, the selection mark takes the glyph's place, since the words say the state.
fn run_heading(
    run: &CrewRun,
    chosen: bool,
    width: usize,
    now: OffsetDateTime,
    theme: &Theme,
) -> Line<'static> {
    let marks = Marks::for_theme(theme);
    let name = [run.name.as_deref(), Some(run.title.as_str())]
        .into_iter()
        .flatten()
        .map(clean)
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| clean(run.id.as_str()));
    let mut tail = String::new();
    if let Some(stands) = &run.stands {
        let ended = matches!(
            stands.state,
            CrewState::Done | CrewState::Stopped | CrewState::Failed | CrewState::Skipped
        );
        tail.push_str(&format!(" · {}", clean(&stands.word)));
        if ended {
            tail.push_str(&format!(" {}", ago(stands.since, now)));
        }
        if let Some(spend) = stands.spent.as_ref().and_then(panes::spend) {
            tail.push_str(&format!(" · {spend}"));
        }
    }
    let opening = match (&run.stands, chosen) {
        (_, true) => Span::styled(
            pad_glyph(marks.selected, marks.slot()),
            theme.style(Role::Question),
        ),
        (Some(stands), false) => {
            let (glyph, role) = glyph(stands.state, false, theme);
            Span::styled(pad_glyph(glyph, marks.slot()), theme.style(role))
        }
        (None, false) => Span::raw(" ".repeat(marks.slot())),
    };
    let room = width.saturating_sub(marks.slot() + 1);
    // The name gives way before the words after it, down to a few cells.
    let name = cut(
        &name,
        room.saturating_sub(str_width(&tail)).max(RUN_NAME_MIN),
    );
    let tail = cut(&tail, room.saturating_sub(str_width(&name)));
    let role = if chosen { Role::Question } else { Role::Bright };
    Line::from(vec![
        opening,
        Span::raw(" "),
        Span::styled(name, theme.style(role).add_modifier(Modifier::BOLD)),
        Span::styled(tail, theme.style(Role::Dim)),
    ])
}

/// ` ×6` after the label of a row that stands for six tries.
fn tries_mark(row: &CrewRow) -> Option<String> {
    (row.tries > 1).then(|| format!(" ×{}", row.tries))
}

/// The name cell with its tries mark dim. A cell cut short of the mark draws in one style.
fn with_tries(cell: String, mark: &str, style: Style, dim: Style) -> Vec<Span<'static>> {
    let padded = cell.trim_end();
    match padded.strip_suffix(mark) {
        Some(label) => vec![
            Span::styled(label.to_owned(), style),
            Span::styled(mark.to_owned(), dim),
            Span::raw(" ".repeat(cell.len() - padded.len())),
        ],
        None => vec![Span::styled(cell, style)],
    }
}

/// What a row goes by: an agent's label with its tries, else its title.
fn name_cell(row: &CrewRow) -> String {
    let name = row
        .label
        .as_deref()
        .map(clean)
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| clean(&row.title));
    format!("{name}{}", tries_mark(row).unwrap_or_default())
}

/// An agent's last line. A conversation or a CLI session says which CLI it's in and that
/// Enter carries it on, since that's what its row is for.
fn latest_cell(row: &CrewRow, names: &DisplayNames) -> String {
    match &row.target {
        CrewTarget::Conversation { .. } | CrewTarget::CliSession { .. } => {
            match row.worker.as_ref() {
                Some(worker) => {
                    format!("{} · enter continues", clean(&names.cli(&worker.agent)))
                }
                None => "enter continues".to_owned(),
            }
        }
        _ => clean(&row.latest),
    }
}

/// The name and the latest line in `room` cells. Labels are short and tell the agents apart,
/// so the name column always fits the widest of them, and a longer title beside them is cut to
/// it. Room the two leave goes first to how long ago a row last changed, then to who runs an
/// agent, then to the titles. The latest line takes the rest, so the times line up at the
/// right edge.
pub(super) fn row_columns(
    rows: &[&CrewRow],
    room: usize,
    now: OffsetDateTime,
    names: &DisplayNames,
) -> Vec<Column> {
    let cells = |f: &dyn Fn(&CrewRow) -> String| rows.iter().map(|row| f(row)).collect::<Vec<_>>();
    let widest = |column: &Column| column.cells.iter().map(|c| str_width(c)).max().unwrap_or(0);
    let fitted = |mut column: Column| {
        column.width = widest(&column);
        column
    };
    let mut name = Column::new("name", cells(&name_cell));
    let name_wants = widest(&name).max(1);
    let widest_label = rows
        .iter()
        .filter(|row| row.label.is_some())
        .map(|row| str_width(&name_cell(row)))
        .max();
    name.width = name_wants.min(match widest_label {
        Some(label) => label.max(NAME_MIN).min((room * 6 / 10).max(NAME_MIN)),
        // With no label listed, titles keep a share of the row.
        None => (room * 4 / 10).max(NAME_MIN),
    });
    let mut latest = Column::new("latest", cells(&|row| latest_cell(row, names))).flex_min(14);
    let latest_wants = widest(&latest);
    let mut spare = room.checked_sub(name.width + GAP + latest_wants);
    let mut extra = |column: Column| {
        if column.width == 0 {
            return None;
        }
        spare = spare?.checked_sub(column.width + GAP);
        spare.map(|_| column)
    };
    let when = extra(fitted(
        Column::new("ago", cells(&|row| ago(row.updated, now))).numeric(),
    ));
    // A session's CLI is in its latest line already.
    let worker = extra(fitted(Column::new(
        "worker",
        cells(&|row| match (&row.run, &row.worker) {
            (Some(_), Some(worker)) => clean(&names.worker(worker)),
            _ => String::new(),
        }),
    )));
    name.width = name_wants.min(name.width + spare.unwrap_or(0));
    let others: usize = [Some(&name), worker.as_ref(), when.as_ref()]
        .into_iter()
        .flatten()
        .map(|column| column.width + GAP)
        .sum();
    latest.width = room.saturating_sub(others);
    [Some(name), Some(latest), worker, when]
        .into_iter()
        .flatten()
        .collect()
}

/// The highlighted row in full, `width` cells wide.
pub(super) fn details_lines(
    row: &CrewRow,
    sleeping: bool,
    width: usize,
    names: &DisplayNames,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let width = width.max(1);
    let dim = theme.style(Role::Dim);
    let mut lines = Vec::new();
    let title = match clean(&row.title) {
        title if title.is_empty() => "Untitled".to_owned(),
        title => title,
    };
    let bold = theme.style(Role::Bright).add_modifier(Modifier::BOLD);
    lines.extend(capped(
        wrap_text(&title, bold, width, &[], &[]),
        TITLE_LINES,
        width,
        theme,
    ));
    let (glyph, role) = glyph(row.state, sleeping, theme);
    lines.push(Line::from(Span::styled(
        format!("{glyph} {}", state_word(row.state)),
        theme.style(role),
    )));
    let details = &row.details;
    let message = details
        .last_message
        .clone()
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| row.latest.clone());
    if !message.trim().is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled("Last message", dim)));
        lines.extend(paragraphs(&message, MESSAGE_LINES, width, theme));
    }
    lines.push(Line::default());
    // Values wrap under themselves, past the names, as Codex's project path does.
    let hang = [Span::raw(" ".repeat(FIELD))];
    let room = width.saturating_sub(FIELD).max(1);
    for (name, value) in fields(row, details, names, room) {
        let first = [Span::styled(left(name, FIELD), dim)];
        let value = wrap_text(&value, theme.style(Role::Text), width, &first, &hang);
        lines.extend(capped(value, TITLE_LINES, width, theme));
    }
    if let Some(prompt) = details.prompt.as_deref().filter(|p| !p.trim().is_empty()) {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled("Prompt", dim)));
        lines.extend(paragraphs(prompt, usize::MAX, width, theme));
    }
    lines
}

/// The short facts, each on a line of its own. Paths are cut in the middle to fit `room`, since
/// a path broken across lines mid-name reads as two.
fn fields(
    row: &CrewRow,
    details: &CrewDetails,
    names: &DisplayNames,
    room: usize,
) -> Vec<(&'static str, String)> {
    let mut fields = vec![("Repo", cut_middle(&clean(&row.repo), room))];
    if let Some(run) = &row.run {
        let value = match clean(&run.title) {
            title if title.is_empty() => clean(run.id.as_str()),
            title => format!("{} · {title}", clean(run.id.as_str())),
        };
        fields.push(("Run", value));
    }
    if let Some(task) = &details.task {
        fields.push(("Task", clean(task.as_str())));
    }
    // The details are where the id shows too, for commands such as /reassign.
    if let Some(worker) = &row.worker {
        let model = format!("{} ({worker})", names.worker(worker));
        fields.push(("Model", clean(&model)));
    }
    if let Some(spent) = &details.spent {
        let mut tokens = format!(
            "{} in · {} out",
            format::tokens(spent.input_tokens),
            format::tokens(spent.output_tokens)
        );
        if let Some(cost) = panes::spend(spent) {
            tokens.push_str(&format!(" · {cost}"));
        }
        fields.push(("Tokens", tokens));
    }
    if let Some(context) = &details.context {
        let mut used = match context.window.filter(|window| *window > 0) {
            Some(window) => format!(
                "{} of {}",
                format::tokens_beside(context.used, window),
                format::window(window)
            ),
            None => format::tokens(context.used),
        };
        if let Some(share) = context.of_window() {
            used.push_str(&format!(" ({:.0}%)", (share * 100.0).round()));
        }
        fields.push(("Context", used));
    }
    if let Some(branch) = &details.branch {
        fields.push(("Branch", cut_middle(&clean(branch), room)));
    }
    fields
}

/// `text`'s lines wrapped to `width`, at most `most` of them, the last cut with `…` when more
/// was left. Blank lines between paragraphs stay, one at a time.
fn paragraphs(text: &str, most: usize, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let text = crate::text::sanitize(text);
    let mut lines = Vec::new();
    let mut blank = false;
    for line in text.lines().map(str::trim_end) {
        if line.trim().is_empty() {
            blank = !lines.is_empty();
            continue;
        }
        if std::mem::take(&mut blank) {
            lines.push(Line::default());
        }
        lines.extend(wrap_text(line, theme.style(Role::Text), width, &[], &[]));
    }
    capped(lines, most, width, theme)
}

/// `lines`, at most `most` of them, the last ending in `…` when some were left out.
fn capped(
    mut lines: Vec<Line<'static>>,
    most: usize,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    if lines.len() <= most {
        return lines;
    }
    lines.truncate(most);
    if let Some(last) = lines.last_mut() {
        let ellipsis = theme.glyphs.ellipsis;
        let mut line = truncate_line(last, width.saturating_sub(str_width(ellipsis)));
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        if !text.ends_with('…') && !text.ends_with("...") {
            let style = line
                .spans
                .last()
                .map_or(Style::default(), |span| span.style);
            line.spans.push(Span::styled(ellipsis, style));
        }
        *last = line;
    }
    lines
}

/// The keys, in place of the list while `?` holds.
/// With the panel opened by ← from the input, → goes back rather than opening.
pub(super) fn keys_lines(
    wide: bool,
    right_closes: bool,
    vim: bool,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let (open, back) = match (right_closes, vim) {
        (false, false) => ("→  or Enter", "←  or Esc"),
        (false, true) => ("→, l or Enter", "←, h or Esc"),
        (true, false) => ("Enter", "←, →  or Esc"),
        (true, true) => ("Enter", "←  →  h l or Esc"),
    };
    let mut keys = vec![
        (if vim { "↑↓  or j k" } else { "↑↓" }, "move"),
        (
            open,
            "open: answer its question, watch the agent, or carry on",
        ),
        ("Tab, Shift+Tab", "the next or the previous tab"),
    ];
    if !wide {
        keys.push(("Space", "the details under the list"));
    }
    keys.extend([
        ("x", "stop the agent, once more to be sure"),
        ("c", "carry a finished agent's session on in its own CLI"),
        ("?", "hide these keys"),
        (back, "back to the conversation"),
    ]);
    let key_width = 16;
    let mut lines = vec![Line::from(Span::styled(
        "Keys",
        theme.style(Role::Bright).add_modifier(Modifier::BOLD),
    ))];
    for (key, does) in keys {
        let key = key_names(key, theme);
        let rest = width.saturating_sub(2 + key_width);
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(left(&key, key_width), theme.style(Role::Text)),
            Span::styled(cut(does, rest), theme.style(Role::Dim)),
        ]));
    }
    lines
}
