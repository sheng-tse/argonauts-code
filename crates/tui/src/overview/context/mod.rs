//! The Context tab: everything Claude Code's /context shows of the main session, in its
//! words, and what Argonauts Code adds to it, then each crew agent's own window.
//!
//! The main session's count is its CLI's own, kept whole on the screen's state. Top to bottom:
//! the title with the model and the totals, the grid beside its legend, how far the session
//! is from compacting, MCP tools, custom agents, memory files and skills, what the messages
//! are made of, and Claude Code's suggestions with what each would save. Every list shows in
//! full. A session whose CLI says only how full it is draws Claude Code's grid in the CLI's
//! colour, with what Argonauts Code knows of it. The run you follow lists every session it
//! started, live or ended, and stays listed once it finishes.

mod crew;
#[cfg(test)]
pub(crate) mod example;
mod how_full;
mod lists;
mod messages;
mod pane;
mod pinned;
mod place;
mod print;
mod suggestions;
mod usage;
mod words;

use argonauts_core::RunId;
use argonauts_core::app::SessionCounted;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::ModelRef;
use argonauts_core::overview::ModelTokens;
use argonauts_core::views::{ContextRow, ContextView as ContextData};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use time::OffsetDateTime;

use self::crew::Crew;
use self::how_full::HowFull;
use self::place::{Columns, at, ending, place};
use self::usage::{Title, bold, level_role};
use super::shades::ModelColors;
use crate::bridge::tools::Places;
use crate::format;
use crate::kit::{Drawn, Kit};
use crate::state::{ContextCount, LastStep};
use crate::text::{sanitize_line, spans_width, truncate_spans};
use crate::theme::Role;

pub use lists::Fold;
pub use pane::{ContextPane, Section};
pub use pinned::{conversation, session_use};
pub use print::ContextPrinted;

/// What the tab draws from.
#[derive(Clone, Copy)]
pub struct ContextTab<'a> {
    /// Every live session, the conversation's own first, and the ended ones of the run you
    /// follow.
    pub view: &'a ContextData,
    pub names: &'a DisplayNames,
    /// The conversation's context as its CLI last counted it.
    pub count: Option<&'a ContextCount>,
    /// Who answers the conversation, for the model's name while no session row says.
    pub main: Option<&'a ModelRef>,
    /// What a turn of the conversation adds to its context.
    pub per_turn: Option<u64>,
    /// The conversation session's last step, as its CLI said it.
    pub last_step: LastStep,
    /// Where paths are shown from.
    pub places: &'a Places,
    /// Now, by the clock the count was stamped with.
    pub now: OffsetDateTime,
    /// A run's name, as the conversation knows it.
    pub run_title: &'a dyn Fn(&RunId) -> String,
    /// Every model's tokens in all, which rank a CLI's models for the shades they draw in.
    pub ranked: &'a [ModelTokens],
    /// A long list of MCP tools shows every tool, as Enter on it asks.
    pub whole_tools: bool,
}

