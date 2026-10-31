use super::crews::{Crew, Key};
use super::*;
use crate::world::map::climate::Climate;

fn ground(key: Key) -> &'static Ground {
    use std::sync::OnceLock;
    static GROUNDS: OnceLock<Vec<Ground>> = OnceLock::new();
    let grounds = GROUNDS.get_or_init(|| {
        Key::ALL
            .iter()
            .map(|key| Ground::grow(Crew::get(*key).facts()))
            .collect()
    });
    &grounds[Key::ALL.iter().position(|k| *k == key).unwrap_or(0)]
}

#[test]
fn the_grids_cover_the_map_the_world_is_given_for() {
    assert_eq!(WIDTH as f64, Plate::MAP.width());
    assert_eq!(HEIGHT as f64, Plate::MAP.height());
    assert_eq!((COARSE_WIDTH, COARSE_HEIGHT), (289, 138));
    assert_eq!((FINE_WIDTH, FINE_HEIGHT), (577, 274));
}

#[test]
fn a_crews_detail_is_the_same_every_time() {
    let crew = Crew::get(Key::R);
    let again = super::crews::Detail::draw(&crew.labels, &crew.l, &crew.habits);
    assert_eq!(again.day, crew.detail.day);
    assert_eq!(again.minute, crew.detail.minute);
    assert_eq!(crew.detail.sessions.len(), 68);
    assert_eq!(Crew::get(Key::S).detail.sessions.len(), 79);
    assert_eq!(Crew::get(Key::T).detail.sessions.len(), 55);
    assert_eq!(Crew::get(Key::U).detail.sessions.len(), 70);
}

#[test]
fn the_crews_share_the_places_but_not_the_land() {
    let tally = |key: Key| {
        let mut tally = [0; 5];
        for climate in ground(key).climate.class.iter().flatten() {
            tally[climate.index()] += 1;
        }
        tally
    };
    assert_eq!(tally(Key::R), [4, 3, 4, 14, 16]);
    assert_eq!(tally(Key::S), [0, 0, 0, 0, 17]);
    assert_eq!(tally(Key::T), [0, 2, 12, 0, 0]);
}

#[test]
fn the_land_is_about_a_quarter_of_the_map() {
    let share = |key: Key| (ground(key).land_share * 1000.0).round() / 1000.0;
    assert_eq!(share(Key::R), 0.27);
    assert_eq!(share(Key::S), 0.185);
    assert_eq!(share(Key::T), 0.14);
    assert_eq!(share(Key::U), 0.256);
    assert!(Key::ALL.iter().all(|key| ground(*key).look == Look::Ocean));
}

#[test]
fn open_sea_lies_all_round_the_land() {
    for key in Key::ALL {
        let land = &ground(key).land;
        let edge = (0.06 * COARSE_WIDTH as f64) as usize;
        let near_an_edge = (0..COARSE_HEIGHT)
            .flat_map(|j| {
                (0..edge)
                    .chain(COARSE_WIDTH - edge..COARSE_WIDTH)
                    .map(move |k| (k, j))
            })
            .filter(|(k, j)| land.get(*k, *j) >= 1.0)
            .count();
        assert_eq!(near_an_edge, 0, "crew {key:?}");
    }
}

#[test]
fn a_mountain_is_as_tall_as_its_count() {
    let ground = ground(Key::R);
    let busiest = ground.busiest.expect("a busiest behavior");
    assert_eq!(
        ground.facts.behaviors[busiest].name,
        "returned its structured answer"
    );
    assert_eq!(ground.count[busiest], 183);
    let (x, y) = ground.centre[busiest];
    let peak = coarse(&ground.peaks, x, y);
    assert!((peak - 183.0).abs() < 20.0, "peak {peak}");
}

#[test]
fn a_steady_habit_is_a_plateau() {
    let steady = ground(Key::S);
    let names: Vec<(&str, &[u32])> = steady
        .plateaus
        .iter()
        .map(|plateau| {
            (
                steady.facts.behaviors[plateau.behavior].name.as_str(),
                plateau.weeks.as_slice(),
            )
        })
        .collect();
    assert_eq!(
        names,
        vec![("tried an edit that didn't apply", &[17, 14, 13, 11][..])]
    );
    assert!(steady.mesa.is_some());
    for key in [Key::R, Key::T, Key::U] {
        assert!(ground(key).plateaus.is_empty());
    }
}

