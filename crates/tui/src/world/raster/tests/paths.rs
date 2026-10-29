use std::f64::consts::{FRAC_PI_2, PI, TAU};

use super::*;

fn filled(width: u32, height: u32, path: &Path) -> Canvas {
    let mut canvas = Canvas::new(width, height);
    canvas.fill(path, &Brush::new(RED));
    canvas
}

#[test]
fn a_box_fills_its_pixels_and_no_more() {
    let mut canvas = Canvas::new(10, 10);
    canvas.fill_rect(2.0, 2.0, 4.0, 3.0, &Brush::new(RED));

    assert_eq!(canvas.pixel(2, 2), [255, 0, 0, 255]);
    assert_eq!(canvas.pixel(5, 4), [255, 0, 0, 255]);
    for (x, y) in [(1, 2), (6, 2), (2, 1), (2, 5)] {
        assert_eq!(canvas.pixel(x, y), [0; 4], "({x}, {y}) stays clear");
    }
}

#[test]
fn an_edge_halfway_through_a_pixel_covers_it_half() {
    let mut canvas = Canvas::new(4, 1);
    canvas.fill_rect(0.0, 0.0, 2.5, 1.0, &Brush::new(Colour::WHITE));

    assert!(near(f64::from(alpha(&canvas, 2, 0)), 127.5, 3.0));
    assert_eq!(canvas.pixel(2, 0)[..3], [255, 255, 255]);
}

#[test]
fn a_circle_covers_its_area() {
    let mut circle = Path::new();
    circle.circle(16.0, 16.0, 10.0);
    let canvas = filled(32, 32, &circle);

    assert!(
        near(coverage(&canvas), PI * 100.0, 3.2),
        "{}",
        coverage(&canvas)
    );
    assert!(inked(&canvas, 16, 16) && inked(&canvas, 25, 16) && clear(&canvas, 27, 16));
}

#[test]
fn an_arc_turns_clockwise_down_the_screen_unless_asked_otherwise() {
    let mut lower = Path::new();
    lower.arc(16.0, 16.0, 10.0, 0.0, PI, false);
    lower.close();
    let canvas = filled(32, 32, &lower);
    assert!(inked(&canvas, 16, 22) && clear(&canvas, 16, 10));

    let mut upper = Path::new();
    upper.arc(16.0, 16.0, 10.0, 0.0, PI, true);
    upper.close();
    let canvas = filled(32, 32, &upper);
    assert!(inked(&canvas, 16, 10) && clear(&canvas, 16, 22));
}

#[test]
fn an_arc_from_a_bigger_angle_to_a_smaller_goes_on_round() {
    // Clockwise from three quarters of a turn to a quarter: up through the right side.
    let mut right = Path::new();
    right.arc(16.0, 16.0, 10.0, 3.0 * FRAC_PI_2, FRAC_PI_2, false);
    right.close();
    let canvas = filled(32, 32, &right);
    assert!(inked(&canvas, 22, 16) && clear(&canvas, 10, 16));
}

#[test]
fn a_full_turn_or_more_draws_the_whole_circle() {
    for (from, to, anticlockwise) in [(0.0, 3.0 * PI, false), (TAU, 0.0, false), (0.0, -TAU, true)]
    {
        let mut path = Path::new();
        path.arc(16.0, 16.0, 10.0, from, to, anticlockwise);
        let canvas = filled(32, 32, &path);
        assert!(near(coverage(&canvas), PI * 100.0, 3.2), "{from} to {to}");
    }
}

#[test]
fn an_arc_is_joined_to_the_pen_by_a_line() {
    let mut path = Path::new();
    path.move_to(2.0, 2.0);
    path.arc(24.0, 24.0, 4.0, 0.0, FRAC_PI_2, false);
    let mut canvas = Canvas::new(32, 32);
    canvas.stroke(&path, &Pen::new(2.0), &Brush::new(RED));

    // Halfway along the line from (2, 2) to the arc's start at (28, 24).
    assert!(alpha(&canvas, 15, 13) > 100);
}

#[test]
fn curves_bend_toward_their_control_points() {
    // A quadratic's middle is halfway between its control point and the middle of its ends.
    let mut quad = Path::new();
    quad.move_to(0.0, 20.0);
    quad.quad_to(16.0, -12.0, 32.0, 20.0);
    quad.close();
    let canvas = filled(32, 24, &quad);
    assert!(inked(&canvas, 16, 5) && clear(&canvas, 16, 2));

    // A cubic pulled up then down crosses its chord.
    let mut cubic = Path::new();
    cubic.move_to(0.0, 16.0);
    cubic.cubic_to(10.0, -8.0, 22.0, 40.0, 32.0, 16.0);
    cubic.line_to(32.0, 0.0);
    cubic.line_to(0.0, 0.0);
    cubic.close();
    let canvas = filled(32, 32, &cubic);
    assert!(inked(&canvas, 8, 6) && clear(&canvas, 8, 12) && inked(&canvas, 24, 18));
}

