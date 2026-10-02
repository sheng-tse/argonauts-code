//! One session in full: the output side streams, pinned to the live end until you scroll up,
//! and `i` shows the input side, exactly what Argonauts sent it.

use std::time::{Duration, Instant};

use argonauts_core::RunId;
use argonauts_core::api::DiffScope;
use argonauts_core::app::{UiCommand, ViewRequest};
use argonauts_core::events::session::{
    Delivery, InputPart, PromptVia, QueuedMessage, SubagentStatus, SubagentUpdate,
};
use argonauts_core::names::SessionKey;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use time::OffsetDateTime;

use crate::bridge::tools;
use crate::cell::{AssistantMessage, DiffCell, DiffView, HistoryCell, ToolItem};
use crate::format::{self, WallClock};
use crate::render::{Clock, RenderCtx};
use crate::state::{AppState, SessionState, SessionStatus};
use crate::text::{sanitize, sanitize_line, str_width, wrap_text};
use crate::theme::{Role, Theme};

use super::crew::CrewView;
use super::layout::cut;
use super::list::{self, ListState, Nav};
use super::panes::{self, Footer, Marks};
use super::{View, ViewCtx, ViewOutcome};

/// Result lines kept under a tool call; the raw log (`r`) has the rest.
const RESULT_LINES: usize = 40;

/// Lines of a prompt part shown before `→` expands it.
const PART_LINES: usize = 6;

#[derive(Debug)]
pub struct SessionView {
    pub run: RunId,
    pub session: SessionKey,
    /// Pinned to the live end until you scroll up; End pins it again.
    pub following: bool,
    pub input_side: bool,
    /// The first row shown while not following.
    pub offset: usize,
    /// For the elapsed time; tests pin it.
    pub clock: WallClock,
    /// Opened from the crew view, so ← goes back to it rather than opening another.
    from_crew: bool,
    /// The selected prompt part on the input side, and the one `→` expanded.
    part: ListState,
    expanded: Option<usize>,
    /// The steer line while you type it.
    steer: Option<String>,
    /// What you last steered with, until it shows as queued or delivered.
    sent: Option<String>,
    page: usize,
    rows: usize,
    ticking: bool,
    cache: Option<(Signature, Body)>,
    /// The first input-side row shown, kept so the selected part stays on screen.
    input_top: usize,
}

/// The rows drawn, and on the input side the row of the selected part's label.
#[derive(Debug, Clone, Default)]
struct Body {
    lines: Vec<Line<'static>>,
    selected_row: usize,
}

/// What the drawn rows depend on, so a frame with nothing new reuses them.
#[derive(Debug, Clone, PartialEq)]
struct Signature {
    width: u16,
    input_side: bool,
    expanded: Option<usize>,
    part: usize,
    last_event: Option<OffsetDateTime>,
    tools: usize,
    finished: usize,
    messages: usize,
    streaming: usize,
    diffs: usize,
    subagents: usize,
    inputs: usize,
    queued: usize,
    sent: Option<String>,
}

impl SessionView {
    pub fn new(run: RunId, session: SessionKey) -> Self {
        Self {
            run,
            session,
            following: true,
            input_side: false,
            offset: 0,
            clock: WallClock::local(),
            from_crew: false,
            part: ListState::default(),
            expanded: None,
            steer: None,
            sent: None,
            page: 1,
            rows: 0,
            ticking: false,
            cache: None,
            input_top: 0,
        }
    }

    /// Opened from the crew view, where ← returns.
    pub fn from_crew(run: RunId, session: SessionKey) -> Self {
        Self {
            from_crew: true,
            ..Self::new(run, session)
        }
    }

    /// The first row shown at `rows` long with `height` rows of room.
    fn top(&self, rows: usize, height: usize) -> usize {
        let last = rows.saturating_sub(height);
        if self.following {
            last
        } else {
            self.offset.min(last)
        }
    }