#[test]
fn waits_and_days_read_as_the_crews_habits() {
    let waited = |key: Key, words: &str| {
        let crew = Crew::get(key);
        let facts = crew.facts();
        let at = crew.label(words);
        (
            facts.behaviors[at].after_a_wait,
            crew.l.iter().filter(|l| **l == at).count(),
        )
    };
    assert_eq!(waited(Key::R, "ran a shell command"), (7, 10));
    assert_eq!(waited(Key::S, "built the project"), (16, 60));
    assert_eq!(waited(Key::T, "fetched from the web"), (31, 75));
    assert_eq!(waited(Key::U, "searched code by pattern"), (6, 16));

    let r = Crew::get(Key::R);
    let facts = r.facts();
    let web = &facts.behaviors[r.label("searched the web")];
    assert_eq!(web.last_used, Some(16));
    let printed = &facts.behaviors[r.label("printed files")];
    assert_eq!(printed.days_in_last_7, 4);
    assert_eq!(facts.quiet_stretches.len(), 4);
    assert_eq!(facts.quiet_days, 8);

    let u = Crew::get(Key::U);
    let sunk = &u.facts().behaviors[u.label("changed git state")];
    assert_eq!((sunk.last_used, sunk.last_period), (Some(34), 130));
}

#[test]
fn a_crew_quiet_for_days_grows_no_rainforest() {
    let ground = ground(Key::T);
    assert_eq!(ground.facts.idle_days, 9);
    assert!(
        !ground
            .climate
            .class
            .iter()
            .flatten()
            .any(|climate| *climate == Climate::Rainforest)
    );
}

#[test]
fn every_action_sits_once_in_a_session_in_its_order() {
    use super::crews::detail::{Ended, Outcome};
    let crew = Crew::get(Key::R);
    let detail = &crew.detail;
    let mut seen = vec![0; crew.len()];
    for (index, session) in detail.sessions.iter().enumerate() {
        for (nth, &at) in session.actions.iter().enumerate() {
            seen[at] += 1;
            assert_eq!((detail.session[at], detail.nth[at]), (index, nth));
            assert_eq!(detail.day[at], session.day);
            assert_eq!(detail.outcome[at], session.outcome);
        }
        let first = session.actions[0];
        assert_eq!(f64::from(detail.minute[first]), session.start as f32 as f64);
        let times: Vec<f32> = session.actions.iter().map(|at| detail.time[*at]).collect();
        assert!(times.windows(2).all(|pair| pair[0] <= pair[1]));
    }
    assert!(seen.iter().all(|times| *times == 1));
    assert!(detail.sessions.iter().any(|s| s.ended == Ended::Failed));
    assert!(
        detail
            .sessions
            .iter()
            .any(|s| s.outcome == Outcome::SentBack)
    );

    let failing = crew.label("ran the test suite, which failed");
    let failed = (0..crew.len()).filter(|at| crew.l[*at] == failing);
    assert!(failed.clone().count() > 0);
    assert!(failed.into_iter().all(|at| !detail.ok[at]));
    assert_eq!(crew.groups[0].name, "Haiku 5.5");
    assert_eq!(crew.groups[0].rgb, [224, 138, 102]);
}

mod from_a_map {
    use argonauts_core::behaviors::behavior_map::{BehaviorMap, MapAction, MapGroup};
    use argonauts_core::behaviors::behavior_world::{
        BehaviorFacts, CrewFacts, Move, QuietStretch, WorldFacts,
    };
    use time::macros::datetime;
    use time::{Duration, OffsetDateTime};

    use super::super::GroundFacts;

    const END: OffsetDateTime = datetime!(2026-10-14 20:00 UTC);

    fn action(behavior: &str, group: usize, days_back: i64) -> MapAction {
        MapAction {
            x: 0.4,
            y: 0.6,
            group,
            behavior: behavior.to_owned(),
            label: behavior.to_owned(),
            summary: None,
            agent: "build".to_owned(),
            model: "Opus 5.5".to_owned(),
            cli: "Claude Code".to_owned(),
            role: "builder".to_owned(),
            workflow: "Maker-checker".to_owned(),
            project: "example".to_owned(),
            run: None,
            at: END - Duration::days(days_back) - Duration::hours(3),
            nth: 1,
            of: 1,
            tool: "Read".to_owned(),
            input: "src/lib.rs".to_owned(),
            place: None,
            run_shows: None,
            failed: false,
            outcome: None,
        }
    }

    fn map(world: Option<WorldFacts>) -> BehaviorMap {
        BehaviorMap {
            groups: vec![
                MapGroup {
                    name: "Opus 5.5".to_owned(),
                    ..MapGroup::default()
                },
                MapGroup {
                    name: "Haiku 5.5".to_owned(),
                    ..MapGroup::default()
                },
            ],
            actions: vec![
                action("ran the test suite", 0, 2),
                action("a summary of its own", 1, 0),
                action("read a source file", 1, 9),
                action("ran the test suite", 1, 0),
            ],
            world: world.map(Box::new),
            ..BehaviorMap::default()
        }
    }

