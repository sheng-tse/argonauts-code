//! The main session's block and card before its CLI's own count, and for a CLI that says only
//! how full the session is: what the session held at its start, every number ours and marked
//! `~`, with the rest of what it holds as its messages once it has said.

use argonauts_core::app::views::ContextRow;
use argonauts_core::base::names::ModelRef;
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::runs::events::context::{StartHolds, StartPart, StartedFrom, StartingCount};
use ratatui::text::Span;
use time::OffsetDateTime;

use super::block::{Block, INDENT, Part};
use super::lists;
use super::words::cc_style;
use crate::base::format::{self, Figures};
use crate::base::kit::context::{BeforeCount, Estimate};
use crate::base::kit::logo::{ModelLabel, NoPicture};
use crate::base::kit::{ContextCard, ContextPart, Drawn, Kit, Limit, LimitKind};

/// The colour Claude Code's messages draw in.
const MESSAGES_COLOR: &str = "purple_FOR_SUBAGENTS_ONLY";

/// Where the main session stands against its CLI's own count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    /// No session yet: the start as kept or as recorded.
    Before,
    /// The CLI is starting, from your first message until its count comes.
    Loading,
    /// A CLI that says only how full the session is has said.
    Filled,
    /// A resumed conversation, before its next turn: how full its record says it was.
    Recorded,
}

/// The main session's start and what its row says of it now.
pub(crate) struct Started<'a> {
    pub start: &'a StartingCount,
    pub row: Option<&'a ContextRow>,
    pub stage: Stage,
    pub window: u64,
    /// The CLI's name, such as `Codex`.
    pub cli: String,
    /// The window is the CLI's own figure, not our list's.
    pub cli_window: bool,
    /// Now, by the clock the rows were stamped with.
    pub now: OffsetDateTime,
}

impl<'a> Started<'a> {
    /// The stage `row` puts `start` in, where its window is known: loading until the CLI
    /// first says how full the session is. A CLI that counts by category is asked only while
    /// a turn runs and something shows the context, so between turns its row has a fill and
    /// no count, and draws filled. A resumed conversation's row stands as its record left it
    /// until a turn runs.
    pub fn of(
        start: &'a StartingCount,
        row: Option<&'a ContextRow>,
        names: &DisplayNames,
        cli_window: bool,
        now: OffsetDateTime,
    ) -> Option<Self> {
        let window = row
            .and_then(|row| row.window)
            .or(start.window)
            .filter(|window| *window > 0)?;
        let stage = match row {
            None => Stage::Before,
            Some(row) if row.recorded.is_some() && row.used > 0 => Stage::Recorded,
            Some(row) if row.recorded.is_some() => Stage::Before,
            Some(row) if row.used == 0 => Stage::Loading,
            Some(_) => Stage::Filled,
        };
        Some(Self {
            start,
            row,
            stage,
            window,
            cli: names.cli(&start.agent),
            cli_window,
            now,
        })
    }

    /// The row says how full the session is, beyond the start.
    fn said(&self) -> bool {
        matches!(self.stage, Stage::Filled | Stage::Recorded)
    }

    fn worker(&self) -> ModelRef {
        self.row.map_or_else(
            || ModelRef::new(self.start.agent.as_str(), self.start.model.clone()),
            |row| row.worker.clone(),
        )
    }

    /// The parts with their tokens and Claude Code's colour names, the messages last once
    /// the CLI has said how full the session is.
    fn parts(&self) -> Vec<(String, u64, String)> {
        let mut parts: Vec<(String, u64, String)> = self
            .start
            .parts
            .iter()
            .map(|part| (self.name_of(part), part.tokens, part.color.clone()))
            .collect();
        if self.said() {
            let messages = self.used().saturating_sub(self.start.used());
            if messages > 0 {
                parts.push(("Messages".to_owned(), messages, MESSAGES_COLOR.to_owned()));
            }
        }
        parts
    }

    /// A part by the CLI's own name for it, or by what it holds, with the CLI's name as it
    /// stands now.
    fn name_of(&self, part: &StartPart) -> String {
        match part.holds {
            Some(StartHolds::OwnPromptAndTools) => format!("{}'s own prompt and tools", self.cli),
            Some(StartHolds::InstructionFiles) => "Instruction files".to_owned(),
            Some(StartHolds::AppTools) => "Argonauts Code's tools".to_owned(),
            None => part.name.clone(),
        }
    }

    fn used(&self) -> u64 {
        match self.row.filter(|_| self.said()) {
            Some(row) => row.used,
            None => self.start.used(),
        }
    }

    /// What the CLI holds back for compacting, kept once it has said for a CLI that counts,
    /// whose window holds one.
    fn buffer(&self) -> u64 {
        let counts = self.row.is_some_and(|row| row.counts);
        match self.stage {
            Stage::Filled | Stage::Recorded if !counts => 0,
            _ => self.start.buffer,
        }
    }

    /// How full it is is ours: before the count, and for a CLI that gives only totals.
    fn used_ours(&self) -> bool {
        !self.said() || self.row.is_some_and(|row| row.estimated)
    }

