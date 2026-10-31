//! The Behaviors sub-tab drawn from made-up maps at 120 × 33, the panel above the input on a
//! 120 × 38 screen, and compared with its pictures cell for cell, colours included. A picture
//! holds the whole panel: the tabs and sub-tabs as the panel draws them, the world's row of
//! levels, the map's cells left blank for its picture or drawn in braille, what ← → steps through
//! beside it, the pointer's line, the summary and the keys.

use std::time::Instant;

use argonauts_core::app::ViewData;
use argonauts_core::app::overview::{ModelTokens, OverviewPeriod};
use argonauts_core::behaviors::behavior_map::{BehaviorMap, GRID, MapRequest, Placing};
use argonauts_core::behaviors::behavior_world::{ModelBehavior, ModelFacts, WorldFacts};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use time::macros::datetime;

use super::sample::Sample;
use super::{Fit, MAP_INSET};

/// The cell the pictures here were drawn at, 18 × 42 px, where the map is 64 × 13.
const DRAWN_AT: (u16, u16) = (18, 42);
const DRAWN_COLUMNS: usize = Fit::DRAWN.columns;
const DRAWN_ROWS: usize = Fit::DRAWN.rows;
use crate::frame::state::AppState;
use crate::frame::view::map::{MapCells, MapView};
use crate::frame::view::{View, ViewCtx, ViewOutcome};
use crate::panels::{OverviewView, Tab, UsageWatch};
use crate::testing::pictures::golden::{self, Picture, Region};

const WIDTH: u16 = 120;
const HEIGHT: u16 = 33;
/// The body's first row: under the rule, the tabs, a blank, the sub-tabs and a blank.
const BODY: u16 = 5;

/// The behaviors the made-up crew's other actions fall under, none of them as common as the
/// six the summary lists.
const FILLER: [&str; 10] = [
    "listed a folder",
    "wrote a doc",
    "built the project",
    "changed git state",
    "read git history",
    "ran a shell command",
    "read a config file",
    "wrote a test",
    "fetched a web page",
    "tried an edit that didn't apply",
];

/// The panel as `buf` holds it, each logo drawn as a picture written as its stand-ins, compared
/// whole with its picture.
fn screen(name: &str, buf: &Buffer) {
    let buf = crate::base::kit::logo::golden::with_stand_ins(buf);
    golden::keep(name, &buf);
    let picture = Picture::screen("behaviors", name);
    let theme = golden::theme();
    golden::assert_matches(&picture, &buf, &Region::whole(&picture, (0, 0)), &theme);
}

/// Every group at its size, Luna's and Haiku's behaviors at their counts, the rest spread over
/// the filler behaviors, over the last 30 days.
fn label_map() -> BehaviorMap {
    let request = MapRequest {
        period: OverviewPeriod::Month,
        ..MapRequest::default()
    };
    let mut sample = groups(request);
    let place = |n: usize| (0.1 + 0.07 * n as f32, 0.2 + 0.05 * n as f32);
    for (n, (behavior, mine, others)) in LUNA.iter().enumerate() {
        sample =
            sample
                .actions(1, behavior, *mine, place(n))
                .actions(0, behavior, *others, place(n));
    }
    for (n, (behavior, mine, others)) in HAIKU.iter().enumerate() {
        sample = sample.actions(0, behavior, *mine, place(n + 5)).actions(
            2,
            behavior,
            *others,
            place(n + 5),
        );
    }
    let mut filler = 0;
    for (group, size) in SIZES.iter().enumerate() {
        for _ in sample.map.groups[group].actions..*size {
            let behavior = FILLER[filler % FILLER.len()];
            sample = sample.actions(group, behavior, 1, (0.9, 0.9));
            filler += 1;
        }
    }
    sample.rows(1, &LUNA).rows(0, &HAIKU).map
}

/// The map's dots as the braille picture draws them, written out below: each dot an action of the
/// group whose colour its cell takes, with the behaviors of [`label_map`], and Luna's red and
/// blue shading where it does more and less.
fn braille_map() -> BehaviorMap {
    const DOTS: &str = r#"                                   ⠈⠉                           
                                         ⡀ ⣀ ⡀                  
  ⣀⡄ ⢀                                ⡨⣸⣼⣕⣿⣶⣧⣔⢅                 
⠆          ⠐  ⣠⣦⣶⣦⡆⠄                   ⠁⠊⠓⠙⠉⠝⠈  ⡀               
            ⡀ ⠈⠙⠙⠉⠃⠈                                            
        ⢀       ⠈                       ⡀                   ⡀   
    ⢀⣦⣤⣤⡀                      ⠒                          ⢲⣵⣶⣵⣧⣗
     ⠉⠉⠁                                                   ⠈⠙⠈⠈ 
                                                                
                     ⢀⢀        ⠣ ⠐⠂                             
                     ⠁⠐⠄⡠⡄⢂    ⣍⢉⡒                              
                      ⠴⠠⢒⠄ ⠐⢀ ⡈                ⠐⠉⠚⠓             
                          ⠒ ⡀⠠⠥⢂⠈                               

...................................00...........................
.........................................0.0.0..................
..00.1................................000000000.................
0..........0..000000...................0000000..0...............
............4.000003............................................
........4.......0.......................0...................1...
....00000......................0..........................121211
.....000...................................................1211.
................................................................
.....................00........4.00.............................
.....................111000....022..............................
......................0001.00.0................0200.............
..........................0.10000...............................

................................................................
................................................................
................................................................
....------......................................................
---------------.................................................
...-----------------.................................+++++++++++
.......---------------...........................+++++++++++++++
...............-----..........................++++++++++++++++++
..........................----.......................++++++++...
..............................----..............................
...................+++++++++.......---..........................
.................+++++..........................................
................................................................"#;
    let parts: Vec<Vec<&str>> = DOTS
        .split("\n\n")
        .map(|part| part.lines().collect())
        .collect();
    let (glyphs, groups_at, shades) = (&parts[0], &parts[1], &parts[2]);
    let request = MapRequest {
        period: OverviewPeriod::Month,
        ..MapRequest::default()
    };
    let mut sample = groups(request);
    let mut grid = vec![0.0f32; GRID * GRID];
    let bits = [
        (0, 0, 0x01),
        (1, 0, 0x02),
        (2, 0, 0x04),
        (0, 1, 0x08),
        (1, 1, 0x10),
        (2, 1, 0x20),
        (3, 0, 0x40),
        (3, 1, 0x80),
    ];
    for row in 0..DRAWN_ROWS {
        let glyphs: Vec<char> = glyphs[row].chars().collect();
        let groups_at: Vec<char> = groups_at[row].chars().collect();
        let shades: Vec<char> = shades[row].chars().collect();
        for column in 0..DRAWN_COLUMNS {
            if let Some(shade @ ('+' | '-')) = shades.get(column) {
                let x = ((column as f32 + 0.5) / DRAWN_COLUMNS as f32 * GRID as f32) as usize;
                let y = ((1.0 - (row as f32 + 0.5) / DRAWN_ROWS as f32) * GRID as f32) as usize;
                grid[y.min(GRID - 1) * GRID + x.min(GRID - 1)] =
                    if *shade == '+' { 1.0 } else { -1.0 };
            }
            let Some(group) = groups_at.get(column).and_then(|g| g.to_digit(10)) else {
                continue;
            };
            let code = glyphs
                .get(column)
                .map_or(0, |ch| u32::from(*ch).wrapping_sub(0x2800));
            for (dr, dc, bit) in bits {
                if code & bit != 0 && code <= 0xff {
                    let x = (column * 2 + dc) as f32 + 0.5;
                    let y = (row * 4 + dr) as f32 + 0.5;
                    let place = (
                        x / (DRAWN_COLUMNS * 2) as f32,
                        1.0 - y / (DRAWN_ROWS * 4) as f32,
                    );
                    sample = sample.at(group as usize, "listed a folder", place);
                }
            }
        }
    }
    for (group, size) in SIZES.iter().enumerate() {
        let short = size.saturating_sub(sample.map.groups[group].actions);
        sample = sample.again(group, short);
    }
    let mut map = sample.rows(1, &LUNA[..4]).map;
    map.comparisons[1].overlay = Some(grid);
    // Each group's actions take the behaviors the label map gives that group, in order.
    let labels = label_map();
    for group in 0..SIZES.len() {
        let behaviors: Vec<String> = labels
            .in_group(group)
            .map(|(_, a)| a.behavior.clone())
            .collect();
        let mine: Vec<usize> = map.in_group(group).map(|(at, _)| at).collect();
        for (at, behavior) in mine.into_iter().zip(behaviors) {
            map.actions[at].label = behavior.clone();
            map.actions[at].behavior = behavior;
        }
    }
    map
}

