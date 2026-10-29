//! What the world costs while it moves, on the real panel at the usual window, a wide one and,
//! for the planet and the universe, a big one.
//!
//! Most runs draw and pack pictures in line on a made-up clock, so each frame's time is the
//! whole of the app's work: on every level, the map, the planet, the universe, a galaxy and a
//! system, and with the keys going down and up the list's rows while the pointer moves, it must
//! stay under 1% of a core a second on average, for a crew like R and one like T whose clouds
//! drift apart. The usual window has the usual cell of 18 × 42 px; the wide and big ones have
//! 17 × 43 px cells, where the map takes its widest box, 88 × 16. The live runs draw and pack on
//! threads of their own, as the app does, and frames come as the screen's loop draws them: when
//! one is due and when a picture arrives. They judge the screen's thread and the bytes it sends
//! against the runs in line, count the frames, and time how long a key takes to reach the
//! screen.
//!
//! The timed runs judge the thread's own CPU on performance cores, or the wall clock, so they run
//! one at a time on an optimized build:
//! `cargo test --release -p argonauts-tui --lib -- --ignored --test-threads=1 world_cost`.

use std::time::{Duration, Instant};

use argonauts_core::app::ViewData;
use argonauts_core::behavior_map::{BehaviorMap, BehaviorRow, Comparison};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Position, Size};
use tokio::runtime::Runtime;
use tokio::sync::watch;

use super::*;
use crate::cpu_time::{Spent, Stopwatch};
use crate::model::ViewModel;
use crate::overview::stats::behaviors::{ASSUMED_CELL, crews, sample};
use crate::overview::{OverviewView, Tab, UsageWatch};
use crate::state::AppState;
use crate::terminal::InlineTerminal;
use crate::theme::{Glyphs, Theme};
use crate::world::Picture;
use crate::world::map::fields::crews::Key;

/// A window the world is timed in.
#[derive(Debug, Clone, Copy)]
struct Window {
    /// Columns and rows.
    size: (u16, u16),
    /// Pixels a cell.
    cell: (u16, u16),
    /// The map's box in cells, which the planet and the universe take too, where the window
    /// reaches the widest one.
    map: Option<(u16, u16)>,
}

/// The usual window at the usual cell.
const USUAL: Window = Window {
    size: (120, 38),
    cell: (18, 42),
    map: None,
};
/// A wide window, where the map takes its widest box with the list beside it.
const WIDE: Window = Window {
    size: (177, 40),
    cell: ASSUMED_CELL,
    map: Some((88, 16)),
};
/// A big window, where the map's box stays the widest one, with the list under it.
const BIG: Window = Window {
    size: (240, 60),
    cell: ASSUMED_CELL,
    map: Some((88, 16)),
};
/// The windows every timed run uses.
const WINDOWS: [Window; 2] = [USUAL, WIDE];
/// The planet and the universe draw as many pixels as their box has, so they are timed in the
/// big window too.
const WINDOWS_FOR_A_GLOBE: [Window; 3] = [USUAL, WIDE, BIG];
/// The keys from the map that light the list's first row: a group compared, then down into
/// its behaviors.
const LIGHT_A_ROW: [KeyCode; 2] = [KeyCode::Right, KeyCode::Down];
/// How many times a second the pointer moves while it sweeps the map: as often as the motion
/// steps.
const MOVES_A_SECOND: u32 = 8;
/// How long a run draws before it is judged: a minute to draw what the world keeps, a planet's
/// whole first turn among it.
const FIRST_TURN: u64 = 61;
/// How much more the world on threads may send a step than in line, in hundredths: the two
/// runs judge different steps, whose parts move by different amounts, about 1% apart over 10 s.
/// A picture sent again is caught on its own.
const MORE_A_STEP: u128 = 5;

/// What the hands do while a run is judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hands {
    /// Nothing moves but the world.
    Off,
    /// The pointer sweeps the map, and ↓ goes down the list a row a step, then ↑ back up, over
    /// `rows` rows.
    SweepingAndStepping { rows: usize },
}

