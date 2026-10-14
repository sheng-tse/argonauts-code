//! The Status tab's cards: where the run stands, with its tasks by state,
//! its goal and plan, and each task with who has it; what the run spent; and what needs
//! you. All of it comes from the summary `/status` always answered with, so it costs nothing.
//!
//! The same cards pin above the input, where rows are dear: the run without its goal and
//! plan and with its first few tasks, and each card folded to one line.

use argonauts_core::api::{StatusSummary, TaskBrief};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::agent_status::RunSpend;
use argonauts_core::events::task::TaskState;
use argonauts_core::settings::limit::{LimitAmount, LimitLineKind, LimitUnit};
use argonauts_core::settings::ui::ModName;
use ratatui::text::Span;

use crate::format;
use crate::kit::bars::Stack;
use crate::kit::{
    Badge, Drawn, Form, Frame, Header, Kit, Level, MeterRow, MeterSlots, State, States, bodies,
    ljust,
};
use crate::text::{sanitize_line, str_width, truncate_spans};
use crate::theme::Role;

/// From this share of a limit on, the spend shows amber.
const NEAR_LIMIT: f64 = 0.9;
/// The most tasks the run card lists before it counts the rest.
const TASK_ROWS: usize = 12;
/// The most tasks it lists above the input.
const PINNED_TASK_ROWS: usize = 5;
/// The most things that need you the card lists above the input.
const PINNED_WAITING_ROWS: usize = 3;
/// The states a run's tasks are counted in, in the order the bar stacks them.
const STATES: [State; 6] = [
    State::Done,
    State::Working,
    State::Waiting,
    State::NeedsYou,
    State::Failed,
    State::Stopped,
];
/// How many lines of what happened last the tab keeps.
const RECENT: usize = 2;

/// A card of the run's that pins above the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusCard {
    Run,
    Spend,
    NeedsYou,
}

impl StatusCard {
    /// The card a mod's name draws, for the names these cards answer to.
    pub fn of(name: ModName) -> Option<Self> {
        match name {
            ModName::Run => Some(StatusCard::Run),
            ModName::RunLimits => Some(StatusCard::Spend),
            ModName::Waiting => Some(StatusCard::NeedsYou),
            _ => None,
        }
    }

    pub fn name(self) -> ModName {
        match self {
            StatusCard::Run => ModName::Run,
            StatusCard::Spend => ModName::RunLimits,
            StatusCard::NeedsYou => ModName::Waiting,
        }
    }
}

/// How much of the run its card shows.
#[derive(Debug, Clone, Copy)]
pub struct RunShape<'a> {
    /// The goal and the plan, a row each.
    pub said: bool,
    /// The most tasks it lists before it counts the rest.
    pub tasks: usize,
    /// On the tab, where the keys pick a task.
    pub picks: Option<Picks<'a>>,
}

/// The task the keys are on. The rows keep a column for the mark whether or not the card
/// has the keys, so they never shift as Tab moves on.
#[derive(Debug, Clone, Copy)]
pub struct Picks<'a> {
    pub at: Option<usize>,
    pub mark: &'a str,
}

impl RunShape<'static> {
    /// The whole card, with no task picked.
    pub const WHOLE: Self = Self {
        said: true,
        tasks: TASK_ROWS,
        picks: None,
    };
}

/// What the cards draw from.
#[derive(Debug, Clone, Copy)]
pub struct StatusCards<'a> {
    pub summary: &'a StatusSummary,
    pub names: &'a DisplayNames,
    /// The run's own name, such as `export-reports-csv`, where the screen knows it.
    pub title: Option<&'a str>,
}

