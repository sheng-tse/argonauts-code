//! Every item starts at column 0 with a one-cell glyph and a space, and its text at column 2.
//! Results hang under a call with `  ⎿  ` at column 5. [`History`](crate::history::History)
//! adds the blank line between items, not the cells.

mod assistant;
mod crew;
mod diff;
mod group;
mod header;
pub(crate) mod layout;
mod notice;
mod record;
mod shell;
mod tool;
mod turn;
mod user;

use std::fmt;

use ratatui::text::{Line, Span};

use crate::render::RenderCtx;
use crate::theme::Role;

pub use assistant::AssistantMessage;
pub use crew::{AgentsStarted, StartedAgent};
pub use diff::{DiffCell, DiffHunk, DiffKind, DiffLine, DiffParseError, DiffView};
pub use group::CollapsedGroup;
pub use header::{AgentReady, SessionHeader};
pub use notice::{ExitNote, HungLine, Notice, ResultLine};
pub use record::Record;
pub use shell::{ShellEnd, ShellRun};
pub use tool::Held;
pub use tool::StepMark;
pub use tool::{
    BLINK_PERIOD, PhaseLine, Todo, TodoState, ToolCall, ToolItem, ToolKind, ToolState, ToolStats,
    ToolTitle,
};
pub use turn::{Recap, TurnEnd};
pub use user::UserMessage;

pub trait HistoryCell: fmt::Debug + Send {
    /// One `Line` per terminal row. A line's own style is its fill, running to the right edge.
    /// Spans with a `Color::Reset` background stay unfilled, so an indent sits outside the fill.
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>>;

    /// For a live region shared by several agents.
    fn lines_within(&self, ctx: &RenderCtx<'_>, rows: usize) -> Vec<Line<'static>> {
        squeeze(self.lines(ctx), rows, ctx)
    }

    fn is_animating(&self) -> bool {
        false
    }

    /// No blank row before it, like `  ⎿  Interrupted · …` right under the answer it stopped.
    fn continues_previous(&self) -> bool {
        false
    }
}

/// Keeps the first row and the newest rows, with a dim `… +N lines` row between them.
pub fn squeeze(
    mut lines: Vec<Line<'static>>,
    rows: usize,
    ctx: &RenderCtx<'_>,
) -> Vec<Line<'static>> {
    if lines.len() <= rows {
        return lines;
    }
    if rows < 3 {
        lines.truncate(rows);
        return lines;
    }
    let tail = rows - 2;
    let hidden = lines.len() - 1 - tail;
    let marker = Line::from(vec![
        layout::indent(layout::BODY_COL),
        Span::styled(
            layout::more_lines(ctx.theme, hidden),
            ctx.theme.style(Role::Dim),
        ),
    ]);
    let marker = crate::text::truncate_line(&marker, usize::from(ctx.width));
    let mut kept = Vec::with_capacity(rows);
    let mut rest = lines.split_off(1);
    kept.append(&mut lines);
    kept.push(marker);
    kept.extend(rest.split_off(rest.len() - tail));
    kept
}

#[derive(Debug)]
pub enum Cell {
    Header(SessionHeader),
    User(UserMessage),
    Assistant(AssistantMessage),
    Tool(ToolCall),
    Diff(DiffCell),
    Group(CollapsedGroup),
    TurnEnd(TurnEnd),
    Recap(Recap),
    Notice(Notice),
    Exit(ExitNote),
    AgentsStarted(AgentsStarted),
    Shell(ShellRun),
    Custom(Box<dyn HistoryCell>),
}

impl Cell {
    fn inner(&self) -> &dyn HistoryCell {
        match self {
            Cell::Header(cell) => cell,
            Cell::User(cell) => cell,
            Cell::Assistant(cell) => cell,
            Cell::Tool(cell) => cell,
            Cell::Diff(cell) => cell,
            Cell::Group(cell) => cell,
            Cell::TurnEnd(cell) => cell,
            Cell::Recap(cell) => cell,
            Cell::Notice(cell) => cell,
            Cell::Exit(cell) => cell,
            Cell::AgentsStarted(cell) => cell,
            Cell::Shell(cell) => cell,
            Cell::Custom(cell) => cell.as_ref(),
        }
    }

    pub fn as_user_mut(&mut self) -> Option<&mut UserMessage> {
        match self {
            Cell::User(message) => Some(message),
            _ => None,
        }
    }

    pub fn as_tool_mut(&mut self) -> Option<&mut ToolCall> {
        match self {
            Cell::Tool(call) => Some(call),
            _ => None,
        }
    }

    pub fn as_shell_mut(&mut self) -> Option<&mut ShellRun> {
        match self {
            Cell::Shell(run) => Some(run),
            _ => None,
        }
    }

    pub fn as_group_mut(&mut self) -> Option<&mut CollapsedGroup> {
        match self {
            Cell::Group(group) => Some(group),
            _ => None,
        }
    }
}

impl HistoryCell for Cell {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        self.inner().lines(ctx)
    }

    fn lines_within(&self, ctx: &RenderCtx<'_>, rows: usize) -> Vec<Line<'static>> {
        self.inner().lines_within(ctx, rows)
    }

    fn is_animating(&self) -> bool {
        self.inner().is_animating()
    }

    fn continues_previous(&self) -> bool {
        self.inner().continues_previous()
    }
}

