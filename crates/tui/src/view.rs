//! Draws the main screen with its header, transcript and live blocks, merge card, and prompt line.

use std::borrow::Cow;

use argonauts_core::{MergeCard, ReportStatus, TodoItem, TodoStatus, Verbosity};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use time::OffsetDateTime;

use crate::app::{App, InputMode};
use crate::format;
use crate::model::{
    Entry, GatesView, Item, MergeView, RunView, SessionView, State, Step, ToolCall, ToolStatus,
    decider_word, decision_text, phase_activity, report_state, shows,
};
use crate::text::{truncate, width, wrap};
use crate::theme::Theme;
use crate::tools::{DiffSign, ToolKind, group_label, tail_lines};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const DETAIL: &str = "   ⎿ ";
const CONTINUE: &str = "     ";
const DIFF_GUTTER: &str = "   │    ";
const BOX_BAR: &str = " │ ";
const OUTPUT_INDENT: &str = "       ";

/// Tool lines a live block shows at normal verbosity before older ones fold into a count.
const LIVE_TOOL_LINES: usize = 6;
const LIVE_DIFF_LINES: usize = 4;
const LIVE_TEXT_LINES: usize = 3;
/// Output lines shown under a failed tool or gate at normal verbosity, and under any at full.
const NORMAL_OUTPUT_LINES: usize = 3;
const FULL_OUTPUT_LINES: usize = 12;
const TASK_LINES: usize = 3;

struct Painter {
    theme: Theme,
    verbosity: Verbosity,
    width: usize,
    now: OffsetDateTime,
    tick: u64,
}

impl Painter {
    fn spinner(&self) -> &'static str {
        SPINNER[(self.tick % SPINNER.len() as u64) as usize]
    }

    fn shows(&self, needed: Verbosity) -> bool {
        shows(self.verbosity, needed)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ScrollLimits {
    pub transcript: usize,
    pub diff: usize,
    pub page: usize,
}

pub(crate) fn draw(frame: &mut Frame, app: &App, now: OffsetDateTime) -> ScrollLimits {
    let area = frame.area();
    if area.height == 0 || area.width == 0 {
        return ScrollLimits::default();
    }
    let painter = Painter {
        theme: app.theme,
        verbosity: app.verbosity,
        width: area.width as usize,
        now,
        tick: app.tick,
    };
    let header = header_line(&app.view, &painter);
    frame.render_widget(Paragraph::new(header), row(area, 0));

    let hint = app.hint.as_deref().map(|h| hint_line(h, &painter));
    let prompt = prompt_line(app, &painter);
    let body_height = body_height(area.height, hint.is_some());
    let body_top = 2.min(area.height);
    let body = Rect::new(
        area.x,
        area.y + body_top,
        area.width,
        u16::try_from(body_height).unwrap_or(0),
    );

    let mut limits = ScrollLimits {
        transcript: app.scroll,
        diff: diff_len(&app.view).saturating_sub(body_height.saturating_sub(2)),
        page: body_height.saturating_sub(1).max(1),
    };
    let lines = match app.diff_scroll {
        Some(scroll) => diff_lines(&app.view, &painter, scroll, body_height),
        None => {
            let all = body_lines(&app.view, &painter);
            limits.transcript = all.len().saturating_sub(body_height);
            visible_window(all, body_height, app.scroll)
        }
    };
    frame.render_widget(Paragraph::new(lines), body);

    if let Some(hint) = hint {
        frame.render_widget(
            Paragraph::new(hint),
            row(area, area.height.saturating_sub(2)),
        );
    }
    let prompt_row = row(area, area.height.saturating_sub(1));
    frame.render_widget(Paragraph::new(prompt), prompt_row);
    if app.diff_scroll.is_none() {
        let prefix = prompt_prefix(app.mode);
        let x = width(prefix) + width(&app.input);
        let x = u16::try_from(x)
            .unwrap_or(u16::MAX)
            .min(area.width.saturating_sub(1));
        frame.set_cursor_position((prompt_row.x + x, prompt_row.y));
    }
    limits
}

/// Rows the transcript gets on a screen `height` rows tall, after the header and a blank line
/// above it, and a blank line, an optional hint, and the prompt below it.
fn body_height(height: u16, has_hint: bool) -> usize {
    let reserved = 4 + u16::from(has_hint);
    height.saturating_sub(reserved) as usize
}

fn row(area: Rect, y: u16) -> Rect {
    Rect::new(
        area.x,
        area.y + y.min(area.height.saturating_sub(1)),
        area.width,
        1,
    )
}

/// The last `height` lines, moved up by `scroll` lines.
fn visible_window(lines: Vec<Line<'static>>, height: usize, scroll: usize) -> Vec<Line<'static>> {
    if lines.len() <= height {
        return lines;
    }
    let max_scroll = lines.len() - height;
    let end = lines.len() - scroll.min(max_scroll);
    lines.into_iter().skip(end - height).take(height).collect()
}

fn header_line(view: &RunView, p: &Painter) -> Line<'static> {
    let theme = p.theme;
    let mut left = vec![Span::styled(" argonauts", theme.bold())];
    let mut right = String::new();
    if let Some(run) = &view.run {
        let mut rest = format!(" · {} · {}", run.repo, run.mode);
        if let Some(id) = &view.run_id {
            rest.push_str(&format!(" · {id}"));
        }
        left.push(Span::styled(rest, theme.dim()));
        right = match &run.model {
            Some(model) => format!("{}/{}", run.agent, model),
            None => run.agent.clone(),
        };
        right.push_str(&format!(
            " · {} ",
            format::spend(&view.spend(), view.limit())
        ));
    } else {
        left.push(Span::styled(" · starting", theme.dim()));
    }
    with_right(left, Span::styled(right, theme.dim()), p.width)
}

fn body_lines(view: &RunView, p: &Painter) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(run) = &view.run {
        task_lines(&run.task, p, &mut lines);
        lines.push(Line::default());
    }
    for entry in &view.entries {
        match entry {
            Entry::Step(step) => step_lines(step, p, &mut lines),
            Entry::Session(index) => {
                if let Some(session) = view.sessions.get(*index) {
                    session_lines(session, p, &mut lines);
                }
            }
            Entry::Gates(gates) => gates_lines(gates, p, &mut lines),
            Entry::Merge(merge) => merge_lines(merge, p, &mut lines),
        }
    }
    if let Some(line) = phase_line(view, p) {
        lines.push(line);
    }
    lines
}

