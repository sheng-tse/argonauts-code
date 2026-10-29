use super::*;

const BLUE: Colour = Colour::rgb(0, 0, 255);

fn canvas_of(colour: Colour) -> Canvas {
    let mut canvas = Canvas::new(16, 16);
    canvas.fill_rect(0.0, 0.0, 16.0, 16.0, &Brush::new(colour));
    canvas
}

fn circle() -> Path {
    let mut path = Path::new();
    path.circle(8.0, 8.0, 4.0);
    path
}

#[test]
fn on_top_mixes_by_the_draw_s_alpha() {
    let mut canvas = canvas_of(RED);
    canvas.fill_rect(0.0, 0.0, 16.0, 16.0, &Brush::new(BLUE).alpha(0.5));
    let [r, g, b, a] = canvas.pixel(3, 3);
    assert!(r.abs_diff(128) <= 1 && g == 0 && b.abs_diff(128) <= 1 && a == 255);
}

#[test]
fn lighter_adds_light_up_to_white() {
    let mut canvas = canvas_of(Colour::rgb(100, 0, 200));
    let lighter = Brush::new(Colour::rgb(0, 100, 200)).blend(Blend::Lighter);
    canvas.fill_rect(0.0, 0.0, 16.0, 16.0, &lighter);
    assert_eq!(canvas.pixel(3, 3), [100, 100, 255, 255]);
}

#[test]
fn screen_lightens_by_what_both_let_through() {
    let grey = Colour::rgb(128, 128, 128);
    let mut canvas = canvas_of(grey);
    canvas.fill_rect(0.0, 0.0, 16.0, 16.0, &Brush::new(grey).blend(Blend::Screen));
    // 1 - (1 - 0.5)² of the way to white.
    let [r, _, _, a] = canvas.pixel(3, 3);
    assert!(r.abs_diff(192) <= 1 && a == 255, "{r}");
}

#[test]
fn atop_draws_only_where_the_canvas_holds_something() {
    let mut canvas = Canvas::new(16, 16);
    canvas.fill_rect(0.0, 0.0, 8.0, 16.0, &Brush::new(RED));
    canvas.fill_rect(
        0.0,
        0.0,
        16.0,
        16.0,
        &Brush::new(BLUE).blend(Blend::SourceAtop),
    );
    assert_eq!(canvas.pixel(3, 3), [0, 0, 255, 255]);
    assert_eq!(canvas.pixel(12, 3), [0; 4]);
}

#[test]
fn keeping_inside_clears_the_whole_canvas_outside_the_draw() {
    let mut canvas = canvas_of(RED);
    canvas.fill(
        &circle(),
        &Brush::new(Colour::WHITE).blend(Blend::DestinationIn),
    );
    assert_eq!(
        canvas.pixel(8, 8),
        [255, 0, 0, 255],
        "the canvas's colour stays inside"
    );
    assert_eq!(canvas.pixel(0, 0), [0; 4]);
    assert_eq!(canvas.pixel(15, 15), [0; 4]);

    let mut faded = canvas_of(RED);
    faded.fill(
        &circle(),
        &Brush::new(Colour::WHITE)
            .blend(Blend::DestinationIn)
            .alpha(0.5),
    );
    assert!(alpha(&faded, 8, 8).abs_diff(128) <= 1);
    assert_eq!(faded.pixel(0, 0), [0; 4]);
}

#[test]
fn rubbing_out_clears_only_under_the_draw() {
    let mut canvas = canvas_of(RED);
    canvas.fill(&circle(), &Brush::new(BLUE).blend(Blend::DestinationOut));
    assert_eq!(canvas.pixel(8, 8), [0; 4]);
    assert_eq!(canvas.pixel(0, 0), [255, 0, 0, 255]);

    let mut half = canvas_of(RED);
    half.fill(
        &circle(),
        &Brush::new(BLUE.with_alpha(0.5)).blend(Blend::DestinationOut),
    );
    assert!(alpha(&half, 8, 8).abs_diff(128) <= 1);
}

#[test]
fn lines_mix_the_same_way_fills_do() {
    let mut canvas = canvas_of(RED);
    let mut line = Path::new();
    line.move_to(0.0, 8.0);
    line.line_to(16.0, 8.0);
    canvas.stroke(
        &line,
        &Pen::new(2.0),
        &Brush::new(Colour::WHITE).blend(Blend::DestinationIn),
    );
    assert_eq!(canvas.pixel(4, 8), [255, 0, 0, 255]);
    assert_eq!(canvas.pixel(4, 3), [0; 4]);
}
