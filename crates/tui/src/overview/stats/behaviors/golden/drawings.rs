//! Every Behaviors drawing in `approved/behaviors/`, drawn as the app docks the panel at the
//! drawing's size, on the cell the app assumes, with vim's keys (the drawings are made with
//! them), a frame after every key, and held to its drawing cell for cell.

use std::time::Instant;

use argonauts_core::app::{ViewData, ViewRequest};
use argonauts_core::behavior_map::{
    BehaviorMap, BehaviorRow, Comparison, GRID, MapRequest, Placing,
};
use argonauts_core::overview::{ModelTokens, OverviewData, OverviewPeriod};
use argonauts_core::settings::ui::ClockStyle;
use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::style::Color;
use time::macros::datetime;

use super::super::ASSUMED_CELL;
use super::super::crews;
use super::super::sample::Sample;
use argonauts_testkit::approved::{self, Drawing, Palette, differences_at, report};

use crate::approved_screen::assert_matches;
use crate::overview::{OverviewView, Tab, UsageWatch};
use crate::stability::grid::on_screen;
use crate::state::AppState;
use crate::views::map::MapView;
use crate::views::{View, ViewCtx, ViewOutcome};
use crate::world::map::fields::crews::Key;

const SMALL: (u16, u16) = (120, 38);
const WIDE: (u16, u16) = (177, 40);

/// The panel on Behaviors, docked in a window of `size`, as the app keeps it between keys.
struct Opened {
    view: OverviewView,
    theme: crate::theme::Theme,
    state: AppState,
    size: (u16, u16),
}

impl Opened {
    /// The panel opened on Behaviors over `overview`, `map` taken for its period. With
    /// `into_map`, ↓ twice: onto the groupings, then into the map.
    fn new(overview: OverviewData, map: &BehaviorMap, size: (u16, u16), into_map: bool) -> Self {
        let mut state = crate::views::list::keys_state(true);
        state.names = crate::overview::sample::names();
        let view = OverviewView::new(Tab::Overview, UsageWatch::default())
            .at(datetime!(2026-10-20 08:40 -4))
            .with_cell(ASSUMED_CELL)
            .with_map_picture();
        let mut opened = Opened {
            view,
            theme: super::pictured_theme(),
            state,
            size,
        };
        assert!(opened.view.accept(&ViewData::Overview {
            overview: Box::new(overview)
        }));
        for code in [KeyCode::Down, KeyCode::Right, KeyCode::Right] {
            opened.press(code);
        }
        let mut period = OverviewPeriod::All;
        while period != map.request.period {
            match opened.key(KeyCode::Char('r')) {
                ViewOutcome::Request(ViewRequest::Behaviors(asked)) => period = asked.period,
                other => panic!("r asks for the next period's map, not {other:?}"),
            }
            opened.draw();
        }
        opened.view.accept(&ViewData::Behaviors {
            map: Box::new(map.clone()),
        });
        opened.draw();
        if into_map {
            opened.press(KeyCode::Down);
            opened.press(KeyCode::Down);
        }
        opened
    }

    fn key(&mut self, code: KeyCode) -> ViewOutcome {
        let ctx = ViewCtx {
            theme: &self.theme,
            state: &self.state,
            now: Instant::now(),
        };
        self.view.handle_key(code.into(), &ctx)
    }

    /// `code` pressed `times` times, a frame drawn after each.
    fn press_times(&mut self, code: KeyCode, times: usize) -> Buffer {
        let mut frame = self.draw();
        for _ in 0..times {
            frame = self.press(code);
        }
        frame
    }

    /// `code` pressed, then the frame the app draws after it.
    fn press(&mut self, code: KeyCode) -> Buffer {
        let _ = self.key(code);
        self.draw()
    }

    /// The panel's rows as the app docks it, its pictures placed.
    fn draw(&mut self) -> Buffer {
        self.view.place_pictures(Some(ASSUMED_CELL));
        let ctx = ViewCtx {
            theme: &self.theme,
            state: &self.state,
            now: Instant::now(),
        };
        let (width, height) = self.size;
        let rows = self
            .view
            .dock_height(Size::new(width, height), &ctx)
            .expect("the panel docks");
        let area = Rect::new(0, 0, width, rows);
        let mut buf = Buffer::empty(area);
        self.view.render(area, &mut buf, &ctx);
        buf
    }

