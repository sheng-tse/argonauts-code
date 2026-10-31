//! The Stats tab of the usage panel, and `/stats`: what the crew did, by CLI, by model and by
//! role, each built the same way: all of them together, a card for each, and a box that sets
//! them side by side. Tokens are the first measure; `u` turns By CLI's bars and the charts to
//! tasks or spend. Tab goes along the three views, `c` and `m` jump to the CLIs and the
//! models, and Enter opens a CLI, a model or a role one level deeper. `,` opens the stats
//! mod's page, which sets what all of these start as. The period's keys are the panel's.
//!
//! Every number comes from the crew stats the usage numbers carry, which the work ledger
//! records beside the usage ledger. Each CLI draws in its own color, each of its models in
//! that color with a texture of its own, and each role in a color of its own.

mod argonauts;
pub mod behaviors;
mod by_model;
mod by_role;
mod cli;
mod data;
pub(super) mod draw;
mod fills;
mod grid;
mod opened;
mod opened_model;
pub mod pinned;
mod settings;
mod table;
mod together;

use argonauts_core::app::UiCommand;
use argonauts_core::behaviors::crew_stats::CrewStats;
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::runs::api::{UsagePeriod, UsageSummary};
use argonauts_core::settings::stats::{StatsGroup, StatsMeasure};
use crossterm::event::{KeyCode, KeyEvent};
use time::OffsetDateTime;

pub(in crate::panels) use cli::letter;
pub use opened::Opened;
pub use settings::SettingsPage;

use self::cli::Stats;
use self::data::Crew;
use self::pinned::StatsCard;
use self::settings::PageKey;
use self::table::{Counted, Table, TableKey};
use super::super::base::theme::shades::shown_as;
use super::view::subtabs::{self, Level};
use crate::base::kit::{Drawn, Form, Frame, Kit};
use crate::base::theme::Role;

/// How the tab shows the crew's stats, kept while it stays open.
#[derive(Debug, Clone, Default)]
pub struct StatsState {
    pub group: StatsGroup,
    pub measure: StatsMeasure,
    /// The card the keys are on: a CLI's and then a role by CLI, a model's, or a role's.
    pub picked: usize,
    /// What Enter opened one level deeper: a CLI, a model or a role.
    pub opened: Option<Opened>,
    /// The group and measure of `mods/stats.toml` were taken, or a key changed them first.
    pub settled: bool,
    /// The stats mod's page, over the tab's cards while it's open.
    pub page: Option<SettingsPage>,
    /// The page was asked for before the numbers that carry the file's settings came.
    page_wanted: bool,
    /// Why a stats file was left out, from the last numbers, for the page.
    left_out: Vec<String>,
    /// The row the keys are on: the tabs, By CLI, By model and By role, or the cards.
    pub level: Level,
    /// A model asked for from elsewhere, such as the Overview, picked once its row is known.
    wanted: Option<Opened>,
    /// Why the model asked for has no row here, said above By model until the next key.
    missing: Option<String>,
    /// By model's and By role's table, which the keys move over in place of the cards.
    table: Table,
    /// The stats mod's file leaves out the all-together box, and with it the table, so the
    /// keys move over the cards.
    no_total: bool,
    /// The table's last key opened a card, which the tab keeps in view until the next one.
    table_opened: bool,
}

/// What a key on the tab asks of the panel.
#[derive(Debug, Clone, PartialEq)]
pub enum StatsKey {
    /// Drawn again, the keys' row in view.
    Moved,
    /// The numbers for another period.
    Period(UsagePeriod),
    Pin,
    Send(UiCommand),
}

impl StatsState {
    /// Takes the group and measure the stats mod's file sets, once, and opens its page if
    /// that was asked for before they came.
    pub fn settle(&mut self, stats: &CrewStats) {
        self.left_out.clone_from(&stats.problems);
        self.no_total = !stats.settings.total;
        if !self.settled {
            self.group = stats.settings.group;
            self.measure = stats.settings.measure;
            self.settled = true;
        }
        if std::mem::take(&mut self.page_wanted) {
            self.open_page(Some(stats));
        }
        if let Some(page) = &mut self.page {
            page.refresh(&stats.settings, &stats.sources);
        }
    }

