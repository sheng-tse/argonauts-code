//! Something done or shown at your word: a workflow written, or a workflow's plan previewed.
//!
//! ```text
//! ◆ spec-first for "add a burst limit" · a preview, nothing runs yet
//!   1 Plan     orchestrator   Haiku 4.5 on Claude Code      splits the request into tasks
//!   then the checks in your settings · then merge
//! ```

use ratatui::text::{Line, Span};

use super::HistoryCell;
use crate::render::RenderCtx;
use crate::text::marks;
use crate::text::{fit_lines, sanitize, sanitize_line, str_width, wrap_spans, wrap_text};
use crate::theme::Role;

/// The green `◆`, the title plain and its tail dim, then plain lines and dim notes set in
/// by two cells, as a table keeps its columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub title: String,
    pub tail: Option<String>,
    pub lines: Vec<String>,
    pub notes: Vec<String>,
}

impl HistoryCell for Record {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let width = usize::from(ctx.width);
        let glyph = theme.glyphs.card;
        let first = [
            marks::decorative(glyph, theme.style(Role::Success)),
            Span::raw(" "),
        ];
        let rest = [Span::raw(" ".repeat(str_width(glyph) + 1))];
        let mut head = vec![Span::styled(
            sanitize_line(&self.title).trim().to_owned(),
            theme.style(Role::Text),
        )];
        if let Some(tail) = self.tail.as_deref().map(sanitize_line)
            && !tail.trim().is_empty()
        {
            head.push(Span::styled(
                format!(" · {}", tail.trim()),
                theme.style(Role::Dim),
            ));
        }
        let mut lines = wrap_spans(&head, width, &first, &rest);
        let indent = [Span::raw("  ")];
        let rows = self
            .lines
            .iter()
            .map(|line| (line, Role::Text))
            .chain(self.notes.iter().map(|note| (note, Role::Dim)));
        for (row, role) in rows {
            lines.extend(wrap_text(
                sanitize(row).trim_end(),
                theme.style(role),
                width,
                &indent,
                &indent,
            ));
        }
        fit_lines(&mut lines, width);
        lines
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use ratatui::style::Modifier;

    use super::*;
    use crate::render::Clock;
    use crate::text::plain_text;
    use crate::theme::{Glyphs, Theme};

    fn drawn(record: &Record) -> (Vec<Line<'static>>, Theme) {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let lines = record.lines(&RenderCtx::new(118, &theme, Clock::still(Instant::now())));
        (lines, theme)
    }

    /// A workflow's preview as drawn: the green `◆`, the title plain and its tail dim, the
    /// plan's rows as they came, and what follows the steps dim.
    #[test]
    fn a_preview_reads_as_drawn() {
        let rows = [
            "1 Plan     orchestrator   Haiku 4.5 on Claude Code      splits the request into tasks",
            "2 Build    workers        Haiku 4.5 on Claude Code      one per task, side by side, \
             up to 3 rounds",
            "3 Review   reviewers      GPT 5.6 Luna on Codex,        each task; sends it back to \
             Build",
            "                          Gemini 3.6 Flash on agy       until they accept",
        ];
        let record = Record {
            title: "spec-first for \"add a burst limit\"".to_owned(),
            tail: Some("a preview, nothing runs yet".to_owned()),
            lines: rows.iter().map(|row| (*row).to_owned()).collect(),
            notes: vec!["then the checks in your settings · then merge".to_owned()],
        };
        let (lines, theme) = drawn(&record);
        assert_eq!(
            plain_text(&lines),
            "◆ spec-first for \"add a burst limit\" · a preview, nothing runs yet\n  \
             1 Plan     orchestrator   Haiku 4.5 on Claude Code      splits the request into \
             tasks\n  \
             2 Build    workers        Haiku 4.5 on Claude Code      one per task, side by \
             side, up to 3 rounds\n  \
             3 Review   reviewers      GPT 5.6 Luna on Codex,        each task; sends it back \
             to Build\n                            \
             Gemini 3.6 Flash on agy       until they accept\n  \
             then the checks in your settings · then merge"
        );
        assert_eq!(lines[0].spans[0].style.fg, theme.style(Role::Success).fg);
        let title = &lines[0].spans[2];
        assert_eq!(title.style, theme.style(Role::Text));
        assert!(!title.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(lines[0].spans[3].style, theme.style(Role::Dim));
        assert_eq!(lines[1].spans[1].style, theme.style(Role::Text));
        assert_eq!(lines[5].spans[1].style, theme.style(Role::Dim));
    }

    #[test]
    fn a_written_workflow_reads_as_drawn() {
        let record = Record {
            title: "Wrote spec-first".to_owned(),
            tail: Some(".argonauts/workflows/spec-first.toml".to_owned()),
            lines: Vec::new(),
            notes: vec![
                "/spec-first <request> runs it once · /workflow spec-first uses it · /workflow \
                 try previews it"
                    .to_owned(),
            ],
        };
        let (lines, theme) = drawn(&record);
        assert_eq!(
            plain_text(&lines),
            "◆ Wrote spec-first · .argonauts/workflows/spec-first.toml\n  \
             /spec-first <request> runs it once · /workflow spec-first uses it · /workflow try \
             previews it"
        );
        assert_eq!(lines[0].spans[0].style.fg, theme.style(Role::Success).fg);
        assert_eq!(lines[0].spans[2].style, theme.style(Role::Text));
        assert_eq!(lines[1].spans[1].style, theme.style(Role::Dim));
    }
}
