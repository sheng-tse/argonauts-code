//! The panel under the bottom line, as Claude Code's workflow rows: one row per run with its
//! glyph, its name, a bar of its agents done, `done/total`, its time and its tokens, dim unless
//! it is selected. Argonauts Code adds one thing at the row's end, only when there is something
//! to say: who fills in for whom, why it needs you, or how it ended. With `ui.panel.agents`,
//! each run's agents follow under its row.

use std::time::{Duration, Instant};

use argonauts_core::events::run::RunOutcome;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::format;
use crate::render::{Clock, RenderCtx};
use crate::text::{Part, fit_parts, full_width, sanitize_line, str_width};
use crate::theme::{Role, Theme};
use crate::views::layout::left;
use crate::views::panes::Marks;

use super::agents::{AgentRef, ListRow, RowKind, more_line, window};
use super::bar::{BAR_CELLS, MIN_BAR_CELLS, bar};
use super::{AgentPanel, PanelEvent};

/// The name column's width, so every bar starts in one column. A longer name widens it only
/// into room the row has to spare, so one long name can't push every bar off the row.
const MAX_NAME_CELLS: usize = 22;

/// Names give up cells down to this to keep the bar.
const MIN_NAME_CELLS: usize = 12;

/// Rows under a run's row sit this much further in.
const AGENT_INDENT: usize = 2;

/// Cells each level of a workflow inside steps further in than the one holding it.
const INSIDE_STEP: usize = 4;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkflowPanel {
    pub rows: Vec<WorkflowRow>,
    /// `ui.panel.max_rows`; the last visible row reads `↓ N more` past it.
    pub max_rows: usize,
    /// `ui.panel.agents`: list each run's agents under its row.
    pub show_agents: bool,
    /// While the panel has focus, the selected row counting runs and agents alike, top down.
    pub selected: Option<usize>,
    /// The key of the row inside a run that Enter opened, whose run's view opens on it.
    pub opening: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowRow {
    /// The run id, reported back when the row is opened.
    pub key: String,
    pub name: String,
    pub state: WorkflowRowState,
    pub agents_done: u32,
    pub agents_total: u32,
    pub elapsed_ms: u64,
    /// While the run goes on, when `elapsed_ms` was measured, so the time keeps counting.
    pub measured_at: Option<Instant>,
    /// False for a run this screen doesn't hold, whose time it doesn't know, so its row shows
    /// none.
    pub timed: bool,
    pub tokens: u64,
    /// Such as `85% of limit · /workflows to stop`, drawn after `!` in the warning color.
    pub notice: Option<String>,
    /// What Argonauts Code says at the row's end when there is something to say: why it needs
    /// you, or how it ended, such as `merged onto main`.
    pub said: Option<String>,
    /// How a run that stopped or that you paused goes on, after what it says: `/resume carries
    /// it on`.
    pub carry_on: Option<String>,
    /// The model that fills in for another while the run goes on, such as `Sonnet 5.5 for
    /// Opus 5.5`, said after the fallback's mark.
    pub fallback: Option<String>,
    /// The outside agent that started it, such as `via hermes`.
    pub outside_agent: Option<String>,
    /// Goals its agents still work toward, for `2 goals running`.
    pub goals_running: usize,
    /// How many workflows in it is: 0 for a run's own row, 1 for a workflow inside the run,
    /// which follows the run's row on `⎿`, and 2 for one inside that.
    pub depth: usize,
    /// The worker whose own CLI runs it, for `run by build:due-flag`.
    pub run_by: Option<String>,
    /// When the row of a run that ended leaves the list.
    pub leaves_at: Option<Instant>,
}

impl WorkflowRow {
    pub fn new(key: impl Into<String>, name: impl Into<String>, state: WorkflowRowState) -> Self {
        Self {
            key: key.into(),
            name: name.into(),
            state,
            agents_done: 0,
            agents_total: 0,
            elapsed_ms: 0,
            measured_at: None,
            timed: true,
            tokens: 0,
            notice: None,
            said: None,
            carry_on: None,
            fallback: None,
            outside_agent: None,
            goals_running: 0,
            depth: 0,
            run_by: None,
            leaves_at: None,
        }
    }

    fn elapsed(&self, now: Instant) -> Duration {
        let since = self
            .measured_at
            .map_or(Duration::ZERO, |at| now.saturating_duration_since(at));
        Duration::from_millis(self.elapsed_ms) + since
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowRowState {
    Running,
    NeedsYou,
    Done,
    Failed,
    Paused,
    /// It waits out an outage you chose to wait out, and tries again on its own.
    Waiting,
    Stopped,
    /// The pull request it opened is followed, and a round of fixes works on it or it needs
    /// you.
    PullRequest,
    /// The pull request it opened is followed, and waits for GitHub.
    PullRequestWaits,
}

impl WorkflowRowState {
    /// How a run that ended reads, the same in its panel row and its Workflow item: work kept
    /// is done, a stop or an abandon is stopped, and a failure is failed.
    pub fn ended(outcome: RunOutcome) -> Self {
        match outcome {
            RunOutcome::Merged | RunOutcome::PullRequest | RunOutcome::BranchLeft => Self::Done,
            RunOutcome::Stopped | RunOutcome::Abandoned => Self::Stopped,
            RunOutcome::Failed => Self::Failed,
        }
    }

    /// Its word, as the row shows it and the run's Workflow item ends with it.
    pub fn word(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::NeedsYou => "needs you",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Paused => "paused",
            Self::Waiting => "waiting",
            Self::Stopped => "stopped",
            Self::PullRequest | Self::PullRequestWaits => "following its pull request",
        }
    }
}

/// One row of the panel, top down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    Run(usize),
    Agent(usize),
}

/// What a panel row is, so the selection can stay on it while the rows change under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelRowId {
    Run(String),
    Agent(AgentRef),
}

impl Entry {
    fn run(self) -> Option<usize> {
        match self {
            Self::Run(i) => Some(i),
            Self::Agent(_) => None,
        }
    }

    fn agent(self) -> Option<usize> {
        match self {
            Self::Agent(j) => Some(j),
            Self::Run(_) => None,
        }
    }
}

impl WorkflowPanel {
    pub fn has_focus(&self) -> bool {
        self.selected.is_some() && !self.rows.is_empty()
    }

    pub fn focus(&mut self) {
        self.selected = (!self.rows.is_empty()).then_some(0);
    }

    /// Focus as ↓ from the input gives it: on the first row past your own conversation's,
    /// since the list is where you go to reach a run or one of the agents.
    pub fn focus_past_main(&mut self, agents: &AgentPanel) {
        self.focus();
        let entries = self.entries(agents);
        let first = entries.iter().position(|entry| {
            entry
                .agent()
                .is_none_or(|j| agents.rows[j].kind != RowKind::Main)
        });
        if self.selected.is_some() {
            self.selected = Some(first.unwrap_or(0));
        }
    }

    pub fn blur(&mut self) {
        self.selected = None;
    }

    /// Lets go of the rows whose time in the list is up, each a run that ended a while ago,
    /// and keeps the selection on the row it was on. True when a row left.
    pub fn let_go(&mut self, agents: &AgentPanel, now: Instant) -> bool {
        let due = |row: &WorkflowRow| row.leaves_at.is_some_and(|at| at <= now);
        if !self.rows.iter().any(due) {
            return false;
        }
        let keep = self.selected_id(agents);
        self.rows.retain(|row| !due(row));
        self.reselect(agents, keep.as_ref());
        true
    }

