//! What the universe makes of each crew's facts: its galaxies, the kind of each model's body,
//! who orbits whom, what flies, and each model's system.

use argonauts_core::behavior_map::{BehaviorMap, MapAction};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::logos::LogoMark;
use time::Duration;

use super::crews::{self, Crew};
use crate::format::WallClock;
use crate::world::cosmos::{Body, Climate, StarKind, Universe, star_actions};

fn star<'a>(crew: &Crew, universe: &'a Universe, id: &str) -> &'a crate::world::cosmos::Star {
    let model = crew
        .facts
        .models
        .iter()
        .position(|m| m.model == id)
        .unwrap();
    &universe.stars[universe.star_of_model(model).unwrap()]
}

fn kind_of(crew: &Crew, universe: &Universe, id: &str) -> StarKind {
    star(crew, universe, id).kind
}

fn makers(universe: &Universe) -> Vec<&str> {
    universe.makers.iter().map(|m| m.key.as_str()).collect()
}

#[test]
fn every_crew_has_a_galaxy_per_maker_and_a_star_per_model() {
    let counts: Vec<(usize, usize)> = [crews::r(), crews::s(), crews::t(), crews::u()]
        .iter()
        .map(|crew| {
            let universe = crew.universe();
            (universe.makers.len(), universe.stars.len())
        })
        .collect();
    assert_eq!(counts, [(6, 12), (3, 5), (4, 5), (6, 12)]);
}

#[test]
fn galaxies_go_biggest_first_and_a_small_maker_is_a_dwarf() {
    let universe = crews::r().universe();
    assert_eq!(
        makers(&universe),
        [
            "anthropic",
            "openai",
            "alibaba",
            "google",
            "moonshot",
            "zhipu"
        ]
    );
    let dwarfs: Vec<bool> = universe.makers.iter().map(|m| m.dwarf).collect();
    assert_eq!(dwarfs, [false, false, false, true, true, true]);
    let shares: f64 = universe.makers.iter().map(|m| m.share).sum();
    assert!((shares - 1.0).abs() < 1e-9);
}

#[test]
fn a_galaxy_turns_with_its_makers_work_of_the_last_week_and_stands_still_without() {
    let r = crews::r().universe();
    let anthropic = &r.makers[0];
    let recent = f64::from(anthropic.last_7_days) / f64::from(anthropic.actions);
    assert!((anthropic.turn - std::f64::consts::TAU * recent / 60.0).abs() < 1e-12);
    let t = crews::t().universe();
    assert!(t.makers.iter().all(|m| m.turn == 0.0));
}

#[test]
fn each_galaxy_carries_its_makers_logo_or_its_colour_dot() {
    let universe = crews::r().universe();
    let marks: Vec<Option<LogoMark>> = universe.makers.iter().map(|m| m.logo.mark).collect();
    assert_eq!(
        marks,
        [
            Some(LogoMark::ClaudeCode),
            Some(LogoMark::OpenAi),
            Some(LogoMark::Qwen),
            Some(LogoMark::Antigravity),
            Some(LogoMark::Kimi),
            None,
        ]
    );
}

#[test]
fn each_star_carries_the_logo_of_whoever_serves_its_model() {
    let crew = crews::r();
    let universe = crew.universe();
    let mark = |id: &str| star(&crew, &universe, id).logo.mark;
    assert_eq!(mark("claude-haiku-5-5"), Some(LogoMark::ClaudeCode));
    assert_eq!(mark("gpt-6-luna"), Some(LogoMark::Codex));
    assert_eq!(mark("gemini-3.8-flash"), Some(LogoMark::Antigravity));
    // Through OpenCode: Ollama for a model it runs here, OpenCode for one its own cloud runs.
    assert_eq!(mark("ollama/qwen3-coder:30b"), Some(LogoMark::Ollama));
    assert_eq!(mark("opencode/kimi-k2.6"), Some(LogoMark::OpenCode));
    assert_eq!(mark("opencode/glm-5"), Some(LogoMark::OpenCode));
}

#[test]
fn a_galaxys_stars_go_most_used_first() {
    let crew = crews::r();
    let universe = crew.universe();
    let names: Vec<&str> = universe.makers[1]
        .stars
        .iter()
        .map(|s| crew.facts.models[universe.stars[*s].model].model.as_str())
        .collect();
    assert_eq!(
        names,
        ["gpt-6-luna", "gpt-6-codex", "gpt-6-astra", "gpt-6.1-sol"]
    );
}

