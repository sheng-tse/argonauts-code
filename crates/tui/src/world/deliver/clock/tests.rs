use std::sync::{Arc, Mutex};

use super::*;

/// A step's length, for counting steps.
const STEP: Duration = Duration::from_millis(125);

/// A focus, an input time and a power source the test sets.
#[derive(Clone)]
struct Fake {
    focused: Arc<Mutex<bool>>,
    last_input: Arc<Mutex<Instant>>,
    power: Arc<Mutex<Power>>,
}

fn set<T>(cell: &Mutex<T>, value: T) {
    *cell.lock().unwrap() = value;
}

impl FocusSource for Fake {
    fn focused(&self) -> bool {
        *self.focused.lock().unwrap()
    }
}

impl IdleClock for Fake {
    fn last_input(&self) -> Instant {
        *self.last_input.lock().unwrap()
    }
}

impl PowerSource for Fake {
    fn power(&self, _now: Instant) -> Power {
        *self.power.lock().unwrap()
    }
}

fn sources(start: Instant) -> (Sources, Fake) {
    let fake = Fake {
        focused: Arc::new(Mutex::new(true)),
        last_input: Arc::new(Mutex::new(start)),
        power: Arc::new(Mutex::new(Power::default())),
    };
    let sources = Sources {
        focus: Box::new(fake.clone()),
        idle: Box::new(fake.clone()),
        power: Box::new(fake.clone()),
    };
    (sources, fake)
}

const MOVING: Showing = Showing {
    shown: true,
    moves: true,
    motion: true,
    kept_pictures: true,
};

#[test]
fn the_world_moves_only_while_every_rule_allows_it() {
    let start = Instant::now();
    let (sources, fake) = sources(start);
    let now = start + Duration::from_secs(1);
    assert_eq!(sources.still(MOVING, now), None);
    let cases = [
        (
            Showing {
                shown: false,
                ..MOVING
            },
            Still::Hidden,
        ),
        (
            Showing {
                moves: false,
                ..MOVING
            },
            Still::Unmoving,
        ),
        (
            Showing {
                motion: false,
                ..MOVING
            },
            Still::Reduced,
        ),
        (
            Showing {
                kept_pictures: false,
                ..MOVING
            },
            Still::WholePictures,
        ),
    ];
    for (showing, still) in cases {
        assert_eq!(sources.still(showing, now), Some(still));
    }

    set(&fake.focused, false);
    assert_eq!(sources.still(MOVING, now), Some(Still::Unfocused));
    set(&fake.focused, true);

    set(
        &fake.power,
        Power {
            low_power: true,
            on_battery: true,
        },
    );
    assert_eq!(sources.still(MOVING, now), Some(Still::LowPower));
    set(&fake.power, Power::default());
    assert_eq!(sources.still(MOVING, now), None);
}

#[test]
fn two_minutes_without_a_key_or_a_pointer_move_hold_it_still() {
    let start = Instant::now();
    let (sources, fake) = sources(start);
    let just_before = start + IDLE_AFTER - Duration::from_millis(1);
    assert_eq!(sources.still(MOVING, just_before), None);
    assert_eq!(sources.still(MOVING, start + IDLE_AFTER), Some(Still::Idle));
    set(&fake.last_input, start + IDLE_AFTER);
    assert_eq!(sources.still(MOVING, start + IDLE_AFTER), None);
}

#[test]
fn a_step_at_its_rate_and_twice_as_long_on_battery() {
    let start = Instant::now();
    let (sources, fake) = sources(start);
    assert_eq!(
        sources.step(start, Rate::QUICK),
        Duration::from_micros(62_500)
    );
    set(
        &fake.power,
        Power {
            low_power: false,
            on_battery: true,
        },
    );
    assert_eq!(sources.step(start, Rate::QUICK), Duration::from_millis(125));
    // On battery it still moves.
    assert_eq!(sources.still(MOVING, start), None);
}

#[test]
fn steps_count_from_when_the_world_first_showed() {
    let origin = Instant::now();
    let mut steps = Steps::new(origin);
    assert_eq!(steps.index(origin, STEP), 0);
    assert_eq!(steps.index(origin + Duration::from_millis(124), STEP), 0);
    assert_eq!(steps.index(origin + Duration::from_millis(125), STEP), 1);
    assert_eq!(
        steps.next(origin + Duration::from_millis(130), STEP),
        origin + Duration::from_millis(250)
    );
}

/// Going onto battery halves how often the world steps, but the count goes on from the step it
/// had reached, and so does coming back: nothing moving jumps to another place in its loop.
#[test]
fn a_change_of_power_slows_the_steps_without_a_jump() {
    let origin = Instant::now();
    let at = |millis: u64| origin + Duration::from_millis(millis);
    let mut steps = Steps::new(origin);
    assert_eq!(steps.index(at(1000), STEP), 8);
    // On battery from here, each step twice as long.
    assert_eq!(steps.next(at(1000), STEP * 2), at(1250));
    assert_eq!(steps.index(at(1000), STEP * 2), 8);
    assert_eq!(steps.index(at(1249), STEP * 2), 8);
    assert_eq!(steps.index(at(1250), STEP * 2), 9);
    assert_eq!(steps.next(at(1300), STEP * 2), at(1500));
    // Back on the wall's power, in the step it had reached.
    assert_eq!(steps.index(at(1600), STEP), 10);
    assert_eq!(steps.index(at(1724), STEP), 10);
    assert_eq!(steps.index(at(1725), STEP), 11);
}

#[test]
fn the_real_sources_follow_the_terminal_and_the_keys() {
    let start = Instant::now();
    let mut focus = ReportedFocus::default();
    assert!(
        focus.focused(),
        "a terminal that never says counts as focused"
    );
    focus.set_focused(false);
    assert!(!focus.focused());
    let mut input = InputClock::new(start);
    input.input(start + Duration::from_secs(5));
    // An event read late, stamped earlier, never moves the clock back.
    input.input(start + Duration::from_secs(1));
    assert_eq!(input.last_input(), start + Duration::from_secs(5));
}

#[test]
fn a_small_map_steps_16_times_a_second_and_a_big_one_12() {
    assert_eq!(Rate::for_map(64, 13), Rate::QUICK);
    assert_eq!(Rate::for_map(88, 16), Rate::STEADY);
    assert_eq!(Rate::STEADY.step(), Duration::from_nanos(83_333_333));
    assert_eq!(Rate::STEADY.steps_in(Duration::from_secs(30)), 360);
    assert_eq!(Rate::QUICK.steps_in(Duration::from_millis(62)), 0);
    assert_eq!(Rate::QUICK.steps_in(Duration::from_micros(62_500)), 1);
    assert_eq!(Rate::STEADY.seconds(18), 1.5);
}

/// A step at 12 a second isn't a whole number of nanoseconds, so the steps are counted in the
/// clock's own steps: a moment it wakes at is the start of the step it woke for, never the end
/// of the one before.
#[test]
fn a_moment_the_clock_wakes_at_begins_the_step_it_woke_for() {
    for rate in [Rate::QUICK, Rate::STEADY] {
        for steps in [1, 7, 360, 4_321] {
            assert_eq!(
                rate.steps_in(rate.step() * steps),
                u64::from(steps),
                "{rate:?}"
            );
        }
    }
}
