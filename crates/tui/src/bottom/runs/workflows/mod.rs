//! The run list under the bottom line, in the shape of Claude Code's workflow rows. Each request
//! is one row, its tasks sit under it, and under a task whatever does its work: a workflow's
//! steps, or its agents. A row is its mark, its name in a fixed column, on a task how it is done,
//! Claude Code's bar of what sits under the row done, then `2/4 · 4m12s · ↓  128k tokens` with
//! each field right-aligned in a column of its own, so every ` · ` lines up down the list. Names
//! are dim unless the row is picked. With `ui.panel.agents`, a run's agents follow under it.

use std::time::{Duration, Instant};

use argonauts_core::base::names::ApprovalId;
use argonauts_core::look::logos::Logo;
use argonauts_core::runs::events::run::RunOutcome;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::base::draw::render::{Clock, RenderCtx};
use crate::base::format;
use crate::base::kit::logo::{LOGO_WIDTH, ModelLabel, NoPicture, logo_spans, numbered_spans};
use crate::base::text::{sanitize_line, str_width, truncate_line};
use crate::base::theme::glyphs::gap_after;
use crate::base::theme::{Role, Theme};

use super::super::{AgentPanel, PanelEvent};
use super::agents::{AgentRef, ListRow, RowKind, more_line, window};
use super::bar::{BAR_CELLS, MIN_BAR_CELLS, bar};

/// The cells before a top-level row's mark: the pick mark and a blank.
const INSET: usize = 2;

/// The mark's cells, so a mark the font draws two cells wide still has a blank before the name.
const MARK_CELLS: usize = 3;
/// The fewest cells a row's name keeps before its logo gives way, however deep the row.
const NAME_FLOOR: usize = 10;

/// The name column of a top-level row. A row further in starts later and ends in the same
/// column, so every bar starts in one column.
const NAME_CELLS: usize = 22;

/// The fewest cells the name column keeps on a row too narrow for everything.
const MIN_NAME_CELLS: usize = 12;

/// Each level in starts this many cells further in.
const STEP_IN: usize = 4;

/// How a task is done: 13 cells of words at most, then a blank.
const WAY_CELLS: usize = 14;

/// From this many columns the way column shows. Below it, Enter on a task still shows how it
/// runs.
const WAY_FROM: usize = 100;

/// Blanks after the name column, and after the bar.
const GAP: usize = 2;

/// Between the count, the time and the tokens.
const SEP: &str = " · ";
const SEP_CELLS: usize = 3;

/// Rows of a run's agents sit this much further in than its row.
const AGENT_INDENT: usize = 2;

/// The mark of a row at work, turning a frame every quarter second as Claude Code turns its own.
const SPINNER: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];
const SPIN: Duration = Duration::from_millis(250);

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

/// What a row is, which says which marks it takes and what it counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunRowKind {
    /// One request, the run that does it.
    Request,
    /// A task of a request, or a workflow at work inside a run.
    Task,
    /// One step of a task's workflow, or one of a task's agents: whatever does the work. It
    /// takes the agents' marks, ○ at work and ✔ done.
    Work,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowRow {
    /// The run id, or the run's id and the row's after a `#`, reported back when the row is
    /// opened.
    pub key: String,
    pub name: String,
    pub kind: RunRowKind,
    pub state: WorkflowRowState,
    /// Done of total for what sits under the row: a request's tasks, a task's steps, agents or
    /// to-do items. `None` for a row with nothing under it to count, which says what it is doing
    /// in the bar's place.
    pub count: Option<(u32, u32)>,
    /// What a row without a count is doing, such as `Reading the diff`.
    pub doing: Option<String>,
    /// How a task is done, in a word or two: `Own session`, its workflow's name, `3 agents`.
    pub way: Option<String>,
    /// Whoever serves the row's model.
    pub logo: Option<Logo>,
    /// A council seat's place among the seats of its model, drawn with its logo, since one logo
    /// can't tell two seats of one model apart.
    pub number: Option<u8>,
    /// The name is a model's whole name, which drops its maker's word beside the row's logo
    /// where that logo draws as a picture, as a council seat's row names it.
    pub model_named: bool,
    pub elapsed_ms: u64,
    /// While the run goes on, when `elapsed_ms` was measured, so the time keeps counting.
    pub measured_at: Option<Instant>,
    /// False for a run this screen doesn't hold, whose time it doesn't know, so its row shows
    /// none.
    pub timed: bool,
    /// The output tokens of every session under the row.
    pub tokens: u64,
    /// It has used most of its limit, so its bar fills in amber.
    pub near_limit: bool,
    /// How far in the row sits: 0 for a request, 1 for its tasks, 2 for what does a task's work,
    /// and 3 for a workflow one of those runs.
    pub depth: usize,
    /// The session the row is, which Enter opens. Without one Enter opens the run's view.
    pub session: Option<AgentRef>,
    /// Where Enter lands in the run's view when that isn't the row's own key, `<run>#<workflow>`:
    /// the workflow a task done by a workflow or by agents works in.
    pub opens: Option<String>,
    /// The question of the row that waits on you, which Enter brings up over the run's view in
    /// place of opening the row.
    pub answers: Option<ApprovalId>,
    /// When the row of a run that ended leaves the list.
    pub leaves_at: Option<Instant>,
}

