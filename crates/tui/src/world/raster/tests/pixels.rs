use super::*;
use crate::world::scene::Image;

#[test]
fn straight_pixels_come_back_as_they_went_in() {
    let rgba = [
        [10, 20, 30, 255],
        [200, 100, 50, 128],
        [0, 0, 0, 0],
        [255, 255, 255, 1],
    ]
    .concat();
    let canvas = Canvas::from_rgba(2, 2, &rgba).expect("four pixels fill two by two");
    assert_eq!(canvas.pixel(0, 0), [10, 20, 30, 255]);
    assert_eq!(canvas.pixel(0, 1), [0; 4]);

    let back = canvas.to_rgba();
    let [r, g, b, a] = canvas.pixel(1, 0);
    assert_eq!(a, 128);
    assert!(
        [r.abs_diff(200), g.abs_diff(100), b.abs_diff(50)]
            .iter()
            .all(|&d| d <= 1)
    );
    assert_eq!(back.len(), 16);
    assert_eq!(&back[..4], &[10, 20, 30, 255]);
}

#[test]
fn pixels_of_the_wrong_length_change_nothing() {
    assert!(Canvas::from_rgba(2, 2, &[255; 12]).is_none());
    let mut canvas = Canvas::new(2, 2);
    assert!(!canvas.put_rgba(&[255; 20]));
    assert_eq!(canvas.to_rgba(), vec![0; 16]);
    assert!(canvas.put_rgba(&[255; 16]));
    assert_eq!(canvas.pixel(1, 1), [255; 4]);
}

#[test]
fn premultiplied_pixels_are_the_drawn_ones() {
    let mut canvas = Canvas::new(1, 1);
    canvas.fill_rect(
        0.0,
        0.0,
        1.0,
        1.0,
        &Brush::new(Colour::rgba(200, 100, 0, 0.5)),
    );
    let [r, g, b, a] = canvas.premultiplied()[..4] else {
        panic!("one pixel");
    };
    assert!(a.abs_diff(128) <= 1 && r.abs_diff(100) <= 1 && g.abs_diff(50) <= 1 && b == 0);

    canvas.premultiplied_mut().copy_from_slice(&[0, 64, 0, 128]);
    let [r, g, b, a] = canvas.pixel(0, 0);
    assert!(r == 0 && g.abs_diff(128) <= 1 && b == 0 && a == 128);
}

#[test]
fn the_picture_and_its_png_carry_the_pixels() {
    let mut canvas = Canvas::new(6, 4);
    canvas.fill_rect(
        1.0,
        1.0,
        3.0,
        2.0,
        &Brush::new(Colour::rgba(20, 40, 60, 0.75)),
    );

    let image = canvas.to_image();
    assert_eq!((image.width(), image.height()), (6, 4));
    assert_eq!(image.pixels(), canvas.to_rgba().as_slice());

    let decoded = Image::from_png(&canvas.to_png()).expect("the encoder's PNG reads back");
    assert_eq!(decoded, image);
    let again = Canvas::from_image(&decoded).expect("the sizes fit");
    assert_eq!(again.to_rgba(), canvas.to_rgba());
}

#[test]
fn a_canvas_asked_for_no_pixels_still_holds_one() {
    let canvas = Canvas::new(0, 5);
    assert_eq!((canvas.width(), canvas.height()), (1, 5));
    assert_eq!(canvas.pixel(3, 3), [0; 4], "outside reads clear");
}

#[test]
fn clearing_empties_every_pixel() {
    let mut canvas = Canvas::new(3, 3);
    canvas.fill_rect(0.0, 0.0, 3.0, 3.0, &Brush::new(RED));
    canvas.clear();
    assert_eq!(coverage(&canvas), 0.0);
}

#[test]
fn colours_read_as_a_stylesheet_reads_them() {
    assert_eq!(
        Colour::of([12.7, -3.0, 300.0], 0.5),
        Colour::rgba(12, 0, 255, 0.5)
    );
    assert_eq!(Colour::of([1.0, 2.0, 3.0], 7.0).a, 1.0);
    assert_eq!(
        Colour::of([f64::NAN, 2.0, 3.0], f64::NAN),
        Colour::rgba(0, 2, 3, 0.0)
    );
    assert_eq!(Colour::hex(0x1f202f), Colour::rgb(31, 32, 47));
    assert_eq!(
        Colour::WHITE.with_alpha(-1.0),
        Colour::rgba(255, 255, 255, 0.0)
    );
}
