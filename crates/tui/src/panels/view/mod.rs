//! The usage panel: one place for `/status`, `/usage`, `/context` and `/limits`, with a tab
//! each and Stats beside them. It draws under the conversation, as Claude
//! Code's dialogs do, so a running answer goes on above it, and takes only the rows its cards
//! need.
//!
//! The usage tabs draw the cards of [`cards`](super::cards) from the numbers the screen
//! keeps, and say so each time they draw, which keeps the numbers coming. Status and Context
//! draw their own cards from the answers `/status` and `/context` always got, and are asked
//! for again while their tab stays open and work goes on.

mod context;
mod head;
mod home;
mod key_line;
mod map;
mod stats;
mod status;
mod usage;

use std::cell::Cell;
use std::time::{Duration, Instant};

use argonauts_core::RunId;
use argonauts_core::app::overview::{ModelTokens, OverviewPeriod};
use argonauts_core::app::status_tab::StatusTabView;
use argonauts_core::app::views::ContextView as ContextData;
use argonauts_core::app::{AppMode, SettingRow, UiCommand, ViewData, ViewRequest};
use argonauts_core::behaviors::deep_map::DeepOffer;
use argonauts_core::input::{ModPlaceChange, ModsAction, SlashCommand};
use argonauts_core::runs::api::{StatusSummary, UsagePeriod, UsageSummary};
use argonauts_core::settings::mods::ModPlace;
use argonauts_core::settings::ui::{ModName, Place};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::text::Line;
use time::OffsetDateTime;

use self::stats::STATS_GROUPS;
use super::UsageWatch;
use super::cards::UsageCard;
use super::context::ContextPane;
use super::home::{HomeKey, HomeTab};
use super::stats::StatsState;
pub use super::view::subtabs::Level;
use crate::base::kit::{Drawn, Form, Frame, Kit, stack_cards};
use crate::base::text::{marks, truncate_spans};
use crate::base::theme::Role;
use crate::frame::state::AppState;
use crate::frame::state::Places;
use crate::frame::view::list;
use crate::frame::view::map::{MapCells, MapView};
use crate::frame::view::{View, ViewCtx, ViewOutcome};

/// Rows above the cards: the rule, the tabs, what the tab is about, and a blank row.
const HEAD_ROWS: u16 = 4;
/// Rows under the cards: a blank row and the keys.
const FOOT_ROWS: u16 = 2;
/// From this width the tokens and history cards share a row.
const SIDE_BY_SIDE_FROM: usize = 100;
/// The tabs that show the clock's work redraw this often, so ages and elapsed times move.
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
    /// The list of limits, where the arrows pick one.
    Limits,
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
    /// Which block holds the list of limits, as Limits last drew it.
    limits_block: Cell<usize>,
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
    config: Box<dyn ConfigPanel>,
    /// Where the Behaviors map drew last, while it shows.
    map_drawn: Option<MapCells>,
    /// The cells the pointer's line under the map left for its picture, while they show.
    line_drawn: Option<Rect>,
}

/// The Config tab, which the shell gives the panel: the settings screens it opens live above
/// the panels, so the panel drives the tab through this.
pub trait ConfigPanel: std::fmt::Debug + Send {
    /// Whether its search box or picker holds the keys, so it stays over a question.
    fn waits(&self) -> bool;
    /// The rows it takes under the panel's tabs on this state's screen.
    fn wanted_rows(&self, state: &AppState) -> u16;
    /// A key while the tab has the keys; `None` leaves it to the panel.
    fn key(&mut self, key: KeyEvent, state: &AppState) -> Option<ViewOutcome>;
    /// What Esc on the tabs does once the tab changed something.
    fn close(&self) -> ViewOutcome;
    fn enter_from_tabs(&mut self);
    fn paste(&mut self, text: &str);
    /// The offer a Behaviors row's picker asked for.
    fn accept_offer(&mut self, offer: &DeepOffer);
    /// Every key, which the tab asked for when it opened.
    fn set_rows(&mut self, rows: Vec<SettingRow>);
    /// Draws the tab in `area`, under the panel's tabs; `on_tabs` is whether the tabs have
    /// the keys.
    fn draw(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>, on_tabs: bool);
}

