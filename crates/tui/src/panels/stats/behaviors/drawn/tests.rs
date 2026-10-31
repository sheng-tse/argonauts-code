//! The world as the tab draws it from the maps the app sends, checked against the world's own
//! pictures of the same made-up crews.

use std::sync::Arc;

use super::pointer::KEPT_GLOWS;
use super::*;
use crate::panels::stats::behaviors::crews;
use crate::world::map::fields::crews::Key;
use crate::world::map::motion::Moment;
use crate::world::{Pointed, Shape};

fn drawn(key: Key, look: Look) -> (Grown, MapLook) {
    let map = crews::map(key);
    let grown = Grown::new(&map, crews::seed(key));
    let shown = MapLook::new(&grown, &look, 1);
    (grown, shown)
}

fn standing(key: Key) -> Look {
    Look {
        flat: false,
        compared: None,
        nature: true,
        colours: crews::colours(key),
    }
}

/// Keeps a picture to look at where `ARGONAUTS_PICTURES_OUT` names a folder.
fn keep(name: &str, png: &[u8]) {
    if let Some(dir) = std::env::var_os("ARGONAUTS_PICTURES_OUT") {
        let file = std::path::Path::new(&dir).join(format!("{name}.png"));
        std::fs::write(&file, png).unwrap_or_else(|err| panic!("{}: {err}", file.display()));
    }
}

/// How many pixels are more than `by` apart in any channel.
fn apart(a: &[u8], b: &[u8], by: u8) -> usize {
    a.chunks(4)
        .zip(b.chunks(4))
        .filter(|(a, b)| a.iter().zip(b.iter()).any(|(a, b)| a.abs_diff(*b) > by))
        .count()
}

#[test]
fn the_scene_stacks_the_maps_pictures_as_the_map_lays_them() {
    use crate::world::raster::{Brush, Colour};
    let (_, shown) = drawn(Key::R, standing(Key::R));
    let ours = shown.scene(Moment::Still, 1).flatten();
    let weather = shown.weather.as_ref().expect("crew R's weather");
    let mut laid = shown.land.picture.clone();
    let white = Brush::new(Colour::WHITE);
    laid.draw(&weather.under_dots(Moment::Still), 0.0, 0.0, &white);
    laid.draw(&shown.dots.picture, 0.0, 0.0, &white);
    laid.draw(&weather.over_dots(Moment::Still), 0.0, 0.0, &white);
    keep("map-r", &ours.to_png());
    assert_eq!(
        (ours.width(), ours.height()),
        (laid.width() as usize, laid.height() as usize)
    );
    assert_eq!(apart(ours.pixels(), &laid.to_rgba(), 2), 0);
}

#[test]
fn what_the_pointer_finds_on_a_crews_map_says_what_nature_made_it_from() {
    let (grown, shown) = drawn(Key::R, standing(Key::R));
    let scene = shown.scene(Moment::Still, 1);
    let storm = grown
        .nature
        .features
        .iter()
        .position(|feature| matches!(feature.kind, crate::world::map::nature::Kind::Hurricane(_)))
        .expect("crew R has a hurricane");
    let mark = scene
        .hits
        .marks()
        .iter()
        .find(|mark| mark.pointed == Pointed::Feature(storm as u32))
        .expect("the hurricane answers the pointer");
    let Shape::Point { x, y } = mark.shape else {
        panic!("a hurricane is found round its eye: {:?}", mark.shape);
    };
    assert_eq!(scene.hits.find(x, y), Some(Pointed::Feature(storm as u32)));
    assert_eq!(
        grown
            .nature
            .line(storm, &grown.ground, &grown.facts, &mut |cli| cli
                .to_owned())
            .as_deref(),
        Some(
            "Hurricane, category 1 · 27 ‘tried an edit that didn't apply’ actions within one \
             hour, 8 days ago at 2:01 PM"
        )
    );
}

#[test]
fn comparing_a_group_hides_nature_and_the_wonders() {
    let look = Look {
        compared: Some(0),
        ..standing(Key::R)
    };
    let (_, shown) = drawn(Key::R, look);
    let scene = shown.scene(Moment::At(1.0), 1);
    assert!(shown.weather.is_none());
    assert!(scene.sprites().is_empty(), "nothing moves or stands");
    assert!(!scene.moves);
    assert!(
        scene
            .hits
            .marks()
            .iter()
            .all(|mark| matches!(mark.pointed, Pointed::Action(_)))
    );
}