    /// Moves the output side. Scrolling up pauses following; reaching the end resumes it.
    fn scroll(&mut self, nav: Nav) {
        let last = self.rows.saturating_sub(self.page);
        let top = self.top(self.rows, self.page);
        let (offset, up) = match nav {
            Nav::Bottom => {
                self.following = true;
                return;
            }
            Nav::Top => (0, true),
            Nav::By(delta) => (list::scroll(top, nav, self.rows, self.page), delta < 0),
        };
        self.offset = offset;
        // When everything fits there's nothing to read back, so following goes on.
        self.following = if up {
            last == 0 && self.following
        } else {
            offset >= last
        };
    }

    fn signature(&self, session: &SessionState, width: u16) -> Signature {
        Signature {
            width,
            input_side: self.input_side,
            expanded: self.expanded,
            part: self.part.selected,
            last_event: Some(session.last_event),
            tools: session.tools.len(),
            finished: session
                .tools
                .iter()
                .filter(|tool| tool.ok.is_some())
                .count(),
            messages: session.messages.len(),
            streaming: session.streaming.len(),
            diffs: session.diffs.len(),
            subagents: session.subagents.len(),
            inputs: session.inputs.len(),
            queued: session.queued.len(),
            sent: self.sent.clone(),
        }
    }

    fn body(&mut self, session: &SessionState, width: u16, theme: &Theme) -> Body {
        let signature = self.signature(session, width);
        if let Some((known, body)) = &self.cache
            && *known == signature
        {
            return body.clone();
        }
        let body = if self.input_side {
            input_lines(
                session,
                &self.part,
                self.expanded,
                usize::from(width),
                theme,
            )
        } else {
            Body {
                lines: output_lines(session, self.sent.as_deref(), width, theme),
                selected_row: 0,
            }
        };
        self.cache = Some((signature, body.clone()));
        body
    }

    fn parts(session: &SessionState) -> usize {
        session.inputs.iter().map(|input| input.parts.len()).sum()
    }

    fn steer_key(&mut self, key: KeyEvent) -> ViewOutcome {
        let Some(line) = self.steer.as_mut() else {
            return ViewOutcome::Ignored;
        };
        match key.code {
            KeyCode::Esc => self.steer = None,
            KeyCode::Enter => {
                let text = line.trim().to_owned();
                self.steer = None;
                if text.is_empty() {
                    return ViewOutcome::Consumed;
                }
                self.sent = Some(text.clone());
                self.following = true;
                return ViewOutcome::Send(UiCommand::Steer {
                    run: self.run.clone(),
                    session: self.session.clone(),
                    text,
                });
            }
            KeyCode::Backspace => {
                line.pop();
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                line.push(c);
            }
            _ => {}
        }
        ViewOutcome::Consumed
    }

    fn back_to_crew(&self, state: &AppState) -> ViewOutcome {
        if self.from_crew {
            ViewOutcome::Close
        } else {
            ViewOutcome::Replace(Box::new(CrewView::new(state)))
        }
    }