/// The panel on Behaviors, its keys on the picture, on a full screen.
struct Panel {
    ui: Ui<Vec<u8>>,
    at: Instant,
    state: AppState,
    cell: (u16, u16),
}

impl Panel {
    /// The made-up crew `key`'s world on the panel, in `window`.
    fn open(key: Key, window: Window) -> Self {
        Self::with(key, window, |_| {})
    }

    /// As [`Panel::open`], its world drawn and its pictures packed on threads of their own, as
    /// the app does, and held still when `still` says so.
    fn on_threads(key: Key, window: Window, still: bool) -> Self {
        worker::threads_for_this_test();
        Self::with(key, window, |theme| theme.reduced_motion = still)
    }

    fn with(key: Key, window: Window, change: impl FnOnce(&mut Theme)) -> Self {
        crate::cpu_time::run_as_interactive();
        let (columns, rows) = window.size;
        let terminal =
            InlineTerminal::new(Vec::new(), Size::new(columns, rows), Position::new(0, 0));
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        change(&mut theme);
        let at = Instant::now();
        let mut ui = Ui::new(terminal, theme, ViewModel::new("conversation"), at);
        ui.use_fullscreen();
        ui.use_pictures(Some(Graphics::Kitty), Some(window.cell));
        ui.use_pixel_mouse(true);
        let mut panel = Self {
            ui,
            at,
            state: AppState::default(),
            cell: window.cell,
        };
        panel.ui.push_view(Box::new(OverviewView::new(
            Tab::Overview,
            UsageWatch::default(),
        )));
        panel.keys(&[KeyCode::Down, KeyCode::Right, KeyCode::Right]);
        // The tab takes the map it asked for, so the crew's map answers its request.
        let mut map = crews::map(key);
        map.request = sample::three_models().map.request;
        map.comparisons = (0..map.groups.len())
            .map(|group| behaviors(&map, group))
            .collect();
        assert!(
            panel
                .ui
                .offer_view_data(&ViewData::Behaviors { map: Box::new(map) })
        );
        panel.keys(&[KeyCode::Down, KeyCode::Down]);
        panel
    }

    fn keys(&mut self, codes: &[KeyCode]) {
        for code in codes {
            let key = Event::Key(KeyEvent::new(*code, KeyModifiers::NONE));
            self.ui.handle_event_in(key, self.at, &self.state);
        }
    }

