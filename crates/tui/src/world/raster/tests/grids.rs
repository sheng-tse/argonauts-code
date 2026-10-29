use super::*;

/// A five by four field of tenths, its blur worked out apart from this code.
fn field() -> Grid {
    let tenths = [
        0.0, 0.4, 0.8, 0.1, 0.5, 0.9, 0.2, 0.6, 1.0, 0.3, 0.7, 0.0, 0.4, 0.8, 0.1, 0.5, 0.9, 0.2,
        0.6, 1.0,
    ];
    Grid::from_fn(5, 4, |x, y| tenths[y * 5 + x])
}

#[test]
fn a_blurred_field_matches_to_the_bit() {
    // Each value is the float the field keeps, written out in full.
    let once: [f64; 20] = [
        0.38271602988243103,
        0.4283950626850128,
        0.4851851761341095,
        0.4740740656852722,
        0.45185184478759766,
        0.4481481611728668,
        0.46666663885116577,
        0.4962962865829468,
        0.4987654387950897,
        0.490123450756073,
        0.509876549243927,
        0.5012345314025879,
        0.5037037134170532,
        0.5333333611488342,
        0.5518518686294556,
        0.5481481552124023,
        0.5259259343147278,
        0.5148148536682129,
        0.5716049671173096,
        0.6172839403152466,
    ];
    let twice: [f64; 20] = [
        0.42800000309944153,
        0.4368000030517578,
        0.4496000111103058,
        0.46239998936653137,
        0.47119998931884766,
        0.46160000562667847,
        0.47040000557899475,
        0.4832000136375427,
        0.4959999918937683,
        0.504800021648407,
        0.495199978351593,
        0.5040000081062317,
        0.5167999863624573,
        0.5296000242233276,
        0.5383999943733215,
        0.5288000106811523,
        0.537600040435791,
        0.5504000186920166,
        0.5631999969482422,
        0.5720000267028809,
    ];
    assert_eq!(field().blurred(1).values(), &once.map(|v| v as f32));
    assert_eq!(field().blurred(2).values(), &twice.map(|v| v as f32));
}

#[test]
fn a_field_reads_smoothly_between_its_points_and_holds_at_its_edge() {
    let grid = Grid::from_fn(4, 3, |x, y| (y * 4 + x) as f64);
    assert_eq!(grid.sample(0.5, 0.5), 2.5);
    assert_eq!(grid.sample(2.25, 1.75), 9.25);
    assert_eq!(grid.sample(-3.0, 9.0), 7.996);
    assert_eq!(grid.sample(2.999, 2.0), 10.995);
    assert!(grid.sample(f64::NAN, 1.0).is_nan());
}

#[test]
fn a_field_keeps_its_points_where_they_are_put() {
    let mut grid = Grid::new(3, 2);
    grid.set(2, 1, 0.75);
    grid.set(9, 9, 1.0);
    assert_eq!(grid.get(2, 1), 0.75);
    assert_eq!(grid.get(9, 9), 0.0);
    assert_eq!((grid.width(), grid.height()), (3, 2));
    grid.values_mut()[0] = 0.5;
    assert_eq!(grid.values()[..2], [0.5, 0.0]);

    let thin = Grid::from_fn(1, 3, |_, y| y as f64);
    assert_eq!(
        thin.sample(4.0, 1.6),
        1.0,
        "too thin to mix, it gives the nearest point"
    );
}
