//! The usage panel: one place for `/status`, `/usage`, `/context` and `/limits`, with a tab
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
use argonauts_core::api::{RunState, StatusSummary, UsagePeriod, UsageSummary};
use argonauts_core::app::{AppMode, UiCommand, ViewData, ViewRequest};
use argonauts_core::input::{ModPlaceChange, ModsAction, SlashCommand, ViewAction};
use argonauts_core::overview::{ModelTokens, OverviewPeriod};
use argonauts_core::settings::mods::ModPlace;
use argonauts_core::settings::stats::StatsGroup;
use argonauts_core::settings::ui::{ModName, Place};
use argonauts_core::status_tab::StatusTabView;
use argonauts_core::views::ContextView as ContextData;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::text::{Line, Span};
use time::OffsetDateTime;

use super::UsageWatch;
use super::cards::{UsageCard, UsageCards, period_words};
use super::config::{self, ConfigTab};
use super::context::{self, ContextPane, ContextTab, Section};
use super::home::{HomeKey, HomeTab};
use super::limits::{LimitsCards, tasks_of};
use super::periods;
use super::shades::ModelColors;
use super::stats::behaviors::{self, BehaviorsTab};
use super::stats::{StatsKey, StatsState};
use super::status::{Picks, RunShape, StatusCard, StatusCards};
use super::status_tab::StatusTabCards;
pub use super::subtabs::Level;
use super::subtabs::{TAB_GAP, chosen, chosen_sub_tab};
use super::usage::{Stop, UsageTab};
use crate::bottom::PinnedRun;
use crate::bridge::tools::Places;
use crate::bridge::transcript::NOTHING_RUNNING;
use crate::format;
use crate::kit::{Drawn, Form, Frame, Kit, side_by_side, spread, stack_cards};
use crate::text::{marks, spans_width, truncate_spans};
use crate::theme::Role;
use crate::views::dock::PANEL_HINT;
use crate::views::map::{MapCells, MapPoint, MapView};
use crate::views::panes::Marks;
use crate::views::{View, ViewCtx, ViewOutcome, list};

/// Rows above the cards: the rule, the tabs, what the tab is about, and a blank row.
const HEAD_ROWS: u16 = 4;
/// Rows under the cards: a blank row and the keys.
const FOOT_ROWS: u16 = 2;
/// The key line's way out, while nothing runs behind the panel.
const CLOSES: &str = "esc closes";
/// From this width the tokens and history cards share a row.
const SIDE_BY_SIDE_FROM: usize = 100;
/// The tabs redraw this often, so ages and elapsed times move.
const REDRAW: Duration = Duration::from_secs(1);
/// How often the Status tab is asked for again while its run works.
const STATUS_EVERY: Duration = Duration::from_secs(1);
/// How often the Context tab is asked for again while work runs. A session reports its
/// context at most this often.
const CONTEXT_EVERY: Duration = Duration::from_secs(10);
/// How often the Overview tab is asked for again while it stays open, so an import's
/// progress and new work show.
const OVERVIEW_EVERY: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Overview,
    Status,
    Config,
    Usage,
    Context,
    Limits,
    Stats,
}

impl Tab {
    pub const ALL: [Tab; 7] = [
        Tab::Overview,
        Tab::Status,
        Tab::Config,
        Tab::Usage,
        Tab::Context,
        Tab::Limits,
        Tab::Stats,
    ];

    /// Where a mod file names this tab. Config shows no mods.
    fn mod_place(self) -> Option<ModPlace> {
        Some(match self {
            Tab::Overview => ModPlace::Overview,
            Tab::Status => ModPlace::Status,
            Tab::Config => return None,
            Tab::Usage => ModPlace::Usage,
            Tab::Context => ModPlace::Context,
            Tab::Limits => ModPlace::Limits,
            Tab::Stats => ModPlace::Stats,
        })
    }

    pub fn title(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Status => "Status",
            Tab::Config => "Config",
            Tab::Usage => "Usage",
            Tab::Context => "Context",
            Tab::Limits => "Limits",
            Tab::Stats => "Stats",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Tab::Overview => "Where the crew stands: plan usage, spend and workflows",
            Tab::Status => "What runs here and how it is set up, for every CLI at once",
            Tab::Config => "Claude Code's settings and Argonauts Code's own",
            Tab::Usage => "Plan usage, spend and tokens across the crew",
            Tab::Context => "What fills each session's context",
            Tab::Limits => "What counts toward the limits you set",
            Tab::Stats => "Each CLI and model by itself, and all together",
        }
    }

    /// The tab that shows the mod `name` at full size: the run's on Status, the context's on
    /// Context, the run's spend on Limits, and every usage mod's on Usage.
    pub fn showing(name: ModName) -> Tab {
        match name {
            ModName::Run | ModName::Waiting | ModName::Recent => Tab::Status,
            ModName::Context | ModName::Sessions => Tab::Context,
            ModName::RunLimits | ModName::Tasks | ModName::Limits => Tab::Limits,
            ModName::Stats => Tab::Stats,
            _ => Tab::Usage,
        }
    }

    /// The tab `by` places along, wrapping at either end.
    fn step(self, by: isize) -> Tab {
        let at = Tab::ALL.iter().position(|tab| *tab == self).unwrap_or(0);
        let count = Tab::ALL.len() as isize;
        Tab::ALL[(at as isize + by).rem_euclid(count) as usize]
    }

    /// It draws the usage cards, so the period and the pin keys work on it.
    fn shows_usage(self) -> bool {
        !matches!(self, Tab::Status | Tab::Config | Tab::Context)
    }

    /// The cards it stacks, in order, each in its form.
    fn cards(self) -> &'static [(UsageCard, Form)] {
        use UsageCard::*;
        match self {
            Tab::Usage => &[(PlanUsage, Form::Full)],
            Tab::Limits => &[(Spend, Form::Full), (PlanUsage, Form::Full)],
            Tab::Overview | Tab::Status | Tab::Config | Tab::Context | Tab::Stats => &[],
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
    /// Where the keys are: on the tabs as the panel opens, as in Claude Code.
    level: Level,
    period: UsagePeriod,
    /// `u`: plan usage as what's left of a window.
    left: bool,
    /// The card `p` and `,` act on.
    focus: usize,
    /// The first card shown, when the cards are taller than the panel.
    scroll: usize,
    /// Which block holds the card in focus, as the tab last drew it.
    focus_block: Cell<usize>,
    /// The blocks the last frame had, and how many of them it showed from the scroll on, for
    /// the page keys and the Usage tab's stops.
    drawn: Cell<(usize, usize)>,
    /// What the next frame scrolls into view, after a key moved to it.
    follow: Option<Follow>,
    status: Option<StatusSummary>,
    /// What the Status tab draws above the run, as the panel's own read last answered.
    setup: Option<StatusTabView>,
    context: Option<ContextData>,
    /// Where the Context tab is scrolled to, the block its keys are on, the lists open, and
    /// the crew agent whose own block shows.
    pane: ContextPane,
    /// Fixed places to show paths from, for tests. Otherwise the conversation's folder and
    /// your home.
    places: Option<Places>,
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
    /// The Usage tab's sub-tab: 0 for every CLI, else one CLI's page.
    cli: usize,
    /// How the Overview tab shows every CLI's history.
    home: HomeTab,
    /// The Config tab's rows, search and changes.
    config: ConfigTab,
    /// Where the Behaviors map drew last, while it shows.
    map_drawn: Option<MapCells>,
}

impl OverviewView {
    pub fn new(tab: Tab, watch: UsageWatch) -> Self {
        Self {
            tab,
            level: Level::Tabs,
            period: UsagePeriod::OPENS_ON,
            left: false,
            focus: 0,
            scroll: 0,
            focus_block: Cell::new(0),
            drawn: Cell::new((0, 0)),
            follow: None,
            status: None,
            setup: None,
            context: None,
            pane: ContextPane::default(),
            places: None,
            task: 0,
            limit: 0,
            watch,
            now: None,
            stats: StatsState::default(),
            stats_period_from_file: false,
            cli: 0,
            home: HomeTab::default(),
            config: ConfigTab::default(),
            map_drawn: None,
        }
    }