    /// Moves the mouse to `(x, y)` on screen, in pixels, as Ghostty reports it.
    fn point(&mut self, (x, y): (u16, u16)) {
        let mouse = MouseEvent {
            kind: MouseEventKind::Moved,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        self.ui
            .handle_event_in(Event::Mouse(mouse), self.at, &self.state);
    }

    /// The cells the map's pictures show over, which the planet and the universe take too.
    fn map_box(&self) -> Rect {
        self.ui.map.shown.expect("the map on screen")
    }

    /// `count` places along the middle row of the map's cells, in pixels, left to right.
    fn across_the_map(&self, count: u32) -> Vec<(u16, u16)> {
        let cells = self.map_box();
        let (width, height) = (u32::from(self.cell.0), u32::from(self.cell.1));
        let left = u32::from(cells.x) * width;
        let span = u32::from(cells.width) * width;
        let y = (u32::from(cells.y) + u32::from(cells.height) / 2) * height + height / 2;
        let pixel = |at: u32| u16::try_from(at).expect("a pixel on screen");
        (0..count)
            .map(|n| (pixel(left + span * n / count), pixel(y)))
            .collect()
    }

    /// Draws a frame at the panel's clock: the bytes it sent.
    fn frame(&mut self) -> usize {
        self.ui.frame_in(self.at, &self.state).unwrap();
        let sent = self.ui.terminal().writer().len();
        self.ui.terminal_mut().writer_mut().clear();
        sent
    }

    /// Wakes the screen at the panel's clock, as its loop does: the bytes it sent, how many
    /// pictures among them, and whether it drew a frame, which it doesn't while one is due no
    /// sooner or ran too lately.
    fn wake(&mut self) -> (usize, usize, bool) {
        let before = self.ui.frames.earliest(self.at);
        self.ui.frame_in(self.at, &self.state).unwrap();
        let writer = self.ui.terminal().writer();
        let (sent, pictures) = (
            writer.len(),
            crate::mascot::graphics::pictures_in(writer).len(),
        );
        self.ui.terminal_mut().writer_mut().clear();
        (sent, pictures, self.ui.frames.earliest(self.at) != before)
    }

    /// Draws a frame a moment later on the made-up clock, past the least time between frames.
    fn next_frame(&mut self) {
        self.at += Duration::from_millis(50);
        self.frame();
    }

    /// Draws every frame the screen asks for until `end` on the made-up clock: the bytes sent.
    fn frames_until(&mut self, end: Instant) -> usize {
        let mut sent = 0;
        while let Some(next) = self.ui.next_deadline(self.at) {
            if next > end {
                break;
            }
            self.at = next.max(self.at + Duration::from_millis(1));
            sent += self.frame();
        }
        self.at = end;
        sent
    }

    /// Draws every frame the screen asks for over `seconds` of the made-up clock, with the
    /// `hands` moving: the thread's work in each second, and the bytes sent in all.
    fn run(&mut self, seconds: u64, hands: Hands) -> (Vec<Spent>, usize) {
        self.run_until(hands, |done| done.len() as u64 >= seconds)
    }

    /// As [`Panel::run`], until `seconds` of the seconds ran on performance cores.
    fn judged_run(&mut self, seconds: usize, hands: Hands) -> (Vec<Spent>, usize) {
        self.run_until(hands, |done| crate::cpu_time::measured(done, seconds))
    }

    fn run_until(&mut self, hands: Hands, done: impl Fn(&[Spent]) -> bool) -> (Vec<Spent>, usize) {
        let (places, keys) = match hands {
            Hands::Off => (Vec::new(), Vec::new()),
            Hands::SweepingAndStepping { rows } => {
                let down = vec![KeyCode::Down; rows.saturating_sub(1)];
                let up = vec![KeyCode::Up; rows.saturating_sub(1)];
                (self.across_the_map(MOVES_A_SECOND * 2), [down, up].concat())
            }
        };
        let mut places = places.iter().cycle();
        let mut keys = keys.iter().cycle();
        let mut each = Vec::new();
        let mut sent = 0;
        while !done(&each) {
            let watch = Stopwatch::start();
            if hands == Hands::Off {
                sent += self.frames_until(self.at + Duration::from_secs(1));
            } else {
                for _ in 0..MOVES_A_SECOND {
                    if let Some(place) = places.next() {
                        self.point(*place);
                    }
                    if let Some(key) = keys.next() {
                        self.keys(&[*key]);
                    }
                    sent += self.frames_until(self.at + Duration::from_secs(1) / MOVES_A_SECOND);
                }
            }
            each.push(watch.stop());
        }
        (each, sent)
    }

    /// From the list's first row lit: how many rows the list has, found by going ↓ until the
    /// glow stays the same, and then back ↑ to the first.
    fn rows_in_the_list(&mut self) -> usize {
        self.next_frame();
        let mut glows = vec![self.glow().expect("the first row's glow").key];
        loop {
            self.keys(&[KeyCode::Down]);
            self.next_frame();
            let glow = self.glow().expect("a row's glow").key;
            if glows.last() == Some(&glow) {
                break;
            }
            glows.push(glow);
        }
        let rows = glows.len();
        self.keys(&vec![KeyCode::Up; rows - 1]);
        self.next_frame();
        assert_eq!(self.glow().map(|glow| glow.key), Some(glows[0]));
        rows
    }

    /// The scene the map shows now.
    fn scene(&self) -> Option<Arc<Scene>> {
        self.ui.map.asked.as_ref()?.scene.clone()
    }

    /// The glow the list lights the map with, where the scene has one.
    fn glow(&self) -> Option<Picture> {
        let scene = self.scene()?;
        let layer = scene
            .layers()
            .iter()
            .find(|layer| layer.depth == Depth::Pointer)?;
        Some(layer.picture.clone())
    }

    /// The step of the world's motion `now` falls in, once the map has shown.
    fn step_at(&self, now: Instant) -> Option<u128> {
        let map = &self.ui.map;
        let mut steps = map.steps?;
        Some(steps.index(now, map.sources.step(now)))
    }

    /// As [`Panel::run`] with nothing moving but the world: the bytes sent, and how many of the
    /// motion's steps began meanwhile, each drawn at its start.
    fn steps(&mut self, seconds: u64) -> (usize, u128) {
        let first = self.step_at(self.at);
        let (_, sent) = self.run(seconds, Hands::Off);
        let steps = first
            .zip(self.step_at(self.at))
            .map_or(0, |(first, last)| last - first);
        (sent, steps)
    }
}

/// What `map`'s group `group` does more than the rest, as the app works it out, for a made-up
/// crew that comes without it: the group's most common behaviors, each lighting every action
/// under it.
fn behaviors(map: &BehaviorMap, group: usize) -> Comparison {
    let mut rows: Vec<BehaviorRow> = Vec::new();
    for action in map.actions.iter().filter(|action| action.group == group) {
        if rows.iter().any(|row| row.behavior == action.behavior) {
            continue;
        }
        let lit: Vec<usize> = (0..map.actions.len())
            .filter(|at| map.actions[*at].behavior == action.behavior)
            .collect();
        rows.push(BehaviorRow {
            behavior: action.behavior.clone(),
            mine: lit.len(),
            lit,
            ..BehaviorRow::default()
        });
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.mine));
    rows.truncate(8);
    let mine = map
        .actions
        .iter()
        .filter(|action| action.group == group)
        .count();
    Comparison {
        rows,
        mine,
        others: map.actions.len() - mine,
        ..Comparison::default()
    }
}