fn task_lines(task: &str, p: &Painter, out: &mut Vec<Line<'static>>) {
    let task_width = p.width.saturating_sub(3).max(1);
    let mut task = wrap(task.trim(), task_width);
    if task.len() > TASK_LINES {
        task.truncate(TASK_LINES);
        if let Some(last) = task.last_mut() {
            *last = truncate(&format!("{last} …"), task_width);
        }
    }
    for (i, text) in task.into_iter().enumerate() {
        let prefix = if i == 0 { " › " } else { "   " };
        out.push(Line::from(vec![
            Span::styled(prefix, p.theme.dim()),
            Span::styled(text, p.theme.bold()),
        ]));
    }
}

fn phase_line(view: &RunView, p: &Painter) -> Option<Line<'static>> {
    let phase = view.active_phase?;
    let busy = view.sessions.iter().any(SessionView::is_running)
        || view.pending_merge().is_some()
        || view
            .entries
            .iter()
            .any(|entry| matches!(entry, Entry::Gates(g) if g.running));
    if busy {
        return None;
    }
    let text = format!("{}…", phase_activity(phase));
    Some(fit(
        vec![
            Span::raw(" "),
            Span::styled(p.spinner(), p.theme.state(State::Working)),
            Span::raw(" "),
            Span::raw(text),
        ],
        p.width,
    ))
}

fn glyph_line(state: State, text: String, p: &Painter) -> Line<'static> {
    fit(
        vec![
            Span::raw(" "),
            Span::styled(state.glyph(), p.theme.state(state)),
            Span::raw(" "),
            Span::raw(text),
        ],
        p.width,
    )
}

fn detail_line(spans: Vec<Span<'static>>, p: &Painter) -> Line<'static> {
    let mut all = vec![Span::styled(DETAIL, p.theme.dim())];
    all.extend(spans);
    fit(all, p.width)
}

fn dim_detail(text: impl Into<Cow<'static, str>>, p: &Painter) -> Line<'static> {
    detail_line(vec![Span::styled(text, p.theme.dim())], p)
}

fn error_line(text: &str, p: &Painter) -> Line<'static> {
    let error = Span::styled(format!("✗ {text}"), p.theme.state(State::Failed));
    detail_line(vec![error], p)
}

fn retry_line(text: &str, p: &Painter) -> Line<'static> {
    let retry = Span::styled(format!("↻ {text}"), p.theme.state(State::Warning));
    detail_line(vec![retry], p)
}

fn step_lines(step: &Step, p: &Painter, out: &mut Vec<Line<'static>>) {
    if !p.shows(step.shown_from) {
        return;
    }
    out.push(glyph_line(step.state, step.text.clone(), p));
    for detail in &step.details {
        out.push(detail_line(vec![Span::raw(detail.clone())], p));
    }
}

fn session_header(session: &SessionView, p: &Painter, activity: Option<String>) -> Line<'static> {
    let state = session.state();
    let mut text = format!("{} · {}", session.agent_label(), session.role_label());
    if let Some(card) = &session.card_id {
        text.push_str(&format!(" · {card}"));
        // A running block names its card; quiet lines show the activity there instead.
        if let (Some(title), true, None) = (&session.card_title, session.is_running(), &activity) {
            text.push_str(&format!(" {title}"));
        }
    }
    if let Some(report) = &session.report {
        text.push_str(" · ");
        text.push_str(&report_badge(report.status));
    }
    if let Some(end) = &session.ended {
        match end.stopped_by {
            Some(cause) => text.push_str(&format!(" · stopped ({})", stop_word(cause))),
            None if state == State::Failed && session.report.is_none() => {
                let code = end
                    .exit_code
                    .map_or_else(|| "no exit code".to_owned(), |c| format!("exit {c}"));
                text.push_str(&format!(" · failed, {code}"));
            }
            None => {}
        }
    }
    if let Some(activity) = activity {
        text.push_str(&format!(" · {activity}"));
    }
    let right = format!(
        "{} · {} tok ",
        format::elapsed(session.elapsed_ms(p.now)),
        format::tokens(session.spend().billable_tokens())
    );
    with_right(
        vec![
            Span::raw(" "),
            Span::styled(state.glyph(), p.theme.state(state)),
            Span::raw(" "),
            Span::styled(text, p.theme.bold()),
        ],
        Span::styled(right, p.theme.dim()),
        p.width,
    )
}

fn session_lines(session: &SessionView, p: &Painter, out: &mut Vec<Line<'static>>) {
    match p.verbosity {
        Verbosity::Quiet => {
            let activity = session.is_running().then(|| current_activity(session));
            out.push(session_header(session, p, activity.flatten()));
            if let Some(error) = &session.last_error {
                out.push(error_line(error, p));
            }
        }
        Verbosity::Normal if !session.is_running() => collapsed_session(session, p, out),
        Verbosity::Normal => live_session(session, p, out),
        Verbosity::Full => full_session(session, p, out),
    }
}

fn current_activity(session: &SessionView) -> Option<String> {
    let running = session.items.iter().rev().find_map(|item| match item {
        Item::Tool(call) if call.status == ToolStatus::Running && call.parent.is_none() => {
            Some(call.summary.describe())
        }
        _ => None,
    });
    running.or_else(|| (!session.stream.is_empty()).then(|| "writing".to_owned()))
}

