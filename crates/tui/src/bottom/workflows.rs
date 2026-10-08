//! The panel under the mode line, like Claude Code's workflow rows: one row per run with a
//! status circle, its name, a progress bar, dim counts, and a notice when it needs you or runs
//! near its budget. With `ui.panel.agents`, each run's agents follow under its row.

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
use crate::views::states;
use crate::widgets::State;

use super::agents::{AgentRef, ListRow, RowKind, more_line, window};
use super::bar::{BAR_CELLS, MIN_BAR_CELLS, bar, run_fill};
use super::{AgentPanel, PanelEvent};

/// The name column's width, so every bar starts in one column. A longer name widens it only into
/// room the row has to spare, so one long name can't push every bar off the row. This holds
/// `council · add --json to export`.
const MAX_NAME_CELLS: usize = 37;

/// Names give up cells to keep the bar down to this, which holds a CLI and a short model.
const MIN_NAME_CELLS: usize = 20;

/// Rows under a run's row sit this much further in.
const AGENT_INDENT: usize = 2;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkflowPanel {
    pub rows: Vec<WorkflowRow>,
    /// `ui.panel.max_rows`; the last visible row reads `↓ N more` past it.
    pub max_rows: usize,
    /// `ui.panel.agents`: list each run's agents under its row.
    pub show_agents: bool,
    /// While the panel has focus, the selected row counting runs and agents alike, top down.
    pub selected: Option<usize>,
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
    /// Such as `$1.20`.
    pub spend: Option<String>,
    /// Such as `85% of budget · /workflows to stop`, drawn after `!` in the warning color.
    pub notice: Option<String>,
    /// The chat front end that started it, such as `via hermes`.
    pub front_end: Option<String>,
    /// The phase going on, such as `Fix`, after the count.
    pub phase: Option<String>,
    /// Goals its agents still work toward, for `2 goals running`.
    pub goals_running: usize,
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
            spend: None,
            notice: None,
            front_end: None,
            phase: None,
            goals_running: 0,
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

    /// Its word, as the row shows it and the run's Workflow item ends with it.
    pub fn word(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::NeedsYou => "needs you",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Paused => "paused",
            Self::Stopped => "stopped",
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
        for (i, row) in self.rows.iter().enumerate() {
            entries.push(Entry::Run(i));
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
        ticking.chain(agents).min()
    }
}

/// The panel's rows: workflow rows, and agent rows when they show. The live region draws them
/// one blank row under the mode line. Without runs it's the agent panel alone.
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
    let fill = run_fill(ctx.theme);
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
                    fill,
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
    /// Such as `x again to stop codex/gpt-5.6 · esc to keep`, on the mode line.
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
    fill: Style,
    ctx: &RenderCtx<'_>,
) -> Line<'static> {
    let theme = ctx.theme;
    // Claude Code's `○` stays for a run that's going, in the working color; the other states
    // draw as they do everywhere.
    let (circle, role) = match row.state {
        WorkflowRowState::Running => (theme.glyphs.agent_waiting, Role::StateWorking),
        WorkflowRowState::NeedsYou => states::mark(State::NeedsYou, theme),
        WorkflowRowState::Done => states::mark(State::Done, theme),
        WorkflowRowState::Failed => states::mark(State::Failed, theme),
        WorkflowRowState::Paused | WorkflowRowState::Stopped => states::mark(State::Stopped, theme),
    };
    let mut spans = if selected {
        vec![Span::styled(
            format!("{} ", theme.glyphs.prompt),
            theme.style(Role::Card),
        )]
    } else {
        vec![Span::raw("  ")]
    };
    spans.push(Span::styled(circle, theme.style(role)));
    spans.push(Span::raw(" "));
    spans.push(Span::raw(left(&cells.name, columns.name_width)));
    if columns.bar_cells > 0 {
        let steps = (row.agents_total > 0).then_some((row.agents_done, row.agents_total));
        spans.push(Span::raw("  "));
        spans.extend(bar(steps, columns.bar_cells, fill, theme));
    }
    spans.push(Span::raw("  "));
    fit_parts(spans, &cells.stats, usize::from(ctx.width))
}