/// The world on one level, keys `to` from the map, for crews R and T in each of `windows`: its
/// first turn drawn, then 20 s on performance cores judged with `hands` moving. The box the
/// window gives the map holds on every level.
fn judge_level(level: &str, to: &[KeyCode], hands: Hands, windows: &[Window]) {
    for window in windows {
        for key in [Key::R, Key::T] {
            let mut panel = Panel::open(key, *window);
            panel.run(1, Hands::Off);
            panel.keys(to);
            let hands = match hands {
                Hands::Off => Hands::Off,
                Hands::SweepingAndStepping { .. } => Hands::SweepingAndStepping {
                    rows: panel.rows_in_the_list(),
                },
            };
            panel.run(FIRST_TURN - 1, Hands::Off);
            let what = format!(
                "the world on the {level}, crew {key:?} at {:?}",
                window.size
            );
            let cells = panel.map_box();
            if let Some(map) = window.map {
                assert_eq!((cells.width, cells.height), map, "{what}: the map's box");
            }
            let (seconds, sent) = panel.judged_run(20, hands);
            eprintln!(
                "{what}: {} × {} cells, {hands:?}, {} bytes a second",
                cells.width,
                cells.height,
                sent / seconds.len().max(1)
            );
            crate::cpu_time::judge(
                &what,
                &seconds,
                Duration::from_millis(10),
                Duration::from_millis(30),
            );
        }
    }
}

/// The screen's loop as a live run drives it, on the wall clock.
struct Live {
    clock: Runtime,
    arrivals: watch::Receiver<u64>,
}

/// What a live run drew.
#[derive(Debug, Default)]
struct Drew {
    /// Frames drawn in each second.
    frames: Vec<u32>,
    /// Times a worker woke the screen in each second, having drawn or packed all it was asked
    /// for.
    arrivals: Vec<u32>,
    /// The screen's thread's work in each second.
    cpu: Vec<Spent>,
    /// The motion's steps the run saw begin, the one under way at its start counted.
    steps: u128,
    /// The steps it drew a frame at.
    drawn_steps: u128,
    sent: usize,
    /// Pictures among what it sent.
    pictures: usize,
}