    /// The stats mod's page, from the file's settings that `stats` carry. Without them yet it
    /// opens when they come.
    pub fn open_page(&mut self, stats: Option<&CrewStats>) {
        match stats {
            Some(stats) => {
                self.page = Some(SettingsPage::new(
                    stats.settings.clone(),
                    stats.sources.clone(),
                ));
            }
            None => self.page_wanted = true,
        }
    }

    pub fn page_open(&self) -> bool {
        self.page.is_some() || self.page_wanted
    }

    /// The page's preview and rows. `usage` is `None` while the numbers for its period are on
    /// their way, and the rows still show.
    pub fn page_blocks(
        &self,
        kit: &Kit<'_>,
        usage: Option<&UsageSummary>,
        names: &DisplayNames,
        now: OffsetDateTime,
        width: usize,
    ) -> Vec<Drawn> {
        let Some(page) = &self.page else {
            return vec![note(kit, "Reading the stats mod's file…")];
        };
        let frame = Frame::view(width);
        let preview = match usage.filter(|usage| usage.stats.is_some()) {
            Some(usage) => {
                StatsCard { usage, names, now }.draw_as(kit, Form::Full, frame, page.file.clone())
            }
            None => {
                let mut reading = Drawn::new();
                reading.push(vec![kit.dim("Reading the numbers for this period…")]);
                frame.draw(kit, reading)
            }
        };
        left_out(kit, &self.left_out)
            .into_iter()
            .chain([preview, page.rows(kit, width)])
            .collect()
    }

    /// A key while the page is open. The tab behind takes the period, the grouping and the
    /// measure as they change.
    pub fn page_key(&mut self, key: KeyEvent) -> Option<StatsKey> {
        let page = self.page.as_mut()?;
        Some(match page.handle_key(key)? {
            PageKey::Moved => StatsKey::Moved,
            PageKey::Period(period) => StatsKey::Period(period.usage()),
            PageKey::Shown => {
                let (group, measure) = (page.file.group, page.file.measure);
                if group != self.group {
                    self.picked = 0;
                    self.opened = None;
                }
                self.group = group;
                self.measure = measure;
                self.settled = true;
                StatsKey::Moved
            }
            PageKey::Send(command) => StatsKey::Send(command),
            PageKey::Close => {
                self.page = None;
                StatsKey::Moved
            }
        })
    }

    /// Esc on a page that's still waiting for the file's settings gives up on it.
    pub fn close_page(&mut self) -> bool {
        let wanted = std::mem::take(&mut self.page_wanted);
        self.page.take().is_some() || wanted
    }