#[test]
fn a_ring_goes_round_what_the_pointer_found_where_the_scene_drew_it() {
    let (_, shown) = drawn(Key::R, standing(Key::R));
    let scene = shown.scene(Moment::Still, 1);
    let marks = Marks::default();
    let dot = marks
        .ring(&scene, Pointed::Action(0))
        .expect("a dot's ring");
    let (x, y) = shown.dots.at[0];
    let (width, height) = (dot.picture.image.width(), dot.picture.image.height());
    assert_eq!(dot.depth, crate::world::Depth::Pointer);
    // The ring's box is centred on the dot.
    assert!((dot.at.0 as f32 + width as f32 / 2.0 - x).abs() <= 1.0);
    assert!((dot.at.1 as f32 + height as f32 / 2.0 - y).abs() <= 1.0);
    assert!(marks.ring(&scene, Pointed::Ground(None)).is_none());
    let lit = marks.lit(&scene, &[0, 1, 2]).expect("the lit dots' layer");
    assert_eq!(
        (lit.picture.image.width(), lit.picture.image.height()),
        (scene.width(), scene.height())
    );
    assert!(marks.lit(&scene, &[]).is_none());
}

/// Whether two pictures share their pixels, as one drawn once and kept does.
fn same_pixels(one: &crate::world::Picture, other: &crate::world::Picture) -> bool {
    std::ptr::eq(one.image.pixels(), other.image.pixels())
}

/// The screen asks for the marks at every step of the motion and every move of the pointer,
/// and the glow covers the whole map: each is kept while what it shows stays, and drawn again
/// when that changes. A glow lit lately is kept for when its row is lit again.
#[test]
fn the_glow_and_the_ring_are_drawn_again_only_when_they_change() {
    let (_, shown) = drawn(Key::R, standing(Key::R));
    let scene = shown.scene(Moment::Still, 1);
    let marks = Marks::default();
    let lit = marks.lit(&scene, &[0, 1, 2]).expect("the lit dots' layer");
    let again = marks.lit(&scene, &[2, 1, 0]).expect("the same dots lit");
    assert!(
        same_pixels(&lit.picture, &again.picture),
        "kept while it stays"
    );
    assert_eq!(lit.picture.key, again.picture.key);
    let other = marks.lit(&scene, &[3]).expect("another behavior's dots");
    assert_ne!(other.picture.key, lit.picture.key);
    assert!(!same_pixels(&other.picture, &lit.picture));
    let back = marks
        .lit(&scene, &[0, 1, 2])
        .expect("the first dots lit again");
    assert!(
        same_pixels(&back.picture, &lit.picture),
        "kept for when it is lit again"
    );
    let others: Vec<usize> = (10..10 + KEPT_GLOWS).collect();
    for at in &others {
        marks.lit(&scene, &[*at]);
    }
    let long_ago = marks
        .lit(&scene, &[0, 1, 2])
        .expect("the first dots lit once more");
    assert!(
        !same_pixels(&long_ago.picture, &lit.picture),
        "lit too long ago to keep"
    );

    let (_, flat) = drawn(
        Key::R,
        Look {
            flat: true,
            ..standing(Key::R)
        },
    );
    assert_ne!(
        flat.dots.at[3], shown.dots.at[3],
        "laid flat, the dots move"
    );
    let flat = flat.scene(Moment::Still, 1);
    let moved = marks.lit(&flat, &[3]).expect("the dots where they moved");
    assert_ne!(
        moved.picture.key, other.picture.key,
        "drawn where they stand now"
    );

    let ring = marks
        .ring(&scene, Pointed::Action(0))
        .expect("a dot's ring");
    let again = marks
        .ring(&scene, Pointed::Action(0))
        .expect("the same ring");
    assert!(same_pixels(&ring.picture, &again.picture));
    let next = marks
        .ring(&scene, Pointed::Action(1))
        .expect("the next dot's ring");
    assert_ne!(next.picture.key, ring.picture.key);
    assert_ne!(next.at, ring.at);
    marks.forget();
    let afresh = marks
        .ring(&scene, Pointed::Action(1))
        .expect("drawn afresh");
    assert!(!same_pixels(&afresh.picture, &next.picture), "let go of");
    assert_eq!(afresh.picture.key, next.picture.key);
}

