//! The Overview tab: every CLI's work on this computer in one picture. A row of periods with
//! the import's note, six tiles, the year of days with the hours and each CLI's share beside
//! it, tokens a week by CLI, the models, where the tokens went, what ran through Argonauts, a
//! few things worth knowing, and a comparison. The first time it opens it asks once whether to
//! import what Claude Code and Codex recorded; the import runs in the app, and the tab shows
//! how far it got.

mod cards;
mod look;
mod year;

use argonauts_core::api::UsagePeriod;
use argonauts_core::app::ViewRequest;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::overview::{ImportStatus, OverviewData, OverviewPeriod};
use argonauts_core::settings::overview::YearColors;
use crossterm::event::KeyCode;
use ratatui::text::Span;

use self::cards::{Cards, header, since_word};
use self::look::{big, cli_word, model_word, share, thousands};
use self::year::{YEAR_WIDTH, Year, hours_and_share};
use super::subtabs::{self, Level};
use crate::text::{spans_width, truncate_spans};
use crate::theme::Role;
use crate::widgets::{Drawn, Frame, Kit, side_by_side, spread};

/// The note beside the tabs.
pub const NOTE: &str = "every CLI on this computer";
/// The least room the hours and the share take beside the year.
const BESIDE_YEAR: usize = 26;

/// What the Overview tab keeps while the panel is open.
#[derive(Debug, Clone, Default)]
pub struct HomeTab {
    pub period: OverviewPeriod,
    pub level: Level,
    /// The model row the keys are on.
    pub picked: usize,
    /// The import question's choice: yes first.
    answer: usize,
    /// `c` turned the year's colours from what the setting says.
    colors: Option<YearColors>,
    data: Option<OverviewData>,
    /// What the import found, said once it finishes while the tab is open.
    found: Option<String>,
    /// How many blocks the last frame drew under the models table, each a stop for the keys
    /// after the last model.
    below: std::cell::Cell<usize>,
}