    /// The run or agent the selection is on, so a new set of rows can keep it.
    pub fn selected_id(&self, agents: &AgentPanel) -> Option<PanelRowId> {
        let entries = self.entries(agents);
        let entry = entries.get(self.selected?)?;
        Some(match *entry {
            Entry::Run(i) => PanelRowId::Run(self.rows[i].key.clone()),
            Entry::Agent(i) => PanelRowId::Agent(agents.rows[i].agent_ref()),
        })
    }

    /// Puts the selection back on `id` after the rows changed, or keeps it in range.
    pub fn reselect(&mut self, agents: &AgentPanel, id: Option<&PanelRowId>) {
        let Some(selected) = self.selected else {
            return;
        };
        let entries = self.entries(agents);
        if entries.is_empty() {
            self.blur();
            return;
        }
        let found = id.and_then(|id| {
            entries.iter().position(|entry| match (*entry, id) {
                (Entry::Run(i), PanelRowId::Run(run)) => self.rows[i].key == *run,
                (Entry::Agent(i), PanelRowId::Agent(agent)) => agents.rows[i].agent_ref() == *agent,
                _ => false,
            })
        });
        self.selected = Some(found.unwrap_or(selected.min(entries.len() - 1)));
    }

    /// Your conversation and its own subagents first, as Claude Code lists them, then the runs
    /// in order, each followed by its agents while they show. Agents of a run without a row
    /// come last.
    fn entries(&self, agents: &AgentPanel) -> Vec<Entry> {
        let mut entries = Vec::new();
        let mut placed = vec![false; agents.rows.len()];
        // These aren't a run's agents, so `ui.panel.agents` doesn't hide them.
        for (j, agent) in agents.rows.iter().enumerate() {
            if agent.kind != RowKind::Agent {
                entries.push(Entry::Agent(j));
                placed[j] = true;
            }
        }
        // A workflow inside a run follows the run's row, and the run's agents come after.
        for (i, row) in self.rows.iter().enumerate() {
            if row.depth > 0 {
                continue;
            }
            entries.push(Entry::Run(i));
            entries.extend(
                self.rows[i + 1..]
                    .iter()
                    .take_while(|inside| inside.depth > 0)
                    .enumerate()
                    .map(|(at, _)| Entry::Run(i + 1 + at)),
            );
            if !self.show_agents {
                continue;
            }
            for (j, agent) in agents.rows.iter().enumerate() {
                if !placed[j] && agent.run.as_deref() == Some(row.key.as_str()) {
                    entries.push(Entry::Agent(j));
                    placed[j] = true;
                }
            }
        }
        if self.show_agents {
            entries.extend(
                placed
                    .iter()
                    .enumerate()
                    .filter(|(_, placed)| !**placed)
                    .map(|(j, _)| Entry::Agent(j)),
            );
        }
        entries
    }

    pub fn next_change(
        &self,
        agents: &AgentPanel,
        clock: &Clock,
        theme: &Theme,
    ) -> Option<Instant> {
        let ticking = self.rows.iter().filter_map(|row| {
            let at = row.measured_at?;
            let whole = row.elapsed(clock.now).as_secs();
            let into = Duration::from_secs(whole + 1)
                .checked_sub(Duration::from_millis(row.elapsed_ms))?;
            Some(at + into)
        });
        let agents = self
            .show_agents
            .then(|| agents.next_change(clock, theme))
            .flatten();
        let leaving = self.rows.iter().filter_map(|row| row.leaves_at);
        ticking.chain(agents).chain(leaving).min()
    }
}

/// The panel's rows: workflow rows, and agent rows when they show. The live region draws them
/// one blank row under the bottom line. Without runs it's the agent panel alone.
pub fn panel_lines(
    workflows: &WorkflowPanel,
    agents: &AgentPanel,
    ctx: &RenderCtx<'_>,
) -> Vec<Line<'static>> {
    if workflows.rows.is_empty() {
        return agents.lines(ctx);
    }
    let entries = workflows.entries(agents);
    let last = entries.len().saturating_sub(1);
    let selected = workflows.selected.map(|i| i.min(last));
    // Your conversation's row is always there, so it doesn't use up a row of the cap.
    let main = usize::from(agents.rows.iter().any(|row| row.kind == RowKind::Main));
    let visible = window(entries.len(), workflows.max_rows + main, selected);

    let entry_of = |i: Option<usize>| i.and_then(|i| entries.get(i).copied());
    let run_selected = entry_of(selected).and_then(Entry::run);
    let agent_selected = entry_of(selected).and_then(Entry::agent);

    // Every row is measured, not just the ones in view, so columns hold while the list
    // scrolls.
    let runs: Vec<RunCells> = workflows
        .rows
        .iter()
        .map(|row| RunCells {
            name: sanitize_line(&row.name).trim().to_owned(),
            stats: run_stats(row, ctx),
        })
        .collect();
    let widest_stats = runs
        .iter()
        .map(|run| full_width(&run.stats))
        .max()
        .unwrap_or(0);
    let name_cells = |cut: usize| {
        runs.iter()
            .map(|run| str_width(&run.name).min(cut))
            .max()
            .unwrap_or(0)
    };
    let columns = PanelColumns::fit(
        usize::from(ctx.width),
        Names {
            widest: name_cells(MAX_NAME_CELLS),
            longest: name_cells(usize::MAX),
            least: MIN_NAME_CELLS,
        },
        widest_stats,
    );
    // The agents' rows are their own list, in Claude Code's shape, a step in under their run.
    let listed: Vec<ListRow> = entries
        .iter()
        .filter_map(|entry| entry.agent())
        .map(|index| ListRow {
            index,
            indent: match agents.rows[index].kind {
                RowKind::Agent if agents.rows[index].run.is_some() => AGENT_INDENT,
                _ => 0,
            },
        })
        .collect();
    let agent_lines = agents.list_lines(&listed, agent_selected, ctx);

    let mut lines = Vec::new();
    if visible.start > 0 {
        lines.push(more_line(false, visible.start, ctx));
    }
    for entry in &entries[visible.clone()] {
        match *entry {
            Entry::Run(i) => {
                let selected = run_selected == Some(i);
                lines.push(run_line(
                    &workflows.rows[i],
                    &runs[i],
                    selected,
                    &columns,
                    ctx,
                ));
            }
            Entry::Agent(j) => {
                let line = listed.iter().position(|row| row.index == j);
                lines.extend(line.and_then(|at| agent_lines.get(at).cloned()));
            }
        }
    }
    let below = entries.len() - visible.end;
    if below > 0 {
        lines.push(more_line(true, below, ctx));
    }
    lines
}

