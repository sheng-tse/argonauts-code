//! The Overview tab: every CLI's work on this computer, laid out as Claude Code's /stats lays
//! out its own. Two sub-tabs, Overview and Models, and a period line that `r` cycles. Overview
//! has the year of days, its numbers in three groups, the tokens drawn by kind and by CLI, a
//! fun fact, what's worth knowing and where the numbers came from; Models has the top models'
//! tokens a day and each model with its CLI, value and sessions; Behaviors maps what the agents
//! did. No boxes. The first time it
//! opens it asks once whether to import what Claude Code and Codex recorded; the import runs
//! in the app, and the tab says how far it got.

mod chart;
mod facts;
pub(super) mod look;
mod models;
mod numbers;
mod year;

use std::cell::Cell;

use argonauts_core::api::UsagePeriod;
use argonauts_core::app::ViewRequest;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::overview::{
    CliSource, ImportStatus, ModelTokens, OverviewData, OverviewPeriod,
};
use crossterm::event::KeyCode;
use ratatui::text::Span;

use self::look::{cli_word, model_colors, since_word};
use self::models::{Models, last_page, page_word};
use self::numbers::{Numbers, periods};
use self::year::Year;
use super::stats::behaviors::{BehaviorsTab, GroupColours, MapKey};
use super::subtabs::{self, Level};
use crate::kit::{Drawn, Frame, Kit};
use crate::text::truncate_spans;
use crate::theme::Role;

/// Behaviors' key down to its map, the only way there from the sub-tabs.
pub(crate) const DOWN_TO_THE_MAP: &str = "↓  groupings, then the map";

/// The Overview tab's own tabs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SubTab {
    #[default]
    Overview,
    Models,
    Behaviors,
}

impl SubTab {
    const ALL: [SubTab; 3] = [SubTab::Overview, SubTab::Models, SubTab::Behaviors];

    fn title(self) -> &'static str {
        match self {
            SubTab::Overview => "Overview",
            SubTab::Models => "Models",
            SubTab::Behaviors => "Behaviors",
        }
    }
}

/// What the Overview tab keeps while the panel is open.
#[derive(Debug, Clone, Default)]
pub struct HomeTab {
    pub period: OverviewPeriod,
    pub level: Level,
    pub sub: SubTab,
    /// The first model the Models sub-tab shows, a row of two at a time; 0 is the top.
    pub picked: usize,
    /// The import question's choice: yes first.
    answer: usize,
    data: Option<OverviewData>,
    /// What the import found, said once it finishes while the tab is open.
    found: Option<String>,
    /// Which models the Models sub-tab shows, for the key line.
    page: String,
    pub behaviors: BehaviorsTab,
    /// The rows the panel has under its tabs, which the Behaviors list fills.
    room: Cell<usize>,
}

/// What a key on the tab asks of the panel.
#[derive(Debug, Clone, PartialEq)]
pub enum HomeKey {
    Moved,
    Ask(ViewRequest),
    Copy(String),
    /// A command for the app, such as Summaries' update starting.
    Send(argonauts_core::app::UiCommand),
    /// Enter on the models: Stats opens the first one shown, for the same span of days.
    OpenModel {
        agent: String,
        model: Option<String>,
        period: UsagePeriod,
    },
}

impl HomeTab {
    /// The request for the tab's numbers, and the answer to its question when there is one.
    pub fn request(&self, import: Option<bool>) -> ViewRequest {
        ViewRequest::Overview {
            period: self.period,
            import,
        }
    }

    /// Takes the numbers for the period it shows. The import's result line stays while the
    /// panel is open.
    pub fn accept(&mut self, data: &OverviewData) {
        let was_running = matches!(
            self.data.as_ref().map(|data| &data.import),
            Some(ImportStatus::Running { .. })
        );
        if was_running && data.import == ImportStatus::Done {
            self.found.clone_from(&data.found);
        }
        if data.period == self.period {
            self.data = Some(data.clone());
            self.picked = self.picked.min(last_page(data.models.len()));
            self.turn_page();
        }
    }

    /// The ranking every tab colours by, as the numbers at hand carry it for any period.
    pub fn all_time_models(&self) -> Option<&[ModelTokens]> {
        self.data
            .as_ref()
            .map(|data| data.all_time_models.as_slice())
    }

