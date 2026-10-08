//! The Overview panel: one place for `/status`, `/usage`, `/context` and `/limits`, with a tab
//! each and Stats beside them. It draws under the conversation, as Claude
//! Code's dialogs do, so a running answer goes on above it, and takes only the rows its cards
//! need.
//!
//! The usage tabs draw the cards of [`cards`](super::cards) from the numbers the screen
//! keeps, and say so each time they draw, which keeps the numbers coming. Status and Context
//! draw their own cards from the answers `/status` and `/context` always got, and are asked
//! for again while their tab stays open and work goes on.

use std::cell::Cell;
use std::time::{Duration, Instant};

use argonauts_core::RunId;
use argonauts_core::api::{StatusSummary, UsagePeriod, UsageSummary};
use argonauts_core::app::{AppMode, UiCommand, ViewData, ViewRequest};
use argonauts_core::input::{SlashCommand, ViewAction, WidgetChange, WidgetsAction};
use argonauts_core::settings::mods::ModPlace;
use argonauts_core::settings::ui::{Place, WidgetName};
use argonauts_core::views::ContextView as ContextData;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::text::{Line, Span};
use time::OffsetDateTime;

use super::UsageWatch;
use super::cards::{UsageCard, UsageCards, period_words};
use super::context::ContextCards;
use super::limits::{LimitsCards, tasks_of};
use super::stats::{self, StatsKey, StatsState};
use super::status::{Picks, RunShape, StatusCard, StatusCards};
use crate::bottom::PinnedRun;
use crate::bridge::transcript::NOTHING_RUNNING;
use crate::format;
use crate::text::{marks, spans_width, truncate_spans};
use crate::theme::Role;
use crate::views::dock::PANEL_HINT;
use crate::views::panes::Marks;
use crate::views::{View, ViewCtx, ViewOutcome, list};
use crate::widgets::{Drawn, Form, Frame, Kit, side_by_side, spread, stack_cards};

/// Rows above the cards: the rule, the tabs, what the tab is about, and a blank row.
const HEAD_ROWS: u16 = 4;
/// Rows under the cards: a blank row and the keys.
const FOOT_ROWS: u16 = 2;
/// From this width the tokens and history cards share a row.
const SIDE_BY_SIDE_FROM: usize = 100;
/// The tabs redraw this often, so ages and elapsed times move.
const REDRAW: Duration = Duration::from_secs(1);
/// How often the Status tab is asked for again while its run works.
const STATUS_EVERY: Duration = Duration::from_secs(1);
/// How often the Context tab is asked for again while work runs. A session reports its
/// context at most this often.
const CONTEXT_EVERY: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Overview,
    Status,
    Usage,
    Context,
    Limits,
    Stats,
}

impl Tab {
    pub const ALL: [Tab; 6] = [
        Tab::Overview,
        Tab::Status,
        Tab::Usage,
        Tab::Context,
        Tab::Limits,
        Tab::Stats,
    ];

    /// Where a mod file names this tab.
    fn mod_place(self) -> ModPlace {
        match self {
            Tab::Overview => ModPlace::Overview,
            Tab::Status => ModPlace::Status,
            Tab::Usage => ModPlace::Usage,
            Tab::Context => ModPlace::Context,
            Tab::Limits => ModPlace::Limits,
            Tab::Stats => ModPlace::Stats,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Status => "Status",
            Tab::Usage => "Usage",
            Tab::Context => "Context",
            Tab::Limits => "Limits",
            Tab::Stats => "Stats",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Tab::Overview => "Where the crew stands: plan usage, spend and workflows",
            Tab::Status => "Where the run stands: goal, plan and tasks",
            Tab::Usage => "Plan usage, spend and tokens across the crew",
            Tab::Context => "What fills each session's context",
            Tab::Limits => "What counts toward the limits you set",
            Tab::Stats => "Each CLI and model by itself, and all together",
        }
    }

    /// The tab that shows the mod `name` at full size: the run's on Status, the context's on
    /// Context, the run's spend on Limits, and every usage mod's on Usage.
    pub fn showing(name: WidgetName) -> Tab {
        match name {
            WidgetName::Run | WidgetName::Waiting | WidgetName::Recent => Tab::Status,
            WidgetName::Context | WidgetName::Sessions => Tab::Context,
            WidgetName::Budget | WidgetName::Tasks | WidgetName::Limits => Tab::Limits,
            WidgetName::Stats => Tab::Stats,
            _ => Tab::Usage,
        }
    }

    /// The tab `by` places along, stopping at either end.
    fn step(self, by: isize) -> Tab {
        let at = Tab::ALL.iter().position(|tab| *tab == self).unwrap_or(0);
        let last = Tab::ALL.len() - 1;
        Tab::ALL[at.saturating_add_signed(by).min(last)]
    }

    /// It draws the usage cards, so the period and the pin keys work on it.
    fn shows_usage(self) -> bool {
        !matches!(self, Tab::Status | Tab::Context)
    }

    /// The cards it stacks, in order, each in its form.
    fn cards(self) -> &'static [(UsageCard, Form)] {
        use UsageCard::*;
        match self {
            Tab::Overview => &[
                (PlanUsage, Form::Compact),
                (Spend, Form::Compact),
                (Workflows, Form::Full),
            ],
            Tab::Usage => &[
                (PlanUsage, Form::Full),
                (Spend, Form::Full),
                (Tokens, Form::Full),
                (History, Form::Full),
                (Workflows, Form::Full),
            ],
            Tab::Limits => &[(Spend, Form::Full), (PlanUsage, Form::Compact)],
            Tab::Status | Tab::Context | Tab::Stats => &[],
        }
    }
}