/// Keys while the panel has focus: ↑↓ pick, Enter or → opens, `x` stops, Esc goes back, and ↓
/// past the last row asks for the workflow view. [`ask_before_stop`] puts the confirm first.
pub fn handle_panel_key(
    workflows: &mut WorkflowPanel,
    agents: &mut AgentPanel,
    key: KeyEvent,
) -> PanelEvent {
    if workflows.rows.is_empty() {
        workflows.blur();
        return agents.handle_key(key);
    }
    // Focus may arrive through the agents' rows; it moves to the panel as a whole.
    if workflows.selected.is_none() {
        if agents.selected.is_none() {
            return PanelEvent::Unhandled;
        }
        agents.blur();
        workflows.focus();
    }
    let entries = workflows.entries(agents);
    let selected = workflows.selected.unwrap_or(0).min(entries.len() - 1);
    workflows.selected = Some(selected);
    let plain = !key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    match key.code {
        KeyCode::Up if selected == 0 => {
            workflows.blur();
            PanelEvent::Leave
        }
        KeyCode::Up => {
            workflows.selected = Some(selected - 1);
            PanelEvent::Moved
        }
        // The window follows the selection, so ↓ scrolls hidden rows in before it leaves.
        KeyCode::Down if selected + 1 < entries.len() => {
            workflows.selected = Some(selected + 1);
            PanelEvent::Moved
        }
        KeyCode::Down => PanelEvent::PastEnd,
        KeyCode::Home => {
            workflows.selected = Some(0);
            PanelEvent::Moved
        }
        KeyCode::End => {
            workflows.selected = Some(entries.len() - 1);
            PanelEvent::Moved
        }
        KeyCode::Enter | KeyCode::Right => match entries[selected] {
            Entry::Run(i) => PanelEvent::OpenWorkflow(workflows.rows[i].key.clone()),
            Entry::Agent(j) => {
                let event = agents.rows[j].open_event();
                if event == PanelEvent::Leave {
                    workflows.blur();
                }
                event
            }
        },
        KeyCode::Char('x') if plain => match entries[selected] {
            Entry::Run(i) => PanelEvent::StopRun(workflows.rows[i].key.clone()),
            Entry::Agent(j) => agents.rows[j].stop_event(),
        },
        KeyCode::Esc => {
            workflows.blur();
            PanelEvent::Leave
        }
        _ => PanelEvent::Unhandled,
    }
}

/// A first `x` on a panel row, waiting for a second on the same row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopAsk {
    pub row: PanelRowId,
    /// Such as `x again to stop codex/gpt-5.6 · esc to keep`, on the bottom line.
    pub hint: String,
}

/// Stops from the panel ask first, like Ctrl+C's double press: the first `x` on a row only
/// asks, and a second `x` on the same row lets the stop through. Any other key keeps the agent.
/// A run can be paused in place of stopped, so its ask offers that too: Esc no longer pauses
/// one, and only an asked row takes `p`, so a message typed with a run's row
/// selected never pauses it.
pub fn ask_before_stop(asked: &mut Option<StopAsk>, event: PanelEvent) -> PanelEvent {
    let earlier = asked.take();
    let (row, name, pause) = match &event {
        PanelEvent::Stop { agent, name } => (PanelRowId::Agent(agent.clone()), name.as_str(), ""),
        PanelEvent::StopRun(run) => (
            PanelRowId::Run(run.clone()),
            run.as_str(),
            " · p to pause it",
        ),
        _ => return event,
    };
    if earlier.is_some_and(|earlier| earlier.row == row) {
        return event;
    }
    let hint = format!(
        "x again to stop {}{pause} · esc to keep",
        sanitize_line(name).trim()
    );
    *asked = Some(StopAsk { row, hint });
    PanelEvent::StopAsked
}

pub fn panel_has_rows(workflows: &WorkflowPanel, agents: &AgentPanel) -> bool {
    !workflows.rows.is_empty() || !agents.rows.is_empty()
}

/// A run's row as the panel draws it: its name, then its stats.
struct RunCells {
    name: String,
    stats: Vec<Part>,
}

fn run_line(
    row: &WorkflowRow,
    cells: &RunCells,
    selected: bool,
    columns: &PanelColumns,
    ctx: &RenderCtx<'_>,
) -> Line<'static> {
    let theme = ctx.theme;
    let ascii = theme.glyphs.is_ascii();
    let pick = |unicode: &'static str, plain: &'static str| if ascii { plain } else { unicode };
    // Claude Code's circle while it goes, its pause mark while it waits, and its tick or cross
    // once it ended.
    let (glyph, role) = match row.state {
        WorkflowRowState::Running => (pick("◯", "o"), Role::Dim),
        WorkflowRowState::NeedsYou => (pick("⏸", "="), Role::Warn),
        // Only a pause you asked for draws the pause mark.
        WorkflowRowState::Paused => (pick("⏸", "="), Role::Text),
        WorkflowRowState::Waiting => (pick("◯", "o"), Role::Dim),
        WorkflowRowState::Done => (pick("✔", "*"), Role::StateDone),
        WorkflowRowState::Failed | WorkflowRowState::Stopped => (pick("✘", "x"), Role::StateFailed),
        // A followed pull request is half done: amber while there's work on it, dim while it
        // waits.
        WorkflowRowState::PullRequest => (pick("◐", "o"), Role::Warn),
        WorkflowRowState::PullRequestWaits => (pick("◐", "o"), Role::Dim),
    };
    let mut spans = if selected {
        vec![Span::styled(
            format!("{} ", theme.glyphs.prompt),
            theme.style(Role::Question),
        )]
    } else {
        vec![Span::raw("  ")]
    };
    // A workflow inside steps in on `⎿`, a step a level, its name column shorter by as much,
    // so every bar starts in one column.
    let step_in = if row.depth > 0 {
        format!(
            "{}  {} ",
            " ".repeat(INSIDE_STEP * (row.depth - 1)),
            theme.glyphs.result
        )
    } else {
        String::new()
    };
    let inner = str_width(&step_in);
    if inner > 0 {
        spans.push(Span::styled(step_in, theme.style(Role::Dim)));
    }
    spans.push(Span::styled(format!("{glyph} "), theme.style(role)));
    // The row is dim unless it is the one selected.
    let name_style = if selected {
        theme.style(Role::Text)
    } else {
        theme.style(Role::Dim)
    };
    // A name inside may take the first of the cells between it and the bar.
    let room = columns.name_width.saturating_sub(inner) + usize::from(inner > 0);
    spans.push(Span::styled(left(&cells.name, room), name_style));
    if columns.bar_cells > 0 {
        let agents = (row.agents_total > 0).then_some((row.agents_done, row.agents_total));
        spans.push(Span::raw(if inner > 0 { " " } else { "  " }));
        spans.extend(bar(agents, columns.bar_cells, theme));
    }
    spans.push(Span::raw("  "));
    fit_parts(spans, &cells.stats, usize::from(ctx.width))
}