#[test]
fn r_has_a_binary_of_6_luna_and_6_astra_and_three_tidal_streams() {
    let crew = crews::r();
    let universe = crew.universe();
    let luna = star(&crew, &universe, "gpt-6-luna");
    let astra = star(&crew, &universe, "gpt-6-astra");
    let (luna_pair, astra_pair) = (luna.binary.unwrap(), astra.binary.unwrap());
    assert_eq!(universe.stars[luna_pair.with].model, astra.model);
    assert!(!luna_pair.companion && astra_pair.companion);
    assert_eq!(luna_pair.runs, 18);
    let binaries = universe.stars.iter().filter(|s| s.binary.is_some()).count();
    assert_eq!(binaries, 2);
    let streams: Vec<[&str; 2]> = universe
        .streams
        .iter()
        .map(|s| {
            s.stars
                .map(|at| crew.facts.models[universe.stars[at].model].model.as_str())
        })
        .collect();
    assert_eq!(
        streams,
        [
            ["gpt-6-luna", "opencode/glm-5"],
            ["gpt-6-astra", "gemini-3.8-flash"],
            ["claude-opus-5-5", "opencode/glm-5"],
        ]
    );
}

#[test]
fn t_quiet_nine_days_has_three_red_giants_and_nothing_that_moves() {
    let crew = crews::t();
    let universe = crew.universe();
    let giants: Vec<&str> = universe
        .stars
        .iter()
        .filter(|s| s.kind == StarKind::RedGiant)
        .map(|s| crew.facts.models[s.model].model.as_str())
        .collect();
    assert_eq!(
        giants,
        [
            "gemini-3.8-flash",
            "claude-sonnet-5-5",
            "opencode/kimi-k2.6"
        ]
    );
    assert_eq!(kind_of(&crew, &universe, "opencode/glm-5"), StarKind::Star);
    assert!(!universe.moves());
    assert!((0..universe.makers.len()).all(|m| !universe.galaxy_moves(m)));
}

#[test]
fn s_has_no_rare_body() {
    let universe = crews::s().universe();
    assert!(
        universe
            .stars
            .iter()
            .all(|s| s.kind == StarKind::Star && s.supernova.is_none() && s.binary.is_none())
    );
    assert!(universe.streams.is_empty());
}

#[test]
fn every_rare_kind_comes_from_its_own_rule() {
    // A model new to you: under 30 actions, first used in the last 14 days.
    let mut crew = crews::r();
    assert_eq!(
        kind_of(&crew, &crew.universe(), "opencode/glm-5"),
        StarKind::Star
    );
    crew.model("opencode/glm-5").first_used = 13;
    assert_eq!(
        kind_of(&crew, &crew.universe(), "opencode/glm-5"),
        StarKind::Protostar
    );

    // Nothing it did merged, and some of it was sent back.
    crew.model("gpt-6.1-sol").merged = 0;
    crew.model("gpt-6.1-sol").sent_back = 4;
    assert_eq!(
        kind_of(&crew, &crew.universe(), "gpt-6.1-sol"),
        StarKind::BlackHole
    );

    // Its CLI failed in three sessions on one day.
    let universe = crew.universe();
    assert!(
        star(&crew, &universe, "claude-haiku-5-5")
            .supernova
            .is_none()
    );
    let end = crew.facts.end;
    crew.model("claude-haiku-5-5").cli_failures = (1..=3)
        .map(|hour| argonauts_core::behavior_world::CliFailure {
            at: end - Duration::days(4) - Duration::hours(hour),
        })
        .collect();
    let universe = crew.universe();
    let (count, day) = star(&crew, &universe, "claude-haiku-5-5")
        .supernova
        .unwrap();
    assert_eq!((count, day), (3, (end - Duration::days(4)).date()));

    // An older model of a line whose newer one did more this week.
    let crew = crews::r_with_older_haiku();
    let universe = crew.universe();
    let older = star(&crew, &universe, "claude-haiku-4-5");
    assert_eq!(older.kind, StarKind::WhiteDwarf);
    let (newer, came) = older.newer.unwrap();
    assert_eq!(
        crew.facts.models[universe.stars[newer].model].model,
        "claude-haiku-5-5"
    );
    assert_eq!(came, 20);
}