    #[test]
    fn behaviors_come_in_the_label_lists_order_then_as_first_seen() {
        let facts = GroundFacts::from_map(&map(None), 7);
        let names: Vec<&str> = facts.behaviors.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "read a source file",
                "ran the test suite",
                "a summary of its own"
            ]
        );
        assert_eq!(facts.seed, 7);
        assert_eq!(facts.groups, 2);
        let behaviors: Vec<usize> = facts.dots.iter().map(|dot| dot.behavior).collect();
        assert_eq!(behaviors, [1, 2, 0, 1]);
        assert_eq!(
            facts.behaviors[0].place,
            Some((0.10_f32 as f64, 1.0 - 0.14_f32 as f64))
        );
        assert_eq!(facts.behaviors[2].place, None);
    }

    #[test]
    fn without_the_worlds_facts_the_days_come_from_the_dots() {
        let facts = GroundFacts::from_map(&map(None), 7);
        let tests = &facts.behaviors[1];
        assert_eq!((tests.last_used, tests.days_in_last_7), (Some(0), 2));
        let source = &facts.behaviors[0];
        assert_eq!((source.last_used, source.days_in_last_7), (Some(9), 0));
        assert!(tests.weeks.is_empty());
    }

    #[test]
    fn the_worlds_facts_give_the_days_waits_moves_and_quiet_days() {
        let world = WorldFacts {
            end: END,
            behaviors: vec![
                BehaviorFacts {
                    behavior: "ran the test suite".to_owned(),
                    count: 2,
                    last_used: Some(1),
                    days_in_last_7: 5,
                    weeks: vec![1, 1],
                    after_a_wait: 1,
                    ..BehaviorFacts::default()
                },
                BehaviorFacts {
                    behavior: "changed git state".to_owned(),
                    last_period: 40,
                    last_used: Some(34),
                    ..BehaviorFacts::default()
                },
            ],
            moves: vec![Move {
                from: "read a source file".to_owned(),
                to: "ran the test suite".to_owned(),
                count: 9,
            }],
            crew: CrewFacts {
                idle_days: 3,
                quiet_days: 5,
                quiet_stretches: vec![
                    QuietStretch { ends: 12, days: 2 },
                    QuietStretch { ends: 4, days: 3 },
                ],
                ..CrewFacts::default()
            },
            ..WorldFacts::default()
        };
        let facts = GroundFacts::from_map(&map(Some(world)), 7);
        let names: Vec<&str> = facts.behaviors.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "read a source file",
                "ran the test suite",
                "changed git state",
                "a summary of its own"
            ]
        );
        let tests = &facts.behaviors[1];
        assert_eq!(
            (tests.last_used, tests.days_in_last_7, tests.after_a_wait),
            (Some(1), 5, 1)
        );
        assert_eq!(tests.weeks, [1, 1]);
        let sunk = &facts.behaviors[2];
        assert_eq!((sunk.last_used, sunk.last_period), (Some(34), 40));
        assert!(sunk.place.is_some());
        // A behavior the world has no facts for still reads its days from its dots.
        assert_eq!(facts.behaviors[0].last_used, Some(9));
        assert_eq!(facts.moves.len(), 1);
        assert_eq!(
            (facts.moves[0].from, facts.moves[0].to, facts.moves[0].count),
            (0, 1, 9)
        );
        assert_eq!((facts.idle_days, facts.quiet_days), (3, 5));
        let ends: Vec<u32> = facts.quiet_stretches.iter().map(|s| s.ends).collect();
        assert_eq!(ends, [4, 12]);
    }
}

#[test]
fn a_crew_with_no_actions_grows_open_sea() {
    use crate::world::map::land::{GroundKind, Placer, land_marks, line_at};
    use crate::world::map::relief::{DotStyle, Dots, Relief};
    let ground = Ground::grow(GroundFacts::default());
    assert_eq!(ground.look, Look::Gas);
    assert_eq!(ground.busiest, None);
    assert_eq!(ground.land_share, 0.0);
    assert_eq!(line_at(&ground, 576.0, 273.0).kind, GroundKind::Sea);
    let mut placer = Placer::new(&ground);
    assert!(land_marks(&ground, &mut placer).is_empty());
    let relief = Relief::new(&ground);
    let style = DotStyle {
        groups: &[],
        compared: None,
    };
    assert_eq!(Dots::new(&ground, &relief, style, 1.0).hidden_count(), 0);
}