/// `2/4 · 4m12s · ↓ 128k tokens`, as Claude Code counts a workflow, then who started it when
/// another agent did, the one thing Argonauts Code adds when there is something to say, and
/// the limit notice. What it says and the notice give way last.
fn run_stats(row: &WorkflowRow, ctx: &RenderCtx<'_>) -> Vec<Part> {
    let theme = ctx.theme;
    let marks = Marks::for_theme(theme);
    let dim = theme.style(Role::Dim);
    let mut parts = Vec::new();
    let mut push = |text: String, style: Style, rank: u16| {
        parts.push(Part::joined(
            Span::styled(" · ", dim),
            vec![Span::styled(text, style)],
            rank,
        ));
    };
    if row.agents_total > 0 {
        push(format!("{}/{}", row.agents_done, row.agents_total), dim, 4);
    }
    if row.timed {
        let elapsed = row.elapsed(ctx.clock.now);
        push(format::duration_slot(elapsed), dim, 3);
    }
    if row.tokens > 0 {
        push(
            format!(
                "{}{}{} tokens",
                theme.glyphs.tokens,
                crate::theme::glyphs::gap_after(theme.glyphs.tokens),
                run_tokens(row.tokens)
            ),
            dim,
            1,
        );
    }
    // Who started the run says more than its time does.
    if let Some(outside_agent) = &row.outside_agent {
        push(sanitize_line(outside_agent).trim().to_owned(), dim, 4);
    }
    if let Some(worker) = &row.run_by {
        push(format!("run by {}", sanitize_line(worker).trim()), dim, 2);
    }
    if let Some((said, role)) = said(row, marks.fallback) {
        push(said, theme.style(role), 6);
    }
    if let Some(carry_on) = row.carry_on.as_deref().map(sanitize_line) {
        // A stop says it in the stop's color, a pause of yours quietly.
        let role = match row.state {
            WorkflowRowState::Stopped | WorkflowRowState::Failed => Role::StateFailed,
            _ => Role::Dim,
        };
        push(carry_on.trim().to_owned(), theme.style(role), 6);
    }
    if let Some(notice) = &row.notice {
        push(
            format!("{} {}", marks.warning, sanitize_line(notice).trim()),
            theme.style(Role::Warn),
            5,
        );
    }
    parts
}

/// The words at a row's end and their color: why it needs you, who fills in while it runs
/// after `mark`, how it ended, and for a run that says nothing more, the goals it still works
/// toward. A run that simply goes on says nothing.
fn said(row: &WorkflowRow, mark: &str) -> Option<(String, Role)> {
    let clean = |words: &Option<String>| {
        words
            .as_deref()
            .map(|words| sanitize_line(words).trim().to_owned())
            .filter(|words| !words.is_empty())
    };
    let filling_in = clean(&row.fallback)
        .filter(|_| row.state == WorkflowRowState::Running)
        .map(|who| format!("{mark} {who}"));
    let own = clean(&row.said).or(filling_in);
    let role = match row.state {
        WorkflowRowState::NeedsYou | WorkflowRowState::Running => Role::Warn,
        WorkflowRowState::Done => Role::StateDone,
        WorkflowRowState::Failed | WorkflowRowState::Stopped => Role::StateFailed,
        WorkflowRowState::Paused => Role::Text,
        WorkflowRowState::Waiting | WorkflowRowState::PullRequest => Role::Warn,
        WorkflowRowState::PullRequestWaits => Role::Dim,
    };
    let words = match (row.state, own) {
        (WorkflowRowState::NeedsYou, Some(why)) => format!("needs you · {why}"),
        (WorkflowRowState::NeedsYou, None) => "needs you".to_owned(),
        (_, Some(said)) => said,
        (WorkflowRowState::Running, None) => {
            let goals = match row.goals_running {
                0 => return None,
                1 => "1 goal running".to_owned(),
                goals => format!("{goals} goals running"),
            };
            return Some((goals, Role::Dim));
        }
        (state, None) => state.word().to_owned(),
    };
    Some((words, role))
}

/// One layout for the whole panel, so every bar and every stat starts in the same column. The
/// name column holds run names and, further in, agent names. A width of zero leaves the bar
/// out.
struct PanelColumns {
    name_width: usize,
    bar_cells: usize,
}

/// What the names ask of the column: `widest` with each cut as a tight row cuts it, `longest`
/// with every name whole, and `least`, the fewest cells they can make do with.
struct Names {
    widest: usize,
    longest: usize,
    least: usize,
}

impl PanelColumns {
    /// While the bar shows, it starts in one column whatever names up to [`MAX_NAME_CELLS`]
    /// are, so a run's bar stays put when its name changes, as it does once the main agent picks
    /// the workflow. A longer name takes the room past the full bar, then the bar's own down to its
    /// least, so it's cut only once the row is full. As the row narrows, the stats stay whole, the
    /// bar shrinks, then the name column gives up cells down to `names.least` to keep it, then it
    /// goes, and then names are cut further.
    fn fit(width: usize, names: Names, stats_width: usize) -> Self {
        // The mark, and the circle and its space.
        let margin = 2 + 2;
        // What the name and the bar share, after the gaps that follow each.
        let shared = width.saturating_sub(margin + 2 + 2 + stats_width);
        if shared >= MAX_NAME_CELLS + MIN_BAR_CELLS {
            let name_width = names.longest.clamp(MAX_NAME_CELLS, shared - MIN_BAR_CELLS);
            return Self {
                name_width,
                bar_cells: (shared - name_width).min(BAR_CELLS),
            };
        }
        let kept = shared.saturating_sub(MIN_BAR_CELLS);
        if kept >= names.least {
            return Self {
                name_width: kept,
                bar_cells: MIN_BAR_CELLS,
            };
        }
        // Without the bar, names take what they need, and keep at least half of what's left
        // after the main agent.
        let room = width.saturating_sub(margin);
        let name_width = names
            .widest
            .min(room.saturating_sub(stats_width + 2).max(room / 2));
        Self {
            name_width,
            bar_cells: 0,
        }
    }
}