    /// The tokens of everything on this computer over every day so far, when the numbers at
    /// hand are for every day. Stats sets the crew's own tokens beside it.
    pub fn every_day_tokens(&self) -> Option<u64> {
        self.data
            .as_ref()
            .filter(|data| data.period == OverviewPeriod::All)
            .map(|data| data.total.total())
    }

    /// The import's question shows, and takes the keys it answers with.
    pub fn asking(&self) -> bool {
        matches!(
            self.data.as_ref().map(|data| &data.import),
            Some(ImportStatus::Unasked)
        )
    }

    /// Overview and Models, and the one shown.
    pub fn sub_tabs(&self) -> (Vec<String>, usize) {
        let titles = SubTab::ALL.map(|sub| sub.title().to_owned());
        (titles.to_vec(), self.sub_index())
    }

    fn sub_index(&self) -> usize {
        SubTab::ALL
            .iter()
            .position(|sub| *sub == self.sub)
            .unwrap_or(0)
    }

    /// The other sub-tab, `by` places along, wrapping. Behaviors asks for its map each time
    /// it opens.
    pub fn step_sub_tab(&mut self, by: isize) -> HomeKey {
        let at = (self.sub_index() as isize + by).rem_euclid(SubTab::ALL.len() as isize);
        self.sub = SubTab::ALL[at as usize];
        self.picked = 0;
        self.turn_page();
        if self.sub == SubTab::Behaviors {
            return HomeKey::Ask(self.behaviors.ask());
        }
        HomeKey::Moved
    }

    pub fn on_behaviors(&self) -> bool {
        self.sub == SubTab::Behaviors
    }

    /// Summaries' questions show on Behaviors.
    pub fn behaviors_asking(&self) -> bool {
        self.on_behaviors() && self.behaviors.deep.asking()
    }

    /// The colour of each of the Behaviors map's groups, as the rest of the tab colours them.
    pub fn behavior_colours(&self, kit: &Kit<'_>, names: &DisplayNames) -> GroupColours {
        let models = self.all_time_models().unwrap_or_default();
        match self.behaviors.map() {
            Some(map) => GroupColours::new(kit, map, models, names),
            None => GroupColours::default(),
        }
    }

    /// ↑ from the rows goes back to the sub-tabs.
    pub fn on_first(&self) -> bool {
        match self.sub {
            SubTab::Behaviors => self.behaviors.on_first(),
            _ => self.picked == 0,
        }
    }

    /// Behaviors answers this key before the panel does: its world's letters and digits from any
    /// row, and every key while its own rows have them.
    pub fn claims(&self, code: KeyCode) -> bool {
        let anywhere = matches!(
            code,
            KeyCode::Char('s' | 'd' | 'x' | '1' | '2' | '3' | '+' | '=' | '-' | 't' | 'n' | 'm')
        );
        self.sub == SubTab::Behaviors
            && (anywhere || self.level != Level::Tabs)
            && self.behaviors.claims(code, self.level == Level::Cards)
    }

    pub fn accept_map(&mut self, map: &argonauts_core::behavior_map::BehaviorMap) {
        self.behaviors.accept(map);
    }

    /// The rows the panel has for the tab's blocks.
    pub fn set_room(&self, rows: usize) {
        self.room.set(rows);
    }

    /// The rows the panel last said it has for the tab's blocks.
    pub fn room(&self) -> usize {
        self.room.get()
    }

