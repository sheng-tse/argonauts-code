//! The top of a council task's workflow view: the way, the task, whose task it is in which
//! council, and the way's own line under it. Esc there goes back to the council, so the keys
//! say so.

use argonauts_core::names::{SeatId, WorkflowId};
use ratatui::text::{Line, Span};

use super::faces::Faces;
use crate::theme::{Role, Theme};

/// The last key of a council task's workflow view, in place of a plain "esc back".
pub const BACK: &str = "esc back to the council";

/// A council task done by a workflow, as its workflow view names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskHeader {
    /// The workflow the view opens on.
    pub workflow: WorkflowId,
    /// The way's name as the task's box writes it: "Maker-checker".
    pub way: String,
    pub task: String,
    pub seat: SeatId,
    /// The council's own title, the run's.
    pub council: String,
    /// The way's one-line description.
    pub about: String,
}

impl TaskHeader {
    /// "Maker-checker  Redis bucket · ▐▌ Opus 5.5's task in the council Rate limit the API",
    /// then the way's line. The view puts its counts at the right of the second.
    pub fn lines(&self, faces: &Faces, theme: &Theme) -> [Line<'static>; 2] {
        let dim = theme.style(Role::Dim);
        let face = faces.face(&self.seat);
        let mut first = vec![
            Span::styled(self.way.clone(), theme.style(Role::Bright)),
            Span::styled(format!("  {} · ", self.task), dim),
        ];
        first.extend(face.badge(theme));
        first.push(Span::styled(
            format!(" {}'s task in the council {}", face.label(), self.council),
            dim,
        ));
        [
            Line::from(first),
            Line::from(Span::styled(self.about.clone(), dim)),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::council::story;

    #[test]
    fn the_way_takes_the_title_colour_and_the_rest_reads_dim() {
        let theme = Theme::dark();
        let header = story::task_header();
        let [first, second] = header.lines(&story::faces(), &theme);
        assert_eq!(first.spans[0].content, "Maker-checker");
        assert_eq!(first.spans[0].style, theme.style(Role::Bright));
        assert!(
            first.spans[1..]
                .iter()
                .filter(|span| span.content.contains("task"))
                .all(|span| span.style == theme.style(Role::Dim))
        );
        assert_eq!(second.spans[0].style, theme.style(Role::Dim));
    }

    #[test]
    fn a_model_that_sits_twice_is_named_by_its_number() {
        let theme = Theme::dark();
        let mut header = story::task_header();
        header.seat = story::opus_one();
        let [first, _] = header.lines(&story::faces_one_model_twice(), &theme);
        let text: String = first
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(
            text.contains("①  Opus 5.5 · 1's task in the council"),
            "{text}"
        );
    }
}