    /// The tab's cards, and which of them holds the row the keys are on. `on_this_computer`
    /// is the Overview's count of every token here over every day, when it's at hand.
    pub fn blocks(
        &self,
        kit: &Kit<'_>,
        usage: &UsageSummary,
        (names, now): (&DisplayNames, OffsetDateTime),
        width: usize,
        on_this_computer: Option<u64>,
    ) -> (Vec<Drawn>, usize) {
        let Some(stats) = &usage.stats else {
            return (vec![note(kit, "Reading the crew's stats…")], 0);
        };
        // A stats file that doesn't read is named first, so a page that looks wrong says why.
        let mut blocks: Vec<Drawn> = left_out(kit, &stats.problems).into_iter().collect();
        if let Some(missing) = self
            .missing
            .as_deref()
            .filter(|_| self.group == StatsGroup::Model)
        {
            blocks.push(note(kit, missing));
        }
        let crew = Crew::of(usage, stats, names, now.date());
        if crew.is_empty() {
            blocks.push(note(
                kit,
                "Nothing recorded in this period yet. Stats fill in as the crew works.",
            ));
            return (blocks, 0);
        }
        let look = Stats {
            crew: &crew,
            usage,
            measure: self.measure,
            on_this_computer,
        };
        let frame = Frame::view(width);
        // The row the keys are on shows only while they're in the cards.
        let picked = match self.level {
            Level::Cards => self.picked.min(self.picks(&crew).saturating_sub(1)),
            Level::Tabs | Level::SubTabs => usize::MAX,
        };
        let mut focus = blocks.len();
        match self.group {
            StatsGroup::Cli => {
                let clis = crew.clis.len();
                let on_cli = (picked < clis).then_some(picked);
                // With the all-together card the keys go down its table's CLIs, which stays
                // in view, and the pointed CLI's card is the one picked under it.
                let in_table = stats.settings.total && on_cli.is_some();
                if stats.settings.total {
                    if in_table {
                        focus = blocks.len();
                    }
                    blocks.push(look.all_together(kit, frame, on_cli));
                }
                let (rows, layout) = look.cli_cards(kit, width, on_cli);
                if let Some(picked) = on_cli.filter(|_| !in_table) {
                    focus = blocks.len() + layout.row_of(picked);
                }
                blocks.extend(rows);
                let opened_cli = matches!(self.opened, Some(Opened::Cli(_)));
                if opened_cli && let Some(card) = self.opened_card(kit, &look, frame) {
                    // From the table, the opened card comes into view as Enter opens it.
                    if in_table && self.table_opened {
                        focus = blocks.len();
                    }
                    blocks.push(card);
                }
                let role = crew.roles().get(picked.saturating_sub(clis)).copied();
                let on_role = role.filter(|_| picked >= clis);
                if on_role.is_some() {
                    focus = blocks.len();
                }
                blocks.push(look.who_did_what(kit, frame, on_role));
                if matches!(self.opened, Some(Opened::Role(_)))
                    && let Some(card) = self.opened_card(kit, &look, frame)
                {
                    blocks.push(card);
                    focus = blocks.len() - 1;
                }
                blocks.push(argonauts::card(kit, frame, usage.argonauts.as_ref()));
            }
            StatsGroup::Model | StatsGroup::Role => {
                let by_model = self.group == StatsGroup::Model;
                let in_table = self.in_table();
                let mut on_card = (picked != usize::MAX).then_some(picked);
                if stats.settings.total {
                    let counted = self.counted(&look);
                    self.table.seen(counted.len());
                    if in_table {
                        focus = blocks.len();
                        // The pointed row's card is the one picked under the table.
                        on_card = self.table.pointed(&counted);
                    }
                    blocks.push(if by_model {
                        look.all_models(kit, frame, &self.table, in_table)
                    } else {
                        look.all_roles(kit, frame, &self.table, in_table)
                    });
                }
                // With the table, cards stop at the six models with the most tokens and the
                // rest get a line each under them; the keys move over the table's rows.
                let models = crew.models().count();
                let (carded, lines) = if by_model && stats.settings.total {
                    self.table.carded(&self.counted(&look), by_model::CARDS)
                } else {
                    ((0..models).collect(), Vec::new())
                };
                let (rows, layout) = if by_model {
                    look.model_cards(kit, width, &carded, on_card)
                } else {
                    look.role_cards(kit, width, on_card)
                };
                if let Some(picked) = on_card.filter(|_| !in_table) {
                    focus = blocks.len() + layout.row_of(picked);
                }
                blocks.extend(rows);
                if let Some(box_of_lines) = look.every_other_model(kit, frame, &lines, on_card) {
                    blocks.push(box_of_lines);
                }
                if let Some(card) = self.opened_card(kit, &look, frame) {
                    // From the table, the opened card comes into view as Enter opens it, and
                    // the table again as the keys move on it.
                    if !in_table || self.table_opened {
                        focus = blocks.len();
                    }
                    blocks.push(card);
                }
                blocks.extend(if by_model {
                    look.same_job(kit, frame)
                } else {
                    look.caught_or_changed(kit, frame)
                });
            }
        }
        (blocks, focus)
    }