impl StatusCards<'_> {
    /// Nothing runs and nothing is planned, so there is no card to draw.
    pub fn is_empty(&self) -> bool {
        let summary = self.summary;
        summary.tasks.is_empty()
            && summary.goal.is_empty()
            && summary.plan.is_none()
            && summary.spend.is_none()
            && summary.waiting.is_empty()
    }

    /// Whether `card` has anything to show for this run.
    pub fn has(&self, card: StatusCard) -> bool {
        match card {
            StatusCard::Run => self.summary.run.is_some() && !self.is_empty(),
            StatusCard::Spend => self.summary.spend.is_some(),
            StatusCard::NeedsYou => !self.summary.waiting.is_empty(),
        }
    }

    /// `card` as it pins above the input, in its full or its compact form.
    pub fn pinned(&self, kit: &Kit<'_>, card: StatusCard, form: Form, frame: Frame) -> Drawn {
        let full = form == Form::Full;
        let drawn = match card {
            StatusCard::Run => {
                let shape = RunShape {
                    said: false,
                    tasks: if full { PINNED_TASK_ROWS } else { 0 },
                    picks: None,
                };
                Some(self.run(kit, frame, shape))
            }
            StatusCard::Spend => self.spend_card(kit, frame, full, true),
            StatusCard::NeedsYou => {
                let rows = if full { PINNED_WAITING_ROWS } else { 1 };
                self.needs_you_rows(kit, frame, rows)
            }
        };
        drawn.unwrap_or_default()
    }

    /// The folded card: its mark and name, then the one number that matters.
    pub fn line(&self, kit: &Kit<'_>, card: StatusCard, width: usize) -> Vec<Span<'static>> {
        let header = match card {
            StatusCard::Run => self.run_header(kit, &Counts::of(&self.summary.tasks)),
            StatusCard::Spend => self.spend_header(true),
            StatusCard::NeedsYou => Header::new("needs you", Role::StateNeedsYou),
        };
        let mut row = header.left(kit);
        row.push(Span::raw("  "));
        match (card, self.summary.waiting.first()) {
            // The name already says they need you, so the line says how many and the first.
            (StatusCard::NeedsYou, Some(first)) => {
                let count = self.summary.waiting.len().to_string();
                row.push(kit.span(count, Level::Warn.number_role()));
                row.push(kit.dim(format!(" · {}", clean(first))));
            }
            _ => row.extend(self.key_number(kit, card)),
        }
        truncate_spans(&row, width)
    }

    /// What a hidden band still says of the card, in its level's color: how far the run
    /// is, its spend against its limit, how many things wait for you.
    pub fn key_number(&self, kit: &Kit<'_>, card: StatusCard) -> Vec<Span<'static>> {
        let summary = self.summary;
        match card {
            StatusCard::Run => {
                let counts = Counts::of(&summary.tasks);
                let stands = counts.badge(!summary.waiting.is_empty());
                let mut words = vec![kit.dim(format!(
                    "{} of {} merged",
                    counts.count(State::Done),
                    counts.planned()
                ))];
                // The hidden band parts its cards with dots, so a card's own words have none.
                if let Some(badge) = stands {
                    words.push(kit.dim(" ("));
                    words.push(kit.span(badge.text, badge.level.number_role()));
                    words.push(kit.dim(")"));
                }
                words
            }
            StatusCard::Spend => {
                let Some(spend) = &summary.spend else {
                    return Vec::new();
                };
                let spent = format::spend(&spend.spent);
                match spend.limit.filter(|limit| limit.amount > 0.0) {
                    Some(limit) => {
                        let share = used(spend, limit.unit) / limit.amount;
                        vec![kit.span(
                            format!("{spent} of {} ({:.0}%)", amount(&limit), share * 100.0),
                            level(share).number_role(),
                        )]
                    }
                    None => vec![kit.dim(format!("{spent}, no limit set"))],
                }
            }
            StatusCard::NeedsYou => {
                let words = match summary.waiting.len() {
                    1 => "1 needs you".to_owned(),
                    n => format!("{n} need you"),
                };
                vec![kit.span(words, Level::Warn.number_role())]
            }
        }
    }

    /// The run: how many of its tasks are merged, a bar of them by state, the goal and the
    /// plan, then a row for each task, as much of it as `shape` asks for.
    pub fn run(&self, kit: &Kit<'_>, frame: Frame, shape: RunShape<'_>) -> Drawn {
        let inner = frame.inner();
        let tasks = &self.summary.tasks;
        let counts = Counts::of(tasks);
        let mut body = Drawn::new();
        body.push(self.run_header(kit, &counts).draw(kit, inner));
        if !tasks.is_empty() {
            body.append(bodies::stack(kit, &counts.stack(), &[], inner, 1, 1));
            body.push(truncate_spans(&counts.legend(kit), inner));
        }
        let said = if shape.said {
            self.goal_and_plan(kit, inner)
        } else {
            Vec::new()
        };
        if !said.is_empty() {
            body.push(Vec::new());
            said.into_iter().for_each(|row| body.push(row));
        }
        if !tasks.is_empty() && shape.tasks > 0 {
            // Above the input a blank row is a row of the conversation lost.
            if shape.said {
                body.push(Vec::new());
            }
            body.append(self.task_rows(kit, inner, shape));
        }
        frame.draw(kit, body)
    }

    fn run_header(&self, kit: &Kit<'_>, counts: &Counts) -> Header {
        let summary = self.summary;
        let title = summary
            .run
            .as_ref()
            .map_or_else(|| "no run".to_owned(), ToString::to_string);
        let mut header = Header {
            subject: self.title.map(clean),
            ..Header::new(title, Role::Question)
        };
        if counts.planned() > 0 {
            header.amount = Some(counts.count(State::Done).to_string());
            header.tail = vec![kit.dim(format!(" of {} merged", counts.planned()))];
        }
        header.badge = counts.badge(!summary.waiting.is_empty());
        header
    }

    fn goal_and_plan(&self, kit: &Kit<'_>, width: usize) -> Vec<Vec<Span<'static>>> {
        let summary = self.summary;
        let row = |label: &str, text: &str| {
            truncate_spans(
                &[kit.dim(format!("{label}  ")), Span::raw(clean(text))],
                width,
            )
        };
        let mut rows = Vec::new();
        if let Some(goal) = summary.goal.first() {
            rows.push(row("Goal", goal));
        }
        if let Some(plan) = summary.plan.as_deref().and_then(|plan| plan.lines().next()) {
            rows.push(row("Plan", plan));
        }
        rows
    }

    /// A row a task: its mark, its id, its title, who has it, and how it stands. The title
    /// gives way first on a narrow card, and under 70 cells nobody is named. More tasks than
    /// the card lists scroll under the picked one.
    fn task_rows(&self, kit: &Kit<'_>, width: usize, shape: RunShape<'_>) -> Drawn {
        let tasks = &self.summary.tasks;
        let picked = shape.picks.and_then(|picks| picks.at);
        let first = picked.map_or(0, |at| (at + 1).saturating_sub(shape.tasks));
        let last = (first + shape.tasks).min(tasks.len());
        let shown = &tasks[first.min(last)..last];
        let worker = |task: &TaskBrief| {
            task.worker
                .as_ref()
                .map_or_else(String::new, |worker| clean(&self.names.worker(worker)))
        };
        let column = |widest: usize| if widest == 0 { 0 } else { widest + 2 };
        let pick_w = if shape.picks.is_some() { 2 } else { 0 };
        let width = width.saturating_sub(pick_w);
        let id_w = column(widest(shown, |task| task.task.to_string()));
        let worker_w = if width >= 70 {
            column(widest(shown, worker))
        } else {
            0
        };
        let fixed = 2 + id_w + worker_w;
        // The standing keeps a third of the row at least.
        let title_w = column(widest(shown, |task| clean(&task.title)))
            .min(width.saturating_sub(fixed + width / 3))
            .max(8);
        let states = States::default();
        let mut body = Drawn::new();
        for (at, task) in shown.iter().enumerate() {
            let on = picked == Some(first + at);
            let state = task_state(task.state);
            let title = truncate_spans(&[Span::raw(clean(&task.title))], title_w - 2);
            let title: String = title.iter().map(|span| span.content.as_ref()).collect();
            let mut row = Vec::new();
            if let Some(picks) = shape.picks {
                row.push(if on {
                    kit.span(format!("{} ", picks.mark), Role::Question)
                } else {
                    Span::raw("  ")
                });
            }
            row.extend([
                states.span(kit, state),
                Span::raw(" "),
                kit.dim(ljust(task.task.as_ref(), id_w)),
            ]);
            row.push(if on {
                kit.strong(ljust(&title, title_w))
            } else {
                Span::raw(ljust(&title, title_w))
            });
            if worker_w > 0 {
                row.push(kit.dim(ljust(&worker(task), worker_w)));
            }
            let standing = standing(task);
            // What waits for you or failed must stand out from the rows that just go on.
            row.push(match state {
                State::NeedsYou | State::Failed => kit.span(standing, state.role()),
                _ => kit.dim(standing),
            });
            body.push(truncate_spans(&row, width + pick_w));
        }
        if tasks.len() > shown.len() {
            let rest = match (picked, shape.picks) {
                (Some(at), _) => format!("{} of {} · ↑↓ shows the rest", at + 1, tasks.len()),
                (None, Some(_)) => format!("and {} more", tasks.len() - shown.len()),
                (None, None) => format!(
                    "and {} more · enter opens the workflow",
                    tasks.len() - shown.len()
                ),
            };
            body.push(vec![kit.dim(format!("{}{rest}", " ".repeat(2 + pick_w)))]);
        }
        body
    }

    /// What the run spent, as a meter against its limit when you set one. With no limit it
    /// says so, since nothing stops the run then.
    pub fn spend(&self, kit: &Kit<'_>, frame: Frame) -> Option<Drawn> {
        self.spend_card(kit, frame, true, false)
    }

    fn spend_header(&self, names_run: bool) -> Header {
        // Beside the run's own card it needs no name. Above the input it does, since the
        // period's spend can sit there too.
        let subject = self.summary.run.as_ref().filter(|_| names_run);
        Header {
            subject: subject.map(ToString::to_string),
            ..Header::new("spend", Role::Success)
        }
    }

    /// The compact form keeps the limit's own meter and leaves out its lines.
    fn spend_card(
        &self,
        kit: &Kit<'_>,
        frame: Frame,
        full: bool,
        names_run: bool,
    ) -> Option<Drawn> {
        let spend = self.summary.spend.as_ref()?;
        let inner = frame.inner();
        let mut header = Header {
            amount: Some(format::spend(&spend.spent)),
            ..self.spend_header(names_run)
        };
        let mut rows = Vec::new();
        match spend.limit.filter(|limit| limit.amount > 0.0) {
            Some(limit) => {
                let share = used(spend, limit.unit) / limit.amount;
                header.tail = vec![kit.dim(format!(" of {}", amount(&limit)))];
                header.badge = Some(Badge::percent(share, level(share)));
                rows.push(meter(unit_word(limit.unit), share));
                let allocated = spend
                    .lines
                    .iter()
                    .filter(|line| full && line.allocated.amount > 0.0);
                rows.extend(
                    allocated
                        .map(|line| meter(line_word(line.line), line.used / line.allocated.amount)),
                );
            }
            None => {
                header.tail = vec![kit.dim(" · no limit set")];
                rows.push(MeterRow::Text {
                    label: "tokens".to_owned(),
                    words: vec![kit.strong(format::tokens(spend.spent.total_tokens()))],
                });
                if full {
                    rows.push(MeterRow::Text {
                        label: "limit".to_owned(),
                        words: vec![kit.dim("none, so nothing stops this run · /limits sets one")],
                    });
                }
            }
        }
        let mut body = Drawn::new();
        body.push(header.draw(kit, inner));
        body.append(bodies::meters(kit, &rows, inner, MeterSlots::default()));
        Some(frame.draw(kit, body))
    }

    /// What waits for you, a row each, with how many in the badge.
    pub fn needs_you(&self, kit: &Kit<'_>, frame: Frame) -> Option<Drawn> {
        self.needs_you_rows(kit, frame, usize::MAX)
    }

    /// The first `most` of them, and a count of the rest.
    fn needs_you_rows(&self, kit: &Kit<'_>, frame: Frame, most: usize) -> Option<Drawn> {
        let waiting = &self.summary.waiting;
        if waiting.is_empty() {
            return None;
        }
        let inner = frame.inner();
        let header = Header {
            badge: Some(Badge::words(waiting.len().to_string(), Level::Warn)),
            ..Header::new("needs you", Role::StateNeedsYou)
        };
        let mut body = Drawn::new();
        body.push(header.draw(kit, inner));
        let states = States::default();
        for what in waiting.iter().take(most) {
            let row = [
                states.span(kit, State::NeedsYou),
                Span::raw(format!(" {}", clean(what))),
            ];
            body.push(truncate_spans(&row, inner));
        }
        if waiting.len() > most {
            body.push(vec![
                kit.dim(format!("  and {} more", waiting.len() - most)),
            ]);
        }
        Some(frame.draw(kit, body))
    }

    /// What happened last, as one dim row under the cards.
    pub fn recent(&self, kit: &Kit<'_>, width: usize) -> Option<Drawn> {
        let recent = &self.summary.recent;
        if recent.is_empty() {
            return None;
        }
        let last: Vec<String> = recent
            .iter()
            .skip(recent.len().saturating_sub(RECENT))
            .map(|line| clean(line))
            .collect();
        let row = [kit.dim(format!(" Recent  {}", last.join(" · ")))];
        let mut block = Drawn::new();
        block.push(truncate_spans(&row, width));
        Some(block)
    }
}

/// A line of the run's own words, safe to draw in one row.
fn clean(text: &str) -> String {
    sanitize_line(text).trim().to_owned()
}

fn widest(tasks: &[TaskBrief], cell: impl Fn(&TaskBrief) -> String) -> usize {
    tasks
        .iter()
        .map(|task| str_width(&cell(task)))
        .max()
        .unwrap_or(0)
}

/// How many of a run's tasks are in each state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Counts([u64; STATES.len()]);

impl Counts {
    fn of(tasks: &[TaskBrief]) -> Self {
        let mut counts = Self::default();
        for task in tasks {
            let state = task_state(task.state);
            let at = STATES.iter().position(|known| *known == state);
            counts.0[at.unwrap_or(STATES.len() - 1)] += 1;
        }
        counts
    }

    fn count(&self, state: State) -> u64 {
        STATES
            .iter()
            .position(|known| *known == state)
            .map_or(0, |at| self.0[at])
    }

    /// The tasks the run still means to do or did: all but the stopped ones.
    fn planned(&self) -> u64 {
        self.0.iter().sum::<u64>() - self.count(State::Stopped)
    }

    fn stack(&self) -> Stack {
        Stack {
            parts: STATES
                .iter()
                .zip(self.0)
                .map(|(state, count)| (count, state.role()))
                .collect(),
            total: self.0.iter().sum::<u64>().max(1),
            tick: None,
            held_from: None,
            track: Role::BarTrack,
        }
    }

    /// `▌ merged 1   ▌ running 1`, each state that has a task, in the bar's order.
    fn legend(&self, kit: &Kit<'_>) -> Vec<Span<'static>> {
        let mut row = Vec::new();
        for (state, count) in STATES.iter().zip(self.0).filter(|(_, count)| *count > 0) {
            if !row.is_empty() {
                row.push(Span::raw("   "));
            }
            row.push(kit.span(kit.glyphs.swatch, state.role()));
            row.push(kit.dim(format!(" {} ", state_word(*state))));
            row.push(kit.strong(count.to_string()));
        }
        row
    }

    /// The run in a word, from its tasks. A run with no tasks yet has none.
    fn badge(&self, waits_on_you: bool) -> Option<Badge> {
        if waits_on_you || self.count(State::NeedsYou) > 0 {
            Some(Badge::words("needs you", Level::Warn))
        } else if self.count(State::Working) > 0 {
            Some(Badge::words("running", Level::Ok))
        } else if self.count(State::Failed) > 0 {
            Some(Badge::words("failed", Level::Danger))
        } else if self.planned() == 0 {
            None
        } else if self.count(State::Done) == self.planned() {
            Some(Badge::words("merged", Level::Ok))
        } else {
            Some(Badge::words("waiting", Level::Ok))
        }
    }
}

/// A task's state as one of the kit's seven, which pick its mark and its color.
pub(super) fn task_state(state: TaskState) -> State {
    match state {
        TaskState::Merged => State::Done,
        TaskState::Running | TaskState::Checking | TaskState::InReview | TaskState::Merging => {
            State::Working
        }
        TaskState::Waiting | TaskState::Ready | TaskState::Deferred => State::Waiting,
        TaskState::Blocked | TaskState::NeedsDecision | TaskState::NeedsYou => State::NeedsYou,
        TaskState::Failed => State::Failed,
        TaskState::Cancelled | TaskState::TakenOver => State::Stopped,
    }
}

/// A task that is done is merged into the run, so the legend says merged.
fn state_word(state: State) -> &'static str {
    match state {
        State::Done => "merged",
        other => other.word(),
    }
}

/// How a task stands, in the run's own words: its step, then its note.
fn standing(task: &TaskBrief) -> String {
    let word = match task.state {
        TaskState::Waiting => "waiting",
        TaskState::Ready => "ready",
        TaskState::Running => "running",
        TaskState::Blocked => "asks you",
        TaskState::NeedsDecision | TaskState::NeedsYou => "needs you",
        TaskState::Checking => "checks",
        TaskState::InReview => "in review",
        TaskState::Merging => "merging",
        TaskState::Merged => "merged",
        TaskState::Failed => "failed",
        TaskState::Deferred => "waiting for a limit",
        TaskState::Cancelled => "cancelled",
        TaskState::TakenOver => "taken over",
    };
    match clean(&task.note) {
        note if note.is_empty() => word.to_owned(),
        note => format!("{word} · {note}"),
    }
}

fn level(share: f64) -> Level {
    if share >= 1.0 {
        Level::Danger
    } else if share >= NEAR_LIMIT {
        Level::Warn
    } else {
        Level::Ok
    }
}

fn meter(label: &str, share: f64) -> MeterRow {
    MeterRow::Meter {
        label: label.to_owned(),
        ratio: share,
        level: level(share),
        unit: String::new(),
        note: String::new(),
        tick: None,
    }
}

/// What the run used so far, in the unit its limit is set in.
fn used(limit: &RunSpend, unit: LimitUnit) -> f64 {
    match unit {
        LimitUnit::Usd => limit.spent.cost_usd,
        LimitUnit::Tokens => limit.spent.total_tokens() as f64,
        // Plan usage and minutes are counted by the limit's lines, not by the spend.
        LimitUnit::PlanUsage | LimitUnit::Minutes => limit.lines.iter().map(|line| line.used).sum(),
    }
}

fn amount(limit: &LimitAmount) -> String {
    match limit.unit {
        LimitUnit::Usd => format::usd(limit.amount),
        LimitUnit::Tokens => format!("{} tokens", format::tokens(limit.amount.max(0.0) as u64)),
        LimitUnit::PlanUsage => format!("{:.0}% of plan usage", limit.amount * 100.0),
        LimitUnit::Minutes => format!("{:.0}m", limit.amount),
    }
}

fn unit_word(unit: LimitUnit) -> &'static str {
    match unit {
        LimitUnit::Usd => "spend",
        LimitUnit::Tokens => "tokens",
        LimitUnit::PlanUsage => "plan usage",
        LimitUnit::Minutes => "time",
    }
}

fn line_word(line: LimitLineKind) -> &'static str {
    match line {
        LimitLineKind::GoalAndPlan => "goal and plan",
        LimitLineKind::Experiments => "experiments",
        LimitLineKind::Contracts => "contracts",
        LimitLineKind::Tasks => "tasks",
        LimitLineKind::Review => "review",
        LimitLineKind::Merge => "merging",
        LimitLineKind::Reserve => "reserve",
    }
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