/// What a key moved to, which may be below the rows the panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Follow {
    /// The card Tab gave the focus.
    Focus,
    /// The tab's last block, such as the list of limits.
    Last,
}

#[derive(Debug)]
pub struct OverviewView {
    tab: Tab,
    period: UsagePeriod,
    /// `u`: plan usage as what's left of a window.
    left: bool,
    /// The card `p` and `,` act on.
    focus: usize,
    /// The first card shown, when the cards are taller than the panel.
    scroll: usize,
    /// Which block holds the card in focus, as the tab last drew it.
    focus_block: Cell<usize>,
    /// What the next frame scrolls into view, after a key moved to it.
    follow: Option<Follow>,
    status: Option<StatusSummary>,
    context: Option<ContextData>,
    /// The session the Context tab's top card shows, by its row.
    picked: usize,
    /// `a`: the Context tab lists every item of every category.
    all: bool,
    /// The task the Status tab's keys are on, by its row in the run's card.
    task: usize,
    /// The limit the Limits tab's keys are on, by its row in the list.
    limit: usize,
    watch: UsageWatch,
    /// A fixed clock, for tests. Otherwise the wall clock, each time it draws.
    now: Option<OffsetDateTime>,
    /// How the Stats tab shows the crew's stats.
    stats: StatsState,
    /// Stats waits for the period `mods/stats.toml` sets, which comes with the numbers.
    stats_period_from_file: bool,
}

impl OverviewView {
    pub fn new(tab: Tab, watch: UsageWatch) -> Self {
        Self {
            tab,
            period: UsagePeriod::default(),
            left: false,
            focus: 0,
            scroll: 0,
            focus_block: Cell::new(0),
            follow: None,
            status: None,
            context: None,
            picked: 0,
            all: false,
            task: 0,
            limit: 0,
            watch,
            now: None,
            stats: StatsState::default(),
            stats_period_from_file: false,
        }
    }

    /// `/stats` with no period: the tab shows the one `mods/stats.toml` sets, once the
    /// numbers for it come.
    pub fn with_stats_period_from_file(mut self) -> Self {
        self.stats_period_from_file = true;
        self
    }

    /// `/mods stats`: the Stats tab with the stats mod's page open, once the numbers that
    /// carry its file's settings come.
    pub fn with_stats_page(mut self) -> Self {
        self.stats.open_page(None);
        self
    }

    pub fn with_period(mut self, period: UsagePeriod) -> Self {
        self.period = period;
        self
    }

    pub fn with_status(mut self, status: StatusSummary) -> Self {
        self.status = Some(status);
        self
    }

    pub fn with_context(mut self, context: ContextData) -> Self {
        self.context = Some(context);
        self
    }

    /// `/context all`: the Context tab opens with every item of every category listed.
    pub fn listing_all(mut self, all: bool) -> Self {
        self.all = all;
        self
    }