    /// The lines under the parts: where the numbers come from and when the CLI's own count
    /// replaces them, or, once it has said, which numbers are ours.
    fn notes(&self) -> Vec<String> {
        let cli = &self.cli;
        if let Some(at) = self
            .row
            .and_then(|row| row.recorded.as_ref())
            .filter(|_| self.stage == Stage::Recorded)
        {
            let since = std::time::Duration::try_from(self.now - at.at).unwrap_or_default();
            return vec![format!(
                "~ is our count. Used is as this conversation's record left it {}.",
                Figures::Prose.ago(since)
            )];
        }
        if self.stage == Stage::Filled {
            let note = if self.row.is_some_and(|row| row.estimated) {
                format!("~ is our count: {cli} gives its turns' totals, not what fills it.")
            } else if self.cli_window {
                format!("~ is our count. Used and the window are {cli}'s own.")
            } else {
                format!("~ is our count. Used is {cli}'s own; the window is from our list.")
            };
            return vec![note];
        }
        let mut notes = Vec::new();
        if self.stage == Stage::Loading {
            notes.push(format!("{cli} is loading."));
        }
        match &self.start.from {
            StartedFrom::Recording { version } if self.stage == Stage::Before => {
                notes.push(format!(
                    "~ from our recording of {cli} {version} and from this machine"
                ));
            }
            StartedFrom::Unrecorded if self.stage == Stage::Before => {
                notes.push(format!(
                    "~ from this machine alone: no recording of {cli} ships with Argonauts Code."
                ));
            }
            _ => {}
        }
        notes.push(match self.start.first_count {
            Some(first) => format!(
                "{cli}'s own count replaces it when its first {} ends.",
                first.word()
            ),
            None => format!("{cli}'s own count replaces it once {cli} says how full it is."),
        });
        notes
    }

    pub fn block(&self, kit: &Kit<'_>, names: &DisplayNames) -> Block<'a> {
        let worker = self.worker();
        let agent = worker.agent.as_str();
        let model = worker.model.as_deref();
        let used = self.used();
        let buffer = self.buffer();
        let parts = self
            .parts()
            .into_iter()
            .map(|(name, tokens, color)| Part {
                name,
                tokens,
                style: cc_style(kit, &color),
            })
            .collect();
        let from_last = !self.said() && self.start.from == StartedFrom::LastSession;
        let label = ModelLabel::of(names, &worker, kit.theme, NoPicture::Nothing);
        let logo = label.whose.clone();
        Block {
            model: label
                .beside_picture()
                .or_else(|| names.model(agent, model))
                .or_else(|| Some(self.cli.clone())),
            model_id: self
                .start
                .model
                .as_deref()
                .or(self.row.and_then(|row| row.worker.model.as_deref())),
            logo: Some(logo),
            used,
            window: self.window,
            percent: None,
            heading: "Estimated usage by category".to_owned(),
            parts,
            free: self.window.saturating_sub(used).saturating_sub(buffer),
            buffer,
            grid: None,
            usage: None,
            notes: self.notes(),
            parts_ours: true,
            used_ours: self.used_ours(),
            used_tail: if from_last {
                " \u{b7} from your last session".to_owned()
            } else {
                String::new()
            },
        }
    }

    /// `Instruction files · loaded by Codex itself` over `└ AGENTS.md · ~1.2k tokens`, once a
    /// CLI that says only how full has said. Nothing while there are none.
    pub fn files(&self, kit: &Kit<'_>) -> Drawn {
        let mut drawn = Drawn::new();
        if !self.said() || self.start.files.is_empty() {
            return drawn;
        }
        let tokens: u64 = self.start.files.iter().map(|file| file.tokens).sum();
        let what = match self.start.files.as_slice() {
            [one] => one.name.clone(),
            many => format!("{} files", many.len()),
        };
        let hint = format!(" \u{b7} loaded by {} itself", self.cli);
        drawn.push(lists::title(kit, "Instruction files", &hint));
        drawn.push(vec![
            Span::raw(" ".repeat(INDENT)),
            kit.dim(format!(
                "\u{2514} {what} \u{b7} ~{} tokens",
                format::context_tokens(tokens)
            )),
        ]);
        drawn
    }

    /// `Tools · as agy names them at start` over `└ 60 tools · 5 offered in a read-only
    /// session, in agy's own prompt and tools`, once a CLI that names its tools has said how
    /// full. Nothing for a CLI that doesn't name them.
    pub fn tools(&self, kit: &Kit<'_>) -> Drawn {
        let mut drawn = Drawn::new();
        let Some(tools) = self.start.tools.filter(|_| self.said()) else {
            return drawn;
        };
        let cli = &self.cli;
        let offered = tools
            .read_only
            .map(|read_only| format!(" \u{b7} {read_only} offered in a read-only session"))
            .unwrap_or_default();
        let hint = format!(" \u{b7} as {cli} names them at start");
        drawn.push(lists::title(kit, "Tools", &hint));
        drawn.push(vec![
            Span::raw(" ".repeat(INDENT)),
            kit.dim(format!(
                "\u{2514} {} tools{offered}, in {cli}'s own prompt and tools",
                tools.named
            )),
        ]);
        drawn
    }

    /// The context card, its parts in the block's order and colours.
    pub fn card(&self) -> ContextCard {
        let parts = self
            .parts()
            .into_iter()
            .map(|(name, tokens, color)| ContextPart::named(name, color, Some(tokens)))
            .collect();
        let buffer = self.buffer();
        let before = (!self.said()).then(|| BeforeCount {
            from: match self.start.from {
                StartedFrom::LastSession => Estimate::LastSession,
                StartedFrom::Recording { .. } | StartedFrom::Unrecorded => Estimate::Recorded,
            },
            loading: self.stage == Stage::Loading,
        });
        ContextCard {
            used: Some(self.used()),
            window: Some(self.window),
            limit: (buffer > 0 && buffer < self.window).then(|| Limit {
                at: self.window - buffer,
                kind: LimitKind::Compacts,
            }),
            used_estimated: self.used_ours(),
            before,
            ..ContextCard::with_parts(parts, true)
        }
    }
}
