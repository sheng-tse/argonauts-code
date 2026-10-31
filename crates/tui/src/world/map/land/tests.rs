use std::sync::OnceLock;

use regex::Regex;

use super::*;
use crate::base::draw::geo::Plate;
use crate::world::map::climate::Climate;
use crate::world::map::fields::crews::{Crew, Key};
use crate::world::map::fields::{COARSE, COARSE_HEIGHT, COARSE_WIDTH, Ground, HEIGHT, WIDTH};

fn ground(key: Key) -> &'static Ground {
    static GROUNDS: OnceLock<Vec<Ground>> = OnceLock::new();
    let grounds = GROUNDS.get_or_init(|| {
        Key::ALL
            .iter()
            .map(|key| Ground::grow(Crew::get(*key).facts()))
            .collect()
    });
    &grounds[Key::ALL.iter().position(|k| *k == key).unwrap_or(0)]
}

fn index(key: Key, words: &str) -> usize {
    Crew::get(key).label(words)
}

#[test]
fn the_ragged_coast_leaves_every_dot_on_land() {
    for key in Key::ALL {
        let ground = ground(key);
        let surface = Surface::map(ground, None);
        let wet = ground
            .dots
            .iter()
            .filter(|(x, y)| surface.at(f64::from(*x), f64::from(*y), None).land < 0.99)
            .count();
        assert_eq!(wet, 0, "crew {key:?}");
    }
}

#[test]
fn the_ice_never_covers_land_or_a_dot() {
    let lat = |y: f64| Plate::MAP.place(0.0, y).lat;
    for key in Key::ALL {
        let ground = ground(key);
        let polar = Polar::of(ground);
        let g = COARSE as f64;
        let land_on_ice = (0..COARSE_HEIGHT)
            .flat_map(|j| (0..COARSE_WIDTH).map(move |k| (k, j)))
            .filter(|(k, j)| ground.land.get(*k, *j) >= 1.0 && polar.covers(lat(*j as f64 * g)))
            .count();
        let dots_on_ice = ground
            .dots
            .iter()
            .filter(|(_, y)| polar.covers(lat(f64::from(*y))))
            .count();
        assert_eq!((land_on_ice, dots_on_ice), (0, 0), "crew {key:?}");
    }
}

#[test]
fn the_ice_reaches_out_by_idle_and_quiet_days() {
    let r = Polar::of(ground(Key::R));
    assert_eq!(r.cap_north, None);
    assert_eq!(r.cap_south, Some(22.0));
    assert_eq!(r.bergs.len(), 4);
    let t = Polar::of(ground(Key::T));
    assert_eq!(t.cap_north.map(|cap| (cap * 10.0).round()), Some(144.0));
    assert!(t.cap_south.is_some());
}

#[test]
fn pointing_at_the_ice_says_what_it_is() {
    let busy = ground(Key::R);
    let polar = Polar::of(busy);
    let line = polar
        .line_at(busy, 576.0, HEIGHT as f64 - 14.0)
        .expect("the south ice");
    assert_eq!(line.text, "South ice · 8 quiet days in the period");
    assert_eq!(line.kind, GroundKind::Ice(IceKind::South));
    assert_eq!(polar.line_at(busy, 576.0, 12.0), None);

    let quiet = ground(Key::T);
    let polar = Polar::of(quiet);
    let cap = polar.line_at(quiet, 300.0, 6.0).expect("the north cap");
    assert_eq!(cap.text, "Ice cap · 9 days since the last action");
    let berg = &polar.bergs[0];
    let (x, y) = Plate::MAP.pixel(crate::base::draw::geo::LonLat::new(berg.lon, berg.lat));
    let line = polar.line_at(quiet, x, y).expect("an iceberg");
    assert_eq!(line.kind, GroundKind::Ice(IceKind::Berg(0)));
    assert!(line.text.starts_with("Iceberg · "), "{}", line.text);
}

#[test]
fn pointing_at_the_sea_says_so() {
    let line = line_at(ground(Key::R), 20.0, 270.0);
    assert_eq!(line.text, "Sea · no behavior here");
    assert_eq!(line.kind, GroundKind::Sea);
    assert_eq!(GroundLine::open_sea().text, "Open sea · no behavior here");
}

