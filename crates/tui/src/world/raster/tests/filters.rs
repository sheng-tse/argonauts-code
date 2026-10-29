use super::*;

fn dot_at_centre() -> Canvas {
    let mut rgba = vec![0; 21 * 21 * 4];
    let centre = (10 * 21 + 10) * 4;
    rgba[centre..centre + 4].copy_from_slice(&[255; 4]);
    Canvas::from_rgba(21, 21, &rgba).expect("21 by 21")
}

fn box_path(x: f64, y: f64, size: f64) -> Path {
    let mut path = Path::new();
    path.rect(x, y, size, size);
    path
}

#[test]
fn a_blur_spreads_a_pixel_evenly_and_keeps_its_ink() {
    let mut canvas = dot_at_centre();
    canvas.blur(1.5);

    assert!(near(coverage(&canvas), 1.0, 0.06), "{}", coverage(&canvas));
    for k in 1..=3 {
        assert_eq!(alpha(&canvas, 10 - k, 10), alpha(&canvas, 10 + k, 10));
        assert_eq!(alpha(&canvas, 10, 10 - k), alpha(&canvas, 10, 10 + k));
        assert_eq!(alpha(&canvas, 10 + k, 10), alpha(&canvas, 10, 10 + k));
    }
    // Three boxes of three reach three pixels out and no further.
    assert!(alpha(&canvas, 13, 10) > 0 && alpha(&canvas, 14, 10) == 0);
    assert!(alpha(&canvas, 10, 10) > alpha(&canvas, 11, 10));
    assert_eq!(
        canvas.pixel(10, 10)[..3],
        [255, 255, 255],
        "a white dot stays white"
    );
}

#[test]
fn a_blurred_edge_falls_off_as_a_gaussian_does() {
    let mut canvas = Canvas::new(40, 40);
    canvas.fill_rect(-10.0, -10.0, 30.0, 60.0, &Brush::new(RED));
    canvas.blur(3.0);
    let level = |x: u32| f64::from(alpha(&canvas, x, 20)) / 255.0;

    // The pixel centres at 19.5 and 20.5 sit half a pixel either side of the edge, and a
    // Gaussian of 3 is at about 0.80 and 0.20 two and a half pixels in and out.
    assert!(near(level(19) + level(20), 1.0, 0.02));
    assert!(near(level(17), 0.80, 0.04), "{}", level(17));
    assert!(near(level(22), 0.20, 0.04), "{}", level(22));
    assert!(level(10) > 0.99 && level(30) < 0.01);
}

#[test]
fn brightness_scales_colour_and_leaves_alpha() {
    let rgba = [[200, 100, 0, 255], [255, 0, 0, 128]].concat();
    let mut canvas = Canvas::from_rgba(2, 1, &rgba).expect("two pixels");
    let mut silhouette = canvas.clone();
    silhouette.brighten(0.0);
    assert_eq!(silhouette.pixel(0, 0), [0, 0, 0, 255]);
    assert_eq!(silhouette.pixel(1, 0), [0, 0, 0, 128]);

    canvas.brighten(1.25);
    assert_eq!(canvas.pixel(0, 0), [250, 125, 0, 255]);
    assert_eq!(canvas.pixel(1, 0), [255, 0, 0, 128], "held at white");
}

#[test]
fn a_blurred_draw_softens_by_canvas_pixels_whatever_its_transform() {
    let mut plain = Canvas::new(48, 48);
    plain.fill(&box_path(14.0, 14.0, 20.0), &Brush::new(RED).blur(2.0));
    assert!(inked(&plain, 24, 24));
    assert!(
        near(f64::from(alpha(&plain, 14, 24)), 160.0, 25.0),
        "{}",
        alpha(&plain, 14, 24)
    );
    assert!(alpha(&plain, 11, 24) > 0 && alpha(&plain, 8, 24) == 0);

    let mut scaled = Canvas::new(48, 48);
    let doubled = Transform::IDENTITY.scale(2.0, 2.0);
    scaled.fill(
        &box_path(7.0, 7.0, 10.0),
        &Brush::new(RED).blur(2.0).at(doubled),
    );
    for x in 8..16 {
        assert!(
            alpha(&scaled, x, 24).abs_diff(alpha(&plain, x, 24)) <= 2,
            "column {x}"
        );
    }
}