    /// Draws ages and reset times as of `now`, whenever it draws.
    pub fn at(mut self, now: OffsetDateTime) -> Self {
        self.now = Some(now);
        self
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    fn wall(&self) -> OffsetDateTime {
        self.now.unwrap_or_else(crate::bottom::wall_now)
    }

    /// The numbers to draw: the state's, when they're for the period this tab shows.
    fn usage<'a>(&self, ctx: &'a ViewCtx<'_>) -> Option<&'a UsageSummary> {
        ctx.state
            .usage
            .as_deref()
            .filter(|usage| usage.period == self.period)
    }

    /// The tab's cards, top to bottom. A note under them is a block too, and so are your mods
    /// from `mods/` that show on the tab, under its own cards.
    fn blocks(&self, kit: &Kit<'_>, ctx: &ViewCtx<'_>, width: usize) -> Vec<Drawn> {
        let mut blocks = match self.tab {
            Tab::Status => self.status_blocks(kit, ctx, width),
            Tab::Context => self.context_blocks(kit, ctx, width),
            Tab::Stats => self.stats_blocks(kit, ctx, width),
            _ => self.usage_blocks(kit, ctx, width),
        };
        if let Some(view) = ctx.state.mods.as_ref() {
            let yours = crate::mods::on_tab(kit, view, self.tab.mod_place(), width);
            if !yours.lines.is_empty() {
                blocks.push(yours);
            }
        }
        blocks
    }

    fn stats_blocks(&self, kit: &Kit<'_>, ctx: &ViewCtx<'_>, width: usize) -> Vec<Drawn> {
        let usage = self.usage(ctx).filter(|_| !self.stats_period_from_file);
        if self.stats.page_open() {
            let blocks = self
                .stats
                .page_blocks(kit, usage, &ctx.state.names, self.wall(), width);
            // The page's rows are the keys' focus, last, under its preview.
            self.focus_block.set(blocks.len().saturating_sub(1));
            return blocks;
        }
        let Some(usage) = usage else {
            return vec![note(kit, "Reading the crew's stats…")];
        };
        let (blocks, focus) = self
            .stats
            .blocks(kit, usage, &ctx.state.names, self.wall(), width);
        self.focus_block.set(focus);
        blocks
    }

    /// Takes what `mods/stats.toml` sets from the first numbers that carry it: the period,
    /// when `/stats` named none, then the group and the measure. Numbers for another period
    /// are asked for as the tab draws.
    fn settle_stats(&mut self, ctx: &ViewCtx<'_>) {
        if self.tab != Tab::Stats {
            return;
        }
        let Some(usage) = ctx.state.usage.as_deref() else {
            return;
        };
        let Some(stats) = &usage.stats else {
            return;
        };
        if self.stats_period_from_file {
            self.period = stats.settings.period.usage();
            self.stats_period_from_file = false;
        }
        if usage.period == self.period {
            self.stats.settle(stats);
        }
    }

    /// How many of the tab's cards Tab steps through.
    fn card_count(&self, ctx: &ViewCtx<'_>) -> usize {
        match self.tab {
            Tab::Status => self.status_cards(ctx).len().max(1),
            Tab::Context => 1,
            _ => self.tab.cards().len().max(1),
        }
    }

    /// The Status tab's framed cards, in the order Tab steps through them, each by the mod
    /// that pins it above the input.
    fn status_cards(&self, ctx: &ViewCtx<'_>) -> Vec<WidgetName> {
        let Some(status) = &self.status else {
            return Vec::new();
        };
        let mut cards = vec![WidgetName::Run];
        if status.budget.is_some() {
            cards.push(WidgetName::Budget);
        }
        if ctx.state.usage.is_some() {
            cards.push(WidgetName::Quota);
        }
        if !status.waiting.is_empty() {
            cards.push(WidgetName::Waiting);
        }
        cards
    }

    /// The Status tab's card that has the keys.
    fn status_focus(&self, ctx: &ViewCtx<'_>) -> Option<WidgetName> {
        let cards = self.status_cards(ctx);
        cards
            .get(self.focus.min(cards.len().saturating_sub(1)))
            .copied()
    }

    /// The run's card, then what it spent beside plan usage where the panel is wide enough,
    /// then what needs you, and a row of what happened last.
    fn status_blocks(&self, kit: &Kit<'_>, ctx: &ViewCtx<'_>, width: usize) -> Vec<Drawn> {
        let Some(summary) = &self.status else {
            return vec![note(kit, "Reading status…")];
        };
        let title = summary.run.as_ref().and_then(|run| run_name(ctx, run));
        let cards = StatusCards {
            summary,
            names: &ctx.state.names,
            title: title.as_deref(),
        };
        if cards.is_empty() {
            return vec![note(kit, NOTHING_RUNNING)];
        }
        // Each framed card takes the next place in Tab's round.
        let mut place = 0;
        let mut framed = |frame: Frame| {
            let active = place == self.focus;
            place += 1;
            Frame { active, ..frame }
        };
        // The block the card in focus lands in, counted as the blocks stack up.
        let in_focus = |first: usize, cards: usize, block: usize| {
            if (first..first + cards).contains(&self.focus) {
                self.focus_block.set(block);
            }
        };
        self.focus_block.set(0);
        // The arrows pick a task while the run's card has the keys.
        let tasks = summary.cards.len();
        let picks = Picks {
            at: (self.focus == 0 && tasks > 0).then(|| self.task.min(tasks - 1)),
            mark: Marks::for_theme(ctx.theme).selected,
        };
        let shape = RunShape {
            picks: Some(picks),
            ..RunShape::WHOLE
        };
        let mut blocks = vec![cards.run(kit, framed(Frame::view(width)), shape)];
        let plan = ctx.state.usage.as_deref().map(|usage| UsageCards {
            usage,
            names: &ctx.state.names,
            now: self.wall(),
            left: self.left,
        });
        let plan_card =
            |frame| plan.map(|plan| plan.draw(kit, UsageCard::PlanUsage, Form::Compact, frame));
        let mut next = 1;
        if summary.budget.is_some() && plan.is_some() && width >= SIDE_BY_SIDE_FROM {
            let (left, right, at) = halves(width);
            let spend = cards.spend(kit, framed(left));
            let plan = plan_card(framed(right));
            if let (Some(spend), Some(plan)) = (spend, plan) {
                in_focus(next, 2, blocks.len());
                blocks.push(side_by_side(spend, plan, at));
            }
            next += 2;
        } else {
            if summary.budget.is_some() {
                in_focus(next, 1, blocks.len());
                next += 1;
                blocks.extend(cards.spend(kit, framed(Frame::view(width))));
            }
            if plan.is_some() {
                in_focus(next, 1, blocks.len());
                next += 1;
                blocks.extend(plan_card(framed(Frame::view(width))));
            }
        }
        if !summary.waiting.is_empty() {
            in_focus(next, 1, blocks.len());
            blocks.extend(cards.needs_you(kit, framed(Frame::view(width))));
        }
        blocks.extend(cards.recent(kit, width));
        blocks
    }

    /// The picked session's context, then the list ↑↓ picks from.
    fn context_blocks(&self, kit: &Kit<'_>, ctx: &ViewCtx<'_>, width: usize) -> Vec<Drawn> {
        let Some(view) = &self.context else {
            return vec![note(kit, "Reading context…")];
        };
        let cards = ContextCards {
            view,
            names: &ctx.state.names,
            selected: self.picked.min(view.rows.len().saturating_sub(1)),
            mark: Marks::for_theme(ctx.theme).selected,
            breakdown: ctx.state.chat.context.as_ref(),
            all: self.all,
        };
        let mut blocks: Vec<Drawn> = cards.session(kit, Frame::view(width)).into_iter().collect();
        // The list is what the keys move in, so it carries the focus.
        let list = Frame {
            active: true,
            ..Frame::view(width)
        };
        blocks.push(cards.sessions(kit, list));
        blocks
    }

    /// The usage cards the tab stacks, the tokens and history cards as one block side by
    /// side where the panel is wide enough.
    fn usage_blocks(&self, kit: &Kit<'_>, ctx: &ViewCtx<'_>, width: usize) -> Vec<Drawn> {
        let Some(usage) = self.usage(ctx) else {
            return vec![note(kit, "Reading usage…")];
        };
        let cards = UsageCards {
            usage,
            names: &ctx.state.names,
            now: self.wall(),
            left: self.left,
        };
        let listed = self.tab.cards();
        let focus = self.focus.min(listed.len().saturating_sub(1));
        let draw = |at: usize, frame: Frame| {
            let (card, form) = listed[at];
            let frame = Frame {
                active: at == focus,
                ..frame
            };
            cards.draw(kit, card, form, frame)
        };
        let pair = listed
            .iter()
            .position(|(card, _)| *card == UsageCard::Tokens);
        let mut stacked = Vec::new();
        // The run the screen follows counts toward a limit of its own, so it comes first.
        let followed = ctx.state.followed_run();
        let run = followed
            .filter(|_| self.tab == Tab::Limits)
            .map(PinnedRun::of);
        if let Some(run) = &run {
            let run = StatusCards {
                summary: &run.summary,
                names: &ctx.state.names,
                title: run.title.as_deref(),
            };
            if run.has(StatusCard::Spend) {
                let frame = Frame::view(width);
                stacked.push(run.pinned(kit, StatusCard::Spend, Form::Full, frame));
            }
        }
        let mut at = 0;
        while at < listed.len() {
            let beside = Some(at) == pair
                && width >= SIDE_BY_SIDE_FROM
                && listed
                    .get(at + 1)
                    .is_some_and(|(card, _)| *card == UsageCard::History);
            let cards = if beside { 2 } else { 1 };
            if (at..at + cards).contains(&focus) {
                self.focus_block.set(stacked.len());
            }
            if beside {
                let (left, right, column) = halves(width);
                stacked.push(side_by_side(draw(at, left), draw(at + 1, right), column));
            } else {
                stacked.push(draw(at, Frame::view(width)));
            }
            at += cards;
        }
        if self.tab == Tab::Limits {
            let tasks = followed.map(tasks_of).unwrap_or_default();
            let limits = LimitsCards {
                tasks: &tasks,
                task_limit: usage.task_limit,
                limits: &usage.limits,
            };
            stacked.extend(limits.tasks(kit, Frame::view(width)));
            let mark = Marks::for_theme(ctx.theme).selected;
            match limits.list(kit, width, self.limit, mark) {
                Some(list) => stacked.push(list),
                // A controller that lists no limits still says what Enter does.
                None => stacked.push(self::note(
                    kit,
                    "Enter opens the limits, which stop a run only once you set one.",
                )),
            }
        }
        stacked
    }

    /// The blocks from `first` on that fit whole in `room` rows, one blank row apart, so no
    /// frame is ever cut open. A first block taller than the room keeps its top rows and its
    /// bottom border.
    fn fitted(blocks: Vec<Drawn>, first: usize, room: usize) -> Drawn {
        let mut shown = Vec::new();
        let mut used = 0;
        for block in blocks.into_iter().skip(first) {
            let gap = usize::from(!shown.is_empty());
            if used + gap + block.height() > room {
                if shown.is_empty() && room > 0 {
                    let mut cut = block;
                    let bottom = cut.lines.pop();
                    cut.lines.truncate(room - 1);
                    cut.lines.extend(bottom);
                    cut.meters.clear();
                    shown.push(cut);
                }
                break;
            }
            used += gap + block.height();
            shown.push(block);
        }
        stack_cards(shown)
    }

    /// The first block to show so that block `wanted` shows whole: `first` when it already
    /// does, and otherwise as little further as brings it in.
    fn scrolled_to(blocks: &[Drawn], first: usize, wanted: usize, room: usize) -> usize {
        if wanted <= first {
            return wanted;
        }
        let fits_from = |from: usize| {
            let shown = &blocks[from..=wanted.min(blocks.len().saturating_sub(1))];
            let rows: usize = shown.iter().map(Drawn::height).sum();
            rows + shown.len().saturating_sub(1) <= room
        };
        (first..wanted)
            .find(|from| fits_from(*from))
            .unwrap_or(wanted)
    }

    /// `Overview  Status  [Usage]  Context  Limits  Stats`, and on a usage tab the periods at
    /// the right, as Claude Code's Settings dialog lays its tabs out.
    fn tabs(&self, kit: &Kit<'_>, width: usize) -> Vec<Span<'static>> {
        let mut left = vec![Span::raw(" ")];
        for tab in Tab::ALL {
            let open = tab == self.tab;
            left.push(if open && kit.has_color() {
                Span::styled(
                    format!(" {} ", tab.title()),
                    kit.cell(Role::BadgeText, Role::Card),
                )
            } else if open {
                kit.strong(format!("[{}]", tab.title()))
            } else if tab == Tab::Overview {
                Span::styled(format!(" {} ", tab.title()), kit.bold(Role::Card))
            } else {
                kit.strong(format!(" {} ", tab.title()))
            });
            left.push(Span::raw(" "));
        }
        let mut right = Vec::new();
        if self.tab.shows_usage() {
            let periods = if self.tab == Tab::Stats {
                stats::PERIODS
            } else {
                [
                    (UsagePeriod::Today, "Today"),
                    (UsagePeriod::Week, "Week"),
                    (UsagePeriod::Month, "Month"),
                    (UsagePeriod::All, "All"),
                ]
            };
            for (period, word) in periods {
                right.push(if period == self.period {
                    Span::styled(word, kit.bold(Role::Card))
                } else {
                    kit.dim(word)
                });
                right.push(Span::raw("  "));
            }
            right.pop();
            right.push(Span::raw(" "));
        }
        if spans_width(&left) + spans_width(&right) > width {
            return truncate_spans(&left, width);
        }
        spread(left, right, width)
    }

    /// What the tab is about, and at the right what the period came to.
    fn about(&self, kit: &Kit<'_>, ctx: &ViewCtx<'_>, width: usize) -> Vec<Span<'static>> {
        if self.tab == Tab::Stats
            && let Some(page) = &self.stats.page
        {
            return page.about(kit, width);
        }
        if self.tab == Tab::Stats {
            let left = vec![kit.dim(format!(" {}", self.stats.description(self.period)))];
            let mut right = self.stats.views(kit);
            right.push(Span::raw(" "));
            if spans_width(&left) + spans_width(&right) + 1 > width {
                return spread(Vec::new(), truncate_spans(&right, width), width);
            }
            return spread(left, right, width);
        }
        let left = vec![kit.dim(format!(" {}", self.tab.description()))];
        let right = match self.summary(ctx) {
            Some(summary) => vec![kit.dim(format!("{summary} "))],
            None => Vec::new(),
        };
        if spans_width(&left) + spans_width(&right) + 2 > width {
            return truncate_spans(&left, width);
        }
        spread(left, right, width)
    }

    /// What the open tab comes to, for the right of the row under the tabs.
    fn summary(&self, ctx: &ViewCtx<'_>) -> Option<String> {
        match self.tab {
            Tab::Status => {
                let status = self.status.as_ref()?;
                let run = status.run.as_ref()?.to_string();
                Some(match &status.budget {
                    Some(budget) => format!("{run} · {}", format::spend(&budget.spent)),
                    None => run,
                })
            }
            Tab::Context => {
                let rows = &self.context.as_ref()?.rows;
                let used: u64 = rows.iter().map(|row| row.used).sum();
                let sessions = match rows.len() {
                    1 => "1 session".to_owned(),
                    n => format!("{n} sessions"),
                };
                Some(format!("{sessions} · {} in context", format::tokens(used)))
            }
            _ => {
                let usage = self.usage(ctx)?;
                Some(format!(
                    "{} {} · {} tokens",
                    format::spend(&usage.total),
                    period_words(usage.period),
                    format::tokens(usage.total.total_tokens())
                ))
            }
        }
    }

    /// The keys in `width` cells. The way out always shows whole, so the keys before it give
    /// way from the last one back.
    fn keys(&self, ctx: &ViewCtx<'_>, scrolls: bool, width: usize) -> String {
        let running = matches!(
            ctx.state.mode,
            Some(AppMode::Answering { .. } | AppMode::Working { .. } | AppMode::Pausing { .. })
        );
        let mut close = if running { PANEL_HINT } else { "esc back" };
        let mut keys = Vec::new();
        if self.tab == Tab::Stats && self.stats.page_open() {
            // Esc goes back to the tab, which the panel's own hint wouldn't say.
            close = "esc back";
            keys = self
                .stats
                .page
                .as_ref()
                .map(|page| page.keys())
                .unwrap_or_default();
        } else if self.tab == Tab::Stats {
            keys = self.stats.keys();
            if scrolls && keys[0] == "←→ tab" {
                keys.insert(0, "↑↓ move");
            }
        }
        let picks_row = match self.tab {
            Tab::Status => self.focus == 0,
            Tab::Context | Tab::Limits => true,
            _ => false,
        };
        if self.tab != Tab::Stats && scrolls && !picks_row {
            keys.push("↑↓ scroll");
        }
        if self.tab != Tab::Stats {
            keys.push("←→ tab");
        }
        match self.tab {
            Tab::Stats => {}
            Tab::Status => {
                let on_run = self.focus == 0;
                let tasks = self.status.as_ref().map_or(0, |status| status.cards.len());
                if on_run && tasks > 0 {
                    keys.extend(["↑↓ task", "enter agent"]);
                } else {
                    keys.push("enter workflow");
                }
                keys.extend([
                    "tab next card",
                    "p pin above the input",
                    "s ask the narrator",
                    "u used or left",
                ]);
            }
            Tab::Context => {
                keys.extend(["↑↓ session", "enter agent view"]);
                keys.push(if self.all {
                    "a the largest items"
                } else {
                    "a every item"
                });
            }
            _ => {
                if self.tab == Tab::Usage {
                    keys.push("d w m a period");
                }
                if self.tab == Tab::Limits {
                    keys.extend(["↑↓ limit", "enter change"]);
                }
                keys.extend([
                    "tab next card",
                    "p pin above the input",
                    "u used or left",
                    ", settings",
                ]);
            }
        }
        loop {
            let line = keys
                .iter()
                .copied()
                .chain([close])
                .collect::<Vec<_>>()
                .join(" · ");
            if crate::text::str_width(&line) < width || keys.pop().is_none() {
                return line;
            }
        }
    }

    /// Opens `tab`, asking for what it shows when the screen doesn't hold it.
    fn open(&mut self, tab: Tab) -> ViewOutcome {
        if tab == self.tab {
            return ViewOutcome::Consumed;
        }
        self.tab = tab;
        self.focus = 0;
        self.scroll = 0;
        match tab {
            Tab::Status => ViewOutcome::Request(ViewRequest::Status),
            Tab::Context => ViewOutcome::Request(ViewRequest::Context),
            _ => ViewOutcome::Consumed,
        }
    }

    fn focused(&self) -> Option<UsageCard> {
        let cards = self.tab.cards();
        cards
            .get(self.focus.min(cards.len().saturating_sub(1)))
            .map(|(card, _)| *card)
    }
}

impl View for OverviewView {
    fn docked(&self) -> bool {
        true
    }

