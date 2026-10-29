use std::collections::BTreeSet;
use std::time::Instant;

use argonauts_core::Spend;
use argonauts_core::api::{DaySpend, UsagePeriod};
use argonauts_core::app::ViewData;
use argonauts_core::names::ModelRef;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use time::Duration;

use super::crew_like::{AGY, CLAUDE, CODEX, CODEX_DEFAULT, OPENCODE};
use super::{CrewLike, crew_like};
use crate::approved_screen;
use crate::overview::{OverviewView, Tab, UsageWatch, sample};
use crate::stability;
use crate::text::str_width;
use crate::theme::{ColorLevel, Glyphs, Theme};
use crate::views::{View, ViewCtx};

/// The sizes people run the app at: the demo window, a wide one, the smallest, the largest.
const SIZES: [(u16, u16); 4] = [(120, 38), (177, 40), (80, 24), (265, 60)];

/// Each model the crew's records name, by its CLI and the name it shows under.
fn shown_models(crew: &CrewLike) -> BTreeSet<(String, String)> {
    let stats = crew.usage.stats.as_ref().expect("the crew's stats");
    stats
        .work
        .iter()
        .map(|row| crew.names.shown_as(&row.agent, row.model.as_deref()))
        .collect()
}

#[test]
fn the_crew_runs_ten_models_over_four_clis() {
    let crew = crew_like();
    let models = shown_models(&crew);
    assert!(models.len() >= 10, "{models:#?}");
    let clis: BTreeSet<&str> = models.iter().map(|(agent, _)| agent.as_str()).collect();
    assert_eq!(clis, BTreeSet::from([AGY, CLAUDE, CODEX, OPENCODE]));
    assert!(
        models.iter().all(|(_, name)| !name.is_empty()),
        "every model has a name: {models:#?}"
    );
}

#[test]
fn history_outside_argonauts_outranks_the_crews_busiest_models() {
    let crew = crew_like();
    let stats = crew.usage.stats.as_ref().unwrap();
    for cli in [CLAUDE, CODEX] {
        let busiest = stats
            .work
            .iter()
            .filter(|row| row.agent == cli)
            .max_by_key(|row| row.spent.total_tokens())
            .and_then(|row| row.model.clone())
            .unwrap();
        let on_this_computer: Vec<&str> = (crew.overview.models.iter())
            .filter(|model| model.agent == cli)
            .filter_map(|model| model.model.as_deref())
            .collect();
        let place = on_this_computer.iter().position(|model| *model == busiest);
        assert!(
            place.is_some_and(|place| place >= 3),
            "{cli}: {busiest} in {on_this_computer:?}"
        );
    }
}

#[test]
fn colours_rank_what_ran_through_argonauts_first() {
    let crew = crew_like();
    let ranked = &crew.usage.all_time_models;
    let here: Vec<u64> = ranked.iter().map(|model| model.tokens).collect();
    assert!(here.is_sorted_by(|a, b| a >= b), "{here:?}");
    let place = |model: &str| {
        (ranked.iter())
            .position(|ranked| ranked.model.as_deref() == Some(model))
            .unwrap()
    };
    assert_eq!(ranked[0].model.as_deref(), Some("claude-haiku-5-5"));
    assert!(place("claude-opus-5-5") < place("claude-sonnet-4-5"));
    // Models that never ran here follow by their count on this computer.
    assert!(place("claude-sonnet-4-5") < place("claude-opus-4-8"));
    assert!(place("claude-opus-4-8") < place("claude-opus-5"));
    assert_eq!(crew.overview.all_time_models, crew.usage.all_time_models);
}

