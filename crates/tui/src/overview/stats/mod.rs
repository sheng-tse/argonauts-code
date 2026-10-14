//! The Stats tab of the usage panel, and `/stats`: what the crew did, by CLI, by model and by
//! role, each built the same way: all of them together, a card for each, and a box that sets
//! them side by side. Tokens are the first measure; `u` turns By CLI's bars and the charts to
//! tasks or spend. Tab goes along the three views, `c` and `m` jump to the CLIs and the
//! models, and Enter opens a CLI, a model or a role one level deeper. `,` opens the stats
//! mod's page, which sets what all of these start as. The period's keys are the panel's.
//!
//! Every number comes from the crew stats the usage numbers carry, which the work ledger
//! records beside the usage ledger. Each CLI draws in its own color, each of its models in
//! its own shade of it, and each role in a color of its own.

mod argonauts;
mod by_model;
mod by_role;
mod cli;
mod data;
pub(super) mod draw;
mod grid;
mod opened;
mod opened_model;
pub mod pinned;
mod settings;
mod together;

use argonauts_core::api::{UsagePeriod, UsageSummary};
use argonauts_core::app::UiCommand;
use argonauts_core::crew_stats::CrewStats;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::settings::stats::{StatsGroup, StatsMeasure};
use crossterm::event::{KeyCode, KeyEvent};
use time::OffsetDateTime;

pub use opened::Opened;
pub use settings::SettingsPage;

use self::cli::Stats;
use self::data::Crew;
use self::pinned::StatsCard;
use self::settings::PageKey;
use super::shades::shown_as;
use super::subtabs::{self, Level};
use crate::kit::{Drawn, Form, Frame, Kit};
use crate::theme::Role;

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
                if stats.settings.total {
                    blocks.push(look.all_together(kit, frame));
                }
                let clis = crew.clis.len();
                let on_cli = (picked < clis).then_some(picked);
                let (rows, layout) = look.cli_cards(kit, width, on_cli);
                if let Some(picked) = on_cli {
                    focus = blocks.len() + layout.row_of(picked);
                }
                blocks.extend(rows);
                let opened_cli = matches!(self.opened, Some(Opened::Cli(_)));
                if opened_cli && let Some(card) = self.opened_card(kit, &look, frame) {
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
                if stats.settings.total {
                    blocks.push(if by_model {
                        look.all_models(kit, frame)
                    } else {
                        look.all_roles(kit, frame)
                    });
                }
                let on_card = (picked != usize::MAX).then_some(picked);
                let (rows, layout) = if by_model {
                    look.model_cards(kit, width, on_card)
                } else {
                    look.role_cards(kit, width, on_card)
                };
                if let Some(picked) = on_card {
                    focus = blocks.len() + layout.row_of(picked);
                }
                blocks.extend(rows);
                if let Some(card) = self.opened_card(kit, &look, frame) {
                    focus = blocks.len();
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
        match code {
            KeyCode::Char('c') => {
                self.regroup(StatsGroup::Cli, Level::Cards);
                return Some(StatsKey::Moved);
            }
            KeyCode::Char('m') => {
                self.regroup(StatsGroup::Model, Level::Cards);
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
                self.level = self.level.down();
                return Some(StatsKey::Moved);
            }
            KeyCode::Up if self.level != Level::Cards || self.picked == 0 => {
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
        let last = self.picks(&crew).saturating_sub(1);
        self.picked = self.picked.min(last);
        match code {
            KeyCode::Up => self.picked = self.picked.saturating_sub(1),
            KeyCode::Down => self.picked = (self.picked + 1).min(last),
            KeyCode::Home => self.picked = 0,
            KeyCode::End => self.picked = last,
            KeyCode::Enter => {
                let here = self.at_pick(&crew);
                self.opened = if self.opened == here { None } else { here };
            }
            _ => return None,
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
    /// move on, and in the cards what each key does there.
    pub fn keys(&self) -> Vec<&'static str> {
        const ALONG: &str = "tab By CLI, By model, By role";
        const INTO: &str = "↓ into the cards";
        const MEASURE: &str = "u tokens, tasks or spend";
        const OPENS: &str = "enter opens or closes it";
        match (self.level, self.group) {
            (Level::Tabs, _) => vec![
                "tab · ←→ tabs",
                "↓ sub-tabs, then cards",
                MEASURE,
                "enter open",
                "p pin",
            ],
            (Level::SubTabs, StatsGroup::Cli) => vec![ALONG, INTO, "↑ tabs"],
            (Level::SubTabs, StatsGroup::Model) => vec![ALONG, INTO, "enter opens a model"],
            (Level::SubTabs, StatsGroup::Role) => vec![ALONG, INTO, "enter opens a role"],
            (Level::Cards, StatsGroup::Cli) => {
                vec!["↑↓ card", OPENS, ALONG, MEASURE, "p pin"]
            }
            (Level::Cards, StatsGroup::Model) => vec!["↑↓ model", OPENS, ALONG, MEASURE],
            (Level::Cards, StatsGroup::Role) => vec!["↑↓ role", OPENS, ALONG, MEASURE],
        }
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
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
mod golden;

#[cfg(test)]
#[path = "golden_tests.rs"]
mod golden_tests;

#[cfg(test)]
mod golden_by_model_and_role;