#[test]
fn a_behavior_last_used_two_weeks_ago_stands_in_savanna() {
    let ground = ground(Key::R);
    let web = index(Key::R, "searched the web");
    assert_eq!(ground.climate.class[web], Some(Climate::Savanna));
    let (x, y) = ground.centre[web];
    let field = ground.climate.field.sample(x / 4.0, y / 4.0);
    assert_eq!(Climate::of_field(field), Climate::Savanna);
    let line = line_at(ground, x, y + 4.0);
    assert!(line.text.starts_with("Savanna"), "{}", line.text);
}

#[test]
fn a_behavior_used_most_days_stands_in_rainforest() {
    let ground = ground(Key::R);
    let rainforest = (0..ground.behaviors()).find(|at| {
        let (x, y) = ground.centre[*at];
        ground.count[*at] >= 20
            && ground.climate.days_in_last_7[*at] >= 4
            && Climate::of_field(ground.climate.field.sample(x / 4.0, y / 4.0))
                == Climate::Rainforest
    });
    assert!(rainforest.is_some());
}

#[test]
fn bare_rock_and_snow_say_their_counts() {
    let ground = ground(Key::R);
    let busiest = ground.busiest.expect("a busiest behavior");
    let (x, y) = ground.centre[busiest];
    let line = line_at(ground, x, y);
    assert_eq!(line.kind, GroundKind::Snow);
    assert_eq!(
        line.text,
        format!(
            "Snow and glacier · about {} actions here, 100 or more · ‘returned its structured answer’",
            ground.height_at(x, y).round()
        )
    );
    let rock = (0..360).find_map(|step| {
        let angle = f64::from(step).to_radians();
        let line = line_at(ground, x + 22.0 * angle.cos(), y + 22.0 * angle.sin());
        (line.kind == GroundKind::Rock).then_some(line.text)
    });
    let rock = rock.expect("bare rock round the busiest peak");
    assert!(
        Regex::new("^Bare rock · about [0-9]+ actions here, 30 to 99 · ‘.+’$")
            .expect("a pattern")
            .is_match(&rock),
        "{rock}"
    );
}

#[test]
fn the_lands_line_names_a_behavior_whose_own_days_match_the_land() {
    let named =
        Regex::new("· near ‘(.+?)’,|· from (?:the bigger )?‘(.+?)’ beside").expect("a pattern");
    for key in Key::ALL {
        let ground = ground(key);
        for at in (0..ground.behaviors()).filter(|at| ground.count[*at] > 0) {
            let (x, y) = ground.centre[at];
            let line = line_at(ground, x, y + 6.0);
            let GroundKind::Climate(climate) = line.kind else {
                continue;
            };
            let Some(found) = named.captures(&line.text) else {
                continue;
            };
            let name = found.get(1).or(found.get(2)).expect("a name").as_str();
            let by = index(key, name);
            assert_eq!(
                ground.climate.class[by],
                Some(climate),
                "{key:?} {}",
                line.text
            );
        }
    }
}

#[test]
fn between_names_one_wetter_and_one_drier_behavior() {
    let between = Regex::new("· between ‘(.+?)’ \\(.+?\\) and ‘(.+?)’ \\(").expect("a pattern");
    let edge = Regex::new("· at the edge of the (\\w+) round ‘(.+?)’").expect("a pattern");
    let (mut betweens, mut edges) = (0, 0);
    for key in Key::ALL {
        let ground = ground(key);
        for y in (6..HEIGHT).step_by(6) {
            for x in (6..WIDTH).step_by(6) {
                let line = line_at(ground, x as f64, y as f64);
                let GroundKind::Climate(climate) = line.kind else {
                    continue;
                };
                let class = |name: &str| ground.climate.class[index(key, name)];
                if let Some(found) = between.captures(&line.text) {
                    betweens += 1;
                    let (one, two) = (class(&found[1]), class(&found[2]));
                    let here = Some(climate);
                    assert!(
                        (one > here && two < here) || (one < here && two > here),
                        "{key:?} {x},{y} {}",
                        line.text
                    );
                }
                if let Some(found) = edge.captures(&line.text) {
                    edges += 1;
                    let theirs = class(&found[2]).expect("a climate");
                    assert_ne!(theirs, climate, "{}", line.text);
                    assert_eq!(theirs.name(), &found[1]);
                }
            }
        }
    }
    assert_eq!((betweens, edges), (1039, 8));
}

