//! A fallback question answered with an agent, without Remember this pick on, stays offered as a
//! rule on one line where the question was: ↑ picks the line, and `m` saves the pick with
//! `UiCommand::RememberPick`, where your changes go. It goes once you send anything else.

use argonauts_core::RunId;
use argonauts_core::names::ApprovalId;
use ratatui::text::{Line, Span};

use crate::render::RenderCtx;
use crate::text::{sanitize_line, truncate_line};
use crate::theme::Role;

/// The key that saves the pick, as on the fallback question's own Remember this pick.
pub const REMEMBER_KEY: char = 'm';

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnsweredPick {
    pub run: RunId,
    pub id: ApprovalId,
    /// What the answer did, such as `task-003 fell back to Codex`.
    pub outcome: String,
    /// ↑ picked the line, so `m` saves the pick and Esc dismisses it.
    pub selected: bool,
}

impl AnsweredPick {
    pub fn new(run: RunId, id: ApprovalId, outcome: impl Into<String>) -> Self {
        Self {
            run,
            id,
            outcome: outcome.into(),
            selected: false,
        }
    }

    pub fn line(&self, ctx: &RenderCtx<'_>) -> Line<'static> {
        let theme = ctx.theme;
        let dim = theme.style(Role::Dim);
        let (mark, style, hint) = if self.selected {
            (
                theme.glyphs.prompt,
                theme.style(Role::Question),
                format!(" · {REMEMBER_KEY} to save it as a rule · esc to dismiss"),
            )
        } else {
            (
                theme.glyphs.question,
                theme.style(Role::Success),
                " · ↑  to remember this pick".to_owned(),
            )
        };
        let line = Line::from(vec![
            Span::styled(format!("{mark} "), style),
            Span::raw(sanitize_line(&self.outcome).into_owned()),
            Span::styled(hint, dim),
        ]);
        truncate_line(&line, usize::from(ctx.width))
    }
}