#[test]
fn a_curve_with_no_pen_starts_at_its_first_control_point() {
    let mut path = Path::new();
    path.quad_to(4.0, 4.0, 28.0, 4.0);
    let mut canvas = Canvas::new(32, 8);
    canvas.stroke(&path, &Pen::new(2.0), &Brush::new(RED));
    assert!(inked(&canvas, 16, 4) && clear(&canvas, 2, 4));
}

#[test]
fn an_ellipse_turns_with_its_rotation() {
    let mut tall = Path::new();
    tall.ellipse(16.0, 16.0, 12.0, 3.0, FRAC_PI_2, 0.0, TAU, false);
    let canvas = filled(32, 32, &tall);
    assert!(inked(&canvas, 16, 6) && clear(&canvas, 6, 16));
    assert!(near(coverage(&canvas), PI * 36.0, 1.2));
}

#[test]
fn rounded_corners_leave_the_very_corner_clear() {
    let mut even = Path::new();
    even.round_rect(0.0, 0.0, 20.0, 20.0, 6.0);
    let canvas = filled(20, 20, &even);
    assert!(clear(&canvas, 0, 0) && clear(&canvas, 19, 19));
    assert!(inked(&canvas, 10, 0) && inked(&canvas, 10, 10));

    let mut top = Path::new();
    top.round_rect(0.0, 0.0, 20.0, 20.0, [6.0, 6.0, 0.0, 0.0]);
    let canvas = filled(20, 20, &top);
    assert!(clear(&canvas, 0, 0) && inked(&canvas, 0, 19) && inked(&canvas, 19, 19));
}

#[test]
fn corners_too_big_for_their_box_shrink_together() {
    let mut stadium = Path::new();
    stadium.round_rect(0.0, 0.0, 20.0, 10.0, 50.0);
    let canvas = filled(20, 10, &stadium);
    assert!(clear(&canvas, 0, 0) && inked(&canvas, 10, 0) && inked(&canvas, 1, 5));
    assert!(near(coverage(&canvas), 10.0 * 10.0 + PI * 25.0, 1.8));
}

#[test]
fn a_box_leaves_the_pen_at_its_top_left_corner() {
    let mut path = Path::new();
    path.rect(2.0, 2.0, 4.0, 4.0);
    path.line_to(28.0, 2.0);
    let mut canvas = Canvas::new(32, 8);
    canvas.stroke(&path, &Pen::new(2.0), &Brush::new(RED));
    assert!(inked(&canvas, 16, 2));
}

#[test]
fn closing_returns_to_where_the_subpath_began() {
    let mut path = Path::new();
    path.move_to(2.0, 2.0);
    path.line_to(28.0, 2.0);
    path.line_to(28.0, 28.0);
    path.close();
    path.line_to(2.0, 28.0);
    let mut canvas = Canvas::new(32, 32);
    canvas.stroke(&path, &Pen::new(2.0), &Brush::new(RED));

    // The closing edge runs corner to corner, and the next line starts back at (2, 2).
    assert!(inked(&canvas, 15, 15));
    assert!(inked(&canvas, 2, 15));
}

#[test]
fn a_transform_moves_and_sizes_what_is_drawn() {
    let mut unit = Path::new();
    unit.rect(0.0, 0.0, 1.0, 1.0);
    let mut canvas = Canvas::new(16, 16);
    let placed = Transform::IDENTITY.translate(8.0, 4.0).scale(4.0, 2.0);
    canvas.fill(&unit, &Brush::new(RED).at(placed));

    assert!(inked(&canvas, 8, 4) && inked(&canvas, 11, 5));
    assert!(clear(&canvas, 12, 4) && clear(&canvas, 8, 6) && clear(&canvas, 7, 4));
}

#[test]
fn transform_steps_apply_to_what_is_drawn_inside_them() {
    let near_point =
        |(x, y): (f64, f64), (ex, ey): (f64, f64)| near(x, ex, 1e-9) && near(y, ey, 1e-9);
    let moved = Transform::IDENTITY.translate(10.0, 0.0).scale(2.0, 1.0);
    assert!(near_point(moved.apply(1.0, 0.0), (12.0, 0.0)));

    let turned = Transform::IDENTITY.rotate(FRAC_PI_2);
    assert!(
        near_point(turned.apply(1.0, 0.0), (0.0, 1.0)),
        "a quarter turn clockwise on the screen"
    );

    let around = Transform::IDENTITY
        .translate(5.0, 5.0)
        .rotate(PI)
        .translate(-5.0, -5.0);
    assert!(near_point(around.apply(6.0, 5.0), (4.0, 5.0)));
    assert!(near_point(
        Transform::new(1.0, 0.0, 0.0, 1.0, 3.0, 4.0).apply(0.0, 0.0),
        (3.0, 4.0)
    ));
}

#[test]
fn an_empty_or_flat_path_draws_nothing() {
    let mut canvas = Canvas::new(8, 8);
    canvas.fill(&Path::new(), &Brush::new(RED));
    let mut flat = Path::new();
    flat.move_to(1.0, 4.0);
    flat.line_to(7.0, 4.0);
    canvas.fill(&flat, &Brush::new(RED));
    assert_eq!(coverage(&canvas), 0.0);
}
