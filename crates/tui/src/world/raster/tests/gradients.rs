use std::f64::consts::FRAC_PI_2;

use super::*;

const BLUE: Colour = Colour::rgb(0, 0, 255);

fn painted(width: u32, height: u32, brush: &Brush) -> Canvas {
    let mut canvas = Canvas::new(width, height);
    canvas.fill_rect(0.0, 0.0, f64::from(width), f64::from(height), brush);
    canvas
}

/// Whether two straight pixels are within `within` of each other in every channel.
fn close(pixel: [u8; 4], expected: [u8; 4], within: u8) -> bool {
    pixel
        .iter()
        .zip(expected)
        .all(|(a, b)| a.abs_diff(b) <= within)
}

#[test]
fn a_linear_gradient_runs_from_its_first_colour_to_its_last() {
    let gradient = Gradient::linear(0.0, 0.0, 10.0, 0.0)
        .stop(0.0, RED)
        .stop(1.0, BLUE);
    let canvas = painted(10, 1, &Brush::new(gradient));

    // Each pixel shows the gradient at its centre.
    assert!(
        close(canvas.pixel(0, 0), [242, 0, 13, 255], 2),
        "{:?}",
        canvas.pixel(0, 0)
    );
    assert!(
        close(canvas.pixel(4, 0), [140, 0, 115, 255], 2),
        "{:?}",
        canvas.pixel(4, 0)
    );
    assert!(
        close(canvas.pixel(9, 0), [13, 0, 242, 255], 2),
        "{:?}",
        canvas.pixel(9, 0)
    );
}

#[test]
fn past_its_ends_a_gradient_keeps_its_end_colours() {
    let gradient = Gradient::linear(4.0, 0.0, 6.0, 0.0)
        .stop(0.0, RED)
        .stop(1.0, BLUE);
    let canvas = painted(10, 1, &Brush::new(gradient));
    assert_eq!(canvas.pixel(0, 0), [255, 0, 0, 255]);
    assert_eq!(canvas.pixel(9, 0), [0, 0, 255, 255]);
}

#[test]
fn stops_sort_by_offset_and_two_at_one_offset_make_a_hard_edge() {
    let backwards = Gradient::linear(0.0, 0.0, 10.0, 0.0)
        .stop(1.0, BLUE)
        .stop(0.0, RED);
    let forwards = Gradient::linear(0.0, 0.0, 10.0, 0.0)
        .stop(0.0, RED)
        .stop(1.0, BLUE);
    assert_eq!(
        painted(10, 1, &Brush::new(backwards)).to_rgba(),
        painted(10, 1, &Brush::new(forwards)).to_rgba()
    );

    let hard = Gradient::linear(0.0, 0.0, 10.0, 0.0)
        .stop(0.0, RED)
        .stop(0.5, RED)
        .stop(0.5, BLUE)
        .stop(1.0, BLUE);
    let canvas = painted(10, 1, &Brush::new(hard));
    assert_eq!(canvas.pixel(4, 0), [255, 0, 0, 255]);
    assert_eq!(canvas.pixel(5, 0), [0, 0, 255, 255]);
}

#[test]
fn a_gradient_with_nowhere_to_run_paints_nothing() {
    let no_stops = Gradient::linear(0.0, 0.0, 10.0, 0.0);
    let no_length = Gradient::linear(3.0, 3.0, 3.0, 3.0)
        .stop(0.0, RED)
        .stop(1.0, BLUE);
    let one_circle = Gradient::radial(3.0, 3.0, 2.0, 3.0, 3.0, 2.0).stop(0.0, RED);
    for gradient in [no_stops, no_length, one_circle] {
        assert_eq!(coverage(&painted(6, 6, &Brush::new(gradient))), 0.0);
    }

    let one_stop = Gradient::linear(0.0, 0.0, 10.0, 0.0).stop(0.3, RED);
    assert_eq!(
        painted(6, 6, &Brush::new(one_stop)).pixel(5, 5),
        [255, 0, 0, 255]
    );
}

