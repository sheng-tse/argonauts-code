use super::*;
use crate::base::draw::scene::{Layer, Picture, Scaled, Sprite};
use crate::base::mascot::graphics::pictures_in;

const FIRST: u32 = 500;
/// A map of 64 × 13 cells from the third column and the eighth row, 10 × 20 pixels a cell.
const SPOT: Spot = Spot {
    cells: Rect {
        x: 2,
        y: 7,
        width: 64,
        height: 13,
    },
    cell: (10, 20),
};

fn picture(key: u64, width: usize, height: usize) -> Picture {
    let pixels = [key as u8, 40, 90, 255].repeat(width * height);
    Picture::new(
        key,
        Image::new(width, height, pixels).expect("whole pixels"),
    )
}

fn ground() -> Layer {
    Layer {
        id: PartId::new("ground", 0),
        depth: Depth::Ground,
        picture: picture(1, 640, 260),
    }
}

fn storm(at: (i32, i32), frame: u64) -> Sprite {
    Sprite {
        id: PartId::new("storm", 0),
        depth: Depth::Weather,
        at,
        picture: picture(100 + frame, 24, 24),
    }
}

fn scene(sprites: &[Sprite]) -> Scene {
    let mut scene = Scene::new(640, 260);
    scene.add_layer(ground());
    for sprite in sprites {
        scene.add_sprite(sprite.clone());
    }
    scene
}

fn text(escapes: &[u8]) -> String {
    String::from_utf8_lossy(escapes).into_owned()
}

/// Each placement that went out, as the cell the cursor moved to and the placement's keys.
fn placements(escapes: &[u8]) -> Vec<((u16, u16), String)> {
    let text = text(escapes);
    let mut cursor = None;
    let mut out = Vec::new();
    for part in text.split('\x1b').filter(|part| !part.is_empty()) {
        if let Some(moved) = part
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix('H'))
        {
            let (row, column) = moved.split_once(';').expect("a row and a column");
            cursor = Some((
                column.parse::<u16>().unwrap() - 1,
                row.parse::<u16>().unwrap() - 1,
            ));
        } else if let Some(keys) = part.strip_prefix("_Ga=p,") {
            let keys = keys.trim_end_matches('\\').to_owned();
            out.push((cursor.expect("the cursor moved first"), keys));
        }
    }
    out
}

fn removed(escapes: &[u8]) -> usize {
    text(escapes).matches("a=d,d=i,").count()
}

fn freed(escapes: &[u8]) -> usize {
    text(escapes).matches("a=d,d=I,").count()
}

fn scaled(at: (i32, i32), size: (u32, u32), picture: Picture) -> Scaled {
    Scaled {
        id: PartId::new("level", 0),
        depth: Depth::Ground,
        at,
        size,
        picture,
    }
}

fn scaled_scene(part: Scaled) -> Scene {
    let mut scene = Scene::new(640, 260);
    scene.add_scaled(part);
    scene
}

/// A scaled part goes out at its own pixels, and the terminal stretches it over whole cells
/// from its own pixel: twice its 64 × 26 pixels from (25, 47) spans 13 cells by 3.
#[test]
fn a_scaled_part_stretches_its_picture_over_whole_cells() {
    let mut stage = Stage::new(FIRST).packing_in_line();
    let sent = stage.show(
        &scaled_scene(scaled((25, 47), (128, 52), picture(9, 64, 26))),
        SPOT,
    );
    let sizes: Vec<(u32, u32)> = pictures_in(&sent)
        .iter()
        .map(|png| {
            let word = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
            (word(16), word(20))
        })
        .collect();
    assert_eq!(sizes, [(64, 26)]);
    assert_eq!(
        placements(&sent),
        [(
            (4, 9),
            "q=2,C=1,i=500,p=1,X=5,Y=7,c=13,r=3,z=65537".to_owned()
        )]
    );
}

/// A part grown past the scene shows only the part of its picture inside, over every cell: a
/// picture of the whole scene grown twice round its middle shows its middle half.
#[test]
fn a_scaled_part_past_the_edges_shows_the_part_of_its_picture_inside() {
    let mut stage = Stage::new(FIRST).packing_in_line();
    let sent = stage.show(
        &scaled_scene(scaled((-320, -130), (1280, 520), picture(9, 640, 260))),
        SPOT,
    );
    assert_eq!(
        placements(&sent),
        [(
            (2, 7),
            "q=2,C=1,i=500,p=1,x=160,y=65,w=320,h=130,c=64,r=13,z=65537".to_owned()
        )]
    );
}

