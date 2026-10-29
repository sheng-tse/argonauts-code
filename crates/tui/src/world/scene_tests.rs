use super::*;
use crate::world::geo::View;

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];

fn filled(width: usize, height: usize, pixel: [u8; 4]) -> Image {
    Image::new(width, height, pixel.repeat(width * height)).expect("the pixels fill it")
}

fn pixel(image: &Image, x: usize, y: usize) -> [u8; 4] {
    let at = (y * image.width() + x) * 4;
    image.pixels()[at..at + 4]
        .try_into()
        .expect("four channels")
}

fn layer(name: &str, depth: Depth, image: Image) -> Layer {
    Layer {
        id: PartId::new(name, 0),
        depth,
        picture: Picture::new(picture_key([1]), image),
    }
}

fn sprite(name: &str, depth: Depth, at: (i32, i32), image: Image) -> Sprite {
    Sprite {
        id: PartId::new(name, 0),
        depth,
        at,
        picture: Picture::new(picture_key([2]), image),
    }
}

#[test]
fn a_part_keeps_its_id_and_parts_keep_apart() {
    const RING: PartId = PartId::new("ring", 0);
    assert_eq!(RING, PartId::new("ring", 0));
    assert_ne!(RING, PartId::new("ring", 1));
    assert_ne!(RING, PartId::new("rings", 0));
    assert_ne!(PartId::new("storm", 12), PartId::new("storm1", 2));
}

#[test]
fn a_picture_key_follows_every_number_in_order() {
    assert_eq!(picture_key([7, 9]), picture_key([7, 9]));
    assert_ne!(picture_key([7, 9]), picture_key([9, 7]));
    assert_ne!(picture_key([7, 9]), picture_key([7, 9, 0]));
}

#[test]
fn an_image_needs_every_pixel() {
    assert!(Image::new(2, 2, vec![0; 15]).is_none());
    assert!(Image::new(2, 2, vec![0; 16]).is_some());
    assert!(Image::new(usize::MAX, 2, Vec::new()).is_none());
}

#[test]
fn an_image_goes_out_as_a_png_and_back() {
    let image = Image::new(2, 1, [RED, [10, 20, 30, 40]].concat()).expect("two pixels");
    assert_eq!(Image::from_png(&image.to_png()), Some(image));
    assert_eq!(format!("{:?}", filled(3, 2, RED)), "Image(3 × 2)");
}

#[test]
fn the_stack_runs_from_the_back_to_the_front() {
    let mut scene = Scene::new(4, 3);
    scene.add_sprite(sprite("ring", Depth::Pointer, (0, 0), filled(1, 1, RED)));
    scene.add_layer(layer("ground", Depth::Ground, filled(4, 3, RED)));
    scene.add_sprite(sprite("pin", Depth::Ground, (1, 1), filled(1, 1, RED)));
    scene.add_layer(layer("sky", Depth::Sky, filled(4, 3, RED)));
    scene.add_sprite(sprite("dot", Depth::Dots, (2, 2), filled(1, 1, RED)));
    scene.add_layer(layer("relief", Depth::Ground, filled(4, 3, RED)));
    let order: Vec<PartId> = scene.stack().iter().map(Part::id).collect();
    let names = ["sky", "ground", "relief", "pin", "dot", "ring"];
    let wanted: Vec<PartId> = names.iter().map(|name| PartId::new(name, 0)).collect();
    assert_eq!(order, wanted);
}

#[test]
fn a_single_picture_is_a_scene_of_its_size() {
    let scene = Scene::single(Picture::new(5, filled(3, 2, GREEN)));
    assert_eq!((scene.width(), scene.height()), (3, 2));
    assert_eq!(scene.layers().len(), 1);
    assert_eq!(scene.layers()[0].picture.key, 5);
    assert!(scene.sprites().is_empty() && !scene.moves);
    assert_eq!(scene.flatten(), filled(3, 2, GREEN));
}