    fn opened_card(&self, kit: &Kit<'_>, look: &Stats<'_>, frame: Frame) -> Option<Drawn> {
        look.opened(kit, frame, self.opened.as_ref()?)
    }

    /// The keys are on By model's or By role's table.
    fn in_table(&self) -> bool {
        self.level == Level::Cards && self.group != StatsGroup::Cli && !self.no_total
    }

    /// The table's rows as it reads them, in the bars' order.
    fn counted(&self, look: &Stats<'_>) -> Vec<Counted> {
        match self.group {
            StatsGroup::Model => look.model_counted(),
            StatsGroup::Cli | StatsGroup::Role => look.role_counted(),
        }
    }

    /// Whether the keys are on the first card or row, so ↑ goes back up to the sub-tabs.
    pub fn on_first(&self) -> bool {
        if self.in_table() {
            self.table.on_first()
        } else {
            self.picked == 0
        }
    }

    /// The tab takes `code` itself before the panel reads it as one of its own keys: on the
    /// table, its filter's letters, the side arrows that sort it, `/` and Esc.
    pub fn claims(&self, code: KeyCode) -> bool {
        self.in_table() && self.table.claims(code)
    }

    /// The cards have rows the keys move through: the crew did something in the period.
    pub fn has_rows(
        &self,
        usage: Option<&UsageSummary>,
        names: &DisplayNames,
        now: OffsetDateTime,
    ) -> bool {
        usage.is_some_and(|usage| {
            usage
                .stats
                .as_ref()
                .is_some_and(|stats| self.picks(&Crew::of(usage, stats, names, now.date())) > 0)
        })
    }

    /// A letter that regroups puts the keys in the cards; with no rows there they go to the
    /// sub-tabs, which show what it chose.
    fn leave_empty_cards(
        &mut self,
        usage: Option<&UsageSummary>,
        names: &DisplayNames,
        now: OffsetDateTime,
    ) {
        if self.level == Level::Cards && !self.has_rows(usage, names, now) {
            self.level = Level::SubTabs;
        }
    }

    /// How many cards the keys move over.
    fn picks(&self, crew: &Crew) -> usize {
        match self.group {
            StatsGroup::Cli => crew.clis.len() + crew.roles().len(),
            StatsGroup::Model => crew.models().count(),
            StatsGroup::Role => crew.filled().len(),
        }
    }

    /// What the card the keys are on opens.
    fn at_pick(&self, crew: &Crew) -> Option<Opened> {
        let picked = self.picked.min(self.picks(crew).saturating_sub(1));
        match self.group {
            StatsGroup::Cli => match crew.clis.get(picked) {
                Some(cli) => Some(Opened::Cli(cli.agent.clone())),
                None => crew
                    .roles()
                    .get(picked - crew.clis.len())
                    .map(|role| Opened::Role(*role)),
            },
            StatsGroup::Model => crew
                .models()
                .nth(picked)
                .map(|(_, model)| Opened::Model(model.agent.clone(), model.model.clone())),
            StatsGroup::Role => crew.filled().get(picked).map(|role| Opened::Role(*role)),
        }
    }