impl WorkflowRow {
    pub fn new(key: impl Into<String>, name: impl Into<String>, state: WorkflowRowState) -> Self {
        Self {
            key: key.into(),
            name: name.into(),
            kind: RunRowKind::Request,
            state,
            count: None,
            doing: None,
            way: None,
            logo: None,
            number: None,
            model_named: false,
            elapsed_ms: 0,
            measured_at: None,
            timed: true,
            tokens: 0,
            near_limit: false,
            depth: 0,
            session: None,
            opens: None,
            answers: None,
            leaves_at: None,
        }
    }

    /// What Enter does on the row, in the words the key line says it with.
    pub fn enter_does(&self) -> &'static str {
        if self.answers.is_some() {
            "enter to answer"
        } else if self.session.is_some() {
            "enter to open its session"
        } else {
            "enter to open its workflow"
        }
    }

    fn elapsed(&self, now: Instant) -> Duration {
        let since = self
            .measured_at
            .map_or(Duration::ZERO, |at| now.saturating_duration_since(at));
        Duration::from_millis(self.elapsed_ms) + since
    }

    fn spins(&self, theme: &Theme) -> bool {
        self.kind != RunRowKind::Work
            && self.state == WorkflowRowState::Running
            && !theme.reduced_motion
            && !theme.glyphs.is_ascii()
    }
}

