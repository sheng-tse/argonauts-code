//! One agent, opened: its tabs on the header row, and Tab goes through them. Output streams,
//! pinned to the live end until you scroll up, with structured results drawn as what they mean.
//! Input shows what the agent was asked, part by part. An agent that works toward a goal has
//! a Goal tab: its lines, its tries, the checks, and what its CLI says. The raw log is the last
//! tab, and `r` from anywhere.

mod asked;
#[cfg(test)]
mod tests;

use std::time::{Duration, Instant};

use argonauts_core::RunId;
use argonauts_core::agent_goal::AgentGoal;
use argonauts_core::api::DiffScope;
use argonauts_core::app::{UiCommand, ViewRequest};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::session::{
    Delivery, QueuedMessage, SubagentKind, SubagentStatus, SubagentUpdate,
};
use argonauts_core::names::SessionKey;
use argonauts_core::steps::is_own_tool;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use time::OffsetDateTime;

use crate::bridge::{tools, vendor_workflow};
use crate::cell::{AssistantMessage, DiffCell, DiffView, HistoryCell, ToolItem};
use crate::format::{self, WallClock};
use crate::render::{Clock, RenderCtx};
use crate::state::{AppState, SessionState};
use crate::text::{sanitize, sanitize_line, spans_width, str_width, wrap_text};
use crate::theme::{Role, Theme};

use super::crew::CrewView;
use super::follow::{Follower, Shown};
use super::goal_keys;
use super::goal_panel;
use super::layout::cut;
use super::line_edit::LineEdit;
use super::list::{self, ListState, Nav};
use super::panes::{self, Footer, Marks};
use super::states;
use super::structured;
use super::{Follow, View, ViewCtx, ViewOutcome};

/// Result lines kept under a tool call; the raw log (`r`) has the rest.
const RESULT_LINES: usize = 40;

/// The opened agent's own tabs, in the order Tab goes through them. The raw log follows them
/// as the last tab, in a pager of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Output,
    Input,
    /// Only for an agent that works toward a goal.
    Goal,
}

impl Tab {
    fn name(self) -> &'static str {
        match self {
            Self::Output => "Output",
            Self::Input => "Input",
            Self::Goal => "Goal",
        }
    }
}

/// The name the raw log goes by on the tab row.
const RAW_TAB: &str = "Raw";

/// The header keeps this much of the agent's name before its time and tokens give way.
const MIN_NAME: usize = 16;

#[derive(Debug)]
pub struct SessionView {
    pub run: RunId,
    pub session: SessionKey,
    /// Pinned to the live end until you scroll up; End pins it again.
    pub following: bool,
    pub tab: Tab,
    /// The first row shown while not following.
    pub offset: usize,
    /// For the elapsed time; tests pin it.
    pub clock: WallClock,
    /// Opened from the crew view, so ← goes back to it rather than opening another.
    from_crew: bool,
    /// Opened from the run's row, so it moves on with the run.
    for_run: bool,
    /// The selected item on the Input tab, and the one `→` expanded.
    part: ListState,
    expanded: Option<usize>,
    /// `→` on the Output tab opened what its structured results fold away, such as what each
    /// task of a plan asks for.
    results_open: bool,
    /// The message to the agent while you type it.
    steer: Option<String>,
    /// The goal's lines while you rewrite them, all on one line with `;` between.
    lines_edit: Option<LineEdit>,
    /// What you last sent it, until it shows as queued or delivered.
    sent: Option<String>,
    /// A second `x` stops the agent.
    confirm_stop: bool,
    page: usize,
    rows: usize,
    ticking: bool,
    cache: Option<(Signature, Body)>,
    /// The first Input row shown, kept so the selected part stays on screen.
    input_top: usize,
    /// The first Goal row shown.
    goal_top: usize,
    /// Keeps the goal current while this view is on top: its run's events ask for the agent
    /// again, and the answer carries the goal as it stands.
    follow: Option<Follower>,
}

/// The rows drawn, on the Input tab the row of the selected part's heading, and on the Output
/// tab whether `→` has more of a result to show.
#[derive(Debug, Clone, Default)]
struct Body {
    lines: Vec<Line<'static>>,
    selected_row: usize,
    more: bool,
}