    /// The tab's own keys. Anything else is the panel's. ↓ goes from the tabs to the
    /// sub-tabs and on into the cards, ↑ back out; on the sub-tabs ←→ regroup. A letter that
    /// regroups puts the keys on the rows, where they act.
    pub fn key(
        &mut self,
        code: KeyCode,
        usage: Option<&UsageSummary>,
        names: &DisplayNames,
        now: OffsetDateTime,
    ) -> Option<StatsKey> {
        self.missing = None;
        // While the table's filter is typed, every letter is the filter's.
        if self.in_table() && self.table.typing {
            let usage = usage?;
            let crew = Crew::of(usage, usage.stats.as_ref()?, names, now.date());
            return self.table_key(code, usage, &crew);
        }
        match code {
            KeyCode::Char('c') => {
                self.regroup(StatsGroup::Cli, Level::Cards);
                self.leave_empty_cards(usage, names, now);
                return Some(StatsKey::Moved);
            }
            KeyCode::Char('m') => {
                self.regroup(StatsGroup::Model, Level::Cards);
                self.leave_empty_cards(usage, names, now);
                return Some(StatsKey::Moved);
            }
            KeyCode::Char('u') => {
                self.measure = self.measure.next();
                self.settled = true;
                return Some(StatsKey::Moved);
            }
            KeyCode::Char('p') => return Some(StatsKey::Pin),
            KeyCode::Char(',') => {
                self.open_page(usage.and_then(|usage| usage.stats.as_ref()));
                // The preview draws the period the file sets, as the card would.
                let wanted = self.page.as_ref().map(|page| page.file.period.usage());
                return match (usage, wanted) {
                    (Some(usage), Some(wanted)) if usage.period != wanted => {
                        Some(StatsKey::Period(wanted))
                    }
                    _ => Some(StatsKey::Moved),
                };
            }
            KeyCode::Left if self.level == Level::SubTabs => return Some(self.step_group(-1)),
            KeyCode::Right if self.level == Level::SubTabs => return Some(self.step_group(1)),
            KeyCode::Down | KeyCode::Enter if self.level != Level::Cards => {
                self.level = self.level.down(self.has_rows(usage, names, now));
                return Some(StatsKey::Moved);
            }
            KeyCode::Up if self.level != Level::Cards || self.on_first() => {
                self.level = self.level.up();
                return Some(StatsKey::Moved);
            }
            _ => {}
        }
        if self.level != Level::Cards {
            return None;
        }
        let usage = usage?;
        let stats = usage.stats.as_ref()?;
        let crew = Crew::of(usage, stats, names, now.date());
        if self.in_table() {
            return self.table_key(code, usage, &crew);
        }
        let last = self.picks(&crew).saturating_sub(1);
        self.picked = self.picked.min(last);
        self.table_opened = false;
        match code {
            KeyCode::Up => self.picked = self.picked.saturating_sub(1),
            KeyCode::Down => self.picked = (self.picked + 1).min(last),
            KeyCode::Home => self.picked = 0,
            KeyCode::End => self.picked = last,
            KeyCode::Enter => {
                let here = self.at_pick(&crew);
                self.opened = if self.opened == here { None } else { here };
                self.table_opened = self.opened.is_some();
            }
            _ => return None,
        }
        Some(StatsKey::Moved)
    }

    /// A key on the table: Enter opens or closes the pointed row's card, and Esc with nothing
    /// left to clear goes back up to the sub-tabs.
    fn table_key(&mut self, code: KeyCode, usage: &UsageSummary, crew: &Crew) -> Option<StatsKey> {
        let look = Stats {
            crew,
            usage,
            measure: self.measure,
            on_this_computer: None,
        };
        let counted = self.counted(&look);
        let key = self.table.key(code, &counted)?;
        self.table_opened = matches!(key, TableKey::Open(_));
        match key {
            TableKey::Moved => {}
            TableKey::Open(at) => {
                let here = match self.group {
                    StatsGroup::Model => crew
                        .models()
                        .nth(at)
                        .map(|(_, model)| Opened::Model(model.agent.clone(), model.model.clone())),
                    StatsGroup::Cli | StatsGroup::Role => {
                        crew.filled().get(at).map(|role| Opened::Role(*role))
                    }
                };
                self.opened = if self.opened == here { None } else { here };
            }
            TableKey::Leave => self.level = Level::SubTabs,
        }
        Some(StatsKey::Moved)
    }

