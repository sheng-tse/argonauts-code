//! Nowhere in the panel does a number or a name touch a bar: every tab, drawn at 80, 100 and
//! 140 columns from numbers that fill windows to 0%, 100% and 105% and names that fill their
//! columns, keeps a blank cell between any bar or squares and the text beside them.

use std::sync::Arc;
use std::time::Instant;

use argonauts_core::api::UsageSummary;
use argonauts_core::app::ViewData;
use argonauts_core::plan_usage::LiveWindow;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};

use super::golden::theme;
use super::{OverviewView, Tab, UsageWatch, sample};
use crate::kit::bars::touching_bar;
use crate::state::AppState;
use crate::views::{View, ViewCtx};
use crossterm::event::KeyCode;

/// The crew's numbers with every window at an edge and long names wherever a name goes.
fn edges() -> UsageSummary {
    let mut usage = sample::usage_with_stats();
    let shares = [0.0, 1.0, 1.05, 0.5];
    for (at, plan) in usage.plans.iter_mut().enumerate() {
        plan.name = format!("{} with a long name", plan.name);
        plan.plan = plan.plan.as_ref().map(|tier| format!("{tier} Ultra Pro"));
        for (k, reading) in plan.windows.iter_mut().enumerate() {
            reading.used = Some(shares[(at + k) % shares.len()]);
        }
        if let Some(live) = &mut plan.live {
            for (k, window) in live.windows.iter_mut().enumerate() {
                window.used = Some(shares[(at + k + 1) % shares.len()]);
                window.label = format!("{} of the longest model name", window.label);
            }
            live.windows.push(
                LiveWindow::new("model:long", "A model with a long name week", "").used(Some(1.05)),
            );
        }
    }
    for share in &mut usage.by_agent {
        if let Some(model) = &mut share.model {
            model.push_str(" with the longest name of all");
        }
    }
    usage
}

fn state() -> AppState {
    let mut state = AppState::default();
    state.usage = Some(Arc::new(edges()));
    state.names = sample::names();
    state
}

fn drawn(tab: Tab, state: &AppState, width: u16) -> Vec<String> {
    let theme = theme();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    let mut view = OverviewView::new(tab, UsageWatch::default()).at(sample::now());
    view.accept(&ViewData::Status {
        summary: sample::status(),
    });
    view.accept(&ViewData::Context {
        view: sample::context(),
    });
    view.accept(&ViewData::Overview {
        overview: Box::new(super::home::sample::overview()),
    });
    let height = view.dock_height(Size::new(width, 200), &ctx).unwrap_or(200);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    crate::views::dock::lines(&buf)
        .iter()
        .map(|line| line.to_string())
        .collect()
}

#[test]
fn no_number_or_name_touches_a_bar_on_any_tab_at_any_width() {
    let state = state();
    let mut touching = Vec::new();
    for tab in Tab::ALL {
        for width in [80, 100, 140] {
            for row in drawn(tab, &state, width) {
                if let Some(column) = touching_bar(&row) {
                    touching.push(format!("{tab:?} at {width}, column {column}: {row}"));
                }
            }
        }
    }
    assert!(touching.is_empty(), "{}", touching.join("\n"));
}

/// Long model names, a long kind of work and four-digit counts in the crew's numbers.
fn crowded() -> AppState {
    let mut state = state();
    let mut usage = edges();
    if let Some(stats) = &mut usage.stats {
        for row in &mut stats.work {
            if let Some(model) = &mut row.model {
                model.push_str("-extended-thinking");
            }
            if row.kind.is_some() {
                row.kind = Some("a-very-long-kind-of-work".to_owned());
            }
            row.done = row.done.max(1000);
        }
    }
    state.usage = Some(Arc::new(usage));
    state
}

/// The Stats tab after `keys`, as every sub-view and every opened row or card draws it.
fn drawn_after(state: &AppState, width: u16, keys: &[KeyCode]) -> Vec<String> {
    let theme = theme();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    let period = state.usage.as_ref().map(|usage| usage.period);
    let mut view = OverviewView::new(Tab::Stats, UsageWatch::default()).at(sample::now());
    if let Some(period) = period {
        view = view.with_period(period);
    }
    for key in keys {
        // Enter opens the first CLI's card, past the all-together card, once the focus is down
        // among the cards.
        if *key == KeyCode::Enter {
            for _ in 0..8 {
                if view.level() == super::subtabs::Level::Cards {
                    break;
                }
                view.handle_key(crossterm::event::KeyEvent::from(KeyCode::Down), &ctx);
            }
            view.handle_key(crossterm::event::KeyEvent::from(KeyCode::Down), &ctx);
        }
        view.handle_key(crossterm::event::KeyEvent::from(*key), &ctx);
    }
    let height = view.dock_height(Size::new(width, 200), &ctx).unwrap_or(200);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    crate::views::dock::lines(&buf)
        .iter()
        .map(|line| line.to_string())
        .collect()
}

/// A small square with anything but a blank after it, by its column.
fn square_touching(line: &str) -> Option<usize> {
    let cells: Vec<char> = line.chars().collect();
    cells
        .windows(2)
        .position(|pair| pair[0] == '■' && !pair[1].is_whitespace())
}

#[test]
fn no_number_or_name_touches_a_bar_in_any_stats_view_opened_or_not() {
    use crossterm::event::KeyCode::{Char, Down, Enter, Left};
    let state = crowded();
    let views: [(&str, Vec<KeyCode>); 6] = [
        ("By CLI", vec![]),
        ("a CLI card opened", vec![Enter]),
        ("By model", vec![Char('m')]),
        ("a model opened", vec![Char('m'), Enter]),
        ("By role", vec![Down, Left]),
        ("a role opened", vec![Down, Left, Enter]),
    ];
    let mut touching = Vec::new();
    for (name, keys) in &views {
        for width in [80, 100, 118, 140] {
            let rows = drawn_after(&state, width, keys);
            if name.ends_with("opened") {
                // A CLI and a role say so; a model says the roles it worked as.
                let opened = rows
                    .iter()
                    .any(|row| row.contains("opened with enter") || row.contains(" · as "));
                assert!(
                    opened,
                    "{name} at {width} opens something:\n{}",
                    rows.join("\n")
                );
            }
            for row in rows {
                let at = touching_bar(&row).or_else(|| square_touching(&row));
                if let Some(column) = at {
                    touching.push(format!("{name} at {width}, column {column}: {row}"));
                }
            }
        }
    }
    assert!(touching.is_empty(), "{}", touching.join("\n"));
}