fn collapsed_session(session: &SessionView, p: &Painter, out: &mut Vec<Line<'static>>) {
    out.push(session_header(session, p, None));
    let counts = tool_counts(session);
    if !counts.is_empty() {
        out.push(dim_detail(counts, p));
    }
    let summary = session
        .report
        .as_ref()
        .map(|r| r.summary.clone())
        .or_else(|| session.last_message().map(str::to_owned))
        .or_else(|| session.result.as_ref().and_then(|r| r.text.clone()));
    if let Some(summary) = summary {
        out.push(detail_line(vec![Span::raw(first_line(&summary))], p));
    }
    if session.state() == State::Failed
        && let Some(error) = &session.last_error
    {
        out.push(error_line(error, p));
    }
    unknown_line(session, p, out);
}

/// `Read 2 files · Write 1 file · Run 1 command`, in first-use order.
fn tool_counts(session: &SessionView) -> String {
    let mut counts: Vec<(ToolKind, &str, usize)> = Vec::new();
    for call in top_level_tools(session) {
        let (kind, label) = (call.summary.kind, call.summary.label.as_str());
        match counts
            .iter_mut()
            .find(|(k, l, _)| *k == kind && *l == label)
        {
            Some(entry) => entry.2 += 1,
            None => counts.push((kind, label, 1)),
        }
    }
    let mut parts: Vec<String> = counts
        .iter()
        .map(|(kind, label, n)| group_label(*kind, label, *n))
        .collect();
    let failed = top_level_tools(session)
        .filter(|call| call.status == ToolStatus::Failed)
        .count();
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    let files = session.files_changed.len();
    if files > 0 {
        parts.push(format!(
            "{} changed",
            format::count(files as u64, "file", "files")
        ));
    }
    parts.join(" · ")
}

fn top_level_tools(session: &SessionView) -> impl Iterator<Item = &ToolCall> {
    session.items.iter().filter_map(|item| match item {
        Item::Tool(call) if call.parent.is_none() && call.summary.kind != ToolKind::Todo => {
            Some(call)
        }
        _ => None,
    })
}

struct ToolGroup<'a> {
    first: &'a ToolCall,
    item_index: usize,
    count: usize,
}

fn tool_groups(session: &SessionView) -> Vec<ToolGroup<'_>> {
    let mut groups: Vec<ToolGroup> = Vec::new();
    for (index, item) in session.items.iter().enumerate() {
        let Item::Tool(call) = item else { continue };
        if call.parent.is_some() || call.summary.kind == ToolKind::Todo {
            continue;
        }
        if let Some(last) = groups.last_mut() {
            let same = last.first.summary.kind == call.summary.kind
                && last.first.summary.label == call.summary.label;
            let both_ok = last.first.status == ToolStatus::Ok && call.status == ToolStatus::Ok;
            if same && both_ok && call.summary.groups() {
                last.count += 1;
                continue;
            }
        }
        groups.push(ToolGroup {
            first: call,
            item_index: index,
            count: 1,
        });
    }
    groups
}

fn live_session(session: &SessionView, p: &Painter, out: &mut Vec<Line<'static>>) {
    out.push(session_header(session, p, None));
    recent_tool_lines(session, p, out);
    if !session.todos.is_empty() {
        out.push(todo_line(&session.todos, p));
    }
    if let Some(retry) = &session.retry {
        out.push(retry_line(retry, p));
    }
    if let Some(error) = &session.last_error {
        out.push(error_line(error, p));
    }
    unknown_line(session, p, out);
    live_text(session, p, out);
    whole_message_note(session, p, out);
}

/// The latest [`LIVE_TOOL_LINES`] tool groups, with older ones folded into a count.
fn recent_tool_lines(session: &SessionView, p: &Painter, out: &mut Vec<Line<'static>>) {
    let groups = tool_groups(session);
    let hidden = groups.len().saturating_sub(LIVE_TOOL_LINES);
    if hidden > 0 {
        let earlier: usize = groups[..hidden].iter().map(|g| g.count).sum();
        let text = format!(
            "… {} earlier",
            format::count(earlier as u64, "tool call", "tool calls")
        );
        out.push(dim_detail(text, p));
    }
    for group in &groups[hidden..] {
        let label = if group.count > 1 {
            group_label(
                group.first.summary.kind,
                &group.first.summary.label,
                group.count,
            )
        } else {
            group.first.summary.describe()
        };
        out.push(tool_line(group.first, label, session, p));
        if group.first.status == ToolStatus::Failed {
            output_lines(group.first.output.as_deref(), NORMAL_OUTPUT_LINES, p, out);
        }
        if session.last_diff == Some(group.item_index) {
            diff_preview(group.first, LIVE_DIFF_LINES, p, out);
        }
    }
}

/// Codex sends text only as whole messages, so without a note its block looks stalled.
fn whole_message_note(session: &SessionView, p: &Painter, out: &mut Vec<Line<'static>>) {
    if session.agent == "codex" && session.is_running() && session.stream.trim().is_empty() {
        out.push(dim_detail(
            "Codex shows its text when the message is complete",
            p,
        ));
    }
}

fn tool_line(call: &ToolCall, label: String, session: &SessionView, p: &Painter) -> Line<'static> {
    let (glyph, style): (&str, Style) = match call.status {
        ToolStatus::Ok => ("✓", p.theme.state(State::Done)),
        ToolStatus::Failed => ("✗", p.theme.state(State::Failed)),
        ToolStatus::Running if session.is_running() => (p.spinner(), p.theme.state(State::Working)),
        ToolStatus::Running => ("■", p.theme.state(State::Stopped)),
    };
    let mut text = label;
    if call.children > 0 {
        text.push_str(&format!(
            " · {}",
            format::count(call.children as u64, "tool call", "tool calls")
        ));
    }
    let indent = if call.parent.is_some() { "  ↳ " } else { "" };
    detail_line(
        vec![
            Span::raw(indent),
            Span::styled(glyph, style),
            Span::raw(" "),
            Span::raw(text),
        ],
        p,
    )
}

