//! The Context tab: Claude Code's own `/context` for the main session, and the crew in the
//! same quiet form.
//!
//! Top to bottom, in Claude Code 2.1.304's words and order: the grid beside the model, how
//! much of the window is used and what fills it, the auto-compact window, then MCP tools,
//! custom agents, memory files and skills, each folded to one line. One more folded line
//! follows for each run's crew. `/context all`, or Enter on a folded line, opens a list to a
//! line a server, group, file or agent, and Enter on an agent shows its own block in the
//! main session's form. The context card ends the tab, as it draws above the input.

mod block;
mod crew;
#[cfg(test)]
pub(crate) mod example;
mod lists;
mod pane;
mod pinned;
mod print;
mod starting;
mod words;

use argonauts_core::RunId;
use argonauts_core::app::views::{ContextRow, ContextView as ContextData};
use argonauts_core::base::names::ModelRef;
use argonauts_core::look::display_names::DisplayNames;
use ratatui::text::{Line, Span};
use time::OffsetDateTime;

use self::block::{Block, INDENT};
use self::crew::{Agent, Crew, Filled};
use self::starting::Started;
use crate::base::format;
use crate::base::kit::logo::{ModelLabel, NoPicture};
use crate::base::kit::{ContextCard, Drawn, Form, Frame, Kit};
use crate::base::text::truncate_spans;
use crate::base::theme::Role;
use crate::frame::state::ContextCount;
use crate::frame::state::Places;

pub use crew::CrewFacts;
pub use pane::{ContextPane, List, Section, Target, blocks};
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
    /// Where paths are shown from.
    pub places: &'a Places,
    /// Now, by the clock the rows were stamped with.
    pub now: OffsetDateTime,
    /// A run's name, as the conversation knows it.
    pub run_title: &'a dyn Fn(&RunId) -> String,
    /// What the screen knows of the crew's sessions beyond their rows.
    pub facts: &'a CrewFacts,
    /// The lists open, and the agent whose block shows.
    pub pane: &'a ContextPane,
}

