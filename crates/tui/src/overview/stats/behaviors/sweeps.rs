//! The world's words swept whole, for each made-up crew: every line any place says on any
//! level starts with a capital; every line of the universe fits its two rows with a logo at
//! each of its marks; every rare body says what it is when the pointer rests on it; and
//! nothing beside the picture lists the kinds of nature, land, wonder or body the world holds.

use argonauts_core::behavior_map::BehaviorMap;
use argonauts_core::behavior_world::{ModelFacts, NewerModel, Wonder, WorldFacts};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::logos::Logo;
use unicode_width::UnicodeWidthStr;

use super::drawn::{Grown, SEED};
use super::hover::{self, LINE_CELLS, LOGO, Reading, Said, StarKind};
use super::universe::{Body, Universe};
use super::{BehaviorsTab, GroupColours, Level, crews, places};
use crate::kit::Kit;
use crate::theme::{Glyphs, Theme};
use crate::world::Pointed;
use crate::world::map::climate::Climate;
use crate::world::map::fields::crews::Key;

const CREWS: [Key; 4] = [Key::R, Key::S, Key::T, Key::U];

/// The universe's levels a crew's world has: the universe, each maker's galaxy and each
/// model's system.
fn levels(universe: &Universe) -> Vec<Level> {
    let mut levels = vec![Level::Universe];
    levels.extend((0..universe.makers.len()).map(Level::Galaxy));
    levels.extend((0..universe.stars.len()).map(Level::System));
    levels
}

/// Everything the pointer can rest on in the universe, at any level.
fn targets(universe: &Universe) -> Vec<Pointed> {
    let index = |at: usize| u32::try_from(at).unwrap_or(u32::MAX);
    let mut targets = vec![Pointed::Home, Pointed::Sky(None)];
    targets.extend((0..24).map(|month| Pointed::Sky(Some(month))));
    targets.extend((0..universe.makers.len()).map(|at| Pointed::Galaxy(index(at))));
    targets.extend((0..universe.stars.len()).map(|at| Pointed::Star(index(at))));
    targets.extend((0..universe.bodies.len()).map(|at| Pointed::Body(index(at))));
    targets
}

/// Every line the universe of `map` says, at each level, for each thing it says one for.
fn cosmos_said(map: &BehaviorMap, names: &DisplayNames) -> Vec<(Level, Pointed, Vec<Said>)> {
    let world = map.world.as_deref().expect("the crew's facts");
    said_in(map, world, &Universe::of(world), names)
}

/// [`cosmos_said`] for `universe` made of `world`, which may not be the map's own.
fn said_in(
    map: &BehaviorMap,
    world: &WorldFacts,
    universe: &Universe,
    names: &DisplayNames,
) -> Vec<(Level, Pointed, Vec<Said>)> {
    let reading = Reading {
        world,
        actions: u32::try_from(map.actions.len()).unwrap_or(u32::MAX),
        unlabelled: u32::try_from(map.unlabelled).unwrap_or(u32::MAX),
        clock: crate::format::WallClock::pinned(world.end),
        names,
    };
    let named = |_: &Logo, model: &ModelFacts| {
        names
            .model(&model.agent, Some(&model.model))
            .unwrap_or_else(|| model.model.clone())
    };
    let mut said = Vec::new();
    for level in levels(universe) {
        for target in targets(universe) {
            if let Some(lines) =
                hover::cosmos_lines(target, universe, world, &reading, level, &named)
            {
                said.push((level, target, lines));
            }
        }
    }
    said
}

/// Whether `line` starts with a capital or a count. A line that opens with a logo opens with
/// the name after it, which keeps its own spelling.
fn starts_with_a_capital(line: &str) -> bool {
    if line.starts_with(LOGO) {
        return true;
    }
    line.chars()
        .find(|c| c.is_alphanumeric())
        .is_none_or(|first| first.is_ascii_digit() || first.is_uppercase())
}

#[test]
fn every_line_any_place_says_starts_with_a_capital() {
    let names = DisplayNames::default();
    for key in CREWS {
        let map = crews::map(key);
        for place in places(&map, &names) {
            for line in &place {
                assert!(starts_with_a_capital(line), "{key:?}: {line:?}");
            }
        }
        for (level, target, lines) in cosmos_said(&map, &names) {
            for said in &lines {
                assert!(
                    starts_with_a_capital(&said.text),
                    "{key:?} {level:?} {target:?}: {:?}",
                    said.text
                );
            }
        }
    }
}

#[test]
fn every_line_of_the_universe_fits_two_rows_with_a_logo_at_each_mark() {
    let names = DisplayNames::default();
    for key in CREWS {
        for (level, target, lines) in cosmos_said(&crews::map(key), &names) {
            assert!(lines.len() <= 2, "{key:?} {level:?} {target:?}: {lines:#?}");
            for said in &lines {
                let marks = said.text.matches(LOGO).count();
                assert_eq!(
                    marks,
                    said.logos.len(),
                    "{key:?} {level:?} {target:?}: a logo for each mark in {:?}",
                    said.text
                );
                // A logo takes two cells where its mark stands.
                let drawn = said.text.replace(LOGO, "  ");
                assert!(
                    drawn.width() <= LINE_CELLS,
                    "{key:?} {level:?} {target:?}: {} cells, {drawn:?}",
                    drawn.width()
                );
            }
        }
    }
}