fn planet_of(key: Key) -> PlanetLook {
    use crate::world::planet::PlanetFacts;
    let map = crews::map(key);
    let grown = Arc::new(Grown::new(&map, crews::seed(key)));
    let look = Look {
        flat: true,
        ..standing(key)
    };
    let flat = MapLook::new(&grown, &look, 2);
    let marks = flat
        .scene(Moment::Still, 2)
        .hits
        .marks()
        .iter()
        .filter(|mark| !matches!(mark.pointed, Pointed::Action(_)))
        .cloned()
        .collect();
    let facts = PlanetFacts::from_map(&map, |_| [120.0, 140.0, 200.0]);
    PlanetLook::new(&grown, &look, &facts, 3).with_marks(marks)
}

#[test]
fn the_planet_wraps_the_crews_map_and_answers_the_pointer() {
    use crate::world::planet::{self, Side};
    let mut shown = planet_of(Key::R);
    let facing = Facing {
        view: crate::world::View::stepped(0.0),
        side: Side::Day,
        size: (1152, 546),
        moment: planet::Moment {
            still: true,
            ..planet::Moment::default()
        },
        sky: None,
    };
    let scene = shown.scene(facing);
    keep("planet-r", &scene.flatten().to_png());
    assert_eq!((scene.width(), scene.height()), (1152, 546));
    assert!(!scene.moves, "motion held still");
    let dot = scene
        .hits
        .marks()
        .iter()
        .find_map(|mark| match (mark.pointed, &mark.shape) {
            (Pointed::Action(at), Shape::Point { x, y }) => Some((at, *x, *y)),
            _ => None,
        })
        .expect("a dot on the near side");
    assert_eq!(scene.hits.find(dot.1, dot.2), Some(Pointed::Action(dot.0)));
    // Off the globe the pointer finds the sky.
    assert_eq!(scene.hits.find(4.0, 4.0), Some(Pointed::Sky(None)));
}

/// Off the globe the pointer is on the universe's sky: it names the earlier month whose
/// cluster it rests in wherever the universe draws that cluster, says "The far stars …" at
/// four places near the sky's edges round the planet, and the globe still answers as itself.
#[test]
fn the_sky_behind_the_planet_says_what_the_universe_says_there() {
    use std::time::Duration;

    use crossterm::event::KeyCode;

    use crate::base::kit::Kit;
    use crate::base::theme::Theme;
    use crate::frame::view::map::MapPoint;
    use crate::panels::stats::behaviors::{BehaviorsTab, GroupColours};

    let theme = Theme::dark();
    let kit = Kit::new(&theme);
    let mut tab = BehaviorsTab::default();
    tab.accept(&crews::map(Key::R));
    tab.moment.still = true;
    tab.picture_placed = true;
    let colours = GroupColours::new(&kit, tab.map().expect("a map"), &[], &Default::default());
    let names = Default::default();
    let mut now = tab.moment.now;
    let mut level = |tab: &mut BehaviorsTab, key: char| {
        tab.key(KeyCode::Char(key));
        now += Duration::from_secs(3);
        let scene = tab.scene(&colours, &theme, &names, (18, 42), now);
        scene.expect("the level drawn")
    };
    let universe = level(&mut tab, '3');
    let clusters: Vec<(u32, f32, f32)> = universe
        .hits
        .marks()
        .iter()
        .filter_map(|mark| match (mark.pointed, &mark.shape) {
            (Pointed::Sky(Some(back)), Shape::Ellipse { x, y, .. }) => Some((back, *x, *y)),
            _ => None,
        })
        .collect();
    assert!(!clusters.is_empty(), "crew R's earlier months");
    let planet = level(&mut tab, '2');
    let at = |x: f32, y: f32| {
        Some(MapPoint {
            x,
            y,
            width: 1152.0,
            height: 546.0,
        })
    };
    let mut named = 0;
    for (back, x, y) in clusters {
        // A cluster behind the globe is hidden by it.
        if planet.hits.find(x, y) != Some(Pointed::Sky(None)) {
            continue;
        }
        tab.point(at(x, y));
        assert_eq!(tab.pointed(), Some(Pointed::Sky(Some(back))), "at {x}, {y}");
        named += 1;
    }
    assert!(named > 0, "an earlier month off the globe");
    let said = |tab: &BehaviorsTab| {
        tab.blocks(&kit, &colours, &names, 118, 40)
            .iter()
            .flat_map(|block| block.lines.iter())
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .find(|line: &String| line.trim_start().starts_with("The far stars · "))
    };
    for (x, y) in [(92.0, 362.0), (40.0, 40.0), (1100.0, 500.0), (1110.0, 60.0)] {
        tab.point(at(x, y));
        assert!(said(&tab).is_some(), "at {x}, {y}: {:?}", tab.pointed());
    }
    tab.point(at(576.0, 273.0));
    assert!(
        tab.pointed()
            .is_some_and(|found| !matches!(found, Pointed::Sky(_))),
        "the globe's middle: {:?}",
        tab.pointed()
    );
}

