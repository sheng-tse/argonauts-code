//! What ran through Argonauts Code itself over the period, as a card under who did what: the
//! workflows by kind, the tasks merged and how many passed review first time, the fallbacks
//! and how many a plan window forced, and the questions settled on autopilot.

use argonauts_core::app::overview::ThroughArgonauts;
use ratatui::text::Span;

use crate::base::format::thousands;
use crate::base::kit::{Drawn, Frame, Header, Kit, fit, spread};
use crate::base::text::{spans_width, truncate_spans};
use crate::base::theme::Role;

/// Cells the row names take.
const NAMES: usize = 18;

/// The card for `through`, in `frame`. With nothing recorded it says so.
pub fn card(kit: &Kit<'_>, frame: Frame, through: Option<&ThroughArgonauts>) -> Drawn {
    let inner = frame.inner();
    let mut drawn = Drawn::new();
    let left = Header::new("through Argonauts Code", Role::Question).left(kit);
    let note = vec![kit.dim("the crew's runs, as Argonauts Code ran them")];
    let room = inner.saturating_sub(spans_width(&left) + 2);
    drawn.push(spread(left, truncate_spans(&note, room), inner));
    let Some(through) = through.filter(|through| {
        through.workflows > 0 || through.merged > 0 || through.answered > 0 || through.fallbacks > 0
    }) else {
        drawn.push(vec![
            kit.dim("Nothing ran through Argonauts Code in this period."),
        ]);
        return frame.draw(kit, drawn);
    };
    for (name, count, tail) in rows(through) {
        let mut row = fit(vec![Span::raw(name)], NAMES);
        row.push(kit.strong(thousands(u64::from(count))));
        if !tail.is_empty() {
            row.push(kit.dim(tail));
        }
        drawn.push(truncate_spans(&row, inner));
    }
    frame.draw(kit, drawn)
}

/// Each row's name, its count, and the words after it.
fn rows(through: &ThroughArgonauts) -> [(&'static str, u32, String); 4] {
    let mut kinds: Vec<String> = Vec::new();
    let mut yours = 0;
    for (name, count) in &through.by_workflow {
        if is_builtin_workflow(name) {
            kinds.push(format!(
                "{} {count}",
                argonauts_core::base::text::shown_name(name)
            ));
        } else {
            yours += count;
        }
    }
    if yours > 0 {
        kinds.push(format!("yours {yours}"));
    }
    let after = |parts: Vec<String>| {
        if parts.is_empty() {
            String::new()
        } else {
            format!(" · {}", parts.join(" · "))
        }
    };
    let first_pass = through
        .first_pass
        .map(|share| format!("{:.0}% passed review first time", share * 100.0));
    let for_limit =
        (through.for_limit > 0).then(|| format!("{} for a usage limit", through.for_limit));
    let questions = match through.answered {
        1 => " question on autopilot",
        _ => " questions on autopilot",
    };
    [
        ("workflows", through.workflows, after(kinds)),
        (
            "tasks merged",
            through.merged,
            after(first_pass.into_iter().collect()),
        ),
        (
            "fallbacks",
            through.fallbacks,
            after(for_limit.into_iter().collect()),
        ),
        ("answered for you", through.answered, questions.to_owned()),
    ]
}

/// The workflows Argonauts Code ships; any other name is one of yours.
fn is_builtin_workflow(name: &str) -> bool {
    argonauts_core::workflows::workflow_files::built_in(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::theme::{ColorLevel, Theme};

    fn text(drawn: &Drawn) -> Vec<String> {
        drawn
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn the_card_counts_what_ran_through_argonauts() {
        let theme = Theme::dark().with_level(ColorLevel::None);
        let kit = Kit::new(&theme);
        let through = ThroughArgonauts {
            since: None,
            workflows: 14,
            by_workflow: vec![
                ("council".into(), 8),
                ("orchestrator".into(), 5),
                ("tidy-docs".into(), 1),
            ],
            merged: 1_063,
            first_pass: Some(0.92),
            fallbacks: 4,
            for_limit: 3,
            answered: 11,
        };
        let rows = text(&card(&kit, Frame::view(90), Some(&through)));
        let has = |words: &str| rows.iter().any(|row| row.contains(words));
        assert!(has("◆ through Argonauts Code"), "{rows:#?}");
        assert!(
            has("workflows         14 · Council 8 · Orchestrator 5 · yours 1"),
            "{rows:#?}"
        );
        assert!(
            has("tasks merged      1,063 · 92% passed review first time"),
            "{rows:#?}"
        );
        assert!(
            has("fallbacks         4 · 3 for a usage limit"),
            "{rows:#?}"
        );
        assert!(
            has("answered for you  11 questions on autopilot"),
            "{rows:#?}"
        );
        let empty = text(&card(&kit, Frame::view(90), None));
        assert!(
            empty
                .iter()
                .any(|row| row.contains("Nothing ran through Argonauts Code"))
        );
    }

    /// Stats by CLI ends with the card, under who did what.
    #[test]
    fn stats_shows_it_under_who_did_what() {
        use std::time::Instant;

        use argonauts_core::app::ViewData;
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        use crate::frame::state::AppState;
        use crate::frame::view::{View, ViewCtx};
        use crate::{
            panels::{OverviewView, Tab, UsageWatch},
            testing::sample,
        };

        let mut usage = sample::usage_with_stats();
        usage.argonauts = Some(ThroughArgonauts {
            workflows: 3,
            by_workflow: vec![("council".into(), 3)],
            merged: 5,
            ..ThroughArgonauts::default()
        });
        let mut state = AppState::default();
        state.names = sample::names();
        state.usage = Some(std::sync::Arc::new(usage.clone()));
        let mut view = OverviewView::new(
            Tab::Stats,
            UsageWatch::default(),
            crate::panels::view::NoConfig::boxed(),
        )
        .with_period(usage.period)
        .at(sample::now());
        view.accept(&ViewData::Usage {
            summary: Box::new(usage),
        });
        let theme = Theme::dark().with_level(ColorLevel::None);
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        let area = Rect::new(0, 0, 120, 200);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf, &ctx);
        let rows: Vec<String> = crate::frame::view::dock::lines(&buf)
            .iter()
            .map(|line| line.to_string())
            .collect();
        let at = |words: &str| rows.iter().position(|row| row.contains(words));
        let who = at("◆ who did what").expect("who did what");
        let through = at("◆ through Argonauts Code").expect("through Argonauts Code");
        assert!(who < through, "{rows:#?}");
        assert!(at("workflows         3 · Council 3").is_some(), "{rows:#?}");
    }
}