    /// The rows its cards take, so a short tab leaves the conversation its room.
    fn dock_height(&mut self, screen: Size, ctx: &ViewCtx<'_>) -> Option<u16> {
        self.settle_stats(ctx);
        let kit = Kit::new(ctx.theme);
        let blocks = self.blocks(&kit, ctx, usize::from(screen.width));
        let body = stack_cards(blocks).height();
        let wanted = u16::try_from(body)
            .unwrap_or(u16::MAX)
            .saturating_add(HEAD_ROWS + FOOT_ROWS);
        // The working line and a few rows of the conversation stay above it.
        Some(
            wanted
                .min(screen.height.saturating_sub(6))
                .max(HEAD_ROWS + FOOT_ROWS + 1),
        )
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        if area.height < HEAD_ROWS || area.width == 0 {
            return;
        }
        self.settle_stats(ctx);
        let kit = Kit::new(ctx.theme);
        let width = usize::from(area.width);
        let rule = Line::from(marks::decorative(
            ctx.theme.glyphs.rule.repeat(width),
            ctx.theme.style(Role::Card),
        ));
        buf.set_line(area.x, area.y, &rule, area.width);
        buf.set_line(
            area.x,
            area.y + 1,
            &Line::from(self.tabs(&kit, width)),
            area.width,
        );
        let about = Line::from(self.about(&kit, ctx, width));
        buf.set_line(area.x, area.y + 2, &about, area.width);
        self.keep_fresh(ctx);
        let blocks = self.blocks(&kit, ctx, width);
        let room = usize::from(area.height.saturating_sub(HEAD_ROWS + FOOT_ROWS));
        let count = blocks.len();
        let whole = stack_cards(blocks.clone()).height();
        self.scroll = self.scroll.min(count.saturating_sub(1));
        if let Some(follow) = self.follow.take() {
            let wanted = match follow {
                Follow::Focus => self.focus_block.get(),
                Follow::Last => count.saturating_sub(1),
            };
            self.scroll = Self::scrolled_to(&blocks, self.scroll, wanted, room);
        }
        if self.tab == Tab::Context {
            // The keys pick a session, so when both cards can't show the list stays. Asked
            // for every item, the card that lists them stays instead, cut where the room ends.
            self.scroll = usize::from(whole > room && count > 1 && !self.all);
        }
        let body = Self::fitted(blocks, self.scroll, room);
        for (row, line) in body.lines.iter().take(room).enumerate() {
            buf.set_line(area.x, area.y + HEAD_ROWS + row as u16, line, area.width);
        }
        let keys = self.keys(ctx, whole > room, width);
        let keys = Line::from(truncate_spans(&[kit.dim(format!(" {keys}"))], width));
        buf.set_line(area.x, area.bottom() - 1, &keys, area.width);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return ViewOutcome::Ignored;
        }
        if self.tab == Tab::Stats && self.stats.page_open() {
            return self.stats_page_key(key, ctx);
        }
        if self.tab == Tab::Stats && key.code == KeyCode::Esc && self.stats.close_opened() {
            return ViewOutcome::Consumed;
        }
        match key.code {
            KeyCode::Right => return self.open(self.tab.step(1)),
            // On the first tab ← has nowhere to go, as in Claude Code's dialogs.
            KeyCode::Left => return self.open(self.tab.step(-1)),
            KeyCode::Esc => return ViewOutcome::Close,
            _ => {}
        }
        match self.tab {
            Tab::Status => self.status_key(key, ctx),
            Tab::Context => self.context_key(key, ctx),
            Tab::Stats => self.stats_key(key, ctx),
            _ => self.usage_key(key, ctx),
        }
    }

