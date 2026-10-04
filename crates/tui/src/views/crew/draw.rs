//! What the crew panel draws: the title row with quotas, the state tabs, the grouped list, the
//! details of the highlighted row, and the keys.

use argonauts_core::crew_panel::{CrewDetails, CrewPanel, CrewRow, CrewState, CrewTab, RepoGroup};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::limits::{QuotaReading, QuotaStatus};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use time::OffsetDateTime;

use crate::bridge::status::label_for_vendor;
use crate::format;
use crate::state::{AppState, binding_reading};
use crate::text::{str_width, truncate_line, wrap_text};
use crate::theme::{Role, Theme};

use super::super::layout::{cut, cut_middle, left};
use super::super::list::{Column, fit_columns};
use super::super::panes::{self, Marks, key_names, pad_glyph};
use super::super::resume::ago;
use super::rows::clean;

/// Cells between columns.
pub(super) const GAP: usize = 2;

/// An agent's name keeps at least this many cells while its column shows.
const AGENT_MIN: usize = 12;

/// Lines of a long message the details keep before cutting.
const MESSAGE_LINES: usize = 6;
const TITLE_LINES: usize = 3;
/// The narrowest the title column starts, before the latest message gives it room.
const TITLE_MIN: usize = 16;

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
    }
}

/// A row's glyph and color: shape and color both say its state.
pub(super) fn glyph(state: CrewState, sleeping: bool, theme: &Theme) -> (&'static str, Role) {
    let marks = Marks::for_theme(theme);
    match state {
        CrewState::NeedsYou => (marks.needs_you, Role::Card),
        CrewState::Working => (marks.running, Role::Accent),
        CrewState::Waiting if sleeping => (theme.glyphs.agent_timer, Role::Dim),
        CrewState::Waiting => (marks.waiting, Role::Dim),
        CrewState::Ready => (ready_glyph(theme), Role::Bright),
        CrewState::Done => (marks.done, Role::Success),
        CrewState::Stopped => (marks.stopped, Role::Dim),
        CrewState::Failed => (marks.failed, Role::Error),
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

/// `argonauts › crew` at the left and the quotas at the right, the quotas giving way first.
pub(super) fn title_line(state: &AppState, width: usize, theme: &Theme) -> Line<'static> {
    let marks = Marks::for_theme(theme);
    let title = format!("argonauts {} crew", marks.crumb);
    let title_width = str_width(&title);
    let mut spans = vec![Span::styled(title, theme.style(Role::Label))];
    let mut parts = quota_parts(state);
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

/// `Claude 39%` for each vendor with a reading, in the status line's order.
fn quota_parts(state: &AppState) -> Vec<(String, Role)> {
    let chips: Vec<QuotaReading> = state
        .status
        .iter()
        .flat_map(|status| &status.quotas)
        .filter_map(|chip| chip.reading.clone())
        .collect();
    let mut vendors: Vec<String> = Vec::new();
    for vendor in chips
        .iter()
        .map(|reading| &reading.vendor.0)
        .chain(state.quotas.keys())
    {
        if !vendors.contains(vendor) {
            vendors.push(vendor.clone());
        }
    }
    vendors
        .iter()
        .filter_map(|vendor| {
            let mut readings: Vec<QuotaReading> =
                state.quotas.get(vendor).cloned().unwrap_or_default();
            readings.extend(
                chips
                    .iter()
                    .filter(|reading| reading.vendor.0 == *vendor)
                    .cloned(),
            );
            let reading = binding_reading(&readings)?;
            Some(quota_part(&clean(&label_for_vendor(vendor)), reading))
        })
        .collect()
}

pub(super) fn quota_part(label: &str, reading: &QuotaReading) -> (String, Role) {
    let warn = reading.status == QuotaStatus::Warning || reading.used.is_some_and(|u| u >= 0.9);
    let role = if warn { Role::Warning } else { Role::Dim };
    match (reading.status, reading.used) {
        (QuotaStatus::Rejected, _) => (format!("{label} out"), Role::Error),
        (_, Some(used)) => {
            let percent = (used.clamp(0.0, 1.0) * 100.0).round();
            (format!("{label} {percent}%"), role)
        }
        (QuotaStatus::Warning, None) => (format!("{label} near"), role),
        (QuotaStatus::Allowed, None) => (format!("{label} ok"), role),
        (QuotaStatus::Unknown, None) => (format!("{label} ?"), role),
    }
}

/// Every tab with its count. Each takes the same cells shown or not, so none moves when you
/// switch; when the roomy `[ All 3 ]` form doesn't fit, the tabs close up to `[All 3]`.
pub(super) fn tabs_line(
    panel: &CrewPanel,
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
                    Role::Card
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

/// The list: each repo, each run in it, and their rows. `selected` indexes the rows in order,
/// and `sleeping` says which wait on a usage reset.
pub(super) fn list_lines(
    groups: &[RepoGroup<'_>],
    sleeping: &[bool],
    selected: usize,
    width: usize,
    now: OffsetDateTime,
    names: &DisplayNames,
    theme: &Theme,
) -> Vec<BodyLine> {
    let marks = Marks::for_theme(theme);
    // Two cells of indent, the selection mark and a space, the glyph slot and a space.
    let lead = 2 + 2 + marks.slot() + 1;
    let rows: Vec<&CrewRow> = groups
        .iter()
        .flat_map(|repo| repo.runs.iter().flat_map(|run| run.rows.iter().copied()))
        .collect();
    let columns = row_columns(&rows, width.saturating_sub(lead), now, names);
    let dim = theme.style(Role::Dim);
    let mut lines = Vec::new();
    let mut index = 0;
    for repo in groups {
        if !lines.is_empty() {
            lines.push(BodyLine::text(Line::default()));
        }
        lines.push(BodyLine::text(Line::from(Span::styled(
            cut_middle(&clean(repo.repo), width),
            theme.style(Role::Bright).add_modifier(Modifier::BOLD),
        ))));
        for run in &repo.runs {
            let mut spans = vec![Span::raw("  ")];
            match run.run {
                // People know a run by its title. The id stays in the details pane.
                Some(run) if run.title.trim().is_empty() => {
                    spans.push(Span::styled(clean(run.id.as_str()), dim));
                }
                Some(run) => spans.push(Span::styled(clean(&run.title), theme.style(Role::Text))),
                None => spans.push(Span::styled(
                    "Conversations and CLI sessions",
                    theme.style(Role::Text),
                )),
            }
            lines.push(BodyLine::text(Line::from(spans)));
            for row in &run.rows {
                let chosen = index == selected;
                let asleep = sleeping.get(index).copied().unwrap_or(false);
                let (glyph, role) = glyph(row.state, asleep, theme);
                let mut spans = vec![
                    Span::raw("  "),
                    if chosen {
                        Span::styled(format!("{} ", marks.selected), theme.style(Role::Label))
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
                    let style = match column.heading {
                        "title" if chosen => theme.style(Role::Label),
                        "title" => theme.style(Role::Text),
                        "latest" if row.state == CrewState::NeedsYou => theme.style(Role::Card),
                        "latest" if row.state == CrewState::Failed => theme.style(Role::Error),
                        _ => dim,
                    };
                    spans.push(Span::styled(column.cell(&column.cells[index]), style));
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

/// The title, the agent, tokens, spend, the latest message, and how long ago, in `room` cells,
/// each padded to its widest. Spend and then tokens give way first, then the agent, already cut
/// with `…` where the row is short, and then the time. The title and the latest message share what's left: each
/// whole when both fit, else the title gets at least half, and all of it that the messages
/// leave free.
pub(super) fn row_columns(
    rows: &[&CrewRow],
    room: usize,
    now: OffsetDateTime,
    names: &DisplayNames,
) -> Vec<Column> {
    let cells = |f: &dyn Fn(&CrewRow) -> String| rows.iter().map(|row| f(row)).collect::<Vec<_>>();
    let widest = |column: &Column| column.cells.iter().map(|c| str_width(c)).max().unwrap_or(0);
    let worker = |row: &CrewRow| {
        row.worker
            .as_ref()
            .map_or(String::new(), |worker| clean(&names.worker(worker)))
    };
    let tokens = |row: &CrewRow| {
        row.details
            .spent
            .as_ref()
            .map(|spent| spent.input_tokens + spent.output_tokens)
            .filter(|tokens| *tokens > 0)
            .map_or(String::new(), panes::tokens)
    };
    let spend = |row: &CrewRow| {
        row.details
            .spent
            .as_ref()
            .and_then(panes::spend)
            .unwrap_or_default()
    };
    let fitted = |mut column: Column| {
        column.width = widest(&column);
        column
    };
    let mut title = Column::new("title", cells(&|row| clean(&row.title)));
    let title_wants = widest(&title).max(1);
    title.width = title_wants.min((room * 3 / 10).max(TITLE_MIN));
    let mut agent = fitted(Column::new("worker", cells(&worker)).keep(3));
    let mut latest = Column::new("latest", cells(&|row| clean(&row.latest))).flex_min(14);
    let latest_wants = widest(&latest);
    latest.width = latest_wants;
    let when = fitted(
        Column::new("ago", cells(&|row| ago(row.updated, now)))
            .numeric()
            .keep(4),
    );
    // The agent is whole where the title and the messages leave room for it, else it keeps a
    // quarter of the row before it's cut, since the messages say what each agent is doing.
    let left_over = room.saturating_sub(title.width + latest_wants + when.width + GAP * 3);
    agent.width = agent.width.min(left_over.max(room / 4).max(AGENT_MIN));
    // Tokens, then spend, take only room the rest leave whole, so they never squeeze a title
    // or a message, and spend never shows without the tokens.
    let whole = title_wants + agent.width + latest_wants + when.width + GAP * 3;
    let mut spare = Some(room.saturating_sub(whole));
    let mut extra = |column: Column| {
        if column.width == 0 {
            return None;
        }
        spare = spare?.checked_sub(column.width + GAP);
        spare.map(|_| column)
    };
    let tokens = extra(fitted(
        Column::new("tokens", cells(&tokens)).numeric().keep(2),
    ));
    let spend = extra(fitted(
        Column::new("spend", cells(&spend)).numeric().keep(1),
    ));
    let agent = (agent.width > 0).then_some(agent);
    let columns = [Some(title), agent, tokens, spend, Some(latest), Some(when)]
        .into_iter()
        .flatten()
        .collect();
    let mut columns = fit_columns(columns, room, GAP);
    let others: usize = columns
        .iter()
        .filter(|c| !matches!(c.heading, "title" | "latest"))
        .map(|c| c.width)
        .sum();
    let share = room.saturating_sub(others + GAP * columns.len().saturating_sub(1));
    let title_width = if title_wants + latest_wants <= share {
        title_wants
    } else {
        title_wants.min((share / 2).max(share.saturating_sub(latest_wants)))
    };
    // What the title leaves goes to the latest message, so the times line up at the right edge.
    for column in &mut columns {
        match column.heading {
            "title" => column.width = title_width,
            "latest" => column.width = share.saturating_sub(title_width),
            _ => {}
        }
    }
    columns
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
    if let Some(card) = &details.card {
        fields.push(("Card", clean(card.as_str())));
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
        let mut used = format!(
            "{} of {}",
            format::tokens(context.used),
            format::tokens(context.cap)
        );
        if context.cap > 0 {
            let percent = (context.used as f64 / context.cap as f64 * 100.0).round();
            used.push_str(&format!(" ({percent}%)"));
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
        (false, false) => ("→ or Enter", "← or Esc"),
        (false, true) => ("→, l or Enter", "←, h or Esc"),
        (true, false) => ("Enter", "←, → or Esc"),
        (true, true) => ("Enter", "← → h l or Esc"),
    };
    let mut keys = vec![
        (if vim { "↑↓ or j k" } else { "↑↓" }, "move"),
        (open, "open: answer its card, watch the agent, or carry on"),
        ("Tab, Shift+Tab", "the next or the previous tab"),
    ];
    if !wide {
        keys.push(("Space", "the details under the list"));
    }
    keys.extend([
        ("x", "stop the agent, once more to be sure"),
        ("c", "carry a finished agent's session on in its own CLI"),
        ("?", "hide these keys"),
        (back, "back to the chat"),
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