    /// ` Overview    Models `, the one shown filled while Tab moves them.
    pub fn about(&self, kit: &Kit<'_>, _names: &DisplayNames, width: usize) -> Vec<Span<'static>> {
        let titles = SubTab::ALL.map(SubTab::title);
        // On Behaviors the row filled is the one with the keys: the sub-tabs only while they
        // have them, since below them Tab steps the groupings.
        let filled = match self.sub {
            SubTab::Behaviors => self.level == Level::SubTabs,
            _ => self.level.moves_sub_tabs(),
        };
        let row = subtabs::row(kit, &titles, self.sub_index(), filled);
        truncate_spans(&row, width)
    }

    /// The tab's blocks top to bottom, and the one to keep in view.
    pub fn blocks(&self, kit: &Kit<'_>, names: &DisplayNames, width: usize) -> (Vec<Drawn>, usize) {
        if self.sub == SubTab::Behaviors {
            let colours = self.behavior_colours(kit, names);
            let blocks = self
                .behaviors
                .blocks(kit, &colours, names, width, self.room.get());
            return (blocks, 0);
        }
        let Some(data) = &self.data else {
            return (vec![note(kit, "Reading every CLI's history…")], 0);
        };
        let mut blocks = Vec::new();
        if data.import == ImportStatus::Unasked {
            blocks.push(self.question(kit, width));
        }
        let empty = data.total.total() == 0 && data.import != ImportStatus::Unasked;
        let shades = &model_colors(data, names);
        match self.sub {
            SubTab::Overview => {
                let year = Year {
                    data,
                    names,
                    colors: data.year_colors,
                    shades,
                };
                blocks.push(year.draw(kit, width));
                blocks.push(year.legend(kit, width));
                blocks.push(periods(kit, self.period));
                let numbers = Numbers { data, names };
                if empty {
                    blocks.push(note(kit, self.nothing_yet(data)));
                } else {
                    blocks.push(numbers.block(kit, width));
                }
                blocks.push(numbers.words(kit, width, self.source(data, names)));
                (blocks, 0)
            }
            SubTab::Models => {
                blocks.push(periods(kit, self.period));
                let models = Models {
                    data,
                    names,
                    shades,
                };
                if empty || data.models.is_empty() {
                    blocks.push(note(kit, self.nothing_yet(data)));
                    let last = blocks.len() - 1;
                    return (blocks, last);
                }
                blocks.extend(models.chart(kit, width));
                blocks.extend(models.favourite(kit, width));
                blocks.push(models.columns(kit, width, self.picked));
                let last = blocks.len() - 1;
                (blocks, last)
            }
            // Drawn before the numbers are needed, above.
            SubTab::Behaviors => (blocks, 0),
        }
    }

    fn nothing_yet(&self, data: &OverviewData) -> &'static str {
        match data.import {
            ImportStatus::Running { .. } => "Importing… the numbers fill in as it reads.",
            ImportStatus::Declined => {
                "Nothing in this period yet. i imports what your CLIs recorded."
            }
            _ => "Nothing in this period yet.",
        }
    }

    /// `Stats from the last 365 days · imported: Claude Code since Oct 2025, Codex since Feb`,
    /// or how far the import is, or what it found.
    fn source(&self, data: &OverviewData, names: &DisplayNames) -> String {
        let mut parts = vec![match data.span_days {
            1 => "Stats from the last day".to_owned(),
            days => format!("Stats from the last {days} days"),
        }];
        let since = |source: &CliSource| {
            let cli = cli_word(names, &source.agent);
            match source.since {
                Some(day) => format!("{cli} since {}", since_word(day, data.today)),
                None => cli,
            }
        };
        let (imported, through): (Vec<&CliSource>, Vec<&CliSource>) =
            data.sources.iter().partition(|source| source.imported);
        let listed = |sources: &[&CliSource]| {
            sources
                .iter()
                .map(|source| since(source))
                .collect::<Vec<_>>()
                .join(", ")
        };
        match (&self.found, &data.import) {
            (Some(found), _) => parts.push(format!("imported {found}")),
            (None, ImportStatus::Running { of: 0, .. }) => {
                parts.push("importing your CLIs' history…".to_owned());
            }
            (None, ImportStatus::Running { done, of }) => {
                parts.push(format!(
                    "importing your CLIs' history · {done} of {of} files"
                ));
            }
            (None, ImportStatus::Failed { why }) => {
                parts.push(format!("the import stopped: {why} · i tries again"));
            }
            _ => {
                if !imported.is_empty() {
                    parts.push(format!("imported: {}", listed(&imported)));
                }
                if !through.is_empty() {
                    parts.push(format!("through Argonauts Code: {}", listed(&through)));
                }
            }
        }
        parts.join(" · ")
    }

