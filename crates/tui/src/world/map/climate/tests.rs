use super::*;
use crate::world::map::fields::crews::{Crew, Key};

#[test]
fn a_behaviors_climate_dries_as_it_lies_unused() {
    assert_eq!(Climate::of(Some(0), 4), Climate::Rainforest);
    assert_eq!(Climate::of(Some(0), 3), Climate::Forest);
    assert_eq!(Climate::of(Some(6), 1), Climate::Forest);
    assert_eq!(Climate::of(Some(7), 0), Climate::Grassland);
    assert_eq!(Climate::of(Some(13), 0), Climate::Grassland);
    assert_eq!(Climate::of(Some(14), 0), Climate::Savanna);
    assert_eq!(Climate::of(Some(20), 0), Climate::Savanna);
    assert_eq!(Climate::of(Some(21), 0), Climate::Desert);
    assert_eq!(Climate::of(None, 0), Climate::Desert);
}

#[test]
fn the_climate_field_reads_each_climate_between_the_halves() {
    let read: Vec<Climate> = [0.49, 0.5, 1.49, 1.5, 2.5, 3.49, 3.5, 9.0]
        .into_iter()
        .map(Climate::of_field)
        .collect();
    assert_eq!(
        read,
        [
            Climate::Desert,
            Climate::Savanna,
            Climate::Savanna,
            Climate::Grassland,
            Climate::Forest,
            Climate::Forest,
            Climate::Rainforest,
            Climate::Rainforest,
        ]
    );
}

#[test]
fn a_small_fresh_behavior_among_unused_ones_lies_in_dry_land() {
    let crew = Crew::get(Key::U);
    let ground = Ground::grow(crew.facts());
    let web = crew.label("fetched from the web");
    assert_eq!(ground.climate.last_used[web], Some(4));
    let (x, y) = ground.centre[web];
    let field = super::super::fields::coarse(&ground.climate.field, x, y);
    assert!(
        Climate::of_field(field) <= Climate::Savanna,
        "field {field}"
    );
}