fn output_lines(output: Option<&str>, max: usize, p: &Painter, out: &mut Vec<Line<'static>>) {
    let Some(output) = output else { return };
    let (lines, skipped) = tail_lines(output, max);
    if skipped > 0 {
        let text = format!("{OUTPUT_INDENT}… {skipped} earlier lines");
        out.push(fit(vec![Span::styled(text, p.theme.dim())], p.width));
    }
    for line in lines {
        out.push(fit(
            vec![Span::styled(
                format!("{OUTPUT_INDENT}{line}"),
                p.theme.dim(),
            )],
            p.width,
        ));
    }
}

fn diff_preview(call: &ToolCall, max: usize, p: &Painter, out: &mut Vec<Line<'static>>) {
    let summary = &call.summary;
    for line in summary.diff.iter().take(max) {
        let sign = match line.sign {
            DiffSign::Added => "+ ",
            DiffSign::Removed => "- ",
        };
        out.push(fit(
            vec![
                Span::styled(DIFF_GUTTER, p.theme.dim()),
                Span::styled(format!("{sign}{}", line.text), p.theme.diff(line.sign)),
            ],
            p.width,
        ));
    }
    let more = summary.diff.len().saturating_sub(max) + summary.diff_hidden;
    if more > 0 {
        out.push(fit(
            vec![Span::styled(
                format!(
                    "{DIFF_GUTTER}… {}",
                    format::count(more as u64, "more line", "more lines")
                ),
                p.theme.dim(),
            )],
            p.width,
        ));
    }
}

fn todo_line(todos: &[TodoItem], p: &Painter) -> Line<'static> {
    let mut spans = vec![Span::styled("Todo", p.theme.bold())];
    for item in todos {
        let (glyph, style) = match item.status {
            TodoStatus::Completed => ("☑", p.theme.state(State::Done)),
            TodoStatus::InProgress => ("◐", p.theme.state(State::Working)),
            TodoStatus::Pending => ("☐", p.theme.dim()),
        };
        spans.push(Span::raw("  "));
        spans.push(Span::styled(glyph, style));
        spans.push(Span::raw(format!(" {}", item.text)));
    }
    detail_line(spans, p)
}

fn unknown_line(session: &SessionView, p: &Painter, out: &mut Vec<Line<'static>>) {
    if session.unknown > 0 {
        let text = format!(
            "? {} kept in the event log",
            format::count(session.unknown as u64, "unknown event", "unknown events")
        );
        out.push(dim_detail(text, p));
    }
}

/// The streaming message with a cursor, or the latest whole message.
fn live_text(session: &SessionView, p: &Painter, out: &mut Vec<Line<'static>>) {
    let (text, streaming) = if session.stream.trim().is_empty() {
        match session.last_message() {
            Some(message) => (message, false),
            None => return,
        }
    } else {
        (session.stream.trim(), true)
    };
    let text_width = p.width.saturating_sub(width(DETAIL) + 1).max(1);
    let mut lines = wrap(text, text_width);
    let skip = lines.len().saturating_sub(LIVE_TEXT_LINES);
    lines.drain(..skip);
    let last = lines.len().saturating_sub(1);
    for (i, line) in lines.into_iter().enumerate() {
        let prefix = if i == 0 { DETAIL } else { CONTINUE };
        let mut spans = vec![Span::styled(prefix, p.theme.dim()), Span::raw(line)];
        if streaming && i == last {
            spans.push(Span::styled("▌", p.theme.state(State::Working)));
        }
        out.push(fit(spans, p.width));
    }
}

fn full_session(session: &SessionView, p: &Painter, out: &mut Vec<Line<'static>>) {
    out.push(session_header(session, p, None));
    if let Some(command) = &session.command {
        out.push(dim_detail(format!("launched · {command}"), p));
    }
    for (index, item) in session.items.iter().enumerate() {
        item_lines(session, index, item, p, out);
    }
    if session.is_running() {
        live_text_full(session, p, out);
        whole_message_note(session, p, out);
    } else if let Some(report) = &session.report {
        let state = report_state(report.status);
        out.push(detail_line(
            vec![
                Span::styled(
                    format!("report {}", report_word(report.status)),
                    p.theme.state(state),
                ),
                Span::raw(format!(" · {}", report.summary)),
            ],
            p,
        ));
    }
}

/// One session item at full verbosity, where `index` is its place in `session.items`.
fn item_lines(
    session: &SessionView,
    index: usize,
    item: &Item,
    p: &Painter,
    out: &mut Vec<Line<'static>>,
) {
    match item {
        Item::Tool(call) => {
            out.push(tool_line(call, call.summary.describe(), session, p));
            if session.last_diff == Some(index) || !call.summary.diff.is_empty() {
                diff_preview(call, FULL_OUTPUT_LINES, p, out);
            }
            output_lines(call.output.as_deref(), FULL_OUTPUT_LINES, p, out);
        }
        Item::Message { text, parent } => {
            let label = if parent.is_some() { "↳ " } else { "" };
            wrapped_detail(&format!("{label}{text}"), Style::new(), p, out);
        }
        Item::Reasoning(text) => {
            wrapped_detail(&format!("thinking · {text}"), p.theme.dim(), p, out);
        }
        Item::File { path, kind } => out.push(dim_detail(format!("~ {kind} {path}"), p)),
        Item::Todo(todos) => out.push(todo_line(todos, p)),
        Item::Error(text) => out.push(error_line(text, p)),
        Item::Retry(text) => out.push(retry_line(text, p)),
        Item::Unknown(raw) => out.push(dim_detail(format!("? unknown event · {raw}"), p)),
    }
}

fn live_text_full(session: &SessionView, p: &Painter, out: &mut Vec<Line<'static>>) {
    let text = session.stream.trim();
    if text.is_empty() {
        return;
    }
    let before = out.len();
    wrapped_detail(text, Style::new(), p, out);
    if out.len() > before
        && let Some(last) = out.last_mut()
    {
        last.spans
            .push(Span::styled("▌", p.theme.state(State::Working)));
    }
}