/// `8/9 agents · Build · 2 goals running · 2h44m · ↓ 1.8M tokens · $1.20 · via hermes`, then
/// the state's word or the budget notice. The state and the notice give way last.
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
        push(
            format!("{}/{} agents", row.agents_done, row.agents_total),
            dim,
            4,
        );
    }
    if let Some(phase) = row
        .phase
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        push(sanitize_line(phase).into_owned(), dim, 3);
    }
    if row.goals_running > 0 {
        let noun = if row.goals_running == 1 {
            "goal"
        } else {
            "goals"
        };
        push(format!("{} {noun} running", row.goals_running), dim, 3);
    }
    if row.timed {
        push(format::duration_compact(row.elapsed(ctx.clock.now)), dim, 3);
    }
    if row.tokens > 0 {
        push(
            format!(
                "{} {} tokens",
                theme.glyphs.tokens,
                format::tokens(row.tokens)
            ),
            dim,
            1,
        );
    }
    if let Some(spend) = &row.spend {
        push(sanitize_line(spend).trim().to_owned(), dim, 2);
    }
    // Who started the run says more than its time does.
    if let Some(front_end) = &row.front_end {
        push(sanitize_line(front_end).trim().to_owned(), dim, 4);
    }
    let state = match row.state {
        WorkflowRowState::Running => None,
        WorkflowRowState::NeedsYou => {
            let (glyph, role) = states::mark(State::NeedsYou, theme);
            Some((format!("{glyph} needs you"), role))
        }
        WorkflowRowState::Done => Some(("done".to_owned(), Role::StateDone)),
        WorkflowRowState::Failed => Some(("failed".to_owned(), Role::StateFailed)),
        WorkflowRowState::Paused => Some(("paused".to_owned(), Role::StateStopped)),
        WorkflowRowState::Stopped => Some(("stopped".to_owned(), Role::StateStopped)),
    };
    if let Some((word, role)) = state {
        push(word, theme.style(role), 6);
    }
    if let Some(notice) = &row.notice {
        push(
            format!("{} {}", marks.warning, sanitize_line(notice).trim()),
            theme.style(Role::Warning),
            5,
        );
    }
    parts
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
    /// are, so a run's bar stays put when its name changes, as it does once the lead picks the
    /// workflow. A longer name takes the room past the full bar, then the bar's own down to its
    /// least, so it's cut only once the row is full. As the row narrows, the stats stay whole,
    /// the bar shrinks, then the name column gives up cells down to `names.least` to keep it,
    /// then it goes, and then names are cut further.
    fn fit(width: usize, names: Names, stats_width: usize) -> Self {
        // The mark, and the circle and its space.
        let lead = 2 + 2;
        // What the name and the bar share, after the gaps that follow each.
        let shared = width.saturating_sub(lead + 2 + 2 + stats_width);
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
        // after the lead.
        let room = width.saturating_sub(lead);
        let name_width = names
            .widest
            .min(room.saturating_sub(stats_width + 2).max(room / 2));
        Self {
            name_width,
            bar_cells: 0,
        }
    }
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
        assert!(text.ends_with("  ↓ 3 more"), "{text}");
        assert!(!text.contains("workflow-2"), "{text}");

        for expected in 1..5 {
            assert_eq!(
                handle_panel_key(&mut panel, &mut agents, key(KeyCode::Down)),
                PanelEvent::Moved
            );
            assert_eq!(panel.selected, Some(expected));
            let text = plain_text(&panel_lines(&panel, &agents, &ctx));
            assert!(text.contains(&format!("❯ ○ workflow-{expected}")), "{text}");
        }
        let text = plain_text(&panel_lines(&panel, &agents, &ctx));
        assert!(text.starts_with("  ↑ 3 more"), "{text}");
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
        assert!(rows[1].starts_with("  ○ workflow-0"), "{wide}");
        assert!(
            rows[2].starts_with("    ○ build:flag   Update(src/export.rs)")
                && rows[2].ends_with("↓ 18.1k tokens"),
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
                && agent_rows[0].contains("↓ 18.1k tokens")
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
        sub.run = Some("chat".into());
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
        assert_eq!(listed(&panel), ["● main", "○ Count", "○ workflow-0"]);
        panel.show_agents = true;
        assert_eq!(
            listed(&panel),
            ["● main", "○ Count", "○ workflow-0", "○ build:flag"]
        );
        // ↓ from the input lands on the first row past main, and typing there is a message.
        panel.focus_past_main(&agents);
        assert_eq!(
            panel.selected_id(&agents),
            Some(PanelRowId::Agent(AgentRef {
                run: Some("chat".into()),
                session: "toolu_a".into()
            }))
        );
        assert_eq!(listed(&panel)[..2], ["○ main", "● Count"]);
    }

    #[test]
    fn a_run_row_reads_like_claude_code() {
        let theme = theme();
        let t0 = Instant::now();
        let ctx = RenderCtx::new(150, &theme, Clock::new(t0, t0 + Duration::from_secs(3)));
        let mut row = WorkflowRow::new("run-0", "argonauts-ui-kit", WorkflowRowState::Running);
        row.agents_done = 8;
        row.agents_total = 9;
        row.elapsed_ms = 9_837_400;
        row.measured_at = Some(t0);
        row.tokens = 1_800_000;
        row.spend = Some("$4.10".into());
        row.notice = Some("85% of limit · /workflows to stop".into());
        let panel = WorkflowPanel {
            rows: vec![row],
            max_rows: 5,
            ..WorkflowPanel::default()
        };
        let lines = panel_lines(&panel, &AgentPanel::default(), &ctx);
        let bar = " ".repeat(BAR_CELLS);
        let pad = " ".repeat(MAX_NAME_CELLS - "argonauts-ui-kit".len());
        assert_eq!(
            plain_text(&lines),
            format!(
                "  ○ argonauts-ui-kit{pad}  {bar}  8/9 agents · 2h44m · ↓ 1.8M tokens · $4.10 · ! 85% of limit · /workflows to stop"
            )
        );
        let fill: Vec<_> = lines[0]
            .spans
            .iter()
            .filter(|s| s.style.bg.is_some())
            .collect();
        assert_eq!(fill[0].content.len(), 8 * BAR_CELLS / 9);
        assert_eq!(fill[0].style.bg, theme.style(Role::Card).fg);
        let notice = lines[0].spans.last().map(|span| span.style);
        assert_eq!(notice, Some(theme.style(Role::Warning)));
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
        run.spend = Some("$0.21".into());
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

    /// Where each row's bar starts and how many cells it takes, from its colored cells or, without
    /// color, its block glyphs.
    fn bars(lines: &[Line<'_>], theme: &Theme) -> Vec<Option<(usize, usize)>> {
        let glyphs = [theme.glyphs.bar_fill, theme.glyphs.bar_track];
        let is_bar = |span: &Span<'_>| {
            !span.content.is_empty()
                && (span.style.bg.is_some()
                    || span
                        .content
                        .chars()
                        .all(|c| glyphs.iter().any(|glyph| glyph.contains(c))))
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
            let starts: Vec<Option<usize>> = ["goal", "council · add per-key rate limiting"]
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
        for (width, whole) in [(80, false), (100, false), (120, true)] {
            let (mut panel, _, clock) = crew(false);
            panel.rows[0].name = name.into();
            let ctx = RenderCtx::new(width, &theme, clock);
            let lines = panel_lines(&panel, &AgentPanel::default(), &ctx);
            assert_lines(&lines, width);
            let text = plain_text(&lines);
            assert_eq!(text.contains(name), whole, "{width} columns\n{text}");
            assert!(text.contains("$0.21"), "{width} columns\n{text}");
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
                    assert!(rows[1].ends_with("2/5 · 4m · ↓ 18.1k tokens"), "{text}");
                    // The agents' stats start in one column.
                    let column = |row: &str, part: &str| str_width(&row[..row.find(part).unwrap()]);
                    assert_eq!(column(rows[1], "2/5"), column(rows[3], "0/5"), "{text}");
                }
            }
        }
    }
}