impl ContextTab<'_> {
    /// The conversation's own row, among the live sessions.
    fn main_row(&self) -> Option<&ContextRow> {
        self.view.rows.iter().find(|row| row.run.is_none())
    }

    /// The shade each model draws in, the one it has on every tab: its place among its CLI's
    /// models by their tokens in all. One the numbers haven't met comes after them, in the
    /// order the tab shows it.
    fn colors(&self) -> ModelColors<'_> {
        let main = self.main_row().map(|row| &row.worker).or(self.main);
        let shown = main
            .into_iter()
            .chain(self.view.rows.iter().map(|row| &row.worker));
        ModelColors::new(self.ranked, self.names)
            .with(shown.map(|worker| (worker.agent.as_str(), worker.model.as_deref())))
    }

    /// The colour a session's model draws in.
    fn model_style(&self, kit: &Kit<'_>, worker: &ModelRef) -> Style {
        let model = worker.model.as_deref();
        self.colors().style(kit, &worker.agent, model)
    }

    /// The tab's blocks, top to bottom, which stand one blank row apart. The main session's
    /// come first, then a header and a block an agent for each run's crew.
    pub fn sections(&self, kit: &Kit<'_>, width: usize) -> Vec<Section> {
        let columns = Columns::for_width(width);
        let mut sections = match self.count {
            Some(count) => self.counted(kit, &columns, count, self.main_agent()),
            None => match self.how_full(kit) {
                Some(told) => how_full::blocks(kit, &columns, &told)
                    .into_iter()
                    .map(Section::of)
                    .collect(),
                None => vec![Section::of(self.uncounted(kit, &columns))],
            },
        };
        let colors = self.colors();
        for crew in self.crews() {
            let own = |row: &ContextRow| {
                colors.style(kit, &row.worker.agent, row.worker.model.as_deref())
            };
            let local = |row: &ContextRow| self.view.runs_locally(&row.worker);
            sections.extend(crew::sections(
                kit, &columns, &crew, self.names, &own, &local, self.now,
            ));
        }
        fitted(sections, width)
    }

    /// The sessions of each run, in the order the runs first appear.
    fn crews(&self) -> Vec<Crew<'_>> {
        let mut crews: Vec<(&RunId, Crew<'_>)> = Vec::new();
        for row in &self.view.rows {
            let Some(run) = &row.run else {
                continue;
            };
            match crews.iter_mut().find(|(known, _)| *known == run) {
                Some((_, crew)) => crew.rows.push(row),
                None => crews.push((
                    run,
                    Crew {
                        title: (self.run_title)(run),
                        finished: self
                            .view
                            .finished
                            .iter()
                            .find(|finished| finished.run == *run)
                            .map(|finished| finished.at),
                        rows: vec![row],
                    },
                )),
            }
        }
        crews.into_iter().map(|(_, crew)| crew).collect()
    }

    /// `counted 6s ago`.
    fn counted_ago(&self, count: &ContextCount) -> String {
        self.counted_at(count.counted)
    }

    /// `counted 6s ago` for a count that came `at`, and `counted just now` within the second.
    fn counted_at(&self, at: OffsetDateTime) -> String {
        let seconds = (self.now - at).whole_seconds().max(0);
        let ago = match seconds {
            0 => return "counted just now".to_owned(),
            1..=59 => format!("{seconds}s"),
            60..=3_599 => format!("{}m", seconds / 60),
            _ => format!("{}h", seconds / 3_600),
        };
        format!("counted {ago} ago")
    }

    /// The CLI that answers the conversation.
    fn main_agent(&self) -> Option<&str> {
        self.main_row()
            .map(|row| row.worker.agent.as_str())
            .or(self.main.map(|main| main.agent.as_str()))
    }

    /// The model `agent`'s count names, by its name and in its colour.
    fn model(&self, kit: &Kit<'_>, agent: &str, id: Option<&str>) -> Option<(String, Style)> {
        let named = self.names.model(agent, id)?;
        let colors = self.colors().with([(agent, id)]);
        Some((named, colors.style(kit, agent, id)))
    }

    /// The main session where its CLI says how full it is and nothing of what fills it, once
    /// its window is known.
    fn how_full(&self, kit: &Kit<'_>) -> Option<HowFull<'_>> {
        // A resumed session's row says how it was left, as its record has it.
        let row = self
            .main_row()
            .filter(|row| !row.counts && row.categories.is_empty() && row.recorded.is_none())?;
        let window = row.window.filter(|window| *window > 0)?;
        let said = self.last_step.at.is_some();
        Some(HowFull {
            row,
            window,
            cli: self.names.cli(&row.worker.agent),
            model: self.model(kit, &row.worker.agent, row.worker.model.as_deref()),
            own: self.model_style(kit, &row.worker),
            counted: self.last_step.at.map(|at| self.counted_at(at)),
            last: said.then_some(self.last_step),
            window_from: self.view.main_window.as_ref(),
        })
    }

    /// One run session, opened from its row: a line that names it, then the blocks the main
    /// session has, from its last whole count.
    pub fn session(
        &self,
        kit: &Kit<'_>,
        width: usize,
        row: &ContextRow,
        counted: Option<&SessionCounted>,
    ) -> Vec<Section> {
        let columns = Columns::for_width(width);
        let own = self.model_style(kit, &row.worker);
        let mut named = vec![bold(kit, sanitize_line(&row.label).trim().to_owned())];
        named.push(kit.dim(" \u{b7} "));
        named.extend(crew::model(kit, row, self.names, own));
        let when = match &row.ended {
            Some(left) => format!(" \u{b7} ended {}", words::time_of(left.at, self.now)),
            None => " \u{b7} at work".to_owned(),
        };
        named.push(kit.dim(when));
        let mut head = Drawn::new();
        head.push(place([at(columns.indent(), named)]));
        let mut sections = vec![Section::of(head)];
        match counted {
            Some(counted) => {
                let count = ContextCount {
                    usage: (*counted.usage).clone(),
                    counted: counted.at,
                };
                let agent = Some(row.worker.agent.as_str());
                sections.extend(self.counted(kit, &columns, &count, agent));
            }
            None => {
                let cli = sanitize_line(&self.names.cli(&row.worker.agent))
                    .trim()
                    .to_owned();
                let mut note = Drawn::new();
                note.push(place([at(
                    columns.indent(),
                    vec![kit.dim(format!("{cli} never counted this session."))],
                )]));
                sections.push(Section::of(note));
            }
        }
        fitted(sections, width)
    }

    /// A session as its CLI counted it, block by block. `agent` is the CLI that counted it.
    fn counted(
        &self,
        kit: &Kit<'_>,
        columns: &Columns,
        count: &ContextCount,
        agent: Option<&str>,
    ) -> Vec<Section> {
        let usage = &count.usage;
        let no_compacting = suggestions::compacting_disabled();
        let found = suggestions::suggestions(usage, self.places, no_compacting);

        let mut top = usage::title(
            kit,
            columns,
            usage,
            &Title {
                counted: Some(self.counted_ago(count)),
                model: agent.and_then(|agent| self.model(kit, agent, usage.model.as_deref())),
                model_id: usage.model.as_deref(),
            },
        );
        if let Some(over) = usage::over_limit(usage, no_compacting) {
            top.push(place([at(
                columns.indent(),
                vec![kit.span(over, Role::Danger)],
            )]));
        }
        let (tools, fold) = lists::mcp_tools(kit, columns, usage, self.whole_tools);
        let mut sections = vec![
            Section::of(top),
            Section::of(usage::grid_and_legend(kit, columns, usage)),
            Section::of(usage::window_lines(kit, columns, usage, self.per_turn)),
            Section {
                fold,
                ..Section::of(tools)
            },
            Section::of(lists::setup(kit, columns, usage, self.places)),
            Section::of(messages::messages(kit, columns, usage, &found)),
        ];
        sections.extend(
            suggestions::blocks(kit, columns, usage, &found)
                .into_iter()
                .map(Section::of),
        );
        sections
    }

    /// The main session before its CLI has counted it, or where its CLI can't: the title, and
    /// where a session row says how full it is, its window as one bar.
    fn uncounted(&self, kit: &Kit<'_>, columns: &Columns) -> Drawn {
        let indent = columns.indent();
        let mut drawn = Drawn::new();
        let Some(row) = self.main_row() else {
            drawn.push(place([at(indent, vec![bold(kit, "Context Usage")])]));
            drawn.push(place([at(
                indent,
                vec![kit.dim("The main session is counted when an answer ends.")],
            )]));
            return drawn;
        };
        let own = self.model_style(kit, &row.worker);
        let level = crew::row_level(row);
        let mut totals = vec![Span::styled(crew::used_of(row), own), kit.dim(" tokens")];
        if let Some(percent) = crew::percent(row) {
            totals.push(kit.dim(" ("));
            totals.push(match level_role(level) {
                Some(role) => kit.span(percent, role),
                None => bold(kit, percent),
            });
            totals.push(kit.dim(")"));
        }
        drawn.push(place([
            at(indent, vec![bold(kit, "Context Usage")]),
            at(
                columns.legend.unwrap_or(indent + 16),
                crew::model(kit, row, self.names, own),
            ),
            ending(columns.edge, totals),
        ]));
        drawn.push(Vec::new());
        let mut under = crew::window_bar(kit, row, own).unwrap_or_default();
        if !under.is_empty() {
            under.push(Span::raw("  "));
        }
        let afresh = crew::afresh(kit, row);
        if !afresh.is_empty() {
            under.extend(afresh);
            under.push(Span::raw("   "));
        }
        match &row.recorded {
            Some(recorded) => under.push(kit.dim(words::as_left(recorded, &row.spent, self.now))),
            None => {
                let room = columns.width.saturating_sub(indent + spans_width(&under));
                under.extend(crew::filled_with(kit, row, self.names, room));
            }
        }
        drawn.push(place([at(indent, under)]));
        drawn
    }
}

/// The blocks that draw anything, each row cut to `width`.
fn fitted(mut sections: Vec<Section>, width: usize) -> Vec<Section> {
    sections.retain(|section| section.drawn.height() > 0);
    for section in &mut sections {
        for line in &mut section.drawn.lines {
            *line = Line::from(truncate_spans(&line.spans, width));
        }
    }
    sections
}

/// `3 sessions · 800k in context`, for the row under the tabs: the live sessions, then how
/// many ended, as `· 9 ended`.
pub fn summary(rows: &[ContextRow]) -> String {
    let (live, ended): (Vec<&ContextRow>, Vec<&ContextRow>) =
        rows.iter().partition(|row| row.ended.is_none());
    let used: u64 = live.iter().map(|row| row.used).sum();
    let sessions = match live.len() {
        1 => "1 session".to_owned(),
        n => format!("{n} sessions"),
    };
    let mut summary = format!("{sessions} \u{b7} {} in context", format::tokens(used));
    if !ended.is_empty() {
        summary.push_str(&format!(" \u{b7} {} ended", ended.len()));
    }
    summary
}

#[cfg(test)]
mod golden_how_full;
#[cfg(test)]
mod golden_tests;
#[cfg(test)]
mod tests;