#[test]
fn a_radial_gradient_spreads_from_its_inner_circle_to_its_outer() {
    let glow = Gradient::radial(16.0, 16.0, 0.0, 16.0, 16.0, 10.0)
        .stop(0.0, Colour::WHITE)
        .stop(1.0, Colour::WHITE.with_alpha(0.0));
    let canvas = painted(32, 32, &Brush::new(glow));
    assert!(alpha(&canvas, 16, 16) > 230);
    // Halfway out, by the pixel's centre at 5.5 from the middle.
    assert!(near(
        f64::from(alpha(&canvas, 21, 16)),
        255.0 * (1.0 - 5.5 / 10.0),
        4.0
    ));
    assert_eq!(alpha(&canvas, 28, 16), 0);

    let ring = Gradient::radial(16.0, 16.0, 4.0, 16.0, 16.0, 12.0)
        .stop(0.0, RED)
        .stop(1.0, BLUE);
    let canvas = painted(32, 32, &Brush::new(ring));
    assert_eq!(
        canvas.pixel(17, 16),
        [255, 0, 0, 255],
        "inside the inner circle"
    );
    // 8.5 out from the middle is 0.56 of the way from the inner circle to the outer.
    assert!(
        close(canvas.pixel(24, 16), [111, 0, 144, 255], 4),
        "{:?}",
        canvas.pixel(24, 16)
    );
}

#[test]
fn a_radial_gradient_between_circles_apart_paints_only_their_cone() {
    // A light falling from a small circle on the left to a bigger one on the right: the
    // cone opens rightwards, and behind the small circle nothing is painted.
    let beam = Gradient::radial(8.0, 16.0, 2.0, 24.0, 16.0, 8.0)
        .stop(0.0, RED)
        .stop(1.0, BLUE);
    let canvas = painted(32, 32, &Brush::new(beam));
    assert_eq!(alpha(&canvas, 1, 16), 0);
    assert!(inked(&canvas, 24, 16));
    assert_eq!(alpha(&canvas, 8, 2), 0, "above the cone's edge");
}

#[test]
fn a_conic_gradient_turns_clockwise_from_its_angle() {
    let turn = |angle: f64| {
        Gradient::conic(angle, 16.0, 16.0)
            .stop(0.0, Colour::BLACK)
            .stop(1.0, Colour::WHITE)
    };
    let canvas = painted(32, 32, &Brush::new(turn(0.0)));
    let level = |x: u32, y: u32| canvas.pixel(x, y)[0];
    // Below the centre is a quarter of the way round, left half, above three quarters.
    assert!(level(28, 16) < 10, "{}", level(28, 16));
    assert!(
        near(f64::from(level(16, 28)), 64.0, 6.0),
        "{}",
        level(16, 28)
    );
    assert!(
        near(f64::from(level(4, 16)), 128.0, 6.0),
        "{}",
        level(4, 16)
    );
    assert!(
        near(f64::from(level(16, 4)), 191.0, 6.0),
        "{}",
        level(16, 4)
    );

    let canvas = painted(32, 32, &Brush::new(turn(FRAC_PI_2)));
    assert!(
        canvas.pixel(15, 28)[0] < 10,
        "a quarter turn starts it below"
    );
}

#[test]
fn a_gradient_moves_with_the_brush_and_fades_with_it() {
    let gradient = Gradient::linear(0.0, 0.0, 10.0, 0.0)
        .stop(0.0, RED)
        .stop(1.0, BLUE);
    let mut canvas = Canvas::new(20, 1);
    let moved = Brush::new(gradient)
        .at(Transform::IDENTITY.translate(10.0, 0.0))
        .alpha(0.5);
    canvas.fill_rect(0.0, 0.0, 10.0, 1.0, &moved);

    assert_eq!(alpha(&canvas, 9, 0), 0);
    let [r, g, b, a] = canvas.pixel(10, 0);
    assert!(r > 230 && g == 0 && b < 20 && a.abs_diff(128) <= 1);
    let [r, _, b, _] = canvas.pixel(19, 0);
    assert!(r < 20 && b > 230);
}