impl ContextTab<'_> {
    /// The conversation's own row, among the live sessions.
    fn main_row(&self) -> Option<&ContextRow> {
        self.view.rows.iter().find(|row| row.run.is_none())
    }

    /// The tab's blocks, top to bottom, which stand one blank row apart unless a block joins
    /// the one above, the context card last. With an agent open, its own block alone.
    pub fn sections(&self, kit: &Kit<'_>, width: usize) -> Vec<Section> {
        let mut sections = self.printed_sections(kit, width);
        if self.pane.opened().is_none()
            && let Some(card) = self.card()
        {
            sections.extend(fitted(
                vec![Section::of(card.card(kit, Form::Full, Frame::view(width)))],
                width,
            ));
        }
        sections
    }

    /// The blocks as `/context` prints them into the conversation, as Claude Code's own
    /// does: the tab's without the card.
    pub(super) fn printed_sections(&self, kit: &Kit<'_>, width: usize) -> Vec<Section> {
        let crews = self.crews();
        if let Some((run, label)) = self.pane.opened() {
            let opened = crews
                .iter()
                .filter(|crew| crew.run == run)
                .flat_map(|crew| &crew.agents)
                .find(|agent| agent.label == label);
            if let Some(agent) = opened {
                let drawn = crew::agent_block(kit, agent, self.names, self.facts, width);
                return fitted(vec![Section::of(drawn)], width);
            }
        }
        let mut sections = vec![Section::of(self.main_block(kit, width, !crews.is_empty()))];
        let mut folded = false;
        if let Some(started) = self.started() {
            for drawn in [started.files(kit), started.tools(kit)] {
                if drawn.height() > 0 {
                    folded = true;
                    sections.push(Section::of(drawn));
                }
            }
        }
        if let Some(count) = self.count {
            let usage = &count.usage;
            if let Some(window) = compact_window(usage) {
                let mut drawn = Drawn::new();
                drawn.push(vec![
                    Span::raw(" ".repeat(INDENT)),
                    Span::styled("Auto-compact window: ", kit.bold(Role::Text)),
                    kit.dim(window),
                ]);
                sections.push(Section::of(drawn));
            }
            let lists = lists::of_usage(usage, self.places);
            let names = lists
                .iter()
                .map(|listed| listed.longest_name())
                .max()
                .unwrap_or(0)
                + 2;
            for listed in &lists {
                let open = self.pane.is_open(&listed.list);
                folded |= !open;
                sections.push(Section {
                    target: Some(Target::List(listed.list.clone())),
                    ..Section::of(listed.drawn(kit, open, names))
                });
            }
        }
        for crew in &crews {
            let list = List::Crew(crew.run.clone());
            let open = self.pane.is_open(&list);
            folded |= !open;
            let mut head = Drawn::new();
            head.push(crew.title(kit));
            if !open {
                head.push(crew.folded(kit, self.now));
            }
            sections.push(Section {
                target: Some(Target::List(list)),
                ..Section::of(head)
            });
            if open {
                let lines = crew.lines(kit, self.names, self.now, width);
                for (agent, line) in crew.agents.iter().zip(lines) {
                    let mut drawn = Drawn::new();
                    drawn.push(line);
                    sections.push(Section {
                        target: Some(agent_target(agent)),
                        joined: true,
                        ..Section::of(drawn)
                    });
                }
            }
        }
        if folded {
            let mut drawn = Drawn::new();
            drawn.push(vec![
                Span::raw(" ".repeat(INDENT)),
                kit.dim("/context all to expand"),
            ]);
            sections.push(Section::of(drawn));
        }
        fitted(sections, width)
    }

    /// The context card of the conversation's session, as the band above the input draws it.
    fn card(&self) -> Option<ContextCard> {
        self.count
            .map(pinned::conversation)
            .or_else(|| self.started().map(|started| started.card()))
    }

    /// The main session's start, while its CLI hasn't counted it by category: before the
    /// session, while it loads, and for a CLI that says only how full it is.
    fn started(&self) -> Option<Started<'_>> {
        if self.count.is_some() {
            return None;
        }
        let start = self.view.main_start.as_ref()?;
        let row = self.main_row();
        if row.is_some_and(|row| row.worker.agent != start.agent) {
            return None;
        }
        let cli_window =
            (self.view.main_window.as_ref()).is_some_and(|window| window.cli.is_some());
        Some(Started::of(start, row, self.names, cli_window, self.now))
    }

    /// `Context Usage` and the main session's block: as its CLI counted it, as its row says
    /// it where its CLI says only how full, or a line that there is none yet.
    fn main_block(&self, kit: &Kit<'_>, width: usize, has_crew: bool) -> Drawn {
        let mut drawn = Drawn::new();
        drawn.push(crew::usage_title(kit, None));
        if let Some(count) = self.count {
            let usage = &count.usage;
            let agent = self
                .main_row()
                .map(|row| row.worker.agent.as_str())
                .or(self.main.map(|main| main.agent.as_str()))
                .unwrap_or("claude-code");
            let (parts, free, buffer) = block::of_usage(kit, usage);
            let worker = ModelRef::new(agent, usage.model.clone());
            let label = ModelLabel::of(self.names, &worker, kit.theme, NoPicture::Nothing);
            let logo = label.whose.clone();
            let block = Block {
                model: label
                    .beside_picture()
                    .or_else(|| self.names.model(agent, usage.model.as_deref())),
                model_id: usage.model.as_deref(),
                logo: Some(logo),
                used: usage.total_tokens,
                window: Some(usage.window),
                percent: Some(u64::from(usage.percentage)),
                heading: "Estimated usage by category".to_owned(),
                parts,
                free,
                buffer,
                grid: Some(&usage.grid),
                usage: Some(usage),
                notes: Vec::new(),
                parts_ours: false,
                used_ours: false,
                used_tail: String::new(),
            };
            drawn.append(block::rows(kit, &block, width));
            return drawn;
        }
        if let Some(started) = self.started() {
            drawn.append(block::rows(kit, &started.block(kit, self.names), width));
            return drawn;
        }
        match self.main_row() {
            Some(row) => {
                let model = ModelLabel::of(self.names, &row.worker, kit.theme, NoPicture::Nothing)
                    .beside_picture()
                    .or_else(|| (self.names).model(&row.worker.agent, row.worker.model.as_deref()))
                    .unwrap_or_else(|| self.names.cli(&row.worker.agent));
                let filled = Filled {
                    row,
                    model,
                    sent: None,
                    notes: Vec::new(),
                };
                drawn.append(crew::row_usage(kit, &filled, self.names, width));
            }
            None => {
                let line = if has_crew {
                    "No session of this conversation yet: your request went straight to a run."
                } else {
                    "No session of this conversation yet."
                };
                drawn.push(vec![Span::raw(" ".repeat(INDENT)), kit.dim(line)]);
            }
        }
        drawn
    }

    /// Each run's crew, in the order the runs first appear.
    fn crews(&self) -> Vec<Crew<'_>> {
        let mut runs: Vec<&RunId> = Vec::new();
        for run in self.view.rows.iter().filter_map(|row| row.run.as_ref()) {
            if !runs.contains(&run) {
                runs.push(run);
            }
        }
        let no_conversation = self.count.is_none() && self.main_row().is_none();
        runs.into_iter()
            .map(|run| Crew {
                run,
                title: (self.run_title)(run),
                agents: crew::agents(&self.view.rows, run),
                no_conversation,
            })
            .collect()
    }
}

