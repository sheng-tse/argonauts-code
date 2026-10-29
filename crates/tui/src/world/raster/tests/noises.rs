//! The numbers here are the formulas' own results, worked out apart from this code, so a world
//! grown from a seed stays the same to the bit.

use super::noise::*;

#[test]
fn a_seed_gives_the_same_stream_every_time() {
    let streams: [(u32, [f64; 5]); 6] = [
        (
            0,
            [
                0.26642920868471265,
                0.0003297457005828619,
                0.2232720274478197,
                0.1462021479383111,
                0.46732782293111086,
            ],
        ),
        (
            1,
            [
                0.6270739405881613,
                0.002735721180215478,
                0.5274470399599522,
                0.9810509674716741,
                0.9683778982143849,
            ],
        ),
        (
            7,
            [
                0.011704753153026104,
                0.06195825757458806,
                0.97690763277933,
                0.6990287057124078,
                0.5214452685322613,
            ],
        ),
        (
            12345,
            [
                0.9797282677609473,
                0.3067522644996643,
                0.484205421525985,
                0.817934412509203,
                0.5094283693470061,
            ],
        ),
        (
            0xdead_beef,
            [
                0.9413696140982211,
                0.26719574979506433,
                0.772033357527107,
                0.35816076025366783,
                0.47554167779162526,
            ],
        ),
        (
            u32::MAX,
            [
                0.8964226141106337,
                0.189478256739676,
                0.7156526781618595,
                0.9440599093213677,
                0.8452364315744489,
            ],
        ),
    ];
    for (seed, expected) in streams {
        let mut rng = Rng::new(seed);
        let drawn: Vec<f64> = (0..5).map(|_| rng.next_f64()).collect();
        assert_eq!(drawn, expected, "seed {seed}");
    }
}

#[test]
fn the_bell_curve_draws_two_numbers_for_each() {
    let mut rng = Rng::new(42);
    let drawn = [rng.normal(), rng.normal(), rng.normal()];
    assert_eq!(
        drawn,
        [
            -1.2848381576290195,
            -0.9453528099747296,
            -0.6112802846514629
        ]
    );
}

#[test]
fn lattice_hashes_round_huge_inputs_as_floats_do() {
    let cases = [
        ([0.0, 0.0, 0.0, 0.0], 0.0),
        ([1.0, 2.0, 3.0, 4.0], 0.6102792997844517),
        ([-5.0, 17.0, -2.0, 99.0], 0.26422525802627206),
        ([1000.0, -2000.0, 300.0, 3_000_000.0], 0.5331484966445714),
        ([3.0, 4.0, 5.0, 4_294_967_295.5], 0.22699447069317102),
        (
            [123_456.0, 654_321.0, 0.0, 12_345_678_901.0],
            0.14457523985765874,
        ),
    ];
    for ([x, y, z, seed], expected) in cases {
        assert_eq!(
            hash3(x, y, z, seed),
            expected,
            "({x}, {y}, {z}) seeded {seed}"
        );
    }

    let wrapping = [
        ([0.0, 0.0, 0.0], 0.0),
        ([1.0, 2.0, 3.0], 0.6911013713106513),
        ([-5.0, 17.0, -2.0], 0.16619833512231708),
        ([3.7, -9.2, 1e10], 0.9903656630776823),
    ];
    for ([x, y, z], expected) in wrapping {
        assert_eq!(hash3_wrapping(x, y, z), expected, "({x}, {y}, {z})");
    }
}

#[test]
fn value_noise_matches_to_the_bit() {
    let cases = [
        ([0.3, 0.7, 0.1, 5.0], 0.3396366432397331),
        ([12.25, -3.5, 0.75, 11.0], 0.542587743421791),
        ([-7.9, 2.2, 100.01, 31337.0], 0.01842855131456611),
        ([0.0, 0.0, 0.0, 0.0], -1.0),
    ];
    for ([x, y, z, seed], expected) in cases {
        assert_eq!(vnoise3(x, y, z, seed), expected);
    }
    let shifted = [
        ([0.3, 0.7, 0.1, 5.0], -0.154438915206228),
        ([12.25, -3.5, 0.75, 11.0], -0.0920898260303602),
    ];
    for ([x, y, z, seed], expected) in shifted {
        assert_eq!(vnoise3_shifted(x, y, z, seed), expected);
    }
}

#[test]
fn fractal_noise_matches_to_the_bit() {
    let cases = [
        ([0.3, 0.7, 0.1, 5.0], 4, 0.10641205170395887),
        ([12.25, -3.5, 0.75, 11.0], 4, 0.36626108539751795),
        ([1.5, 2.5, 3.5, 77.0], 6, -0.39174010447212637),
        ([-0.4, 9.9, 0.2, 1.0], 1, 0.022471445887363606),
    ];
    for ([x, y, z, seed], octaves, expected) in cases {
        assert_eq!(fbm3(x, y, z, seed, octaves), expected);
    }
    let shifted = [
        ([0.3, 0.7, 0.1, 5.0], 4, 0.05268657979482154),
        ([1.5, 2.5, 3.5, 77.0], 6, 0.13765701045642786),
    ];
    for ([x, y, z, seed], octaves, expected) in shifted {
        assert_eq!(fbm3_shifted(x, y, z, seed, octaves), expected);
    }
}

#[test]
fn whole_numbers_wrap_to_32_bits() {
    assert_eq!(int32(3.9), 3);
    assert_eq!(int32(-3.9), -3);
    assert_eq!(int32(4_294_967_296.0 + 5.0), 5);
    assert_eq!(int32(2_147_483_648.0), i32::MIN);
    assert_eq!(int32(f64::NAN), 0);
    assert_eq!(uint32(-1.0), u32::MAX);
    assert_eq!(uint32(f64::INFINITY), 0);
    assert_eq!(uint32(1e20), (1e20_f64 % 4_294_967_296.0) as u32);
}

#[test]
fn mixing_and_easing_read_as_their_names_say() {
    assert_eq!(mix(2.0, 6.0, 0.25), 3.0);
    assert_eq!(mix(2.0, 6.0, 1.5), 8.0);
    assert_eq!(smooth(0.0, 1.0, -1.0), 0.0);
    assert_eq!(smooth(0.0, 1.0, 0.5), 0.5);
    assert_eq!(smooth(0.0, 1.0, 2.0), 1.0);
    assert_eq!(smooth(1.0, 0.0, 0.25), 0.84375, "a falling edge eases down");
}