    fn header(
        &self,
        session: &SessionState,
        state: &AppState,
        width: usize,
        theme: &Theme,
    ) -> Line<'static> {
        let marks = Marks::for_theme(theme);
        let (glyph, role) = match session.status {
            SessionStatus::Starting | SessionStatus::Working => (marks.running, Role::Accent),
            SessionStatus::Waiting | SessionStatus::Sleeping => (marks.waiting, Role::Dim),
            SessionStatus::Done => (marks.done, Role::Success),
            SessionStatus::Failed => (marks.failed, Role::Error),
            SessionStatus::Stopped => (marks.stopped, Role::Dim),
        };
        let mut name = vec![
            clean(&state.names.worker(&session.worker)),
            clean(session.role.as_str()),
        ];
        if let Some(card) = &session.card {
            let title = state
                .runs
                .get(&self.run)
                .and_then(|run| run.cards.get(card))
                .map(|row| clean(&row.title))
                .filter(|title| !title.is_empty());
            name.push(match title {
                Some(title) => format!("{} ({title})", clean(card.as_str())),
                None => clean(card.as_str()),
            });
        }
        if let Some(worker) = &session.standing_in_for {
            name.push(format!(
                "{} for {}",
                marks.stands_in,
                clean(&state.names.worker(worker))
            ));
        }
        let end = session.ended.unwrap_or_else(|| self.clock.now());
        let elapsed = (end - session.started).whole_milliseconds().max(0);
        let mut numbers = vec![panes::elapsed(u64::try_from(elapsed).unwrap_or(u64::MAX))];
        if session.tokens() > 0 {
            numbers.push(format!(
                "{} {}",
                theme.glyphs.tokens,
                format::tokens(session.tokens())
            ));
        }
        let numbers = format!(" · {}", numbers.join(" · "));
        let lead = format!("{glyph} ");
        let room = width.saturating_sub(str_width(&lead) + str_width(&numbers));
        Line::from(vec![
            Span::styled(lead, theme.style(role)),
            Span::styled(
                cut(&name.join(" · "), room),
                theme.style(Role::Bright).add_modifier(Modifier::BOLD),
            ),
            Span::styled(numbers, theme.style(Role::Dim)),
        ])
    }

    fn sides_line(&self, width: usize, theme: &Theme) -> Line<'static> {
        let side = |name: &str, on: bool| {
            if on {
                Span::styled(
                    format!("[ {name} ]"),
                    theme.style(Role::Label).add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(format!("  {name}  "), theme.style(Role::Dim))
            }
        };
        let mut spans = vec![
            side("Output", !self.input_side),
            Span::raw(" "),
            side("Input", self.input_side),
        ];
        let status = if self.input_side {
            "what Argonauts sent it"
        } else if self.following {
            "following"
        } else {
            "paused · End follows"
        };
        let used = crate::text::spans_width(&spans);
        let status = cut(status, width.saturating_sub(used + 2));
        if !status.is_empty() {
            spans.push(Span::raw(
                " ".repeat(width.saturating_sub(used + str_width(&status))),
            ));
            let role = if !self.input_side && !self.following {
                Role::Warning
            } else {
                Role::Dim
            };
            spans.push(Span::styled(status, theme.style(role)));
        }
        Line::from(spans)
    }

    fn footer(&self, width: usize, theme: &Theme) -> Footer {
        if let Some(line) = &self.steer {
            let lead = format!("steer {} ", theme.glyphs.prompt);
            let room = width.saturating_sub(str_width(&lead) + 1);
            return Footer::Line(Line::from(vec![
                Span::styled(lead, theme.style(Role::Label)),
                Span::styled(
                    list::tail(&sanitize_line(line), room),
                    theme.style(Role::Text),
                ),
                Span::styled("▌", theme.style(Role::Dim)),
            ]));
        }
        let parts: &[(&str, u8)] = if self.input_side {
            &[
                ("↑↓ select", 0),
                ("→ expand", 3),
                ("i output", 4),
                ("s steer", 2),
                (panes::ESC_BACK, 0),
            ]
        } else if self.following {
            &[
                ("↑↓ scroll", 0),
                ("i input", 8),
                ("s steer", 7),
                ("t take over", 4),
                ("r raw", 6),
                ("d diff", 5),
                ("← crew", 3),
                (panes::ESC_BACK, 0),
            ]
        } else {
            &[
                ("End follow", 0),
                ("↑↓ scroll", 9),
                ("i input", 8),
                ("s steer", 7),
                ("t take over", 4),
                ("r raw", 6),
                ("d diff", 5),
                (panes::ESC_BACK, 0),
            ]
        };
        Footer::hints(parts)
    }
}

/// The input side's first row: moved the least that shows the selected part's label, and a
/// few rows of its text when there's room.
fn input_top(top: usize, selected: usize, rows: usize, height: usize) -> usize {
    let height = height.max(1);
    let want_end = (selected + PART_LINES.min(height)).min(rows);
    let top = if selected < top {
        selected
    } else if want_end > top + height {
        want_end - height
    } else {
        top
    };
    top.min(rows.saturating_sub(height)).min(selected)
}