impl Live {
    fn new() -> Self {
        Self {
            clock: tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap(),
            arrivals: worker::arrivals(),
        }
    }

    /// Waits as the screen's loop does, until a frame is due or a picture drawn or packed off
    /// the frame arrives, and tells the panel what arrived: whether pictures arrived, or `None`
    /// once `end` comes first.
    fn wait(&mut self, panel: &mut Panel, end: Instant) -> Option<bool> {
        let due = panel
            .ui
            .next_deadline(Instant::now())
            .map_or(end, |at| at.min(end));
        let arrivals = &mut self.arrivals;
        let arrived = self.clock.block_on(async {
            tokio::select! {
                Ok(()) = arrivals.changed() => true,
                () = tokio::time::sleep_until(due.into()) => false,
            }
        });
        if arrived {
            panel.ui.pictures_arrived();
        }
        (arrived || due < end).then_some(arrived)
    }

    /// Draws `panel`'s frames over `seconds` of the wall clock, as the screen's loop does.
    fn run(&mut self, panel: &mut Panel, seconds: u64) -> Drew {
        self.run_until(panel, |done| done.len() as u64 >= seconds)
    }

    /// As [`Live::run`], until `seconds` of the seconds ran on performance cores.
    fn judged_run(&mut self, panel: &mut Panel, seconds: usize) -> Drew {
        self.run_until(panel, |done| crate::cpu_time::measured(done, seconds))
    }

    fn run_until(&mut self, panel: &mut Panel, done: impl Fn(&[Spent]) -> bool) -> Drew {
        let mut drew = Drew::default();
        let first = panel.step_at(Instant::now());
        let mut last_drawn = None;
        while !done(&drew.cpu) {
            let end = Instant::now() + Duration::from_secs(1);
            let watch = Stopwatch::start();
            let (mut frames, mut arrivals) = (0, 0);
            while let Some(arrived) = self.wait(panel, end) {
                panel.at = Instant::now();
                let (sent, pictures, drawn) = panel.wake();
                drew.sent += sent;
                drew.pictures += pictures;
                frames += u32::from(drawn);
                arrivals += u32::from(arrived);
                let step = panel.step_at(panel.at);
                if drawn && step != last_drawn {
                    drew.drawn_steps += 1;
                    last_drawn = step;
                }
            }
            drew.frames.push(frames);
            drew.arrivals.push(arrivals);
            drew.cpu.push(watch.stop());
        }
        drew.steps = first
            .zip(panel.step_at(Instant::now()))
            .map_or(0, |(first, last)| last - first + 1);
        drew
    }

    /// How long `key` takes to reach the screen: from the key until a frame has sent the scene
    /// it changed, with none of its pictures still being packed.
    fn key_to_screen(&mut self, panel: &mut Panel, key: KeyCode) -> Duration {
        let before = panel.scene().map(|scene| deliver::look(&scene));
        panel.at = Instant::now();
        let pressed = panel.at;
        panel.keys(&[key]);
        let give_up = pressed + Duration::from_secs(60);
        loop {
            assert!(
                self.wait(panel, give_up).is_some(),
                "{key:?} never reached the screen"
            );
            panel.at = Instant::now();
            panel.frame();
            let shown = panel.scene().map(|scene| deliver::look(&scene));
            if shown != before && !panel.ui.map.stage.is_packing() {
                return pressed.elapsed();
            }
        }
    }
}