#[test]
fn a_star_breathes_fast_for_the_last_hour_and_slow_for_the_last_day() {
    let crew = crews::r();
    let universe = crew.universe();
    assert_eq!(star(&crew, &universe, "claude-haiku-5-5").pulse, Some(2.4));
    assert_eq!(
        star(&crew, &universe, "opencode/kimi-k2.6").pulse,
        Some(4.8)
    );
    assert_eq!(star(&crew, &universe, "ollama/gemma3:12b").pulse, None);
}

#[test]
fn comets_fly_for_the_latest_mixed_runs_and_meteors_for_the_last_ten_minutes() {
    let crew = crews::r();
    let universe = crew.universe();
    assert_eq!(universe.comets.len(), 6);
    let first = universe.comets[0];
    assert_eq!(
        (
            universe.makers[first.from].key.as_str(),
            universe.makers[first.to].key.as_str()
        ),
        ("anthropic", "openai")
    );
    let phases: Vec<f64> = universe.comets.iter().map(|c| c.phase).collect();
    assert!(phases.windows(2).all(|p| p[0] != p[1]));
    assert_eq!(universe.meteors.len(), 8);
    assert!(universe.meteors.windows(2).all(|m| m[0].age >= m[1].age));
    assert!(universe.meteors.iter().all(|m| m.age < 600.0));
    assert!(universe.moves());
}

#[test]
fn the_sky_keeps_each_earlier_month_with_a_star_per_action() {
    let universe = crews::t().universe();
    assert_eq!(universe.months.len(), 12);
    assert!(universe.months.iter().all(|m| m.stars == m.actions));
    let mut crew = crews::t();
    crew.facts.crew.earlier_months[0].actions = 12_000;
    let crowded = crew.universe();
    let stars: u32 = crowded.months.iter().map(|m| m.stars).sum();
    assert!((5_990..=6_010).contains(&stars), "{stars}");
}

#[test]
fn a_system_takes_the_six_busiest_behaviors_as_planets_and_the_rest_as_its_belt() {
    let crew = crews::r();
    let universe = crew.universe();
    let haiku = star(&crew, &universe, "claude-haiku-5-5");
    let facts = &crew.facts.models[haiku.model];
    let system = &haiku.system;
    // The behaviors busy enough to be planets, up to six: Haiku's five, as its record has it.
    let busy = facts
        .behaviors
        .iter()
        .filter(|b| !b.behavior.contains(": "))
        .filter(|b| b.count >= 3 && f64::from(b.count) >= 0.04 * f64::from(facts.actions))
        .count()
        .min(6);
    assert_eq!(busy, 5);
    assert_eq!(system.planets.len(), busy);
    for planet in &system.planets {
        assert!(planet.count >= 3);
        assert!(f64::from(planet.count) >= 0.04 * f64::from(facts.actions));
        assert!(!facts.behaviors[planet.behavior].behavior.contains(": "));
    }
    let counts: Vec<u32> = system.planets.iter().map(|p| p.count).collect();
    assert!(counts.windows(2).all(|c| c[0] >= c[1]));
    let on_planets: u32 = system
        .planets
        .iter()
        .map(|p| p.count + p.moons.iter().map(|m| m.count).sum::<u32>())
        .sum();
    assert_eq!(on_planets + system.belt, facts.actions);
    let lava: Vec<&str> = system
        .planets
        .iter()
        .filter(|p| p.lava)
        .map(|p| facts.behaviors[p.behavior].behavior.as_str())
        .collect();
    assert_eq!(lava, ["tried an edit that didn't apply"]);
    assert_eq!(system.rocks.len() as u32, system.belt);
    assert_eq!(system.flares.len(), 3);
    assert!(system.sunspots >= 1);
}