/// Each state has its own mark, so none is told apart by colour alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowRowState {
    /// At work: ⣾, turning.
    Running,
    /// At work on a fallback model while its own is out: ≈.
    Fallback,
    /// Something waits on you: ◆.
    NeedsYou,
    /// It waits out an outage or a reset, and tries again on its own: ◌ in amber.
    Waiting,
    /// It hasn't started: a task waiting on another, or for its turn. A dim ·, an empty slot.
    Queued,
    /// It stops at its next safe point: ¦.
    Pausing,
    /// You paused it: ‖.
    Paused,
    /// The pull request it opened is followed, and waits on its checks: ◐.
    PullRequest,
    Done,
    Failed,
    Stopped,
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
    /// in order, each followed by the rows under it and then its agents while they show. Agents
    /// of a run without a row come last.
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

    /// When the rows next look different: a time's next second, a mark's next frame, an agent's
    /// next change, or a row due to leave.
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
        let turning = self
            .rows
            .iter()
            .any(|row| row.spins(theme))
            .then(|| clock.next_frame_at(SPIN));
        let agents = self
            .show_agents
            .then(|| agents.next_change(clock, theme))
            .flatten();
        let leaving = self.rows.iter().filter_map(|row| row.leaves_at);
        ticking.chain(turning).chain(agents).chain(leaving).min()
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
    let columns = Columns::of(&workflows.rows, ctx);
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
    for at in visible.clone() {
        match entries[at] {
            Entry::Run(i) => {
                // Only the first row of a group hangs on ⎿, as Claude Code draws it.
                let row = &workflows.rows[i];
                let above = at
                    .checked_sub(1)
                    .and_then(|before| entries[before].run())
                    .map(|before| workflows.rows[before].depth);
                let first = row.depth > 0 && above.is_some_and(|above| above < row.depth);
                lines.push(run_line(row, first, run_selected == Some(i), &columns, ctx));
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
        // Enter opens what the row is: a session, or the run's view on the row.
        KeyCode::Enter | KeyCode::Right => match entries[selected] {
            Entry::Run(i) => {
                let row = &workflows.rows[i];
                if row.answers.is_some() {
                    PanelEvent::OpenWorkflow(row.key.clone())
                } else if let Some(session) = &row.session {
                    PanelEvent::Open(session.clone())
                } else {
                    PanelEvent::OpenWorkflow(row.opens.clone().unwrap_or(row.key.clone()))
                }
            }
            Entry::Agent(j) => {
                let event = agents.rows[j].open_event();
                if event == PanelEvent::Leave {
                    workflows.blur();
                }
                event
            }
        },
        // A request's row stops its run, a row that is one session stops that session, and a
        // task done by several stops from its view.
        KeyCode::Char('x') if plain => match entries[selected] {
            Entry::Run(i) => {
                let row = &workflows.rows[i];
                match (&row.session, row.kind) {
                    (_, RunRowKind::Request) => PanelEvent::StopRun(row.key.clone()),
                    (Some(session), _) => PanelEvent::Stop {
                        agent: session.clone(),
                        name: row.name.clone(),
                    },
                    (None, _) => PanelEvent::Unhandled,
                }
            }
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
/// selected never pauses it. `selected` is the row the `x` was pressed on, which a task's row
/// that stops its session is too.
pub fn ask_before_stop(
    asked: &mut Option<StopAsk>,
    event: PanelEvent,
    selected: Option<PanelRowId>,
) -> PanelEvent {
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
    let row = selected.unwrap_or(row);
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

/// Where every field of the list starts: one layout for every row, so each field lines up down
/// the list. The count, time and token columns are as wide as the widest value any row has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Columns {
    /// Whether the way column shows.
    way: bool,
    /// The bar's cells, none once the row is too narrow for its least.
    bar: usize,
    /// Whether `tokens` follows the number.
    word: bool,
    /// What the name column gives up on a row too narrow for everything else.
    name_cut: usize,
    count: usize,
    time: usize,
    number: usize,
    /// The arrow and its gap, `↓  `.
    arrow: usize,
}

impl Columns {
    fn of(rows: &[WorkflowRow], ctx: &RenderCtx<'_>) -> Self {
        let widest = |cells: &dyn Fn(&WorkflowRow) -> Option<String>| {
            rows.iter()
                .filter_map(cells)
                .map(|text| str_width(&text))
                .max()
                .unwrap_or(0)
        };
        let arrow = ctx.theme.glyphs.tokens;
        let mut columns = Self {
            way: usize::from(ctx.width) >= WAY_FROM,
            bar: 0,
            word: true,
            name_cut: 0,
            count: widest(&|row| row.count.map(count_text)),
            time: widest(&|row| time_text(row, ctx)),
            number: widest(&|row| tokens_text(row)),
            arrow: str_width(arrow) + str_width(gap_after(arrow)),
        };
        columns.fit(usize::from(ctx.width));
        columns
    }

    /// Below the full row the bar gives way first, down to its least; then the word `tokens`
    /// goes and the bar takes back what it can; then the bar goes and the name column gives up
    /// cells. The last column stays blank.
    fn fit(&mut self, width: usize) {
        let room = width.saturating_sub(1);
        for word in [true, false] {
            self.word = word;
            let left = room.saturating_sub(self.fixed() + GAP);
            if left >= MIN_BAR_CELLS {
                self.bar = left.min(BAR_CELLS);
                return;
            }
        }
        self.bar = 0;
        self.name_cut = self
            .fixed()
            .saturating_sub(room)
            .min(NAME_CELLS - MIN_NAME_CELLS);
    }

    /// Every cell of the row but the bar's and the gap after it.
    fn fixed(&self) -> usize {
        self.bar_at() + self.stats()
    }

    fn tokens(&self) -> usize {
        match self.number {
            0 => 0,
            number => self.arrow + number + if self.word { " tokens".len() } else { 0 },
        }
    }

    /// The count, the time and the tokens, with a ` · ` between each two that have a column.
    fn stats(&self) -> usize {
        let fields = [self.count, self.time, self.tokens()];
        let shown = fields.iter().filter(|cells| **cells > 0).count();
        fields.iter().sum::<usize>() + SEP_CELLS * shown.saturating_sub(1)
    }

    fn name_end(&self) -> usize {
        INSET + MARK_CELLS + NAME_CELLS - self.name_cut
    }

    fn way_at(&self) -> usize {
        self.name_end() + GAP
    }

    fn bar_at(&self) -> usize {
        self.way_at() + if self.way { WAY_CELLS } else { 0 }
    }

    fn count_at(&self) -> usize {
        self.bar_at() + if self.bar > 0 { self.bar + GAP } else { 0 }
    }

    fn time_at(&self) -> usize {
        let sep = if self.count > 0 && self.time > 0 {
            SEP_CELLS
        } else {
            0
        };
        self.count_at() + self.count + sep
    }

    fn tokens_at(&self) -> usize {
        let sep = if self.count + self.time > 0 && self.number > 0 {
            SEP_CELLS
        } else {
            0
        };
        self.time_at() + self.time + sep
    }
}

fn count_text((done, total): (u32, u32)) -> String {
    format!("{done}/{total}")
}

fn time_text(row: &WorkflowRow, ctx: &RenderCtx<'_>) -> Option<String> {
    row.timed
        .then(|| format::duration_slot(row.elapsed(ctx.clock.now)))
}

fn tokens_text(row: &WorkflowRow) -> Option<String> {
    (row.tokens > 0).then(|| format::tokens_in_tenths(row.tokens))
}

/// A line built left to right, each piece put at its column.
#[derive(Default)]
struct Cells {
    spans: Vec<Span<'static>>,
    at: usize,
}

impl Cells {
    /// `text` from `column`, after blanks up to it.
    fn put(&mut self, column: usize, text: impl Into<String>, style: Style) {
        self.pad_to(column);
        self.push(Span::styled(text.into(), style));
    }

    /// `text` ending in the cell before `end`.
    fn put_right(&mut self, end: usize, text: String, style: Style) {
        self.put(end.saturating_sub(str_width(&text)), text, style);
    }

    /// Blanks up to `column`.
    fn pad_to(&mut self, column: usize) {
        if column > self.at {
            self.spans.push(Span::raw(" ".repeat(column - self.at)));
            self.at = column;
        }
    }

    fn push(&mut self, span: Span<'static>) {
        self.at += str_width(&span.content);
        self.spans.push(span);
    }
}

fn run_line(
    row: &WorkflowRow,
    first: bool,
    selected: bool,
    columns: &Columns,
    ctx: &RenderCtx<'_>,
) -> Line<'static> {
    let theme = ctx.theme;
    let dim = theme.style(Role::Dim);
    let mut line = Cells::default();
    if selected {
        line.put(0, theme.glyphs.prompt, theme.style(Role::Question));
    }
    let mark_at = INSET + STEP_IN * row.depth;
    if first {
        line.put(mark_at - 2, theme.glyphs.result, dim);
    }
    let (mark, role) = mark(row, ctx);
    line.put(mark_at, mark, theme.style(role));

    // The name is dim unless the row is the one picked, so the filled bar is what stands out.
    // A row that leaves the way column blank, as everything under a task does, runs its name
    // on into it where the row is so deep in the tree that its name would get under NAME_FLOOR
    // cells, or where it names a model too long for its own column, such as a seat's whole
    // name where no logo draws; where that's still short, the logo gives way before the name
    // does.
    let name_at = mark_at + MARK_CELLS;
    let logo_cells = usize::from(LOGO_WIDTH) + 1;
    let short = |end: usize| end.saturating_sub(name_at) < logo_cells + NAME_FLOOR;
    let name = sanitize_line(&row.name).trim().to_owned();
    let named = |logo: Option<&Logo>| match logo.filter(|_| row.model_named) {
        Some(logo) => ModelLabel::named(logo, &name, theme, NoPicture::Dot).name,
        None => name.clone(),
    };
    let wanted = crate::base::text::str_width(&named(row.logo.as_ref()))
        + row.logo.as_ref().map_or(0, |_| logo_cells);
    let long = |end: usize| row.model_named && wanted > end.saturating_sub(name_at);
    let name_end = match row.way {
        None if columns.way && (short(columns.name_end()) || long(columns.name_end())) => {
            columns.bar_at().saturating_sub(GAP)
        }
        _ => columns.name_end(),
    };
    let mut room = name_end.saturating_sub(name_at);
    line.pad_to(name_at);
    let logo = row
        .logo
        .as_ref()
        .filter(|_| room >= logo_cells + NAME_FLOOR);
    if let Some(logo) = logo {
        let badge = match row.number {
            Some(number) => {
                let style = theme.style(Role::for_cli(&logo.agent.0));
                numbered_spans(logo, number, style, theme)
            }
            None => logo_spans(logo, theme),
        };
        for span in badge {
            line.push(span);
        }
        line.push(Span::raw(" "));
        room -= logo_cells;
    }
    let name_style = theme.style(if selected { Role::Text } else { Role::Dim });
    line.push(Span::styled(cut(&named(logo), room, ctx), name_style));

    if let Some(way) = row.way.as_deref().filter(|_| columns.way) {
        let way = sanitize_line(way);
        line.put(columns.way_at(), cut(way.trim(), WAY_CELLS - 1, ctx), dim);
    }

    match row.count {
        Some(count) => {
            if columns.bar > 0 {
                line.pad_to(columns.bar_at());
                for span in bar(Some(count), columns.bar, row.near_limit, theme) {
                    line.push(span);
                }
            }
            line.put_right(columns.count_at() + columns.count, count_text(count), dim);
        }
        None => {
            // What it is doing takes the bar's cells and the count's, in the text colour.
            if let Some(doing) = row.doing.as_deref() {
                let doing = sanitize_line(doing);
                let mut at = columns.bar_at();
                let mut room = columns.count_at() + columns.count - at;
                // Near its limit with no bar to dash, the words start with the dashed track.
                if row.near_limit && room > 3 {
                    let track = super::bar::near_track(theme).repeat(2);
                    line.put(at, track, theme.style(Role::Warn));
                    at += 3;
                    room -= 3;
                }
                let doing = cut(doing.trim(), room, ctx);
                line.put(at, doing, theme.style(Role::Text));
            }
        }
    }

    let time = time_text(row, ctx);
    if let Some(time) = &time {
        if row.count.is_some() && columns.count > 0 {
            line.put(columns.count_at() + columns.count, SEP, dim);
        }
        line.put_right(columns.time_at() + columns.time, time.clone(), dim);
    }
    if let Some(number) = tokens_text(row) {
        if time.is_some() {
            line.put(columns.time_at() + columns.time, SEP, dim);
        }
        let arrow = theme.glyphs.tokens;
        let pad = columns.number.saturating_sub(str_width(&number));
        let word = if columns.word { " tokens" } else { "" };
        let tokens = format!(
            "{arrow}{}{}{number}{word}",
            gap_after(arrow),
            " ".repeat(pad)
        );
        line.put(columns.tokens_at(), tokens, dim);
    }
    truncate_line(&Line::from(line.spans), usize::from(ctx.width))
}

/// The row's mark and its colour. Requests and tasks take the run marks whatever their way;
/// what does a task's work takes the agents' marks.
fn mark(row: &WorkflowRow, ctx: &RenderCtx<'_>) -> (&'static str, Role) {
    let ascii = ctx.theme.glyphs.is_ascii();
    let pick = |unicode: &'static str, plain: &'static str| if ascii { plain } else { unicode };
    use WorkflowRowState as State;
    if row.kind == RunRowKind::Work {
        return match row.state {
            State::Done => (pick("✔", "*"), Role::StateDone),
            State::Failed | State::Stopped => (pick("×", "x"), Role::StateFailed),
            State::NeedsYou => (pick("◆", "!"), Role::StateNeedsYou),
            _ => (pick("○", "o"), Role::Dim),
        };
    }
    match row.state {
        State::Running if row.spins(ctx.theme) => {
            let frame = ctx.clock.frame(SPIN, SPINNER.len());
            (SPINNER[frame], Role::StateWorking)
        }
        State::Running => (pick(SPINNER[0], "o"), Role::StateWorking),
        State::Fallback => (pick("≈", "~"), Role::Warn),
        State::NeedsYou => (pick("◆", "!"), Role::StateNeedsYou),
        State::Waiting => (pick("◌", "."), Role::Warn),
        State::Queued => (pick("·", "_"), Role::Dim),
        State::Pausing => (pick("¦", ":"), Role::Dim),
        State::Paused => (pick("‖", "="), Role::Text),
        State::PullRequest => (pick("◐", "%"), Role::Dim),
        State::Done => (pick("✔", "*"), Role::StateDone),
        State::Failed | State::Stopped => (pick("×", "x"), Role::StateFailed),
    }
}

/// `text` in `room` cells, cut with an ellipsis where it's longer. Fonts such as Iosevka draw
/// `…` two cells wide, so the cell after it is left blank.
fn cut(text: &str, room: usize, ctx: &RenderCtx<'_>) -> String {
    if str_width(text) <= room {
        return text.to_owned();
    }
    let ellipsis = ctx.theme.glyphs.ellipsis;
    let spill = usize::from(str_width(ellipsis) == 1);
    let keep = room.saturating_sub(str_width(ellipsis) + spill);
    let mut kept = String::new();
    let mut cells = 0;
    for ch in text.chars() {
        let mut buf = [0; 4];
        let width = str_width(ch.encode_utf8(&mut buf));
        if cells + width > keep {
            break;
        }
        kept.push(ch);
        cells += width;
    }
    if kept.trim().is_empty() {
        return String::new();
    }
    format!("{}{ellipsis}", kept.trim_end())
}

/// A run's tokens with one decimal in the thousands whatever their size, `538.7k`, as its
/// workflow view keeps them, and none when it is `.0`: `128k`. Millions as every count has them.
#[cfg(test)]
mod tests;