/// What the drawn rows depend on, so a frame with nothing new reuses them.
#[derive(Debug, Clone, PartialEq)]
struct Signature {
    width: u16,
    tab: Tab,
    expanded: Option<usize>,
    results_open: bool,
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
            tab: Tab::Output,
            offset: 0,
            clock: WallClock::local(),
            from_crew: false,
            for_run: false,
            part: ListState::default(),
            expanded: None,
            results_open: false,
            steer: None,
            lines_edit: None,
            sent: None,
            confirm_stop: false,
            page: 1,
            rows: 0,
            ticking: false,
            cache: None,
            input_top: 0,
            goal_top: 0,
            follow: None,
        }
    }

    /// Opened from the run's row: once its agent is done and the run has moved on, to the lead's
    /// next design or to its declared workflow, so does the view, though never while you type
    /// a message. A view opened for one agent follows only that agent, into the session that
    /// carries on its work.
    pub fn for_run(run: RunId, session: SessionKey) -> Self {
        Self {
            for_run: true,
            ..Self::new(run, session)
        }
    }

    /// Opened from the crew view, where ← returns.
    pub fn from_crew(run: RunId, session: SessionKey) -> Self {
        Self {
            from_crew: true,
            ..Self::new(run, session)
        }
    }

    /// Opened on `tab` rather than on what the agent says.
    pub fn on(mut self, tab: Tab) -> Self {
        self.tab = tab;
        self
    }

    /// Opened to message the agent: the message line is already there to type into.
    pub fn messaging(mut self) -> Self {
        self.steer = Some(String::new());
        self
    }

    /// Opened to rewrite `goal`'s lines: they are already there to change.
    pub fn editing_lines(mut self, goal: &AgentGoal) -> Self {
        self.lines_edit = Some(LineEdit::new(&goal_keys::lines_as_typed(goal)));
        self
    }

    /// Keeps what it shows of the agent's goal current with its run.
    pub fn following(mut self, follower: Follower) -> Self {
        self.follow = Some(follower);
        self
    }

    fn goal<'a>(&self, state: &'a AppState) -> Option<&'a AgentGoal> {
        state.goals.of(&self.run, &self.session)
    }

    /// What `p` does to the agent's goal, when it has one that can be held.
    fn goal_hold(&self, state: &AppState) -> Option<goal_keys::GoalKey> {
        goal_keys::hold(&self.run, self.goal(state)?)
    }

    /// What `c` does: the goal's checks run now, when it is going and they aren't running.
    fn goal_check(&self, state: &AppState) -> Option<goal_keys::GoalKey> {
        goal_keys::check_now(&self.run, self.goal(state)?)
    }

    fn can_edit_lines(&self, state: &AppState) -> bool {
        self.goal(state).is_some_and(goal_keys::can_edit)
    }

    /// `e` on the Goal tab: the goal's lines open on one line to be changed.
    fn edit_lines(&mut self, state: &AppState) -> ViewOutcome {
        match self.goal(state).filter(|goal| goal_keys::can_edit(goal)) {
            Some(goal) => {
                self.lines_edit = Some(LineEdit::new(&goal_keys::lines_as_typed(goal)));
                ViewOutcome::Consumed
            }
            None => ViewOutcome::Notice("This goal is over, so its lines stay as they are".into()),
        }
    }

    /// A key while the goal's lines are open: Enter rewrites them, Esc leaves them.
    fn lines_key(&mut self, key: KeyEvent, state: &AppState) -> ViewOutcome {
        let Some(line) = self.lines_edit.as_mut() else {
            return ViewOutcome::Ignored;
        };
        match key.code {
            KeyCode::Esc => self.lines_edit = None,
            KeyCode::Enter => {
                let typed = line.text();
                self.lines_edit = None;
                let Some(goal) = self.goal(state) else {
                    return ViewOutcome::Notice("This agent no longer works toward a goal".into());
                };
                return match goal_keys::edit(&self.run, goal, &typed) {
                    Ok(command) => ViewOutcome::Send(command),
                    Err(why) => ViewOutcome::Notice(why),
                };
            }
            _ => {
                line.key(key);
            }
        }
        ViewOutcome::Consumed
    }

    /// The tabs this agent has, in order: Goal only when it works toward one.
    fn tabs(&self, state: &AppState) -> Vec<Tab> {
        let mut tabs = vec![Tab::Output, Tab::Input];
        if self.goal(state).is_some() {
            tabs.push(Tab::Goal);
        }
        tabs
    }

    /// Tab goes right and Shift+Tab left. Past the last of its own tabs is the raw log, which
    /// opens over the view and leaves it on Output, so the tabs go round.
    fn next_tab(&mut self, back: bool, state: &AppState) -> ViewOutcome {
        let tabs = self.tabs(state);
        let at = tabs.iter().position(|tab| *tab == self.tab).unwrap_or(0);
        let next = if back {
            at.checked_sub(1)
        } else {
            Some(at + 1).filter(|next| *next < tabs.len())
        };
        self.confirm_stop = false;
        match next {
            Some(next) => {
                self.tab = tabs[next];
                ViewOutcome::Consumed
            }
            None => {
                self.tab = if back { tabs[tabs.len() - 1] } else { tabs[0] };
                self.raw()
            }
        }
    }

    fn raw(&self) -> ViewOutcome {
        ViewOutcome::Request(ViewRequest::Raw {
            run: self.run.clone(),
            session: self.session.clone(),
        })
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

    /// Moves the Output tab. Scrolling up pauses following; reaching the end resumes it.
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
            tab: self.tab,
            expanded: self.expanded,
            results_open: self.results_open,
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

    fn body(&mut self, session: &SessionState, width: u16, ctx: &ViewCtx<'_>) -> Body {
        let theme = ctx.theme;
        let signature = self.signature(session, width);
        // A goal changes by its own reports, which the signature doesn't see.
        if self.tab != Tab::Goal
            && let Some((known, body)) = &self.cache
            && *known == signature
        {
            return body.clone();
        }
        let body = match self.tab {
            Tab::Goal => Body {
                lines: match self.goal(ctx.state) {
                    Some(goal) => goal_panel::tab(goal, usize::from(width), theme).lines,
                    None => vec![note("This agent works without a goal", Role::Dim, theme)],
                },
                selected_row: 0,
                more: false,
            },
            Tab::Input => {
                let draw = asked::Draw {
                    theme,
                    width: usize::from(width),
                    selected: self.part.selected,
                    expanded: self.expanded,
                };
                let asked::Drawn {
                    mut lines,
                    selected_row,
                } = asked::lines(&session.inputs, &draw);
                if !session.queued.is_empty() {
                    lines.push(Line::default());
                    lines.extend(queued_lines(
                        &session.queued,
                        None,
                        usize::from(width),
                        theme,
                    ));
                }
                Body {
                    lines,
                    selected_row,
                    more: false,
                }
            }
            Tab::Output => {
                // The echo stands only while the session can still take it. The controller's
                // answer in the transcript says what came of it.
                let sent = self.sent.as_deref().filter(|_| session.is_running());
                let shown = OutputDraw {
                    names: &ctx.state.names,
                    theme,
                    width,
                    results_open: self.results_open,
                };
                let (lines, more) = output_lines(session, sent, &shown);
                Body {
                    lines,
                    selected_row: 0,
                    more,
                }
            }
        };
        self.cache = Some((signature, body.clone()));
        body
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
                    // Ctrl+Enter sends it now, as Claude Code's own key does.
                    now: key.modifiers.contains(KeyModifiers::CONTROL),
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
    /// `● build:cli · GPT 5.6 on Codex`, the tabs, and the time and tokens at the right end.
    /// The tabs sit midway between the two while there's room, and close after the name when
    /// there isn't; the name is what gives way.
    fn header(
        &self,
        session: &SessionState,
        state: &AppState,
        width: usize,
        theme: &Theme,
    ) -> Line<'static> {
        let marks = Marks::for_theme(theme);
        let (glyph, role) = states::mark(states::of_session(session.status), theme);
        // Headed as the chat heads its block: the label, then the agent, as `fix:slugify ·
        // Haiku 4.5 on Claude Code`. A log from before labels names the role and the task.
        let mut name = match &session.label {
            Some(label) => vec![clean(label), clean(&state.names.worker(&session.worker))],
            None => vec![
                clean(&state.names.worker(&session.worker)),
                clean(session.role.as_str()),
            ],
        };
        if let Some(card) = session.card.as_ref().filter(|_| session.label.is_none()) {
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
        let mut numbers = Vec::new();
        if let Some(attempt) = self
            .goal(state)
            .map(AgentGoal::current_try)
            .filter(|n| *n > 0)
        {
            numbers.push(format!("try {attempt}"));
        }
        numbers.push(panes::elapsed(u64::try_from(elapsed).unwrap_or(u64::MAX)));
        if session.tokens() > 0 {
            numbers.push(format!(
                "{} {}",
                theme.glyphs.tokens,
                format::tokens(session.tokens())
            ));
        }
        let mut numbers = numbers.join(" · ");
        let tabs = self.tab_spans(state, theme);
        let lead = format!("{glyph} ");
        // Two cells apart at the least, on each side of the tabs. On a narrow screen the time
        // and tokens go before the name is cut to nothing.
        let room_beside = |numbers: &str| {
            let numbers = match str_width(numbers) {
                0 => 0,
                cells => 2 + cells,
            };
            width.saturating_sub(str_width(&lead) + 2 + spans_width(&tabs) + numbers)
        };
        let least = name.first().map_or(0, |own| str_width(own).min(MIN_NAME));
        if room_beside(&numbers) < least {
            numbers.clear();
        }
        let name_room = room_beside(&numbers);
        let (own, rest) = name
            .split_first()
            .map_or((String::new(), &[][..]), |(own, rest)| {
                (cut(own, name_room), rest)
            });
        let rest = if rest.is_empty() {
            String::new()
        } else {
            let rest = format!(" · {}", rest.join(" · "));
            cut(&rest, name_room.saturating_sub(str_width(&own)))
        };
        let mut spans = vec![
            Span::styled(lead, theme.style(role)),
            Span::styled(own, theme.style(Role::Bright).add_modifier(Modifier::BOLD)),
            Span::styled(rest, theme.style(Role::Dim)),
        ];
        let free = width.saturating_sub(spans_width(&spans) + str_width(&numbers));
        let tabs_width = spans_width(&tabs);
        if free < tabs_width + 2 {
            // Too narrow for the numbers as well: the name and the tabs are what stay.
            spans.push(Span::raw("  "));
            spans.extend(tabs);
            return Line::from(crate::text::truncate_spans(&spans, width));
        }
        let before = ((free - tabs_width) / 2).max(2);
        spans.push(Span::raw(" ".repeat(before)));
        spans.extend(tabs);
        spans.push(Span::raw(" ".repeat(free - tabs_width - before)));
        spans.push(Span::styled(numbers, theme.style(Role::Dim)));
        Line::from(spans)
    }

    /// `Output  Input  Raw`, the open one as a chip, or in brackets where there's no color.
    fn tab_spans(&self, state: &AppState, theme: &Theme) -> Vec<Span<'static>> {
        let chip = theme
            .style(Role::Card)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD);
        let names = self
            .tabs(state)
            .into_iter()
            .map(|tab| (tab.name(), tab == self.tab))
            .chain([(RAW_TAB, false)]);
        let mut spans = Vec::new();
        for (name, open) in names {
            spans.push(match (open, theme.has_color()) {
                (true, true) => Span::styled(format!(" {name} "), chip),
                (true, false) => Span::styled(
                    format!("[{name}]"),
                    theme.style(Role::Bright).add_modifier(Modifier::BOLD),
                ),
                (false, _) => Span::styled(format!(" {name} "), theme.style(Role::Text)),
            });
        }
        spans
    }

    fn footer(&self, width: usize, state: &AppState, theme: &Theme, more: bool) -> Footer {
        if let Some(line) = &self.steer {
            let lead = format!("message {} ", theme.glyphs.prompt);
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
        if let Some(line) = &self.lines_edit {
            let lead = format!("its lines, with ; between {} ", theme.glyphs.prompt);
            let mut spans = vec![Span::styled(lead.clone(), theme.style(Role::Label))];
            spans.extend(line.spans(width.saturating_sub(str_width(&lead)), theme));
            return Footer::Line(Line::from(spans));
        }
        if self.confirm_stop {
            return Footer::hints(&[("x again to stop it", 0), ("esc to keep", 0)]);
        }
        // Only keys that can act now are offered.
        let session = state.session(&self.run, &self.session);
        let message = super::steer_refusal(state, &self.run, &self.session).is_none();
        let has_card = session.is_some_and(|session| session.card.is_some());
        let take_over = has_card && super::run_going(state, &self.run);
        let stop = self.can_stop(state);
        // The tab after Output is named, as the first thing to learn; past it, Tab goes on.
        let tab = match self.tab {
            Tab::Output => "tab input",
            Tab::Input | Tab::Goal => "tab next tab",
        };
        let mut parts = Vec::new();
        match self.tab {
            Tab::Goal => {
                parts.push(("↑↓ scroll", 1));
                if let Some(hold) = self.goal_hold(state) {
                    parts.push((hold.hint, 5));
                }
                if self.can_edit_lines(state) {
                    parts.push((goal_keys::EDIT_HINT, 5));
                }
                if let Some(check) = self.goal_check(state) {
                    parts.push((check.hint, 5));
                }
                if message {
                    parts.push(("m message it", 5));
                }
                if stop {
                    parts.push(("x stop it", 4));
                }
                // The tabs are in the header, so this hint goes before a key on the goal does.
                parts.push((tab, 2));
            }
            Tab::Input => {
                parts.extend([("↑↓ select", 0), ("→ expand", 3), (tab, 4)]);
                if message {
                    parts.push(("m message it", 2));
                }
                if stop {
                    parts.push(("x stop it", 1));
                }
            }
            Tab::Output => {
                if !self.following {
                    parts.push(("End follow", 0));
                }
                parts.push(("↑↓ scroll", if self.following { 0 } else { 9 }));
                if more {
                    let hint = if self.results_open {
                        "→ fold"
                    } else {
                        "→ expand"
                    };
                    parts.push((hint, 5));
                }
                parts.push((tab, 8));
                if message {
                    parts.push(("m message it", 7));
                }
                if take_over {
                    parts.push(("t take over", 4));
                }
                if stop {
                    parts.push(("x stop it", 4));
                }
                parts.push(("r raw", 6));
                if has_card {
                    parts.push(("d diff", 5));
                }
                if self.following {
                    parts.push(("← crew", 3));
                }
            }
        }
        parts.push((panes::ESC_BACK, 0));
        Footer::hints(&parts)
    }

    /// Stopping needs the agent still at work in a run that goes on.
    fn can_stop(&self, state: &AppState) -> bool {
        super::run_going(state, &self.run)
            && state
                .session(&self.run, &self.session)
                .is_some_and(SessionState::is_running)
    }

    /// `x` asks first, then stops this agent alone.
    fn stop(&mut self, state: &AppState) -> ViewOutcome {
        if let Some(why) = super::ended_why(state, &self.run) {
            return ViewOutcome::Notice(why.to_owned());
        }
        if !self.can_stop(state) {
            return ViewOutcome::Notice(
                "This agent isn't running, so there's nothing to stop".into(),
            );
        }
        if !self.confirm_stop {
            self.confirm_stop = true;
            return ViewOutcome::Consumed;
        }
        self.confirm_stop = false;
        ViewOutcome::Send(UiCommand::StopAgent {
            run: self.run.clone(),
            session: self.session.clone(),
        })
    }
}