/// The list lights a behavior's dots with a glow over the whole map, and every move of the
/// pointer asks for the map's scene again with the ring where it points: the glow is drawn
/// once and kept while its row stays lit, drawn afresh for another row, and kept for when its
/// row is lit again.
#[test]
fn a_lit_row_keeps_its_glow_while_the_pointer_moves() {
    for window in WINDOWS {
        let size = window.size;
        let mut panel = Panel::open(Key::R, window);
        panel.run(1, Hands::Off);
        panel.keys(&LIGHT_A_ROW);
        panel.next_frame();
        let first = panel.glow().expect("the first row's glow");
        panel.keys(&[KeyCode::Down]);
        panel.next_frame();
        let next = panel.glow().expect("the next row's glow");
        assert_ne!(
            next.key, first.key,
            "{size:?}: each row lights its own dots"
        );
        panel.keys(&[KeyCode::Up]);
        panel.next_frame();
        let lit = panel.glow().expect("the first row's glow again");
        assert_eq!(lit.key, first.key, "{size:?}");
        assert!(
            std::ptr::eq(lit.image.pixels(), first.image.pixels()),
            "{size:?}: the first row's glow was drawn again"
        );
        let mut asked = 0;
        let mut last = panel.scene();
        for place in panel.across_the_map(16) {
            panel.point(place);
            panel.next_frame();
            let scene = panel.scene();
            if scene.as_ref().map(Arc::as_ptr) != last.as_ref().map(Arc::as_ptr) {
                asked += 1;
            }
            last = scene;
            let glow = panel.glow().expect("the lit row's glow");
            assert_eq!(glow.key, lit.key, "{size:?}");
            assert!(
                std::ptr::eq(glow.image.pixels(), lit.image.pixels()),
                "{size:?}: the glow was drawn again"
            );
        }
        assert!(asked >= 4, "{size:?}: the pointer asked for {asked} scenes");
    }
}

/// The wide and big windows give the map its widest box, which the planet and the universe
/// take too.
#[test]
fn the_timed_windows_give_the_map_the_box_they_are_timed_in() {
    for window in WINDOWS_FOR_A_GLOBE {
        let Some(map) = window.map else {
            continue;
        };
        let mut panel = Panel::open(Key::R, window);
        for level in [None, Some('2'), Some('3')] {
            if let Some(level) = level {
                panel.keys(&[KeyCode::Char(level)]);
            }
            panel.next_frame();
            let cells = panel.map_box();
            assert_eq!(
                (cells.width, cells.height),
                map,
                "{:?} on {level:?}",
                window.size
            );
        }
    }
}

#[test]
#[ignore = "timed: run alone on an optimized build"]
fn the_world_costs_under_a_percent_of_a_core_on_the_map() {
    judge_level("map", &[], Hands::Off, &WINDOWS);
}

#[test]
#[ignore = "timed: run alone on an optimized build"]
fn the_world_costs_under_a_percent_of_a_core_with_a_row_lit_and_the_pointer_moving() {
    judge_level(
        "map with the list's rows lit in turn",
        &LIGHT_A_ROW,
        Hands::SweepingAndStepping { rows: 0 },
        &WINDOWS,
    );
}

#[test]
#[ignore = "timed: run alone on an optimized build"]
fn the_world_costs_under_a_percent_of_a_core_on_the_planet() {
    judge_level(
        "planet",
        &[KeyCode::Char('2')],
        Hands::Off,
        &WINDOWS_FOR_A_GLOBE,
    );
}

#[test]
#[ignore = "timed: run alone on an optimized build"]
fn the_world_costs_under_a_percent_of_a_core_in_the_universe() {
    judge_level(
        "universe",
        &[KeyCode::Char('3')],
        Hands::Off,
        &WINDOWS_FOR_A_GLOBE,
    );
}

#[test]
#[ignore = "timed: run alone on an optimized build"]
fn the_world_costs_under_a_percent_of_a_core_in_a_galaxy() {
    judge_level(
        "galaxy",
        &[KeyCode::Char('3'), KeyCode::Right, KeyCode::Enter],
        Hands::Off,
        &WINDOWS,
    );
}

#[test]
#[ignore = "timed: run alone on an optimized build"]
fn the_world_costs_under_a_percent_of_a_core_in_a_system() {
    judge_level(
        "system",
        &[
            KeyCode::Char('3'),
            KeyCode::Right,
            KeyCode::Enter,
            KeyCode::Right,
            KeyCode::Enter,
        ],
        Hands::Off,
        &WINDOWS,
    );
}