/// Rounded to cells, a box never runs past the spot: one that starts part way into a cell
/// near the right edge stops at the last whole cell.
#[test]
fn a_scaled_part_never_runs_past_the_spot() {
    let mut stage = Stage::new(FIRST).packing_in_line();
    let sent = stage.show(
        &scaled_scene(scaled((605, 0), (40, 20), picture(9, 40, 20))),
        SPOT,
    );
    assert_eq!(
        placements(&sent),
        [(
            (62, 7),
            "q=2,C=1,i=500,p=1,X=5,x=0,y=0,w=35,h=20,c=3,r=1,z=65537".to_owned()
        )]
    );
}

/// Each step of a zoom grows its parts again: the same pictures in new boxes, one short
/// placement each and nothing sent or taken away.
#[test]
fn a_scaled_part_grown_again_is_one_placement() {
    let mut stage = Stage::new(FIRST).packing_in_line();
    stage.show(
        &scaled_scene(scaled((25, 47), (128, 52), picture(9, 64, 26))),
        SPOT,
    );
    let grown = stage.show(
        &scaled_scene(scaled((5, 37), (192, 78), picture(9, 64, 26))),
        SPOT,
    );
    assert!(pictures_in(&grown).is_empty());
    assert_eq!(
        placements(&grown),
        [(
            (2, 8),
            "q=2,C=1,i=500,p=1,X=5,Y=17,c=19,r=4,z=65537".to_owned()
        )]
    );
    assert_eq!(removed(&grown), 0);
}

#[test]
fn a_first_scene_sends_each_picture_once_and_places_every_part() {
    let mut stage = Stage::new(FIRST).packing_in_line();
    let sent = stage.show(&scene(&[storm((25, 47), 0)]), SPOT);
    assert_eq!(pictures_in(&sent).len(), 2);
    let placed = placements(&sent);
    assert_eq!(
        placed,
        [
            ((2, 7), "q=2,C=1,i=500,p=1,c=64,r=13,z=65537".to_owned()),
            // 25 pixels in is the third cell and 5 pixels; 47 down the third row and 7.
            ((4, 9), "q=2,C=1,i=501,p=2,X=5,Y=7,z=196609".to_owned()),
        ]
    );
    assert!(stage.is_showing());
    // The same scene again changes nothing, so nothing goes out.
    assert!(stage.show(&scene(&[storm((25, 47), 0)]), SPOT).is_empty());
}

#[test]
fn a_sprite_that_moves_is_one_short_placement() {
    let mut stage = Stage::new(FIRST).packing_in_line();
    stage.show(&scene(&[storm((25, 47), 0)]), SPOT);
    let moved = stage.show(&scene(&[storm((31, 47), 0)]), SPOT);
    assert!(pictures_in(&moved).is_empty());
    assert_eq!(
        placements(&moved),
        [((5, 9), "q=2,C=1,i=501,p=2,X=1,Y=7,z=196609".to_owned())]
    );
    assert_eq!(removed(&moved), 0);
    assert!(moved.len() < 80, "{} bytes", moved.len());
}

#[test]
fn a_frame_kept_already_swaps_in_without_sending_its_picture() {
    let mut stage = Stage::new(FIRST).packing_in_line();
    stage.show(&scene(&[storm((25, 47), 0)]), SPOT);
    let next = stage.show(&scene(&[storm((25, 47), 1)]), SPOT);
    assert_eq!(pictures_in(&next).len(), 1, "the new frame, once");
    assert!(text(&next).ends_with("\x1b_Ga=d,d=i,q=2,i=501,p=2\x1b\\"));
    let back = stage.show(&scene(&[storm((25, 47), 0)]), SPOT);
    assert!(
        pictures_in(&back).is_empty(),
        "the first frame is still kept"
    );
    assert_eq!(
        text(&back),
        "\x1b[10;5H\x1b_Ga=p,q=2,C=1,i=501,p=2,X=5,Y=7,z=196609\x1b\\\
         \x1b_Ga=d,d=i,q=2,i=502,p=2\x1b\\"
    );
}

