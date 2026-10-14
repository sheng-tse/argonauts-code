pub(super) mod body;
mod squeeze;
mod stats;

use std::time::{Duration, Instant};

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::diff::DiffView;
use super::group::CollapsedGroup;
use super::{HistoryCell, Tone};
use crate::format;
use crate::render::RenderCtx;
use crate::text::marks;
use crate::text::{
    fit_lines, sanitize_line, spans_width, str_width, truncate_line, truncate_spans, wrap_spans,
};
use crate::theme::{AgentColor, ColorLevel, Role};

pub use stats::ToolStats;

/// How long a running call's bullet shows, then hides.
pub const BLINK_PERIOD: Duration = Duration::from_millis(600);

/// A tool call, a running shell command, or an agent session.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// The host's id for finding this call again. Never shown.
    pub key: Option<String>,
    pub kind: ToolKind,
    pub title: ToolTitle,
    pub state: ToolState,
    pub stats: ToolStats,
    pub body: Vec<ToolItem>,
    /// The whole call, when this one is its summary. The transcript overlay shows this instead.
    pub full: Option<Box<ToolCall>>,
    /// Shown in the transcript overlay, like `10:58 AM codex/gpt-5.6`.
    pub stamp: Option<String>,
    /// Rides after the header's stats, such as `· retrying in 12s (attempt 2)`.
    pub note: Option<HeaderNote>,
    /// The body wraps at this many columns however wide the screen is, as the Triage item's
    /// lines wrap at 100.
    pub wrap_at: Option<u16>,
    /// Dim words after the title, such as ` · orchestrator · ctrl+t to open`. The first thing
    /// to go when the header has no room.
    pub tail: Option<String>,
    /// A failed agent's row, marked `✗` rather than a red bullet, as one that gave up says so.
    pub crossed: bool,
    /// What an edit shows only once it ends, and its words until then.
    pub held: Option<Box<Held>>,
}

/// An edit's preview drawn from its input waits for the end, where the patch its result
/// reports takes its place, so the diff keeps one shape from start to finish.
#[derive(Debug, Clone, PartialEq)]
pub struct Held {
    /// Under the title while the call runs and has nothing else to show, such as `Updating…`.
    pub waiting: String,
    /// Shown once the call ends well, unless its result brought its own.
    pub items: Vec<ToolItem>,
}

/// Worked out at each frame, so a countdown counts and a quiet spell grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderNote {
    /// `· retrying in 12s (attempt 2)` in the warning color.
    Retrying { at: Option<Instant>, attempt: u32 },
    /// `· quiet 2m · may be waiting on a limit`, for harnesses that wait out limits without a
    /// word. Dim once nothing has come for a while, then the warning color.
    Quiet { since: Instant },
}

/// A quiet spell shorter than this is just thinking.
pub const QUIET_AFTER: Duration = Duration::from_secs(30);