#[test]
fn a_blurred_draw_takes_colour_from_beyond_the_canvas_edge() {
    let mut canvas = Canvas::new(20, 20);
    canvas.fill(&box_path(-20.0, 0.0, 30.0), &Brush::new(RED).blur(2.0));
    assert!(alpha(&canvas, 0, 10) > 250, "{}", alpha(&canvas, 0, 10));
}

#[test]
fn a_silhouette_draws_a_dark_shape_of_the_same_outline() {
    let mut canvas = Canvas::new(16, 16);
    let mut dot = Path::new();
    dot.circle(8.0, 8.0, 5.0);
    canvas.fill(
        &dot,
        &Brush::new(Colour::rgb(250, 200, 10))
            .brightness(0.0)
            .alpha(0.3),
    );
    let [r, g, b, a] = canvas.pixel(8, 8);
    assert!(r == 0 && g == 0 && b == 0 && a.abs_diff(77) <= 1);
    assert_eq!(canvas.pixel(0, 0), [0; 4]);
}

#[test]
fn a_glow_spreads_round_a_line_in_its_own_colour() {
    let mut line = Path::new();
    line.move_to(4.0, 16.0);
    line.line_to(28.0, 16.0);
    let glow = Colour::rgb(120, 200, 255);
    let mut canvas = Canvas::new(32, 32);
    canvas.stroke(
        &line,
        &Pen::new(2.0).round(),
        &Brush::new(glow).glow(glow, 8.0),
    );

    assert_eq!(canvas.pixel(16, 16), [120, 200, 255, 255]);
    let halo = canvas.pixel(16, 19);
    assert!(halo[3] > 20 && halo[3] < 200, "{halo:?}");
    // Faint pixels keep their colour to within the rounding of their small alpha.
    let tint = [120, 200, 255];
    assert!(
        halo.iter()
            .zip(tint)
            .all(|(&got, want)| got.abs_diff(want) <= 4),
        "{halo:?}"
    );

    let mut plain = Canvas::new(32, 32);
    plain.stroke(&line, &Pen::new(2.0).round(), &Brush::new(glow));
    assert_eq!(alpha(&plain, 16, 19), 0);

    let mut unblurred = Canvas::new(32, 32);
    unblurred.stroke(
        &line,
        &Pen::new(2.0).round(),
        &Brush::new(glow).glow(glow, 0.0),
    );
    assert_eq!(unblurred.to_rgba(), plain.to_rgba(), "no blur, no glow");
}

#[test]
fn a_filtered_draw_off_the_canvas_draws_nothing() {
    let mut canvas = Canvas::new(16, 16);
    canvas.fill(&box_path(100.0, 100.0, 10.0), &Brush::new(RED).blur(2.0));
    canvas.fill(
        &box_path(-40.0, 4.0, 10.0),
        &Brush::new(RED).brightness(0.5),
    );
    assert_eq!(coverage(&canvas), 0.0);
}

#[test]
fn a_blurred_keep_inside_still_clears_everywhere_else() {
    let mut canvas = Canvas::new(32, 32);
    canvas.fill_rect(0.0, 0.0, 32.0, 32.0, &Brush::new(RED));
    let mut dot = Path::new();
    dot.circle(16.0, 16.0, 6.0);
    canvas.fill(
        &dot,
        &Brush::new(Colour::WHITE)
            .blend(Blend::DestinationIn)
            .blur(1.5),
    );
    assert_eq!(canvas.pixel(16, 16), [255, 0, 0, 255]);
    assert_eq!(canvas.pixel(0, 0), [0; 4]);
    let edge = alpha(&canvas, 22, 16);
    assert!(edge > 0 && edge < 255, "the kept edge is soft: {edge}");
}
