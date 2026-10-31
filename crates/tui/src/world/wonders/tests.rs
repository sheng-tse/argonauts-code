use argonauts_core::behaviors::behavior_world::Wonder;

use super::*;
use crate::world::map::fields::crews::Key;
use crate::world::map::nature::crews::{raised, world};
use crate::world::map::weather::{FlatLayers, Keep};

fn placed(key: Key) -> Vec<Placed> {
    let world = world(key);
    let sunk = sunk(&world.ground, &world.facts, &raised(key));
    let layers = FlatLayers::new(&world.ground, &world.nature, Keep::All, sunk);
    place(&world.ground, &world.nature, &layers, &raised(key))
}

#[test]
fn every_wonder_draws_its_own_picture_at_the_maps_size_and_larger() {
    use crate::world::map::relief::cells::{compare_means, mean_colours};
    let (tile_w, tile_h) = (72_u32, 84_u32);
    let mut sheet = Canvas::new(tile_w * 8, tile_h * 4);
    for (i, wonder) in Wonder::ALL.iter().enumerate() {
        let (column, row) = ((i % 8) as u32, (i / 8) as u32 * 2);
        for (k, size) in [(0, 34.0), (1, 60.0)] {
            let x = f64::from(column * tile_w) + 36.0;
            let y = f64::from((row + k) * tile_h) + 42.0;
            draw(&mut sheet, *wonder, (x, y), size, key_light());
        }
        let alone = sprite(*wonder, 34.0, key_light());
        let solid = alone
            .canvas
            .to_rgba()
            .chunks(4)
            .filter(|p| p[3] > 128)
            .count();
        assert!(solid > 150, "{wonder:?} draws {solid} solid pixels");
    }
    let means = mean_colours(
        (tile_w as usize * 8, tile_h as usize * 4),
        (18, 42),
        |x, y| {
            let [r, g, b, _] = sheet.pixel(x as u32, y as u32);
            [r, g, b]
        },
    );
    compare_means(
        "wonders-sheet",
        &means,
        include_str!("golden/wonders-sheet.txt"),
        || sheet.to_png(),
    );
}

#[test]
fn a_wonders_box_holds_its_picture_from_its_ground_point() {
    let pyramid = bounds(Wonder::Pyramid, ON_MAP);
    assert!(pyramid.x0 < -12.0 && pyramid.x1 > 12.0, "{pyramid:?}");
    assert!(
        pyramid.y0 < -15.0 && pyramid.y1 > 0.0 && pyramid.y1 < 12.0,
        "{pyramid:?}"
    );
    let atlantis = bounds(Wonder::Atlantis, ATLANTIS_ON_MAP);
    assert!(atlantis.x1 - atlantis.x0 >= 72.0, "{atlantis:?}");
}

#[test]
fn each_wonder_stands_on_ground_of_its_kind_or_a_kindred_one() {
    for key in Key::ALL {
        let placed = placed(key);
        assert_eq!(
            placed.len(),
            raised(key).len(),
            "{key:?}: every wonder found a place"
        );
        for wonder in &placed {
            let own = Terrain::of(wonder.wonder);
            let kindred = own.kin().contains(&wonder.terrain);
            assert!(
                wonder.terrain == own || kindred,
                "{key:?} {:?} stands on {:?}",
                wonder.wonder,
                wonder.terrain
            );
        }
    }
}

#[test]
fn no_dot_lies_under_any_wonder() {
    for key in Key::ALL {
        let ground = &world(key).ground;
        for wonder in placed(key) {
            let b = wonder.bounds;
            let under = ground
                .dots
                .iter()
                .filter(|&&(x, y)| {
                    let (x, y) = (f64::from(x), f64::from(y));
                    x > b.x0 - 5.5 && x < b.x1 + 5.5 && y > b.y0 - 5.5 && y < b.y1 + 5.5
                })
                .count();
            assert_eq!(under, 0, "{key:?} {:?}", wonder.wonder);
        }
    }
}

#[test]
fn atlantis_stands_at_sea_where_its_behavior_stood() {
    let world = world(Key::U);
    let placed = placed(Key::U);
    let atlantis = placed
        .iter()
        .find(|wonder| wonder.wonder == Wonder::Atlantis)
        .expect("crew U raised Atlantis");
    let sunk = world
        .ground
        .facts
        .behaviors
        .iter()
        .position(|days| days.name == "changed git state");
    assert_eq!(Some(atlantis.behavior), sunk);
    assert!(matches!(atlantis.terrain, Terrain::Sea | Terrain::Strait));
    assert!(
        atlantis.moved <= 110.0,
        "{} px from its place",
        atlantis.moved
    );
    assert!(atlantis.size >= 72.0);
}

#[test]
fn a_gas_giant_raises_no_wonder_on_its_map() {
    use crate::world::map::fields::Ground;
    use crate::world::map::fields::crews::Crew;
    use crate::world::map::nature::Nature;
    let mut facts = Crew::get(Key::R).facts();
    facts.dots.truncate(30);
    let ground = Ground::grow(facts);
    let nature = Nature::default();
    let layers = FlatLayers::new(&ground, &nature, Keep::All, None);
    assert!(place(&ground, &nature, &layers, &raised(Key::R)).is_empty());
}

#[test]
fn atlantis_rises_over_the_land_that_sank_and_its_atoll_gives_way() {
    use crate::world::map::nature::{GroundMarks, Kind};
    let world = world(Key::U);
    let sunk = sunk(&world.ground, &world.facts, &raised(Key::U)).expect("crew U raised Atlantis");
    assert_eq!(
        world.ground.facts.behaviors[sunk.behavior].name,
        "changed git state"
    );
    assert_eq!(sunk.before, 130);
    assert!(sunk_or_none(Key::R).is_none());
    let atoll = world
        .nature
        .features
        .iter()
        .find(|f| matches!(f.kind, Kind::Atoll) && f.behavior == sunk.behavior)
        .expect("its atoll was found");
    let pixel = |marks: &GroundMarks| {
        let laid = marks.laid().expect("nature shows");
        let at = ((atoll.y.round() as usize) * crate::world::map::fields::WIDTH
            + atoll.x.round() as usize)
            * 4;
        laid[at + 3]
    };
    let with_atoll = GroundMarks::new(&world.ground, &world.nature, true, None);
    let given_way = GroundMarks::new(&world.ground, &world.nature, true, Some(sunk));
    assert!(
        pixel(&with_atoll) > 200,
        "the atoll stands without Atlantis"
    );
    assert_eq!(pixel(&given_way), 0, "and gives way to it");
    let bank = given_way.marks().sunk.expect("a bank under the water");
    let (cx, cy) = world.ground.centre[sunk.behavior];
    let (k, colour) = bank(cx.round(), cy.round()).expect("the bank at its middle");
    assert!(k > 0.6 && colour[1] > 80.0, "{k} {colour:?}");
    assert!(bank(cx + 200.0, cy).is_none(), "nothing far from it");
}

fn sunk_or_none(key: Key) -> Option<crate::world::map::nature::Sunk> {
    let world = world(key);
    sunk(&world.ground, &world.facts, &raised(key))
}