/// A run's tokens with one decimal in the thousands whatever their size, `538.7k`, as its
/// workflow view keeps them, and none when it is `.0`: `128k`. Millions as every count has them.
fn run_tokens(count: u64) -> String {
    if !(1_000..999_500).contains(&count) {
        return format::tokens(count);
    }
    let thousands = format!("{:.1}", count as f64 / 1_000.0);
    format!("{}k", thousands.strip_suffix(".0").unwrap_or(&thousands))
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyEvent;

    use super::*;
    use crate::bottom::{AgentRow, AgentState};
    use crate::stability::assert_lines;
    use crate::text::plain_text;
    use crate::theme::{ColorLevel, Glyphs};

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn runs(count: usize) -> WorkflowPanel {
        WorkflowPanel {
            rows: (0..count)
                .map(|i| {
                    let mut row = WorkflowRow::new(
                        format!("run-{i}"),
                        format!("workflow-{i}"),
                        WorkflowRowState::Running,
                    );
                    row.agents_done = 1;
                    row.agents_total = 3;
                    row
                })
                .collect(),
            max_rows: 2,
            ..WorkflowPanel::default()
        }
    }

    #[test]
    fn panel_down_scrolls_then_past_end() {
        let theme = theme();
        let t0 = Instant::now();
        let ctx = RenderCtx::new(100, &theme, Clock::still(t0));
        let mut panel = runs(5);
        let mut agents = AgentPanel::default();
        panel.focus();
        let text = plain_text(&panel_lines(&panel, &agents, &ctx));
        assert!(text.ends_with("  ↓  3 more"), "{text}");
        assert!(!text.contains("workflow-2"), "{text}");

        for expected in 1..5 {
            assert_eq!(
                handle_panel_key(&mut panel, &mut agents, key(KeyCode::Down)),
                PanelEvent::Moved
            );
            assert_eq!(panel.selected, Some(expected));
            let text = plain_text(&panel_lines(&panel, &agents, &ctx));
            assert!(text.contains(&format!("❯ ◯ workflow-{expected}")), "{text}");
        }
        let text = plain_text(&panel_lines(&panel, &agents, &ctx));
        assert!(text.starts_with("  ↑  3 more"), "{text}");
        assert_eq!(
            handle_panel_key(&mut panel, &mut agents, key(KeyCode::Down)),
            PanelEvent::PastEnd
        );
        assert_eq!(panel.selected, Some(4));
    }

    #[test]
    fn enter_on_run_row_opens_workflow() {
        let mut panel = runs(2);
        panel.show_agents = true;
        let mut agents = AgentPanel::default();
        let mut codex = AgentRow::new("codex", "worker · task-002", AgentState::Working);
        codex.key = Some("s-002".into());
        codex.run = Some("run-0".into());
        agents.rows.push(codex);

        // Focus that came through the agents' rows moves to the panel as a whole.
        agents.focus();
        assert_eq!(
            handle_panel_key(&mut panel, &mut agents, key(KeyCode::Enter)),
            PanelEvent::OpenWorkflow("run-0".into())
        );
        assert!(!agents.has_focus() && panel.has_focus());
        assert_eq!(
            handle_panel_key(&mut panel, &mut agents, key(KeyCode::Down)),
            PanelEvent::Moved
        );
        let codex = AgentRef {
            run: Some("run-0".into()),
            session: "s-002".into(),
        };
        assert_eq!(
            handle_panel_key(&mut panel, &mut agents, key(KeyCode::Right)),
            PanelEvent::Open(codex.clone())
        );
        assert_eq!(
            handle_panel_key(&mut panel, &mut agents, key(KeyCode::Char('x'))),
            PanelEvent::Stop {
                agent: codex,
                name: "codex".into(),
            }
        );
        assert_eq!(
            handle_panel_key(&mut panel, &mut agents, key(KeyCode::Down)),
            PanelEvent::Moved
        );
        assert_eq!(
            handle_panel_key(&mut panel, &mut agents, key(KeyCode::Enter)),
            PanelEvent::OpenWorkflow("run-1".into())
        );

        // A run's row stops the run, never an agent that happens to share its name.
        let theme = theme();
        let t0 = Instant::now();
        let ctx = RenderCtx::new(100, &theme, Clock::still(t0));
        let before = plain_text(&panel_lines(&panel, &agents, &ctx));
        assert_eq!(
            handle_panel_key(&mut panel, &mut agents, key(KeyCode::Char('x'))),
            PanelEvent::StopRun("run-1".into())
        );
        assert_eq!(plain_text(&panel_lines(&panel, &agents, &ctx)), before);
        assert_eq!(
            handle_panel_key(&mut panel, &mut agents, key(KeyCode::Esc)),
            PanelEvent::Leave
        );
        assert!(!panel.has_focus());
        assert_eq!(
            handle_panel_key(&mut panel, &mut agents, key(KeyCode::Enter)),
            PanelEvent::Unhandled
        );
    }

    #[test]
    fn agent_rows_are_known_by_run_and_session() {
        let mut panel = runs(2);
        panel.show_agents = true;
        let mut agents = AgentPanel::default();
        let row = |run: &str| {
            let mut row = AgentRow::new("claude-code", "worker", AgentState::Working);
            row.key = Some("s-001".into());
            row.run = Some(run.into());
            row
        };
        agents.rows = vec![row("run-0"), row("run-1")];
        panel.focus();
        // run-0, its agent, run-1, then run-1's agent.
        panel.selected = Some(3);
        let id = panel.selected_id(&agents);
        assert_eq!(
            id,
            Some(PanelRowId::Agent(AgentRef {
                run: Some("run-1".into()),
                session: "s-001".into(),
            }))
        );
        // run-0's agent finished, so run-1's rows move up one.
        agents.rows.remove(0);
        panel.reselect(&agents, id.as_ref());
        assert_eq!(panel.selected, Some(2));
        assert_eq!(
            handle_panel_key(&mut panel, &mut agents, key(KeyCode::Enter)),
            PanelEvent::Open(AgentRef {
                run: Some("run-1".into()),
                session: "s-001".into(),
            })
        );
    }

    /// Under a run's row each agent is a row of Claude Code's list, a step further in: its
    /// label, what it's doing, and how many tokens. Your conversation's row comes first.
    #[test]
    fn agents_under_a_run_read_by_label_and_what_they_do() {
        let mut theme = Theme::dark().with_level(ColorLevel::None);
        theme.glyphs = Glyphs::UNICODE;
        let t0 = Instant::now();
        let mut panel = runs(1);
        panel.show_agents = true;
        panel.max_rows = 6;
        let mut agents = AgentPanel::default();
        agents.rows.push(AgentRow::main());
        for (label, doing) in [
            ("build:flag", "Update(src/export.rs)"),
            ("review:flag", "Reading the diff"),
        ] {
            let mut row =
                AgentRow::new("claude-code", doing, AgentState::Working).with_label(label, label);
            row.run = Some("run-0".into());
            row.tokens_down = 18_100;
            agents.rows.push(row);
        }
        let at = |width| {
            let ctx = RenderCtx::new(width, &theme, Clock::still(t0));
            plain_text(&panel_lines(&panel, &agents, &ctx))
        };
        let wide = at(100);
        let rows: Vec<&str> = wide.lines().collect();
        assert_eq!(rows[0], "  ● main", "{wide}");
        assert!(rows[1].starts_with("  ◯ workflow-0"), "{wide}");
        assert!(
            rows[2].starts_with("    ○ build:flag   Update(src/export.rs)")
                && rows[2].ends_with("↓  18.1k tokens"),
            "{wide}"
        );
        assert!(
            rows[3].starts_with("    ○ review:flag  Reading the diff"),
            "{wide}"
        );
        // What it's doing reads in the text color, and the stats dim.
        let colored = self::theme();
        let ctx = RenderCtx::new(100, &colored, Clock::still(t0));
        let lines = panel_lines(&panel, &agents, &ctx);
        let style_of = |text: &str| {
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .find(|span| span.content.contains(text))
                .unwrap_or_else(|| panic!("{text} shows"))
                .style
        };
        assert_eq!(style_of("Reading the diff"), colored.style(Role::Text));
        assert_eq!(style_of("18.1k tokens"), colored.style(Role::Dim));
        // A narrow row keeps the label and the tokens, and what it's doing gives way.
        let narrow = at(40);
        let agent_rows: Vec<&str> = narrow.lines().filter(|row| row.contains(":flag")).collect();
        assert!(
            agent_rows[0].starts_with("    ○ build:flag")
                && agent_rows[0].contains("↓  18.1k tokens")
                && !agent_rows[0].contains("Update"),
            "{narrow}"
        );
    }

    /// The main agent's own subagents aren't a run's agents: they list under `main`, before
    /// the runs, whether or not `ui.panel.agents` lists each run's.
    #[test]
    fn the_conversations_subagents_come_first_and_always_show() {
        let theme = theme();
        let t0 = Instant::now();
        let mut panel = runs(1);
        panel.max_rows = 6;
        let mut agents = AgentPanel::default();
        agents.rows.push(AgentRow::main());
        let mut sub = AgentRow::new("Count slowly", "Reading src", AgentState::Working)
            .with_label("Count slowly", "Count slowly");
        sub.kind = RowKind::Subagent;
        sub.run = Some("conversation".into());
        sub.key = Some("toolu_a".into());
        agents.rows.push(sub);
        let mut worker = AgentRow::new("codex", "Update(src/a.rs)", AgentState::Working)
            .with_label("build:flag", "build:flag");
        worker.run = Some("run-0".into());
        agents.rows.push(worker);
        let listed = |panel: &WorkflowPanel| {
            let ctx = RenderCtx::new(100, &theme, Clock::still(t0));
            let text = plain_text(&panel_lines(panel, &agents, &ctx));
            text.lines()
                .map(|row| row.split_whitespace().take(2).collect::<Vec<_>>().join(" "))
                .collect::<Vec<_>>()
        };
        assert_eq!(listed(&panel), ["● main", "○ Count", "◯ workflow-0"]);
        panel.show_agents = true;
        assert_eq!(
            listed(&panel),
            ["● main", "○ Count", "◯ workflow-0", "○ build:flag"]
        );
        // ↓ from the input lands on the first row past main, and typing there is a message.
        panel.focus_past_main(&agents);
        assert_eq!(
            panel.selected_id(&agents),
            Some(PanelRowId::Agent(AgentRef {
                run: Some("conversation".into()),
                session: "toolu_a".into()
            }))
        );
        assert_eq!(listed(&panel)[..2], ["○ main", "● Count"]);
    }

    /// The picture's run in each state its row has, all named alike.
    fn pictured(done: u32, total: u32, seconds: u64, tokens: u64) -> WorkflowRow {
        let mut row = WorkflowRow::new("run-0", "Add due dates", WorkflowRowState::Running);
        row.agents_done = done;
        row.agents_total = total;
        row.elapsed_ms = seconds * 1000;
        row.tokens = tokens;
        row
    }

    /// A run at work, one that stopped fully, and one you paused, as the bottom list draws each
    /// with the cursor on it at the width of 118: only /resume carries the last two on.
    #[test]
    fn a_stopped_run_and_one_you_paused_read_as_their_pictures() {
        use crate::views::workflow_pictures::{buffer_of, dark, picture};
        let theme = dark();
        let ctx = RenderCtx::new(118, &theme, Clock::still(Instant::now()));
        let row = |state, (done, total): (u32, u32), seconds: u64, tokens: u64| {
            let mut row = pictured(done, total, seconds, tokens);
            row.name = "Per key rate limit".to_owned();
            row.state = state;
            row
        };
        let stopped = WorkflowRow {
            said: Some("stopped".to_owned()),
            carry_on: Some("/resume carries it on".to_owned()),
            ..row(WorkflowRowState::Stopped, (0, 3), 41, 6_600)
        };
        let paused = WorkflowRow {
            said: Some("paused by you".to_owned()),
            carry_on: Some("/resume carries it on".to_owned()),
            ..row(WorkflowRowState::Paused, (1, 3), 62, 31_400)
        };
        let cases = [
            (28, row(WorkflowRowState::Running, (1, 3), 62, 31_400)),
            (40, stopped),
            (48, paused),
        ];
        let picture = picture!("waiting_rows");
        for (at, row) in cases {
            let panel = WorkflowPanel {
                rows: vec![row],
                max_rows: 5,
                selected: Some(0),
                ..WorkflowPanel::default()
            };
            let lines = panel_lines(&panel, &AgentPanel::default(), &ctx);
            picture.assert_drawn(at..at + 1, &buffer_of(&lines, 118), 0);
        }
    }

    /// Every case of a workflow inside in the bottom list, as one list: a worker's workflow at
    /// work, then ended; Captain's second workflow at work, then merged; the deepest, with two
    /// `⎿` rows a step apart; and the cursor on a row inside, with another run below. Every bar
    /// starts in one column.
    #[test]
    fn the_rows_inside_read_as_their_picture() {
        let theme = crate::views::workflow_pictures::dark();
        let row = |name: &str, (done, total): (u32, u32), seconds: u64, tokens: u64| {
            let mut row = pictured(done, total, seconds, tokens);
            row.name = name.to_owned();
            row
        };
        let inside = |mut row: WorkflowRow, depth: usize, by: Option<&str>| {
            row.depth = depth;
            row.run_by = by.map(str::to_owned);
            row
        };
        let mut merged = row("Fix the money rounding", (8, 8), 126, 226_000);
        merged.state = WorkflowRowState::Done;
        merged.said = Some("merged onto main".to_owned());
        let by = Some("build:due-flag");
        let rows = vec![
            row("Add due dates", (3, 5), 172, 91_400),
            inside(row("add-due-flag", (2, 3), 118, 25_900), 1, by),
            row("Add due dates", (5, 5), 211, 99_500),
            row("Fix the money rounding", (6, 7), 100, 201_000),
            inside(row("final-signoff-money", (1, 2), 15, 48_300), 1, None),
            merged,
            row("Add due dates", (5, 7), 197, 101_000),
            inside(row("add-due-flag", (4, 5), 143, 35_100), 1, by),
            inside(row("date-cases", (1, 2), 22, 6_600), 2, None),
            row("Add due dates", (3, 5), 172, 91_400),
            inside(row("add-due-flag", (2, 3), 118, 25_900), 1, by),
            row("Per key rate limit", (1, 2), 62, 25_200),
        ];
        let panel = WorkflowPanel {
            rows,
            max_rows: 20,
            selected: Some(10),
            ..WorkflowPanel::default()
        };
        let ctx = RenderCtx::new(124, &theme, Clock::still(Instant::now()));
        let lines = panel_lines(&panel, &AgentPanel::default(), &ctx);
        let picture = crate::views::workflow_pictures::picture!("nested_bottom_list");
        let drawn = crate::views::workflow_pictures::buffer_of(&lines, 124);
        for (rows, at) in [
            (1..3, 0),
            (5..6, 2),
            (8..10, 3),
            (12..13, 5),
            (15..18, 6),
            (20..23, 9),
        ] {
            picture.assert_drawn(rows, &drawn, at);
        }
    }

    /// The rows as their picture draws them, cell for cell and colour for colour: Claude Code's
    /// row, and at its end who fills in, why it needs you, or how it ended.
    #[test]
    fn the_rows_read_as_their_picture() {
        let theme = crate::views::workflow_pictures::dark();
        let said = |mut row: WorkflowRow, state: WorkflowRowState, said: &str| {
            row.state = state;
            row.said = Some(said.to_owned());
            row
        };
        let panel = WorkflowPanel {
            rows: vec![
                pictured(2, 4, 252, 128_000),
                WorkflowRow {
                    fallback: Some("Sonnet 5.5 for Opus 5.5".to_owned()),
                    ..pictured(1, 3, 146, 83_600)
                },
                said(
                    pictured(1, 3, 166, 84_200),
                    WorkflowRowState::NeedsYou,
                    "Claude Code is down",
                ),
                said(
                    pictured(6, 6, 380, 177_000),
                    WorkflowRowState::Done,
                    "merged onto main",
                ),
                said(
                    pictured(3, 6, 302, 140_000),
                    WorkflowRowState::Stopped,
                    "stopped · /resume carries it on",
                ),
            ],
            max_rows: 5,
            selected: Some(4),
            ..WorkflowPanel::default()
        };
        let t0 = Instant::now();
        let ctx = RenderCtx::new(124, &theme, Clock::still(t0));
        let lines = panel_lines(&panel, &AgentPanel::default(), &ctx);
        let picture = crate::views::workflow_pictures::picture!("run_rows");
        let drawn = crate::views::workflow_pictures::buffer_of(&lines, 124);
        picture.assert_drawn(0..5, &drawn, 0);
    }

    /// The moment a run's steps are known its row shows them all as agents to come: its name,
    /// an empty bar, none of the four done and its time. No token is counted yet, so none
    /// are shown, as in Claude Code's row. The time then counts by itself.
    #[test]
    fn a_run_whose_steps_were_just_written_has_an_empty_bar_and_its_time() {
        let theme = crate::views::workflow_pictures::dark();
        let t0 = Instant::now();
        let mut row = pictured(0, 4, 2, 0);
        row.measured_at = Some(t0);
        let panel = WorkflowPanel {
            rows: vec![row],
            max_rows: 5,
            ..WorkflowPanel::default()
        };
        let read = |at: Instant| {
            let ctx = RenderCtx::new(124, &theme, Clock::still(at));
            plain_text(&panel_lines(&panel, &AgentPanel::default(), &ctx))
        };
        let empty = "▱".repeat(BAR_CELLS);
        assert_eq!(
            read(t0),
            format!("  ◯ Add due dates           {empty}  0/4 · 2s")
        );
        assert_eq!(
            read(t0 + Duration::from_secs(1)),
            format!("  ◯ Add due dates           {empty}  0/4 · 3s")
        );
    }

    /// Each picture of the workflow view ends with the run's row in the bottom list, which is
    /// this same row.
    #[test]
    fn the_row_under_each_view_reads_as_its_picture() {
        use crate::views::workflow_pictures::{buffer_of, dark, picture};
        let theme = dark();
        let ctx = RenderCtx::new(124, &theme, Clock::still(Instant::now()));
        let row = |mut row: WorkflowRow, state: WorkflowRowState, said: Option<&str>| {
            row.state = state;
            row.said = said.map(str::to_owned);
            row
        };
        let done = WorkflowRowState::Done;
        let cases = [
            (
                picture!("workflow_running"),
                14,
                row(
                    pictured(2, 4, 252, 128_000),
                    WorkflowRowState::Running,
                    None,
                ),
            ),
            (
                picture!("workflow_done"),
                14,
                row(pictured(6, 6, 380, 177_000), done, Some("merged onto main")),
            ),
            (
                picture!("workflow_done_no_git"),
                14,
                row(
                    pictured(5, 5, 380, 165_000),
                    done,
                    Some("written into your folder"),
                ),
            ),
            (
                picture!("workflow_cli_down"),
                22,
                row(
                    pictured(1, 3, 166, 84_200),
                    WorkflowRowState::NeedsYou,
                    Some("Claude Code is down"),
                ),
            ),
        ];
        for (picture, at, row) in cases {
            let panel = WorkflowPanel {
                rows: vec![row],
                max_rows: 5,
                ..WorkflowPanel::default()
            };
            let lines = panel_lines(&panel, &AgentPanel::default(), &ctx);
            picture.assert_drawn(at..at + 1, &buffer_of(&lines, 124), 0);
        }
    }

    /// A Captain run's row the moment its phases are known, and once it merged.
    #[test]
    fn a_captain_runs_row_reads_as_its_pictures() {
        use crate::views::workflow_pictures::{buffer_of, dark, picture};
        let theme = dark();
        let ctx = RenderCtx::new(124, &theme, Clock::still(Instant::now()));
        let captain = |done: u32, total: u32, seconds: u64, tokens: u64| {
            let mut row =
                WorkflowRow::new("run-0", "Fix the money rounding", WorkflowRowState::Running);
            row.agents_done = done;
            row.agents_total = total;
            row.elapsed_ms = seconds * 1000;
            row.tokens = tokens;
            row
        };
        let mut merged = captain(5, 5, 91, 153_000);
        merged.state = WorkflowRowState::Done;
        merged.said = Some("merged onto main".to_owned());
        let cases = [
            (
                picture!("workflow_shape_known"),
                27,
                captain(0, 1, 15, 2_200),
            ),
            (picture!("captain_done"), 24, merged),
        ];
        for (picture, at, row) in cases {
            let panel = WorkflowPanel {
                rows: vec![row],
                max_rows: 5,
                ..WorkflowPanel::default()
            };
            let lines = panel_lines(&panel, &AgentPanel::default(), &ctx);
            picture.assert_drawn(at..at + 1, &buffer_of(&lines, 124), 0);
        }
    }

    /// A run near its limit says so after Claude Code's counts, in the warning color, and its
    /// time keeps counting.
    #[test]
    fn a_run_near_its_limit_says_so_at_the_rows_end() {
        let theme = theme();
        let t0 = Instant::now();
        let ctx = RenderCtx::new(150, &theme, Clock::new(t0, t0 + Duration::from_secs(3)));
        let mut row = WorkflowRow::new("run-0", "argonauts-ui-kit", WorkflowRowState::Running);
        row.agents_done = 8;
        row.agents_total = 9;
        row.elapsed_ms = 9_837_400;
        row.measured_at = Some(t0);
        row.tokens = 1_800_000;
        row.notice = Some("85% of limit · /workflows to stop".into());
        let panel = WorkflowPanel {
            rows: vec![row],
            max_rows: 5,
            ..WorkflowPanel::default()
        };
        let lines = panel_lines(&panel, &AgentPanel::default(), &ctx);
        let pad = " ".repeat(MAX_NAME_CELLS - "argonauts-ui-kit".len());
        let done = "▰".repeat(8 * BAR_CELLS / 9);
        let left = "▱".repeat(BAR_CELLS - 8 * BAR_CELLS / 9);
        assert_eq!(
            plain_text(&lines),
            format!(
                "  ◯ argonauts-ui-kit{pad}  {done}{left}  8/9 · 2h44m · ↓  1.8M tokens · ! 85% of limit · /workflows to stop"
            )
        );
        let notice = lines[0].spans.last().map(|span| span.style);
        assert_eq!(notice, Some(theme.style(Role::Warn)));
        assert_eq!(
            panel.next_change(&AgentPanel::default(), &ctx.clock, &theme),
            Some(t0 + Duration::from_millis(3_600))
        );
    }

    /// A run with three agents under it, four minutes in. `long` gives one agent a model name
    /// too long for the name column.
    fn crew(long: bool) -> (WorkflowPanel, AgentPanel, Clock) {
        let t0 = Instant::now();
        let mut run = WorkflowRow::new("run-0", "add --json to export", WorkflowRowState::Running);
        run.agents_done = 2;
        run.agents_total = 5;
        run.elapsed_ms = 252_000;
        run.tokens = 54_300;
        let panel = WorkflowPanel {
            rows: vec![run],
            max_rows: 5,
            show_agents: true,
            ..WorkflowPanel::default()
        };
        let codex = if long {
            "gpt-5.6-codex-max-preview-2026-09"
        } else {
            "gpt-5.6"
        };
        let mut agents = AgentPanel::default();
        for (name, model, done, tokens) in [
            ("claude-code", "opus", 2, 18_100),
            ("codex", codex, 1, 12_900),
            ("agy", "gemini-3.6-flash", 0, 0),
        ] {
            let mut row = AgentRow::new(name, "task-002", AgentState::Working).with_model(model);
            row.steps = Some((done, 5));
            row.started_at = Some(t0);
            row.tokens_down = tokens;
            row.run = Some("run-0".into());
            agents.rows.push(row);
        }
        (panel, agents, Clock::new(t0, t0 + Duration::from_secs(252)))
    }

    /// Where each row's bar starts and how many cells it takes, from its cells' glyphs.
    fn bars(lines: &[Line<'_>], theme: &Theme) -> Vec<Option<(usize, usize)>> {
        let glyphs = ["▰", "▱", theme.glyphs.bar_fill, theme.glyphs.bar_track];
        let is_bar = |span: &Span<'_>| {
            !span.content.is_empty()
                && span
                    .content
                    .chars()
                    .all(|c| glyphs.iter().any(|glyph| glyph.contains(c)))
        };
        lines
            .iter()
            .map(|line| {
                let mut column = 0;
                let mut found: Option<(usize, usize)> = None;
                for span in &line.spans {
                    let width = str_width(&span.content);
                    if is_bar(span) {
                        found.get_or_insert((column, 0)).1 += width;
                    } else if found.is_some() {
                        break;
                    }
                    column += width;
                }
                found
            })
            .collect()
    }

    #[test]
    fn a_runs_bar_keeps_its_column_when_its_name_changes() {
        let theme = theme();
        for width in [80, 100, 120, 200] {
            let starts: Vec<Option<usize>> = ["goal", "add --json to export"]
                .into_iter()
                .map(|name| {
                    let (mut panel, _, clock) = crew(false);
                    panel.rows[0].name = name.into();
                    let ctx = RenderCtx::new(width, &theme, clock);
                    let lines = panel_lines(&panel, &AgentPanel::default(), &ctx);
                    bars(&lines, &theme)[0].map(|(start, _)| start)
                })
                .collect();
            assert!(starts[0].is_some(), "{width} columns");
            assert_eq!(starts[0], starts[1], "{width} columns");
        }
    }

    #[test]
    fn a_long_run_name_takes_the_room_the_row_has() {
        let theme = theme();
        let name = "council · Add a --json flag to export, with tests";
        for (width, whole) in [(80, false), (100, true), (120, true)] {
            let (mut panel, _, clock) = crew(false);
            panel.rows[0].name = name.into();
            let ctx = RenderCtx::new(width, &theme, clock);
            let lines = panel_lines(&panel, &AgentPanel::default(), &ctx);
            assert_lines(&lines, width);
            let text = plain_text(&lines);
            assert_eq!(text.contains(name), whole, "{width} columns\n{text}");
            assert!(bars(&lines, &theme)[0].is_some(), "{width} columns\n{text}");
        }
    }

    /// A run's row keeps its bar and stats, and the agents under it are rows of the list: a
    /// step in, with no bar of their own, their names and stats in columns of their own.
    #[test]
    fn a_run_keeps_its_bar_and_its_agents_list_under_it() {
        let mut plain = Theme::dark().with_level(ColorLevel::None);
        plain.glyphs = Glyphs::UNICODE;
        for theme in [theme(), plain] {
            for long in [false, true] {
                for width in [80, 100, 120] {
                    let (panel, agents, clock) = crew(long);
                    let ctx = RenderCtx::new(width, &theme, clock);
                    let lines = panel_lines(&panel, &agents, &ctx);
                    assert_lines(&lines, width);
                    let text = plain_text(&lines);
                    assert_eq!(lines.len(), 4, "{text}");
                    let bars = bars(&lines, &theme);
                    assert!(bars[0].is_some(), "{width} columns\n{text}");
                    assert!(
                        bars[1..].iter().all(Option::is_none),
                        "{width} columns: {bars:?}\n{text}"
                    );
                    let rows: Vec<&str> = text.lines().collect();
                    assert!(rows[1].starts_with("    ○ claude-code/opus"), "{text}");
                    assert!(rows[3].starts_with("    ○ agy/gemini-3.6-flash"), "{text}");
                    assert!(rows[1].ends_with("2/5 · 4m · ↓  18.1k tokens"), "{text}");
                    // The agents' stats start in one column.
                    let column = |row: &str, part: &str| str_width(&row[..row.find(part).unwrap()]);
                    assert_eq!(column(rows[1], "2/5"), column(rows[3], "0/5"), "{text}");
                }
            }
        }
    }

    /// A workflow inside a run follows the run's row on `⎿`, its bar in the run's column and
    /// who runs it after its numbers, and the run's agents come after it.
    #[test]
    fn a_workflow_inside_follows_its_runs_row() {
        let mut theme = Theme::dark().with_level(ColorLevel::None);
        theme.glyphs = Glyphs::UNICODE;
        let t0 = Instant::now();
        let mut panel = runs(1);
        panel.show_agents = true;
        panel.max_rows = 8;
        let mut inner = WorkflowRow::new(
            "run-0#add-due-dates",
            "add-due-dates",
            WorkflowRowState::Running,
        );
        inner.depth = 1;
        inner.run_by = Some("build:add-due-dates".into());
        inner.agents_done = 1;
        inner.agents_total = 3;
        panel.rows.push(inner);
        let mut agents = AgentPanel::default();
        agents.rows.push(AgentRow::main());
        for (key, label) in [
            ("s-001", "plan:due-dates"),
            ("s-002", "build:add-due-dates"),
        ] {
            let mut row = AgentRow::new("claude-code", "Working", AgentState::Working)
                .with_label(label, label);
            row.run = Some("run-0".into());
            row.key = Some(key.into());
            agents.rows.push(row);
        }
        let ctx = RenderCtx::new(100, &theme, Clock::still(t0));
        let text = plain_text(&panel_lines(&panel, &agents, &ctx));
        let rows: Vec<&str> = text.lines().collect();
        assert!(rows[1].starts_with("  ◯ workflow-0"), "{text}");
        assert!(rows[2].starts_with("    ⎿ ◯ add-due-dates"), "{text}");
        assert!(rows[2].contains("  1/3 · "), "{text}");
        assert!(rows[2].contains("run by build:add-due-dates"), "{text}");
        let bar = |row: &str| row.chars().position(|cell| cell == '▰' || cell == '▱');
        assert_eq!(
            bar(rows[1]),
            bar(rows[2]),
            "one column for every bar: {text}"
        );
        assert!(rows[4].starts_with("    ○ build:add-due-dates"), "{text}");
        assert_eq!(rows.len(), 5, "and listed once: {text}");
    }

    /// Two runs at once: a workflow inside follows its own run's row.
    #[test]
    fn a_workflow_inside_follows_its_own_run() {
        let mut theme = Theme::dark().with_level(ColorLevel::None);
        theme.glyphs = Glyphs::UNICODE;
        let t0 = Instant::now();
        let mut panel = runs(2);
        panel.max_rows = 12;
        let mut inner =
            WorkflowRow::new("run-1#fix-median", "fix-median", WorkflowRowState::Running);
        inner.depth = 1;
        panel.rows.push(inner);
        let ctx = RenderCtx::new(100, &theme, Clock::still(t0));
        let text = plain_text(&panel_lines(&panel, &AgentPanel::default(), &ctx));
        let rows: Vec<&str> = text.lines().collect();
        let at = |part: &str| rows.iter().position(|row| row.contains(part));
        assert_eq!(
            at("fix-median"),
            at("workflow-1").map(|run| run + 1),
            "{text}"
        );
    }
}
