//! The map's motion on screen: one clock steps it while its cells show, and every rule that
//! holds it still keeps the app asleep.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect, Size};

use super::super::*;
use crate::app::model::ViewModel;
use crate::base::theme::Theme;
use crate::frame::state::AppState;
use crate::frame::view::map::MapCells;
use crate::frame::view::{View, ViewOutcome};
use crate::terminal::InlineTerminal;
use crate::world::deliver::clock::{InputClock, Power, PowerSource, Rate, ReportedFocus, Sources};
use crate::world::{Depth, Image, Layer, PartId, Picture, Sprite};

/// A step of the test's map, 20 × 4 cells: a small map steps 16 times a second.
const STEP: Duration = Duration::from_micros(62_500);

const CELL: (u16, u16) = (10, 20);

/// A docked map whose storm crosses it a step at a time.
#[derive(Debug)]
struct Weather {
    /// Its cells show, as on its own tab.
    shows: Arc<AtomicBool>,
    moving: bool,
    /// How many scenes the screen asked for.
    asked: Arc<AtomicUsize>,
    origin: Instant,
}

impl View for Weather {
    fn render(&mut self, _area: Rect, _buf: &mut Buffer, _ctx: &ViewCtx<'_>) {}

    fn docked(&self) -> bool {
        true
    }

    fn handle_key(&mut self, _key: KeyEvent, _ctx: &ViewCtx<'_>) -> ViewOutcome {
        ViewOutcome::Consumed
    }

    fn map(&mut self) -> Option<&mut dyn MapView> {
        Some(self)
    }
}

impl MapView for Weather {
    fn place_pictures(&mut self, _cell: Option<(u16, u16)>) {}

    fn map_cells(&self) -> Option<MapCells> {
        self.shows.load(Ordering::Relaxed).then(|| MapCells {
            area: Rect::new(2, 1, 20, 4),
            picture: Some(1),
        })
    }

    fn scene(&self, cell: (u16, u16), ctx: &ViewCtx<'_>) -> Option<Arc<Scene>> {
        self.asked.fetch_add(1, Ordering::Relaxed);
        let (width, height) = (20 * usize::from(cell.0), 4 * usize::from(cell.1));
        let step = ctx.now.saturating_duration_since(self.origin).as_nanos() / STEP.as_nanos();
        let mut scene = Scene::new(width, height);
        scene.add_layer(Layer {
            id: PartId::new("ground", 0),
            depth: Depth::Ground,
            picture: Picture::new(1, Image::transparent(width, height)),
        });
        scene.add_sprite(Sprite {
            id: PartId::new("storm", 0),
            depth: Depth::Weather,
            at: (i32::try_from(step % 20).unwrap() * 10, 10),
            picture: Picture::new(2, Image::transparent(8, 8)),
        });
        // As the world draws its own motion: held still when motion is reduced.
        scene.moves = self.moving && !ctx.theme.reduced_motion;
        Some(Arc::new(scene))
    }

    fn point(&mut self, _at: Option<MapPoint>) -> bool {
        false
    }

    fn click(&mut self) -> bool {
        false
    }
}

/// The system's power as a test sets it.
#[derive(Debug, Clone, Default)]
struct SetPower(Arc<Mutex<Power>>);

impl PowerSource for SetPower {
    fn power(&self, _now: Instant) -> Power {
        *self.0.lock().unwrap()
    }
}

struct World {
    ui: Ui<Vec<u8>>,
    start: Instant,
    at: Instant,
    state: AppState,
    shows: Arc<AtomicBool>,
    asked: Arc<AtomicUsize>,
    power: SetPower,
}

impl World {
    fn new(graphics: Graphics, moving: bool, change: impl FnOnce(&mut Theme)) -> Self {
        let terminal = InlineTerminal::new(Vec::new(), Size::new(80, 24), Position::new(0, 0));
        let mut theme = Theme::dark();
        change(&mut theme);
        let start = Instant::now();
        let mut ui = Ui::new(terminal, theme, ViewModel::new("conversation"), start);
        ui.use_fullscreen();
        ui.use_pictures(Some(graphics), Some(CELL));
        let power = SetPower::default();
        ui.use_world_sources(Sources {
            focus: Box::new(ReportedFocus::default()),
            idle: Box::new(InputClock::new(start)),
            power: Box::new(power.clone()),
        });
        let shows = Arc::new(AtomicBool::new(true));
        let asked = Arc::new(AtomicUsize::new(0));
        ui.push_view(Box::new(Weather {
            shows: Arc::clone(&shows),
            moving,
            asked: Arc::clone(&asked),
            origin: start,
        }));
        Self {
            ui,
            start,
            at: start,
            state: AppState::default(),
            shows,
            asked,
            power,
        }
    }

    /// Draws a frame now and takes what went out.
    fn frame(&mut self) -> String {
        self.ui.frame_in(self.at, &self.state).unwrap();
        String::from_utf8_lossy(&std::mem::take(self.ui.terminal_mut().writer_mut())).into_owned()
    }

    fn next(&self) -> Option<Instant> {
        self.ui.next_deadline(self.at)
    }

    /// Sleeps until the app's next wake, as the event loop does, and draws that frame.
    fn wake(&mut self) -> String {
        self.at = self.next().expect("something to wake for");
        self.frame()
    }

