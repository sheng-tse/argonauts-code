//! How long a test waits for something before it fails. A shared CI runner can stall for
//! seconds, so every deadline a test driver sets is its base time multiplied by
//! `ARGONAUTS_TEST_DEADLINE_SCALE` (1 when unset). CI sets it higher, and higher still on
//! macOS. Only the longest a wait may take grows: a wait still ends as soon as what it waits
//! for is there.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub const SCALE_ENV: &str = "ARGONAUTS_TEST_DEADLINE_SCALE";

/// The multiplier, read once per test process.
///
/// # Panics
///
/// When the variable is set to something other than a number from 1 up, so a typo can't
/// quietly leave CI with the short deadlines.
pub fn scale() -> f64 {
    static SCALE: OnceLock<f64> = OnceLock::new();
    *SCALE.get_or_init(|| {
        let value = std::env::var(SCALE_ENV).ok();
        parse(value.as_deref()).unwrap_or_else(|why| panic!("{SCALE_ENV}: {why}"))
    })
}

/// `base` multiplied by [`scale`]: the longest a wait whose base time is `base` may take.
pub fn scaled(base: Duration) -> Duration {
    base.mul_f64(scale())
}

/// When a wait that starts now with base time `base` gives up.
pub fn after(base: Duration) -> Instant {
    Instant::now() + scaled(base)
}

fn parse(value: Option<&str>) -> Result<f64, String> {
    let Some(text) = value.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(1.0);
    };
    match text.parse::<f64>() {
        Ok(scale) if scale.is_finite() && scale >= 1.0 => Ok(scale),
        _ => Err(format!("{text:?} isn't a number from 1 up")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_or_blank_keeps_the_base_times() {
        assert_eq!(parse(None), Ok(1.0));
        assert_eq!(parse(Some("")), Ok(1.0));
        assert_eq!(parse(Some("  ")), Ok(1.0));
    }

    #[test]
    fn a_number_from_one_up_multiplies() {
        assert_eq!(parse(Some("3")), Ok(3.0));
        assert_eq!(parse(Some(" 2.5 ")), Ok(2.5));
        assert_eq!(parse(Some("1")), Ok(1.0));
    }

    #[test]
    fn anything_else_is_refused() {
        for bad in ["0.5", "0", "-2", "fast", "inf", "NaN", "3x"] {
            assert!(parse(Some(bad)).is_err(), "{bad} was taken");
        }
    }

    #[test]
    fn the_scale_stretches_a_base_time() {
        let base = Duration::from_millis(400);
        assert_eq!(scaled(base), base.mul_f64(scale()));
        assert!(scaled(base) >= base);
    }
}