    /// Panics unless the frame `buf`, docked at the bottom of the window, is the drawing
    /// `behaviors/<name>@<size>.txt`.
    fn holds(&self, name: &str, buf: &Buffer) {
        let (width, height) = self.size;
        let file = format!("behaviors/{name}@{width}x{height}.txt");
        let shown = self.with_pictures(buf);
        assert_matches(&file, &shown, self.size, height - buf.area.height);
    }

    /// [`Opened::holds`] for a drawing of the whole screen: the rows the panel draws, under
    /// the app's header, which the panel leaves to the conversation above it.
    fn holds_under_the_header(&self, name: &str, buf: &Buffer) {
        let (width, height) = self.size;
        let file = format!("behaviors/{name}@{width}x{height}.txt");
        let drawing = Drawing::load(&approved::dir().join(&file))
            .unwrap_or_else(|err| panic!("{file}: {err:#}"));
        let top = height - buf.area.height;
        let drawn = on_screen(&self.with_pictures(buf), self.size, top);
        let placement = drawing.place(&drawn);
        let above_the_panel = |row: u16, _| row < top;
        let found = differences_at(
            &drawing,
            &placement,
            &drawn,
            &Palette::TOKYONIGHT_MOON,
            &above_the_panel,
        );
        if !found.is_empty() {
            panic!("{}", report(&file, &drawing, &drawn, &placement, &found));
        }
    }

    /// `buf` as the terminal shows it once the screen puts its pictures over the cells the map
    /// left for them: the map's, and the one beside the pointer's line.
    fn with_pictures(&self, buf: &Buffer) -> Buffer {
        let mut shown = buf.clone();
        let map = self
            .view
            .map_cells()
            .filter(|cells| cells.picture.is_some())
            .map(|cells| cells.area);
        let beside = self.view.beside().map(|beside| beside.area);
        let placeholder = crate::mascot::placeholder::PLACEHOLDER.to_string();
        for (id, area) in [map, beside].into_iter().enumerate() {
            let Some(area) = area else { continue };
            let ink = Color::Rgb(0, 0, u8::try_from(id + 1).unwrap_or(u8::MAX));
            for y in area.top()..area.bottom() {
                for x in area.left()..area.right() {
                    shown[(x, y)].set_symbol(&placeholder).set_fg(ink);
                }
            }
        }
        shown
    }
}

