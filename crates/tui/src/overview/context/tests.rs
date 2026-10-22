use super::example;
use super::*;
use crate::text::plain_text;
use crate::theme::Theme;

fn shown(example: &example::Example, pane: &ContextPane, width: usize) -> Vec<String> {
    let theme = Theme::dark();
    let kit = Kit::new(&theme);
    let facts = example.facts();
    let title = |_: &RunId| example::RUN_TITLE.to_owned();
    let tab = ContextTab {
        view: &example.view,
        names: &example.names,
        count: example.count.as_ref(),
        main: None,
        places: &example.places,
        now: example.now,
        run_title: &title,
        facts: &facts,
        pane,
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    for section in tab.sections(&kit, width) {
        if !lines.is_empty() && !section.joined {
            lines.push(Line::default());
        }
        lines.extend(section.drawn.lines);
    }
    plain_text(&lines)
        .lines()
        .map(|row| row.trim_end().to_owned())
        .collect()
}

/// The row under the tabs counts the live sessions, and the crew's agents that ended with
/// their tries and tie-breakers counted as one.
#[test]
fn the_summary_counts_live_sessions_and_ended_agents() {
    assert_eq!(
        summary(&example::after_the_run().view.rows),
        "1 session \u{b7} 182.5k in context \u{b7} 7 ended"
    );
    assert_eq!(
        summary(&example::straight_to_a_run().view.rows),
        "none open \u{b7} 7 ended"
    );
    assert_eq!(summary(&[]), "none open");
}

/// Folded, the tab says how to open its lists; open, it doesn't.
#[test]
fn the_fold_hint_shows_while_a_list_is_folded() {
    let example = example::after_the_run();
    let folded = shown(&example, &ContextPane::default(), 118);
    assert_eq!(
        folded.last().map(String::as_str),
        Some("     /context all to expand")
    );
    let mut pane = ContextPane::default();
    pane.open_every_list();
    let open = shown(&example, &pane, 118);
    assert!(
        !open.iter().any(|row| row.contains("to expand")),
        "{open:#?}"
    );
    assert!(
        open.iter().any(|row| row.starts_with("     └ main:answer")),
        "{open:#?}"
    );
}

/// A narrow panel keeps every row inside it, the column beside the grid under it.
#[test]
fn a_narrow_tab_keeps_its_rows_inside_it() {
    let example = example::after_the_run();
    let mut pane = ContextPane::default();
    pane.open_every_list();
    for width in [40, 60, 80] {
        let rows = shown(&example, &pane, width);
        assert!(
            rows.iter().all(|row| crate::text::str_width(row) <= width),
            "at {width}: {rows:#?}"
        );
        assert!(rows.iter().any(|row| row.contains("Opus 5.5")), "{rows:#?}");
    }
}