    /// The Config tab as `/config` opens it, with its search box taking the keys.
    pub fn with_config(mut self, config: ConfigTab) -> Self {
        self.config = config;
        self.set_level(Level::Cards);
        self
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

    /// The Status tab's own read, with the run it sums up.
    pub fn with_status_tab(mut self, view: StatusTabView) -> Self {
        self.status = view.run.clone();
        self.setup = Some(view);
        self
    }

    pub fn with_context(mut self, context: ContextData) -> Self {
        self.context = Some(context);
        self
    }

    /// Every list of the Context tab shows a line each, as `/context all` asks.
    pub fn with_every_list(mut self) -> Self {
        self.pane.open_every_list();
        self
    }

    /// Shows the Context tab's paths from `places`, wherever the test runs.
    #[cfg(test)]
    pub(crate) fn showing_paths_from(mut self, places: Places) -> Self {
        self.places = Some(places);
        self
    }

    /// The mouse rests at `place` on the Behaviors map, in the map's own units, or has left
    /// it. Whether the dot it is nearest changed.
    pub fn point_at_map(&mut self, place: Option<(f32, f32)>) -> bool {
        self.tab == Tab::Overview
            && self.home.on_behaviors()
            && self.home.behaviors.map_shown()
            && self.home.behaviors.point_at(place)
    }

    /// A click on the Behaviors map opens the card of the dot nearest the mouse.
    pub fn click_map(&mut self) -> bool {
        self.tab == Tab::Overview && self.home.on_behaviors() && self.home.behaviors.click()
    }

    /// Where the Behaviors map's cells are among the body's rows, when the frame shows them
    /// whole: `heights` are the blocks', of which `shown` show from the scroll on, in `room`
    /// rows `width` cells wide.
    fn map_drawn(
        &self,
        kit: &Kit<'_>,
        ctx: &ViewCtx<'_>,
        heights: &[usize],
        shown: usize,
        (width, room): (usize, usize),
    ) -> Option<MapCells> {
        let tab = &self.home.behaviors;
        if self.tab != Tab::Overview || !self.home.on_behaviors() || !tab.map_shown() {
            return None;
        }
        let block = behaviors::MAP_BLOCK;
        if self.scroll > block || self.scroll + shown <= block {
            return None;
        }
        // The blocks are one blank row apart.
        let top: usize = heights[self.scroll..block].iter().map(|h| h + 1).sum();
        let rows = heights.get(block).copied()?;
        if rows < behaviors::MAP_ROWS || top + behaviors::MAP_ROWS > room {
            return None;
        }
        // The tab's blocks sit one cell in, and the map one more.
        let left = 1 + behaviors::MAP_INSET;
        if left + behaviors::MAP_COLUMNS > width {
            return None;
        }
        let cells = |n: usize| u16::try_from(n).ok();
        let area = Rect::new(
            cells(left)?,
            cells(top)?,
            cells(behaviors::MAP_COLUMNS)?,
            cells(behaviors::MAP_ROWS)?,
        );
        let picture = tab
            .picture_placed
            .then(|| tab.picture_key(&self.home.behavior_colours(kit, &ctx.state.names)));
        Some(MapCells { area, picture })
    }

    /// The screen places the Behaviors map's picture over the cells the tab leaves for it.
    pub fn with_map_picture(mut self) -> Self {
        self.home.behaviors.picture_placed = true;
        self
    }

    /// Draws ages and reset times as of `now`, whenever it draws.
    pub fn at(mut self, now: OffsetDateTime) -> Self {
        self.now = Some(now);
        self.home.behaviors.clock = crate::format::WallClock::pinned(now);
        self
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    /// The row with the keys. Stats and the Overview keep it with their own rows, since their
    /// keys move it too, as a letter that regroups Stats goes to the rows.
    pub fn level(&self) -> Level {
        match self.tab {
            Tab::Stats => self.stats.level,
            Tab::Overview => self.home.level,
            _ => self.level,
        }
    }

    fn set_level(&mut self, level: Level) {
        self.level = level;
        self.stats.level = level;
        self.home.level = level;
    }

    /// Rows above the cards. Status has no row under its tabs: its cards say what it is.
    fn head_rows(&self) -> u16 {
        match self.tab {
            Tab::Status => HEAD_ROWS - 1,
            // A blank row between the tabs and the sub-tabs, as Claude Code's /stats has.
            Tab::Overview => HEAD_ROWS + 1,
            _ => HEAD_ROWS,
        }
    }

    /// The open tab's own tabs and the one chosen, when it has them.
    fn sub_tabs(&self, ctx: &ViewCtx<'_>) -> Option<(Vec<String>, usize)> {
        match self.tab {
            Tab::Stats if !self.stats.page_open() => {
                let chosen = STATS_GROUPS
                    .iter()
                    .position(|(group, _)| *group == self.stats.group)
                    .unwrap_or(0);
                let names = STATS_GROUPS
                    .iter()
                    .map(|(_, name)| (*name).to_owned())
                    .collect();
                Some((names, chosen))
            }
            Tab::Usage => self.plan_usage_tab(ctx).map(|tab| tab.sub_tabs()),
            Tab::Overview => Some(self.home.sub_tabs()),
            _ => None,
        }
    }

    /// Chooses the sub-tab `by` places along, wrapping, and the cards under it change at once.
    fn step_sub_tab(&mut self, by: isize, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let Some((names, chosen)) = self.sub_tabs(ctx) else {
            return ViewOutcome::Consumed;
        };
        let at = (chosen as isize + by).rem_euclid(names.len() as isize) as usize;
        match self.tab {
            Tab::Usage => {
                self.cli = at;
                self.focus = 0;
                self.scroll = 0;
                ViewOutcome::Consumed
            }
            Tab::Stats => {
                // From the cards the keys stay on the cards, on the new grouping's first row.
                let level = self.level();
                let key = self.stats.step_group(by);
                self.stats.level = level;
                self.stats_outcome(Some(key))
            }
            Tab::Overview => {
                let stepped = self.home.step_sub_tab(by);
                self.scroll = 0;
                match stepped {
                    HomeKey::Ask(request) => ViewOutcome::Request(request),
                    _ => ViewOutcome::Consumed,
                }
            }
            _ => ViewOutcome::Consumed,
        }
    }

    /// The keys go down a row: to the sub-tabs, or into the cards.
    fn go_down(&mut self, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let level = match self.level() {
            Level::Tabs if self.sub_tabs(ctx).is_some() => Level::SubTabs,
            _ => Level::Cards,
        };
        self.set_level(level);
        // On Limits the keys pick a limit, so its list comes into view.
        self.follow = Some(if self.tab == Tab::Limits {
            Follow::Last
        } else {
            Follow::Focus
        });
        ViewOutcome::Consumed
    }

    /// The keys go up a row, from the cards to the sub-tabs or the tabs.
    fn go_up(&mut self, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let level = match self.level() {
            Level::Cards if self.sub_tabs(ctx).is_some() => Level::SubTabs,
            _ => Level::Tabs,
        };
        self.set_level(level);
        ViewOutcome::Consumed
    }

    /// Tab and Shift+Tab go along the row with the keys: the tabs on the tabs row, and on a
    /// tab with sub-tabs the sub-tabs, from their row and from the cards under it.
    fn step_along(&mut self, by: isize, ctx: &ViewCtx<'_>) -> ViewOutcome {
        if self.level() == Level::Tabs || self.sub_tabs(ctx).is_none() {
            return self.open(self.tab.step(by));
        }
        self.step_sub_tab(by, ctx)
    }

    /// Enter on the sub-tabs: into the cards on the first row, which Stats opens in place.
    fn enter_first_row(&mut self, ctx: &ViewCtx<'_>) -> ViewOutcome {
        self.set_level(Level::Cards);
        self.focus = 0;
        self.follow = Some(Follow::Focus);
        match self.tab {
            Tab::Stats => {
                self.stats.picked = 0;
                self.stats_key(KeyEvent::from(KeyCode::Enter), ctx);
            }
            Tab::Overview => self.home.picked = 0,
            _ => {}
        }
        ViewOutcome::Consumed
    }

    /// The keys are on the first card or row, so ↑ goes back up to the tabs.
    fn on_first(&self, ctx: &ViewCtx<'_>) -> bool {
        match self.tab {
            Tab::Status => {
                let tasks = self.status.as_ref().map_or(0, |status| status.tasks.len());
                let on_run = self.status_focus(ctx) == Some(ModName::Run);
                self.focus == 0 && (!on_run || tasks == 0 || self.task == 0)
            }
            // Config hands ↑ back only from its search box, above its first row.
            Tab::Config => true,
            Tab::Context => self.pane.on_first(),
            Tab::Stats => self.stats.picked == 0,
            Tab::Overview => self.home.on_first(),
            Tab::Limits
                if self
                    .usage(ctx)
                    .is_some_and(|usage| !usage.limits.is_empty()) =>
            {
                self.limit == 0
            }
            _ => self.focus == 0,
        }
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

    /// Every model's tokens in all, which rank a CLI's models for the shade each draws in on
    /// every tab: the list the usage numbers carry, whatever their period, or else the one
    /// the Overview's numbers carry, which is the same list. Empty before either has come.
    fn ranked<'a>(&'a self, ctx: &'a ViewCtx<'_>) -> &'a [ModelTokens] {
        let held = ctx.state.usage.as_deref();
        held.map(|usage| usage.all_time_models.as_slice())
            .filter(|ranked| !ranked.is_empty())
            .or_else(|| self.home.all_time_models())
            .unwrap_or_default()
    }

    /// The tab's cards, top to bottom. A note under them is a block too, and so are your mods
    /// from `mods/` that show on the tab, under its own cards.
    fn blocks(&self, kit: &Kit<'_>, ctx: &ViewCtx<'_>, width: usize) -> Vec<Drawn> {
        let mut blocks = match self.tab {
            Tab::Status => self.status_blocks(kit, ctx, width),
            Tab::Context => {
                // Your mods for the tab are among its blocks already.
                return context::blocks(self.context_sections(kit, ctx, width));
            }
            Tab::Stats => self.stats_blocks(kit, ctx, width),
            Tab::Usage => self.plan_usage_blocks(kit, ctx, width),
            Tab::Config => return Vec::new(),
            // Inside the panel's one-column inset, as every other tab's cards are.
            Tab::Overview => {
                let (mut blocks, focus) =
                    self.home
                        .blocks(kit, &ctx.state.names, width.saturating_sub(1));
                for block in &mut blocks {
                    block.indent(1);
                }
                self.focus_block.set(focus);
                blocks
            }
            _ => self.usage_blocks(kit, ctx, width),
        };
        if let (Some(view), Some(place)) = (ctx.state.mods.as_ref(), self.tab.mod_place()) {
            let yours = crate::mods::on_tab(kit, view, place, width);
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
        let (blocks, focus) = self.stats.blocks(
            kit,
            usage,
            (&ctx.state.names, self.wall()),
            width,
            self.home.every_day_tokens(),
        );
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
            self.stats.pick_wanted(usage, &ctx.state.names, self.wall());
        }
    }

    /// How many of the tab's cards Tab steps through.
    fn card_count(&self, ctx: &ViewCtx<'_>) -> usize {
        match self.tab {
            Tab::Status => (self.setup_parts() + self.status_cards(ctx).len()).max(1),
            Tab::Context => 1,
            // A stop per card and per line above and below them, or per block of a CLI's page.
            Tab::Usage => match self.plan_usage_tab(ctx) {
                Some(tab) if self.cli == 0 => tab.stops(),
                _ => self.drawn.get().0.max(1),
            },
            _ => self.tab.cards().len().max(1),
        }
    }

    /// The Status tab's framed cards, in the order Tab steps through them, each by the mod
    /// that pins it above the input.
    fn status_cards(&self, ctx: &ViewCtx<'_>) -> Vec<ModName> {
        let Some(status) = &self.status else {
            return Vec::new();
        };
        let mut cards = vec![ModName::Run];
        if status.spend.is_some() {
            cards.push(ModName::RunLimits);
        }
        if ctx.state.usage.is_some() {
            cards.push(ModName::PlanUsage);
        }
        if !status.waiting.is_empty() {
            cards.push(ModName::Waiting);
        }
        cards
    }

    /// The Status tab's run card that has the keys, when one has.
    fn status_focus(&self, ctx: &ViewCtx<'_>) -> Option<ModName> {
        let cards = self.status_cards(ctx);
        let at = self.focus.checked_sub(self.setup_parts())?;
        cards.get(at.min(cards.len().saturating_sub(1))).copied()
    }

    /// The Status tab's cards above the run's: your CLIs, their health, the settings, and the
    /// runs.
    fn setup_parts(&self) -> usize {
        if self.setup.is_some() {
            SETUP_PARTS.len()
        } else {
            0
        }
    }

    /// Which of the cards above the run's has the keys.
    fn setup_focus(&self) -> Option<SetupPart> {
        SETUP_PARTS
            .get(self.focus)
            .copied()
            .filter(|_| self.setup.is_some())
    }

    /// Version, folder, session and mode, then your CLIs, the settings in effect and this
    /// folder's runs, each card taking its place in ↑ and ↓'s round.
    fn setup_blocks(&self, kit: &Kit<'_>, ctx: &ViewCtx<'_>, width: usize) -> Vec<Drawn> {
        let Some(view) = &self.setup else {
            return Vec::new();
        };
        let mode = ctx
            .state
            .status
            .as_ref()
            .map(|status| status.approval_mode.label());
        let cards = StatusTabCards {
            view,
            names: &ctx.state.names,
            clock: match self.now {
                Some(now) => crate::format::WallClock::pinned(now),
                None => crate::format::WallClock::local(),
            },
            mode,
        };
        let mut blocks = vec![cards.facts(kit, width)];
        for (at, part) in SETUP_PARTS.iter().enumerate() {
            let frame = Frame::view(width);
            let mut card = match part {
                SetupPart::Clis => cards.clis(kit, frame),
                SetupPart::Health => {
                    let shades = ModelColors::new(self.ranked(ctx), &ctx.state.names);
                    super::health::card(&view.health, kit, frame, &cards.clock, shades)
                }
                SetupPart::Settings => cards.settings(kit, frame),
                SetupPart::Runs => cards.runs(kit, frame),
            };
            if at == self.focus {
                self.focus_block.set(blocks.len());
                if self.level() == Level::Cards {
                    fill_title(kit, &mut card);
                }
            }
            blocks.push(card);
        }
        blocks
    }

    /// The run's card, then what it spent beside plan usage where the panel is wide enough,
    /// then what needs you, and a row of what happened last.
    fn status_blocks(&self, kit: &Kit<'_>, ctx: &ViewCtx<'_>, width: usize) -> Vec<Drawn> {
        self.focus_block.set(0);
        let setup = self.setup_blocks(kit, ctx, width);
        let first = self.setup_parts();
        let Some(summary) = &self.status else {
            if setup.is_empty() {
                return vec![note(kit, super::status_tab::NO_RUNS)];
            }
            return setup;
        };
        let title = summary.run.as_ref().and_then(|run| run_name(ctx, run));
        let cards = StatusCards {
            summary,
            names: &ctx.state.names,
            title: title.as_deref(),
        };
        if cards.is_empty() {
            if setup.is_empty() {
                return vec![note(kit, NOTHING_RUNNING)];
            }
            return setup;
        }
        // Each framed card takes the next place in ↑ and ↓'s round. The one in focus has its
        // title filled once it's drawn, never a box of its own.
        let filled = std::cell::RefCell::new(Vec::new());
        let mut place = first;
        let mut framed = |frame: Frame| {
            filled
                .borrow_mut()
                .push(place == self.focus && self.level() == Level::Cards);
            place += 1;
            frame
        };
        // The block the card in focus lands in, counted as the blocks stack up.
        let in_focus = |first: usize, cards: usize, block: usize| {
            if (first..first + cards).contains(&self.focus) {
                self.focus_block.set(block);
            }
        };
        // The arrows pick a task while the run's card has the keys.
        let tasks = summary.tasks.len();
        let picks = Picks {
            at: (self.level() == Level::Cards && self.focus == first && tasks > 0)
                .then(|| self.task.min(tasks - 1)),
            mark: Marks::for_theme(ctx.theme).selected,
        };
        let shape = RunShape {
            picks: Some(picks),
            ..RunShape::WHOLE
        };
        // The card just framed, with its title filled when it has the keys.
        let fill = |card: Option<Drawn>| {
            card.map(|mut card| {
                if filled.borrow().last() == Some(&true) {
                    fill_title(kit, &mut card);
                }
                card
            })
        };
        let mut blocks = setup;
        if self.focus == first {
            self.focus_block.set(blocks.len());
        }
        blocks.extend(fill(Some(cards.run(
            kit,
            framed(Frame::view(width)),
            shape,
        ))));
        let plan = ctx.state.usage.as_deref().map(|usage| UsageCards {
            usage,
            names: &ctx.state.names,
            now: self.wall(),
            left: self.left,
        });
        let plan_card =
            |frame| plan.map(|plan| plan.draw(kit, UsageCard::PlanUsage, Form::Compact, frame));
        let mut next = first + 1;
        if summary.spend.is_some() && plan.is_some() && width >= SIDE_BY_SIDE_FROM {
            let (left, right, at) = halves(width);
            let spend = fill(cards.spend(kit, framed(left)));
            let plan = fill(plan_card(framed(right)));
            if let (Some(spend), Some(plan)) = (spend, plan) {
                in_focus(next, 2, blocks.len());
                blocks.push(side_by_side(spend, plan, at));
            }
            next += 2;
        } else {
            if summary.spend.is_some() {
                in_focus(next, 1, blocks.len());
                next += 1;
                blocks.extend(fill(cards.spend(kit, framed(Frame::view(width)))));
            }
            if plan.is_some() {
                in_focus(next, 1, blocks.len());
                next += 1;
                blocks.extend(fill(plan_card(framed(Frame::view(width)))));
            }
        }
        if !summary.waiting.is_empty() {
            in_focus(next, 1, blocks.len());
            blocks.extend(fill(cards.needs_you(kit, framed(Frame::view(width)))));
        }
        blocks.extend(cards.recent(kit, width));
        blocks
    }

    /// The Usage tab: a card per CLI, or the picked CLI's page.
    fn plan_usage_blocks(&self, kit: &Kit<'_>, ctx: &ViewCtx<'_>, width: usize) -> Vec<Drawn> {
        let Some(tab) = self.plan_usage_tab(ctx) else {
            self.focus_block.set(0);
            return vec![note(kit, "Reading usage…")];
        };
        // The cards leave the last column blank, as every other tab's do.
        let width = width.saturating_sub(1);
        let blocks = tab.blocks(kit, width);
        let block = tab.block_of(self.focus, width);
        self.focus_block
            .set(block.min(blocks.len().saturating_sub(1)));
        blocks
    }

    /// Plan usage is read live and holds for any period, so any numbers will do.
    fn plan_usage_tab<'a>(&self, ctx: &'a ViewCtx<'_>) -> Option<UsageTab<'a>> {
        let now = self.wall();
        let talk = &ctx.state.talk;
        Some(UsageTab {
            usage: ctx.state.usage.as_deref()?,
            now,
            cli: self.cli,
            left: self.left,
            talked: talk
                .started
                .map(|started| ((now - started).unsigned_abs(), talk.worked)),
            changed: (talk.added, talk.removed),
            focus: (self.level() == Level::Cards && self.cli == 0).then_some(self.focus),
        })
    }

    /// The Context tab's blocks: the main session as its CLI counted it, each run's crew, and
    /// then your mods for the tab. With a crew agent opened, its own block alone.
    fn context_sections(&self, kit: &Kit<'_>, ctx: &ViewCtx<'_>, width: usize) -> Vec<Section> {
        let Some(view) = &self.context else {
            return vec![Section::of(note(kit, "Reading context…"))];
        };
        let state = ctx.state;
        let places = match &self.places {
            Some(places) => places.clone(),
            None => Places::of_conversation(state),
        };
        let run_title = |run: &RunId| {
            let known = state.runs.get(run).map(|run| run.title.trim().to_owned());
            known
                .filter(|title| !title.is_empty())
                .or_else(|| run_name(ctx, run))
                .unwrap_or_else(|| run.to_string())
        };
        let facts = context::CrewFacts::of(state, view);
        let tab = ContextTab {
            view,
            names: &state.names,
            count: state.conversation.context.as_ref(),
            main: state.conversation.worker.as_ref(),
            places: &places,
            now: self.wall(),
            run_title: &run_title,
            facts: &facts,
            pane: &self.pane,
        };
        if self.pane.opened().is_some() {
            return tab.sections(kit, width);
        }
        let mut sections = tab.sections(kit, width);
        if let (Some(view), Some(place)) = (state.mods.as_ref(), self.tab.mod_place()) {
            let yours = crate::mods::on_tab(kit, view, place, width);
            if !yours.lines.is_empty() {
                sections.push(Section::of(yours));
            }
        }
        sections
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
            let mut drawn = cards.draw(kit, card, form, frame);
            if at == focus && self.level() == Level::Cards {
                fill_title(kit, &mut drawn);
            }
            drawn
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
    fn fitted(blocks: Vec<Drawn>, first: usize, room: usize) -> (Drawn, usize) {
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
        let count = shown.len();
        (stack_cards(shown), count)
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
        // The panel's title in bold, as Claude Code's Settings, then every tab but the open one
        // in plain text, all alike, so none looks picked.
        let mut left = vec![Span::styled(" Settings", kit.bold(Role::Text))];
        for tab in Tab::ALL {
            left.push(Span::raw(TAB_GAP));
            // Config's search box and rows belong to its tab, so it stays lit while they
            // have the keys.
            let lit = self.level() == Level::Tabs || tab == Tab::Config;
            left.push(if tab == self.tab {
                chosen(kit, tab.title(), lit)
            } else {
                Span::raw(format!(" {} ", tab.title()))
            });
        }
        let mut right = Vec::new();
        if self.tab.shows_usage() && self.tab != Tab::Overview {
            // A cell between the last tab and the periods, however narrow the screen.
            left.push(Span::raw(" "));
            for (period, word) in periods::PERIODS {
                right.push(if period == self.period {
                    Span::styled(word, kit.bold(Role::Question))
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

    /// The open tab's own tabs, the chosen one filled while Tab moves them.
    fn sub_tab_spans(&self, kit: &Kit<'_>, ctx: &ViewCtx<'_>) -> Vec<Span<'static>> {
        let Some((names, at)) = self.sub_tabs(ctx) else {
            return Vec::new();
        };
        let mut spans = Vec::new();
        for (index, name) in names.into_iter().enumerate() {
            if index > 0 {
                spans.push(Span::raw(TAB_GAP));
            }
            spans.push(if index == at {
                chosen_sub_tab(kit, &name, self.level().moves_sub_tabs())
            } else {
                kit.dim(format!(" {name} "))
            });
        }
        spans
    }

    /// What the tab is about, and at the right what the period came to.
    fn about(&self, kit: &Kit<'_>, ctx: &ViewCtx<'_>, width: usize) -> Vec<Span<'static>> {
        if self.tab == Tab::Stats
            && let Some(page) = &self.stats.page
        {
            return page.about(kit, width);
        }
        if self.tab == Tab::Usage
            && let Some(tab) = self.plan_usage_tab(ctx)
        {
            let mut row = vec![Span::raw(" ")];
            row.extend(self.sub_tab_spans(kit, ctx));
            // When the plans were read, at the right where the periods were before.
            let read = vec![kit.dim(tab.read_words()), Span::raw(" ")];
            if spans_width(&row) + spans_width(&read) + 2 > width {
                return truncate_spans(&row, width);
            }
            return spread(row, read, width);
        }
        if self.tab == Tab::Overview {
            let mut row = vec![Span::raw(" ")];
            row.extend(self.home.about(kit, &ctx.state.names, width - 1));
            return row;
        }
        if self.tab == Tab::Stats {
            let left = vec![kit.dim(format!(" {}", self.stats.description()))];
            let mut right = self.sub_tab_spans(kit, ctx);
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
                Some(match &status.spend {
                    Some(limit) => format!("{run} · {}", format::spend(&limit.spent)),
                    None => run,
                })
            }
            Tab::Context => Some(context::summary(&self.context.as_ref()?.rows)),
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
    fn keys(&self, ctx: &ViewCtx<'_>, more: bool, width: usize) -> String {
        let running = matches!(
            ctx.state.mode,
            Some(AppMode::Answering { .. } | AppMode::Working { .. } | AppMode::Pausing { .. })
        );
        let close = if running { PANEL_HINT } else { CLOSES };
        if self.tab == Tab::Overview && self.home.on_behaviors() && self.level() == Level::Cards {
            let mut keys = self.home.behaviors.keys(width);
            let last = keys.pop().unwrap_or_default();
            return fit_keys(keys, &last, width);
        }
        let along = self.tab_words(ctx);
        let has_sub_tabs = self.sub_tabs(ctx).is_some();
        let rows: Vec<String> = match self.level() {
            // Iosevka draws the side arrows two cells wide, so each has two blanks after it.
            Level::Tabs if self.tab == Tab::Context => vec![
                "tab · ←  →  tabs".to_owned(),
                "↓  into the list".to_owned(),
                "enter opens one".to_owned(),
            ],
            Level::Tabs => vec![
                along.clone(),
                if has_sub_tabs {
                    "↓  sub-tabs, then cards".to_owned()
                } else {
                    "↓  into the cards".to_owned()
                },
            ],
            Level::SubTabs => vec![
                along.clone(),
                "↓  into the cards".to_owned(),
                "↑  tabs".to_owned(),
            ],
            Level::Cards => vec![],
        };
        let owned = |keys: Vec<&str>| keys.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let mut close = close;
        let mut keys = if self.tab == Tab::Stats && self.stats.page_open() {
            close = "esc back";
            self.stats
                .page
                .as_ref()
                .map(|page| owned(page.keys()))
                .unwrap_or_default()
        } else if self.tab == Tab::Overview {
            owned(self.home.keys())
        } else if self.tab == Tab::Stats {
            owned(self.stats.keys())
        } else {
            rows
        };
        // The period's hint comes first, before the word that more waits below.
        let with_hint = |keys: Vec<String>| {
            if self.has_periods() {
                periods::hint_first(keys)
            } else {
                keys
            }
        };
        if self.tab == Tab::Usage {
            let mut keys = match self.level() {
                Level::Tabs => vec![along, "↓  CLIs".to_owned()],
                Level::SubTabs => vec![along, "↓  into the cards".to_owned(), "↑  tabs".to_owned()],
                Level::Cards if self.cli > 0 => vec![
                    "↑↓  move".to_owned(),
                    "enter closes the page".to_owned(),
                    along,
                ],
                Level::Cards => vec![
                    "↑↓  card".to_owned(),
                    "enter opens its page".to_owned(),
                    along,
                ],
            };
            keys.push("ctrl+r reads again".to_owned());
            if more {
                keys.insert(0, "↓  more".to_owned());
            }
            return fit_keys(with_hint(keys), close, width);
        }
        // In the cards ↑ and ↓ move a card or a row at a time, and the tab scrolls with them.
        let in_cards = self.level() == Level::Cards;
        let words = |list: &[&str]| list.iter().map(|key| (*key).to_owned()).collect::<Vec<_>>();
        match self.tab {
            Tab::Stats | Tab::Overview => {}
            Tab::Status => {
                let tasks = self.status.as_ref().map_or(0, |status| status.tasks.len());
                let has_runs = self
                    .setup
                    .as_ref()
                    .is_some_and(|view| !view.runs.is_empty());
                let on_run = self.status_focus(ctx) == Some(ModName::Run);
                let list: &[&str] = match (in_cards, self.setup_focus()) {
                    (false, _) => &[],
                    (true, Some(SetupPart::Settings)) => &["↑↓  card", "enter all settings"],
                    (true, Some(SetupPart::Runs)) if has_runs => &["↑↓  card", "enter workflow"],
                    (true, Some(_)) => &["↑↓  card"],
                    (true, None) if on_run && tasks > 0 => &["↑↓  task", "enter agent"],
                    (_, None) => &["enter workflow"],
                };
                keys.extend(words(list));
                if in_cards {
                    keys.push(along);
                }
                if self.setup_focus().is_none() {
                    keys.extend(words(&[
                        "p pin above the input",
                        "s ask the narrator",
                        "u used or left",
                    ]));
                }
            }
            Tab::Context => {
                // An agent's own block has one way out, back to the list.
                if self.pane.opened().is_some() {
                    let keys = if more {
                        vec!["↓  more".to_owned()]
                    } else {
                        Vec::new()
                    };
                    return fit_keys(keys, "esc back to the list", width);
                }
                if in_cards {
                    keys.push("↑↓  move".to_owned());
                    let enter = match self.pane.target() {
                        Some(context::Target::List(list)) if self.pane.is_open(&list) => {
                            Some("enter folds it")
                        }
                        Some(context::Target::List(_)) => Some("enter opens it"),
                        Some(context::Target::Agent { .. }) => Some("enter its own block"),
                        None => None,
                    };
                    keys.extend(enter.map(str::to_owned));
                    keys.push(along);
                }
            }
            _ => {
                if in_cards {
                    keys.push(if self.tab == Tab::Limits {
                        "↑↓  limit".to_owned()
                    } else {
                        "↑↓  card".to_owned()
                    });
                    if self.tab == Tab::Limits {
                        keys.push("enter change".to_owned());
                    }
                    keys.push(along);
                }
                keys.extend(words(&[
                    "p pin above the input",
                    "u used or left",
                    ", settings",
                ]));
            }
        }
        // On the Context tab's tabs, ↓ already says it goes into the list.
        let down_said = self.tab == Tab::Context && self.level() == Level::Tabs;
        if more && !down_said {
            keys.insert(0, "↓  more".to_owned());
        }
        fit_keys(with_hint(keys), close, width)
    }

    /// What Tab does on the row with the keys: `tab · ← → tabs` on the tabs, and from the
    /// sub-tabs down the sub-tabs it goes along, such as `tab By CLI, By model, By role`.
    fn tab_words(&self, ctx: &ViewCtx<'_>) -> String {
        let sub_tabs = self.sub_tabs(ctx).map(|(names, _)| names);
        match (self.level(), sub_tabs) {
            (Level::Tabs, _) => "tab · ←  →  tabs".to_owned(),
            (Level::SubTabs, Some(names)) => {
                format!("tab · ←  →  {}", along_words(self.tab, &names))
            }
            (Level::Cards, Some(names)) => format!("tab {}", along_words(self.tab, &names)),
            (_, None) => "tab tabs".to_owned(),
        }
    }

    /// The Behaviors map, beside Overview and Models, as Config's Behaviors rows open it. The
    /// map is asked for at once; the Overview sub-tab's numbers come with its first refresh.
    fn open_behaviors(&mut self) -> ViewOutcome {
        let request = self.home.show_behaviors();
        let _overview = self.open(Tab::Overview);
        ViewOutcome::Request(request)
    }

    /// Opens `tab`, asking for what it shows when the screen doesn't hold it. The keys go to
    /// the tabs, where they moved.
    fn open(&mut self, tab: Tab) -> ViewOutcome {
        self.set_level(Level::Tabs);
        if tab == self.tab {
            return ViewOutcome::Consumed;
        }
        self.tab = tab;
        self.focus = 0;
        self.scroll = 0;
        // Another tab leaves the agent opened on Context.
        self.pane.back();
        match tab {
            Tab::Status => ViewOutcome::Request(ViewRequest::StatusTab),
            Tab::Config => ViewOutcome::Request(ViewRequest::Settings { section: None }),
            Tab::Context => ViewOutcome::Request(ViewRequest::Context),
            Tab::Usage => ViewOutcome::Request(ViewRequest::ReadPlanUsage),
            Tab::Overview => ViewOutcome::Request(self.home.request(None)),
            // Stats opens on the period the stats mod's file sets, whichever the tab before
            // it showed.
            Tab::Stats => {
                self.stats_period_from_file = true;
                ViewOutcome::Consumed
            }
            _ => ViewOutcome::Consumed,
        }
    }

    /// The period's keys, from any row of a tab that has periods: `r` cycles to the next and
    /// Shift+R to the one before, through the Overview's own three or through Today, Week,
    /// Month and All, which 1 to 4 also pick.
    fn period_key(&mut self, key: KeyEvent) -> Option<ViewOutcome> {
        let request = match self.tab {
            // Its import question answers to 1 and 2 while it shows.
            Tab::Overview if !self.home.asking() => {
                match self.home.step_period(periods::step(key)?) {
                    HomeKey::Ask(request) => request,
                    _ => return Some(ViewOutcome::Consumed),
                }
            }
            Tab::Usage | Tab::Limits | Tab::Stats => {
                self.period = periods::asked(key, self.period)?;
                ViewRequest::Usage {
                    period: self.period,
                }
            }
            _ => return None,
        };
        self.scroll = 0;
        Some(ViewOutcome::Request(request))
    }

    /// The tab's key line starts with the period's hint.
    fn has_periods(&self) -> bool {
        match self.tab {
            Tab::Overview => !self.home.asking() && !self.home.on_behaviors(),
            Tab::Stats => !self.stats.page_open(),
            Tab::Usage | Tab::Limits => true,
            Tab::Status | Tab::Config | Tab::Context => false,
        }
    }

    /// The Usage tab's own keys: in the cards Enter opens the CLI whose card has the keys, or
    /// the one that says what made up most of the usage, on its own page; Enter on the page
    /// closes it, back to its card. `None` leaves the key to the panel.
    fn plan_usage_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> Option<ViewOutcome> {
        match key.code {
            KeyCode::Enter if self.level() == Level::Cards => {
                let tab = self.plan_usage_tab(ctx)?;
                if self.cli > 0 {
                    // The CLI's card is the stop after the conversation's, in the same order.
                    self.focus = self.cli;
                    self.cli = 0;
                } else {
                    let usage = tab.usage;
                    let says_most = || {
                        usage
                            .plans
                            .iter()
                            .position(|plan| {
                                plan.live
                                    .as_ref()
                                    .is_some_and(|live| live.behaviors.is_some())
                            })
                            .unwrap_or(0)
                    };
                    match tab.stop(self.focus) {
                        Stop::Conversation => return Some(ViewOutcome::Consumed),
                        Stop::Card(plan) => self.cli = plan + 1,
                        Stop::MostUsage => self.cli = says_most() + 1,
                    }
                    self.focus = 0;
                }
            }
            _ => return None,
        }
        self.scroll = 0;
        self.follow = Some(Follow::Focus);
        Some(ViewOutcome::Consumed)
    }

    fn focused(&self) -> Option<UsageCard> {
        let cards = self.tab.cards();
        cards
            .get(self.focus.min(cards.len().saturating_sub(1)))
            .map(|(card, _)| *card)
    }
}

impl MapView for OverviewView {
    fn place_pictures(&mut self, yes: bool) {
        self.home.behaviors.picture_placed = yes;
    }

    fn map_cells(&self) -> Option<MapCells> {
        self.map_drawn
    }

    fn picture(&self, cell: (u16, u16), ctx: &ViewCtx<'_>) -> Option<Vec<u8>> {
        let kit = Kit::new(ctx.theme);
        let colours = self.home.behavior_colours(&kit, &ctx.state.names);
        self.home.behaviors.picture(&colours, cell)
    }

    fn point(&mut self, at: Option<MapPoint>) -> bool {
        let picture = self.map_drawn.is_some_and(|cells| cells.picture.is_some());
        let place = at.map(|at| BehaviorsTab::place_of(at, picture));
        self.point_at_map(place)
    }

    fn click(&mut self) -> bool {
        self.click_map()
    }
}

impl View for OverviewView {
    fn docked(&self) -> bool {
        true
    }

    fn map(&mut self) -> Option<&mut dyn MapView> {
        Some(self)
    }

    /// A change Config sent may ask whether to change a safety setting, which shows under the
    /// panel, so the tab is still there once it's answered.
    fn stays_over_question(&self) -> bool {
        self.tab == Tab::Config && self.config.waits()
    }

    /// The rows its cards take, so a short tab leaves the conversation its room.
    fn dock_height(&mut self, screen: Size, ctx: &ViewCtx<'_>) -> Option<u16> {
        if self.tab == Tab::Config {
            // The rule and the tabs, then the tab's own rows.
            let wanted = 2 + config::body_rows(self.config.wanted(ctx.state));
            return Some(wanted.min(screen.height.saturating_sub(6)).max(12));
        }
        self.settle_stats(ctx);
        // The working line and a few rows of the conversation stay above it.
        let most = screen.height.saturating_sub(6);
        // The Behaviors list takes as many rows as it's given, so it's measured with every row
        // the panel could have, or the panel would never grow past the rows it last had.
        let head_and_foot = self.head_rows() + FOOT_ROWS;
        self.home
            .set_room(usize::from(most.saturating_sub(head_and_foot)));
        let kit = Kit::new(ctx.theme);
        let blocks = self.blocks(&kit, ctx, usize::from(screen.width));
        let body = stack_cards(blocks).height();
        let wanted = u16::try_from(body)
            .unwrap_or(u16::MAX)
            .saturating_add(head_and_foot);
        Some(wanted.min(most).max(head_and_foot + 1))
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let head = self.head_rows();
        if area.height < head || area.width == 0 {
            return;
        }
        if self.tab == Tab::Config {
            self.render_config(area, buf, ctx);
            return;
        }
        self.settle_stats(ctx);
        self.home.behaviors.deep.sync(&ctx.state.deep);
        let kit = Kit::new(ctx.theme);
        let width = usize::from(area.width);
        let rule = Line::from(marks::decorative(
            ctx.theme.glyphs.rule.repeat(width),
            ctx.theme.style(Role::Question),
        ));
        buf.set_line(area.x, area.y, &rule, area.width);
        buf.set_line(
            area.x,
            area.y + 1,
            &Line::from(self.tabs(&kit, width)),
            area.width,
        );
        if head >= HEAD_ROWS {
            let about = Line::from(self.about(&kit, ctx, width));
            let at = area.y + head - 2;
            buf.set_line(area.x, at, &about, area.width);
        }
        self.keep_fresh(ctx);
        let room = usize::from(area.height.saturating_sub(head + FOOT_ROWS));
        if self.tab == Tab::Context {
            self.render_context(area, buf, ctx, room);
            return;
        }
        self.home.set_room(room);
        let blocks = self.blocks(&kit, ctx, width);
        let heights: Vec<usize> = blocks.iter().map(Drawn::height).collect();
        let count = blocks.len();
        self.scroll = self.scroll.min(count.saturating_sub(1));
        if let Some(follow) = self.follow.take() {
            let wanted = match follow {
                Follow::Focus => self.focus_block.get(),
                Follow::Last => count.saturating_sub(1),
            };
            self.scroll = Self::scrolled_to(&blocks, self.scroll, wanted, room);
        }
        let (body, shown) = Self::fitted(blocks, self.scroll, room);
        self.drawn.set((count, shown));
        self.map_drawn = self
            .map_drawn(&kit, ctx, &heights, shown, (width, room))
            .map(|mut cells| {
                cells.area.x += area.x;
                cells.area.y += area.y + head;
                cells
            });
        for (row, line) in body.lines.iter().take(room).enumerate() {
            buf.set_line(area.x, area.y + head + row as u16, line, area.width);
        }
        // More is below: the arrows or a page down reach it.
        let more = self.scroll + shown < count;
        let keys = self.keys(ctx, more, width);
        // The Overview's key line sits under its rows, two cells in, as Claude Code's does.
        let inset = if self.tab == Tab::Overview {
            "   "
        } else {
            " "
        };
        let keys = Line::from(truncate_spans(&[kit.dim(format!("{inset}{keys}"))], width));
        buf.set_line(area.x, area.bottom() - 1, &keys, area.width);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let ctrl = key.modifiers == KeyModifiers::CONTROL;
        if ctrl && self.tab == Tab::Overview && key.code == KeyCode::Char('s') {
            return self.home_key(key, ctx);
        }
        // `r` alone cycles the period, so reading the plans again takes Ctrl.
        if ctrl && self.tab == Tab::Usage && key.code == KeyCode::Char('r') {
            return ViewOutcome::Request(ViewRequest::ReadPlanUsage);
        }
        // Config's list moves with ctrl+n and ctrl+p, as Claude Code's Settings list does.
        let moves = ctrl && matches!(key.code, KeyCode::Char('n' | 'p'));
        if moves
            && self.tab == Tab::Config
            && self.level() != Level::Tabs
            && let Some(outcome) = self.config.key(key, ctx.state)
        {
            return outcome;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return ViewOutcome::Ignored;
        }
        // The stats mod's settings page is a dialog of its own, where Esc leaves the page.
        if self.tab == Tab::Stats && self.stats.page_open() {
            return self.stats_page_key(key, ctx);
        }
        // Config's search box takes every letter, so vim's keys stay letters there.
        let in_config = self.tab == Tab::Config && self.level() != Level::Tabs;
        if in_config && let Some(outcome) = self.config.key(key, ctx.state) {
            if self.config.followed_to_map() {
                return self.open_behaviors();
            }
            return outcome;
        }
        // The Behaviors sub-tab's `g` groups its map, so it reaches the sub-tab as the letter
        // and doesn't jump to the top as `g` does elsewhere in the panel.
        let groups = key.code == KeyCode::Char('g');
        if groups && self.tab == Tab::Overview && self.home.claims(key.code) {
            return self.home_key(key, ctx);
        }
        let key = panel_key(key, list::vim(ctx.state));
        // Esc goes back from a crew agent's block on the Context tab to its list.
        if key.code == KeyCode::Esc && self.tab == Tab::Context && self.pane.back() {
            return ViewOutcome::Consumed;
        }
        if self.tab == Tab::Overview && self.home.claims(key.code) {
            return self.home_key(key, ctx);
        }
        // Esc closes the panel at once, from any row, an opened row or a CLI's page included,
        // leaving what Config changed under the command.
        if key.code == KeyCode::Esc {
            return self.config.close();
        }
        match key.code {
            KeyCode::Tab => return self.step_along(1, ctx),
            KeyCode::BackTab => return self.step_along(-1, ctx),
            // A page at a time: the first block the last frame didn't show comes to the top.
            KeyCode::PageDown if self.tab != Tab::Context => {
                let (count, shown) = self.drawn.get();
                self.scroll = (self.scroll + shown.max(1)).min(count.saturating_sub(1));
                return ViewOutcome::Consumed;
            }
            KeyCode::PageUp if self.tab != Tab::Context => {
                let (_, shown) = self.drawn.get();
                self.scroll = self.scroll.saturating_sub(shown.max(1));
                return ViewOutcome::Consumed;
            }
            _ => {}
        }
        if let Some(outcome) = self.period_key(key) {
            return outcome;
        }
        if self.tab == Tab::Usage
            && let Some(outcome) = self.plan_usage_key(key, ctx)
        {
            return outcome;
        }
        // The import's question takes ↑↓ and Enter while it shows; the tabs keep ←→.
        let moves_tabs = matches!(key.code, KeyCode::Left | KeyCode::Right);
        if self.tab == Tab::Overview && self.home.asking() && !moves_tabs {
            return self.home_key(key, ctx);
        }
        match (self.level(), key.code) {
            (Level::Tabs, KeyCode::Right) => return self.open(self.tab.step(1)),
            (Level::Tabs, KeyCode::Left) => return self.open(self.tab.step(-1)),
            (Level::SubTabs, KeyCode::Right) => return self.step_sub_tab(1, ctx),
            (Level::SubTabs, KeyCode::Left) => return self.step_sub_tab(-1, ctx),
            // In the cards ←→ do nothing, except where the Overview's own rows take them and
            // where the Context tab opens a session and comes back.
            (Level::Cards, KeyCode::Left | KeyCode::Right)
                if !matches!(self.tab, Tab::Overview | Tab::Context) =>
            {
                return ViewOutcome::Consumed;
            }
            (Level::Tabs, KeyCode::Down) if self.tab == Tab::Config => {
                self.config.enter_from_tabs();
                self.set_level(Level::Cards);
                return ViewOutcome::Consumed;
            }
            (Level::Tabs | Level::SubTabs, KeyCode::Down) => return self.go_down(ctx),
            (Level::SubTabs, KeyCode::Enter) => return self.enter_first_row(ctx),
            (Level::SubTabs, KeyCode::Up) => return self.go_up(ctx),
            (Level::Tabs, KeyCode::Up | KeyCode::Enter | KeyCode::Home | KeyCode::End) => {
                return ViewOutcome::Consumed;
            }
            (Level::Cards, KeyCode::Up) if self.on_first(ctx) => return self.go_up(ctx),
            _ => {}
        }
        match self.tab {
            Tab::Status => self.status_key(key, ctx),
            Tab::Context => self.context_key(key, ctx),
            Tab::Stats => self.stats_key(key, ctx),
            Tab::Overview => self.home_key(key, ctx),
            _ => self.usage_key(key, ctx),
        }
    }

    /// A paste searches Config's rows, as typing does there.
    fn handle_paste(&mut self, text: &str, _ctx: &ViewCtx<'_>) -> ViewOutcome {
        if self.tab != Tab::Config || self.level() == Level::Tabs {
            return ViewOutcome::Ignored;
        }
        self.config.paste(text);
        ViewOutcome::Consumed
    }

    /// Status and Context answers land under their tabs. Usage numbers go on to the screen's
    /// state, where the cards above the input read them too.
    fn accept(&mut self, data: &ViewData) -> bool {
        match data {
            ViewData::Status { summary } => self.status = Some(summary.clone()),
            ViewData::StatusTab { view } => {
                self.status = view.run.clone();
                self.setup = Some((**view).clone());
            }
            ViewData::Context { view } => self.context = Some(view.clone()),
            ViewData::Overview { overview } => self.home.accept(overview),
            ViewData::Behaviors { map } => self.home.accept_map(map),
            ViewData::DeepOffer { offer } => self.home.behaviors.accept_offer(offer),
            // Every key, which the Config tab asked for when it opened.
            ViewData::Settings {
                rows,
                command: None,
                section: None,
                ..
            } if self.tab == Tab::Config => self.config.set_rows(rows.clone()),
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
    /// The Config tab: the rule in the question colour, as every tab's is, as Claude Code draws
    /// each Settings tab, then the tabs, and the tab's own rows under them.
    fn render_config(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let kit = Kit::new(ctx.theme);
        let width = usize::from(area.width);
        let rule = Line::from(marks::decorative(
            ctx.theme.glyphs.rule.repeat(width),
            ctx.theme.style(Role::Question),
        ));
        buf.set_line(area.x, area.y, &rule, area.width);
        let tabs = Line::from(self.tabs(&kit, width));
        buf.set_line(area.x, area.y + 1, &tabs, area.width);
        let body = Rect {
            y: area.y + 2,
            height: area.height.saturating_sub(2),
            ..area
        };
        let on_tabs = self.level() == Level::Tabs;
        config::draw(&mut self.config, body, buf, ctx, on_tabs);
    }

    /// Stats shows every day so far, and the Overview would answer for every day too, but
    /// hasn't yet.
    fn wants_every_day_tokens(&self) -> bool {
        self.period == UsagePeriod::All
            && self.home.period == OverviewPeriod::All
            && self.home.every_day_tokens().is_none()
    }

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
                        .drawn_tab(ViewRequest::StatusTab, STATUS_EVERY, ctx.now);
                }
            }
            // Behaviors asks for its map as it opens and as its keys change it.
            Tab::Overview if !self.home.on_behaviors() => {
                self.watch
                    .drawn_tab(self.home.request(None), OVERVIEW_EVERY, ctx.now);
            }
            // Over every day so far Stats says how the crew's tokens compare with everything
            // on this computer, which the Overview's numbers hold.
            Tab::Stats if self.wants_every_day_tokens() => {
                self.watch
                    .drawn_tab(self.home.request(None), OVERVIEW_EVERY, ctx.now);
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

    /// ↑ or ↓ to the card before or after, stopping at the last.
    fn step_focus(&mut self, back: bool, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let last = self.card_count(ctx) - 1;
        let at = self.focus.min(last);
        self.focus = if back {
            at.saturating_sub(1)
        } else {
            (at + 1).min(last)
        };
        self.follow = Some(Follow::Focus);
        ViewOutcome::Consumed
    }

    fn status_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let tasks = self.status.as_ref().map_or(0, |status| status.tasks.len());
        // On the run's card the arrows pick a task. On any other they scroll the tab.
        let on_run = self.status_focus(ctx) == Some(ModName::Run) && tasks > 0;
        match key.code {
            KeyCode::Char('s') => ViewOutcome::SendToConversation(UiCommand::Summary),
            KeyCode::Char('a') if self.lists_a_run_to_archive() => {
                ViewOutcome::SendToConversation(UiCommand::Slash {
                    command: SlashCommand::Runs { archive: None },
                })
            }
            KeyCode::Enter => match self.setup_focus() {
                // Every key with where it came from, over the panel, which Esc comes back to.
                Some(SetupPart::Settings) => {
                    ViewOutcome::Request(ViewRequest::Settings { section: None })
                }
                Some(SetupPart::Runs) => self.open_newest_run(),
                Some(SetupPart::Clis | SetupPart::Health) => ViewOutcome::Consumed,
                None => self.open_from_status(on_run, ctx),
            },
            KeyCode::Char('u') => {
                self.left = !self.left;
                ViewOutcome::Consumed
            }
            KeyCode::Char('p') => match self.status_focus(ctx) {
                Some(card) => pin(card),
                None => ViewOutcome::Ignored,
            },
            KeyCode::Up if on_run && self.task > 0 => {
                self.task = self.task.min(tasks - 1) - 1;
                ViewOutcome::Consumed
            }
            KeyCode::Down if on_run && self.task + 1 < tasks => {
                self.task += 1;
                ViewOutcome::Consumed
            }
            // On the run's card the first and last task, and from there the first and last card.
            KeyCode::Home if on_run && self.task > 0 => {
                self.task = 0;
                ViewOutcome::Consumed
            }
            KeyCode::End if on_run && self.task + 1 < tasks => {
                self.task = tasks - 1;
                ViewOutcome::Consumed
            }
            KeyCode::Home => {
                self.focus = 0;
                self.follow = Some(Follow::Focus);
                ViewOutcome::Consumed
            }
            KeyCode::End => {
                self.focus = self.card_count(ctx) - 1;
                self.follow = Some(Follow::Focus);
                ViewOutcome::Consumed
            }
            KeyCode::Up => self.step_focus(true, ctx),
            KeyCode::Down => self.step_focus(false, ctx),
            KeyCode::Char('q') => ViewOutcome::Close,
            _ => ViewOutcome::Ignored,
        }
    }

    /// Enter on the runs card: the newest run's workflow, which lists what it is doing.
    /// Whether a run of this folder was written by another build, which `a` moves out of the
    /// way.
    fn lists_a_run_to_archive(&self) -> bool {
        self.setup.as_ref().is_some_and(|view| {
            view.runs
                .iter()
                .any(|run| run.state == RunState::FromAnotherBuild)
        })
    }

    fn open_newest_run(&self) -> ViewOutcome {
        let newest = self.setup.as_ref().and_then(|view| view.runs.first());
        match newest {
            Some(run) => ViewOutcome::Request(ViewRequest::Workflow {
                run: run.run.clone(),
                workflow: None,
            }),
            None => ViewOutcome::Consumed,
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
            .tasks
            .get(self.task.min(status.tasks.len().saturating_sub(1)))
            .filter(|_| on_run);
        let agent = task.and_then(|task| {
            let sessions = ctx.state.runs.get(&run)?.sessions_in_order();
            let mut of_task: Vec<_> = sessions
                .filter(|session| session.task.as_ref() == Some(&task.task))
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

    /// The Context tab's rows as its pane scrolls them, with the pointer on the block the keys
    /// are on, and the key line under them.
    fn render_context(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>, room: usize) {
        let kit = Kit::new(ctx.theme);
        let width = usize::from(area.width);
        let sections = self.context_sections(&kit, ctx, width);
        let pointer = (self.level() == Level::Cards).then(|| Marks::for_theme(ctx.theme).selected);
        let (rows, more) = self.pane.rows(&kit, sections, room, pointer);
        let top = area.y + self.head_rows();
        for (row, line) in rows.iter().enumerate() {
            buf.set_line(area.x, top + row as u16, line, area.width);
        }
        let keys = self.keys(ctx, more, width);
        let keys = Line::from(truncate_spans(&[kit.dim(format!(" {keys}"))], width));
        buf.set_line(area.x, area.bottom() - 1, &keys, area.width);
    }

    /// In the cards ↑ and ↓ step through the tab's blocks, Enter folds or opens the list the
    /// keys are on or opens the agent's own block, and ← comes back from it as Esc does. A page
    /// at a time works from the tabs too.
    fn context_key(&mut self, key: KeyEvent, _ctx: &ViewCtx<'_>) -> ViewOutcome {
        match key.code {
            KeyCode::Enter => {
                self.pane.enter();
                ViewOutcome::Consumed
            }
            KeyCode::Left => {
                self.pane.back();
                ViewOutcome::Consumed
            }
            KeyCode::Right => ViewOutcome::Consumed,
            KeyCode::Char('q') => ViewOutcome::Close,
            code if self.pane.key(code) => ViewOutcome::Consumed,
            _ => ViewOutcome::Ignored,
        }
    }

    /// The Overview tab's keys. Enter on a model opens it in Stats, for the same days.
    fn home_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let ctrl = key.modifiers == KeyModifiers::CONTROL;
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        let names = &ctx.state.names;
        self.home.behaviors.deep.sync(&ctx.state.deep);
        match self.home.key(key.code, ctrl, names) {
            Some(HomeKey::Moved) => {
                self.follow = Some(Follow::Focus);
                ViewOutcome::Consumed
            }
            Some(HomeKey::Ask(request)) => {
                self.scroll = 0;
                ViewOutcome::Request(request)
            }
            Some(HomeKey::Copy(text)) => ViewOutcome::Copy(text),
            Some(HomeKey::Send(command)) => ViewOutcome::Send(command),
            Some(HomeKey::OpenModel {
                agent,
                model,
                period,
            }) => {
                self.tab = Tab::Stats;
                self.scroll = 0;
                self.follow = Some(Follow::Focus);
                self.stats.open_model(agent, model);
                self.period = period;
                ViewOutcome::Request(ViewRequest::Usage { period })
            }
            None if key.code == KeyCode::Char('q') => ViewOutcome::Close,
            None => ViewOutcome::Ignored,
        }
    }

    fn stats_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
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
            Some(StatsKey::Pin) => pin(ModName::Stats),
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
                return ViewOutcome::SendToConversation(UiCommand::Slash {
                    command: SlashCommand::Limits {
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
                    command: SlashCommand::Mods {
                        action: ModsAction::Page {
                            the_mod: card.name(),
                        },
                        scope: None,
                    },
                }),
                None => ViewOutcome::Ignored,
            },
            KeyCode::Up => self.step_focus(true, ctx),
            KeyCode::Down => self.step_focus(false, ctx),
            KeyCode::Home => {
                self.focus = 0;
                self.follow = Some(Follow::Focus);
                ViewOutcome::Consumed
            }
            KeyCode::End => {
                self.focus = self.card_count(ctx) - 1;
                self.follow = Some(Follow::Focus);
                ViewOutcome::Consumed
            }
            // With no list to pick from, Enter opens every limit in their own picker.
            KeyCode::Enter if self.tab == Tab::Limits => {
                ViewOutcome::SendToConversation(UiCommand::Slash {
                    command: SlashCommand::Limits {
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

/// The Status tab's cards above the run's, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetupPart {
    Clis,
    Health,
    Settings,
    Runs,
}

const SETUP_PARTS: [SetupPart; 4] = [
    SetupPart::Clis,
    SetupPart::Health,
    SetupPart::Settings,
    SetupPart::Runs,
];

/// Stats' sub-tabs, in display order.
const STATS_GROUPS: [(StatsGroup, &str); 3] = [
    (StatsGroup::Cli, "By CLI"),
    (StatsGroup::Model, "By model"),
    (StatsGroup::Role, "By role"),
];

/// The key as the panel reads it. On every tab g and G go to the first and the last card, as
/// Home and End do. With vim keys on, h and l also move along a row as ← and →, j and k
/// between rows, and q closes.
fn panel_key(key: KeyEvent, vim: bool) -> KeyEvent {
    if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
        return key;
    }
    let code = match key.code {
        KeyCode::Char('g') => KeyCode::Home,
        KeyCode::Char('G') => KeyCode::End,
        KeyCode::Char('h') if vim => KeyCode::Left,
        KeyCode::Char('l') if vim => KeyCode::Right,
        KeyCode::Char('j') if vim => KeyCode::Down,
        KeyCode::Char('k') if vim => KeyCode::Up,
        KeyCode::Char('q') if vim => KeyCode::Esc,
        _ => return key,
    };
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// `p`: pins the mod above the input, or unpins it when it's there.
fn pin(the_mod: ModName) -> ViewOutcome {
    ViewOutcome::Send(UiCommand::Slash {
        command: SlashCommand::Mods {
            action: ModsAction::Change {
                the_mod,
                change: ModPlaceChange::Toggle {
                    place: Place::AboveInput,
                },
            },
            scope: None,
        },
    })
}

/// A title with the keys: filled in the accent, as the tabs are, or reversed without colour.
pub(super) fn filled_title(kit: &Kit<'_>, style: ratatui::style::Style) -> ratatui::style::Style {
    if kit.has_color() {
        kit.cell(Role::BadgeText, Role::Question)
    } else {
        style.add_modifier(ratatui::style::Modifier::REVERSED)
    }
}

/// The card with the keys shows it by its title, filled in the accent, as the tabs do.
fn fill_title(kit: &Kit<'_>, card: &mut Drawn) {
    let mark = kit.glyphs.the_mod;
    for line in &mut card.lines {
        let Some(at) = line.spans.iter().position(|span| span.content == mark) else {
            continue;
        };
        if let Some(title) = line.spans[at + 1..]
            .iter_mut()
            .find(|span| !span.content.trim().is_empty())
        {
            title.style = filled_title(kit, title.style);
        }
        return;
    }
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

/// The keys in `width` cells, the way out always whole, the others giving way from the last.
fn fit_keys(mut keys: Vec<String>, close: &str, width: usize) -> String {
    loop {
        let line = keys
            .iter()
            .map(String::as_str)
            .chain([close])
            .collect::<Vec<_>>()
            .join(" · ");
        if crate::text::str_width(&line) < width || keys.pop().is_none() {
            return line;
        }
    }
}

/// The sub-tabs Tab goes along, named: `Overview or Models`, `By CLI, By model, By role`, or
/// on Usage, where a name each would crowd the line, `CLIs`.
fn along_words(tab: Tab, names: &[String]) -> String {
    match names {
        _ if tab == Tab::Usage => "CLIs".to_owned(),
        [first, second] => format!("{first} or {second}"),
        names => names.join(", "),
    }
}