    /// The one question: whether to import what the CLIs recorded on this computer.
    fn question(&self, kit: &Kit<'_>, width: usize) -> Drawn {
        let frame = Frame {
            active: true,
            ..Frame::view(width)
        };
        let inner = frame.inner();
        let mut body = Drawn::new();
        body.push(vec![
            kit.span(kit.glyphs.the_mod, Role::Question),
            Span::raw(" "),
            kit.strong("your history"),
        ]);
        let style = kit.style(Role::Text);
        for line in crate::text::wrap_text(QUESTION, style, inner, &[], &[]) {
            body.push(line.spans);
        }
        body.push(vec![kit.dim(PROMISE.to_owned())]);
        body.push(Vec::new());
        for (at, choice) in ["Yes, import it", "Not now"].iter().enumerate() {
            let words = format!("{}. {choice}", at + 1);
            body.push(if at == self.answer {
                vec![Span::styled(format!("❯ {words}"), kit.bold(Role::Question))]
            } else {
                vec![Span::raw(format!("  {words}"))]
            });
        }
        frame.draw(kit, body)
    }

    /// The keys, in display order. Behaviors' own come first wherever it shows.
    pub fn keys(&self) -> Vec<String> {
        if self.asking() {
            return owned(&["↑↓  choose", "enter answer", "1 2 answer"]);
        }
        if self.sub == SubTab::Behaviors && self.level == Level::Cards {
            return Vec::new();
        }
        // The period's key comes first, as on every tab that has periods.
        let mut keys = if self.sub == SubTab::Behaviors {
            self.behaviors.own_keys()
        } else {
            owned(&[super::periods::CYCLE_HINT])
        };
        if self.level == Level::Tabs {
            keys.extend(owned(&[
                "tab · ←  →  tabs",
                "↓  Overview, Models, Behaviors",
            ]));
        } else {
            if self.sub == SubTab::Models && !self.page.is_empty() {
                keys.push(self.page.clone());
            }
            // ↓ reaches Behaviors' groupings first and its map after them, which nothing on
            // the screen shows.
            if self.sub == SubTab::Behaviors && self.behaviors.map().is_some() {
                keys.push(DOWN_TO_THE_MAP.to_owned());
            }
            keys.push("↑  tabs".to_owned());
            if self.sub == SubTab::Overview {
                keys.push("tab Overview, Models, Behaviors".to_owned());
            }
        }
        keys.push("ctrl+s to copy".to_owned());
        if matches!(
            self.data.as_ref().map(|data| &data.import),
            Some(ImportStatus::Declined | ImportStatus::Failed { .. })
        ) {
            keys.push("i import".to_owned());
        }
        keys
    }

    /// The tab's own keys. Anything else is the panel's.
    pub fn key(&mut self, code: KeyCode, ctrl: bool, names: &DisplayNames) -> Option<HomeKey> {
        if ctrl {
            return match code {
                KeyCode::Char('s') => Some(HomeKey::Copy(self.summary_text(names)?)),
                _ => None,
            };
        }
        if self.asking() {
            return self.question_key(code);
        }
        if self.sub == SubTab::Behaviors {
            return match self.behaviors.key(code)? {
                MapKey::Moved => Some(HomeKey::Moved),
                MapKey::Ask(request) => Some(HomeKey::Ask(request)),
                MapKey::Copy(text) => Some(HomeKey::Copy(text)),
                MapKey::Send(command) => Some(HomeKey::Send(command)),
            };
        }
        if code == KeyCode::Char('i') {
            return Some(HomeKey::Ask(self.request(Some(true))));
        }
        if self.level != Level::Cards || self.sub != SubTab::Models {
            return None;
        }
        let models = self.data.as_ref().map_or(0, |data| data.models.len());
        let last = last_page(models);
        match code {
            KeyCode::Up => self.picked = self.picked.saturating_sub(2),
            KeyCode::Down => self.picked = (self.picked + 2).min(last),
            KeyCode::Home => self.picked = 0,
            KeyCode::End => self.picked = last,
            KeyCode::Enter => {
                let model = self.data.as_ref()?.models.get(self.picked)?;
                return Some(HomeKey::OpenModel {
                    agent: model.agent.clone(),
                    model: model.model.clone(),
                    period: match self.period {
                        OverviewPeriod::All => UsagePeriod::All,
                        OverviewPeriod::Month => UsagePeriod::Month,
                        OverviewPeriod::Week => UsagePeriod::Week,
                    },
                });
            }
            _ => return None,
        }
        self.turn_page();
        Some(HomeKey::Moved)
    }

