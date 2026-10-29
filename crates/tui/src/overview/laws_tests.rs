//! Every tab of the panel keeps the screen laws at the sizes people use: as it opens, with
//! names longer than its columns, and down a list longer than its room.

use std::collections::BTreeSet;
use std::time::Instant;

use argonauts_core::api::StatusSummary;
use argonauts_core::app::ViewData;
use argonauts_core::behavior_map::BehaviorMap;
use argonauts_testkit::screen::{Grid, Look, REAL_SIZES, Size, laws};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size as Room};

use super::golden::theme;
use super::sample::{CrewLike, crew_like};
use super::stats::behaviors::sample::three_models;
use super::{OverviewView, Tab, UsageWatch, sample};
use crate::stability::grid::grid;
use crate::views::{View, ViewCtx};

/// What the panel is drawn from: a crew shaped like a real machine's, and the run its Status
/// tab shows.
struct Machine {
    crew: CrewLike,
    run: StatusSummary,
}

fn machine() -> Machine {
    Machine {
        crew: crew_like(),
        run: sample::status(),
    }
}

/// The panel on `tab` after `keys`, drawn in colour from `machine` at the height it asks for
/// on a screen of `size`, with every answer it asks for in.
fn drawn(machine: &Machine, tab: Tab, size: Size, keys: &[KeyCode]) -> Grid {
    drawn_with(machine, tab, &three_models().map, size, keys)
}

