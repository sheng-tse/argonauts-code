//! What a run on autopilot decided in your place, listed right under its closing answer.

use argonauts_core::runs::events::run::DecidedAlone;
use ratatui::text::{Line, Span};

use super::HistoryCell;
use super::layout::{TEXT_COL, indent};
use crate::base::draw::render::RenderCtx;
use crate::base::text::{fit_lines, sanitize_line, wrap_spans};
use crate::base::theme::Role;

/// What each row starts with, as a list item hung under the answer's words.
const OPENING: &str = "- Decided on autopilot: ";
/// Where the words under an item's dash start.
const UNDER_ITEM: usize = TEXT_COL + 2;

/// `  - Decided on autopilot: <what>` with whose call it was on the row beneath. Only the
/// decision reads at full strength, so the eye lands on what to review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecidedOnAutopilot {
    pub decided: Vec<DecidedAlone>,
    /// How many more were decided than are listed.
    pub more: usize,
}

impl DecidedOnAutopilot {
    /// The first `named` of them listed, and the rest counted.
    pub fn new(decided: &[DecidedAlone], named: usize) -> Self {
        Self {
            decided: decided.iter().take(named).cloned().collect(),
            more: decided.len().saturating_sub(named),
        }
    }
}

impl HistoryCell for DecidedOnAutopilot {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let width = usize::from(ctx.width);
        let dim = ctx.theme.style(Role::Dim);
        let under = [indent(UNDER_ITEM)];
        let mut lines = Vec::new();
        for decided in &self.decided {
            let what = sanitize_line(&decided.what);
            let what = what.trim().trim_end_matches('.').to_owned();
            let opening = [indent(TEXT_COL), Span::styled(OPENING, dim)];
            lines.extend(wrap_spans(&[Span::raw(what)], width, &opening, &under));
            let note = sanitize_line(&decided.review_note()).into_owned();
            lines.extend(wrap_spans(
                &[Span::styled(note, dim)],
                width,
                &under,
                &under,
            ));
        }
        if self.more > 0 {
            let more = format!("- and {} more decided on autopilot", self.more);
            lines.extend(wrap_spans(
                &[Span::styled(more, dim)],
                width,
                &[indent(TEXT_COL)],
                &under,
            ));
        }
        fit_lines(&mut lines, width);
        lines
    }

    /// The list belongs to the answer above it.
    fn continues_previous(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::base::draw::render::Clock;
    use crate::base::text::plain_text;
    use crate::base::theme::Theme;

    fn decided(what: &str) -> DecidedAlone {
        DecidedAlone {
            what: what.to_owned(),
            whose: "the worker's call".to_owned(),
        }
    }

    fn drawn(cell: &DecidedOnAutopilot, width: u16) -> Vec<String> {
        let theme = Theme::dark();
        let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
        cell.lines(&ctx)
            .iter()
            .map(|line| plain_text(std::slice::from_ref(line)))
            .collect()
    }

    #[test]
    fn a_long_decision_wraps_under_its_own_words_and_the_rest_are_counted() {
        let all = [
            decided("dates without a year mean this year, as the importer reads them."),
            decided("ISO dates only"),
            decided("amounts are whole cents"),
        ];
        let cell = DecidedOnAutopilot::new(&all, 2);
        assert_eq!(
            drawn(&cell, 60),
            [
                "  - Decided on autopilot: dates without a year mean this",
                "    year, as the importer reads them",
                "    (the worker's call; ask for a change if you want",
                "    otherwise)",
                "  - Decided on autopilot: ISO dates only",
                "    (the worker's call; ask for a change if you want",
                "    otherwise)",
                "  - and 1 more decided on autopilot",
            ]
        );
        assert!(cell.continues_previous());
    }
}