/// What a key on the tab asks of the panel.
#[derive(Debug, Clone, PartialEq)]
pub enum HomeKey {
    Moved,
    Ask(ViewRequest),
    Copy(String),
    /// Enter on a model: Stats opens it, for the same span of days.
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
        }
    }

    /// The import's question shows, and takes the keys it answers with.
    pub fn asking(&self) -> bool {
        matches!(
            self.data.as_ref().map(|data| &data.import),
            Some(ImportStatus::Unasked)
        )
    }

    /// The periods, the panel's sub-tabs on this tab, and the one shown.
    pub fn periods(&self) -> (Vec<String>, usize) {
        let titles = OverviewPeriod::ALL.map(|period| period.title().to_owned());
        (titles.to_vec(), self.period_index())
    }

    fn period_index(&self) -> usize {
        OverviewPeriod::ALL
            .iter()
            .position(|period| *period == self.period)
            .unwrap_or(0)
    }

    /// The periods, the one shown filled, and the import's note at the right, cut first.
    pub fn about(&self, kit: &Kit<'_>, names: &DisplayNames, width: usize) -> Vec<Span<'static>> {
        let titles = OverviewPeriod::ALL.map(OverviewPeriod::title);
        let chosen = self.period_index();
        let mut left = vec![Span::raw(" ")];
        left.extend(subtabs::row(
            kit,
            &titles,
            chosen,
            self.level == Level::SubTabs,
        ));
        let note = self.note(names);
        let room = width.saturating_sub(spans_width(&left) + 3);
        if note.is_empty() || room < 8 {
            return truncate_spans(&left, width);
        }
        let right = truncate_spans(&[kit.dim(note)], room);
        let mut right = right;
        right.push(Span::raw(" "));
        spread(left, right, width)
    }

    /// `imported · Claude Code since Oct 2025 · Codex since Feb · agy since Jun`, or how far
    /// the import is, or what it found.
    fn note(&self, names: &DisplayNames) -> String {
        if let Some(found) = &self.found {
            return format!("imported {found}");
        }
        let Some(data) = &self.data else {
            return String::new();
        };
        let since = |source: &argonauts_core::overview::CliSource| {
            let cli = cli_word(names, &source.agent);
            match source.since {
                Some(day) if source.imported => {
                    format!("{cli} since {}", since_word(day, data.today))
                }
                Some(day) => format!(
                    "{cli} through Argonauts since {}",
                    since_word(day, data.today)
                ),
                None => cli,
            }
        };
        let sources: Vec<String> = data.sources.iter().map(since).collect();
        match &data.import {
            ImportStatus::Running { of: 0, .. } => "importing your CLIs' history…".to_owned(),
            ImportStatus::Running { done, of } => {
                format!("importing your CLIs' history · {done} of {of} files")
            }
            ImportStatus::Failed { why } => format!("the import stopped: {why}"),
            ImportStatus::Done if data.sources.iter().any(|source| source.imported) => {
                format!("imported · {}", sources.join(" · "))
            }
            _ if sources.is_empty() => String::new(),
            _ => format!("through Argonauts · {}", sources.join(" · ")),
        }
    }

    /// The tab's blocks top to bottom, and the one holding the row the keys are on.
    pub fn blocks(&self, kit: &Kit<'_>, names: &DisplayNames, width: usize) -> (Vec<Drawn>, usize) {
        let Some(data) = &self.data else {
            return (vec![note(kit, "Reading every CLI's history…")], 0);
        };
        let mut blocks = Vec::new();
        if data.import == ImportStatus::Unasked {
            blocks.push(self.question(kit, width));
        }
        if data.total.total() == 0 && data.import != ImportStatus::Unasked {
            blocks.push(note(
                kit,
                match data.import {
                    ImportStatus::Running { .. } => "Importing… the cards fill in as it reads.",
                    ImportStatus::Declined => {
                        "Nothing in this period yet. i imports what your CLIs recorded."
                    }
                    _ => "Nothing in this period yet.",
                },
            ));
        }
        let look = Cards { data, names };
        blocks.extend(look.tiles(kit, width));
        blocks.push(self.year_and_share(kit, data, names, width));
        blocks.push(look.weeks(kit, width));
        let table = blocks.len();
        let models = data.models.len();
        let in_cards = self.level == Level::Cards && !self.asking();
        let picked = (in_cards && self.picked < models.max(1))
            .then(|| self.picked.min(models.saturating_sub(1)));
        blocks.push(look.models(kit, width, picked));
        blocks.extend(look.folders_and_argonauts(kit, width));
        blocks.extend(look.worth_knowing(kit, width));
        blocks.extend(look.comparison(kit, width));
        let below = blocks.len() - table - 1;
        self.below.set(below);
        // Past the last model, the keys stop on each block under the table in turn.
        let focus = match self.picked.checked_sub(models.max(1)) {
            Some(past) if in_cards => table + 1 + past.min(below.saturating_sub(1)),
            _ => table,
        };
        (blocks, focus)
    }

    /// The year with the hours and the share beside it where both fit, else under it.
    fn year_and_share(
        &self,
        kit: &Kit<'_>,
        data: &OverviewData,
        names: &DisplayNames,
        width: usize,
    ) -> Drawn {
        let colors = self.colors.unwrap_or(data.year_colors);
        let year = Year {
            data,
            names,
            colors,
        };
        let room = width.saturating_sub(2);
        if room >= YEAR_WIDTH + 3 + BESIDE_YEAR {
            let mut left = year.draw(kit, YEAR_WIDTH);
            left.indent(1);
            let right = hours_and_share(kit, data, names, room - YEAR_WIDTH - 3);
            return side_by_side(left, right, 1 + YEAR_WIDTH + 3);
        }
        let mut drawn = year.draw(kit, room);
        drawn.push(Vec::new());
        drawn.append(hours_and_share(kit, data, names, room));
        drawn.indent(1);
        drawn
    }

    /// The one question: whether to import what the CLIs recorded on this computer.
    fn question(&self, kit: &Kit<'_>, width: usize) -> Drawn {
        let frame = Frame {
            active: true,
            ..Frame::view(width)
        };
        let inner = frame.inner();
        let mut body = Drawn::new();
        body.push(header(kit, "your history", "asked once", inner));
        let style = kit.style(Role::Text);
        for line in crate::text::wrap_text(QUESTION, style, inner, &[], &[]) {
            body.push(line.spans);
        }
        body.push(vec![kit.dim(PROMISE.to_owned())]);
        body.push(Vec::new());
        for (at, choice) in ["Yes, import it", "Not now"].iter().enumerate() {
            let words = format!("{}. {choice}", at + 1);
            body.push(if at == self.answer {
                vec![Span::styled(format!("❯ {words}"), kit.bold(Role::Card))]
            } else {
                vec![Span::raw(format!("  {words}"))]
            });
        }
        frame.draw(kit, body)
    }

    /// The keys, in display order.
    pub fn keys(&self) -> Vec<&'static str> {
        if self.asking() {
            return vec!["↑↓ choose", "enter answer", "1 2 answer"];
        }
        let mut keys = vec!["←→ tabs", "↓ periods", "↓ cards", "enter open"];
        if matches!(
            self.data.as_ref().map(|data| &data.import),
            Some(ImportStatus::Declined | ImportStatus::Failed { .. })
        ) {
            keys.push("i import");
        }
        keys.extend(["c colours", "ctrl+s copies a summary"]);
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
        match code {
            KeyCode::Left if self.level == Level::SubTabs => return Some(self.step_period(-1)),
            KeyCode::Right if self.level == Level::SubTabs => return Some(self.step_period(1)),
            KeyCode::Down | KeyCode::Enter if self.level != Level::Cards => {
                self.level = self.level.down();
                return Some(HomeKey::Moved);
            }
            KeyCode::Up if self.level != Level::Cards || self.picked == 0 => {
                self.level = self.level.up();
                return Some(HomeKey::Moved);
            }
            KeyCode::Char('c') => {
                let shown = self
                    .colors
                    .or(self.data.as_ref().map(|data| data.year_colors))
                    .unwrap_or_default();
                self.colors = Some(shown.next());
                return Some(HomeKey::Moved);
            }
            KeyCode::Char('i') => {
                return Some(HomeKey::Ask(self.request(Some(true))));
            }
            _ => {}
        }
        if self.level != Level::Cards {
            return None;
        }
        // The models, then each block under the table, are the stops.
        let models = self.data.as_ref().map_or(0, |data| data.models.len());
        let last = (models.max(1) + self.below.get()).saturating_sub(1);
        self.picked = self.picked.min(last);
        match code {
            KeyCode::Up => self.picked = self.picked.saturating_sub(1),
            KeyCode::Down => self.picked = (self.picked + 1).min(last),
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

    /// The next period, wrapping at the ends, asked for at once.
    pub fn step_period(&mut self, by: isize) -> HomeKey {
        let at = self.period_index();
        self.period = OverviewPeriod::ALL[subtabs::step(at, by, OverviewPeriod::ALL.len())];
        self.picked = 0;
        HomeKey::Ask(self.request(None))
    }

    /// Ctrl+S: the period in a few plain lines, without the folders, which stay on this
    /// computer.
    pub fn summary_text(&self, names: &DisplayNames) -> Option<String> {
        let data = self.data.as_ref()?;
        let total = data.total.total();
        let mut lines = vec![format!(
            "Argonauts Code · {} · every CLI on this computer",
            data.period.title()
        )];
        lines.push(format!(
            "{} tokens · API value {} at list prices · {} sessions · {} active days of {} · streak {} (longest {})",
            big(total),
            look::dollars(data.api_value),
            thousands(u64::from(data.sessions())),
            data.active_days,
            data.span_days,
            data.streak,
            data.longest_streak,
        ));
        let clis: Vec<String> = data
            .by_cli()
            .iter()
            .map(|(agent, tokens)| format!("{} {}", cli_word(names, agent), share(*tokens, total)))
            .collect();
        if !clis.is_empty() {
            lines.push(format!("By CLI: {}", clis.join(" · ")));
        }
        for model in &data.models {
            lines.push(format!(
                "{} on {}: {} tokens, {}, {} sessions",
                model_word(names, &model.agent, model.model.as_deref()),
                cli_word(names, &model.agent),
                big(model.tokens.total()),
                share(model.tokens.total(), total),
                model.sessions,
            ));
        }
        if let Some(said) = cards::input_and_output(data) {
            lines.push(said);
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

fn note(kit: &Kit<'_>, words: &str) -> Drawn {
    let mut block = Drawn::new();
    block.push(vec![kit.dim(format!(" {words}"))]);
    block
}

#[cfg(test)]
pub(crate) mod sample;
#[cfg(test)]
mod tests;
