//! The Stats tab of the Overview, and `/stats`: what the crew did,
//! per CLI and per model, side by side and summed. Tokens are the first measure; `u` turns
//! the bars to tasks or spend. `m` shows one row per model, `r` one per role, `c` goes back
//! to the CLIs, and Enter opens a CLI, a model or a role one level deeper. `,` opens the stats
//! mod's page, which sets what all of these start as.
//!
//! Every number comes from the crew stats the usage numbers carry, which the work ledger
//! records beside the usage ledger. Each CLI draws in its own color and each of its models in
//! its own shade of it.

mod cli;
mod data;
mod draw;
mod opened;
pub mod pinned;
mod settings;
mod table;

use argonauts_core::api::{UsagePeriod, UsageSummary};
use argonauts_core::app::UiCommand;
use argonauts_core::crew_stats::CrewStats;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::settings::stats::{StatsGroup, StatsMeasure};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::text::Span;
use time::OffsetDateTime;

pub use opened::Opened;
pub use settings::SettingsPage;
pub use table::Sort;

use self::cli::Stats;
use self::data::Crew;
use self::pinned::StatsCard;
use self::settings::PageKey;
use crate::theme::Role;
use crate::widgets::{Drawn, Form, Frame, Kit};

/// How the tab shows the crew's stats, kept while it stays open.
#[derive(Debug, Clone, Default)]
pub struct StatsState {
    pub group: StatsGroup,
    pub measure: StatsMeasure,
    pub sort: Sort,
    /// The row the keys are on: a CLI's card and then a role by CLI, a model, or a role.
    pub picked: usize,
    pub opened: Option<Opened>,
    /// The group and measure of `mods/stats.toml` were taken, or a key changed them first.
    pub settled: bool,
    /// The stats mod's page, over the tab's cards while it's open.
    pub page: Option<SettingsPage>,
    /// The page was asked for before the numbers that carry the file's settings came.
    page_wanted: bool,
    /// Why a stats file was left out, from the last numbers, for the page.
    left_out: Vec<String>,
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

    /// The tab's cards, and which of them holds the row the keys are on.
    pub fn blocks(
        &self,
        kit: &Kit<'_>,
        usage: &UsageSummary,
        names: &DisplayNames,
        now: OffsetDateTime,
        width: usize,
    ) -> (Vec<Drawn>, usize) {
        let Some(stats) = &usage.stats else {
            return (vec![note(kit, "Reading the crew's stats…")], 0);
        };
        // A stats file that doesn't read is named first, so a page that looks wrong says why.
        let mut blocks: Vec<Drawn> = left_out(kit, &stats.problems).into_iter().collect();
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
        };
        let frame = Frame::view(width);
        let picked = self.picked.min(self.picks(&crew).saturating_sub(1));
        let mut focus = blocks.len();
        match self.group {
            StatsGroup::Cli => {
                if stats.settings.total {
                    blocks.push(look.all_together(kit, frame));
                }
                let clis = crew.clis.len();
                let on_cli = (picked < clis).then_some(picked);
                let rows = look.cli_cards(kit, width, on_cli);
                let per_row = clis.div_ceil(rows.len().max(1));
                if let Some(picked) = on_cli {
                    focus = blocks.len() + picked / per_row.max(1);
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
            }
            StatsGroup::Model => {
                focus = blocks.len();
                blocks.push(look.by_model(kit, frame, self.sort, picked));
                if let Some(card) = self.opened_card(kit, &look, frame) {
                    blocks.push(card);
                }
            }
            StatsGroup::Role => {
                focus = blocks.len();
                blocks.push(look.by_role(kit, frame, picked));
                if let Some(card) = self.opened_card(kit, &look, frame) {
                    blocks.push(card);
                }
            }
        }
        (blocks, focus)
    }

    fn opened_card(&self, kit: &Kit<'_>, look: &Stats<'_>, frame: Frame) -> Option<Drawn> {
        look.opened(kit, frame, self.opened.as_ref()?)
    }

    /// How many rows the keys move over.
    fn picks(&self, crew: &Crew) -> usize {
        match self.group {
            StatsGroup::Cli => crew.clis.len() + crew.roles().len(),
            StatsGroup::Model => crew.models().count(),
            StatsGroup::Role => crew.roles().len(),
        }
    }

    /// What the row the keys are on opens.
    fn at_pick(&self, usage: &UsageSummary, crew: &Crew) -> Option<Opened> {
        let picked = self.picked;
        match self.group {
            StatsGroup::Cli => match crew.clis.get(picked) {
                Some(cli) => Some(Opened::Cli(cli.agent.clone())),
                None => crew
                    .roles()
                    .get(picked - crew.clis.len())
                    .map(|role| Opened::Role(*role)),
            },
            StatsGroup::Model => {
                let look = Stats {
                    crew,
                    usage,
                    measure: self.measure,
                };
                look.sorted_models(self.sort)
                    .get(picked)
                    .map(|(_, model)| Opened::Model(model.agent.clone(), model.model.clone()))
            }
            StatsGroup::Role => crew.roles().get(picked).map(|role| Opened::Role(*role)),
        }
    }