/// Each made-up crew's groups are the models its sky knows, as many actions each, and every
/// action names its model as the app's maps do, so each is a star in its maker's galaxy.
#[test]
fn every_made_up_crews_action_names_its_model() {
    use crate::base::format::WallClock;
    use crate::world::cosmos::star_actions;
    for key in [Key::R, Key::S, Key::T, Key::U] {
        let map = crews::map(key);
        let facts = map.world.as_deref().expect("the crew's facts");
        let clock = WallClock::pinned(facts.end);
        let linked = star_actions(&map, facts, &Default::default(), &clock);
        assert_eq!(linked.len(), map.actions.len(), "{key:?}");
    }
}

#[test]
fn the_tabs_universe_numbers_everything_as_the_worlds_cosmos_draws_it() {
    use crate::panels::stats::behaviors::universe::{Body, Universe};
    use crate::world::cosmos;
    use crate::world::cosmos::tests::crews;
    let mut moons = 0;
    for crew in [crews::r(), crews::s(), crews::t(), crews::u()] {
        let facts = crew.facts;
        let drawn = cosmos::Universe::new(&facts, &[], Vec::new());
        let ours = Universe::of(&facts);
        assert_eq!(ours.makers.len(), drawn.makers.len());
        assert_eq!(ours.stars.len(), drawn.stars.len());
        assert_eq!(ours.bodies.len(), drawn.bodies.len());
        for (at, star) in ours.stars.iter().enumerate() {
            assert_eq!(star.model, drawn.stars[at].model);
            assert_eq!(star.bodies, drawn.stars[at].bodies);
        }
        moons += ours
            .bodies
            .iter()
            .filter(|body| matches!(body, Body::Moon { .. }))
            .count();
    }
    assert!(moons > 0, "a crew's planet has a moon");
}

/// Every level of crew R's world as the tab hands it to the screen, each the cells' size. With
/// `ARGONAUTS_PICTURES_OUT` set each is kept there to look at.
#[test]
fn every_level_of_a_crews_world_is_drawn_at_the_cells_size() {
    use std::time::Duration;

    use crossterm::event::KeyCode;

    use crate::base::kit::Kit;
    use crate::base::theme::Theme;
    use crate::panels::stats::behaviors::{BehaviorsTab, GroupColours};

    let theme = Theme::dark();
    let kit = Kit::new(&theme);
    let mut tab = BehaviorsTab::default();
    tab.accept(&crews::map(Key::R));
    tab.moment.still = true;
    tab.picture_placed = true;
    let colours = GroupColours::new(&kit, tab.map().expect("a map"), &[], &Default::default());
    let names = Default::default();
    let mut now = tab.moment.now;
    for (keys, name) in [
        (&[][..], "map"),
        (&[KeyCode::Char('2')][..], "planet"),
        (&[KeyCode::Char('3')][..], "universe"),
        (&[KeyCode::Right, KeyCode::Enter][..], "galaxy"),
        (&[KeyCode::Right, KeyCode::Enter][..], "system"),
    ] {
        // Motion runs while the keys go, so each level is zoomed to.
        tab.moment.still = false;
        for key in keys {
            tab.moment.now = now;
            tab.key(*key);
        }
        // Halfway through the zoom to it, where one joins the levels: the universe's levels
        // stretched over their skies rather than drawn again.
        if tab.zoom(now).is_some() {
            let halfway = now + Duration::from_millis(375);
            assert!(tab.zoom(halfway).is_some(), "{name}: still zooming");
            let scene = tab
                .scene(&colours, &theme, &names, (18, 42), halfway)
                .unwrap_or_else(|| panic!("{name}: a zoom's frame"));
            assert!(scene.moves, "{name}: a zoom moves");
            if name != "planet" {
                assert!(!scene.scaled().is_empty(), "{name}: stretched pictures");
            }
            keep(&format!("zoom-r-{name}"), &scene.flatten().to_png());
        }
        // Past any zoom's last frame, held still.
        tab.moment.still = true;
        now += Duration::from_secs(3);
        let scene = tab
            .scene(&colours, &theme, &names, (18, 42), now)
            .unwrap_or_else(|| panic!("{name}: a scene"));
        assert_eq!((scene.width(), scene.height()), (1152, 546), "{name}");
        keep(&format!("level-r-{name}"), &scene.flatten().to_png());
    }
}