#[test]
fn a_part_that_goes_takes_only_its_placement_with_it() {
    let mut stage = Stage::new(FIRST).packing_in_line();
    stage.show(&scene(&[storm((25, 47), 0)]), SPOT);
    let gone = stage.show(&scene(&[]), SPOT);
    assert_eq!(text(&gone), "\x1b_Ga=d,d=i,q=2,i=501,p=2\x1b\\");
    // Coming back places it again from the picture kept.
    let back = stage.show(&scene(&[storm((25, 47), 0)]), SPOT);
    assert!(pictures_in(&back).is_empty());
    assert_eq!(placements(&back).len(), 1);
}

#[test]
fn hidden_parts_keep_their_pictures_and_freeing_lets_them_go() {
    let mut stage = Stage::new(FIRST).packing_in_line();
    stage.show(&scene(&[storm((25, 47), 0)]), SPOT);
    let hidden = stage.hide();
    assert_eq!(removed(&hidden), 2);
    assert_eq!(freed(&hidden), 0);
    assert!(!stage.is_showing());
    assert!(stage.keeps_pictures());
    let back = stage.show(&scene(&[storm((25, 47), 0)]), SPOT);
    assert!(pictures_in(&back).is_empty());
    assert_eq!(placements(&back).len(), 2);
    let freed_now = stage.free();
    assert_eq!(removed(&freed_now), 2);
    assert_eq!(freed(&freed_now), 2);
    assert!(!stage.keeps_pictures());
    assert!(stage.free().is_empty(), "nothing is left to free");
}

#[test]
fn a_sprite_past_the_edge_shows_only_the_part_inside() {
    let mut stage = Stage::new(FIRST).packing_in_line();
    let sent = stage.show(&scene(&[storm((-10, 250), 0)]), SPOT);
    let placed = placements(&sent);
    assert_eq!(
        placed[1],
        (
            (2, 19),
            "q=2,C=1,i=501,p=2,Y=10,x=10,y=0,w=14,h=10,z=196609".to_owned()
        )
    );
    // Wholly outside, it isn't placed at all.
    let outside = stage.show(&scene(&[storm((700, 10), 0)]), SPOT);
    assert_eq!(text(&outside), "\x1b_Ga=d,d=i,q=2,i=501,p=2\x1b\\");
}

#[test]
fn a_scene_drawn_for_other_cells_is_stretched_over_these() {
    let mut stage = Stage::new(FIRST).packing_in_line();
    // Drawn at 5 × 10 pixels a cell, before the font grew.
    let mut small = Scene::new(320, 130);
    small.add_layer(Layer {
        picture: picture(1, 320, 130),
        ..ground()
    });
    small.add_sprite(storm((100, 30), 0));
    let sent = stage.show(&small, SPOT);
    let placed = placements(&sent);
    // The layer still fills the cells, and the sprite stands where its point falls, stretched
    // as much as the scene: twice its own 24 pixels each way.
    assert_eq!(placed[0].1, "q=2,C=1,i=500,p=1,c=64,r=13,z=65537");
    assert_eq!(
        placed[1],
        ((22, 10), "q=2,C=1,i=501,p=2,z=196609".to_owned())
    );
    let sizes: Vec<(u32, u32)> = pictures_in(&sent)
        .iter()
        .map(|png| {
            let word = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
            (word(16), word(20))
        })
        .collect();
    assert_eq!(sizes, [(320, 130), (48, 48)]);
}

#[test]
fn depths_stack_apart_so_a_new_part_moves_none_at_another_depth() {
    let mut stage = Stage::new(FIRST).packing_in_line();
    let ring = Sprite {
        id: PartId::new("ring", 0),
        depth: Depth::Pointer,
        at: (100, 100),
        picture: picture(7, 12, 12),
    };
    stage.show(&scene(std::slice::from_ref(&ring)), SPOT);
    let with_storm = stage.show(&scene(&[storm((25, 47), 0), ring]), SPOT);
    let placed = placements(&with_storm);
    assert_eq!(placed.len(), 1, "only the storm: {placed:?}");
    assert!(placed[0].1.ends_with("z=196609"));
}

#[test]
fn the_pictures_used_longest_ago_go_once_the_limit_is_passed() {
    // Room for the ground and two of the storm's frames.
    let bytes = 640 * 260 * 4 + 2 * 24 * 24 * 4;
    let mut stage = Stage::new(FIRST).packing_in_line().with_limit(bytes);
    stage.show(&scene(&[storm((25, 47), 0)]), SPOT);
    let second = stage.show(&scene(&[storm((25, 47), 1)]), SPOT);
    assert_eq!(freed(&second), 0);
    let third = stage.show(&scene(&[storm((25, 47), 2)]), SPOT);
    assert_eq!(
        text(&third)
            .matches("\x1b_Ga=d,d=I,q=2,i=501\x1b\\")
            .count(),
        1,
        "the first frame, used longest ago"
    );
    let mut kept: Vec<u32> = stage.kept_ids().collect();
    kept.sort_unstable();
    assert_eq!(kept, [500, 502, 503]);
}