fn clean(text: &str) -> String {
    sanitize_line(text).trim().to_owned()
}

/// A dim row for text a view adds itself, such as a heading or a note.
fn note(text: impl Into<String>, role: Role, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(text.into(), theme.style(role)))
}

/// Every message, tool call with its result, diff, and subagent, as the chat draws them, and
/// under them what waits to be delivered.
fn output_lines(
    session: &SessionState,
    sent: Option<&str>,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let ctx = RenderCtx::new(width, theme, Clock::still(Instant::now())).with_expanded(true);
    let mut cells: Vec<Box<dyn HistoryCell>> = Vec::new();
    for tool in &session.tools {
        let mut call = tools::start_call(&session.vendor, &tool.name, &tool.input);
        if let Some(ok) = tool.ok {
            tools::finish_call(&mut call, ok, tool.output.as_deref());
            let shell = tools::kind_of(&session.vendor, &tool.name) == crate::cell::ToolKind::Shell;
            // A shell call's output is already under it; other calls say only what they did.
            if ok
                && !shell
                && let Some(output) = &tool.output
            {
                push_result(&mut call, output);
            }
        }
        cells.push(Box::new(call));
    }
    for diff in &session.diffs {
        let path = clean(&diff.path);
        let view = DiffView::from_unified(&diff.hunk).unwrap_or_default();
        let view = view.updated(path.clone(), diff.added as usize, diff.removed as usize);
        cells.push(Box::new(DiffCell {
            title: format!("Update({path})"),
            diff: view,
        }));
    }
    for message in &session.messages {
        cells.push(Box::new(AssistantMessage::new(message.clone())));
    }
    if !session.streaming.trim().is_empty() {
        cells.push(Box::new(AssistantMessage::new(session.streaming.clone())));
    }
    let mut lines = Vec::new();
    for cell in cells {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.extend(cell.lines(&ctx));
    }
    let tree = subagent_tree(&session.subagents, usize::from(width), theme);
    if !tree.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.extend(tree);
    }
    let waiting = queued_lines(&session.queued, sent, usize::from(width), theme);
    if !waiting.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.extend(waiting);
    }
    if lines.is_empty() {
        lines.push(note("Nothing from this agent yet", Role::Dim, theme));
    }
    lines
}

/// The result text under a call as one block, cut at [`RESULT_LINES`] with the count left out.
fn push_result(call: &mut crate::cell::ToolCall, output: &str) {
    let clean = sanitize(output);
    let lines: Vec<String> = clean.lines().map(str::to_owned).collect();
    let hidden = lines.len().saturating_sub(RESULT_LINES);
    call.push(ToolItem::Command {
        command: String::new(),
        output: lines.into_iter().take(RESULT_LINES).collect(),
        hidden,
    });
}