fn wrapped_detail(text: &str, style: Style, p: &Painter, out: &mut Vec<Line<'static>>) {
    let text_width = p.width.saturating_sub(width(DETAIL) + 1).max(1);
    for (i, line) in wrap(text, text_width).into_iter().enumerate() {
        let prefix = if i == 0 { DETAIL } else { CONTINUE };
        out.push(fit(
            vec![
                Span::styled(prefix, p.theme.dim()),
                Span::styled(line, style),
            ],
            p.width,
        ));
    }
}

fn gates_lines(gates: &GatesView, p: &Painter, out: &mut Vec<Line<'static>>) {
    out.push(gates_header(gates, p));
    for gate in &gates.results {
        if p.verbosity == Verbosity::Quiet && gate.passed {
            continue;
        }
        out.push(gate_line(gate, p));
        let max = match (p.verbosity, gate.passed) {
            (Verbosity::Full, _) => FULL_OUTPUT_LINES,
            (Verbosity::Normal, false) => NORMAL_OUTPUT_LINES,
            _ => 0,
        };
        if max > 0 && !gate.output.trim().is_empty() {
            output_lines(Some(&gate.output), max, p, out);
        }
    }
}

fn gates_header(gates: &GatesView, p: &Painter) -> Line<'static> {
    let total = gates.results.len();
    let passed = gates.passed();
    let failed = total - passed;
    let (state, text) = if gates.running {
        let so_far = if total == 0 {
            "running".to_owned()
        } else {
            format!("{passed} of {total} passed so far")
        };
        (State::Working, format!("Gates · {so_far}"))
    } else if total == 0 {
        (State::Done, "Gates · none ran".to_owned())
    } else if failed == 0 {
        (State::Done, format!("Gates · {passed} of {total} passed"))
    } else {
        (State::Failed, format!("Gates · {failed} of {total} failed"))
    };
    let mut header = glyph_line(state, text, p);
    if gates.running {
        header.spans.push(Span::raw(" "));
        header
            .spans
            .push(Span::styled(p.spinner(), p.theme.state(State::Working)));
    }
    header
}

fn gate_line(gate: &argonauts_core::GateResult, p: &Painter) -> Line<'static> {
    let (glyph, state) = if gate.passed {
        ("✓", State::Done)
    } else {
        ("✗", State::Failed)
    };
    let mut text = gate.command.join(" ");
    if !gate.passed {
        match gate.exit_code {
            Some(code) => text.push_str(&format!(" · exit {code}")),
            None => text.push_str(" · no exit code"),
        }
    }
    text.push_str(&format!(" · {}", format::seconds(gate.duration_ms)));
    detail_line(
        vec![
            Span::styled(glyph, p.theme.state(state)),
            Span::raw(" "),
            Span::raw(text),
        ],
        p,
    )
}

fn merge_lines(merge: &MergeView, p: &Painter, out: &mut Vec<Line<'static>>) {
    let card = merge
        .card_id
        .as_ref()
        .map_or_else(|| "the card".to_owned(), ToString::to_string);
    if let Some(decided) = &merge.decided {
        let text = format!(
            "Merge {card} · {} · decided by {}",
            decision_text(&decided.decision),
            decider_word(decided.by)
        );
        out.push(diamond_line(State::Done, text, p));
        if p.shows(Verbosity::Normal) {
            out.push(detail_line(
                vec![Span::styled(card_facts(&merge.card), p.theme.dim())],
                p,
            ));
        }
        return;
    }
    if let Some(chosen) = &merge.chosen {
        let text = format!(
            "Merge {card} · you chose: {} · waiting for the conductor",
            decision_text(chosen)
        );
        out.push(diamond_line(State::Waiting, text, p));
        return;
    }
    merge_box(merge, &card, p, out);
}

fn diamond_line(state: State, text: String, p: &Painter) -> Line<'static> {
    fit(
        vec![
            Span::raw(" "),
            Span::styled("◆", p.theme.state(state)),
            Span::raw(" "),
            Span::raw(text),
        ],
        p.width,
    )
}

fn card_facts(card: &MergeCard) -> String {
    let target = card.target_branch.as_deref().unwrap_or("the base branch");
    let mut facts = format!(
        "{} → {} · {}",
        card.branch,
        target,
        format::diff_stat(&card.stat)
    );
    if !card.gates.is_empty() {
        let passed = card.gates.iter().filter(|g| g.passed).count();
        let glyph = if passed == card.gates.len() {
            "✓"
        } else {
            "✗"
        };
        facts.push_str(&format!(
            " · gates {glyph} {passed} of {}",
            card.gates.len()
        ));
    }
    facts
}

fn merge_box(merge: &MergeView, card_label: &str, p: &Painter, out: &mut Vec<Line<'static>>) {
    let state = if merge.awaiting {
        State::NeedsYou
    } else {
        State::Waiting
    };
    let border = p.theme.state(state);
    out.push(merge_box_top(merge.awaiting, card_label, border, p));
    let bar = || Span::styled(BOX_BAR, border);
    let card = &merge.card;
    out.push(fit(vec![bar(), Span::raw(card_facts(card))], p.width));
    if let Some(status) = &card.budget {
        let spend = format::spend(&status.spend, status.limit);
        out.push(fit(
            vec![bar(), Span::raw(format!("spent {spend}"))],
            p.width,
        ));
    }
    if let Some(cause) = card.stopped_by {
        let text = format::stopped_card(cause, card.report.is_some());
        let stopped = Span::styled(format!("■ {text}"), p.theme.state(State::Stopped));
        out.push(fit(vec![bar(), stopped], p.width));
    }
    if let Some(report) = &card.report {
        let badge = Span::styled(
            report_badge(report.status),
            p.theme.state(report_state(report.status)),
        );
        let summary = Span::raw(format!(" · {}", report.summary));
        out.push(fit(vec![bar(), badge, summary], p.width));
        for concern in &report.concerns {
            let mark = Span::styled("! ", p.theme.state(State::Warning));
            out.push(fit(vec![bar(), mark, Span::raw(concern.clone())], p.width));
        }
    }
    for gate in card.gates.iter().filter(|g| !g.passed) {
        let mark = Span::styled("✗ ", p.theme.state(State::Failed));
        let command = Span::raw(format!("gate failed · {}", gate.command.join(" ")));
        out.push(fit(vec![bar(), mark, command], p.width));
    }
    if merge.awaiting {
        out.push(merge_keys_line(border, p));
    }
    let bottom = format!(" └{}", "─".repeat(p.width.saturating_sub(3)));
    out.push(fit(vec![Span::styled(bottom, border)], p.width));
}