#[test]
fn a_compared_group_tints_the_land_it_leans_to() {
    let ground = ground(Key::R);
    let compared = Compared::new(ground, 0).expect("enough actions to compare");
    assert!(
        compared
            .leaning
            .iter()
            .flatten()
            .any(|l| *l > Leaning::Same)
    );
    assert!(
        compared
            .leaning
            .iter()
            .flatten()
            .any(|l| *l < Leaning::Same)
    );
    let tint = Tint::new(ground, compared);
    let values = tint.field.values();
    assert!(values.iter().any(|v| *v > 0.1));
    assert!(values.iter().any(|v| *v < -0.1));
    let calm = Surface::map(ground, None);
    let tinted = Surface {
        tint: Some(&tint),
        ..calm
    };
    let busiest = ground.busiest.expect("a busiest behavior");
    let (x, y) = ground.centre[busiest];
    assert_ne!(tinted.at(x, y, None).colour, calm.at(x, y, None).colour);
    let few = (0..ground.group_count.len()).find(|g| ground.group_count[*g] < 5);
    if let Some(few) = few {
        assert_eq!(Compared::new(ground, few), None);
    }
}

#[test]
fn the_planet_wraps_the_maps_own_ground() {
    let ground = ground(Key::T);
    let texture = Texture::new(ground, None, Marks::default());
    let plate = Plate::TEXTURE;
    assert_eq!(
        (texture.width, texture.height),
        (plate.width() as usize, plate.height() as usize)
    );
    let surface = Surface {
        sea: SeaColours::planet(ground.look),
        step_lines: false,
        polar: None,
        ..Surface::map(ground, None)
    };
    let busiest = ground.busiest.expect("a busiest behavior");
    let (x, y) = ground.centre[busiest];
    let place = Plate::MAP.place(x, y);
    let (u, v) = plate.pixel(place);
    let (column, row) = (u as usize, v as usize);
    let centre_lon =
        (column as f64 + 0.5) / texture.width as f64 * std::f64::consts::TAU - std::f64::consts::PI;
    let centre_lat = std::f64::consts::FRAC_PI_2
        - (row as f64 + 0.5) / texture.height as f64 * std::f64::consts::PI;
    let (mx, my) = Plate::MAP.pixel(crate::base::draw::geo::LonLat::new(centre_lon, centre_lat));
    let expected = crate::world::map::fields::to_bytes(surface.at(mx, my, None).colour);
    assert_eq!(texture.colour(column, row), expected);
    assert!(!texture.wet[row * texture.width + column]);
    // The north cap of an idle crew and the open sea beyond the map's edge.
    let light =
        |c: [u8; 3]| 0.3 * f64::from(c[0]) + 0.59 * f64::from(c[1]) + 0.11 * f64::from(c[2]);
    assert!(light(texture.colour(100, 2)) > 170.0);
    assert!(texture.wet[texture.height / 2 * texture.width]);
}

#[test]
fn crew_ts_planet_texture_is_drawn_as_before() {
    use crate::world::map::relief::cells::{compare_means, mean_colours};
    use crate::world::raster::Canvas;

    let texture = Texture::new(ground(Key::T), None, Marks::default());
    let size = (texture.width, texture.height);
    let means = mean_colours(size, (24, 24), |x, y| texture.colour(x, y));
    let png = || {
        let rgba: Vec<u8> = texture
            .rgb
            .chunks(3)
            .flat_map(|texel| [texel[0], texel[1], texel[2], 255])
            .collect();
        Canvas::from_rgba(size.0 as u32, size.1 as u32, &rgba)
            .map(|canvas| canvas.to_png())
            .unwrap_or_default()
    };
    compare_means(
        "texture-t",
        &means,
        include_str!("golden/texture-t.txt"),
        png,
    );
}