/// Coming down from the planet, the globe unrolls into the flat map and the land then stands
/// up over the zoom's last frames, so the map arrives standing instead of cutting to it.
#[test]
fn the_land_stands_up_as_a_zoom_from_the_planet_arrives_on_the_map() {
    use std::time::Duration;

    use crossterm::event::KeyCode;

    use crate::base::kit::Kit;
    use crate::base::theme::Theme;
    use crate::panels::stats::behaviors::{BehaviorsTab, GroupColours};

    let theme = Theme::dark();
    let kit = Kit::new(&theme);
    let mut tab = BehaviorsTab::default();
    tab.accept(&crews::map(Key::R));
    tab.picture_placed = true;
    let colours = GroupColours::new(&kit, tab.map().expect("a map"), &[], &Default::default());
    let names = Default::default();
    tab.moment.still = false;
    tab.key(KeyCode::Char('2'));
    let left = tab.moment.now + Duration::from_secs(3);
    tab.moment.now = left;
    tab.key(KeyCode::Char('1'));
    let frame = |tab: &BehaviorsTab, at: u64| {
        let when = left + Duration::from_millis(125 * at);
        let scene = tab.scene(&colours, &theme, &names, (18, 42), when);
        scene.expect("a frame").flatten()
    };
    // The zoom's last three frames, as the land stands up into the map.
    let last = u64::from(super::super::level::ZOOM_FRAMES);
    let unrolled = frame(&tab, last - 3);
    let rising = frame(&tab, last - 2);
    let risen = frame(&tab, last - 1);
    tab.moment.still = true;
    let arrived = frame(&tab, 40);
    keep("stand-r-unrolled", &unrolled.to_png());
    keep("stand-r-rising", &rising.to_png());
    keep("stand-r-risen", &risen.to_png());
    keep("stand-r-arrived", &arrived.to_png());
    let from_arrived = |frame: &crate::world::Image| apart(frame.pixels(), arrived.pixels(), 24);
    let (unrolled, rising, risen) = (
        from_arrived(&unrolled),
        from_arrived(&rising),
        from_arrived(&risen),
    );
    assert!(
        risen < rising && rising < unrolled,
        "pixels apart from the standing map: {unrolled} unrolled, {rising} rising, {risen} risen"
    );
}

/// A screen reader hears the map, the planet and the universe in a few words, and no list of
/// the kinds of nature, land or wonders the world holds: each place says its own line when the
/// pointer rests on it.
#[test]
fn a_screen_reader_hears_the_crews_planet_and_no_list_of_kinds() {
    let lines = crate::panels::stats::behaviors::spoken(&crews::map(Key::R), &Default::default());
    let find = |start: &str| {
        lines
            .iter()
            .find(|line| line.starts_with(start))
            .unwrap_or_else(|| panic!("{start}: {lines:#?}"))
            .clone()
    };
    assert!(find("Map: 961 actions").contains("on the list never used"));
    // Land on less than 30% of the map makes an ocean world.
    find("Planet: an ocean world; 73% of the map is sea; ");
    let said = lines.join("\n").to_lowercase();
    for kind in [
        "nature:",
        "hurricane",
        "volcano",
        "earthquake",
        "rainforest",
        "grassland",
        "savanna",
        "desert",
        "wonder",
        "ice cap",
        "iceberg",
        "out at sea",
    ] {
        assert!(!said.contains(kind), "{kind}: {lines:#?}");
    }
}