/// The Input tab's first row: moved the least that shows the selected part's heading, and a
/// few rows of its text when there's room.
fn input_top(top: usize, selected: usize, rows: usize, height: usize) -> usize {
    let height = height.max(1);
    let want_end = (selected + asked::PART_LINES.min(height)).min(rows);
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

/// What the Output tab's rows are drawn with.
struct OutputDraw<'a> {
    names: &'a DisplayNames,
    theme: &'a Theme,
    width: u16,
    results_open: bool,
}

/// Every tool call with its result, diff, message, and subagent, as the chat draws them, and
/// under them what waits to be delivered. A structured result, whether the agent reported it
/// through Argonauts' own tool or wrote it out at the end of a message, is drawn as what it
/// means; the rest of its talk with Argonauts is left to the raw log. Also says whether `→`
/// has more of a result to show.
fn output_lines(
    session: &SessionState,
    sent: Option<&str>,
    shown: &OutputDraw<'_>,
) -> (Vec<Line<'static>>, bool) {
    let (width, theme) = (shown.width, shown.theme);
    let ctx = RenderCtx::new(width, theme, Clock::still(Instant::now())).with_expanded(true);
    let draw = structured::Draw {
        names: shown.names,
        theme,
        width: usize::from(width),
        expanded: shown.results_open,
        hints: true,
    };
    let mut more = false;
    let mut blocks: Vec<Vec<Line<'static>>> = Vec::new();
    let mut result = |value: &serde_json::Value, blocks: &mut Vec<Vec<Line<'static>>>| {
        let Some((lines, has_more)) = structured::lines(value, &draw) else {
            return false;
        };
        more |= has_more;
        blocks.push(lines);
        true
    };
    for tool in &session.tools {
        if is_own_tool(&tool.name) {
            let reports = tool
                .input
                .as_object()
                .is_some_and(|input| !input.is_empty());
            if reports {
                result(&tool.input, &mut blocks);
            }
            continue;
        }
        if vendor_workflow::is_call(&session.vendor, &tool.name) {
            let reported = session
                .workflows
                .iter()
                .find(|workflow| workflow.call.as_deref() == Some(tool.id.as_str()));
            let call = match reported {
                Some(workflow) => vendor_workflow::row(workflow),
                None => {
                    vendor_workflow::before_report(&tool.input, tool.ok, tool.output.as_deref())
                }
            };
            blocks.push(call.lines(&ctx));
            continue;
        }
        let mut call = tools::start_call(&session.vendor, &tool.name, &tool.input);
        for (_, path, hunks) in session.patches.iter().filter(|(id, ..)| *id == tool.id) {
            tools::take_patch(&mut call, path, hunks);
        }
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
        blocks.push(call.lines(&ctx));
    }
    // What Argonauts saw change after the calls, for a file no call's own patch drew, such as
    // one a shell command changed.
    for diff in &session.diffs {
        let drawn = session
            .patches
            .iter()
            .any(|(_, path, _)| tools::same_file(path, &diff.path));
        if drawn {
            continue;
        }
        let path = clean(&diff.path);
        let view = DiffView::from_unified(&diff.hunk).unwrap_or_default();
        let view = view.updated(path.clone(), diff.added as usize, diff.removed as usize);
        let cell = DiffCell {
            title: format!("Update({path})"),
            diff: view,
        };
        blocks.push(cell.lines(&ctx));
    }
    let said = |text: &str| AssistantMessage::new(text.to_owned()).lines(&ctx);
    for message in &session.messages {
        match tools::written_object(message) {
            Some((words, value)) => {
                if !words.is_empty() {
                    blocks.push(said(&words));
                }
                if !result(&value, &mut blocks) {
                    blocks.push(said(message));
                }
            }
            None => blocks.push(said(message)),
        }
    }
    if !session.streaming.trim().is_empty() {
        blocks.push(said(&session.streaming));
    }
    // A workflow drawn as its call's row isn't listed again among the subagents.
    let as_rows: Vec<SubagentUpdate> = session
        .subagents
        .iter()
        .filter(|sub| {
            sub.kind != SubagentKind::Workflow
                || !session.tools.iter().any(|tool| {
                    tool.id == sub.id && vendor_workflow::is_call(&session.vendor, &tool.name)
                })
        })
        .cloned()
        .collect();
    blocks.push(subagent_tree(&as_rows, usize::from(width), theme));
    blocks.push(queued_lines(
        &session.queued,
        sent,
        usize::from(width),
        theme,
    ));
    let mut lines = Vec::new();
    for block in blocks.into_iter().filter(|block| !block.is_empty()) {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.extend(block);
    }
    if lines.is_empty() {
        lines.push(note("Nothing from this agent yet", Role::Dim, theme));
    }
    (lines, more)
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
        let word = match sub.status {
            SubagentStatus::Started => "started",
            SubagentStatus::Running => "running",
            SubagentStatus::Done => "done",
            SubagentStatus::Failed => "failed",
            SubagentStatus::Unknown => "state unknown",
        };
        let (glyph, role) = states::mark(states::of_subagent(sub.status), theme);
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