/// A Config tab with nothing in it, for the panels' own tests.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct NoConfig;

#[cfg(test)]
impl NoConfig {
    pub(crate) fn boxed() -> Box<dyn ConfigPanel> {
        Box::new(Self)
    }
}

#[cfg(test)]
impl ConfigPanel for NoConfig {
    fn waits(&self) -> bool {
        false
    }
    fn wanted_rows(&self, _: &AppState) -> u16 {
        0
    }
    fn key(&mut self, _: KeyEvent, _: &AppState) -> Option<ViewOutcome> {
        None
    }
    fn close(&self) -> ViewOutcome {
        ViewOutcome::Close
    }
    fn enter_from_tabs(&mut self) {}
    fn paste(&mut self, _: &str) {}
    fn accept_offer(&mut self, _: &DeepOffer) {}
    fn set_rows(&mut self, _: Vec<SettingRow>) {}
    fn draw(&mut self, _: Rect, _: &mut Buffer, _: &ViewCtx<'_>, _: bool) {}
}

impl OverviewView {
    pub fn new(tab: Tab, watch: UsageWatch, config: Box<dyn ConfigPanel>) -> Self {
        Self {
            tab,
            level: Level::Tabs,
            period: UsagePeriod::OPENS_ON,
            left: false,
            focus: 0,
            scroll: 0,
            focus_block: Cell::new(0),
            limits_block: Cell::new(0),
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
            config,
            map_drawn: None,
            line_drawn: None,
        }
    }

    /// The Config tab as `/config` opens it, with its search box taking the keys.
    pub fn with_config(mut self, config: Box<dyn ConfigPanel>) -> Self {
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

    /// Draws ages and reset times as of `now`, whenever it draws.
    pub fn at(mut self, now: OffsetDateTime) -> Self {
        self.now = Some(now);
        self.home.behaviors.clock = crate::base::format::WallClock::pinned(now);
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
        self.behaviors_from_above();
        // On Limits the keys pick a limit, so its list comes into view.
        self.follow = Some(if self.tab == Tab::Limits {
            Follow::Limits
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
        self.behaviors_from_above();
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
            Tab::Stats => self.stats.on_first(),
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
        self.now.unwrap_or_else(crate::base::format::wall_now)
    }

    /// The numbers to draw: the state's, when they're for the period this tab shows.
    fn usage<'a>(&self, ctx: &'a ViewCtx<'_>) -> Option<&'a UsageSummary> {
        ctx.state
            .usage
            .as_deref()
            .filter(|usage| usage.period == self.period)
    }

    /// The ranking every tab colours by, which places a CLI's models for the shade each draws
    /// in: the list the usage numbers carry, whatever their period, or else the one the
    /// Overview's numbers carry, which is the same list. Empty before either has come.
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
                return super::context::blocks(self.context_sections(kit, ctx, width));
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
            let yours = crate::panels::mods::on_tab(kit, view, place, width);
            if !yours.lines.is_empty() {
                blocks.push(yours);
            }
        }
        blocks
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
            // The import's question and Summaries' take the keys while they show.
            Tab::Overview => !(self.home.asking() || self.home.behaviors_asking()),
            Tab::Stats => !self.stats.page_open(),
            Tab::Usage | Tab::Limits => true,
            Tab::Status | Tab::Config | Tab::Context => false,
        }
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
            let wanted = 2 + self.config.wanted_rows(ctx.state);
            return Some(wanted.min(screen.height.saturating_sub(6)).max(12));
        }
        self.settle_stats(ctx);
        // The working line and a few rows of the conversation stay above it.
        let most = screen.height.saturating_sub(6);
        // The Behaviors list takes as many rows as it's given, so it's measured with every row
        // the panel could have, or the panel would never grow past the rows it last had.
        let head_and_foot = self.head_rows() + FOOT_ROWS;
        let body = usize::from(most.saturating_sub(head_and_foot));
        self.home.set_room(body);
        self.home.behaviors.size_map_for(body);
        let kit = Kit::new(ctx.theme);
        let blocks = self.blocks(&kit, ctx, usize::from(screen.width));
        let body = stack_cards(blocks).height();
        let wanted = u16::try_from(body)
            .unwrap_or(u16::MAX)
            .saturating_add(head_and_foot);
        Some(wanted.min(most).max(head_and_foot + 1))
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        // Whatever this frame draws, the map's cells from the last are gone: a tab that draws no
        // map, or a panel too small for its head, must take the world's pictures off at once.
        let had_map = self.map_drawn.take().is_some();
        self.line_drawn = None;
        self.render_tab(area, buf, ctx);
        if had_map && self.map_drawn.is_none() {
            self.home.behaviors.left();
        }
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
        // Stats' table takes its filter's letters, the side arrows and Esc before the panel.
        if self.tab == Tab::Stats && self.stats.claims(key.code) {
            return self.stats_key(key, ctx);
        }
        // Config's search box takes every letter, so vim's keys stay letters there.
        let in_config = self.tab == Tab::Config && self.level() != Level::Tabs;
        if in_config && let Some(outcome) = self.config.key(key, ctx.state) {
            return outcome;
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
            ViewData::DeepOffer { offer } => {
                self.home.behaviors.accept_offer(offer);
                self.config.accept_offer(offer);
            }
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
        // A picture of the world drawn off the frame wakes the screen itself when it arrives.
        self.moves_with_time().then(|| now + REDRAW)
    }
}

impl OverviewView {
    /// Whether the tab shown says something that moves with the clock: a session's elapsed
    /// time, or a window's reset. Every other tab changes only when what it shows does, so it
    /// never wakes the app on its own.
    fn moves_with_time(&self) -> bool {
        matches!(self.tab, Tab::Status | Tab::Usage | Tab::Limits)
    }