    /// Status and Context answers land under their tabs. Usage numbers go on to the screen's
    /// state, where the cards above the input read them too.
    fn accept(&mut self, data: &ViewData) -> bool {
        match data {
            ViewData::Status { summary } => self.status = Some(summary.clone()),
            ViewData::Context { view } => self.context = Some(view.clone()),
            _ => return false,
        }
        // It may be the answer to a refresh of the open tab, which is then settled.
        self.watch.answered(data, Instant::now());
        true
    }

    fn next_change(&self, now: Instant) -> Option<Instant> {
        Some(now + REDRAW)
    }
}

impl OverviewView {
    /// Says what the open tab draws, so it's asked for again while it stays on screen: the
    /// usage numbers every second, the run's status while the run works, and the sessions'
    /// context while anything does.
    fn keep_fresh(&self, ctx: &ViewCtx<'_>) {
        if self.tab != Tab::Context {
            self.watch.drawn(self.period, true, ctx.now);
        }
        let state = ctx.state;
        match self.tab {
            Tab::Status => {
                let run = self.status.as_ref().and_then(|status| status.run.as_ref());
                let works = run
                    .and_then(|run| state.runs.get(run))
                    .is_some_and(|run| run.is_active());
                if works {
                    self.watch
                        .drawn_tab(ViewRequest::Status, STATUS_EVERY, ctx.now);
                }
            }
            Tab::Context => {
                let works = matches!(
                    state.mode,
                    Some(AppMode::Answering { .. } | AppMode::Working { .. })
                ) || state.runs.values().any(|run| run.is_active());
                if works {
                    self.watch
                        .drawn_tab(ViewRequest::Context, CONTEXT_EVERY, ctx.now);
                }
            }
            _ => {}
        }
    }

