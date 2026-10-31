//! `/context print`: what the Context tab shows, as rows in the conversation under `⎿`, the
//! way Claude Code prints its own /context. The rows are as of when they were printed.

use argonauts_core::RunId;
use argonauts_core::app::views::ContextView;
use argonauts_core::base::names::ModelRef;
use argonauts_core::look::display_names::DisplayNames;
use ratatui::text::{Line, Span};
use time::OffsetDateTime;

use super::block::INDENT;
use super::starting::Started;
use super::{ContextPane, ContextTab, CrewFacts};
use crate::base::draw::render::RenderCtx;
use crate::base::kit::{ContextCard, Kit};
use crate::base::theme::Role;
use crate::cells::cell::HistoryCell;
use crate::frame::state::Places;
use crate::frame::state::{AppState, ContextCount};

/// Where the rows' text starts, after `  ⎿  `: where the tab's own starts.
const HANG: usize = INDENT;

/// Everything the tab drew from when `/context print` ran.
#[derive(Debug, Clone)]
pub struct ContextPrinted {
    view: ContextView,
    count: Option<ContextCount>,
    main: Option<ModelRef>,
    names: DisplayNames,
    places: Places,
    now: OffsetDateTime,
    run_titles: Vec<(RunId, String)>,
    facts: CrewFacts,
}

impl ContextPrinted {
    /// The sessions the controller listed, with the main session's count as the screen holds
    /// it now.
    pub fn of(view: &ContextView, state: &AppState, now: OffsetDateTime) -> Self {
        let run_titles = view
            .rows
            .iter()
            .filter_map(|row| row.run.clone())
            .filter_map(|run| {
                let title = state.runs.get(&run)?.title.trim().to_owned();
                (!title.is_empty()).then_some((run, title))
            })
            .collect();
        Self {
            view: view.clone(),
            count: state.conversation.context.clone(),
            main: state.conversation.worker.clone(),
            names: state.names.clone(),
            places: Places::of_conversation(state),
            now,
            run_titles,
            facts: CrewFacts::of(state, view),
        }
    }

    /// The card the tab ends with for the main session before its CLI counts it by category,
    /// and for a CLI that says only how full it is, from the start the screen keeps: the card
    /// above the input draws it too.
    pub fn started_card(state: &AppState) -> Option<ContextCard> {
        let main = state.conversation.main_start.as_ref()?;
        let row = main.row.as_ref();
        if row.is_some_and(|row| row.worker.agent != main.start.agent) {
            return None;
        }
        let now = crate::base::format::wall_now();
        Some(Started::of(&main.start, row, &state.names, main.cli_window, now).card())
    }

    /// Shows paths from `places`, wherever a test runs.
    #[cfg(test)]
    pub(crate) fn showing_paths_from(mut self, places: Places) -> Self {
        self.places = places;
        self
    }
}

impl HistoryCell for ContextPrinted {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let kit = Kit::new(ctx.theme);
        let width = usize::from(ctx.width);
        let run_title = |run: &RunId| {
            let known = self.run_titles.iter().find(|(known, _)| known == run);
            known.map_or_else(|| run.to_string(), |(_, title)| title.clone())
        };
        // A printed tab has no keys, so its lists stay folded.
        let pane = ContextPane::default();
        let tab = ContextTab {
            view: &self.view,
            names: &self.names,
            count: self.count.as_ref(),
            main: self.main.as_ref(),
            places: &self.places,
            now: self.now,
            run_title: &run_title,
            facts: &self.facts,
            pane: &pane,
        };
        let mut lines: Vec<Line<'static>> = Vec::new();
        for section in tab.printed_sections(&kit, width) {
            if !lines.is_empty() && !section.joined {
                lines.push(Line::default());
            }
            lines.extend(section.drawn.lines);
        }
        if let Some(first) = lines.first_mut() {
            *first = hung(&kit, std::mem::take(first));
        }
        lines
    }

    /// The rows hang right under the command's own row.
    fn continues_previous(&self) -> bool {
        true
    }
}

/// `line` with `  ⎿  ` in place of the blanks before its text.
fn hung(kit: &Kit<'_>, line: Line<'static>) -> Line<'static> {
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(kit.theme.glyphs.result, kit.style(Role::Dim)),
        Span::raw("  "),
    ];
    let mut skip = HANG;
    for span in line.spans {
        let blanks = span.content.len() - span.content.trim_start_matches(' ').len();
        let cut = blanks.min(skip);
        if cut < span.content.len() {
            spans.push(Span::styled(span.content[cut..].to_owned(), span.style));
            // Only the blanks before the row's first word give way to the hang.
            skip = 0;
        } else {
            skip -= cut;
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::super::example;
    use super::*;
    use crate::base::draw::render::Clock;
    use crate::base::text::{plain_text, spans_width};
    use crate::base::theme::{ColorLevel, Glyphs, Theme};

    fn printed(example: &example::Example, width: u16) -> Vec<String> {
        let state = example.state();
        let cell = ContextPrinted::of(&example.view, &state, example.now)
            .showing_paths_from(example.places.clone());
        let mut theme = Theme::dark().with_level(ColorLevel::None);
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(width, &theme, Clock::still(std::time::Instant::now()));
        let lines = cell.lines(&ctx);
        for line in &lines {
            assert!(spans_width(&line.spans) <= usize::from(width), "{line}");
        }
        plain_text(&lines)
            .lines()
            .map(|row| row.trim_end().to_owned())
            .collect()
    }

    /// The tab's rows, folded, hung under `⎿` with their text where the tab has it.
    #[test]
    fn the_tabs_rows_print_under_the_hang() {
        let rows = printed(&example::after_the_run(), 118);
        assert_eq!(rows[0], "  \u{23bf}  Context Usage", "{rows:#?}");
        assert!(rows[1].starts_with("     \u{26c1} \u{26c1}"), "{rows:#?}");
        assert!(rows[1].ends_with(" Opus 5.5"), "{rows:#?}");
        for part in [
            "     Auto-compact window: 1m tokens",
            "     MCP tools \u{b7} /mcp (loaded on-demand)",
            "     \u{2514} 14 tools \u{b7} 0 tokens",
            "     Crew agents \u{b7} the last run, Make the rate limiter per API key",
            "     /context all to expand",
        ] {
            assert!(rows.iter().any(|row| row == part), "{part}: {rows:#?}");
        }
        assert_eq!(rows.len(), 26, "every row of the tab's picture: {rows:#?}");
    }

    #[test]
    fn a_narrow_print_keeps_its_text_under_the_hang() {
        let example = example::after_the_run();
        for width in [60, 80, 100] {
            let rows = printed(&example, width);
            assert!(
                rows[0].starts_with("  \u{23bf}  Context Usage"),
                "{rows:#?}"
            );
            let set_in = |row: &String| row.len() - row.trim_start().len();
            assert!(
                rows.iter()
                    .filter(|row| !row.is_empty())
                    .all(|row| set_in(row) >= 5 || row.starts_with("  \u{23bf}")),
                "at {width}: {rows:#?}"
            );
        }
    }
}