    /// The panel's tab as this frame draws it, the map's cells set again where it draws one.
    fn render_tab(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let head = self.head_rows();
        if area.height < head || area.width == 0 {
            return;
        }
        if self.tab == Tab::Config {
            self.render_config(area, buf, ctx);
            return;
        }
        self.settle_stats(ctx);
        self.sync_behaviors(ctx);
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
                Follow::Limits => self.limits_block.get(),
            };
            self.scroll = Self::scrolled_to(&blocks, self.scroll, wanted, room);
        }
        let (body, shown) = Self::fitted(blocks, self.scroll, room);
        self.drawn.set((count, shown));
        let on_screen = |cells: Rect| Rect {
            x: cells.x + area.x,
            y: cells.y + area.y + head,
            ..cells
        };
        let drawn = self.map_drawn(&kit, ctx, &heights, shown, (width, room));
        self.map_drawn = drawn.map(|(cells, _)| MapCells {
            area: on_screen(cells.area),
            ..cells
        });
        self.line_drawn = drawn.and_then(|(_, line)| line).map(on_screen);
        for (row, line) in body.lines.iter().take(room).enumerate() {
            buf.set_line(area.x, area.y + head + row as u16, line, area.width);
        }
        // More is below: the arrows or a page down reach it.
        let more = self.scroll + shown < count;
        // The Overview's key line sits under its rows, two cells in, as Claude Code's does.
        let inset = if self.tab == Tab::Overview {
            "   "
        } else {
            " "
        };
        let keys = self.keys(ctx, more, width.saturating_sub(inset.len()));
        let keys = Line::from(truncate_spans(&[kit.dim(format!("{inset}{keys}"))], width));
        buf.set_line(area.x, area.bottom() - 1, &keys, area.width);
    }

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
        self.config.draw(body, buf, ctx, on_tabs);
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

pub mod periods;
pub(crate) mod subtabs;
#[cfg(test)]
mod tests;
pub(crate) mod watch;