/// With the world drawn and packed on threads, as the app does, its first turn drawn, the
/// screen's thread stays under the 1% of a core the whole work in line has, a steady motion
/// sends no picture again, and its placements cost as many bytes a step as in line, which draws
/// every step. The screen draws a frame only for a step or for pictures that arrived after their
/// step's frame, never one every few milliseconds to look for them.
#[test]
#[ignore = "timed: run alone on an optimized build"]
fn the_world_on_threads_draws_a_frame_only_when_something_arrived() {
    // In line first: from the first panel on threads, this test's thread draws on threads.
    let mut in_line = Vec::new();
    for window in WINDOWS {
        for key in [Key::R, Key::T] {
            let mut panel = Panel::open(key, window);
            panel.run(FIRST_TURN, Hands::Off);
            in_line.push(panel.steps(10));
        }
    }
    let mut in_line = in_line.into_iter();
    for window in WINDOWS {
        for key in [Key::R, Key::T] {
            let (in_line, in_line_steps) = in_line.next().unwrap_or_default();
            let mut panel = Panel::on_threads(key, window, false);
            let mut live = Live::new();
            live.run(&mut panel, FIRST_TURN);
            let drew = live.judged_run(&mut panel, 10);
            let what = format!("the world on threads, crew {key:?} at {:?}", window.size);
            let frames: u32 = drew.frames.iter().sum();
            let arrivals: u32 = drew.arrivals.iter().sum();
            let a_step = |sent: usize, steps: u128| sent as u128 / steps.max(1);
            eprintln!(
                "{what}: {:?} frames each second for {} steps, {:?} arrivals, {} bytes a step, \
                 {} in line",
                drew.frames,
                drew.steps,
                drew.arrivals,
                a_step(drew.sent, drew.drawn_steps),
                a_step(in_line, in_line_steps),
            );
            crate::cpu_time::judge(
                &format!("{what}, the screen's thread"),
                &drew.cpu,
                Duration::from_millis(10),
                Duration::from_millis(30),
            );
            assert_eq!(drew.pictures, 0, "{what}: pictures sent again");
            assert!(
                drew.sent as u128 * in_line_steps * 100
                    <= in_line as u128 * drew.drawn_steps * (100 + MORE_A_STEP),
                "{what}: {} bytes over {} steps, {in_line} over {in_line_steps} in line",
                drew.sent,
                drew.drawn_steps
            );
            assert!(
                u128::from(frames) <= drew.steps + u128::from(arrivals),
                "{what}: {frames} frames for {} steps and {arrivals} arrivals",
                drew.steps
            );
        }
    }
}

/// With the world drawn and packed on threads, a key reaches the screen within two steps of the
/// motion when it only lights a row, and inside half a second when it asks for a new look of
/// the map or goes to the planet.
#[test]
#[ignore = "timed: run alone on an optimized build"]
fn a_key_reaches_the_screen_in_time() {
    for window in WINDOWS {
        for key in [Key::R, Key::T] {
            let mut panel = Panel::on_threads(key, window, true);
            let mut live = Live::new();
            live.run(&mut panel, 5);
            let look = live.key_to_screen(&mut panel, LIGHT_A_ROW[0]);
            live.run(&mut panel, 1);
            let lit = live.key_to_screen(&mut panel, LIGHT_A_ROW[1]);
            live.run(&mut panel, 1);
            let next = live.key_to_screen(&mut panel, KeyCode::Down);
            live.run(&mut panel, 1);
            let planet = live.key_to_screen(&mut panel, KeyCode::Char('2'));
            eprintln!(
                "crew {key:?} at {:?}: a new look in {look:?}, a lit row in {lit:?}, the next \
                 row in {next:?}, the planet in {planet:?}",
                window.size
            );
            assert!(look < Duration::from_millis(500), "a new look: {look:?}");
            assert!(lit < Duration::from_millis(250), "a lit row: {lit:?}");
            assert!(next < Duration::from_millis(250), "the next row: {next:?}");
            assert!(
                planet < Duration::from_millis(500),
                "the planet: {planet:?}"
            );
        }
    }
}