fn merge_box_top(awaiting: bool, card_label: &str, border: Style, p: &Painter) -> Line<'static> {
    let title = if awaiting {
        format!(" Merge {card_label}? · needs you ")
    } else {
        format!(" Merge {card_label} · waiting for the decision ")
    };
    let used = 2 + width(&title);
    let rule = "─".repeat(p.width.saturating_sub(used + 1));
    fit(
        vec![
            Span::raw(" "),
            Span::styled("◆", border),
            Span::styled(title, p.theme.bold()),
            Span::styled(rule, border),
        ],
        p.width,
    )
}

fn merge_keys_line(border: Style, p: &Painter) -> Line<'static> {
    let mut keys = vec![Span::styled(BOX_BAR, border)];
    for (i, (key, label)) in MERGE_KEYS.iter().enumerate() {
        if i > 0 {
            keys.push(Span::raw("   "));
        }
        keys.push(Span::styled(format!("[{key}]"), p.theme.key()));
        keys.push(Span::raw(format!(" {label}")));
    }
    fit(keys, p.width)
}

pub(crate) const MERGE_KEYS: [(char, &str); 5] = [
    ('a', "approve and merge"),
    ('d', "diff"),
    ('c', "request changes"),
    ('p', "open PR"),
    ('l', "leave"),
];

fn report_word(status: ReportStatus) -> &'static str {
    match status {
        ReportStatus::Done => "done",
        ReportStatus::Blocked => "blocked",
        ReportStatus::Failed => "failed",
        ReportStatus::NeedsDecision => "needs decision",
    }
}

fn stop_word(cause: argonauts_core::StopCause) -> &'static str {
    match cause {
        argonauts_core::StopCause::Budget => "budget",
        argonauts_core::StopCause::User => "by you",
        argonauts_core::StopCause::Timeout => "timeout",
    }
}

fn report_glyph(status: ReportStatus) -> &'static str {
    match status {
        ReportStatus::Done => "✓",
        ReportStatus::Blocked | ReportStatus::Failed => "✗",
        ReportStatus::NeedsDecision => "◆",
    }
}

fn report_badge(status: ReportStatus) -> String {
    format!("report {} {}", report_glyph(status), report_word(status))
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_owned()
}

fn diff_lines(view: &RunView, p: &Painter, scroll: usize, height: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let title = match view.pending_merge().or_else(|| last_merge(view)) {
        Some(merge) => format!(" Diff · {}", card_facts(&merge.card)),
        None => " Diff".to_owned(),
    };
    lines.push(fit(vec![Span::styled(title, p.theme.bold())], p.width));
    lines.push(Line::from(Span::styled("─".repeat(p.width), p.theme.dim())));
    let Some(diff) = view.merge_diff() else {
        lines.push(Line::from(Span::styled(
            " No diff available.",
            p.theme.dim(),
        )));
        return lines;
    };
    let body: Vec<&str> = diff.lines().collect();
    let room = height.saturating_sub(lines.len());
    let start = scroll.min(body.len().saturating_sub(room));
    for text in body.iter().skip(start).take(room) {
        let style = diff_style(text, p.theme);
        lines.push(fit(
            vec![Span::raw(" "), Span::styled((*text).to_owned(), style)],
            p.width,
        ));
    }
    lines
}

fn last_merge(view: &RunView) -> Option<&MergeView> {
    view.entries.iter().rev().find_map(|entry| match entry {
        Entry::Merge(merge) => Some(merge.as_ref()),
        _ => None,
    })
}

fn diff_len(view: &RunView) -> usize {
    view.merge_diff().map_or(0, |d| d.lines().count())
}

fn diff_style(line: &str, theme: Theme) -> Style {
    if line.starts_with("+++") || line.starts_with("---") || line.starts_with("diff ") {
        theme.bold()
    } else if line.starts_with('+') {
        theme.diff(DiffSign::Added)
    } else if line.starts_with('-') {
        theme.diff(DiffSign::Removed)
    } else if line.starts_with("@@") {
        theme.dim()
    } else {
        theme.plain()
    }
}

fn hint_line(hint: &str, p: &Painter) -> Line<'static> {
    fit(
        vec![Span::styled(
            format!(" {hint}"),
            p.theme.state(State::Warning),
        )],
        p.width,
    )
}

fn prompt_prefix(mode: InputMode) -> &'static str {
    match mode {
        InputMode::Prompt => " › ",
        InputMode::Note => " note › ",
    }
}

fn prompt_line(app: &App, p: &Painter) -> Line<'static> {
    let hints = prompt_hints(app);
    let left = vec![
        Span::styled(prompt_prefix(app.mode), p.theme.bold()),
        Span::raw(app.input.clone()),
    ];
    with_right(left, Span::styled(hints, p.theme.dim()), p.width)
}

fn prompt_hints(app: &App) -> String {
    if app.diff_scroll.is_some() {
        return "↑↓ scroll · PgUp PgDn · Esc back ".to_owned();
    }
    if app.mode == InputMode::Note {
        return "Enter send · Esc cancel ".to_owned();
    }
    if app.view.finished.is_some() {
        return "run finished ".to_owned();
    }
    if app.stopping {
        return "stopping the run… ".to_owned();
    }
    if app.confirming_merge() {
        return "y or Enter merge · any other key cancels ".to_owned();
    }
    if app.view.pending_merge().is_some() && app.input.is_empty() {
        return "a approve · d diff · c changes · p PR · l leave ".to_owned();
    }
    if app.scroll > 0 {
        return "scrolled up · End to go live ".to_owned();
    }
    "Ctrl+C stop · PgUp scroll · / commands ".to_owned()
}

