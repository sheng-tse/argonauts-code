use super::*;

fn line(from: (f64, f64), to: (f64, f64)) -> Path {
    let mut path = Path::new();
    path.move_to(from.0, from.1);
    path.line_to(to.0, to.1);
    path
}

fn stroked(width: u32, height: u32, path: &Path, pen: &Pen) -> Canvas {
    let mut canvas = Canvas::new(width, height);
    canvas.stroke(path, pen, &Brush::new(RED));
    canvas
}

#[test]
fn a_line_is_as_wide_as_its_pen() {
    let canvas = stroked(32, 20, &line((2.0, 10.0), (28.0, 10.0)), &Pen::new(4.0));
    for y in 8..12 {
        assert!(inked(&canvas, 15, y), "row {y} is under the line");
    }
    assert!(clear(&canvas, 15, 7) && clear(&canvas, 15, 12));
}

#[test]
fn round_and_square_ends_reach_past_the_end_and_butt_ends_stop_there() {
    let path = line((10.0, 10.0), (20.0, 10.0));
    let butt = stroked(32, 20, &path, &Pen::new(6.0));
    assert!(clear(&butt, 21, 10) && inked(&butt, 19, 10));

    let round = stroked(32, 20, &path, &Pen::new(6.0).round());
    assert!(inked(&round, 21, 10) && clear(&round, 23, 10) && clear(&round, 22, 7));

    let square = stroked(32, 20, &path, &Pen::new(6.0).cap(Cap::Square));
    assert!(inked(&square, 22, 10) && inked(&square, 22, 7) && clear(&square, 23, 10));
}

#[test]
fn a_sharp_corner_comes_to_a_point_and_others_cut_it_off() {
    let mut corner = Path::new();
    corner.move_to(4.0, 20.0);
    corner.line_to(20.0, 20.0);
    corner.line_to(20.0, 4.0);

    let miter = stroked(32, 32, &corner, &Pen::new(6.0));
    assert!(inked(&miter, 22, 22));
    let bevel = stroked(32, 32, &corner, &Pen::new(6.0).join(Join::Bevel));
    assert!(clear(&bevel, 22, 22));
    let round = stroked(32, 32, &corner, &Pen::new(6.0).join(Join::Round));
    assert!(clear(&round, 22, 22) && inked(&round, 21, 21));
}

#[test]
fn dashes_and_gaps_take_turns_along_the_line() {
    let path = line((0.0, 5.0), (40.0, 5.0));
    let canvas = stroked(40, 10, &path, &Pen::new(2.0).dash(&[6.0, 3.0]));
    // On 0 to 6, off to 9, on to 15, off to 18.
    assert!(inked(&canvas, 3, 5) && clear(&canvas, 7, 5));
    assert!(inked(&canvas, 12, 5) && clear(&canvas, 16, 5));
}

#[test]
fn the_dash_offset_starts_the_line_further_into_the_pattern() {
    let path = line((0.0, 5.0), (40.0, 5.0));
    let canvas = stroked(
        40,
        10,
        &path,
        &Pen::new(2.0).dash(&[6.0, 3.0]).dash_offset(3.0),
    );
    // Three pixels in: on to 3, off to 6, on to 12.
    assert!(inked(&canvas, 1, 5) && clear(&canvas, 4, 5) && inked(&canvas, 8, 5));

    let back = stroked(
        40,
        10,
        &path,
        &Pen::new(2.0).dash(&[6.0, 3.0]).dash_offset(-3.0),
    );
    // Three pixels back: the gap from -3 to 0 is gone, so off to 3, then on to 9.
    assert!(clear(&back, 1, 5) && inked(&back, 5, 5));
}

#[test]
fn an_odd_dash_list_runs_twice_and_a_bad_one_draws_solid() {
    let path = line((0.0, 5.0), (40.0, 5.0));
    let odd = stroked(40, 10, &path, &Pen::new(2.0).dash(&[4.0]));
    assert!(inked(&odd, 2, 5) && clear(&odd, 5, 5) && inked(&odd, 10, 5));

    for pattern in [&[3.0, -1.0][..], &[f64::INFINITY, 2.0], &[], &[0.0, 0.0]] {
        let solid = stroked(40, 10, &path, &Pen::new(2.0).dash(pattern));
        assert!(
            (1..39).all(|x| inked(&solid, x, 5)),
            "{pattern:?} draws solid"
        );
    }
}

#[test]
fn a_line_thinner_than_a_pixel_lands_faintly() {
    let canvas = stroked(20, 10, &line((2.0, 5.5), (18.0, 5.5)), &Pen::new(0.5));
    let darkest = (0..10).map(|y| alpha(&canvas, 10, y)).max().unwrap_or(0);
    assert!((100..=160).contains(&darkest), "{darkest}");
}

#[test]
fn a_line_lands_at_its_brush_strength() {
    let mut canvas = Canvas::new(20, 10);
    canvas.stroke(
        &line((2.0, 5.0), (18.0, 5.0)),
        &Pen::new(4.0),
        &Brush::new(Colour::rgba(255, 255, 255, 0.5)).alpha(0.5),
    );
    assert!(near(f64::from(alpha(&canvas, 10, 5)), 255.0 * 0.25, 2.0));
}