/// An earthquake the pointer rests on names the CLI that crashed by its name, as the app's
/// facts give its id.
#[test]
fn a_screen_reader_hears_which_cli_crashed_where_the_pointer_rests() {
    use crate::world::Pointed;

    use crate::world::map::nature::Kind;

    let map = crews::map(Key::S);
    let quake = Grown::new(&map, SEED)
        .nature
        .features
        .iter()
        .position(|feature| matches!(&feature.kind, Kind::Rift(rift) if rift.shaking()))
        .expect("an earthquake");
    let mut tab = super::super::BehaviorsTab::default();
    tab.accept(&map);
    tab.picture_placed = true;
    tab.moment.still = true;
    let names = Default::default();
    let theme = crate::base::theme::Theme::dark();
    let kit = crate::base::kit::Kit::new(&theme);
    let colours = super::super::GroupColours::new(&kit, &map, &[], &names);
    tab.scene(
        &colours,
        &theme,
        &Default::default(),
        (18, 42),
        std::time::Instant::now(),
    )
    .expect("the map drawn");
    tab.pointed = Some(Pointed::Feature(u32::try_from(quake).unwrap()));
    let lines = tab.spoken_here(&names);
    assert!(
        lines.iter().any(|line| {
            line.starts_with("Earthquake · Claude Code crashed 12 hours ago, ending a session on ")
        }),
        "{lines:#?}"
    );
    tab.pointed = None;
    assert!(
        !tab.spoken_here(&names)
            .iter()
            .any(|line| line.contains("Earthquake"))
    );
}

/// A screen reader steps through every place on the world, west to east: each behavior's
/// land, each mark of nature and each wonder says the line the pointer would show there, with
/// whole names and no logo marks.
#[test]
fn a_screen_reader_steps_to_every_place_and_hears_its_own_line() {
    let names = Default::default();
    for key in [Key::R, Key::S, Key::T, Key::U] {
        let map = crews::map(key);
        let grown = Grown::new(&map, SEED);
        let places = super::super::places(&map, &names);
        let marks = grown.nature.features.len() + grown.nature.shoals.len() + grown.wonders.len();
        assert!(places.len() >= marks, "{key:?}: {places:#?}");
        for place in &places {
            let first = place.first().expect("a line");
            assert!(
                first.chars().next().is_some_and(char::is_uppercase),
                "{key:?}: {place:?}"
            );
            assert!(place.iter().all(|line| !line.contains('{')), "{place:?}");
        }
        for wonder in &grown.wonders {
            let name = grown.risen[wonder.index].wonder.name();
            assert!(
                places
                    .iter()
                    .any(|place| place[0].starts_with(name) && place.len() == 2),
                "{key:?}: {name} and its rule: {places:#?}"
            );
        }
    }
    let places = super::super::places(&crews::map(Key::S), &names);
    assert!(
        places.iter().any(|place| place[0]
            .starts_with("Earthquake · Claude Code crashed 12 hours ago, ending a session on ")),
        "{places:#?}"
    );
}

/// Crew U's thunderstorms never flash more than 3 times in any second between them, on the map
/// or on the planet, whatever the sessions each stopped.
#[test]
fn crew_us_storms_never_flash_more_than_three_times_a_second() {
    use crate::world::map::motion::{FLASHES_A_SECOND, most_flashes_in_a_second};

    let (grown, shown) = drawn(Key::U, standing(Key::U));
    let storms = crate::world::map::weather::thunderstorms(&grown.nature);
    assert!(storms.len() >= 2, "U's storms stopped {storms:?} sessions");
    let schedule = crate::world::map::motion::Flashes::for_storms(&storms);
    assert!(most_flashes_in_a_second(&schedule) <= FLASHES_A_SECOND);
    // Each storm the map flies, and each on the planet, keeps its place in that schedule.
    let on_map = shown.weather.as_ref().expect("U's weather").lightning();
    let on_planet = planet_of(Key::U).lightning();
    assert!(!on_map.is_empty() && !on_planet.is_empty());
    for flashes in on_map.iter().chain(&on_planet) {
        assert!(schedule.contains(flashes), "{flashes:?} in {schedule:?}");
    }
    assert!(most_flashes_in_a_second(&on_map) <= FLASHES_A_SECOND);
    assert!(most_flashes_in_a_second(&on_planet) <= FLASHES_A_SECOND);
}