/// What a rare body's first row must say, by what it is.
fn rare_words(universe: &Universe, world: &WorldFacts, target: Pointed) -> Vec<&'static str> {
    let at = |index: u32| usize::try_from(index).unwrap_or(usize::MAX);
    match target {
        Pointed::Star(index) => {
            let Some(star) = universe.stars.get(at(index)) else {
                return Vec::new();
            };
            let mut words = Vec::new();
            match world
                .models
                .get(star.model)
                .map(|model| StarKind::of(model, world))
            {
                Some(StarKind::Protostar) => words.push("protostar"),
                Some(StarKind::RedGiant) => words.push("red giant"),
                Some(StarKind::WhiteDwarf) => words.push("white dwarf"),
                Some(StarKind::BlackHole) => words.push("black hole"),
                _ => {}
            }
            if star.binary.is_some() {
                words.push("in a binary with");
            }
            if star.supernova {
                words.push("supernova remnant");
            }
            words
        }
        Pointed::Body(index) => match universe.bodies.get(at(index)) {
            Some(Body::Flare { .. }) => vec!["Flare"],
            Some(Body::Comet { .. }) => vec!["Comet"],
            Some(Body::Stream { .. }) => vec!["Tidal stream"],
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// Crew R's world with a model of each rare kind the crews don't hold raised in it: a black
/// hole, a protostar and a white dwarf, and a star in a supernova's remnant.
fn raised() -> (BehaviorMap, WorldFacts, Universe) {
    let map = crews::map(Key::R);
    let mut world = map.world.as_deref().expect("the crew's facts").clone();
    let models = &mut world.models;
    models[0].merged = 0;
    models[0].sent_back = 3;
    models[0].actions = models[0].actions.max(20);
    models[1].actions = 12;
    models[1].merged = 1;
    models[1].first_used = 3;
    models[3].last_7_days = models[2].last_7_days + 5;
    models[2].newer = Some(NewerModel {
        model: models[3].model.clone(),
        came: 5,
    });
    let mut universe = Universe::of(&world);
    universe.stars[4].supernova = true;
    (map, world, universe)
}

#[test]
fn every_rare_body_names_itself_where_the_pointer_rests_on_it() {
    let names = DisplayNames::default();
    let mut found = Vec::new();
    let mut check = |case: &str, map: &BehaviorMap, world: &WorldFacts, universe: &Universe| {
        for (level, target, lines) in said_in(map, world, universe, &names) {
            for word in rare_words(universe, world, target) {
                found.push(word);
                assert!(
                    lines[0].text.contains(word),
                    "{case} {level:?} {target:?}: {word:?} on the first row of {lines:#?}"
                );
            }
        }
    };
    for key in CREWS {
        let map = crews::map(key);
        let world = map.world.as_deref().expect("the crew's facts");
        check(&format!("{key:?}"), &map, world, &Universe::of(world));
    }
    let (map, world, universe) = raised();
    check("raised", &map, &world, &universe);
    for word in [
        "red giant",
        "black hole",
        "protostar",
        "white dwarf",
        "in a binary with",
        "supernova remnant",
        "Flare",
        "Tidal stream",
    ] {
        assert!(found.contains(&word), "{word} is swept: {found:?}");
    }
}

/// The words that would list what the world holds: each kind of nature the crews have, each
/// kind of land and ice, each wonder, and each rare kind of star or body.
fn kinds() -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    for key in CREWS {
        let grown = Grown::new(&crews::map(key), SEED);
        words.extend(
            grown
                .nature
                .features
                .iter()
                .map(|f| f.kind.word().to_owned()),
        );
    }
    words.extend(Climate::ALL.iter().map(|climate| climate.name().to_owned()));
    words.extend(Wonder::ALL.iter().map(|wonder| wonder.name().to_owned()));
    for word in [
        "shoal",
        "ice cap",
        "iceberg",
        "south ice",
        "aurora",
        "protostar",
        "red giant",
        "white dwarf",
        "black hole",
        "supernova",
        "binary",
        "comet",
        "flare",
        "tidal stream",
    ] {
        words.push(word.to_owned());
    }
    words.sort();
    words.dedup();
    words
}

/// The tab's rows beside and under the picture, 118 wide.
fn shown(tab: &BehaviorsTab, pictures: bool) -> String {
    let mut tab = tab.clone();
    tab.picture_placed = pictures;
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme.mascot_pictures = pictures;
    theme.logo_pictures = pictures;
    let kit = Kit::new(&theme);
    let map = tab.map().expect("a map");
    let colours = GroupColours::new(&kit, map, &[], &DisplayNames::default());
    tab.blocks(&kit, &colours, &DisplayNames::default(), 118, 40)
        .iter()
        .flat_map(|block| block.lines.iter())
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase()
}

/// Whether `text` holds `word` as a word of its own.
fn holds_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

#[test]
fn nothing_beside_the_picture_lists_a_kind_on_any_level() {
    let kinds = kinds();
    for key in CREWS {
        let map = crews::map(key);
        let mut tab = BehaviorsTab::default();
        tab.accept(&map);
        tab.moment.still = true;
        tab.moment.skips_frames = true;
        let universe = Universe::of(map.world.as_deref().expect("the crew's facts"));
        let mut every = vec![Level::Map, Level::Planet];
        every.extend(levels(&universe));
        for level in every {
            tab.go(level);
            for pictures in [true, false] {
                let rows = shown(&tab, pictures);
                for kind in &kinds {
                    assert!(
                        !holds_word(&rows, &kind.to_lowercase()),
                        "{key:?} {level:?}, pictures {pictures}: {kind:?} in\n{rows}"
                    );
                }
            }
        }
    }
}