macro_rules! cell_from {
    ($($variant:ident($ty:ty)),* $(,)?) => {
        $(impl From<$ty> for Cell {
            fn from(cell: $ty) -> Self {
                Cell::$variant(cell)
            }
        })*
    };
}

cell_from!(
    Header(SessionHeader),
    User(UserMessage),
    Assistant(AssistantMessage),
    Tool(ToolCall),
    Diff(DiffCell),
    Group(CollapsedGroup),
    TurnEnd(TurnEnd),
    Recap(Recap),
    Notice(Notice),
    Exit(ExitNote),
    AgentsStarted(AgentsStarted),
    Shell(ShellRun),
);

impl From<Box<dyn HistoryCell>> for Cell {
    fn from(cell: Box<dyn HistoryCell>) -> Self {
        Cell::Custom(cell)
    }
}

/// What a piece of text means, for text not tied to one cell type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Tone {
    #[default]
    Normal,
    Dim,
    Success,
    Warning,
    Error,
    Accent,
    Code,
}

impl Tone {
    pub fn role(self) -> Role {
        match self {
            Tone::Normal => Role::Text,
            Tone::Dim => Role::Dim,
            Tone::Success => Role::Success,
            Tone::Warning => Role::Warning,
            Tone::Error => Role::Error,
            Tone::Accent => Role::Accent,
            Tone::Code => Role::Code,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::render::Clock;
    use crate::text::plain_text;
    use crate::theme::Theme;

    #[test]
    fn squeeze_keeps_the_first_row_and_the_newest_rows() {
        let mut theme = Theme::dark();
        theme.glyphs = crate::theme::Glyphs::UNICODE;
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let lines: Vec<Line<'static>> = (1..=8).map(|i| Line::from(format!("row {i}"))).collect();

        assert_eq!(squeeze(lines.clone(), 8, &ctx).len(), 8);
        assert_eq!(
            plain_text(&squeeze(lines.clone(), 5, &ctx)),
            "row 1\n     … +4 lines\nrow 6\nrow 7\nrow 8"
        );
        assert_eq!(plain_text(&squeeze(lines, 2, &ctx)), "row 1\nrow 2");
    }

    fn every_cell() -> Vec<Cell> {
        let long = "a long line of words that goes on 你好世界 and on \x1b[31mpast any width";
        let mut group = CollapsedGroup::new();
        group.push(ToolCall::call(ToolKind::Read, "Read", long).with_state(ToolState::Done));
        let mut session = ToolCall::described(ToolKind::Agent, long).with_stats(ToolStats {
            started_at: Some(Instant::now()),
            ended_at: None,
            tokens_down: 123_456_789,
        });
        session.push_call(ToolCall::call(ToolKind::Edit, "Update", long).with_item(
            ToolItem::Diff(DiffView::between("old words\n", &format!("{long}\n"), 1)),
        ));
        session.push(ToolItem::Command {
            command: long.into(),
            output: vec![long.into(); 7],
            hidden: 2,
        });
        session.push(ToolItem::Markdown(format!(
            "# {long}\n**{long}**\n\n```sh\ncd {long}\n```\n\n```\n{long}\n```\n1. {long}\n   1. {long}"
        )));
        session.push(ToolItem::Todos(vec![Todo {
            text: long.into(),
            state: TodoState::InProgress,
        }]));
        vec![
            SessionHeader {
                app: long.into(),
                version: "0.0.1".into(),
                main_agent: Some(long.into()),
                model: Some(long.into()),
                effort: Some(long.into()),
                workflow: long.into(),
                folder: long.into(),
                agents: vec![AgentReady {
                    name: long.into(),
                    ready: false,
                    note: Some(long.into()),
                }],
                runs_going: 3,
                runs_need_you: 0,
            }
            .into(),
            UserMessage::new(long).into(),
            AssistantMessage::new(format!(
                "- {long}\n\n> {long}\n\n| a | b |\n|---|---|\n| {long} | x |"
            ))
            .into(),
            session.into(),
            group.into(),
            DiffCell {
                title: long.into(),
                diff: DiffView::between(long, "short", 0),
            }
            .into(),
            TurnEnd::new(long, std::time::Duration::from_secs(4000), None).into(),
            Recap::new(long).into(),
            Notice::new(Tone::Error, long)
                .with_glyph("◆")
                .with_detail(long)
                .into(),
            Cell::Custom(Box::new(ResultLine::new(Tone::Dim, long))),
        ]
    }

    #[test]
    fn no_cell_draws_past_the_width_or_leaks_an_escape() {
        for glyphs in [crate::theme::Glyphs::UNICODE, crate::theme::Glyphs::ASCII] {
            let mut theme = Theme::dark();
            theme.glyphs = glyphs;
            for width in [0u16, 1, 2, 3, 4, 6, 9, 14, 23, 40, 80, 200] {
                let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
                for expanded in [false, true] {
                    let ctx = ctx.with_expanded(expanded);
                    for cell in every_cell() {
                        let mut all = cell.lines(&ctx);
                        all.extend(cell.lines_within(&ctx, 4));
                        for line in &all {
                            assert!(
                                crate::text::spans_width(&line.spans) <= usize::from(width),
                                "{width}: {line:?}"
                            );
                            for span in &line.spans {
                                assert!(!span.content.contains('\x1b'), "{span:?}");
                            }
                        }
                    }
                }
            }
        }
    }
}
