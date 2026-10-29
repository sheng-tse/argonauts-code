use super::*;

const BLUE: Colour = Colour::rgb(0, 0, 255);

fn square(x: f64, y: f64, size: f64) -> Path {
    let mut path = Path::new();
    path.rect(x, y, size, size);
    path
}

fn red_canvas() -> Canvas {
    let mut canvas = Canvas::new(32, 32);
    canvas.fill_rect(0.0, 0.0, 32.0, 32.0, &Brush::new(RED));
    canvas
}

#[test]
fn draws_inside_a_clip_stay_inside_it_until_it_ends() {
    let mut canvas = red_canvas();
    let mut hole = Path::new();
    hole.circle(16.0, 16.0, 8.0);
    canvas.with_clip(&hole, Transform::IDENTITY, |canvas| {
        canvas.fill_rect(0.0, 0.0, 32.0, 32.0, &Brush::new(BLUE));
    });
    assert_eq!(canvas.pixel(16, 16), [0, 0, 255, 255]);
    assert_eq!(
        canvas.pixel(2, 2),
        [255, 0, 0, 255],
        "outside keeps what it had"
    );

    canvas.fill_rect(0.0, 0.0, 4.0, 4.0, &Brush::new(BLUE));
    assert_eq!(
        canvas.pixel(2, 2),
        [0, 0, 255, 255],
        "the clip ended with its draw"
    );
}

#[test]
fn a_clip_inside_a_clip_keeps_only_where_both_let_through() {
    let mut canvas = Canvas::new(32, 32);
    canvas.with_clip(&square(0.0, 0.0, 20.0), Transform::IDENTITY, |canvas| {
        canvas.with_clip(&square(10.0, 10.0, 20.0), Transform::IDENTITY, |canvas| {
            canvas.fill_rect(0.0, 0.0, 32.0, 32.0, &Brush::new(RED));
        });
        canvas.fill_rect(0.0, 0.0, 4.0, 4.0, &Brush::new(BLUE));
    });
    assert!(inked(&canvas, 15, 15));
    assert!(clear(&canvas, 5, 15) && clear(&canvas, 25, 15) && clear(&canvas, 15, 25));
    assert_eq!(
        canvas.pixel(2, 2),
        [0, 0, 255, 255],
        "the outer clip came back"
    );
    assert!(near(coverage(&canvas), 100.0 + 16.0, 0.5));
}

#[test]
fn a_clip_is_placed_by_its_transform() {
    let mut canvas = Canvas::new(32, 32);
    let placed = Transform::IDENTITY.translate(16.0, 0.0).scale(2.0, 2.0);
    canvas.with_clip(&square(0.0, 0.0, 4.0), placed, |canvas| {
        canvas.fill_rect(0.0, 0.0, 32.0, 32.0, &Brush::new(RED));
    });
    assert!(inked(&canvas, 16, 0) && inked(&canvas, 23, 7));
    assert!(clear(&canvas, 15, 0) && clear(&canvas, 24, 0) && clear(&canvas, 16, 8));
}

#[test]
fn a_clip_edge_through_a_pixel_lets_part_of_it_through() {
    let mut canvas = Canvas::new(4, 1);
    let mut half = Path::new();
    half.rect(0.0, 0.0, 2.5, 1.0);
    canvas.with_clip(&half, Transform::IDENTITY, |canvas| {
        canvas.fill_rect(0.0, 0.0, 4.0, 1.0, &Brush::new(RED));
    });
    assert!(near(f64::from(alpha(&canvas, 2, 0)), 127.5, 3.0));
}

#[test]
fn an_empty_clip_lets_nothing_through() {
    let mut canvas = Canvas::new(8, 8);
    canvas.with_clip(&Path::new(), Transform::IDENTITY, |canvas| {
        canvas.fill_rect(0.0, 0.0, 8.0, 8.0, &Brush::new(RED));
    });
    assert_eq!(coverage(&canvas), 0.0);
}

#[test]
fn clipped_lines_blurs_and_keep_inside_all_stop_at_the_clip() {
    let left = square(0.0, 0.0, 16.0);
    let mut left_half = Path::new();
    left_half.rect(0.0, 0.0, 16.0, 32.0);

    let mut blurred = Canvas::new(32, 32);
    blurred.with_clip(&left_half, Transform::IDENTITY, |canvas| {
        canvas.fill(&square(8.0, 8.0, 16.0), &Brush::new(RED).blur(2.0));
    });
    assert!(inked(&blurred, 12, 16) && alpha(&blurred, 16, 16) == 0);

    let mut kept = red_canvas();
    kept.with_clip(&left_half, Transform::IDENTITY, |canvas| {
        canvas.fill(&left, &Brush::new(BLUE).blend(Blend::DestinationIn));
    });
    assert_eq!(kept.pixel(4, 4), [255, 0, 0, 255], "inside the draw");
    assert_eq!(
        kept.pixel(4, 24),
        [0; 4],
        "inside the clip, outside the draw"
    );
    assert_eq!(kept.pixel(24, 24), [255, 0, 0, 255], "outside the clip");

    let mut line = Path::new();
    line.move_to(0.0, 20.0);
    line.line_to(32.0, 20.0);
    let mut stroked = Canvas::new(32, 32);
    stroked.with_clip(&left_half, Transform::IDENTITY, |canvas| {
        canvas.stroke(&line, &Pen::new(4.0), &Brush::new(RED));
    });
    assert!(inked(&stroked, 8, 20) && clear(&stroked, 24, 20));
}

#[test]
fn clearing_inside_a_clip_clears_only_there() {
    let mut canvas = red_canvas();
    canvas.with_clip(&square(0.0, 0.0, 8.0), Transform::IDENTITY, |canvas| {
        canvas.clear()
    });
    assert_eq!(canvas.pixel(4, 4), [0; 4]);
    assert_eq!(canvas.pixel(12, 4), [255, 0, 0, 255]);
}

#[test]
fn a_clip_hands_back_what_its_draw_returns() {
    let mut canvas = Canvas::new(4, 4);
    let drawn = canvas.with_clip(&square(0.0, 0.0, 2.0), Transform::IDENTITY, |canvas| {
        canvas.fill_rect(0.0, 0.0, 4.0, 4.0, &Brush::new(RED));
        coverage(canvas)
    });
    assert!(near(drawn, 4.0, 0.01));
}

#[test]
fn keep_inside_leaves_alone_whatever_the_clip_holds_back() {
    // A clip edge in the middle of a run of pixels, and a draw that covers none of them.
    let mut narrow = Path::new();
    narrow.rect(0.0, 0.0, 5.0, 32.0);
    let mut canvas = red_canvas();
    canvas.with_clip(&narrow, Transform::IDENTITY, |canvas| {
        canvas.fill(
            &square(100.0, 100.0, 4.0),
            &Brush::new(BLUE).blend(Blend::DestinationIn),
        );
    });
    assert_eq!(canvas.pixel(2, 2), [0; 4]);
    assert_eq!(canvas.pixel(8, 2), [255, 0, 0, 255]);

    let mut unclipped = red_canvas();
    unclipped.fill(
        &square(100.0, 100.0, 4.0),
        &Brush::new(BLUE).blend(Blend::DestinationIn),
    );
    assert_eq!(
        coverage(&unclipped),
        0.0,
        "a draw off the canvas keeps nothing"
    );
}