impl View for SessionView {
    fn follow(&mut self, ctx: &ViewCtx<'_>) -> Option<Follow> {
        let run = ctx.state.runs.get(&self.run)?;
        let working = run
            .sessions
            .get(&self.session)
            .is_some_and(SessionState::is_running);
        // It stays while its agent works, and while you type a message to it.
        if working || self.steer.is_some() {
            return None;
        }
        if self.for_run {
            return super::waiting::follow(ctx.state, &self.run, Some(&self.session));
        }
        // The agent you chose carried on in another session, so the view does too.
        let next = run.carried_on(&self.session)?;
        Some(Follow::Replace(Box::new(Self {
            from_crew: self.from_crew,
            ..Self::new(self.run.clone(), next.key.clone())
        })))
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let theme = ctx.theme;
        let width = usize::from(area.width);
        // The header, a row that is blank unless the output is paused, the tab's rows, and
        // the footer.
        let [header, under, body_area, footer] = Layout::vertical([
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
        if let Some(follow) = &self.follow {
            follow.show(Shown::Agent {
                run: self.run.clone(),
                session: self.session.clone(),
            });
        }
        // A goal that ended, or was cleared, takes its tab with it.
        if self.tab == Tab::Goal && self.goal(ctx.state).is_none() {
            self.tab = Tab::Output;
        }
        panes::put(
            buf,
            header.x,
            header.y,
            header.width,
            &self.header(session, ctx.state, width, theme),
        );
        if self.tab == Tab::Output && !self.following {
            let paused = "paused · End follows";
            let pad = " ".repeat(width.saturating_sub(str_width(paused)));
            let line = Line::from(vec![
                Span::raw(pad),
                Span::styled(paused, theme.style(Role::Warning)),
            ]);
            panes::put(buf, under.x, under.y, under.width, &line);
        }

        let Body {
            lines,
            selected_row,
            more,
        } = self.body(session, area.width, ctx);
        let height = usize::from(body_area.height);
        self.page = height.max(1);
        let top = match self.tab {
            Tab::Goal => {
                self.goal_top = self.goal_top.min(lines.len().saturating_sub(height));
                self.goal_top
            }
            Tab::Input => {
                self.input_top = input_top(self.input_top, selected_row, lines.len(), height);
                self.input_top
            }
            Tab::Output => {
                self.rows = lines.len();
                let top = self.top(lines.len(), height);
                if !self.following {
                    self.offset = top;
                }
                top
            }
        };
        panes::put_lines(buf, body_area, &lines[top.min(lines.len())..]);

        let marks = Marks::for_theme(theme);
        let mark = match self.tab {
            Tab::Output | Tab::Goal => {
                super::pager::position_mark(top, height, lines.len(), marks.down)
            }
            Tab::Input => String::new(),
        };
        // The message line leaves room for the mark beside it.
        let room = match str_width(&mark) {
            0 => width,
            mark => width.saturating_sub(mark + 2),
        };
        panes::draw_footer(
            buf,
            footer,
            self.footer(room, ctx.state, theme, more),
            &mark,
            theme,
        );
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        if self.steer.is_some() {
            return self.steer_key(key);
        }
        if self.lines_edit.is_some() {
            return self.lines_key(key, ctx.state);
        }
        let state = ctx.state;
        let session = state.session(&self.run, &self.session);
        // `h` goes back to the crew, as ← does.
        let key = list::standard(key, list::vim(state), KeyCode::Left);
        // Ctrl and Alt letters are global keys, such as Ctrl+D to exit.
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if self.confirm_stop && !(plain && key.code == KeyCode::Char('x')) {
            self.confirm_stop = false;
            if key.code == KeyCode::Esc {
                return ViewOutcome::Consumed;
            }
        }
        match key.code {
            KeyCode::Esc => return ViewOutcome::Close,
            KeyCode::Tab => return self.next_tab(false, state),
            KeyCode::BackTab => return self.next_tab(true, state),
            KeyCode::Char('m') if plain => {
                if let Some(why) = super::steer_refusal(state, &self.run, &self.session) {
                    return ViewOutcome::Notice(why);
                }
                self.steer = Some(String::new());
                return ViewOutcome::Consumed;
            }
            KeyCode::Char('t') if plain => {
                return super::take_over(state, &self.run, &self.session);
            }
            KeyCode::Char('x') if plain => return self.stop(state),
            KeyCode::Char('p') if plain && self.tab == Tab::Goal => {
                return match self.goal_hold(state) {
                    Some(hold) => ViewOutcome::Send(hold.command),
                    None => ViewOutcome::Ignored,
                };
            }
            KeyCode::Char('c') if plain && self.tab == Tab::Goal => {
                return match self.goal_check(state) {
                    Some(check) => ViewOutcome::Send(check.command),
                    None => ViewOutcome::Ignored,
                };
            }
            KeyCode::Char('e') if plain && self.tab == Tab::Goal => return self.edit_lines(state),
            KeyCode::Char('r') if plain => return self.raw(),
            KeyCode::Char('d') if plain => {
                return match session.and_then(|session| session.card.clone()) {
                    Some(card) => ViewOutcome::Request(ViewRequest::Diff {
                        run: self.run.clone(),
                        card,
                        scope: DiffScope::Card,
                    }),
                    None => ViewOutcome::Notice("This agent has no task, so no diff".into()),
                };
            }
            KeyCode::Left if self.tab == Tab::Input && self.expanded.is_some() => {
                self.expanded = None;
                return ViewOutcome::Consumed;
            }
            KeyCode::Left => return self.back_to_crew(state),
            KeyCode::Right | KeyCode::Enter if self.tab == Tab::Goal => {
                return ViewOutcome::Consumed;
            }
            KeyCode::Right | KeyCode::Enter if self.tab == Tab::Input => {
                let selected = self.part.selected;
                self.expanded = if self.expanded == Some(selected) {
                    None
                } else {
                    Some(selected)
                };
                return ViewOutcome::Consumed;
            }
            // What a plan's tasks ask for, and back.
            KeyCode::Right | KeyCode::Enter => {
                self.results_open = !self.results_open;
                return ViewOutcome::Consumed;
            }
            _ => {}
        }
        let Some(nav) = list::nav_key(&key, self.page) else {
            return ViewOutcome::Ignored;
        };
        match self.tab {
            Tab::Input => {
                let items = session.map_or(0, |session| asked::items(&session.inputs));
                self.part.apply(nav, items, 1);
                if self.expanded.is_some_and(|open| open != self.part.selected) {
                    self.expanded = None;
                }
            }
            Tab::Output => self.scroll(nav),
            Tab::Goal => {
                self.goal_top = match nav {
                    Nav::Top => 0,
                    Nav::Bottom => usize::MAX,
                    Nav::By(delta) => self.goal_top.saturating_add_signed(delta),
                };
            }
        }
        ViewOutcome::Consumed
    }

    /// The agent's answer, asked for while this view is on top: the state has taken its goal
    /// in already, so the next frame draws it.
    fn accept(&mut self, data: &argonauts_core::app::ViewData) -> bool {
        matches!(data, argonauts_core::app::ViewData::Agent { view }
            if self.follow.is_some() && view.run == self.run && view.row.session == self.session)
    }

    fn next_change(&self, now: Instant) -> Option<Instant> {
        self.ticking.then(|| now + Duration::from_secs(1))
    }
}

#[cfg(test)]
mod scroll_and_keys {
    use super::*;
    use crate::state::SessionStatus;

    fn view(rows: usize, page: usize) -> SessionView {
        let mut view = SessionView::new(
            RunId::new("run-01").unwrap(),
            SessionKey::parse("s-001").unwrap(),
        );
        view.rows = rows;
        view.page = page;
        view
    }

    /// Keys that can't act aren't offered, and pressed anyway they say why in plain words:
    /// an agent of a merged run can't be steered or taken over.
    #[test]
    fn a_merged_runs_agent_offers_only_the_keys_that_act() {
        let mut state = crate::mentions::fixture::working_state();
        let run = state.attached.clone().unwrap();
        let session = SessionKey::numbered(1);
        let theme = Theme::dark();
        let offered =
            |view: &SessionView, state: &AppState| match view.footer(120, state, &theme, false) {
                Footer::Hints(hints) => hints.into_iter().map(|(hint, _)| hint).collect(),
                Footer::Line(_) => Vec::<String>::new(),
            };
        let mut view = SessionView::new(run.clone(), session.clone());
        let keys = offered(&view, &state);
        for hint in ["m message it", "t take over", "x stop it", "d diff"] {
            assert!(keys.iter().any(|key| key == hint), "{hint}: {keys:?}");
        }

        let held = state.runs.get_mut(&run).unwrap();
        held.finished = Some(argonauts_core::events::run::RunOutcome::Merged);
        held.sessions.get_mut(&session).unwrap().status = SessionStatus::Done;
        let keys = offered(&view, &state);
        for hint in ["m message it", "t take over", "x stop it"] {
            assert!(!keys.iter().any(|key| key == hint), "{hint}: {keys:?}");
        }
        assert!(keys.iter().any(|key| key == "d diff"), "{keys:?}");
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        for c in ['t', 'm', 'x'] {
            let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
            let ViewOutcome::Notice(why) = view.handle_key(key, &ctx) else {
                panic!("{c} says why");
            };
            assert_eq!(
                why,
                "That run already merged, so its agents are done. To build on its result, \
                 type /follow-up and what you want next."
            );
        }
        assert!(view.steer.is_none());
    }

    /// A steer typed as its session ended never reached it, such as the lead's design session
    /// that ends when code sends its brief back, so the view stops saying it was sent. The
    /// controller's answer says why in the transcript.
    #[test]
    fn a_steer_shows_as_sent_only_while_its_session_runs() {
        let mut state = crate::mentions::fixture::working_state();
        let run = state.attached.clone().unwrap();
        let session = SessionKey::numbered(1);
        let theme = Theme::dark();
        let mut view = SessionView::new(run.clone(), session.clone());
        let press = |view: &mut SessionView, state: &AppState, code: KeyCode| {
            let ctx = ViewCtx {
                theme: &theme,
                state,
                now: Instant::now(),
            };
            view.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx)
        };
        press(&mut view, &state, KeyCode::Char('m'));
        for c in "keep the old limits".chars() {
            press(&mut view, &state, KeyCode::Char(c));
        }
        let sent = press(&mut view, &state, KeyCode::Enter);
        assert!(matches!(sent, ViewOutcome::Send(UiCommand::Steer { .. })));
        let shown = |view: &mut SessionView, state: &AppState| {
            let ctx = ViewCtx {
                theme: &theme,
                state,
                now: Instant::now(),
            };
            let session = state.session(&run, &session).unwrap();
            let body = view.body(session, 80, &ctx);
            body.lines
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect()
                })
                .collect::<Vec<String>>()
                .join("\n")
        };
        assert!(
            shown(&mut view, &state).contains("Sent keep the old limits"),
            "{}",
            shown(&mut view, &state)
        );
        let ended = state.runs.get_mut(&run).unwrap();
        let ended = ended.sessions.get_mut(&session).unwrap();
        ended.status = SessionStatus::Done;
        ended.last_event += time::Duration::seconds(1);
        let after = shown(&mut view, &state);
        assert!(!after.contains("Sent keep the old limits"), "{after}");
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
        for c in ['d', 't', 'r', 'm', 'x'] {
            let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
            assert!(
                matches!(view.handle_key(key, &ctx), ViewOutcome::Ignored),
                "Ctrl+{c}"
            );
        }
        assert!(view.steer.is_none() && view.tab == Tab::Output && !view.confirm_stop);
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
