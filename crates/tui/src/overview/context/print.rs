//! `/context print`: what the Context tab shows, as rows in the conversation under `⎿`, the
//! way Claude Code prints its own /context. The rows are as of when they were printed.

use argonauts_core::RunId;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::ModelRef;
use argonauts_core::overview::ModelTokens;
use argonauts_core::views::ContextView;
use ratatui::text::{Line, Span};
use time::OffsetDateTime;

use super::ContextTab;
use super::place::Columns;
use crate::bridge::tools::Places;
use crate::cell::HistoryCell;
use crate::kit::Kit;
use crate::render::RenderCtx;
use crate::state::{AppState, ContextCount};
use crate::theme::Role;

/// Where the rows' text starts, after `  ⎿  `.
const HANG: usize = 5;

/// Everything the tab drew from when `/context print` ran.
#[derive(Debug, Clone)]
pub struct ContextPrinted {
    view: ContextView,
    count: Option<ContextCount>,
    main: Option<ModelRef>,
    per_turn: Option<u64>,
    names: DisplayNames,
    places: Places,
    now: OffsetDateTime,
    run_titles: Vec<(RunId, String)>,
    /// Every model's tokens in all, for the shade each model draws in.
    ranked: Vec<ModelTokens>,
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
            per_turn: state.conversation.growth.per_turn(),
            names: state.names.clone(),
            places: Places::of_conversation(state),
            now,
            run_titles,
            ranked: state
                .usage
                .as_ref()
                .map_or_else(Vec::new, |usage| usage.all_time_models.clone()),
        }
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
        let tab = ContextTab {
            view: &self.view,
            names: &self.names,
            count: self.count.as_ref(),
            main: self.main.as_ref(),
            per_turn: self.per_turn,
            places: &self.places,
            now: self.now,
            run_title: &run_title,
            ranked: &self.ranked,
        };
        // A narrow tab sets its text two cells in, so it is drawn narrower and moved under
        // the hang.
        let shift = HANG.saturating_sub(Columns::for_width(width).indent());
        let mut lines: Vec<Line<'static>> = Vec::new();
        for section in tab.sections(&kit, width.saturating_sub(shift)) {
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            for mut line in section.drawn.lines {
                if shift > 0 && !line.spans.is_empty() {
                    line.spans.insert(0, Span::raw(" ".repeat(shift)));
                }
                lines.push(line);
            }
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
    use crate::render::Clock;
    use crate::state::{ContextGrowth, RunState};
    use crate::text::{plain_text, spans_width};
    use crate::theme::{ColorLevel, Glyphs, Theme};

    fn printed(example: &example::Example, width: u16) -> Vec<String> {
        let mut state = AppState::default();
        state.names = example.names.clone();
        state.conversation.context = Some(example.count.clone());
        state.conversation.growth = ContextGrowth::of(example::PER_TURN);
        state.conversation.worker = Some(example.view.rows[0].worker.clone());
        let mut run = RunState::new(example::run());
        run.title = example::RUN_TITLE.to_owned();
        state.runs.insert(example::run(), run);
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

    /// The same rows as the tab, hung under `⎿` with their text where the tab has it.
    #[test]
    fn the_tabs_rows_print_under_the_hang() {
        let example = example::at_three_quarters();
        let rows = printed(&example, 118);
        assert!(
            rows[0].starts_with("  ⎿  Context Usage  counted 6s ago"),
            "{rows:#?}"
        );
        assert!(rows[0].ends_with("750k/1M tokens (75%)"), "{rows:#?}");
        assert_eq!(rows[1], "");
        assert!(
            rows[2].ends_with("Estimated usage by category"),
            "{rows:#?}"
        );
        assert!(rows[3].starts_with("     ⛁ ⛁ ⛁ ⛁ ⛀"), "{rows:#?}");
        for part in [
            "     Auto-compact window:  1M tokens",
            "     MCP tools · /mcp (loaded on-demand)",
            "     └ search_issues",
            "     Memory files · /memory",
            "     Messages · by kind",
            "     ⚠ Bash results using 180k tokens (18%)",
            "     Crew agents · Add due dates",
            "     ◐ build:due-tests",
        ] {
            assert!(
                rows.iter().any(|row| row.starts_with(part)),
                "{part}: {rows:#?}"
            );
        }
        assert_eq!(rows.len(), 66, "every row of the tab's picture: {rows:#?}");
    }

    #[test]
    fn a_narrow_print_keeps_its_text_under_the_hang() {
        let example = example::near_full();
        for width in [60, 80, 100] {
            let rows = printed(&example, width);
            assert!(rows[0].starts_with("  ⎿  Context Usage"), "{rows:#?}");
            let set_in = |row: &String| row.len() - row.trim_start().len();
            assert!(
                rows.iter()
                    .filter(|row| !row.is_empty())
                    .all(|row| set_in(row) >= 5 || row.starts_with("  ⎿")),
                "at {width}: {rows:#?}"
            );
        }
    }
}