#[test]
fn a_finer_behavior_goes_round_its_planet_as_a_moon() {
    let crew = crews::s();
    let universe = crew.universe();
    let opus = star(&crew, &universe, "claude-opus-5-5");
    let facts = &crew.facts.models[opus.model];
    let with_moons: Vec<(&str, Vec<&str>)> = opus
        .system
        .planets
        .iter()
        .filter(|p| !p.moons.is_empty())
        .map(|p| {
            (
                facts.behaviors[p.behavior].behavior.as_str(),
                p.moons
                    .iter()
                    .map(|m| facts.behaviors[m.behavior].behavior.as_str())
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        with_moons,
        [(
            "edited a test file",
            vec!["edited a test file: added tests"]
        )]
    );
}

#[test]
fn planets_go_round_faster_the_more_recently_used_and_slower_further_out() {
    let crew = crews::r();
    let universe = crew.universe();
    let haiku = star(&crew, &universe, "claude-haiku-5-5");
    for (k, planet) in haiku.system.planets.iter().enumerate() {
        let inner = planet.climate.orbit_seconds();
        assert_eq!(planet.period, (inner * (1.0 + 0.45 * k as f64)).round());
    }
    assert_eq!(haiku.system.planets[0].climate, Climate::Rainforest);
    let t = crews::t();
    let universe = t.universe();
    let flash = star(&t, &universe, "gemini-3.8-flash");
    assert!(
        flash
            .system
            .planets
            .iter()
            .all(|p| p.climate != Climate::Rainforest)
    );
}

#[test]
fn the_bodies_go_star_by_star_then_comets_then_streams() {
    let universe = crews::r().universe();
    let mut seen = 0;
    for (at, star) in universe.stars.iter().enumerate() {
        for body in &star.bodies {
            assert_eq!(*body, seen);
            seen += 1;
            let sun = match universe.bodies[*body] {
                Body::Planet { star, .. }
                | Body::Moon { star, .. }
                | Body::Belt { star }
                | Body::Flare { star, .. } => star,
                other => panic!("{other:?} is no star's"),
            };
            assert_eq!(sun, at);
        }
    }
    let rest: Vec<Body> = universe.bodies[seen..].to_vec();
    assert_eq!(rest.len(), universe.comets.len() + universe.streams.len());
    assert!(matches!(rest[0], Body::Comet { comet: 0 }));
    assert!(matches!(rest.last(), Some(Body::Stream { stream: 2 })));
    for (at, body) in universe.bodies.iter().enumerate() {
        assert_eq!(universe.body_index(*body), Some(at as u32));
    }
}

#[test]
fn the_maps_actions_meet_their_models_through_the_names_the_app_keeps() {
    let crew = crews::r();
    let names = DisplayNames::default();
    let end = crew.facts.end;
    let action = |cli: &str, model: &str, minutes: i64| MapAction {
        x: 0.5,
        y: 0.5,
        group: 0,
        behavior: "read a source file".to_owned(),
        label: "read a source file".to_owned(),
        summary: None,
        agent: "build".to_owned(),
        model: model.to_owned(),
        cli: names.cli(cli),
        role: "build".to_owned(),
        workflow: String::new(),
        project: "example".to_owned(),
        run: None,
        at: end - Duration::minutes(minutes),
        nth: 1,
        of: 1,
        tool: "Read".to_owned(),
        input: "src/lib.rs".to_owned(),
        place: None,
        run_shows: None,
        failed: minutes == 3,
        outcome: None,
    };
    let opus = names.model("claude-code", Some("claude-opus-5-5")).unwrap();
    let map = BehaviorMap {
        actions: vec![
            action("claude-code", &opus, 3),
            action("codex", "Some Model Nobody Ran", 4),
            action("claude-code", &opus, 60 * 26),
        ],
        world: None,
        ..BehaviorMap::default()
    };
    let linked = star_actions(&map, &crew.facts, &names, &WallClock::pinned(end));
    let opus_at = crew
        .facts
        .models
        .iter()
        .position(|m| m.model == "claude-opus-5-5")
        .unwrap();
    assert_eq!(linked.len(), 2);
    assert_eq!(
        (linked[0].index, linked[0].model, linked[0].day),
        (0, opus_at, 0)
    );
    assert!(linked[0].failed && (linked[0].age - 180.0).abs() < 1e-6);
    assert_eq!((linked[1].index, linked[1].day), (2, 1));
}

#[test]
fn a_planets_climate_follows_the_maps_days() {
    let cases = [
        ((0, 4), Climate::Rainforest),
        ((3, 7), Climate::Rainforest),
        ((0, 3), Climate::Forest),
        ((6, 1), Climate::Forest),
        ((7, 0), Climate::Grassland),
        ((13, 0), Climate::Grassland),
        ((14, 0), Climate::Savanna),
        ((20, 0), Climate::Savanna),
        ((21, 0), Climate::Desert),
        ((40, 0), Climate::Desert),
    ];
    for ((last_used, days_in_last_7), climate) in cases {
        assert_eq!(
            Climate::of(last_used, days_in_last_7),
            climate,
            "last used {last_used} days ago, on {days_in_last_7} of the last 7"
        );
    }
}