    /// Esc closes what Enter opened before it closes the panel.
    pub fn close_opened(&mut self) -> bool {
        self.opened.take().is_some()
    }

    /// The row under the tabs: what this view is.
    pub fn description(&self) -> &'static str {
        match self.group {
            StatsGroup::Cli => "What the crew did: every CLI together, then each one by itself",
            StatsGroup::Model => "Every model the crew used: all together, then each one by itself",
            StatsGroup::Role => "Every role in the crew: all together, then each one by itself",
        }
    }

    /// ←→ on the sub-tabs: the next grouping, wrapping at the ends, shown at once.
    pub fn step_group(&mut self, by: isize) -> StatsKey {
        let at = GROUPS
            .iter()
            .position(|(group, _)| *group == self.group)
            .unwrap_or(0);
        self.regroup(
            GROUPS[subtabs::step(at, by, GROUPS.len())].0,
            Level::SubTabs,
        );
        StatsKey::Moved
    }

    fn regroup(&mut self, group: StatsGroup, level: Level) {
        if group != self.group {
            self.table = Table::default();
        }
        self.group = group;
        self.picked = 0;
        self.opened = None;
        self.settled = true;
        self.level = level;
    }

    /// Opens `model` of `agent` in the by-model view, its card picked once the numbers show
    /// where it is.
    pub fn open_model(&mut self, agent: String, model: Option<String>) {
        self.page = None;
        self.page_wanted = false;
        self.regroup(StatsGroup::Model, Level::Cards);
        self.wanted = Some(Opened::Model(agent, model));
    }

    /// Picks and opens the card of the model asked for with [`open_model`](Self::open_model),
    /// now the numbers are here. A model that did no crew work in the period stays unopened.
    pub fn pick_wanted(&mut self, usage: &UsageSummary, names: &DisplayNames, now: OffsetDateTime) {
        let Some(wanted) = self.wanted.take() else {
            return;
        };
        let Some(stats) = usage.stats.as_ref() else {
            self.wanted = Some(wanted);
            return;
        };
        let crew = Crew::of(usage, stats, names, now.date());
        // The Overview may spell the model another way than the crew's records.
        let shown = |agent: &str, model: Option<&str>| shown_as(names, agent, model);
        let found = crew
            .models()
            .enumerate()
            .find(|(_, (_, model))| match &wanted {
                Opened::Model(agent, wanted) => {
                    shown(agent, wanted.as_deref()) == shown(&model.agent, model.model.as_deref())
                }
                _ => false,
            });
        match found {
            Some((at, (_, model))) => {
                self.picked = at;
                self.opened = Some(Opened::Model(model.agent.clone(), model.model.clone()));
                let look = Stats {
                    crew: &crew,
                    usage,
                    measure: self.measure,
                    on_this_computer: None,
                };
                self.table.point_at(at, &look.model_counted());
            }
            None => {
                if let Opened::Model(agent, model) = &wanted {
                    let span = match usage.period {
                        UsagePeriod::Today => "today",
                        UsagePeriod::Week => "this week",
                        UsagePeriod::Month => "this month",
                        UsagePeriod::All => "so far",
                    };
                    self.missing = Some(format!(
                        "{} did no crew work {span}; its history is on the Overview",
                        names.agent(agent, model.as_deref())
                    ));
                }
            }
        }
    }

    /// The keys, in display order, after the period's: on the tabs and the sub-tabs how to
    /// move on, and in the cards what each key does there. Without `rows` in the cards, ↓
    /// stops at the sub-tabs and Enter there opens nothing.
    pub fn keys(&self, rows: bool) -> Vec<&'static str> {
        const ALONG: &str = "tab By CLI, By model, By role";
        let into = if self.no_total {
            "↓  into the cards"
        } else {
            "↓  into the table"
        };
        const MEASURE: &str = "u tokens, tasks or spend";
        const OPENS: &str = "enter opens or closes it";
        match (self.level, self.group) {
            (Level::Tabs, _) => vec![
                "tab · ←  →  tabs",
                if rows {
                    "↓  sub-tabs, then cards"
                } else {
                    "↓  sub-tabs"
                },
                MEASURE,
                "enter open",
                "p pin",
            ],
            (Level::SubTabs, _) if !rows => vec![ALONG, "↑  tabs"],
            (Level::SubTabs, StatsGroup::Cli) => vec![ALONG, into, "↑  tabs"],
            (Level::SubTabs, StatsGroup::Model) => vec![ALONG, into, "enter opens a model"],
            (Level::SubTabs, StatsGroup::Role) => vec![ALONG, into, "enter opens a role"],
            (Level::Cards, StatsGroup::Cli) => {
                // With the all-together card, the keys go down its CLIs, then the roles.
                let moves = if self.no_total {
                    "↑↓  card"
                } else {
                    "↑↓  CLI or role"
                };
                vec![moves, OPENS, ALONG, MEASURE, "p pin"]
            }
            (Level::Cards, StatsGroup::Model) => vec!["↑↓  model", OPENS, ALONG, MEASURE],
            (Level::Cards, StatsGroup::Role) => vec!["↑↓  role", OPENS, ALONG, MEASURE],
        }
    }
}