/// The made-up crew's world: each model's facts from its actions on `map`, Anthropic's, OpenAI's
/// and Google's.
fn world_of(map: &BehaviorMap) -> WorldFacts {
    let end = datetime!(2026-10-20 08:40 -4);
    let models = map
        .groups
        .iter()
        .enumerate()
        .map(|(group, found)| {
            let agent = found.agent.clone().unwrap_or_default();
            let maker = match agent.as_str() {
                "claude-code" => "anthropic",
                "codex" => "openai",
                _ => "google",
            };
            let mut behaviors: Vec<ModelBehavior> = Vec::new();
            for (_, action) in map.in_group(group) {
                match behaviors.iter_mut().find(|b| b.behavior == action.behavior) {
                    Some(b) => b.count += 1,
                    None => behaviors.push(ModelBehavior {
                        behavior: action.behavior.clone(),
                        count: 1,
                        last_used: 2,
                        days_in_last_7: 3,
                        ..ModelBehavior::default()
                    }),
                }
            }
            behaviors.sort_by(|a, b| b.count.cmp(&a.count).then(a.behavior.cmp(&b.behavior)));
            let actions = found.actions as u32;
            ModelFacts {
                model: found.model.clone().unwrap_or_default(),
                agent,
                maker: maker.to_owned(),
                actions,
                last_7_days: actions / 3,
                first_used: 26,
                last_used: 0,
                days_used: 12,
                sessions: 1 + actions / 20,
                merged: actions / 2,
                last_action: Some(end - time::Duration::minutes(5 + 40 * group as i64)),
                behaviors,
                ..ModelFacts::default()
            }
        })
        .collect();
    WorldFacts {
        end,
        models,
        ..WorldFacts::default()
    }
}

fn with_world(mut map: BehaviorMap) -> BehaviorMap {
    map.world = Some(Box::new(world_of(&map)));
    map
}

/// The panel on Behaviors with `map`, after `keys` on the picture, the mouse resting at
/// `pointer` on the map; `size` wide and tall; the world drawn as pictures or in braille.
fn drawn_sized(
    state: &AppState,
    map: &BehaviorMap,
    keys: &[KeyCode],
    pointer: Option<(f32, f32)>,
    (width, height): (u16, u16),
    pictures: bool,
) -> (OverviewView, Buffer) {
    let (mut view, theme) = opened_view(state, map, keys, pointer, pictures, DRAWN_AT);
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    (view, buf)
}

/// The panel docked in a window `width` × `height` with `cell`-pixel cells, as the app docks
/// it: as many rows as its blocks take, up to all but six, the map's pictures placed. The
/// buffer holds the panel's rows.
fn drawn_docked(
    map: &BehaviorMap,
    (width, height): (u16, u16),
    cell: (u16, u16),
) -> (OverviewView, Buffer) {
    drawn_docked_in(&state(), map, &[], (width, height), cell)
}

/// [`drawn_docked`] in `state`, after `keys` on the picture.
fn drawn_docked_in(
    state: &AppState,
    map: &BehaviorMap,
    keys: &[KeyCode],
    (width, height): (u16, u16),
    cell: (u16, u16),
) -> (OverviewView, Buffer) {
    let (mut view, theme) = opened_view(state, map, keys, None, true, cell);
    view.place_pictures(Some(cell));
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    let rows = view
        .dock_height(ratatui::layout::Size::new(width, height), &ctx)
        .expect("the panel docks");
    let area = Rect::new(0, 0, width, rows);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    (view, buf)
}

/// The panel on Behaviors with `map` taken and `keys` pressed in the picture, at `cell` pixels
/// a cell.
fn opened_view(
    state: &AppState,
    map: &BehaviorMap,
    keys: &[KeyCode],
    pointer: Option<(f32, f32)>,
    pictures: bool,
    cell: (u16, u16),
) -> (OverviewView, crate::base::theme::Theme) {
    let theme = if pictures {
        pictured_theme()
    } else {
        golden::theme()
    };
    let mut view = OverviewView::new(
        Tab::Overview,
        UsageWatch::default(),
        crate::panels::view::NoConfig::boxed(),
    )
    .at(datetime!(2026-10-20 08:40 -4))
    .with_cell(cell);
    if pictures {
        view = view.with_map_picture();
    }
    let press = |view: &mut OverviewView, key: KeyEvent| {
        let ctx = ViewCtx {
            theme: &theme,
            state,
            now: Instant::now(),
        };
        view.handle_key(key, &ctx)
    };
    assert!(view.accept(&ViewData::Overview {
        overview: Box::new(models())
    }));
    for code in [KeyCode::Down, KeyCode::Right, KeyCode::Right] {
        press(&mut view, code.into());
    }
    // The period the map is for, stepped to with the panel's r.
    let mut period = OverviewPeriod::All;
    while period != map.request.period {
        match press(&mut view, KeyCode::Char('r').into()) {
            ViewOutcome::Request(argonauts_core::app::ViewRequest::Behaviors(asked)) => {
                period = asked.period;
            }
            other => panic!("r asks for the next period's map, not {other:?}"),
        }
    }
    view.accept(&ViewData::Behaviors {
        map: Box::new(map.clone()),
    });
    // Down onto the groupings, then into the picture.
    press(&mut view, KeyCode::Down.into());
    press(&mut view, KeyCode::Down.into());
    for code in keys {
        press(&mut view, (*code).into());
    }
    if pointer.is_some() {
        assert!(view.point_at_map(pointer), "the mouse rests near a dot");
    }
    (view, theme)
}

/// The goldens' theme in a terminal that draws pictures: the world's, and logos drawn as pictures
/// beside it.
fn pictured_theme() -> crate::base::theme::Theme {
    let mut theme = golden::theme();
    theme.mascot_pictures = true;
    theme.logo_pictures = true;
    theme
}

fn drawn(
    map: &BehaviorMap,
    keys: &[KeyCode],
    pointer: Option<(f32, f32)>,
    pictures: bool,
) -> (OverviewView, Buffer) {
    drawn_sized(&state(), map, keys, pointer, (WIDTH, HEIGHT), pictures)
}

fn draw(map: &BehaviorMap, keys: &[KeyCode], pictures: bool) -> Buffer {
    drawn(map, keys, None, pictures).1
}

#[test]
fn the_world_opens_on_the_map_with_every_models_dots_and_the_summary_under_it() {
    screen("open_plain", &draw(&label_map(), &[], true));
}

#[test]
fn right_compares_a_model_with_the_rest_on_the_land() {
    let keys = [KeyCode::Right, KeyCode::Right];
    screen("open_compared", &draw(&label_map(), &keys, true));
}