    fn question_key(&mut self, code: KeyCode) -> Option<HomeKey> {
        let answer = match code {
            KeyCode::Up | KeyCode::Down => {
                self.answer = 1 - self.answer.min(1);
                return Some(HomeKey::Moved);
            }
            KeyCode::Enter => self.answer == 0,
            KeyCode::Char('1') => true,
            KeyCode::Char('2') => false,
            _ => return None,
        };
        // Answered here, so the question goes at once rather than when the numbers come back.
        if let Some(data) = &mut self.data {
            data.import = if answer {
                ImportStatus::Running { done: 0, of: 0 }
            } else {
                ImportStatus::Declined
            };
        }
        Some(HomeKey::Ask(self.request(Some(answer))))
    }

    /// The period `by` along from this one, wrapping, asked for at once. The panel reads the
    /// period's keys for every tab that has periods and steps this tab's through here.
    pub fn step_period(&mut self, by: isize) -> HomeKey {
        if self.sub == SubTab::Behaviors {
            return match self.behaviors.step_period(by) {
                Some(MapKey::Ask(request)) => HomeKey::Ask(request),
                _ => HomeKey::Moved,
            };
        }
        self.period = super::periods::cycled(&OverviewPeriod::ALL, self.period, by);
        self.picked = 0;
        HomeKey::Ask(self.request(None))
    }

    fn turn_page(&mut self) {
        let models = self.data.as_ref().map_or(0, |data| data.models.len());
        self.page = page_word(models, self.picked).unwrap_or_default();
    }

    /// Ctrl+S: what the sub-tab shows, as plain lines, with each bar as its shares in words.
    pub fn summary_text(&self, names: &DisplayNames) -> Option<String> {
        if self.sub == SubTab::Behaviors {
            self.behaviors.map()?;
            let mut lines = vec!["Argonauts Code · Behaviors".to_owned(), String::new()];
            lines.extend(self.behaviors.spoken_here(names));
            return Some(lines.join("\n"));
        }
        let data = self.data.as_ref()?;
        let theme = crate::theme::Theme::dark().with_level(crate::theme::ColorLevel::None);
        let kit = Kit::new(&theme);
        let width = 100;
        let mut lines = vec![format!(
            "Argonauts Code · {} · {}",
            self.sub.title(),
            data.period.title()
        )];
        let mut blocks = Vec::new();
        match self.sub {
            SubTab::Overview => {
                let numbers = Numbers { data, names };
                blocks.push(numbers.groups(&kit, width));
                let mut shares = Drawn::new();
                for line in numbers.shares() {
                    shares.push(vec![Span::raw(line)]);
                }
                blocks.push(shares);
                blocks.push(numbers.words(&kit, width, self.source(data, names)));
            }
            SubTab::Models => {
                let models = Models {
                    data,
                    names,
                    shades: &model_colors(data, names),
                };
                blocks.extend(models.favourite(&kit, width));
                for first in (0..data.models.len()).step_by(models::PAGE) {
                    blocks.push(models.columns(&kit, width, first));
                }
            }
            // Its own words, above.
            SubTab::Behaviors => {}
        }
        for block in blocks {
            lines.push(String::new());
            for line in block.lines {
                let text: String = line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect();
                lines.push(text.trim_end().to_owned());
            }
        }
        Some(lines.join("\n"))
    }
}

/// The tab's numbers as plain lines, for a screen reader.
pub fn spoken(data: &OverviewData, names: &DisplayNames) -> Vec<String> {
    let tab = HomeTab {
        data: Some(data.clone()),
        period: data.period,
        ..HomeTab::default()
    };
    tab.summary_text(names)
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

const QUESTION: &str = "Claude Code and Codex keep a record of every answer on this computer. \
Import the date, CLI, model, folder and token counts of each, so the Overview starts with your \
history?";
const PROMISE: &str = "What was said is skipped as each line is read and never kept, and nothing leaves this \
     computer.";

fn owned(keys: &[&str]) -> Vec<String> {
    keys.iter().map(|key| (*key).to_owned()).collect()
}

fn note(kit: &Kit<'_>, words: &str) -> Drawn {
    let mut block = Drawn::new();
    block.push(vec![kit.dim(words.to_owned())]);
    block
}

#[cfg(test)]
pub(crate) mod sample;
#[cfg(test)]
mod tests;