#[test]
fn sessions_that_named_no_model_are_on_every_list_and_run_codexs_default() {
    let crew = crew_like();
    let stats = crew.usage.stats.as_ref().unwrap();
    let unnamed = |agent: &str, model: Option<&str>| agent == CODEX && model.is_none();
    assert!(
        stats
            .work
            .iter()
            .any(|row| unnamed(&row.agent, row.model.as_deref()))
    );
    assert!(
        stats
            .tasks
            .iter()
            .any(|task| unnamed(&task.agent, task.model.as_deref()))
    );
    assert!(
        stats
            .days
            .iter()
            .any(|day| unnamed(&day.agent, day.model.as_deref()))
    );
    assert!(
        crew.usage
            .by_agent
            .iter()
            .any(|share| unnamed(&share.agent, share.model.as_deref()))
    );
    assert!(
        crew.usage
            .all_time_models
            .iter()
            .any(|model| unnamed(&model.agent, model.model.as_deref()))
    );
    assert!(
        crew.context
            .rows
            .iter()
            .any(|row| unnamed(&row.worker.agent, row.worker.model.as_deref()))
    );
    let running = crew.names.running(&ModelRef::new(CODEX, None));
    assert_eq!(running.model.as_deref(), Some(CODEX_DEFAULT));
}

#[test]
fn some_names_are_longer_than_a_narrow_column() {
    let crew = crew_like();
    let longest = shown_models(&crew)
        .into_iter()
        .map(|(_, name)| str_width(&name))
        .max()
        .unwrap();
    assert!(longest >= 22, "{longest}");
    let run = crew.usage.runs.iter().filter_map(|run| run.name.as_deref());
    assert!(run.map(str_width).max().unwrap() > 50);
    let folder = crew
        .overview
        .folders
        .iter()
        .map(|(path, _)| str_width(path));
    assert!(folder.max().unwrap() > 60);
}

#[test]
fn two_models_ran_without_using_a_token() {
    let crew = crew_like();
    let stats = crew.usage.stats.as_ref().unwrap();
    let silent: BTreeSet<_> = stats
        .work
        .iter()
        .filter(|row| row.spent.total_tokens() == 0)
        .map(|row| (row.agent.as_str(), row.model.as_deref()))
        .collect();
    assert_eq!(silent.len(), 2, "{silent:?}");
    for (agent, model) in silent {
        let ranked = (crew.usage.all_time_models.iter())
            .find(|ranked| ranked.agent == agent && ranked.model.as_deref() == model);
        assert_eq!(ranked.map(|ranked| ranked.tokens), Some(0), "{model:?}");
        let seen = (crew.overview.models.iter())
            .find(|seen| seen.agent == agent && seen.model.as_deref() == model)
            .unwrap();
        assert!(seen.tokens.total() > 0, "{seen:?}");
    }
}

#[test]
fn the_week_so_far_has_spent_nothing_though_the_period_has() {
    let crew = crew_like();
    let usage = &crew.usage;
    let today = crew.now.date();
    let week_start = UsagePeriod::Week
        .first_day(today, usage.week_starts.weekday())
        .unwrap();
    assert!(week_start < today, "the week is days old");
    let this_week: Vec<_> = usage
        .history
        .iter()
        .filter(|day| day.day >= week_start)
        .collect();
    assert!(this_week.len() >= 3, "{this_week:?}");
    assert!(this_week.iter().all(|day| day.spent.cost_usd == 0.0));
    assert!(
        usage
            .by_agent
            .iter()
            .all(|share| share.this_week.is_empty())
    );
    assert_eq!(usage.history.last().map(|day| day.day), Some(today));
    assert!(usage.total.cost_usd > 7.0);
}

/// Stats and Limits show what their drawings show: 12.69M tokens in all, ~$7.93,
/// Haiku 5.5 with half the tokens and Opus 5.5 with two thirds of the spend.
#[test]
fn every_list_adds_up_to_the_same_figures() {
    let crew = crew_like();
    let usage = &crew.usage;
    let stats = usage.stats.as_ref().unwrap();
    let tokens = usage.total.total_tokens();
    assert_eq!(tokens, 12_690_000);
    assert_eq!(
        usage
            .by_agent
            .iter()
            .map(|share| share.spent.total_tokens())
            .sum::<u64>(),
        tokens
    );
    assert_eq!(
        stats
            .work
            .iter()
            .map(|row| row.spent.total_tokens())
            .sum::<u64>(),
        tokens
    );
    assert_eq!(stats.days.iter().map(|day| day.tokens).sum::<u64>(), tokens);
    let history: u64 = usage
        .history
        .iter()
        .map(|day| day.spent.total_tokens())
        .sum();
    assert_eq!(history, tokens);
    let spend: f64 = usage.history.iter().map(|day| day.spent.cost_usd).sum();
    assert!((spend - usage.total.cost_usd).abs() < 1e-9, "{spend}");
    assert!((usage.total.cost_usd - 7.927).abs() < 1e-9);

    let haiku = usage
        .by_agent
        .iter()
        .find(|share| share.model.as_deref() == Some("claude-haiku-5-5"))
        .unwrap();
    assert_eq!(haiku.spent.total_tokens() * 100 / tokens, 51);
    let opus = &usage.by_agent[0];
    assert_eq!(opus.model.as_deref(), Some("claude-opus-5-5"));
    assert!(!opus.spent.cost_estimated);
    assert_eq!(
        (opus.spent.cost_usd / usage.total.cost_usd * 100.0).round(),
        68.0
    );

    let overview: u64 = crew.overview.days.iter().map(|day| day.tokens).sum();
    assert_eq!(overview, crew.overview.total.total());
}