/// The subagents under their parents, each with its state and latest word.
fn subagent_tree(subagents: &[SubagentUpdate], width: usize, theme: &Theme) -> Vec<Line<'static>> {
    if subagents.is_empty() {
        return Vec::new();
    }
    let marks = Marks::for_theme(theme);
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!("{} ", theme.glyphs.bullet),
            theme.style(Role::Accent),
        ),
        Span::styled(
            panes::count(subagents.len(), "subagent"),
            theme.style(Role::Bright),
        ),
    ])];
    let known = |id: &str| subagents.iter().any(|sub| sub.id == id);
    // Children of a parent this list never saw hang at the top, so none goes missing.
    let mut stack: Vec<(&SubagentUpdate, usize)> = subagents
        .iter()
        .filter(|sub| sub.parent.as_deref().is_none_or(|parent| !known(parent)))
        .rev()
        .map(|sub| (sub, 0))
        .collect();
    let mut seen = Vec::new();
    while let Some((sub, depth)) = stack.pop() {
        if seen.contains(&sub.id) {
            continue;
        }
        seen.push(sub.id.clone());
        let (glyph, word, role) = match sub.status {
            SubagentStatus::Started => (marks.waiting, "started", Role::Dim),
            SubagentStatus::Running => (marks.running, "running", Role::Accent),
            SubagentStatus::Done => (marks.done, "done", Role::Success),
            SubagentStatus::Failed => (marks.failed, "failed", Role::Error),
            SubagentStatus::Unknown => (marks.waiting, "state unknown", Role::Dim),
        };
        let label = sub
            .label
            .as_deref()
            .map(clean)
            .unwrap_or_else(|| format!("{:?}", sub.kind).to_lowercase());
        let mut detail = vec![word.to_owned()];
        if let Some(text) = sub.last_message.as_deref().or(sub.prompt.as_deref()) {
            let text = clean(text);
            if !text.is_empty() {
                detail.push(text);
            }
        }
        if !sub.steps_visible {
            detail.push("its steps don't stream here".to_owned());
        }
        let indent = 2 + depth * 3;
        let first = vec![
            Span::raw(" ".repeat(indent)),
            Span::styled(format!("{} ", theme.glyphs.result), theme.style(Role::Dim)),
            Span::styled(format!("{glyph} "), theme.style(role)),
            Span::styled(label, theme.style(Role::Text)),
            Span::styled(" · ", theme.style(Role::Dim)),
        ];
        let rest = vec![Span::raw(" ".repeat(indent + 5))];
        lines.extend(wrap_text(
            &detail.join(" · "),
            theme.style(Role::Dim),
            width,
            &first,
            &rest,
        ));
        let children: Vec<&SubagentUpdate> = subagents
            .iter()
            .filter(|child| child.parent.as_deref() == Some(sub.id.as_str()))
            .collect();
        stack.extend(children.into_iter().rev().map(|child| (child, depth + 1)));
    }
    lines
}

fn delivery_words(delivery: Delivery) -> &'static str {
    match delivery {
        Delivery::Steer => "arrives after the current tool call",
        Delivery::NextTurn => "arrives at its next turn",
        Delivery::SessionStart => "arrives when its next session starts",
    }
}

/// Messages that wait to reach the agent, and a steer just sent that doesn't show yet.
fn queued_lines(
    queued: &[QueuedMessage],
    sent: Option<&str>,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let dim = theme.style(Role::Dim);
    for message in queued {
        let first = vec![Span::styled(
            format!("{} Queued ", theme.glyphs.result),
            dim,
        )];
        let rest = vec![Span::raw("  ")];
        lines.extend(wrap_text(
            &clean(&message.text),
            theme.style(Role::Text),
            width,
            &first,
            &rest,
        ));
        lines.push(Line::from(Span::styled(
            format!("  {}", delivery_words(message.delivery)),
            dim,
        )));
    }
    if let Some(text) = sent
        && !queued.iter().any(|message| message.text.trim() == text)
    {
        let first = vec![Span::styled(format!("{} Sent ", theme.glyphs.result), dim)];
        lines.extend(wrap_text(
            &clean(text),
            theme.style(Role::Text),
            width,
            &first,
            &[Span::raw("  ")],
        ));
    }
    lines
}

fn prompt_via(via: PromptVia) -> &'static str {
    match via {
        PromptVia::Argument => "prompt as an argument",
        PromptVia::Stdin => "prompt on stdin",
        PromptVia::StreamInput => "prompt on an open input stream",
        PromptVia::Request => "prompt in the request body",
    }
}

/// An argument as typed at a shell, quoted only when it has to be, so the line pastes back as
/// the same argv. Line breaks fold into spaces, the one change it makes.
fn shell_arg(arg: &str) -> String {
    shell_words::quote(&sanitize_line(arg)).into_owned()
}