impl StatsState {
    /// The keys while they're on the table, in display order: the way out comes last, or with
    /// a filter set, first.
    pub fn table_keys(&self) -> Option<Vec<String>> {
        if !self.in_table() {
            return None;
        }
        let table = &self.table;
        let moves = match self.group {
            StatsGroup::Model => "↑↓  model",
            StatsGroup::Cli | StatsGroup::Role => "↑↓  role",
        };
        let sort = format!("←  →  sort: {}", table.sort_for(table.parts()).word());
        let enter = match table.on_mark {
            Some(_) => "enter shows every row",
            None => "enter opens it",
        };
        // What Tab does comes last before the way out, so a narrow line gives it up first.
        const TAB: &str = "tab By CLI, By model, By role";
        let words = |keys: &[&str]| keys.iter().map(|key| (*key).to_owned()).collect();
        Some(match (&table.query, table.typing) {
            (Some(_), true) => words(&["type to filter", moves, "enter keeps it", "esc clears it"]),
            (Some(query), false) => {
                let mut keys = vec![format!("esc clears {query}")];
                keys.extend(words(&[moves, enter]));
                keys.extend([sort, TAB.to_owned(), "/ search".to_owned()]);
                keys
            }
            (None, _) => {
                let out = if table.every_row {
                    "esc shows nine rows"
                } else {
                    "esc leaves the table"
                };
                let mut keys = words(&[moves]);
                keys.push(sort);
                keys.extend(words(&["/ search", enter, TAB, out]));
                keys
            }
        })
    }
}

/// The sub-tabs, in order.
const GROUPS: [(StatsGroup, &str); 3] = [
    (StatsGroup::Cli, "By CLI"),
    (StatsGroup::Model, "By model"),
    (StatsGroup::Role, "By role"),
];

/// Each stats file that was left out, and why, in the warning colour.
fn left_out(kit: &Kit<'_>, problems: &[String]) -> Option<Drawn> {
    if problems.is_empty() {
        return None;
    }
    let mut block = Drawn::new();
    for problem in problems {
        block.push(vec![
            kit.span(format!(" {problem}"), Role::Warn),
            kit.dim(" · left out until it reads"),
        ]);
    }
    Some(block)
}

fn note(kit: &Kit<'_>, words: &str) -> Drawn {
    let mut block = Drawn::new();
    block.push(vec![kit.dim(format!(" {words}"))]);
    block
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod golden;

#[cfg(test)]
mod golden_by_model_and_role;

pub mod deep_flow;
pub mod deep_lines;
pub mod deep_map;
pub mod deep_open;
#[cfg(test)]
mod golden_many_models;