#[test]
fn ids_come_round_again_but_never_onto_a_kept_picture() {
    let mut stage = Stage::new(FIRST).packing_in_line();
    stage.next_id = FIRST + IDS - 1;
    let sent = stage.show(&scene(&[storm((25, 47), 0)]), SPOT);
    let placed = placements(&sent);
    assert!(placed[0].1.contains(&format!("i={}", FIRST + IDS - 1)));
    assert!(placed[1].1.contains(&format!("i={FIRST},")));
    stage.next_id = FIRST;
    let next = stage.show(&scene(&[storm((25, 47), 1)]), SPOT);
    assert!(
        placements(&next)[0]
            .1
            .contains(&format!("i={},", FIRST + 1))
    );
}

#[test]
fn the_look_changes_with_any_picture_or_place() {
    let base = look(&scene(&[storm((25, 47), 0)]));
    assert_eq!(base, look(&scene(&[storm((25, 47), 0)])));
    assert_ne!(base, look(&scene(&[storm((26, 47), 0)])));
    assert_ne!(base, look(&scene(&[storm((25, 47), 1)])));
    assert_ne!(base, look(&scene(&[])));
}

#[test]
fn every_picture_kept_is_listed_for_a_panic_to_free_until_it_goes() {
    let first = 0x7a00_0000;
    let mut stage = Stage::new(first)
        .packing_in_line()
        .with_limit(640 * 260 * 4 + 24 * 24 * 4);
    stage.show(&scene(&[storm((25, 47), 0)]), SPOT);
    let listed = |id: u32| kept_anywhere().contains(&id);
    assert!(listed(first) && listed(first + 1));
    // A frame over the limit pushes out the one before it.
    stage.show(&scene(&[storm((25, 47), 1)]), SPOT);
    assert!(!listed(first + 1));
    assert!(listed(first + 2));
    stage.free();
    assert!(!listed(first) && !listed(first + 2));
}

#[test]
fn new_pictures_are_packed_off_the_frame_and_their_parts_wait_for_them() {
    let mut stage = Stage::new(FIRST);
    let first = stage.show(&scene(&[storm((25, 47), 0)]), SPOT);
    // Whatever was packed by now goes out; the rest follows on a later call.
    let deadline = argonauts_testkit::pty::deadline::after(std::time::Duration::from_secs(5));
    let mut sent = first;
    while stage.is_packing() || placements(&sent).len() < 2 {
        assert!(std::time::Instant::now() < deadline, "still packing");
        std::thread::yield_now();
        sent.extend(stage.show(&scene(&[storm((25, 47), 0)]), SPOT));
    }
    assert_eq!(pictures_in(&sent).len(), 2);
    assert_eq!(placements(&sent).len(), 2);

    // A new frame waits, and its storm stays as it was meanwhile.
    let mut next = stage.show(&scene(&[storm((25, 47), 1)]), SPOT);
    if stage.is_packing() {
        assert!(next.is_empty(), "{:?}", text(&next));
    }
    while stage.is_packing() || pictures_in(&next).is_empty() {
        assert!(std::time::Instant::now() < deadline, "still packing");
        std::thread::yield_now();
        next.extend(stage.show(&scene(&[storm((25, 47), 1)]), SPOT));
    }
    assert!(text(&next).ends_with("\x1b_Ga=d,d=i,q=2,i=501,p=2\x1b\\"));
}

/// The ids the pictures in `escapes` were sent under, in order.
fn stored_ids(escapes: &[u8]) -> Vec<u32> {
    text(escapes)
        .split("\x1b_Ga=t,")
        .skip(1)
        .filter_map(|keys| {
            let id = keys.split(",i=").nth(1)?.split([',', ';']).next()?;
            id.parse().ok()
        })
        .collect()
}