#[test]
fn flattening_lays_sprites_over_layers_and_cuts_them_at_the_edge() {
    let mut scene = Scene::new(4, 3);
    scene.add_layer(layer("ground", Depth::Ground, filled(4, 3, RED)));
    scene.add_sprite(sprite(
        "glass",
        Depth::Weather,
        (3, 2),
        filled(2, 2, [0, 0, 255, 128]),
    ));
    scene.add_sprite(sprite("corner", Depth::Dots, (-1, -1), filled(2, 2, GREEN)));
    let flat = scene.flatten();
    assert_eq!(pixel(&flat, 3, 2), [127, 0, 128, 255]);
    assert_eq!(pixel(&flat, 0, 0), GREEN);
    assert_eq!(pixel(&flat, 1, 0), RED);
    assert_eq!(pixel(&flat, 2, 2), RED);
}

/// A scaled part flattens stretched over the part of its box inside the scene: two red pixels
/// and two green ones grown twice from two pixels left of the scene show one red pixel's
/// worth and both green ones'.
#[test]
fn flattening_stretches_a_scaled_part_over_its_box() {
    let mut pixels = RED.repeat(2);
    pixels.extend(GREEN.repeat(2));
    let image = Image::new(4, 1, pixels).expect("the pixels fill it");
    let mut scene = Scene::new(8, 2);
    scene.add_scaled(Scaled {
        id: PartId::new("level", 0),
        depth: Depth::Ground,
        at: (-2, 0),
        size: (8, 2),
        picture: Picture::new(picture_key([3]), image),
    });
    let flat = scene.flatten();
    assert_eq!(pixel(&flat, 0, 1), RED);
    assert_eq!(pixel(&flat, 1, 0), RED);
    assert_eq!(pixel(&flat, 2, 0), GREEN);
    assert_eq!(pixel(&flat, 5, 1), GREEN);
    assert_eq!(pixel(&flat, 6, 0), [0, 0, 0, 0]);
}

/// A picture cut to what it shows, and where that lies in it.
#[test]
fn trimming_keeps_the_smallest_part_that_shows_everything() {
    let mut pixels = [0, 0, 0, 0].repeat(12);
    pixels[(4 + 1) * 4..(4 + 1) * 4 + 4].copy_from_slice(&RED);
    pixels[(8 + 2) * 4..(8 + 2) * 4 + 4].copy_from_slice(&GREEN);
    let image = Image::new(4, 3, pixels).expect("the pixels fill it");
    let (trimmed, at) = image.trimmed();
    assert_eq!(at, (1, 1));
    assert_eq!((trimmed.width(), trimmed.height()), (2, 2));
    assert_eq!(pixel(&trimmed, 0, 0), RED);
    assert_eq!(pixel(&trimmed, 1, 1), GREEN);
    let (nothing, _) = Image::transparent(3, 3).trimmed();
    assert_eq!((nothing.width(), nothing.height()), (1, 1));
}

#[test]
fn flattening_fits_a_layer_drawn_for_another_size() {
    let mut scene = Scene::new(4, 2);
    scene.add_layer(layer("ground", Depth::Ground, filled(2, 1, GREEN)));
    assert_eq!(scene.flatten(), filled(4, 2, GREEN));
}

#[test]
fn a_dot_under_the_pointer_beats_the_river_and_the_river_a_dot_further_off() {
    let mut hits = HitMap::new();
    let dot = Shape::Point { x: 10.0, y: 10.0 };
    let river = Shape::Path {
        points: vec![(0.0, 20.0), (40.0, 20.0)],
        width: 4.0,
    };
    hits.add(Mark::new(dot.clone(), Pointed::Action(0)).reach(6.0));
    hits.add(Mark::new(river, Pointed::Feature(3)).reach(3.0).tier(1));
    hits.add(Mark::new(dot, Pointed::Action(0)).reach(18.0).tier(2));
    assert_eq!(hits.find(12.0, 12.0), Some(Pointed::Action(0)));
    assert_eq!(hits.find(10.0, 19.0), Some(Pointed::Feature(3)));
    assert_eq!(hits.find(10.0, 2.0), Some(Pointed::Action(0)));
    assert_eq!(hits.find(100.0, 100.0), None);
}

#[test]
fn the_nearest_mark_of_a_tier_answers_and_the_first_of_equals() {
    let mut hits = HitMap::new();
    for (index, x) in [(1, 0.0), (2, 10.0)] {
        hits.add(Mark::new(Shape::Point { x, y: 0.0 }, Pointed::Action(index)).reach(6.0));
    }
    assert_eq!(hits.find(6.0, 0.0), Some(Pointed::Action(2)));
    assert_eq!(hits.find(5.0, 0.0), Some(Pointed::Action(1)));
}