#[test]
fn down_lights_lunas_first_behavior() {
    let keys = [KeyCode::Right, KeyCode::Right, KeyCode::Down];
    screen("lit_luna", &draw(&label_map(), &keys, true));
}

#[test]
fn down_lights_haikus_first_behavior() {
    let keys = [KeyCode::Right, KeyCode::Down];
    screen("lit_haiku", &draw(&label_map(), &keys, true));
}

#[test]
fn the_first_visit_offers_summaries() {
    let mut map = label_map();
    map.offer_summaries = true;
    screen(
        "first_visit",
        &draw(&map, &[KeyCode::Right, KeyCode::Right], true),
    );
}

#[test]
fn summaries_light_haikus_first_summary() {
    let keys = [KeyCode::Right, KeyCode::Down];
    screen("deep_lit_haiku", &draw(&deep_map(), &keys, true));
}

#[test]
fn enter_on_a_summarized_behavior_rings_its_actions() {
    let mut map = deep_map();
    let actions = [
        (
            "Read external task's implementation",
            "build:per-api-key",
            "run-20261017-071701-3f92",
            "read git history",
        ),
        (
            "Outline task implementation plan",
            "plan:due",
            "run-20261014-055153-0a1b",
            "returned its structured answer",
        ),
        (
            "Outline implementation phases and details",
            "plan:orchestrator",
            "run-20261017-070940-0a1b",
            "returned its structured answer",
        ),
        (
            "Generate task metadata and phase description",
            "plan:council",
            "run-20261020-122145-0a1b",
            "returned its structured answer",
        ),
        (
            "Document task name and description",
            "plan:orchestrator",
            "run-20261014-055235-0a1b",
            "returned its structured answer",
        ),
        (
            "Outline project plan and build phases for task management",
            "plan:orchestrator",
            "run-20261017-070202-0a1b",
            "returned its structured answer",
        ),
        (
            "Outline the tests",
            "plan:due",
            "run-20261014-055300-0a1b",
            "returned its structured answer",
        ),
        (
            "Outline the review",
            "plan:due",
            "run-20261014-055400-0a1b",
            "returned its structured answer",
        ),
    ];
    let haiku: Vec<usize> = map
        .in_group(0)
        .map(|(at, _)| at)
        .take(actions.len())
        .collect();
    for (at, (summary, agent, run, label)) in haiku.iter().zip(actions) {
        let action = &mut map.actions[*at];
        action.summary = Some(summary.to_owned());
        action.behavior = summary.to_owned();
        action.agent = agent.to_owned();
        action.run = Some(run.to_owned());
        action.label = label.to_owned();
    }
    let row = &mut map.comparisons[0].rows[0];
    row.behavior = actions[0].0.to_owned();
    row.lit = haiku;
    let keys = [KeyCode::Right, KeyCode::Down, KeyCode::Enter];
    screen("deep_examples", &draw(&map, &keys, true));
}

#[test]
fn enter_lists_lunas_git_history_reads_and_opens_the_seconds_card() {
    let keys = [
        KeyCode::Right,
        KeyCode::Right,
        KeyCode::Down,
        KeyCode::Enter,
        KeyCode::Down,
    ];
    screen("examples", &draw(&examples_map(false), &keys, true));
    let mut keys = keys.to_vec();
    keys.push(KeyCode::Enter);
    screen("examples_card", &draw(&examples_map(true), &keys, true));
}

/// Without pictures the map is today's braille, with the legend and its counts beside it, whole
/// names, and the summary under it in the same words as the world.
#[test]
fn without_pictures_the_map_is_braille_with_its_legend_and_the_summary() {
    screen("braille", &draw(&braille_map(), &[], false));
}

/// Comparing Luna in braille: red and blue behind the dots, the legend saying so with the two
/// fills, and the pointer resting on one of Luna's dots, which the line under the map names.
#[test]
fn in_braille_the_comparison_shades_behind_the_dots_and_the_pointer_names_a_dot() {
    let mut map = braille_map();
    let luna: Vec<usize> = map.in_group(1).map(|(at, _)| at).collect();
    let near = &mut map.actions[luna[0]];
    near.summary = Some("Reviewed git history and previous implementations".to_owned());
    near.behavior = "Reviewed git history and previous implementations".to_owned();
    near.label = "read git history".to_owned();
    near.agent = "tie-breaker-2".to_owned();
    let pointer = (near.x + 0.001, near.y - 0.001);
    let (_, buf) = drawn(
        &map,
        &[KeyCode::Right, KeyCode::Right],
        Some(pointer),
        false,
    );
    screen("braille_compared", &buf);
}

/// In the world the pointer's line says what a dot is in the world's words: its behavior, whose
/// it is with that model's count and everyone's, and when.
#[test]
fn the_mouse_on_a_dot_says_what_it_is_in_the_worlds_words() {
    let map = label_map();
    let luna: Vec<usize> = map.in_group(1).map(|(at, _)| at).collect();
    let near = &map.actions[luna[0]];
    let pointer = (near.x + 0.001, near.y - 0.001);
    let (_, buf) = drawn(&map, &[], Some(pointer), true);
    screen("mouse", &buf);
}

/// The cells the panel leaves for the picture, under the row of levels, and the braille map's,
/// at the top of the body; the picture's key changes as a behavior lights, and only then.
#[test]
fn the_map_says_where_its_cells_are_for_the_picture_and_the_mouse() {
    let map = label_map();
    let (view, _) = drawn(&map, &[], None, true);
    let cells = view.map_cells().expect("the map shows");
    let area = |top: u16| {
        Rect::new(
            (1 + MAP_INSET) as u16,
            top,
            DRAWN_COLUMNS as u16,
            DRAWN_ROWS as u16,
        )
    };
    assert_eq!(cells.area, area(BODY + 2));
    let (again, _) = drawn(&map, &[], None, true);
    assert_eq!(again.map_cells(), Some(cells));
    let (lit, _) = drawn(&map, &[KeyCode::Right, KeyCode::Down], None, true);
    let lit = lit.map_cells().expect("the map shows");
    assert_eq!(lit.area, area(BODY + 2));
    assert_ne!(lit.picture, cells.picture);
    assert!(lit.picture.is_some());
    let (braille, _) = drawn(&map, &[], None, false);
    assert_eq!(
        braille.map_cells(),
        Some(MapCells {
            area: area(BODY),
            picture: None
        })
    );
    // A card has the map put away.
    let keys = [
        KeyCode::Right,
        KeyCode::Down,
        KeyCode::Enter,
        KeyCode::Enter,
    ];
    let (card, _) = drawn(&map, &keys, None, true);
    assert_eq!(card.map_cells(), None);
}

#[test]
fn while_summaries_are_made_the_tab_says_how_far_above_the_map() {
    let mut state = state();
    state.deep.running = Some(argonauts_core::behaviors::deep_map::DeepProgress {
        done: 312,
        total: 562,
        seconds_left: Some(50),
        summarizer: "Qwen3 Coder 30B on Ollama".to_owned(),
        short: "Qwen3 Coder 30B".to_owned(),
        redo: false,
        downloading: false,
    });
    let keys = [KeyCode::Right, KeyCode::Right];
    let (_, buf) = drawn_sized(&state, &label_map(), &keys, None, (WIDTH, HEIGHT), true);
    screen("deep_running", &buf);
}