/// The launch command, then each part with its label and token estimate, then queued messages.
fn input_lines(
    session: &SessionState,
    part: &ListState,
    expanded: Option<usize>,
    width: usize,
    theme: &Theme,
) -> Body {
    let dim = theme.style(Role::Dim);
    let text = theme.style(Role::Text);
    let marks = Marks::for_theme(theme);
    if session.inputs.is_empty() {
        return Body {
            lines: vec![note("Nothing sent to this agent yet", Role::Dim, theme)],
            selected_row: 0,
        };
    }
    let mut lines = Vec::new();
    let mut selected_row = 0;
    let mut index = 0;
    for (n, input) in session.inputs.iter().enumerate() {
        if n == 0 {
            let launch = &input.launch;
            let command = std::iter::once(launch.program.as_str())
                .chain(launch.args.iter().map(String::as_str))
                .map(shell_arg)
                .collect::<Vec<_>>()
                .join(" ");
            lines.extend(wrap_text(
                &command,
                text,
                width,
                &[Span::styled("$ ", dim)],
                &[Span::raw("    ")],
            ));
            let mut facts = vec![
                format!("cwd {}", clean(&launch.cwd.to_string_lossy())),
                prompt_via(launch.prompt_via).to_owned(),
            ];
            if let Some(sandbox) = &launch.sandbox {
                facts.push(format!("sandbox {}", clean(sandbox)));
            }
            if !launch.env_set.is_empty() {
                let names: Vec<String> = launch.env_set.iter().map(|name| clean(name)).collect();
                facts.push(format!("sets {}", names.join(", ")));
            }
            if !launch.env_removed.is_empty() {
                let names: Vec<String> =
                    launch.env_removed.iter().map(|name| clean(name)).collect();
                facts.push(format!("clears {}", names.join(", ")));
            }
            lines.extend(wrap_text(
                &facts.join(" · "),
                dim,
                width,
                &[Span::raw("  ")],
                &[Span::raw("  ")],
            ));
        } else {
            lines.push(Line::default());
            lines.push(note(format!("Added by delivery {n}"), Role::Bright, theme));
        }
        for item in &input.parts {
            lines.push(Line::default());
            let selected = index == part.selected;
            if selected {
                selected_row = lines.len();
            }
            lines.extend(part_lines(
                item,
                selected,
                expanded == Some(index),
                width,
                theme,
                &marks,
            ));
            index += 1;
        }
    }
    if !session.queued.is_empty() {
        lines.push(Line::default());
        lines.extend(queued_lines(&session.queued, None, width, theme));
    }
    Body {
        lines,
        selected_row,
    }
}

/// A part's label row, then its text word for word, each line ending its own row.
fn part_lines(
    part: &InputPart,
    selected: bool,
    expanded: bool,
    width: usize,
    theme: &Theme,
    marks: &Marks,
) -> Vec<Line<'static>> {
    let dim = theme.style(Role::Dim);
    let mut head = vec![
        if selected {
            Span::styled(format!("{} ", marks.selected), theme.style(Role::Label))
        } else {
            Span::raw("  ")
        },
        Span::styled(
            clean(&part.label),
            theme.style(if selected { Role::Label } else { Role::Bright }),
        ),
        Span::styled(format!(" · ~{} tok", part.approx_tokens), dim),
    ];
    if let Some(source) = &part.source {
        head.push(Span::styled(format!(" · from {}", clean(source)), dim));
    }
    let mut lines = vec![Line::from(head)];
    let body = sanitize(&part.text);
    let rows: Vec<&str> = body.lines().collect();
    let shown = if expanded {
        rows.len()
    } else {
        rows.len().min(PART_LINES)
    };
    let text = theme.style(Role::Text);
    for row in &rows[..shown] {
        lines.extend(wrap_text(
            row,
            text,
            width,
            &[Span::raw("    ")],
            &[Span::raw("    ")],
        ));
    }
    if shown < rows.len() {
        lines.push(Line::from(Span::styled(
            format!("    … +{} lines · → expands", rows.len() - shown),
            dim,
        )));
    }
    lines
}