    fn event(&mut self, event: Event) {
        self.ui.handle_event_in(event, self.at, &self.state);
    }

    fn asked(&self) -> usize {
        self.asked.load(Ordering::Relaxed)
    }

    /// Whether anything wakes the app in the next `span` of time.
    fn asleep_for(&self, span: Duration) -> bool {
        self.next().is_none_or(|at| at >= self.at + span)
    }
}

fn pointer_moved() -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column: 60,
        row: 0,
        modifiers: KeyModifiers::NONE,
    })
}

#[test]
fn the_world_steps_at_its_rate_while_it_shows() {
    let mut world = World::new(Graphics::Kitty, true, |_| {});
    let first = world.frame();
    assert_eq!(first.matches("\x1b_Ga=t").count(), 2, "{first:?}");
    assert_eq!(world.next(), Some(world.start + STEP));
    let step = world.wake();
    // Only the storm moved: one placement again, and no picture sent.
    assert_eq!(step.matches("\x1b_Ga=p").count(), 1, "{step:?}");
    assert!(step.contains(",p=2,"), "{step:?}");
    assert!(!step.contains("\x1b_Ga=t"), "{step:?}");
    let before = world.asked();
    for _ in 0..8 {
        world.wake();
    }
    assert_eq!(world.at, world.start + STEP * 9, "a step every 62.5 ms");
    assert_eq!(world.asked() - before, 8, "one scene a step");
}

#[test]
fn leaving_takes_the_pictures_off_and_nothing_wakes_the_app_after() {
    let mut world = World::new(Graphics::Kitty, true, |_| {});
    world.frame();
    world.wake();
    // Off to another tab: its cells are gone from this frame.
    world.shows.store(false, Ordering::Relaxed);
    world.ui.dirty = true;
    world.at += Duration::from_millis(10);
    let left = world.frame();
    assert_eq!(left.matches("\x1b_Ga=d,d=i,").count(), 2, "{left:?}");
    assert!(
        world.asleep_for(Duration::from_secs(10)),
        "{:?}",
        world.next()
    );
    let asked = world.asked();
    // The one wake left is the minute's end, which frees the pictures kept for coming back.
    let left_at = world.at;
    let freed = world.wake();
    assert_eq!(world.at, left_at + Duration::from_secs(60));
    assert_eq!(freed.matches("\x1b_Ga=d,d=I,").count(), 2, "{freed:?}");
    assert_eq!(world.next(), None);
    assert_eq!(world.asked(), asked, "no scene is drawn while it is away");
}

#[test]
fn a_window_without_focus_holds_the_world_still_until_focus_comes_back() {
    let mut world = World::new(Graphics::Kitty, true, |_| {});
    world.frame();
    world.event(Event::FocusLost);
    // The step already asked for still comes, and finds the world held.
    let held = world.wake();
    assert!(!held.contains("\x1b_G"), "{held:?}");
    assert!(
        world.asleep_for(Duration::from_secs(10)),
        "{:?}",
        world.next()
    );
    world.at += Duration::from_secs(3);
    world.event(Event::FocusGained);
    world.frame();
    assert!(world.next().is_some_and(|at| at <= world.at + STEP));
}

#[test]
fn two_idle_minutes_hold_the_world_still_until_a_key_or_the_pointer_moves() {
    let mut world = World::new(Graphics::Kitty, true, |_| {});
    world.frame();
    world.at = world.start + Duration::from_secs(120);
    world.ui.dirty = true;
    world.frame();
    assert!(
        world.asleep_for(Duration::from_secs(10)),
        "{:?}",
        world.next()
    );
    world.event(pointer_moved());
    world.frame();
    assert!(world.next().is_some_and(|at| at <= world.at + STEP));
}

#[test]
fn low_power_holds_the_world_still_and_a_battery_halves_its_clock() {
    let mut world = World::new(Graphics::Kitty, true, |_| {});
    world.frame();
    *world.power.0.lock().unwrap() = Power {
        low_power: false,
        on_battery: true,
    };
    world.wake();
    assert_eq!(world.next(), Some(world.start + STEP * 2));
    *world.power.0.lock().unwrap() = Power {
        low_power: true,
        on_battery: true,
    };
    world.wake();
    assert!(
        world.asleep_for(Duration::from_secs(10)),
        "{:?}",
        world.next()
    );
}

#[test]
fn reduced_motion_a_still_scene_and_whole_pictures_never_wake_the_app() {
    let reduced = World::new(Graphics::Kitty, true, |theme| theme.reduced_motion = true);
    let still = World::new(Graphics::Kitty, false, |_| {});
    let whole = World::new(Graphics::Iterm, true, |_| {});
    for mut world in [reduced, still, whole] {
        world.frame();
        world.at += Duration::from_millis(10);
        world.ui.dirty = true;
        world.frame();
        assert!(
            world.asleep_for(Duration::from_secs(10)),
            "{:?}",
            world.next()
        );
        assert_eq!(world.asked(), 1);
    }
}

#[test]
fn the_test_map_steps_at_the_quick_rate() {
    assert_eq!(Rate::for_map(20, 4).step(), STEP);
}