    /// The tab's own keys. Anything else is the panel's.
    pub fn key(
        &mut self,
        code: KeyCode,
        usage: Option<&UsageSummary>,
        names: &DisplayNames,
        now: OffsetDateTime,
    ) -> Option<StatsKey> {
        let period = |period| Some(StatsKey::Period(period));
        let regroup = |state: &mut Self, group| {
            state.group = group;
            state.picked = 0;
            state.opened = None;
            state.settled = true;
            Some(StatsKey::Moved)
        };
        match code {
            KeyCode::Char('t') => return period(UsagePeriod::Today),
            KeyCode::Char('7') => return period(UsagePeriod::Week),
            KeyCode::Char('3') => return period(UsagePeriod::Month),
            KeyCode::Char('a') => return period(UsagePeriod::All),
            KeyCode::Char('c') => return regroup(self, StatsGroup::Cli),
            KeyCode::Char('m') => return regroup(self, StatsGroup::Model),
            KeyCode::Char('r') => return regroup(self, StatsGroup::Role),
            KeyCode::Char('u') => {
                self.measure = self.measure.next();
                self.settled = true;
                return Some(StatsKey::Moved);
            }
            KeyCode::Char('s') if self.group == StatsGroup::Model => {
                self.sort = self.sort.next();
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
            _ => {}
        }
        let usage = usage?;
        let stats = usage.stats.as_ref()?;
        let crew = Crew::of(usage, stats, names, now.date());
        let last = self.picks(&crew).saturating_sub(1);
        self.picked = self.picked.min(last);
        match code {
            KeyCode::Up | KeyCode::BackTab => self.picked = self.picked.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => self.picked = (self.picked + 1).min(last),
            KeyCode::Home => self.picked = 0,
            KeyCode::End => self.picked = last,
            KeyCode::Enter => {
                let here = self.at_pick(usage, &crew);
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

    /// The row under the tabs: what this view is, for `period`.
    pub fn description(&self, period: UsagePeriod) -> String {
        let span = match period {
            UsagePeriod::Today => "Today",
            UsagePeriod::Week => "The same seven days",
            UsagePeriod::Month => "The same thirty days",
            UsagePeriod::All => "Every day so far",
        };
        match self.group {
            StatsGroup::Cli => {
                "What the crew did: every CLI together, then each one by itself".to_owned()
            }
            StatsGroup::Model => format!("{span}, one row per model, whichever CLI runs it"),
            StatsGroup::Role => format!("{span}, one row per role, every model in it"),
        }
    }

    /// `by CLI · m by model · r by role · u bars measure tokens`, the view in use in bold.
    pub fn views(&self, kit: &Kit<'_>) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        for (group, key, words) in [
            (StatsGroup::Cli, "c", "by CLI"),
            (StatsGroup::Model, "m", "by model"),
            (StatsGroup::Role, "r", "by role"),
        ] {
            if !spans.is_empty() {
                spans.push(kit.dim(" · "));
            }
            if group == self.group {
                spans.push(Span::styled(words, kit.bold(crate::theme::Role::Card)));
            } else {
                spans.push(kit.dim(format!("{key} {words}")));
            }
        }
        spans.push(kit.dim(" · u bars measure "));
        spans.push(kit.strong(self.measure.word()));
        spans
    }

    /// The keys, in display order.
    pub fn keys(&self) -> Vec<&'static str> {
        match self.group {
            StatsGroup::Cli => vec![
                "←→ tab",
                "t 7 3 a period",
                "m by model",
                "r by role",
                "u tasks, tokens or spend",
                "enter open",
                "p pin",
            ],
            StatsGroup::Model => vec![
                "↑↓ model",
                "enter open or close",
                "c by CLI",
                "r by role",
                "u tasks, tokens or spend",
                "s sort",
            ],
            StatsGroup::Role => vec![
                "↑↓ role",
                "enter open or close",
                "c by CLI",
                "m by model",
                "u tasks, tokens or spend",
            ],
        }
    }
}

/// The Stats tab's periods, as its row of tabs writes them.
pub const PERIODS: [(UsagePeriod, &str); 4] = [
    (UsagePeriod::Today, "Today"),
    (UsagePeriod::Week, "7 days"),
    (UsagePeriod::Month, "30 days"),
    (UsagePeriod::All, "All"),
];

/// Each stats file that was left out, and why, in the warning colour.
fn left_out(kit: &Kit<'_>, problems: &[String]) -> Option<Drawn> {
    if problems.is_empty() {
        return None;
    }
    let mut block = Drawn::new();
    for problem in problems {
        block.push(vec![
            kit.span(format!(" {problem}"), Role::Warning),
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