    fn step_focus(&mut self, back: bool, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let cards = self.card_count(ctx);
        let by = if back { cards - 1 } else { 1 };
        self.focus = (self.focus.min(cards - 1) + by) % cards;
        self.follow = Some(Follow::Focus);
        ViewOutcome::Consumed
    }

    fn status_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        let tasks = self.status.as_ref().map_or(0, |status| status.cards.len());
        // On the run's card the arrows pick a task. On any other they scroll the tab.
        let on_run = self.status_focus(ctx) == Some(WidgetName::Run) && tasks > 0;
        match key.code {
            KeyCode::Char('s') => ViewOutcome::SendToChat(UiCommand::Summary),
            KeyCode::Enter => self.open_from_status(on_run, ctx),
            KeyCode::Tab => self.step_focus(false, ctx),
            KeyCode::BackTab => self.step_focus(true, ctx),
            KeyCode::Char('u') => {
                self.left = !self.left;
                ViewOutcome::Consumed
            }
            KeyCode::Char('p') => match self.status_focus(ctx) {
                Some(card) => pin(card),
                None => ViewOutcome::Ignored,
            },
            KeyCode::Up if on_run => {
                self.task = self.task.min(tasks - 1).saturating_sub(1);
                ViewOutcome::Consumed
            }
            KeyCode::Down if on_run => {
                self.task = (self.task + 1).min(tasks - 1);
                ViewOutcome::Consumed
            }
            KeyCode::Home if on_run => {
                self.task = 0;
                ViewOutcome::Consumed
            }
            KeyCode::End if on_run => {
                self.task = tasks - 1;
                ViewOutcome::Consumed
            }
            KeyCode::Up => {
                self.scroll = self.scroll.saturating_sub(1);
                ViewOutcome::Consumed
            }
            KeyCode::Down => {
                self.scroll += 1;
                ViewOutcome::Consumed
            }
            KeyCode::Char('q') => ViewOutcome::Close,
            _ => ViewOutcome::Ignored,
        }
    }

    /// Enter on the Status tab: the picked task's agent where the screen knows one, and
    /// otherwise the run's workflow, which lists them all.
    fn open_from_status(&self, on_run: bool, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let Some(status) = &self.status else {
            return ViewOutcome::Consumed;
        };
        let Some(run) = status.run.clone() else {
            return ViewOutcome::Notice("No run to open".to_owned());
        };
        let task = status
            .cards
            .get(self.task.min(status.cards.len().saturating_sub(1)))
            .filter(|_| on_run);
        let agent = task.and_then(|task| {
            let sessions = ctx.state.runs.get(&run)?.sessions_in_order();
            let mut of_task: Vec<_> = sessions
                .filter(|session| session.card.as_ref() == Some(&task.card))
                .collect();
            // The one at work now, else the last that worked on it.
            of_task.sort_by_key(|session| session.is_running());
            of_task.last().map(|session| session.key.clone())
        });
        match agent {
            Some(session) => ViewOutcome::Request(ViewRequest::Agent { run, session }),
            None => ViewOutcome::Request(ViewRequest::Workflow {
                run,
                workflow: None,
            }),
        }
    }

    fn context_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        let rows = self.context.as_ref().map_or(&[][..], |view| &view.rows);
        let last = rows.len().saturating_sub(1);
        match key.code {
            KeyCode::Up => self.picked = self.picked.min(last).saturating_sub(1),
            KeyCode::Down => self.picked = (self.picked + 1).min(last),
            KeyCode::Home => self.picked = 0,
            KeyCode::End => self.picked = last,
            KeyCode::Enter => {
                return match rows.get(self.picked) {
                    Some(row) => match &row.run {
                        Some(run) => ViewOutcome::Request(ViewRequest::Agent {
                            run: run.clone(),
                            session: row.session.clone(),
                        }),
                        None => ViewOutcome::Notice(
                            "Chat with the main agent has no agent view".to_owned(),
                        ),
                    },
                    None => ViewOutcome::Consumed,
                };
            }
            KeyCode::Char('a') => self.all = !self.all,
            KeyCode::Char('q') => return ViewOutcome::Close,
            _ => return ViewOutcome::Ignored,
        }
        ViewOutcome::Consumed
    }

    fn stats_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        let usage = self.usage(ctx);
        let names = &ctx.state.names;
        match self.stats.key(key.code, usage, names, self.wall()) {
            None if key.code == KeyCode::Char('q') => ViewOutcome::Close,
            stats_key => self.stats_outcome(stats_key),
        }
    }

    /// The stats mod's page has the keys. Esc goes back to the tab.
    fn stats_page_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        if self.stats.page.is_none() {
            // Still waiting for the file's settings.
            return match key.code {
                KeyCode::Esc => {
                    self.stats.close_page();
                    ViewOutcome::Consumed
                }
                _ => ViewOutcome::Ignored,
            };
        }
        let stats_key = self.stats.page_key(key);
        self.stats_outcome(stats_key)
    }

    fn stats_outcome(&mut self, key: Option<StatsKey>) -> ViewOutcome {
        match key {
            Some(StatsKey::Moved) => {
                self.follow = Some(Follow::Focus);
                ViewOutcome::Consumed
            }
            Some(StatsKey::Period(period)) => {
                self.period = period;
                self.scroll = 0;
                ViewOutcome::Request(ViewRequest::Usage { period })
            }
            Some(StatsKey::Pin) => pin(WidgetName::Stats),
            Some(StatsKey::Send(command)) => ViewOutcome::Send(command),
            None => ViewOutcome::Ignored,
        }
    }

    fn usage_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        // On the Limits tab the arrows pick a limit, and Enter opens it to change.
        let limits = match (self.tab, self.usage(ctx)) {
            (Tab::Limits, Some(usage)) => usage.limits.as_slice(),
            _ => &[],
        };
        if let Some(last) = limits.len().checked_sub(1) {
            let picked = self.limit.min(last);
            let moved = match key.code {
                KeyCode::Up => Some(picked.saturating_sub(1)),
                KeyCode::Down => Some((picked + 1).min(last)),
                KeyCode::Home => Some(0),
                KeyCode::End => Some(last),
                _ => None,
            };
            if let Some(moved) = moved {
                self.limit = moved;
                self.follow = Some(Follow::Last);
                return ViewOutcome::Consumed;
            }
            // Its picker opens under the input, so the panel gives the keys back first.
            if key.code == KeyCode::Enter {
                return ViewOutcome::SendToChat(UiCommand::Slash {
                    command: SlashCommand::Budget {
                        key: Some(limits[picked].key.clone()),
                        value: None,
                        action: ViewAction::Open,
                    },
                });
            }
        }
        let period = |period| ViewOutcome::Request(ViewRequest::Usage { period });
        match key.code {
            KeyCode::Char(c @ ('d' | 'w' | 'm' | 'a')) if self.tab == Tab::Usage => {
                self.period = match c {
                    'd' => UsagePeriod::Today,
                    'w' => UsagePeriod::Week,
                    'm' => UsagePeriod::Month,
                    _ => UsagePeriod::All,
                };
                self.scroll = 0;
                period(self.period)
            }
            KeyCode::Tab => {
                self.focus = (self.focus + 1) % self.tab.cards().len().max(1);
                self.follow = Some(Follow::Focus);
                ViewOutcome::Consumed
            }
            KeyCode::BackTab => {
                let cards = self.tab.cards().len().max(1);
                self.focus = (self.focus + cards - 1) % cards;
                self.follow = Some(Follow::Focus);
                ViewOutcome::Consumed
            }
            KeyCode::Char('u') => {
                self.left = !self.left;
                ViewOutcome::Consumed
            }
            KeyCode::Char('p') => match self.focused() {
                Some(card) => pin(card.name()),
                None => ViewOutcome::Ignored,
            },
            KeyCode::Char(',') => match self.focused() {
                Some(card) => ViewOutcome::Send(UiCommand::Slash {
                    command: SlashCommand::Widgets {
                        action: WidgetsAction::Page {
                            widget: card.name(),
                        },
                        scope: None,
                    },
                }),
                None => ViewOutcome::Ignored,
            },
            KeyCode::Up => {
                self.scroll = self.scroll.saturating_sub(1);
                ViewOutcome::Consumed
            }
            KeyCode::Down => {
                self.scroll += 1;
                ViewOutcome::Consumed
            }
            // With no list to pick from, Enter opens every limit in their own picker.
            KeyCode::Enter if self.tab == Tab::Limits => {
                ViewOutcome::SendToChat(UiCommand::Slash {
                    command: SlashCommand::Budget {
                        key: None,
                        value: None,
                        action: ViewAction::Open,
                    },
                })
            }
            KeyCode::Char('q') => ViewOutcome::Close,
            _ => ViewOutcome::Ignored,
        }
    }
}