/// What Enter opens on an agent's line.
fn agent_target(agent: &Agent<'_>) -> Target {
    Target::Agent {
        run: agent.run.clone(),
        label: agent.label.to_owned(),
    }
}

/// `1m tokens`, as Claude Code says the window its session compacts in, where something set
/// it. Claude Code says nothing of a window it picks itself.
fn compact_window(usage: &argonauts_core::runs::events::context::ContextUsage) -> Option<String> {
    let window = format::context_tokens(usage.raw_window);
    match usage.auto_compact_source.as_str() {
        "auto" => None,
        "experiment" | "clientdata" => Some(format!("auto ({window} tokens)")),
        "unknown-model" => Some(format!(
            "{window} tokens (default for an unrecognized model)"
        )),
        _ => Some(format!("{window} tokens")),
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

/// `1 session · 182.5k in context · 7 ended`, for the row under the tabs: the live sessions
/// and what they hold, then how many crew agents ended, their tries and tie-breakers counted
/// as one. With nothing live, `none open · 7 ended`. `count` is the conversation's context as
/// its CLI last counted it.
pub fn summary(view: &ContextData, count: Option<&ContextCount>) -> String {
    let rows = &view.rows;
    let live: Vec<&ContextRow> = rows.iter().filter(|row| row.ended.is_none()).collect();
    let used: u64 = live.iter().map(|row| row.used).sum();
    let (_, ended) = crew::tally(rows);
    let mut summary = match live.as_slice() {
        [] => "none open".to_owned(),
        // The conversation's session, started and not yet said how full.
        [one] if one.run.is_none() && one.used == 0 => "1 session \u{b7} loading".to_owned(),
        // One session reads as its block's line does, in the block's window: marked when the
        // count is ours, and in 1,024s beside a window counted in them.
        [one] => {
            let window = match one.run {
                None => main_window(view, count, one),
                Some(_) => one.window,
            };
            format!(
                "1 session \u{b7} {}{} in context",
                block::mark(one.estimated),
                block::beside(one.used, window.unwrap_or_default())
            )
        }
        several => format!(
            "{} sessions \u{b7} {} in context",
            several.len(),
            format::context_tokens(used)
        ),
    };
    if ended > 0 {
        summary.push_str(&format!(" \u{b7} {ended} ended"));
    }
    summary
}

/// The window the conversation's block counts `row` in, as the tab draws it: its CLI's count's,
/// else as its start gives it while the start is this session's, else the row's own. A CLI
/// that never says its window, such as agy, counts in our list's.
fn main_window(view: &ContextData, count: Option<&ContextCount>, row: &ContextRow) -> Option<u64> {
    if let Some(count) = count {
        return Some(count.usage.window);
    }
    match &view.main_start {
        Some(start) if start.agent == row.worker.agent => starting::window_of(start, Some(row)),
        _ => row.window,
    }
}

#[cfg(test)]
mod tests;