fn fit(spans: Vec<Span<'static>>, max: usize) -> Line<'static> {
    let mut used = 0;
    let mut out = Vec::with_capacity(spans.len());
    for span in spans {
        let w = width(&span.content);
        if used + w <= max {
            used += w;
            out.push(span);
            continue;
        }
        let room = max - used;
        if room > 0 {
            out.push(Span::styled(truncate(&span.content, room), span.style));
        }
        break;
    }
    Line::from(out)
}

/// `left` with `right` pushed to the far edge, cutting `left` first when space runs out.
fn with_right(left: Vec<Span<'static>>, right: Span<'static>, max: usize) -> Line<'static> {
    let right_width = width(&right.content);
    if right_width == 0 || right_width + 2 > max {
        return fit(left, max);
    }
    let mut line = fit(left, max - right_width - 1);
    let used: usize = line.spans.iter().map(|s| width(&s.content)).sum();
    line.spans
        .push(Span::raw(" ".repeat(max - used - right_width)));
    line.spans.push(right);
    line
}

pub(crate) fn summary_text(view: &RunView, verbosity: Verbosity, width: usize) -> Vec<String> {
    let painter = Painter {
        theme: Theme::new(false),
        verbosity,
        width,
        now: OffsetDateTime::now_utc(),
        tick: 0,
    };
    let mut lines = vec![header_line(view, &painter)];
    lines.push(Line::default());
    lines.extend(body_lines(view, &painter));
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use argonauts_core::{AgentEvent, KeyScheme, MergeDecision, RunOutcome};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use serde_json::json;

    use super::*;
    use crate::UiOptions;
    use crate::app::App;
    use crate::testing::{DIFF, Script, merge_card};

    const WIDTH: u16 = 100;
    const HEIGHT: u16 = 30;

    fn app_for(script: &Script, verbosity: Verbosity) -> App {
        let options = UiOptions {
            verbosity,
            keys: KeyScheme::Standard,
            color: false,
            bell: false,
            home_dir: Some("/home/you".into()),
        };
        let mut app = App::new(&options);
        for event in &script.events {
            app.view.apply(event);
        }
        app.tick = 1;
        app
    }

    fn render(app: &App, now: OffsetDateTime, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, app, now);
            })
            .unwrap();
        terminal.backend().to_string()
    }

    fn screen(script: &Script, verbosity: Verbosity) -> String {
        render(&app_for(script, verbosity), script.now(), WIDTH, HEIGHT)
    }

    #[test]
    fn starting_screen() {
        let mut s = Script::new();
        s.run_started("Create a file named hello.txt containing the word hi");
        s.phase(
            argonauts_core::Phase::Worktree,
            argonauts_core::PhaseStatus::Started,
            None,
        );
        insta::assert_snapshot!(render(&app_for(&s, Verbosity::Normal), s.now(), WIDTH, 12));
    }

    #[test]
    fn working_normal() {
        insta::assert_snapshot!(screen(&Script::solo_until_working(), Verbosity::Normal));
    }

    #[test]
    fn working_full() {
        insta::assert_snapshot!(screen(&Script::solo_until_working(), Verbosity::Full));
    }

    #[test]
    fn working_quiet() {
        let s = Script::solo_until_working();
        insta::assert_snapshot!(render(&app_for(&s, Verbosity::Quiet), s.now(), WIDTH, 12));
    }

    #[test]
    fn merge_card_waiting_for_the_user() {
        let s = Script::solo_finished();
        let mut app = app_for(&s, Verbosity::Normal);
        app.view.show_merge_prompt(
            &argonauts_core::CardId::numbered(1),
            &merge_card(),
            Some(DIFF),
        );
        insta::assert_snapshot!(render(&app, s.now(), WIDTH, HEIGHT));
    }

    #[test]
    fn diff_view() {
        let s = Script::solo_finished();
        let mut app = app_for(&s, Verbosity::Normal);
        app.view.show_merge_prompt(
            &argonauts_core::CardId::numbered(1),
            &merge_card(),
            Some(DIFF),
        );
        app.diff_scroll = Some(0);
        insta::assert_snapshot!(render(&app, s.now(), WIDTH, 14));
    }

    #[test]
    fn finished_run() {
        let mut s = Script::solo_finished();
        s.decision(MergeDecision::Leave, argonauts_core::DecidedBy::Config);
        s.finished(RunOutcome::BranchLeft, Some("merge.mode is leave"));
        insta::assert_snapshot!(screen(&s, Verbosity::Normal));
    }

    #[test]
    fn failing_gate_and_tool() {
        let mut s = Script::new();
        s.run_started("Fix the flaky store test");
        s.session_started();
        s.tool("b1", "Bash", json!({"command": "npm test"}));
        s.finish_tool(
            "b1",
            false,
            Some("41 passed\n1 failed\n✗ store › expires keys"),
        );
        s.agent(AgentEvent::Error {
            category: argonauts_core::ErrorCategory::RateLimit,
            message: "429 from the API".into(),
            retryable: true,
        });
        s.session_ended(Some(1), None);
        s.phase(
            argonauts_core::Phase::Gates,
            argonauts_core::PhaseStatus::Started,
            None,
        );
        s.gate(&["npm", "test"], false, "FAIL store.test.ts\n1 failed");
        s.phase(
            argonauts_core::Phase::Gates,
            argonauts_core::PhaseStatus::Failed,
            None,
        );
        insta::assert_snapshot!(render(&app_for(&s, Verbosity::Normal), s.now(), WIDTH, 16));
    }

    #[test]
    fn a_stopped_card_warns_that_its_work_may_be_partial() {
        let s = Script::solo_finished();
        let mut app = app_for(&s, Verbosity::Normal);
        let mut card = merge_card();
        card.stopped_by = Some(argonauts_core::StopCause::Budget);
        card.report = None;
        app.view
            .show_merge_prompt(&argonauts_core::CardId::numbered(1), &card, Some(DIFF));
        let out = render(&app, s.now(), WIDTH, HEIGHT);
        assert!(
            out.contains("■ budget hit · no report · partial work"),
            "{out}"
        );
    }

    #[test]
    fn a_running_codex_session_explains_its_quiet() {
        let mut s = Script::new();
        s.run_started("Add a flag");
        s.session_started_as("codex");
        for verbosity in [Verbosity::Normal, Verbosity::Full] {
            let out = screen(&s, verbosity);
            assert!(
                out.contains("Codex shows its text when the message is complete"),
                "{out}"
            );
        }
        let mut claude = Script::new();
        claude.run_started("Add a flag");
        claude.session_started();
        assert!(!screen(&claude, Verbosity::Normal).contains("Codex shows"));
    }

    #[test]
    fn the_goal_line_shows() {
        let mut s = Script::new();
        s.run_started("Add a flag");
        s.phase(
            argonauts_core::Phase::Goal,
            argonauts_core::PhaseStatus::Finished,
            Some("your prompt, recorded as the goal · sha256 9f86d08"),
        );
        let out = screen(&s, Verbosity::Normal);
        assert!(
            out.contains("● Goal · your prompt, recorded as the goal · sha256 9f86d08"),
            "{out}"
        );
    }

    #[test]
    fn the_prompt_line_asks_to_confirm_an_approval() {
        let s = Script::solo_finished();
        let mut app = app_for(&s, Verbosity::Normal);
        let (prompt, _answer) = crate::channel::MergePrompt::new(
            argonauts_core::CardId::numbered(1),
            merge_card(),
            Some(DIFF.into()),
        );
        app.on_event(crate::channel::UiEvent::MergeCardShown(Box::new(prompt)));
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('a'),
            crossterm::event::KeyModifiers::NONE,
        ));
        let out = render(&app, s.now(), WIDTH, HEIGHT);
        assert!(out.contains("y or Enter merge"), "{out}");
        assert!(out.contains("Merge into main?"), "{out}");
    }

    #[test]
    fn narrow_terminal_never_overflows() {
        let s = Script::solo_until_working();
        let out = render(&app_for(&s, Verbosity::Full), s.now(), 24, 20);
        assert!(out.lines().all(|l| l.chars().count() <= 26));
    }

    #[test]
    fn tiny_terminal_does_not_panic() {
        let s = Script::solo_until_working();
        for (w, h) in [(1, 1), (5, 2), (10, 3), (0, 0)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            let app = app_for(&s, Verbosity::Normal);
            terminal
                .draw(|frame| {
                    draw(frame, &app, s.now());
                })
                .unwrap();
        }
    }

    #[test]
    fn color_theme_paints_state_glyphs() {
        let s = Script::solo_until_working();
        let mut app = app_for(&s, Verbosity::Normal);
        app.theme = Theme::new(true);
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, &app, s.now());
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let working = (0..HEIGHT)
            .find(|&y| buffer[(1, y)].symbol() == "⏺")
            .expect("a live block");
        assert!(buffer[(1, working)].fg != ratatui::style::Color::Reset);
    }

    #[test]
    fn scrolling_moves_the_window_up() {
        let s = Script::solo_until_working();
        let mut app = app_for(&s, Verbosity::Full);
        let bottom = render(&app, s.now(), WIDTH, 8);
        app.scroll = 3;
        let scrolled = render(&app, s.now(), WIDTH, 8);
        assert_ne!(bottom, scrolled);
        assert!(scrolled.contains("scrolled up"));
    }

    #[test]
    fn monochrome_screen_has_no_colors() {
        let s = Script::solo_until_working();
        let app = app_for(&s, Verbosity::Full);
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, &app, s.now());
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert!(
            buffer
                .content()
                .iter()
                .all(|cell| cell.fg == Color::Reset && cell.bg == Color::Reset)
        );
    }

    #[test]
    fn escape_sequences_never_reach_the_screen() {
        let mut s = Script::new();
        s.run_started("\x1b]0;title\x07Fix \x1b[31mit\x1b[0m");
        s.session_started();
        s.tool("t", "Bash", json!({"command": "echo \u{1b}[5mblink"}));
        s.finish_tool("t", false, Some("\x1b[31mred\x1b[0m"));
        s.text("\x1b[2Jwiped \u{202E}txt.exe");
        let out = screen(&s, Verbosity::Full);
        assert!(out.contains("Fix it"));
        assert!(out.contains("Run echo blink"));
        assert!(out.contains("red"));
        assert!(out.contains("wiped txt.exe"));
        for remnant in ["[31m", "[0m", "[5m", "[2J", "]0;", "\u{202E}"] {
            assert!(!out.contains(remnant), "{remnant} leaked");
        }
    }

    #[test]
    fn draw_reports_scroll_limits() {
        let s = Script::solo_until_working();
        let app = app_for(&s, Verbosity::Full);
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, 10)).unwrap();
        let mut limits = ScrollLimits::default();
        terminal
            .draw(|frame| limits = draw(frame, &app, s.now()))
            .unwrap();
        assert_eq!(limits.page, 5);
        assert!(limits.transcript > 0);
    }

    #[test]
    fn summary_text_is_plain() {
        let mut s = Script::solo_finished();
        s.decision(MergeDecision::Approve, argonauts_core::DecidedBy::User);
        s.finished(RunOutcome::Merged, None);
        let app = app_for(&s, Verbosity::Normal);
        let text = summary_text(&app.view, Verbosity::Normal, 80).join("\n");
        assert!(text.contains("Merge card-001 · approve and merge · decided by you"));
        assert!(text.contains("● Merged · argonauts/run-20260929-101500-a1b2/card-001"));
        assert!(!text.contains('\x1b'));
    }
}