#[test]
fn d_on_the_first_visit_asks_in_the_tab_and_starts_on_the_answers() {
    use argonauts_core::app::{UiCommand, ViewRequest};

    let mut map = label_map();
    map.offer_summaries = true;
    let state = state();
    let theme = pictured_theme();
    let (mut view, _) = drawn_sized(&state, &map, &[], None, (WIDTH, HEIGHT), true);
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    assert!(matches!(
        view.handle_key(KeyCode::Char('d').into(), &ctx),
        ViewOutcome::Request(ViewRequest::DeepOffer { start: None })
    ));
    let offer = crate::panels::stats::deep_map::sample::page_offer();
    assert!(view.accept(&ViewData::DeepOffer {
        offer: Box::new(offer)
    }));
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    screen("deep_questions", &buf);
    for _ in 0..2 {
        assert!(matches!(
            view.handle_key(KeyCode::Enter.into(), &ctx),
            ViewOutcome::Consumed
        ));
    }
    assert!(matches!(
        view.handle_key(KeyCode::Enter.into(), &ctx),
        ViewOutcome::Send(UiCommand::DeepStart { .. })
    ));
    // The map the update makes is the one the tab asks for next.
    let next = view.handle_key(KeyCode::Char('r').into(), &ctx);
    assert!(
        matches!(
            next,
            ViewOutcome::Request(ViewRequest::Behaviors(MapRequest {
                map: Some(argonauts_core::settings::behaviors::BehaviorMap::Summaries),
                ..
            }))
        ),
        "{next:?}"
    );
}

#[test]
fn once_summaries_are_made_their_list_marks_what_labels_only_group() {
    let mut map = deep_map();
    let rows = [
        (
            "Accept task completion",
            "returned its structured answer",
            true,
        ),
        (
            "Reject change due to negative input handling failure",
            "returned its structured answer",
            true,
        ),
        (
            "Located key code elements and references",
            "checked git status or diff and ran a shell command",
            false,
        ),
        ("Run full unit test suite", "ran the test suite", false),
        (
            "Check current repository status and file changes",
            "checked git status or diff and read git history",
            false,
        ),
        (
            "Confirm task completion",
            "returned its structured answer",
            true,
        ),
    ];
    let luna: Vec<usize> = map.in_group(1).map(|(at, _)| at).take(14).collect();
    map.comparisons[1].rows = rows
        .iter()
        .map(
            |(summary, label, new)| argonauts_core::behaviors::behavior_map::BehaviorRow {
                behavior: (*summary).to_owned(),
                label: Some((*label).to_owned()),
                mine: 14,
                others: 0,
                lit: luna.clone(),
                new: *new,
            },
        )
        .collect();
    let keys = [KeyCode::Right, KeyCode::Right, KeyCode::Down];
    screen("deep_done", &draw(&map, &keys, true));
}

/// Behaviors shows its choices top right as Stats does: the periods on the tab row, the chosen
/// one bold in the accent, and the groupings on the sub-tab row, the chosen one on the grey
/// band, filled in the accent while the keys are on them. `r` and Shift+R step the period, Tab
/// the grouping and `s` the projects, from the map and from the tabs alike; `p` asks for
/// nothing.
#[test]
fn the_period_and_the_grouping_show_top_right_and_their_keys_change_them() {
    let theme = pictured_theme();
    let mut map = label_map();
    map.request.period = OverviewPeriod::All;
    let (mut view, buf) = drawn_sized(&state(), &map, &[], None, (WIDTH, 40), true);
    assert!(
        text_at(&buf, (0, 1)).ends_with("Stats     All time  Last 7 days  Last 30 days"),
        "{:?}",
        text_at(&buf, (0, 1))
    );
    let groupings = "By model    By CLI    By role    By workflow    By project    By outcome";
    assert!(
        text_at(&buf, (0, 3)).ends_with(groupings),
        "{:?}",
        text_at(&buf, (0, 3))
    );
    let accent = theme.color(crate::base::theme::Role::Question);
    let at = |buf: &Buffer, y: u16, words: &str| {
        let row = text_at(buf, (0, y));
        let x = row.find(words).unwrap_or_else(|| panic!("{words}: {row}"));
        u16::try_from(row[..x].chars().count()).unwrap()
    };
    let chosen = &buf[(at(&buf, 1, "All time"), 1)];
    assert_eq!(chosen.fg, accent.unwrap());
    assert!(chosen.modifier.contains(ratatui::style::Modifier::BOLD));
    // The map is the groupings' content, so the grouping shown keeps the orange chip.
    let orange = theme.color(crate::base::theme::Role::AgentOrange).unwrap();
    assert_eq!(buf[(at(&buf, 3, "By model"), 3)].bg, orange);
    assert_ne!(buf[(at(&buf, 3, "By CLI"), 3)].bg, orange);

    let week = asked_map(press_key(&mut view, KeyCode::Char('r').into()));
    assert_eq!(week.period, OverviewPeriod::Week);
    let buf = redrawn(&mut view, WIDTH);
    assert_eq!(buf[(at(&buf, 1, "Last 7 days"), 1)].fg, accent.unwrap());
    assert_ne!(buf[(at(&buf, 1, "All time"), 1)].fg, accent.unwrap());
    let shift_r = KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT);
    assert_eq!(
        asked_map(press_key(&mut view, shift_r)).period,
        OverviewPeriod::All
    );
    let by_cli = asked_map(press_key(&mut view, KeyCode::Tab.into()));
    assert_eq!(
        by_cli.grouping,
        argonauts_core::behaviors::behavior_map::Grouping::Cli
    );
    let buf = redrawn(&mut view, WIDTH);
    assert_eq!(buf[(at(&buf, 3, "By CLI"), 3)].bg, orange);
    let here = asked_map(press_key(&mut view, KeyCode::Char('s').into()));
    assert_eq!(
        here.scope,
        argonauts_core::behaviors::behavior_map::Scope::ThisProject
    );
    assert!(matches!(
        press_key(&mut view, KeyCode::Char('p').into()),
        ViewOutcome::Ignored | ViewOutcome::Consumed
    ));
    let next = asked_map(press_key(&mut view, KeyCode::Tab.into()));
    assert_eq!(
        (next.period, next.scope),
        (here.period, here.scope),
        "p changed nothing"
    );

    // Up onto the groupings, the chosen one stays orange.
    press_key(&mut view, KeyCode::Up.into());
    let buf = redrawn(&mut view, WIDTH);
    let filled = &buf[(at(&buf, 3, "By role"), 3)];
    assert_eq!(filled.bg, orange, "the row with the keys fills");
    // Up to the tabs, the period still steps and the key line starts with Behaviors' keys.
    for _ in 0..3 {
        press_key(&mut view, KeyCode::Up.into());
    }
    let buf = redrawn(&mut view, WIDTH);
    let keys = text_at(&buf, (0, 39));
    assert!(
        keys.trim_start()
            .starts_with("r to cycle dates · s all projects · d summaries · tab · ←  →  tabs"),
        "{keys:?}"
    );
    assert_eq!(
        asked_map(press_key(&mut view, KeyCode::Char('r').into())).period,
        OverviewPeriod::Week
    );
}

/// Where the row has no room for every grouping, the chosen one shows between its neighbours,
/// and where not even that fits, alone; the tab row keeps the chosen period alone.
#[test]
fn on_a_narrow_screen_the_choices_keep_the_chosen_one() {
    let mut map = label_map();
    map.request.period = OverviewPeriod::All;
    let (mut view, buf) = drawn_sized(&state(), &map, &[], None, (100, 40), true);
    assert!(
        text_at(&buf, (0, 3)).ends_with("By outcome    By model    By CLI"),
        "{:?}",
        text_at(&buf, (0, 3))
    );
    let tabs = text_at(&buf, (0, 1));
    assert!(
        tabs.ends_with(" All time") && !tabs.contains("Last 7 days"),
        "{tabs:?}"
    );
    let buf = redrawn(&mut view, 60);
    let sub_tabs = text_at(&buf, (0, 3));
    assert!(
        sub_tabs.ends_with(" By model") && !sub_tabs.contains("By CLI"),
        "{sub_tabs:?}"
    );
}