impl View for SessionView {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let theme = ctx.theme;
        let width = usize::from(area.width);
        let [header, sides, rule, body_area, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(area);
        let Some(session) = ctx.state.session(&self.run, &self.session) else {
            let gone = note(
                "This session isn't on screen anymore. Esc goes back",
                Role::Dim,
                theme,
            );
            panes::put(buf, header.x, header.y, header.width, &gone);
            self.ticking = false;
            return;
        };
        self.ticking = session.is_running();
        panes::put(
            buf,
            header.x,
            header.y,
            header.width,
            &self.header(session, ctx.state, width, theme),
        );
        panes::put(
            buf,
            sides.x,
            sides.y,
            sides.width,
            &self.sides_line(width, theme),
        );
        let line = theme.glyphs.rule.repeat(width);
        panes::put(
            buf,
            rule.x,
            rule.y,
            rule.width,
            &note(line, Role::Rule, theme),
        );

        let Body {
            lines,
            selected_row,
        } = self.body(session, area.width, theme);
        let height = usize::from(body_area.height);
        self.page = height.max(1);
        let top = if self.input_side {
            self.input_top = input_top(self.input_top, selected_row, lines.len(), height);
            self.input_top
        } else {
            self.rows = lines.len();
            let top = self.top(lines.len(), height);
            if !self.following {
                self.offset = top;
            }
            top
        };
        panes::put_lines(buf, body_area, &lines[top.min(lines.len())..]);

        let marks = Marks::for_theme(theme);
        let mark = if self.input_side {
            String::new()
        } else {
            super::pager::position_mark(top, height, lines.len(), marks.down)
        };
        // The steer input leaves room for the mark beside it.
        let room = match str_width(&mark) {
            0 => width,
            mark => width.saturating_sub(mark + 2),
        };
        panes::draw_footer(buf, footer, self.footer(room, theme), &mark, theme);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        if self.steer.is_some() {
            return self.steer_key(key);
        }
        let state = ctx.state;
        let session = state.session(&self.run, &self.session);
        // `h` goes back to the crew, as ← does.
        let key = list::standard(key, list::vim(state), KeyCode::Left);
        // Ctrl and Alt letters are global keys, such as Ctrl+D to exit.
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => return ViewOutcome::Close,
            KeyCode::Char('i') if plain => {
                self.input_side = !self.input_side;
                return ViewOutcome::Consumed;
            }
            KeyCode::Char('s') if plain => {
                self.steer = Some(String::new());
                return ViewOutcome::Consumed;
            }
            KeyCode::Char('t') if plain => {
                return match session.and_then(|session| session.card.clone()) {
                    Some(card) => ViewOutcome::ConfirmTakeOver {
                        run: self.run.clone(),
                        card,
                    },
                    None => ViewOutcome::Notice("This agent has no card to take over".into()),
                };
            }
            KeyCode::Char('r') if plain => {
                return ViewOutcome::Request(ViewRequest::Raw {
                    run: self.run.clone(),
                    session: self.session.clone(),
                });
            }
            KeyCode::Char('d') if plain => {
                return match session.and_then(|session| session.card.clone()) {
                    Some(card) => ViewOutcome::Request(ViewRequest::Diff {
                        run: self.run.clone(),
                        card,
                        scope: DiffScope::Card,
                    }),
                    None => ViewOutcome::Notice("This agent has no card, so no diff".into()),
                };
            }
            KeyCode::Left if self.input_side && self.expanded.is_some() => {
                self.expanded = None;
                return ViewOutcome::Consumed;
            }
            KeyCode::Left => return self.back_to_crew(state),
            KeyCode::Right | KeyCode::Enter if self.input_side => {
                let selected = self.part.selected;
                self.expanded = if self.expanded == Some(selected) {
                    None
                } else {
                    Some(selected)
                };
                return ViewOutcome::Consumed;
            }
            _ => {}
        }
        let Some(nav) = list::nav_key(&key, self.page) else {
            return ViewOutcome::Ignored;
        };
        if self.input_side {
            let parts = session.map_or(0, Self::parts);
            self.part.apply(nav, parts, 1);
            if self.expanded.is_some_and(|open| open != self.part.selected) {
                self.expanded = None;
            }
        } else {
            self.scroll(nav);
        }
        ViewOutcome::Consumed
    }

    fn next_change(&self, now: Instant) -> Option<Instant> {
        self.ticking.then(|| now + Duration::from_secs(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(rows: usize, page: usize) -> SessionView {
        let mut view = SessionView::new(
            RunId::new("run-01").unwrap(),
            SessionKey::parse("s-001").unwrap(),
        );
        view.rows = rows;
        view.page = page;
        view
    }

    #[test]
    fn ctrl_letters_are_left_to_the_global_keys() {
        let state = AppState::default();
        let theme = Theme::dark();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        let mut view = view(10, 10);
        for c in ['d', 't', 'r', 's', 'i'] {
            let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
            assert!(
                matches!(view.handle_key(key, &ctx), ViewOutcome::Ignored),
                "Ctrl+{c}"
            );
        }
        assert!(view.steer.is_none() && !view.input_side);
    }

    #[test]
    fn vim_keys_scroll_and_h_goes_back_only_when_on() {
        let theme = Theme::dark();
        let press = |view: &mut SessionView, c: char, vim: bool| {
            let state = list::keys_state(vim);
            let ctx = ViewCtx {
                theme: &theme,
                state: &state,
                now: Instant::now(),
            };
            view.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &ctx)
        };
        let mut view = view(100, 20);
        for c in ['j', 'k', 'g', 'G', 'h', 'l'] {
            assert!(
                matches!(press(&mut view, c, false), ViewOutcome::Ignored),
                "{c}"
            );
        }
        assert!(view.following);

        press(&mut view, 'g', true);
        assert_eq!(view.top(100, 20), 0);
        press(&mut view, 'j', true);
        assert_eq!(view.top(100, 20), 1);
        press(&mut view, 'k', true);
        assert_eq!(view.top(100, 20), 0);
        press(&mut view, 'G', true);
        assert!(view.following);
        // `h` is ←, back to the crew view this one replaced.
        assert!(matches!(
            press(&mut view, 'h', true),
            ViewOutcome::Replace(_)
        ));
    }

    #[test]
    fn launch_args_paste_back_as_the_same_argv() {
        let args = [
            "claude",
            "-p",
            "it's done",
            "effort=\"low\"",
            "$HOME",
            "a;b",
            "*.rs",
            "x|y",
            "&",
            "`ls`",
            "",
            "plain",
        ];
        let line = args.iter().map(|arg| shell_arg(arg)).collect::<Vec<_>>();
        assert_eq!(line[0], "claude");
        assert_eq!(line[11], "plain");
        assert_eq!(shell_words::split(&line.join(" ")).unwrap(), args);
    }

    #[test]
    fn follow_mode_pauses_on_scroll_and_resumes_on_end() {
        let mut view = view(100, 20);
        assert!(view.following);
        assert_eq!(view.top(100, 20), 80);

        view.scroll(Nav::By(-20));
        assert!(!view.following, "PgUp pauses");
        assert_eq!(view.top(100, 20), 60);
        // New rows arrive below; a paused view stays where you left it.
        view.rows = 130;
        assert_eq!(view.top(130, 20), 60);

        view.scroll(Nav::By(-1));
        assert_eq!(view.top(130, 20), 59);
        view.scroll(Nav::Bottom);
        assert!(view.following, "End resumes");
        assert_eq!(view.top(130, 20), 110);

        // Scrolling back down to the live end resumes too.
        view.scroll(Nav::By(-5));
        assert!(!view.following);
        view.scroll(Nav::By(5));
        assert!(view.following);
        view.scroll(Nav::Top);
        assert!(!view.following);
        assert_eq!(view.top(130, 20), 0);
    }
}