/// Each picture is packed into the escapes that send it off the frame, under an id taken when
/// it was asked for, so pictures asked for while others are packed never share one.
#[test]
fn pictures_packed_at_once_each_go_under_an_id_of_their_own() {
    let mut stage = Stage::new(FIRST);
    let deadline = argonauts_testkit::pty::deadline::after(std::time::Duration::from_secs(5));
    let mut sent = Vec::new();
    for frame in 0..4 {
        sent.extend(stage.show(&scene(&[storm((25, 47), frame)]), SPOT));
    }
    while stage.is_packing() || placements(&sent).is_empty() {
        assert!(std::time::Instant::now() < deadline, "still packing");
        std::thread::yield_now();
        sent.extend(stage.show(&scene(&[storm((25, 47), 3)]), SPOT));
    }
    let ids = stored_ids(&sent);
    let distinct: HashSet<u32> = ids.iter().copied().collect();
    assert_eq!(distinct.len(), ids.len(), "{ids:?}");
    assert_eq!(pictures_in(&sent).len(), ids.len());
    let kept: HashSet<u32> = stage.kept_ids().collect();
    for (_, keys) in placements(&sent) {
        let id = keys
            .split(",i=")
            .nth(1)
            .and_then(|id| id.split(',').next())
            .and_then(|id| id.parse::<u32>().ok())
            .expect("a placement names its picture");
        assert!(kept.contains(&id), "{id} placed but not kept: {keys}");
    }
}

/// A picture packed after the motion went past its scene still goes to the terminal, so the
/// scene coming back shows it at once, rather than packing it again to come late again.
#[test]
fn a_picture_packed_after_its_scene_went_by_is_kept_for_its_next_turn() {
    let mut stage = Stage::new(FIRST);
    let deadline = argonauts_testkit::pty::deadline::after(std::time::Duration::from_secs(5));
    let mut sent = stage.show(&scene(&[storm((25, 47), 0)]), SPOT);
    // The motion steps on at once, while the first storm is packed.
    sent.extend(stage.show(&scene(&[storm((25, 47), 1)]), SPOT));
    while stage.is_packing() || pictures_in(&sent).len() < 3 {
        assert!(std::time::Instant::now() < deadline, "still packing");
        std::thread::yield_now();
        sent.extend(stage.show(&scene(&[storm((25, 47), 1)]), SPOT));
    }
    assert_eq!(pictures_in(&sent).len(), 3, "the ground and both storms");
    let again = stage.show(&scene(&[storm((25, 47), 0)]), SPOT);
    assert!(!stage.is_packing());
    assert!(pictures_in(&again).is_empty(), "{:?}", text(&again));
    assert_eq!(placements(&again).len(), 1, "the first storm placed again");
}

#[test]
fn hiding_gives_up_the_pictures_still_being_packed() {
    let mut stage = Stage::new(FIRST);
    stage.show(&scene(&[storm((25, 47), 0)]), SPOT);
    stage.hide();
    assert!(!stage.is_packing());
}

#[test]
fn a_stage_beneath_the_text_stacks_every_part_under_coloured_cells() {
    let mut stage = Stage::new(FIRST).packing_in_line().beneath_text();
    let sent = stage.show(&scene(&[storm((25, 47), 0)]), SPOT);
    let depths: Vec<i64> = placements(&sent)
        .iter()
        .map(|(_, keys)| {
            let z = keys.rsplit(",z=").next().expect("a depth");
            z.parse().expect("a number")
        })
        .collect();
    assert_eq!(depths.len(), 2);
    assert!(depths[0] < depths[1], "the ground under the storm");
    for z in depths {
        assert!(
            z < i64::from(crate::base::mascot::graphics::UNDER_BACKGROUNDS) + 1,
            "{z}"
        );
    }
}

#[test]
fn a_sprite_wholly_outside_is_never_packed() {
    let mut stage = Stage::new(FIRST);
    stage.show(&scene(&[storm((700, 10), 0)]), SPOT);
    let deadline = argonauts_testkit::pty::deadline::after(std::time::Duration::from_secs(5));
    let mut sent = Vec::new();
    while stage.is_packing() || placements(&sent).is_empty() {
        assert!(std::time::Instant::now() < deadline, "still packing");
        std::thread::yield_now();
        sent.extend(stage.show(&scene(&[storm((700, 10), 0)]), SPOT));
    }
    assert_eq!(pictures_in(&sent).len(), 1, "the ground alone");
    assert!(stage.show(&scene(&[storm((700, 10), 0)]), SPOT).is_empty());
    assert!(!stage.is_packing());
}