/// [`drawn`], the Behaviors sub-tab answered with `map`.
fn drawn_with(
    machine: &Machine,
    tab: Tab,
    map: &BehaviorMap,
    size: Size,
    keys: &[KeyCode],
) -> Grid {
    let crew = &machine.crew;
    let theme = theme();
    let state = crew.state();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let mut view = OverviewView::new(tab, UsageWatch::default())
        .at(crew.now)
        .with_period(crew.usage.period);
    view.accept(&ViewData::Status {
        summary: machine.run.clone(),
    });
    view.accept(&ViewData::Context {
        view: crew.context.clone(),
    });
    view.accept(&ViewData::Overview {
        overview: Box::new(crew.overview.clone()),
    });
    // The Behaviors sub-tab takes its map only once it's open and has asked for it.
    let mut mapped = false;
    for key in keys {
        view.handle_key(KeyEvent::from(*key), &ctx);
        if !mapped {
            mapped = view.accept(&ViewData::Behaviors {
                map: Box::new(map.clone()),
            });
        }
    }
    let room = Room::new(size.cols, size.rows);
    let height = view.dock_height(room, &ctx).unwrap_or(size.rows);
    let area = Rect::new(0, 0, size.cols, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    grid(&buf)
}

/// Every name the crew's records and the run's tasks show their models under.
fn models(machine: &Machine) -> BTreeSet<String> {
    let Machine { crew, run } = machine;
    let usage = &crew.usage;
    let shares = usage
        .by_agent
        .iter()
        .map(|share| (&share.agent, &share.model));
    let stats = usage.stats.iter().flat_map(|stats| {
        let work = stats.work.iter().map(|row| (&row.agent, &row.model));
        let tasks = stats.tasks.iter().map(|task| (&task.agent, &task.model));
        work.chain(tasks)
    });
    let in_all = usage
        .all_time_models
        .iter()
        .map(|model| (&model.agent, &model.model));
    let workers = run
        .tasks
        .iter()
        .filter_map(|task| task.worker.as_ref())
        .map(|worker| (&worker.agent, &worker.model));
    shares
        .chain(stats)
        .chain(in_all)
        .chain(workers)
        .map(|(agent, model)| crew.names.shown_as(agent, model.as_deref()).1)
        .filter(|name| !name.is_empty())
        .collect()
}

/// What the laws need to know of a screen drawn from `machine`: its models, and their names
/// and its CLIs' to show whole.
fn look(machine: &Machine) -> Look {
    let models: Vec<String> = models(machine).into_iter().collect();
    let clis: Vec<&String> = machine.crew.names.clis.values().collect();
    Look::new().models(&models).whole(&models).whole(&clis)
}

/// Words that make every model's id longer than any column gives its name.
const LONGER: &str = " with the longest name of all";

/// The machine with every model it names made longer by [`LONGER`], its CLIs' defaults too, so
/// a session that named none shows the longer name.
fn long_names() -> Machine {
    let lengthen = |model: &mut Option<String>| {
        if let Some(model) = model {
            model.push_str(LONGER);
        }
    };
    let Machine { mut crew, mut run } = machine();
    crew.names
        .defaults
        .values_mut()
        .for_each(|model| model.push_str(LONGER));
    let usage = &mut crew.usage;
    usage
        .by_agent
        .iter_mut()
        .for_each(|share| lengthen(&mut share.model));
    usage
        .all_time_models
        .iter_mut()
        .for_each(|model| lengthen(&mut model.model));
    if let Some(stats) = &mut usage.stats {
        stats
            .work
            .iter_mut()
            .for_each(|row| lengthen(&mut row.model));
        stats
            .tasks
            .iter_mut()
            .for_each(|task| lengthen(&mut task.model));
        stats
            .days
            .iter_mut()
            .for_each(|day| lengthen(&mut day.model));
    }
    run.tasks
        .iter_mut()
        .filter_map(|task| task.worker.as_mut())
        .for_each(|worker| lengthen(&mut worker.model));
    Machine { crew, run }
}

/// A dozen things Haiku does, more than the Behaviors list has rows for at most sizes.
const DOES: [&str; 12] = [
    "read a source file",
    "edited a source file",
    "ran the tests",
    "read git history",
    "searched the code",
    "wrote a new test",
    "listed a folder",
    "ran a build",
    "read the docs",
    "fixed a lint",
    "opened a pull request",
    "asked a question",
];

/// [`DOES`] as the list writes its rows, each starting with a capital.
fn listed() -> Vec<String> {
    DOES.iter()
        .map(|does| {
            let mut letters = does.chars();
            letters
                .next()
                .map(|first| first.to_uppercase().chain(letters).collect())
                .unwrap_or_default()
        })
        .collect()
}

/// Haiku doing each of [`DOES`].
fn busy() -> BehaviorMap {
    let mut sample = three_models();
    let mut rows = Vec::new();
    for (at, does) in DOES.iter().enumerate() {
        let place = (0.1 + 0.07 * at as f32, 0.15 + 0.06 * at as f32);
        sample = sample.actions(0, does, 8 + at, place);
        rows.push((*does, 8 + at, at % 3));
    }
    sample.rows(0, &rows).map
}

/// Down from the tabs to the sub-tabs, across to Behaviors, and into its list.
const INTO_THE_LIST: [KeyCode; 7] = [
    KeyCode::Down,
    KeyCode::Right,
    KeyCode::Right,
    KeyCode::Down,
    KeyCode::Down,
    KeyCode::Right,
    KeyCode::Down,
];

/// The tab as it opens, at every real size, with the crew's names and with longer ones, the
/// row showing `selected` drawn as selected when one is named.
fn keeps_the_laws(tab: Tab, keys: &[KeyCode], selected: Option<&str>) {
    let plain = machine();
    let longer = long_names();
    for size in REAL_SIZES {
        for (machine, names) in [(&plain, ""), (&longer, ", its names longer")] {
            let mut look = look(machine);
            if let Some(selected) = selected {
                look = look.selected(selected);
            }
            let grid = drawn(machine, tab, size, keys);
            laws::assert_kept(&grid, &look, &format!("{tab:?} at {size}{names}"));
        }
    }
}

#[test]
fn overview_keeps_the_screen_laws() {
    keeps_the_laws(Tab::Overview, &[], None);
}

#[test]
fn status_keeps_the_screen_laws() {
    keeps_the_laws(Tab::Status, &[], None);
}

#[test]
fn config_keeps_the_screen_laws() {
    keeps_the_laws(Tab::Config, &[], None);
}

#[test]
fn usage_keeps_the_screen_laws() {
    keeps_the_laws(Tab::Usage, &[], None);
}

#[test]
fn context_keeps_the_screen_laws() {
    keeps_the_laws(Tab::Context, &[], None);
}

#[test]
fn limits_keeps_the_screen_laws() {
    keeps_the_laws(Tab::Limits, &[], None);
}

#[test]
fn stats_by_cli_keeps_the_screen_laws() {
    keeps_the_laws(Tab::Stats, &[], None);
}

#[test]
fn stats_by_model_keeps_the_screen_laws() {
    keeps_the_laws(Tab::Stats, &[KeyCode::Char('m')], Some("Haiku 5.5"));
}

/// Down to the sub-tabs, along to By model, into the cards, and Enter on the first.
#[test]
fn stats_with_a_models_card_open_keeps_the_screen_laws() {
    let keys = [KeyCode::Down, KeyCode::Right, KeyCode::Down, KeyCode::Enter];
    keeps_the_laws(Tab::Stats, &keys, None);
}

/// Down the Behaviors list one row at a time, past its last row: the pointer stays in view
/// and the list counts the rows it hides at every step, at every size.
#[test]
fn the_behaviors_list_keeps_the_screen_laws_all_the_way_down() {
    let machine = machine();
    let map = busy();
    let rows = listed();
    for size in REAL_SIZES {
        let mut keys = INTO_THE_LIST.to_vec();
        for down in 0..=12 {
            let grid = drawn_with(&machine, Tab::Overview, &map, size, &keys);
            let screen = format!("Behaviors at {size}, {down} rows down the list");
            let look = look(&machine).selection().list(&rows);
            laws::assert_kept(&grid, &look, &screen);
            keys.push(KeyCode::Down);
        }
    }
}