#[test]
fn between_marks_the_flat_map_names_the_ground_under_the_point() {
    let plate = Plate::new(1152.0, 546.0);
    let mut hits = HitMap::new();
    hits.set_under(Under::Map(plate));
    let found = hits.find(300.0, 100.0);
    assert_eq!(
        found,
        Some(Pointed::Ground(Some(plate.place(300.0, 100.0))))
    );
}

#[test]
fn a_globe_names_the_ground_on_its_disc_and_the_sky_round_it() {
    let globe = Globe::in_scene(View::stepped(0.0), 1152.0, 546.0);
    let mut hits = HitMap::new();
    hits.set_under(Under::Globe(globe));
    let middle = globe.surface_at(576.0, 273.0).map(|surface| surface.place);
    assert_eq!(hits.find(576.0, 273.0), Some(Pointed::Ground(middle)));
    assert_eq!(hits.find(5.0, 5.0), Some(Pointed::Sky(None)));
}

#[test]
fn the_land_in_relief_names_the_row_of_the_flat_map_it_shows() {
    let plate = Plate::new(2.0, 2.0);
    let mut hits = HitMap::new();
    hits.set_under(Under::Relief {
        plate,
        width: 2,
        rows: Arc::from([1.5, -1.0, 0.25, 0.75]),
    });
    let shown = plate.place(0.5, 1.5);
    assert_eq!(hits.find(0.5, 0.5), Some(Pointed::Ground(Some(shown))));
    assert_eq!(hits.find(1.2, 0.3), Some(Pointed::Ground(None)));
    assert_eq!(hits.find(3.0, 0.0), None);
    assert_eq!(hits.find(-1.0, 0.0), None);
    assert_eq!(hits.find(0.0, 9.0), None);
    let printed = format!("{:?}", hits.under());
    assert!(printed.contains("rows: 4"), "{printed}");
}

#[test]
fn behind_the_universe_the_pointer_finds_the_sky() {
    let mut hits = HitMap::new();
    assert_eq!(hits.find(1.0, 1.0), None);
    hits.set_under(Under::Sky);
    assert_eq!(hits.find(1.0, 1.0), Some(Pointed::Sky(None)));
}

#[test]
fn each_shape_measures_how_far_a_point_lies_outside_it() {
    let rect = Shape::Rect {
        left: 0.0,
        top: 0.0,
        right: 10.0,
        bottom: 10.0,
    };
    assert_eq!(rect.distance(5.0, 5.0), -5.0);
    assert_eq!(rect.distance(13.0, 14.0), 5.0);
    let disc = Shape::Disc {
        x: 0.0,
        y: 0.0,
        radius: 2.0,
    };
    assert_eq!(disc.distance(3.0, 4.0), 3.0);
    let lone = Shape::Path {
        points: vec![(0.0, 0.0)],
        width: 2.0,
    };
    assert_eq!(lone.distance(3.0, 4.0), 4.0);
    let empty = Shape::Path {
        points: Vec::new(),
        width: 2.0,
    };
    assert!(empty.distance(0.0, 0.0).is_infinite());
    let turned = Shape::Ellipse {
        x: 0.0,
        y: 0.0,
        rx: 10.0,
        ry: 5.0,
        angle: std::f32::consts::FRAC_PI_2,
    };
    assert!(turned.distance(0.0, 10.0).abs() < 1e-4);
    assert!(turned.distance(5.0, 0.0).abs() < 1e-4);
    assert!(turned.distance(0.0, 0.0) < 0.0);
}

#[test]
fn a_scene_can_be_drawn_on_another_thread() {
    fn crosses_threads<T: Send + Sync + 'static>() {}
    crosses_threads::<Scene>();
    crosses_threads::<Arc<Scene>>();
}

#[test]
fn depths_run_from_the_sky_to_the_pointer() {
    let depths = [
        Depth::Sky,
        Depth::Ground,
        Depth::Nature,
        Depth::Weather,
        Depth::Dots,
        Depth::Pointer,
    ];
    assert!(depths.windows(2).all(|pair| pair[0] < pair[1]));
}