/// The run's own name, such as `export-reports-csv`, from the lists the screen holds: the
/// period's workflows, or the runs going on behind this conversation.
fn run_name(ctx: &ViewCtx<'_>, run: &RunId) -> Option<String> {
    let listed = ctx.state.usage.iter().flat_map(|usage| &usage.runs);
    listed
        .chain(&ctx.state.background)
        .find(|summary| summary.run == *run)
        .map(|summary| {
            summary
                .name
                .clone()
                .unwrap_or_else(|| summary.title.clone())
        })
}

/// `p`: pins the mod above the input, or unpins it when it's there.
fn pin(widget: WidgetName) -> ViewOutcome {
    ViewOutcome::Send(UiCommand::Slash {
        command: SlashCommand::Widgets {
            action: WidgetsAction::Change {
                widget,
                change: WidgetChange::Toggle {
                    place: Place::AboveInput,
                },
            },
            scope: None,
        },
    })
}

/// One dim row under the tabs, such as what a tab says before its answer comes.
fn note(kit: &Kit<'_>, words: &str) -> Drawn {
    let mut block = Drawn::new();
    block.push(vec![kit.dim(format!(" {words}"))]);
    block
}

/// Two cards side by side in `width` cells: each one's frame, and the column the right one
/// starts at. They sit a column in from the screen's edges with two between them.
fn halves(width: usize) -> (Frame, Frame, usize) {
    let half = width.saturating_sub(4) / 2;
    let frame = |left, width| Frame {
        left,
        width,
        toggle: None,
        active: false,
    };
    (
        frame(1, half),
        frame(0, width.saturating_sub(4) - half),
        1 + half + 2,
    )
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