/// In the whole panel one ↓ moves the cursor one row: the list scrolls by the rows it draws,
/// says how many it passed and how many are left, and always draws the cursor, down to the last
/// row, where ↓ stays.
/// At 120 × 38 and 177 × 40, docked, a frame after every key: each ↓ moves the cursor one row
/// down the list and past its end it stays on the last; each ↑ moves it back up a row and off
/// the top onto the map. The list always says what it hides.
#[test]
fn at_the_real_sizes_each_key_moves_the_cursor_one_row_in_a_fresh_frame() {
    let names: Vec<String> = (0..12).map(|n| format!("behavior number {n}")).collect();
    let mut sample = groups(MapRequest {
        period: OverviewPeriod::Month,
        ..MapRequest::default()
    });
    for (at, name) in names.iter().enumerate() {
        let place = (0.1 + 0.06 * at as f32, 0.5);
        sample = sample
            .actions(0, name, 3 + at, place)
            .actions(1, name, 1, place);
    }
    let rows: Vec<(&str, usize, usize)> = names
        .iter()
        .enumerate()
        .map(|(at, name)| (name.as_str(), 3 + at, 1))
        .collect();
    let map = sample.rows(0, &rows).map;
    let last = names.len() - 1;
    for size in [(120, 38), (177, 40)] {
        let state = state();
        let mut docked = Docked::new(&state, &map, size);
        docked.press(KeyCode::Right);
        let mut keys = vec![KeyCode::Down; names.len() + 1];
        keys.extend(vec![KeyCode::Up; names.len()]);
        let mut at: Option<usize> = None;
        for key in keys {
            at = match (key, at) {
                (KeyCode::Down, None) => Some(0),
                (KeyCode::Down, Some(row)) => Some((row + 1).min(last)),
                (_, Some(0) | None) => None,
                (_, Some(row)) => Some(row - 1),
            };
            let text = docked.press(key);
            let cursor: Vec<&String> = text.iter().filter(|row| row.contains(" ❯ ")).collect();
            let marks = text
                .iter()
                .filter(|row| row.contains(" more above") || row.contains(" more below"))
                .count();
            match at {
                Some(row) => {
                    assert_eq!(cursor.len(), 1, "{size:?} on row {row}: {text:#?}");
                    assert!(
                        cursor[0].contains(&format!("❯ Behavior number {row} ")),
                        "{size:?} on row {row}: {text:#?}"
                    );
                    assert!(marks >= 1, "{size:?} on row {row}: {text:#?}");
                }
                None => assert!(cursor.is_empty(), "{size:?} back on the map: {text:#?}"),
            }
        }
    }
}

/// The panel docked in a window, drawn afresh after every key, as the app draws it.
struct Docked<'a> {
    state: &'a AppState,
    view: OverviewView,
    theme: crate::base::theme::Theme,
    size: (u16, u16),
}