fn plain() -> Theme {
    let mut theme = Theme::dark().with_level(ColorLevel::None);
    theme.glyphs = Glyphs::UNICODE;
    theme
}

/// `tab` drawn from the crew on a screen `cols` by `rows`, at the height it asks for.
fn drawn(crew: &CrewLike, tab: Tab, (cols, rows): (u16, u16)) -> Vec<String> {
    let theme = plain();
    let state = crew.state();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let mut view = OverviewView::new(tab, UsageWatch::default())
        .at(crew.now)
        .with_period(UsagePeriod::All)
        .with_context(crew.context.clone());
    view.accept(&ViewData::Overview {
        overview: Box::new(crew.overview.clone()),
    });
    let screen = Size::new(cols, rows);
    let height = view
        .dock_height(screen, &ctx)
        .unwrap_or_else(|| crate::views::dock::height(rows));
    let area = Rect::new(0, 0, cols, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    stability::assert_frames(&buf);
    crate::views::dock::lines(&buf)
        .iter()
        .map(|line| line.to_string().trim_end().to_owned())
        .collect()
}

#[test]
fn every_tab_draws_the_crew_at_the_sizes_people_use() {
    let crew = crew_like();
    for size in SIZES {
        for tab in Tab::ALL {
            let shown = drawn(&crew, tab, size);
            assert!(shown.len() >= 5, "{tab:?} at {size:?}: {shown:#?}");
        }
    }
    let limits = drawn(&crew, Tab::Limits, (177, 40)).join("\n");
    for name in ["Opus 5.5", "GPT 6 Astra", "Gemma4 E2B"] {
        assert!(limits.contains(name), "{name}: {limits}");
    }
}

/// `tab` drawn from the crew after `keys`, docked at the foot of a screen `cols` × `rows` as
/// the app docks it, in the theme the approved drawings were made in: the buffer and the
/// screen row it starts at.
fn docked(crew: &CrewLike, tab: Tab, size: (u16, u16), keys: &[KeyCode]) -> (Buffer, u16) {
    docked_in(&approved_screen::theme(), crew, tab, size, keys)
}

/// The theme the drawings on a light terminal were made in: the light theme, in full colour,
/// with Unicode glyphs and logos drawn as pictures.
fn light() -> Theme {
    let mut theme = Theme::light().with_level(ColorLevel::TrueColor);
    theme.glyphs = Glyphs::UNICODE;
    theme.logo_pictures = true;
    theme
}

/// [`docked`] in `theme`.
fn docked_in(
    theme: &Theme,
    crew: &CrewLike,
    tab: Tab,
    (cols, rows): (u16, u16),
    keys: &[KeyCode],
) -> (Buffer, u16) {
    let state = crew.state();
    let ctx = ViewCtx {
        theme,
        state: &state,
        now: Instant::now(),
    };
    let mut view = OverviewView::new(tab, UsageWatch::default())
        .at(crew.now)
        .with_period(crew.usage.period);
    view.accept(&ViewData::Status {
        summary: sample::status(),
    });
    view.accept(&ViewData::Context {
        view: crew.context.clone(),
    });
    view.accept(&ViewData::Overview {
        overview: Box::new(crew.overview.clone()),
    });
    for key in keys {
        view.handle_key(KeyEvent::from(*key), &ctx);
    }
    let height = view
        .dock_height(Size::new(cols, rows), &ctx)
        .unwrap_or(rows);
    let area = Rect::new(0, 0, cols, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    (buf, rows - height)
}

#[test]
fn limits_draws_the_crew_as_its_drawings_do() {
    let crew = crew_like();
    for (file, size) in [
        ("limits/spend-card@120x38.txt", (120, 38)),
        ("limits/spend-card@177x40.txt", (177, 40)),
    ] {
        let (buf, top) = docked(&crew, Tab::Limits, size, &[]);
        approved_screen::assert_matches(file, &buf, size, top);
    }
}

/// The crew with spend since Sunday: on the two days the week has had, each model's dollars.
#[test]
fn limits_draws_a_week_with_spend_as_its_drawing_does() {
    let mut crew = crew_like();
    let sunday = crew.now.date() - Duration::days(2);
    let week: [(&str, [f64; 2]); 6] = [
        ("claude-opus-5-5", [0.20, 0.62]),
        ("gpt-6-astra", [0.41, 0.0]),
        ("claude-haiku-5-5", [0.08, 0.27]),
        (CODEX_DEFAULT, [0.0, 0.18]),
        ("Gemini 3.8 Flash (Low)", [0.0, 0.09]),
        ("gpt-6-luna", [0.04, 0.0]),
    ];
    for share in &mut crew.usage.by_agent {
        let days = week
            .iter()
            .find(|(model, _)| share.model.as_deref() == Some(*model))
            .map_or([0.0; 2], |(_, days)| *days);
        share.this_week = (days.iter().enumerate())
            .filter(|(_, usd)| **usd > 0.0)
            .map(|(at, usd)| DaySpend {
                day: sunday + Duration::days(at as i64),
                spent: Spend {
                    cost_usd: *usd,
                    ..Spend::default()
                },
            })
            .collect();
    }
    let (buf, top) = docked(&crew, Tab::Limits, (120, 38), &[]);
    approved_screen::assert_matches("limits/spend-card-week@120x38.txt", &buf, (120, 38), top);
}

/// The limits the tab is named for show under the spend card, not below the fold, wherever
/// the spend card leaves them room. At 80 × 24 the card alone fills the panel.
#[test]
fn limits_shows_its_list_at_the_sizes_people_use() {
    let crew = crew_like();
    let small = drawn(&crew, Tab::Limits, (80, 24));
    assert!(
        small.last().is_some_and(|keys| keys.contains("↓  more")),
        "{small:#?}"
    );
    for size in SIZES.into_iter().filter(|size| *size != (80, 24)) {
        let shown = drawn(&crew, Tab::Limits, size);
        let heading = shown.iter().position(|line| line.starts_with(" Limits · "));
        let Some(heading) = heading else {
            panic!("no list at {size:?}: {shown:#?}");
        };
        let first = shown.get(heading + 1).map(String::as_str).unwrap_or("");
        assert!(first.contains("Spend for a run"), "at {size:?}: {shown:#?}");
    }
}

/// Stats › By model, all time, the sub-tabs chosen: its table as the drawings have it, each
/// model in its CLI's hue and no two of a CLI in one shape, and no row chosen. At 120 columns
/// the model column gives up the room the screen lacks, and on a light terminal each hue is the
/// light theme's.
#[test]
fn stats_by_model_draws_the_crew_as_its_drawings_do() {
    let mut crew = crew_like();
    crew.usage.period = UsagePeriod::All;
    let keys = [KeyCode::Down, KeyCode::Right];
    let (dark, light) = (approved_screen::theme(), light());
    for (file, theme, size) in [
        ("stats/by-model-table@177x40.txt", &dark, (177, 40)),
        ("stats/by-model-table@120x38.txt", &dark, (120, 38)),
        (
            "stats/by-model-table-light-terminal@120x38.txt",
            &light,
            (120, 38),
        ),
    ] {
        let (buf, top) = docked_in(theme, &crew, Tab::Stats, size, &keys);
        approved_screen::assert_matches(file, &buf, size, top);
    }
}