/// A model group of a crew: its name, CLI, model id and how many actions it took.
type Group = (&'static str, &'static str, &'static str, usize);

/// A crew of `groups` with `behaviors` (name, count) dealt out among them: each behavior's
/// actions go a few at a time to the group with the most room left. `fixed` gives a group its
/// own count of a behavior first.
fn crew(groups: &[Group], fixed: &[(usize, &str, usize)], behaviors: &[(&str, usize)]) -> Sample {
    let mut sample = groups.iter().fold(
        Sample::new(MapRequest::default()),
        |s, (name, agent, model, _)| s.model(name, agent, model),
    );
    let mut left: Vec<usize> = groups.iter().map(|group| group.3).collect();
    let mut placed = 0usize;
    let mut spot = || {
        placed += 1;
        (
            0.05 + 0.9 * ((placed * 37) % 100) as f32 / 100.0,
            0.05 + 0.9 * ((placed * 61) % 100) as f32 / 100.0,
        )
    };
    for (group, behavior, count) in fixed {
        sample = sample.actions(*group, behavior, *count, spot());
        left[*group] -= count;
    }
    for (behavior, count) in behaviors {
        let mut count = *count;
        while count > 0 {
            let group = (0..left.len())
                .max_by_key(|group| (left[*group], usize::MAX - group))
                .expect("a crew has groups");
            assert!(left[group] > 0, "no room left for {behavior}");
            let take = count.min(left[group]).min(3);
            sample = sample.actions(group, behavior, take, spot());
            left[group] -= take;
            count -= take;
        }
    }
    assert!(
        left.iter().all(|left| *left == 0),
        "groups not filled: {left:?}"
    );
    sample
}

/// The models of the crew whose label map is compared, by how many actions each took, Codex's
/// sessions that recorded no model filed under its default, GPT 6.1 Sol.
const LABELLED: [Group; 11] = [
    ("Haiku 5.5", "claude-code", "claude-haiku-5-5", 467),
    ("GPT 6.1 Sol", "codex", "gpt-6.1-sol", 80),
    ("GPT 6 Luna", "codex", "gpt-6-luna", 67),
    ("Opus 5.5", "claude-code", "claude-opus-5-5", 63),
    (
        "Qwen3 Coder 30B (64k)",
        "opencode",
        "ollama/qwen3-coder:30b-64k",
        52,
    ),
    ("GPT 6 Astra", "codex", "gpt-6-astra", 36),
    ("Haiku 4.5", "claude-code", "claude-haiku-4-5", 22),
    (
        "Gemini 3.8 Flash (Low)",
        "antigravity-cli",
        "gemini-3.8-flash-low",
        19,
    ),
    ("Gemma4 E2B", "opencode", "ollama/gemma4:e2b", 8),
    ("Opus 5", "claude-code", "claude-opus-5", 6),
    (
        "Gemini 3.8 Flash (High)",
        "antigravity-cli",
        "gemini-3.8-flash-high",
        3,
    ),
];

/// The label map of 823 actions over 11 models, Opus 5.5's ten labels its list.
fn labels() -> BehaviorMap {
    let opus = 3;
    let rows: [(&str, usize, usize); 10] = [
        ("presented a plan", 3, 3),
        ("wrote a workflow script", 4, 6),
        ("read a doc", 3, 7),
        ("read git history", 4, 10),
        ("edited a source file", 8, 26),
        ("searched code by pattern", 9, 33),
        ("checked git status or diff", 5, 25),
        ("read a source file", 14, 94),
        ("listed files", 3, 35),
        ("ran the test suite", 3, 38),
    ];
    let mut fixed: Vec<(usize, &str, usize)> = rows
        .iter()
        .map(|(behavior, mine, _)| (opus, *behavior, *mine))
        .collect();
    fixed.push((opus, "returned its structured answer", 7));
    let mut behaviors: Vec<(&str, usize)> = rows
        .iter()
        .map(|(behavior, _, others)| (*behavior, *others))
        .collect();
    behaviors.push(("returned its structured answer", 183 - 7));
    behaviors.push(("read a test file", 58));
    let used = behaviors.iter().map(|b| b.1).sum::<usize>() + LABELLED[opus].3;
    let mut rest = 823 - used;
    for behavior in [
        "wrote a doc",
        "built the project",
        "changed git state",
        "listed a folder",
        "wrote a test",
        "fetched a web page",
        "ran a shell command",
        "read a config file",
        "edited a test file",
    ] {
        let take = rest.min(30);
        behaviors.push((behavior, take));
        rest -= take;
    }
    assert_eq!(rest, 0);
    let mut sample = crew(&LABELLED, &fixed, &behaviors);
    sample.map.runs = 21;
    sample.map.conversations = 18;
    sample.map.projects = (1..=12).map(|n| format!("p{n}")).collect();
    sample.map.offer_summaries = false;
    sample.rows(opus, &rows).map
}

/// The Summaries of the same crew: 650 of 823 actions summarized by its local model, Opus
/// 5.5's nine in eight summaries its list.
fn summaries() -> BehaviorMap {
    let groups: [Group; 9] = [
        ("Haiku 5.5", "claude-code", "claude-haiku-5-5", 449),
        ("GPT 6 Luna", "codex", "gpt-6-luna", 67),
        ("GPT 6.1 Sol", "codex", "gpt-6.1-sol", 49),
        ("Haiku 4.5", "claude-code", "claude-haiku-4-5", 22),
        (
            "Qwen3 Coder 30B (64k)",
            "opencode",
            "ollama/qwen3-coder:30b-64k",
            22,
        ),
        (
            "Gemini 3.8 Flash (Low)",
            "antigravity-cli",
            "gemini-3.8-flash-low",
            19,
        ),
        ("Opus 5.5", "claude-code", "claude-opus-5-5", 9),
        ("Gemma4 E2B", "opencode", "ollama/gemma4:e2b", 8),
        ("GPT 6 Astra", "codex", "gpt-6-astra", 5),
    ];
    let opus = 6;
    // Each of Opus 5.5's summaries, whether it says more than its label, and its actions.
    let own: [(&str, bool, usize); 8] = [
        ("Broad search for cache invalidation", false, 1),
        ("No change made", true, 2),
        ("Listing project files to understand structure", false, 1),
        (
            "Checking repository status and local environment setup",
            false,
            1,
        ),
        ("Responded with a greeting", true, 1),
        ("Drafting the retry handler notes", true, 1),
        ("Measuring each test suite before a merge", true, 1),
        ("Reading the token bucket config values", false, 1),
    ];
    let fixed: Vec<(usize, &str, usize)> = own
        .iter()
        .map(|(summary, _, count)| (opus, *summary, *count))
        .collect();
    let mut rest = 650 - groups[opus].3;
    let others = [
        "Plan rate limiting changes",
        "Fix token clamping issue",
        "Committed the fix",
        "Built the workspace with cargo",
        "Accepted successful test outcomes",
        "Wrote the limiter's design notes",
    ];
    let behaviors: Vec<(&str, usize)> = others
        .iter()
        .enumerate()
        .map(|(at, summary)| {
            let take = if at + 1 == others.len() {
                rest
            } else {
                rest.min(7)
            };
            rest -= take;
            (*summary, take)
        })
        .collect();
    let mut map = crew(&groups, &fixed, &behaviors).map;
    map.placing = Placing::Summaries {
        by: "Qwen3 Coder 30B (64k)".to_owned(),
    };
    map.unsummarized = 173;
    map.offer_summaries = false;
    for action in &mut map.actions {
        action.summary = Some(action.behavior.clone());
        action.label = "read a source file".to_owned();
    }
    let (mine, rest): (Vec<usize>, Vec<usize>) =
        (0..map.actions.len()).partition(|at| map.actions[*at].group == opus);
    // Each summary lights its own actions and the 13 summarized most like it.
    let rows = own
        .iter()
        .map(|(summary, new, _)| {
            let mut lit: Vec<usize> = mine
                .iter()
                .copied()
                .filter(|at| map.actions[*at].behavior == *summary)
                .collect();
            lit.extend(rest.iter().take(14 - lit.len()));
            BehaviorRow {
                behavior: (*summary).to_owned(),
                label: Some("read a source file".to_owned()),
                mine: 1,
                others: 13,
                lit,
                new: *new,
            }
        })
        .collect();
    let total = map.actions.len();
    map.comparisons[opus] = Comparison {
        overlay: Some(vec![0.0; GRID * GRID]),
        why_not: None,
        rows,
        mine: groups[opus].3,
        others: total - groups[opus].3,
    };
    map
}

#[test]
fn the_summaries_list_steps_down_and_back_up_as_drawn() {
    let mut opened = Opened::new(super::models(), &summaries(), SMALL, true);
    let compared = opened.press_times(KeyCode::Char('l'), 7);
    opened.holds("deeper-map-compared", &compared);
    for down in 1..=9 {
        let frame = opened.press(KeyCode::Char('j'));
        opened.holds(&format!("deeper-list-down-{down}"), &frame);
    }
    for up in 1..=8 {
        let frame = opened.press(KeyCode::Char('k'));
        opened.holds(&format!("deeper-list-down-9-up-{up}"), &frame);
    }
}

#[test]
fn beside_the_map_summaries_compare_a_model_as_drawn() {
    let mut opened = Opened::new(super::models(), &summaries(), WIDE, true);
    let compared = opened.press_times(KeyCode::Char('l'), 7);
    opened.holds("deeper-map-compared", &compared);
}

#[test]
fn the_label_list_steps_down_as_drawn() {
    let mut opened = Opened::new(super::models(), &labels(), SMALL, true);
    opened.press_times(KeyCode::Char('l'), 4);
    for down in 1..=11 {
        let frame = opened.press(KeyCode::Char('j'));
        if [1, 4, 5, 6, 10, 11].contains(&down) {
            opened.holds(&format!("label-list-down-{down}"), &frame);
        }
    }
}

/// Crew R's map as the label map's drawings show it, with the runs, conversations and projects
/// its headline counts.
fn crew_r() -> BehaviorMap {
    let mut map = crews::map(Key::R);
    map.runs = 41;
    map.conversations = 27;
    map.projects = (1..=6).map(|at| format!("project-{at}")).collect();
    map
}

/// The usage `map`'s crew is ranked by: each model's tokens as many as its actions, so each
/// CLI's three busiest models take its shades.
fn ranked_by(map: &BehaviorMap) -> OverviewData {
    let mut overview = super::models();
    overview.all_time_models = map
        .groups
        .iter()
        .filter_map(|group| {
            Some(ModelTokens {
                agent: group.agent.clone()?,
                model: group.model.clone(),
                tokens: u64::try_from(group.actions).ok()?,
            })
        })
        .collect();
    overview
}

#[test]
fn the_label_map_opens_with_nothing_compared_and_the_mouse_on_the_hurricane_as_drawn() {
    // The drawings write times on a 24-hour clock.
    crate::format::set_clock(ClockStyle::TwentyFourHour);
    let map = crew_r();
    for size in [SMALL, WIDE] {
        let mut opened = Opened::new(ranked_by(&map), &map, size, false);
        opened.draw();
        let ctx = ViewCtx {
            theme: &opened.theme,
            state: &opened.state,
            now: Instant::now(),
        };
        super::point_at_the_hurricane(&mut opened.view, &map, ASSUMED_CELL, &ctx);
        let frame = opened.draw();
        opened.holds_under_the_header("label-map", &frame);
    }
}

/// The crew the colours are drawn for: the label map's, and three models more, each with the
/// tokens it ran through the app, which rank each CLI's models for their shades.
fn colours_crew() -> (OverviewData, BehaviorMap) {
    const MORE: [Group; 3] = [
        ("Sonnet 5.5", "claude-code", "claude-sonnet-5-5", 30),
        ("Devstral 24B", "opencode", "ollama/devstral:24b", 9),
        ("GPT 5.6 Terra", "codex", "gpt-5.6-terra", 4),
    ];
    let groups: Vec<Group> = LABELLED.iter().chain(&MORE).copied().collect();
    let tokens = |model: &str| match model {
        "claude-haiku-5-5" => 6_500_000,
        "claude-opus-5-5" => 1_970_000,
        "claude-sonnet-5-5" => 840_000,
        "claude-haiku-4-5" => 120_000,
        "claude-opus-5" => 50_000,
        "gpt-6-luna" => 1_470_000,
        "gpt-6.1-sol" => 630_000,
        "gpt-6-astra" => 390_000,
        "gpt-5.6-terra" => 80_000,
        "ollama/qwen3-coder:30b-64k" => 1_430_000,
        "ollama/devstral:24b" => 310_000,
        "ollama/gemma4:e2b" => 90_000,
        "gemini-3.8-flash-low" => 200_000,
        _ => 60_000,
    };
    let mut overview = super::models();
    overview.all_time_models = groups
        .iter()
        .map(|(_, agent, model, _)| ModelTokens {
            agent: (*agent).to_owned(),
            model: Some((*model).to_owned()),
            tokens: tokens(model),
        })
        .collect();
    let total: usize = groups.iter().map(|group| group.3).sum();
    let behaviors = [
        ("read a source file", total / 3),
        ("edited a source file", total / 3),
        ("ran the test suite", total - 2 * (total / 3)),
    ];
    (overview, crew(&groups, &[], &behaviors).map)
}

#[test]
fn the_legend_draws_each_model_of_the_colours_crew_in_its_clis_shade() {
    let (overview, map) = colours_crew();
    let mut opened = Opened::new(overview, &map, SMALL, false);
    let frame = opened.draw();
    opened.holds("legend-sample-crew", &frame);
}