/// Each made-up crew's headline splits its sessions into its runs and the rest, as the headline
/// counts them.
#[test]
fn each_crews_headline_counts_its_runs_and_the_rest_of_its_sessions() {
    for (key, sessions) in [
        (Key::R, "in 19 runs and 49 conversations across 6 projects"),
        (Key::S, "in 31 runs and 48 conversations across 4 projects"),
        (Key::T, "in 8 runs and 47 conversations across 3 projects"),
        (Key::U, "in 20 runs and 50 conversations across 6 projects"),
    ] {
        let headline = super::super::summary::headline_words(&crews::map(key));
        assert!(headline.contains(sessions), "{key:?}: {headline}");
    }
}

/// A step of the same look asked for while the last is still drawn is let go, so a slow
/// drawing drops steps rather than falling behind; the scene of the step drawn lands.
#[test]
fn a_step_asked_while_the_last_is_drawn_is_let_go() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use crate::world::deliver::worker::Worker;

    let drawings = Drawings::new(Worker::threaded());
    let (release, held) = mpsc::channel::<()>();
    drawings.want(1, Some(0), move |_, _| {
        held.recv().ok()?;
        Some(Made {
            scene: crate::world::Scene::new(4, 4),
            world: None,
            globe: None,
        })
    });
    assert!(drawings.busy());
    let asked = Arc::new(AtomicBool::new(false));
    let second = asked.clone();
    drawings.want(1, Some(1), move |_, _| {
        second.store(true, Ordering::SeqCst);
        None
    });
    release.send(()).expect("the first step waits");
    let deadline = argonauts_testkit::pty::deadline::after(Duration::from_secs(20));
    while drawings.busy() || drawings.newest().is_none() {
        assert!(Instant::now() < deadline, "the first step lands");
        std::thread::yield_now();
    }
    assert_eq!(drawings.newest().map(|drawn| drawn.tick), Some(Some(0)));
    assert!(!asked.load(Ordering::SeqCst), "the second step was let go");
}

/// The world leaving the screen stops the drawing under way at once: nothing is busy, so the
/// screen waits on no picture, and the drawing gives up when it next asks.
#[test]
fn leaving_stops_the_drawing_under_way() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use crate::world::deliver::worker::Worker;

    let drawings = Drawings::new(Worker::threaded());
    let gave_up = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicBool::new(false));
    let (seen, starting) = (gave_up.clone(), started.clone());
    drawings.want(1, Some(0), move |_, cancel| {
        starting.store(true, Ordering::SeqCst);
        let until = Instant::now() + Duration::from_secs(20);
        while !cancel.cancelled() && Instant::now() < until {
            std::thread::yield_now();
        }
        seen.store(cancel.cancelled(), Ordering::SeqCst);
        None
    });
    assert!(drawings.busy());
    let deadline = argonauts_testkit::pty::deadline::after(Duration::from_secs(20));
    while !started.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "the drawing starts");
        std::thread::yield_now();
    }
    let left = Instant::now();
    drawings.leave();
    assert!(!drawings.busy());
    assert!(left.elapsed() < Duration::from_millis(5));
    let deadline = argonauts_testkit::pty::deadline::after(Duration::from_secs(20));
    while !gave_up.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "the drawing gives up");
        std::thread::yield_now();
    }
    assert!(drawings.newest().is_none());
}

/// A drawing given up stops growing its world and drawing its look between their steps, and
/// keeps nothing half made, so the next job asked for grows the world whole.
#[test]
fn a_drawing_given_up_stops_between_steps_and_keeps_nothing() {
    use std::cell::Cell;

    let map = crews::map(Key::R);
    let seed = crews::seed(Key::R);
    let asked = Cell::new(0);
    let stop = || {
        asked.set(asked.get() + 1);
        true
    };
    assert!(Grown::unless(&map, seed, &stop).is_none());
    assert_eq!(asked.get(), 1, "it stops after the step under way");

    let mut kept = Kept::default();
    assert!(
        kept.grown(1, || Grown::unless(&map, seed, &|| true))
            .is_none()
    );
    let grown = kept
        .grown(1, || Grown::unless(&map, seed, &|| false))
        .expect("grown whole");
    assert!(
        kept.look(2, || MapLook::unless(&grown, &standing(Key::R), 2, &|| {
            true
        }))
        .is_none()
    );
    let look = kept.look(2, || {
        MapLook::unless(&grown, &standing(Key::R), 2, &|| false)
    });
    assert!(look.is_some());
}