impl<'a> Docked<'a> {
    fn new(state: &'a AppState, map: &BehaviorMap, size: (u16, u16)) -> Docked<'a> {
        let cell = super::ASSUMED_CELL;
        let (mut view, theme) = opened_view(state, map, &[], None, true, cell);
        view.place_pictures(Some(cell));
        Docked {
            state,
            view,
            theme,
            size,
        }
    }

    /// `key` pressed, then the frame it draws, a row a line.
    fn press(&mut self, key: KeyCode) -> Vec<String> {
        let ctx = ViewCtx {
            theme: &self.theme,
            state: self.state,
            now: Instant::now(),
        };
        self.view.handle_key(key.into(), &ctx);
        let rows = self
            .view
            .dock_height(ratatui::layout::Size::new(self.size.0, self.size.1), &ctx)
            .expect("the panel docks");
        let area = Rect::new(0, 0, self.size.0, rows);
        let mut buf = Buffer::empty(area);
        self.view.render(area, &mut buf, &ctx);
        text_rows(&buf)
    }
}

fn text_rows(buf: &Buffer) -> Vec<String> {
    (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
        .collect()
}

#[test]
fn each_down_moves_the_cursor_one_row_and_the_list_keeps_it_drawn() {
    let names: Vec<String> = (0..12).map(|n| format!("behavior number {n}")).collect();
    let request = MapRequest {
        period: OverviewPeriod::Month,
        ..MapRequest::default()
    };
    let mut sample = groups(request);
    for (at, name) in names.iter().enumerate() {
        let place = (0.1 + 0.06 * at as f32, 0.5);
        sample = sample
            .actions(0, name, 3 + at, place)
            .actions(1, name, 1, place);
    }
    let rows: Vec<(&str, usize, usize)> = names
        .iter()
        .enumerate()
        .map(|(at, name)| (name.as_str(), 3 + at, 1))
        .collect();
    let map = sample.rows(0, &rows).map;
    for down in 1..=names.len() + 1 {
        let mut keys = vec![KeyCode::Right];
        keys.extend(std::iter::repeat_n(KeyCode::Down, down));
        let (_, buf) = drawn(&map, &keys, None, true);
        let text: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        let cursor: Vec<&String> = text.iter().filter(|row| row.contains(" ❯ ")).collect();
        let at = (down - 1).min(names.len() - 1);
        assert_eq!(cursor.len(), 1, "{down} down: {text:#?}");
        assert!(
            cursor[0].contains(&format!("❯ Behavior number {at} ")),
            "{down} down: {text:#?}"
        );
        let marks = text
            .iter()
            .filter(|row| row.contains(" more above") || row.contains(" more below"))
            .filter(|row| row.trim_start().starts_with('↑') || row.trim_start().starts_with('↓'))
            .count();
        assert!(marks >= 1, "the list says it runs on: {text:#?}");
    }
}

/// Beside the map every model is listed, in a list that scrolls to keep the compared one in
/// view and says how many it hides above and below.
#[test]
fn beside_the_map_every_model_scrolls_with_the_compared_one_in_view() {
    let keys = [KeyCode::Right; 9];
    let (_, buf) = drawn(&ten_models_map(), &keys, None, true);
    screen("legend_scrolled", &buf);
    let (_, buf) = drawn(&ten_models_map(), &[], None, true);
    screen("legend_more", &buf);
}

/// 2 goes out to the planet: the same groups beside it, the planet's own keys, and what
/// pointing finds there.
#[test]
fn the_planet_shows_the_same_groups_and_its_own_keys() {
    screen(
        "planet",
        &draw(&with_world(label_map()), &[KeyCode::Char('2')], true),
    );
}

/// The wheel over the map goes out to the planet as 2 does, and the panel shows the same; a
/// drag across it then turns it, and only while the map shows does the mouse reach it.
#[test]
fn the_wheel_and_a_drag_reach_the_world_through_the_panel() {
    use crate::frame::view::map::MapPoint;

    let map = with_world(label_map());
    let (mut view, _) = drawn(&map, &[], None, true);
    let corner = MapPoint {
        x: 2.0,
        y: 2.0,
        width: 1152.0,
        height: 546.0,
    };
    assert!(MapView::wheel(&mut view, corner, false, Instant::now()));
    let theme = pictured_theme();
    let state = state();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut wheeled = Buffer::empty(area);
    view.render(area, &mut wheeled, &ctx);
    let by_key = draw(&map, &[KeyCode::Char('2')], true);
    assert!(wheeled == by_key, "the wheel shows the planet as 2 does");
    MapView::point(&mut view, Some(MapPoint { x: 500.0, ..corner }));
    assert!(MapView::drag(&mut view, MapPoint { x: 620.0, ..corner }));

    let (mut card, _) = drawn(
        &map,
        &[
            KeyCode::Right,
            KeyCode::Down,
            KeyCode::Enter,
            KeyCode::Enter,
        ],
        None,
        true,
    );
    assert!(
        !MapView::wheel(&mut card, corner, false, Instant::now()),
        "a card has the map put away"
    );
}

/// ? lists every key the map answers under the pointer's line, in place of the summary, `m`
/// among them, in three columns as the prompt's own ? lists its keys.
#[test]
fn question_mark_lists_every_key_of_the_map() {
    screen(
        "shortcuts",
        &draw(&label_map(), &[KeyCode::Char('?')], true),
    );
}

/// In the universe ? lists the universe's own keys; in braille the list sits under its line too.
#[test]
fn question_mark_lists_the_keys_of_the_level_shown() {
    let keys = [KeyCode::Char('3'), KeyCode::Char('?')];
    screen(
        "shortcuts_universe",
        &draw(&with_world(label_map()), &keys, true),
    );
    screen(
        "shortcuts_braille",
        &draw(&braille_map(), &[KeyCode::Char('?')], false),
    );
}

/// 3 goes out to the universe: every maker beside it with its logo and count, → picks the
/// biggest galaxy and the line under the picture says what it is.
#[test]
fn the_universe_lists_every_maker_and_says_what_the_arrows_pick() {
    let keys = [KeyCode::Char('3'), KeyCode::Right];
    screen("universe", &draw(&with_world(label_map()), &keys, true));
}

/// Enter goes into the galaxy picked: its models beside it, the path above the picture, and →
/// picks a star.
#[test]
fn a_galaxy_lists_its_models_and_the_path_above_names_it() {
    let keys = [
        KeyCode::Char('3'),
        KeyCode::Right,
        KeyCode::Enter,
        KeyCode::Right,
    ];
    screen("galaxy", &draw(&with_world(label_map()), &keys, true));
}

/// Enter on a star shows its system: its busiest behaviors as planets beside it, the rest as a
/// belt, and a planet picked says its climate and its turn.
#[test]
fn a_system_lists_its_planets_and_a_picked_planet_says_its_climate() {
    let keys = [
        KeyCode::Char('3'),
        KeyCode::Right,
        KeyCode::Enter,
        KeyCode::Right,
        KeyCode::Enter,
        KeyCode::Right,
        KeyCode::Right,
    ];
    screen("system", &draw(&with_world(label_map()), &keys, true));
}
/// Each model's name, CLI, id and colour, by its group.
const MODELS: [(&str, &str, &str, &str); 5] = [
    ("Haiku 5.5", "claude-code", "claude-haiku-5-5", "#f7a07f"),
    ("GPT 6 Luna", "codex", "gpt-6-luna", "#6a9ee6"),
    ("GPT 5.6 Luna", "codex", "gpt-5.6-luna", "#8a8fa8"),
    ("Haiku 4.5", "claude-code", "claude-haiku-4-5", "#f7a07f"),
    (
        "Gemini 3.8 Flash",
        "antigravity-cli",
        "gemini-3.8-flash",
        "#6cd1cf",
    ),
];
const SIZES: [usize; 5] = [406, 66, 49, 22, 19];

/// GPT 6 Luna's behaviors and Haiku 5.5's, with each one's count and the others'.
const LUNA: [(&str, usize, usize); 5] = [
    ("checked git status or diff and read git history", 4, 2),
    ("checked git status or diff and ran a shell command", 3, 4),
    ("ran the test suite", 8, 19),
    ("returned its structured answer", 34, 135),
    ("edited a source file", 3, 23),
];
const HAIKU: [(&str, usize, usize); 6] = [
    ("wrote a workflow script", 13, 0),
    ("gave a structured answer that didn't fit", 20, 1),
    ("asked another agent to review or judge", 8, 0),
    ("edited a test file", 13, 1),
    ("searched code by pattern", 25, 3),
    ("read a source file", 53, 9),
];

fn models() -> argonauts_core::app::overview::OverviewData {
    let mut data = crate::panels::home::sample::overview();
    data.period = OverviewPeriod::All;
    // The places among each CLI's models that give each model its shade.
    let tokens = [
        ("claude-code", "claude-haiku-5-5", 900),
        ("claude-code", "claude-opus-5-5", 800),
        ("claude-code", "claude-haiku-4-5", 700),
        ("codex", "gpt-6-luna", 900),
        ("codex", "gpt-6", 800),
        ("codex", "gpt-5.6", 700),
        ("codex", "gpt-5.6-luna", 600),
        ("antigravity-cli", "gemini-3.8-flash", 900),
    ];
    data.all_time_models = tokens
        .iter()
        .map(|(agent, model, tokens)| ModelTokens {
            agent: (*agent).to_owned(),
            model: Some((*model).to_owned()),
            tokens: *tokens,
        })
        .collect();
    data
}

fn groups(request: MapRequest) -> Sample {
    MODELS
        .iter()
        .fold(Sample::new(request), |sample, (name, agent, model, _)| {
            sample.model(name, agent, model)
        })
}

fn state() -> AppState {
    let mut state = AppState::default();
    state.names = crate::testing::sample::names();
    state
}

/// Haiku's summarized behaviors in the model's words, each with its label, two marked new.
fn deep_map() -> BehaviorMap {
    let mut map = label_map();
    map.placing = Placing::Summaries {
        by: "Qwen3 Coder 30B".to_owned(),
    };
    let rows = [
        (
            "Plan rate limiting changes",
            "returned its structured answer",
            true,
        ),
        (
            "Initiate due date feature development workflow",
            "wrote a workflow script",
            false,
        ),
        (
            "Fix token clamping issue",
            "returned its structured answer",
            true,
        ),
        (
            "Run tests and verify parse behavior",
            "ran the test suite and ran a Python script",
            false,
        ),
        (
            "Read bucket implementation file",
            "read a source file",
            false,
        ),
        (
            "Write and modify token bucket implementation",
            "ran the test suite",
            false,
        ),
    ];
    let haiku: Vec<usize> = map.in_group(0).map(|(at, _)| at).take(14).collect();
    map.comparisons[0].rows = rows
        .iter()
        .map(
            |(summary, label, new)| argonauts_core::behaviors::behavior_map::BehaviorRow {
                behavior: (*summary).to_owned(),
                label: Some((*label).to_owned()),
                mine: 14,
                others: 0,
                lit: haiku.clone(),
                new: *new,
            },
        )
        .collect();
    map
}

/// GPT 6 Luna's nine git history reads, newest first, and the other models' nine.
fn examples_map(card: bool) -> BehaviorMap {
    let commands = [
        (
            "git status --porcelain=v1 && git show HEAD:ratelimit/bucket.py",
            "run-20261020-123036-4e50",
        ),
        (
            "git show --stat --oneline ef3f187 && git show ef3f187 -- ratelimit/bucket.py",
            "run-20261020-123036-4e50",
        ),
        (
            "\"git status --short && git diff -- ratelimit/bucket.py ratelimit/__init__.py\"",
            "run-20261020-123036-4e50",
        ),
        (
            "\"git status --short && git log -1 --oneline && sed -n '1,240p' ratelimit/bucket.py\"",
            "run-20261020-123036-4e50",
        ),
        (
            "tmpdir=$(mktemp -d) && cp -R ratelimit tests \"$tmpdir\"/ && git -C \"$tmpdir\" init",
            "run-20261017-071701-3f92",
        ),
    ];
    let mut sample = groups(MapRequest::default())
        .actions(1, "read git history", 9, (0.6, 0.8))
        .actions(0, "read git history", 9, (0.6, 0.8));
    for (group, size) in SIZES.iter().enumerate() {
        let short = size - sample.map.groups[group].actions;
        sample = sample.actions(group, "listed a folder", short, (0.9, 0.9));
    }
    let mut map = sample.rows(1, &[("read git history", 9, 9)]).map;
    let luna: Vec<usize> = map.in_group(1).map(|(at, _)| at).take(9).collect();
    for (n, at) in luna.iter().enumerate() {
        let action = &mut map.actions[*at];
        action.at = datetime!(2026-10-20 12:40 UTC) - time::Duration::minutes(n as i64 * 4);
        if let Some((command, run)) = commands.get(n) {
            action.input = (*command).to_owned();
            action.run = Some((*run).to_owned());
        }
    }
    let second = &mut map.actions[luna[1]];
    second.at = datetime!(2026-10-20 12:32 UTC) + time::Duration::minutes(4);
    if card {
        second.input = "git show --stat --oneline ef3f187 && git show ef3f187".to_owned();
        second.run_shows = Some(
            "ef3f187 is cap-bucket-refills' commit. This task started without it, so it read the change \
             from history and copied it in, and review sent the copy back for its scope."
                .to_owned(),
        );
        second.at = datetime!(2026-10-20 12:32 UTC);
    }
    map
}

/// Ten models with a few actions each, more than the legend's rows beside the map hold.
fn ten_models_map() -> BehaviorMap {
    let mut sample = Sample::new(MapRequest::default());
    for (name, agent, model, _) in MODELS {
        sample = sample.model(name, agent, model);
    }
    for (name, agent, model) in [
        ("Sonnet 5", "claude-code", "claude-sonnet-5"),
        ("Opus 5.5", "claude-code", "claude-opus-5-5"),
        ("GPT 6", "codex", "gpt-6"),
        ("Qwen3 Coder 30B", "opencode", "ollama/qwen3-coder:30b"),
        ("Gemma 3 12B", "opencode", "ollama/gemma3:12b"),
    ] {
        sample = sample.model(name, agent, model);
    }
    for group in 0..10 {
        let place = (0.1 + 0.08 * group as f32, 0.2 + 0.06 * group as f32);
        sample = sample.actions(group, "read a source file", 12 - group, place);
    }
    sample.map
}

/// The panel as it is now, `width` wide and 40 rows tall.
fn redrawn(view: &mut OverviewView, width: u16) -> Buffer {
    let theme = pictured_theme();
    let state = state();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let area = Rect::new(0, 0, width, 40);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    buf
}

fn press_key(
    view: &mut OverviewView,
    key: crossterm::event::KeyEvent,
) -> crate::frame::view::ViewOutcome {
    let theme = pictured_theme();
    let state = state();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    view.handle_key(key, &ctx)
}

/// The request a key asked for, which must be a new map.
fn asked_map(outcome: crate::frame::view::ViewOutcome) -> MapRequest {
    match outcome {
        crate::frame::view::ViewOutcome::Request(argonauts_core::app::ViewRequest::Behaviors(
            request,
        )) => request,
        other => panic!("{other:?}"),
    }
}

fn text_at(buf: &Buffer, (x, y): (u16, u16)) -> String {
    (x..buf.area.width)
        .map(|x| buf[(x, y)].symbol().to_owned())
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// Without pictures the planet is braille too: its rim, and the dots on its near side, with
/// the same legend and summary as the map's.
#[test]
fn without_pictures_the_planet_is_braille_with_its_rim_and_near_side() {
    screen(
        "braille_planet",
        &draw(&with_world(braille_map()), &[KeyCode::Char('2')], false),
    );
}

/// Without pictures the universe is braille knots round your planet, its makers beside it by
/// their whole names, and a galaxy and a system the same way.
#[test]
fn without_pictures_the_universe_a_galaxy_and_a_system_are_braille() {
    let map = with_world(braille_map());
    let universe = [KeyCode::Char('3'), KeyCode::Right];
    screen("braille_universe", &draw(&map, &universe, false));
    let galaxy = [
        KeyCode::Char('3'),
        KeyCode::Right,
        KeyCode::Enter,
        KeyCode::Right,
    ];
    screen("braille_galaxy", &draw(&map, &galaxy, false));
    let system = [
        KeyCode::Char('3'),
        KeyCode::Right,
        KeyCode::Enter,
        KeyCode::Right,
        KeyCode::Enter,
        KeyCode::Right,
        KeyCode::Right,
    ];
    screen("braille_system", &draw(&map, &system, false));
}

/// Crew R's map with the pointer on its first hurricane: the line under the picture says what
/// made it in the crew's own numbers, and the summary lists only behaviors, as the terminal
/// draws them.
#[test]
fn the_pointer_on_a_hurricane_says_what_made_it() {
    use crate::world::map::fields::crews::Key;
    use crate::world::map::nature::Kind;

    let mut map = super::crews::map(Key::R);
    map.request.period = OverviewPeriod::Month;
    let state = state();
    let (mut view, _) = drawn_sized(&state, &map, &[], None, (WIDTH, HEIGHT), true);
    let theme = pictured_theme();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let (grown, storm) = point_at_the_hurricane(&mut view, &map, (18, 42), &ctx);
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);

    // The line opens with the storm's own picture, over the two cells it leaves blank, wound
    // as the storm on the map winds.
    let beside = view.beside().expect("the line's picture");
    let text = |row: u16| {
        (0..WIDTH)
            .map(|x| buf[(x, row)].symbol())
            .collect::<String>()
    };
    let line = (0..HEIGHT)
        .find(|row| text(*row).trim_start().starts_with("Hurricane · "))
        .expect("the hurricane's line");
    assert_eq!(beside.area, Rect::new(2, line, 2, 1));
    let south = matches!(&grown.nature.features[storm].kind, Kind::Hurricane(h) if h.south);
    let icon = crate::world::map::weather::icon::icon("hurricane", south).expect("its picture");
    assert_eq!(beside.picture.image.pixels(), icon.to_image().pixels());
    if let Some(dir) = std::env::var_os("ARGONAUTS_PICTURES_OUT") {
        let file = std::path::Path::new(&dir).join("hurricane-line.png");
        std::fs::write(&file, beside.picture.image.to_png()).expect("the line's picture kept");
    }
}

/// The mouse resting on the hurricane of crew R's `map`, which `view` shows at `cell` pixels a
/// cell: the world `map` grows and which of its features the storm is.
fn point_at_the_hurricane(
    view: &mut OverviewView,
    map: &BehaviorMap,
    cell: (u16, u16),
    ctx: &ViewCtx<'_>,
) -> (super::drawn::Grown, usize) {
    use crate::world::map::nature::Kind;
    use crate::world::{Pointed, Shape};

    let scene = view.scene(cell, ctx).expect("crew R's map");
    let grown = super::drawn::Grown::new(map, super::drawn::SEED);
    let storm = grown
        .nature
        .features
        .iter()
        .position(|feature| matches!(feature.kind, Kind::Hurricane(_)))
        .expect("a hurricane");
    let (x, y) = scene
        .hits
        .marks()
        .iter()
        .find_map(|mark| match (mark.pointed, &mark.shape) {
            (Pointed::Feature(at), Shape::Point { x, y }) if at as usize == storm => Some((*x, *y)),
            _ => None,
        })
        .expect("the hurricane answers the pointer");
    let at = crate::frame::view::map::MapPoint {
        x,
        y,
        width: scene.width() as f32,
        height: scene.height() as f32,
    };
    assert!(view.point(Some(at)));
    (grown, storm)
}

/// Crew S's map with the pointer on its earthquake: the line names the CLI that crashed after
/// that CLI's logo, says how long ago, and goes on in the row under it, under its words, where
/// one row can't hold it.
#[test]
fn the_pointer_on_an_earthquake_names_the_cli_that_crashed_after_its_logo() {
    use crate::world::map::fields::crews::Key;
    use crate::world::map::nature::Kind;
    use crate::world::{Pointed, Shape};

    let mut map = super::crews::map(Key::S);
    map.request.period = OverviewPeriod::Month;
    let state = state();
    let (mut view, _) = drawn_sized(&state, &map, &[], None, (WIDTH, HEIGHT), true);
    let theme = pictured_theme();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let scene = view.scene((18, 42), &ctx).expect("crew S's map");
    let grown = super::drawn::Grown::new(&map, super::drawn::SEED);
    let quake = grown
        .nature
        .features
        .iter()
        .position(|feature| matches!(&feature.kind, Kind::Rift(rift) if rift.shaking()))
        .expect("an earthquake");
    let (x, y, rx, ry) = scene
        .hits
        .marks()
        .iter()
        .find_map(|mark| match (mark.pointed, &mark.shape) {
            (Pointed::Feature(at), Shape::Ellipse { x, y, rx, ry, .. }) if at as usize == quake => {
                Some((*x, *y, *rx, *ry))
            }
            _ => None,
        })
        .expect("the earthquake's mark");
    // Its rings lie clear of dots, but a dot near them answers first: point where none does.
    let pointed = Some(Pointed::Feature(quake as u32));
    let (x, y) = (0..=20)
        .flat_map(|step| (0..12).map(move |turn| (f64::from(step) / 20.0, f64::from(turn) / 12.0)))
        .map(|(out, turn)| {
            let angle = turn * std::f64::consts::TAU;
            let x = x + (out * f64::from(rx) * angle.cos()) as f32;
            let y = y + (out * f64::from(ry) * angle.sin()) as f32;
            (x, y)
        })
        .find(|&(x, y)| scene.hits.find(x, y) == pointed)
        .expect("the earthquake answers the pointer");
    let at = crate::frame::view::map::MapPoint {
        x,
        y,
        width: scene.width() as f32,
        height: scene.height() as f32,
    };
    assert!(view.point(Some(at)));
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    screen("earthquake", &buf);

    // Claude Code's logo, drawn as its picture, stands before its name, and the hours since
    // the crash follow; the golden's colours say whose picture it is.
    let shown = crate::base::kit::logo::golden::with_stand_ins(&buf);
    let text = |row: u16| {
        (0..WIDTH)
            .map(|x| shown[(x, row)].symbol())
            .collect::<String>()
    };
    let line = (0..HEIGHT)
        .find(|row| text(*row).trim_start().starts_with("Earthquake · "))
        .expect("the earthquake's line");
    let words = text(line);
    assert!(
        words.contains("Earthquake · ◧◨ Claude Code crashed 12 hours ago"),
        "{words}"
    );
    // The rest goes on under its words, the line broken at its last blank that fits.
    assert!(text(line).trim_end().ends_with("judge’ · 2"));
    assert!(
        text(line + 1)
            .trim_end()
            .ends_with("     sessions ended here because the CLI crashed")
    );
}

/// Where the panel's rows and columns are in a buffer: the row holding `text`, and its column.
fn found(buf: &Buffer, text: &str) -> Option<(u16, u16)> {
    (0..buf.area.height).find_map(|row| {
        let line: String = (0..buf.area.width)
            .map(|x| buf[(x, row)].symbol())
            .collect();
        let at = line.find(text)?;
        Some((line[..at].chars().count() as u16, row))
    })
}

/// At 177 × 40 and 120 × 38 on 17 × 43 px cells, the map takes the box the screens in
/// `approved/behaviors/label-map@*.txt` draw: 88 × 16 with the legend and the list beside it,
/// 64 × 12 with the list under it. Rows count from the panel's rule.
#[test]
fn at_the_real_sizes_the_map_takes_the_box_its_drawings_give_it() {
    let map = label_map();
    let cell = super::ASSUMED_CELL;

    let (view, wide) = drawn_docked(&map, (177, 40), cell);
    assert_eq!(wide.area.height, 34, "all but six rows");
    let cells = view.map_cells().expect("the map shows");
    assert_eq!(cells.area, Rect::new(2, 7, 88, 16));
    assert_eq!(found(&wide, "Every model"), Some((92, 7)));
    assert_eq!(found(&wide, "Most common"), Some((92, 21)));
    assert_eq!(found(&wide, "Label map · ").map(|at| at.1), Some(26));

    let (view, narrow) = drawn_docked(&map, (120, 38), cell);
    let cells = view.map_cells().expect("the map shows");
    assert_eq!(cells.area, Rect::new(2, 7, 64, 12));
    assert_eq!(found(&narrow, "Every model"), Some((68, 7)));
    let headline = found(&narrow, "Label map · ").expect("the headline");
    assert_eq!(headline.1, 7 + 12 + 1 + 2);
    assert_eq!(found(&narrow, "Most common"), Some((2, headline.1 + 1)));
}

/// Beside the map, the progress of Summaries takes the list's rows: the map keeps its box and
/// its picture, the legend and the headline stay drawn.
#[test]
fn beside_the_map_an_update_keeps_the_map_and_takes_the_lists_rows() {
    let mut updating = state();
    updating.deep.running = Some(argonauts_core::behaviors::deep_map::DeepProgress {
        done: 312,
        total: 562,
        seconds_left: Some(50),
        summarizer: "Qwen3 Coder 30B on Ollama".to_owned(),
        short: "Qwen3 Coder 30B".to_owned(),
        redo: false,
        downloading: false,
    });
    let cell = super::ASSUMED_CELL;
    for size in [(177, 40), (140, 40)] {
        let (still, _) = drawn_docked(&label_map(), size, cell);
        let (view, buf) = drawn_docked_in(&updating, &label_map(), &[], size, cell);
        let text: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        let box_of = |view: &OverviewView| {
            view.map_cells()
                .map(|cells| (cells.area.width, cells.area.height))
        };
        assert!(box_of(&view).is_some(), "{size:?}: {text:#?}");
        assert_eq!(
            box_of(&view),
            box_of(&still),
            "{size:?}: the map keeps its box"
        );
        assert!(found(&buf, "Every model").is_some(), "{size:?}: {text:#?}");
        assert!(found(&buf, "Label map · ").is_some(), "{size:?}: {text:#?}");
    }
}

/// Beside the map at 177 × 40 the offer of Summaries wraps in the list's column, its key
/// whole, and nothing runs to the screen's edge.
#[test]
fn beside_the_map_the_offer_wraps_in_the_lists_column() {
    let mut map = label_map();
    map.offer_summaries = true;
    let (_, buf) = drawn_docked(&map, (177, 40), super::ASSUMED_CELL);
    let text = text_rows(&buf);
    let offer = text
        .iter()
        .position(|row| row.contains("✻  Summaries: a model summarizes"))
        .expect("the offer");
    let key = text
        .iter()
        .position(|row| row.contains("d to set them up"))
        .expect("the offer's key");
    assert_eq!(key, offer + 1, "{text:#?}");
    for row in text.iter().filter(|row| !row.starts_with('─')) {
        assert!(
            row.chars().nth(176).is_none_or(|ch| ch == ' '),
            "a row runs to the edge: {row}"
        );
    }
}

/// At 80 × 24 the legend beside the small map lists the models it has room for and says how
/// many more are below.
#[test]
fn at_80_by_24_the_legend_says_how_many_models_are_below() {
    let (_, buf) = drawn_docked(&ten_models_map(), (80, 24), super::ASSUMED_CELL);
    let text = text_rows(&buf);
    assert!(
        text.iter().any(|row| row.contains("more below")),
        "{text:#?}"
    );
}

mod compared;
mod drawings;