/// Past this a quiet session looks stalled.
pub const QUIET_WARN_AFTER: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolTitle {
    Call {
        name: String,
        args: String,
    },
    Text(String),
    /// Reads `Opus 5.5 on Claude Code · owner · card-002 (Add the JSON writer)`.
    Agent {
        /// Its display name, such as `Opus 5.5 on Claude Code`.
        name: String,
        color: Option<AgentColor>,
        /// Such as `owner · card-002`.
        role: String,
        task: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum ToolState {
    #[default]
    Running,
    Done,
    Failed,
    /// By Esc or a stop, before it finished. Its work so far stays.
    Stopped,
    /// Its CLI's permission rules turned it down, so nothing ran. Not a failure: it reads dim.
    Refused,
    /// The model is still writing the call: its name shows with a steady dim bullet, and the
    /// rest fills in once the call is whole and runs.
    Writing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum ToolKind {
    Read,
    Search,
    List,
    Shell,
    Edit,
    Write,
    Fetch,
    Mcp,
    Agent,
    #[default]
    Other,
}

impl ToolKind {
    /// Quiet calls fold into collapsed groups. Edits and writes stay visible with their diffs.
    pub fn is_quiet(self) -> bool {
        matches!(
            self,
            ToolKind::Read | ToolKind::Search | ToolKind::List | ToolKind::Shell | ToolKind::Fetch
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolItem {
    /// Slash commands in it show in the code color.
    Text {
        text: String,
        tone: Tone,
    },
    /// A workflow's phases as it declares them, numbered, such as `2. ★ Secrets scan  the
    /// reviewers scan the change`. The details line up and wrap under their own column, a step
    /// you require carries the mark, and a list with one ends by saying what the mark means.
    Phases(Vec<PhaseLine>),
    /// The phases on their line, then a dim note beside them, such as who reviews.
    PhasesBeside {
        phases: Vec<PhaseLine>,
        beside: String,
    },
    /// `hidden` counts output lines left out, shown as `… +N lines`.
    Command {
        command: String,
        output: Vec<String>,
        hidden: usize,
    },
    Diff(DiffView),
    Markdown(String),
    Call(ToolCall),
    Group(CollapsedGroup),
    /// Squeezed into a live block it becomes one line, like `◧ Writing the JSON writer · 2/5 done`.
    Todos(Vec<Todo>),
    /// A live block's to-do line, `☒ wire middleware  ☐ tests`, always one row.
    TodoLine(Vec<Todo>),
    /// A workflow's steps on one row, each after its state's mark: `✓ build · ◐ review · ○ judge`.
    Steps(Vec<StepMark>),
    /// The newest line of text an agent is writing, with a cursor after it.
    Streaming(String),
    /// `Running… (4s)` while a call waits for its first output. It goes when anything else
    /// arrives. `background` adds the `(ctrl+b to run in background)` row Claude Code puts under
    /// the last of several calls running at once.
    Running {
        since: Instant,
        background: bool,
    },
}

/// One phase of a workflow, as the chat lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseLine {
    pub title: String,
    /// One line on who does what. Empty when the workflow gave none.
    pub detail: String,
    /// A step you require of the workflow.
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepMark {
    pub label: String,
    pub state: crate::widgets::State,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Todo {
    pub text: String,
    pub state: TodoState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum TodoState {
    #[default]
    Pending,
    InProgress,
    Done,
}

impl ToolTitle {
    pub fn plain(&self) -> String {
        match self {
            ToolTitle::Call { name, args } if args.is_empty() => name.clone(),
            ToolTitle::Call { name, args } => format!("{name}({args})"),
            ToolTitle::Text(text) => text.clone(),
            ToolTitle::Agent {
                name, role, task, ..
            } => {
                let mut text = name.trim().to_owned();
                if !role.trim().is_empty() {
                    text.push_str(" · ");
                    text.push_str(role.trim());
                }
                if !task.trim().is_empty() {
                    text.push_str(&format!(" ({})", task.trim()));
                }
                text
            }
        }
    }
}

impl ToolItem {
    pub fn text(text: impl Into<String>) -> Self {
        ToolItem::Text {
            text: text.into(),
            tone: Tone::Dim,
        }
    }
}

impl ToolCall {
    /// Every constructor starts the call running.
    pub fn call(kind: ToolKind, name: impl Into<String>, args: impl Into<String>) -> Self {
        Self::new(
            kind,
            ToolTitle::Call {
                name: name.into(),
                args: args.into(),
            },
        )
    }

    pub fn described(kind: ToolKind, text: impl Into<String>) -> Self {
        Self::new(kind, ToolTitle::Text(text.into()))
    }

    /// `name` is the agent's display name, such as `Opus 5.5 on Claude Code`.
    pub fn agent(
        name: impl Into<String>,
        color: Option<AgentColor>,
        role: impl Into<String>,
        task: impl Into<String>,
    ) -> Self {
        Self::new(
            ToolKind::Agent,
            ToolTitle::Agent {
                name: name.into(),
                color,
                role: role.into(),
                task: task.into(),
            },
        )
    }

    fn new(kind: ToolKind, title: ToolTitle) -> Self {
        Self {
            key: None,
            kind,
            title,
            state: ToolState::Running,
            stats: ToolStats::default(),
            body: Vec::new(),
            full: None,
            stamp: None,
            note: None,
            wrap_at: None,
            tail: None,
            crossed: false,
            held: None,
        }
    }

    pub fn with_tail(mut self, tail: impl Into<String>) -> Self {
        self.tail = Some(tail.into());
        self
    }

    pub fn with_wrap_at(mut self, columns: u16) -> Self {
        self.wrap_at = Some(columns);
        self
    }

    pub fn with_stamp(mut self, stamp: impl Into<String>) -> Self {
        self.stamp = Some(stamp.into());
        self
    }

    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// A call the model is still writing, known only by its name so far.
    pub fn writing(kind: ToolKind, name: impl Into<String>) -> Self {
        Self::call(kind, name, String::new()).with_state(ToolState::Writing)
    }

    pub fn with_state(mut self, state: ToolState) -> Self {
        self.state = state;
        self
    }

    pub fn with_item(mut self, item: ToolItem) -> Self {
        self.body.push(item);
        self
    }

    pub fn with_stats(mut self, stats: ToolStats) -> Self {
        self.stats = stats;
        self
    }

    /// `at` is when the next attempt starts, if the vendor said.
    pub fn retrying(&mut self, at: Option<Instant>, attempt: u32) {
        self.note = Some(HeaderNote::Retrying { at, attempt });
    }

    /// Nothing has arrived since `since`.
    pub fn quiet_since(&mut self, since: Instant) {
        self.note = Some(HeaderNote::Quiet { since });
    }

    /// Anything but another `Running…` row replaces the one waiting for output.
    pub fn push(&mut self, item: ToolItem) {
        if !matches!(item, ToolItem::Running { .. }) {
            self.body
                .retain(|item| !matches!(item, ToolItem::Running { .. }));
        }
        self.body.push(item);
    }

    /// A quiet call joins the collapsed group at the end of the body, or starts one.
    pub fn push_call(&mut self, call: ToolCall) {
        if !call.kind.is_quiet() {
            self.body.push(ToolItem::Call(call));
            return;
        }
        match self.body.last_mut() {
            Some(ToolItem::Group(group)) => group.push(call),
            _ => {
                let mut group = CollapsedGroup::new();
                group.push(call);
                self.body.push(ToolItem::Group(group));
            }
        }
    }

    /// A call on a row of its own, never folded with the quiet calls beside it.
    pub fn push_alone(&mut self, call: ToolCall) {
        self.body.push(ToolItem::Call(call));
    }

    /// Takes the call with `key` out of the fold line it sits in, leaving the line when it
    /// holds others. `None` when no fold line holds it.
    pub fn take_from_fold(&mut self, key: &str) -> Option<ToolCall> {
        let (at, index) = self
            .body
            .iter()
            .enumerate()
            .find_map(|(at, item)| match item {
                ToolItem::Group(group) => group
                    .calls
                    .iter()
                    .position(|call| call.key.as_deref() == Some(key))
                    .map(|index| (at, index)),
                _ => None,
            })?;
        let ToolItem::Group(group) = &mut self.body[at] else {
            return None;
        };
        let call = group.calls.remove(index);
        if group.calls.is_empty() {
            self.body.remove(at);
        }
        Some(call)
    }

    /// Searches nested calls at every depth, inside collapsed groups too.
    pub fn child_mut(&mut self, key: &str) -> Option<&mut ToolCall> {
        for item in &mut self.body {
            let calls: &mut [ToolCall] = match item {
                ToolItem::Call(call) => std::slice::from_mut(call),
                ToolItem::Group(group) => &mut group.calls,
                _ => continue,
            };
            for call in calls {
                if call.key.as_deref() == Some(key) {
                    return Some(call);
                }
                if let Some(found) = call.child_mut(key) {
                    return Some(found);
                }
            }
        }
        None
    }

    /// Takes out the call under it with `key`, at any depth, when the model never finished
    /// writing it. A collapsed group left empty goes too. True when one went.
    pub fn drop_child(&mut self, key: &str) -> bool {
        let mut dropped = false;
        self.body.retain_mut(|item| {
            if dropped {
                return true;
            }
            match item {
                ToolItem::Call(call) if call.key.as_deref() == Some(key) => {
                    dropped = true;
                    false
                }
                ToolItem::Call(call) => {
                    dropped = call.drop_child(key);
                    true
                }
                ToolItem::Group(group) => {
                    let before = group.calls.len();
                    group.calls.retain(|call| call.key.as_deref() != Some(key));
                    dropped = group.calls.len() < before
                        || group.calls.iter_mut().any(|call| call.drop_child(key));
                    !group.calls.is_empty()
                }
                _ => true,
            }
        });
        dropped
    }

    pub fn finish(&mut self, ok: bool) {
        self.end(if ok {
            ToolState::Done
        } else {
            ToolState::Failed
        });
    }

    /// Its CLI turned it down before it ran.
    pub fn refuse(&mut self) {
        self.end(ToolState::Refused);
    }

    /// Stops every call still running inside it too.
    pub fn stop(&mut self) {
        if self.is_running() {
            self.end(ToolState::Stopped);
        }
        for item in &mut self.body {
            match item {
                ToolItem::Call(child) => child.stop(),
                ToolItem::Group(group) => group.calls.iter_mut().for_each(ToolCall::stop),
                _ => {}
            }
        }
    }

    fn end(&mut self, state: ToolState) {
        self.state = state;
        // A finished call no longer waits for output.
        self.body
            .retain(|item| !matches!(item, ToolItem::Running { .. }));
    }

    /// Running, or still being written.
    pub fn is_running(&self) -> bool {
        matches!(self.state, ToolState::Running | ToolState::Writing)
    }

    /// The call as the model finished writing it takes this one's place, which keeps its key
    /// and where it stands. Its row is already on screen, so nothing moves.
    pub fn fill_in(&mut self, whole: ToolCall) {
        let key = self.key.take();
        *self = ToolCall { key, ..whole };
    }

    /// Folds a finished agent session's body into one line, `Done · 12 tool uses · {detail}`.
    /// The live stats go, since `detail` says them. The whole call stays for the overlay.
    pub fn summarized(&self, detail: Option<&str>) -> ToolCall {
        let outcome = match self.state {
            ToolState::Running | ToolState::Writing => "Running",
            ToolState::Done => "Done",
            ToolState::Failed => "Failed",
            ToolState::Stopped => "Stopped",
            ToolState::Refused => "Not allowed",
        };
        let uses = self.tool_uses();
        let unit = if uses == 1 { "tool use" } else { "tool uses" };
        let mut text = format!("{outcome} · {uses} {unit}");
        if let Some(detail) = detail.filter(|detail| !detail.trim().is_empty()) {
            text.push_str(" · ");
            text.push_str(detail);
        }
        ToolCall {
            key: self.key.clone(),
            kind: self.kind,
            title: self.title.clone(),
            state: self.state,
            stats: ToolStats::default(),
            body: vec![ToolItem::text(text)],
            full: Some(Box::new(self.clone())),
            stamp: self.stamp.clone(),
            note: None,
            wrap_at: self.wrap_at,
            tail: self.tail.clone(),
            crossed: false,
            held: None,
        }
    }

    fn tool_uses(&self) -> usize {
        let count = |call: &ToolCall| 1 + call.tool_uses();
        self.body
            .iter()
            .map(|item| match item {
                ToolItem::Call(call) => count(call),
                ToolItem::Group(group) => group.calls.iter().map(count).sum(),
                _ => 0,
            })
            .sum()
    }

    fn bullet(&self, ctx: &RenderCtx<'_>) -> Span<'static> {
        let theme = ctx.theme;
        match self.state {
            // An agent's block keeps the quiet bullet; a call's own row blinks in color.
            ToolState::Running if matches!(self.title, ToolTitle::Agent { .. }) => {
                running_bullet(ctx)
            }
            ToolState::Running => working_bullet(ctx),
            ToolState::Writing => marks::decorative(theme.glyphs.bullet, theme.style(Role::Dim)),
            ToolState::Stopped => {
                marks::decorative(theme.glyphs.bullet, theme.style(Role::Warning))
            }
            ToolState::Done => marks::decorative(theme.glyphs.bullet, theme.style(Role::Success)),
            ToolState::Failed if self.crossed => {
                marks::decorative(theme.glyphs.not_ready, theme.style(Role::Error))
            }
            ToolState::Failed => marks::decorative(theme.glyphs.bullet, theme.style(Role::Error)),
            ToolState::Refused => marks::decorative(theme.glyphs.bullet, theme.style(Role::Dim)),
        }
    }

    /// Without color a failed or stopped call says so in words, since only the bullet's color
    /// would.
    fn failed_word(&self, ctx: &RenderCtx<'_>) -> Option<Span<'static>> {
        if ctx.theme.level != ColorLevel::None {
            return None;
        }
        match self.state {
            ToolState::Failed => Some(Span::styled(" failed", ctx.theme.style(Role::Error))),
            ToolState::Stopped => Some(Span::styled(" stopped", ctx.theme.style(Role::Warning))),
            ToolState::Running | ToolState::Writing | ToolState::Done | ToolState::Refused => None,
        }
    }

    /// Long args hang under the `(`. A header with live stats, or an agent session's, stays on
    /// one row.
    pub(super) fn header(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let mut lines = self.title_rows(ctx);
        // The tail rides on a header of one row that has room for it, else it gives way.
        if let (Some(tail), [line]) = (&self.tail, lines.as_mut_slice()) {
            let tail = sanitize_line(tail).into_owned();
            if spans_width(&line.spans) + str_width(&tail) <= usize::from(ctx.width) {
                line.spans
                    .push(Span::styled(tail, ctx.theme.style(Role::Dim)));
            }
        }
        lines
    }

    fn title_rows(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let trails = self.trails(ctx);
        if !trails[0].is_empty() || matches!(self.title, ToolTitle::Agent { .. }) {
            return vec![self.one_row_header(ctx, &trails)];
        }
        let theme = ctx.theme;
        let width = usize::from(ctx.width);
        let bullet = theme.glyphs.bullet;
        let mut first = vec![self.bullet(ctx), Span::raw(" ")];
        let text = theme.style(Role::Text);
        let mut content = Vec::new();
        let hang = match &self.title {
            ToolTitle::Call { name, args } => {
                let name = sanitize_line(name).into_owned();
                let hang = str_width(bullet) + 1 + str_width(&name);
                first.push(Span::styled(name, text.add_modifier(Modifier::BOLD)));
                let args = sanitize_line(args);
                if !args.is_empty() {
                    content.push(Span::styled(format!("({args})"), text));
                }
                hang
            }
            ToolTitle::Text(title) | ToolTitle::Agent { name: title, .. } => {
                content.push(Span::styled(sanitize_line(title).into_owned(), text));
                str_width(bullet) + 1
            }
        };
        content.extend(self.failed_word(ctx));
        let indent = Span::raw(" ".repeat(hang));
        wrap_spans(&content, width, &first, std::slice::from_ref(&indent))
    }

    /// What may follow the title, most wanted first: the stats and the note, then a shorter
    /// note, then the note alone, which says more than the time does, then nothing.
    fn trails(&self, ctx: &RenderCtx<'_>) -> Vec<Vec<Span<'static>>> {
        let stats: Vec<Span<'static>> = self.stats.span(ctx).into_iter().collect();
        let mut trails = Vec::new();
        match self.note_spans(ctx) {
            Some((full, short)) => {
                trails.push(stats.iter().cloned().chain([full]).collect());
                trails.push(stats.iter().cloned().chain([short.clone()]).collect());
                trails.push(vec![short]);
            }
            None => trails.push(stats),
        }
        trails.push(Vec::new());
        trails
    }

    /// The note in full and cut short.
    fn note_spans(&self, ctx: &RenderCtx<'_>) -> Option<(Span<'static>, Span<'static>)> {
        let theme = ctx.theme;
        let now = ctx.clock.now;
        match self.note? {
            HeaderNote::Retrying { at, attempt } => {
                let mut short = String::from(" · retrying");
                if let Some(wait) = at
                    .map(|at| at.saturating_duration_since(now))
                    .filter(|wait| wait.as_secs() > 0)
                {
                    short.push_str(" in ");
                    short.push_str(&format::duration(wait));
                }
                let mut full = short.clone();
                if attempt > 0 {
                    full.push_str(&format!(" (attempt {attempt})"));
                }
                let style = theme.style(Role::Warning);
                Some((Span::styled(full, style), Span::styled(short, style)))
            }
            HeaderNote::Quiet { since } => {
                let quiet = now.saturating_duration_since(since);
                if quiet < QUIET_AFTER {
                    return None;
                }
                let spell = if quiet.as_secs() < 60 {
                    format!("{}s", quiet.as_secs())
                } else {
                    format!("{}m", quiet.as_secs() / 60)
                };
                let role = if quiet >= QUIET_WARN_AFTER {
                    Role::Warning
                } else {
                    Role::Dim
                };
                let short = format!(" · quiet {spell}");
                let full = format!("{short} · may be waiting on a limit");
                let style = theme.style(role);
                Some((Span::styled(full, style), Span::styled(short, style)))
            }
        }
    }

    /// Never wraps, so a row can't start with the stats' `·`.
    fn one_row_header(&self, ctx: &RenderCtx<'_>, trails: &[Vec<Span<'static>>]) -> Line<'static> {
        let theme = ctx.theme;
        let width = usize::from(ctx.width);
        let text = theme.style(Role::Text);
        let mut fixed = vec![self.bullet(ctx), Span::raw(" ")];
        let (cuttable, open, close) = match &self.title {
            ToolTitle::Call { name, args } => {
                let name = sanitize_line(name).into_owned();
                fixed.push(Span::styled(name, text.add_modifier(Modifier::BOLD)));
                (sanitize_line(args).into_owned(), "(", ")")
            }
            // A description in parentheses at the end is what gets cut, so `)` stays.
            ToolTitle::Text(title) => {
                let title = sanitize_line(title).into_owned();
                match title
                    .strip_suffix(')')
                    .and_then(|rest| rest.rsplit_once(" ("))
                {
                    Some((head, task)) => {
                        fixed.push(Span::styled(head.to_owned(), text));
                        (task.to_owned(), " (", ")")
                    }
                    None => (title, "", ""),
                }
            }
            ToolTitle::Agent {
                name,
                color: _,
                role,
                task,
            } => {
                // The label reads bold and who runs it dim, as Claude Code heads an agent's row.
                fixed.push(Span::styled(
                    sanitize_line(name).trim().to_owned(),
                    text.add_modifier(Modifier::BOLD),
                ));
                let role = sanitize_line(role);
                if !role.trim().is_empty() {
                    fixed.push(Span::styled(
                        format!(" · {}", role.trim()),
                        theme.style(Role::Dim),
                    ));
                }
                (sanitize_line(task).trim().to_owned(), " (", ")")
            }
        };
        let failed: Vec<Span<'static>> = self.failed_word(ctx).into_iter().collect();
        let row = |cut: Option<usize>, stats: &[Span<'static>]| {
            let mut spans = fixed.clone();
            if !cuttable.is_empty() && cut != Some(0) {
                let part = [Span::styled(cuttable.clone(), text)];
                let shown = match cut {
                    Some(room) => truncate_spans(&part, room),
                    None => part.to_vec(),
                };
                spans.push(Span::styled(open, text));
                spans.extend(shown);
                spans.push(Span::styled(close, text));
            }
            spans.extend(failed.iter().cloned());
            spans.extend(stats.iter().cloned());
            spans
        };
        // The task shrinks, then goes, before the stats do.
        let full = row(None, &trails[0]);
        if spans_width(&full) <= width {
            return Line::from(full);
        }
        let frame = str_width(open) + str_width(close);
        // Under a set width every agent header drops its task, so headers side by side read
        // alike instead of one cutting its task while the next drops it.
        let task_fits =
            !matches!(self.title, ToolTitle::Agent { .. }) || width >= AGENT_TASK_MIN_WIDTH;
        for stats in trails {
            let without = spans_width(&row(Some(0), stats));
            let room = width.saturating_sub(without + frame);
            let least = MIN_CUT_COLUMNS.min(str_width(&cuttable));
            if task_fits && !cuttable.is_empty() && room > 0 && room >= least {
                return Line::from(row(Some(room), stats));
            }
            if without <= width {
                return Line::from(row(Some(0), stats));
            }
        }
        truncate_line(&Line::from(row(Some(0), &[])), width)
    }
}

/// The fewest columns a cut task keeps before the stats give way instead. Kept small, so
/// headers side by side cut their tasks alike.
const MIN_CUT_COLUMNS: usize = 4;

const AGENT_TASK_MIN_WIDTH: usize = 64;

pub(crate) fn running_bullet(ctx: &RenderCtx<'_>) -> Span<'static> {
    blinking_bullet(ctx, Role::Dim)
}

/// A call's own row while it runs: the bullet blinks in the working line's color.
fn working_bullet(ctx: &RenderCtx<'_>) -> Span<'static> {
    blinking_bullet(ctx, Role::Accent)
}

fn blinking_bullet(ctx: &RenderCtx<'_>, role: Role) -> Span<'static> {
    let theme = ctx.theme;
    let hidden = !theme.reduced_motion && ctx.clock.frame(BLINK_PERIOD, 2) == 1;
    let glyph = if hidden { " " } else { theme.glyphs.bullet };
    marks::decorative(glyph, theme.style(role))
}

impl HistoryCell for ToolCall {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        if ctx.expanded
            && let Some(full) = &self.full
        {
            return full.lines(ctx);
        }
        let mut lines = Vec::new();
        if ctx.expanded
            && let Some(stamp) = &self.stamp
        {
            lines.push(super::layout::stamp_row(stamp, ctx));
        }
        lines.extend(self.header(ctx));
        lines.extend(body::rows(self, ctx));
        fit_lines(&mut lines, usize::from(ctx.width));
        lines
    }

    fn lines_within(&self, ctx: &RenderCtx<'_>, rows: usize) -> Vec<Line<'static>> {
        let mut lines = squeeze::within(self, ctx, rows);
        fit_lines(&mut lines, usize::from(ctx.width));
        lines
    }

    fn is_animating(&self) -> bool {
        self.is_running()
    }
}

#[cfg(test)]
mod tests;
