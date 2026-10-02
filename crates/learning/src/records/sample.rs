//! Seeded draws for Beta posteriors, so a draw logged with its seed can be repeated.

use rand::RngExt;
use rand::rngs::StdRng;

/// A draw from Beta(alpha, beta), as the first of two Gamma draws over their sum.
pub(crate) fn beta(alpha: f64, beta: f64, rng: &mut StdRng) -> f64 {
    let x = gamma(alpha, rng);
    let y = gamma(beta, rng);
    let total = x + y;
    if total > 0.0 && total.is_finite() {
        x / total
    } else {
        // Both shapes so small that each draw underflowed: fall back to the mean.
        alpha / (alpha + beta)
    }
}

/// Marsaglia and Tsang's method. Shapes below 1 draw at shape + 1 and scale back by
/// `u^(1/shape)`, which keeps one method for every shape.
fn gamma(shape: f64, rng: &mut StdRng) -> f64 {
    if shape < 1.0 {
        let u = 1.0 - rng.random::<f64>();
        return gamma(shape + 1.0, rng) * u.powf(1.0 / shape);
    }
    let d = shape - 1.0 / 3.0;
    let c = 1.0 / (9.0 * d).sqrt();
    loop {
        let x = normal(rng);
        let v = (1.0 + c * x).powi(3);
        if v <= 0.0 {
            continue;
        }
        let u = 1.0 - rng.random::<f64>();
        if u < 1.0 - 0.0331 * x.powi(4) || u.ln() < 0.5 * x * x + d * (1.0 - v + v.ln()) {
            return d * v;
        }
    }
}

/// Box and Muller's transform.
fn normal(rng: &mut StdRng) -> f64 {
    let u1 = 1.0 - rng.random::<f64>();
    let u2 = rng.random::<f64>();
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;

    use super::*;

    #[test]
    fn beta_draws_center_on_the_mean() {
        let mut rng = StdRng::seed_from_u64(7);
        for (alpha, b) in [(3.0, 7.0), (0.5, 0.5), (40.0, 10.0), (0.05, 2.0)] {
            let draws = 20_000;
            let mean: f64 =
                (0..draws).map(|_| beta(alpha, b, &mut rng)).sum::<f64>() / draws as f64;
            let expected = alpha / (alpha + b);
            assert!((mean - expected).abs() < 0.02, "Beta({alpha}, {b}): {mean}");
        }
    }
}
