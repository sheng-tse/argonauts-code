use std::f64::consts::{FRAC_PI_2, FRAC_PI_4};

use super::*;

const BLUE: [u8; 4] = [0, 0, 255, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const WHITE: [u8; 4] = [255; 4];
const RED_PIXEL: [u8; 4] = [255, 0, 0, 255];

fn picture(width: u32, height: u32, pixels: &[[u8; 4]]) -> Canvas {
    Canvas::from_rgba(width, height, &pixels.concat()).expect("the pixels fill the picture")
}

/// Red, green, blue and white, two by two.
fn four() -> Canvas {
    picture(2, 2, &[RED_PIXEL, GREEN, BLUE, WHITE])
}

fn sharp() -> Brush {
    Brush::default().smoothing(Smoothing::Nearest)
}

#[test]
fn a_picture_at_whole_pixels_lands_exactly() {
    let mut canvas = Canvas::new(8, 8);
    canvas.draw(&four(), 3.0, 4.0, &Brush::default());
    assert_eq!(canvas.pixel(3, 4), RED_PIXEL);
    assert_eq!(canvas.pixel(4, 4), GREEN);
    assert_eq!(canvas.pixel(3, 5), BLUE);
    assert_eq!(canvas.pixel(4, 5), WHITE);
    assert!(near(coverage(&canvas), 4.0, 0.01));
}

#[test]
fn a_picture_between_pixels_mixes_its_neighbours() {
    let pair = picture(2, 1, &[RED_PIXEL, BLUE]);
    let mut canvas = Canvas::new(4, 1);
    canvas.draw(&pair, 0.25, 0.0, &Brush::default());

    // Pixel 1's centre falls a quarter of the way from the pair's red centre to its blue.
    let [r, g, b, a] = canvas.pixel(1, 0);
    assert!(
        r.abs_diff(64) <= 2 && g == 0 && b.abs_diff(191) <= 2 && a == 255,
        "{:?}",
        canvas.pixel(1, 0)
    );
    // A picture landing square covers each pixel whose centre it covers, its edge
    // colours carried out to its edge.
    assert_eq!(canvas.pixel(0, 0), RED_PIXEL);
    assert_eq!(canvas.pixel(2, 0), [0; 4]);
}

#[test]
fn a_picture_left_of_and_above_whole_pixels_mixes_the_same_way() {
    let pair = picture(2, 1, &[RED_PIXEL, BLUE]);
    let mut canvas = Canvas::new(4, 2);
    canvas.draw(&pair, 1.25, -0.25, &Brush::default());
    assert_eq!(canvas.pixel(1, 0), RED_PIXEL);
    let [r, _, b, a] = canvas.pixel(2, 0);
    assert!(r.abs_diff(64) <= 2 && b.abs_diff(191) <= 2 && a == 255);
    assert_eq!(canvas.pixel(3, 0), [0; 4]);
    assert_eq!(canvas.pixel(2, 1), [0; 4]);
}

#[test]
fn square_tiles_meet_without_a_seam() {
    let tile = picture(1, 1, &[WHITE]);
    let mut canvas = Canvas::new(8, 1);
    canvas.draw_sized(&tile, 0.3, 0.0, 3.4, 1.0, &Brush::default());
    canvas.draw_sized(&tile, 3.7, 0.0, 3.4, 1.0, &Brush::default());
    assert!(
        (0..7).all(|x| canvas.pixel(x, 0) == WHITE),
        "{:?}",
        canvas.to_rgba()
    );
}

#[test]
fn smoothing_off_keeps_pixels_hard_as_they_grow() {
    let pair = picture(2, 1, &[RED_PIXEL, BLUE]);
    let mut canvas = Canvas::new(8, 1);
    canvas.draw_sized(&pair, 0.0, 0.0, 8.0, 1.0, &sharp());
    for x in 0..4 {
        assert_eq!(canvas.pixel(x, 0), RED_PIXEL);
        assert_eq!(canvas.pixel(x + 4, 0), BLUE);
    }

    let mut smooth = Canvas::new(8, 1);
    smooth.draw_sized(&pair, 0.0, 0.0, 8.0, 1.0, &Brush::default());
    let [r, _, b, _] = smooth.pixel(4, 0);
    assert!(r > 20 && b > 20, "the middle mixes when smoothed");

    let mut cubic = Canvas::new(8, 1);
    cubic.draw_sized(
        &pair,
        0.0,
        0.0,
        8.0,
        1.0,
        &Brush::default().smoothing(Smoothing::Bicubic),
    );
    assert_eq!(cubic.pixel(0, 0)[..3], RED_PIXEL[..3]);
}

#[test]
fn a_picture_grows_to_the_box_it_is_given() {
    let mut canvas = Canvas::new(8, 8);
    canvas.draw_sized(&four(), 0.0, 0.0, 8.0, 8.0, &sharp());
    assert_eq!(canvas.pixel(1, 1), RED_PIXEL);
    assert_eq!(canvas.pixel(6, 1), GREEN);
    assert_eq!(canvas.pixel(1, 6), BLUE);
    assert_eq!(canvas.pixel(6, 6), WHITE);
}

#[test]
fn a_sprite_turns_round_its_centre() {
    // A red pixel at the top middle of a three by three sprite.
    let mut pixels = [[0; 4]; 9];
    pixels[1] = RED_PIXEL;
    let sprite = picture(3, 3, &pixels);
    let mut canvas = Canvas::new(9, 9);
    let turned = Transform::IDENTITY.translate(4.5, 4.5).rotate(FRAC_PI_2);
    canvas.draw(&sprite, -1.5, -1.5, &sharp().at(turned));

    // A quarter turn clockwise takes the top middle to the right middle.
    assert_eq!(canvas.pixel(5, 4), RED_PIXEL);
    assert_eq!(canvas.pixel(4, 3), [0; 4]);
}

#[test]
fn part_of_a_picture_reads_nothing_outside_its_box() {
    let sheet = picture(
        4,
        2,
        &[
            RED_PIXEL, RED_PIXEL, BLUE, BLUE, RED_PIXEL, RED_PIXEL, BLUE, BLUE,
        ],
    );
    let mut canvas = Canvas::new(8, 8);
    canvas.draw_part(
        &sheet,
        [2.0, 0.0, 2.0, 2.0],
        [0.0, 0.0, 8.0, 8.0],
        &Brush::default(),
    );
    for (x, y) in [(0, 0), (7, 0), (0, 7), (4, 4), (7, 7)] {
        assert_eq!(canvas.pixel(x, y), BLUE, "({x}, {y})");
    }

    let mut offset = Canvas::new(8, 8);
    offset.draw_part(&sheet, [1.0, 0.0, 2.0, 1.0], [2.0, 2.0, 4.0, 2.0], &sharp());
    assert_eq!(offset.pixel(2, 2), RED_PIXEL);
    assert_eq!(offset.pixel(5, 3), BLUE);
    assert_eq!(offset.pixel(1, 2), [0; 4]);
    assert_eq!(offset.pixel(6, 2), [0; 4]);
}

#[test]
fn a_turned_sprite_has_soft_edges() {
    let block = picture(10, 10, &[WHITE; 100]);
    let mut canvas = Canvas::new(20, 20);
    let turned = Transform::IDENTITY.translate(10.0, 10.0).rotate(FRAC_PI_4);
    canvas.draw(&block, -5.0, -5.0, &Brush::default().at(turned));
    let partly = (0..20)
        .filter(|&x| (40..215).contains(&alpha(&canvas, x, 10)))
        .count();
    assert!(partly >= 2, "the diamond's edge is anti-aliased");
    assert!(near(coverage(&canvas), 100.0, 2.0), "{}", coverage(&canvas));
}

#[test]
fn a_sprite_lands_by_its_brush() {
    let block = picture(4, 4, &[[200, 0, 0, 255]; 16]);
    let mut faint = Canvas::new(8, 8);
    faint.draw(&block, 0.0, 0.0, &Brush::default().alpha(0.5));
    assert!(alpha(&faint, 1, 1).abs_diff(128) <= 1);

    let mut lit = Canvas::new(8, 8);
    lit.fill_rect(0.0, 0.0, 8.0, 8.0, &Brush::new(Colour::rgb(0, 100, 0)));
    lit.draw(&block, 0.0, 0.0, &Brush::default().blend(Blend::Lighter));
    assert_eq!(lit.pixel(1, 1), [200, 100, 0, 255]);

    let mut silhouette = Canvas::new(16, 16);
    silhouette.draw(
        &block,
        6.0,
        6.0,
        &Brush::default().brightness(0.0).blur(1.5).alpha(0.3),
    );
    let [r, g, b, a] = silhouette.pixel(7, 7);
    assert!(r == 0 && g == 0 && b == 0 && a > 40);
    assert!(
        alpha(&silhouette, 4, 7) > 0,
        "the blur spreads past the sprite"
    );

    let mut clipped = Canvas::new(8, 8);
    let mut left = Path::new();
    left.rect(0.0, 0.0, 2.0, 8.0);
    clipped.with_clip(&left, Transform::IDENTITY, |canvas| {
        canvas.draw(&block, 0.0, 0.0, &Brush::default());
    });
    assert!(inked(&clipped, 1, 1) && clear(&clipped, 2, 1));
}

#[test]
fn an_empty_or_upside_down_box_is_handled() {
    let mut canvas = Canvas::new(8, 8);
    canvas.draw_sized(&four(), 0.0, 0.0, 0.0, 4.0, &Brush::default());
    canvas.draw_part(
        &four(),
        [0.0, 0.0, 0.0, 1.0],
        [0.0, 0.0, 4.0, 4.0],
        &Brush::default(),
    );
    canvas.draw_part(
        &four(),
        [5.0, 5.0, 2.0, 2.0],
        [0.0, 0.0, 4.0, 4.0],
        &Brush::default(),
    );
    assert_eq!(coverage(&canvas), 0.0);

    // A negative width flips the picture, as a canvas does.
    canvas.draw_sized(&four(), 4.0, 0.0, -4.0, 4.0, &sharp());
    assert_eq!(canvas.pixel(0, 0), GREEN);
    assert_eq!(canvas.pixel(3, 0), RED_PIXEL);
}
