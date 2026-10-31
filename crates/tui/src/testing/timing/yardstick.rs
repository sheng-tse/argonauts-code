//! A yardstick for the timed tests: a small fixed task that never changes with the code,
//! sorting and hashing a fixed buffer. Timed on the same thread right after a stretch of work,
//! it gets the same core and clock as the work, so how many times longer it takes than on the
//! reference Mac says how much slower this machine is, and a figure divided by that is what the
//! reference Mac would have measured. It needs no undocumented call, on macOS or Linux.

use std::hint::black_box;
use std::time::Duration;

use super::cpu_time::of_this_thread;

/// The yardstick on the reference Mac, the maintainer's Apple M3 Max, on an optimized build on
/// 2026-10-30 (`the_yardstick_on_this_machine`). Other builds were running, so the medians of
/// five sets of 200 measures read 673 to 721 µs and their quickest 618 to 653 µs; this is
/// near the quickest, as the Mac measures it alone.
pub const REFERENCE: Duration = Duration::from_micros(640);

/// A machine whose yardstick takes more than this many times the reference's is too slow for
/// its figures to be judged against limits set on the reference.
pub const SLOWEST: f64 = 3.0;

/// The buffer's values, enough that a run takes most of a millisecond on the reference.
const VALUES: usize = 1 << 16;

/// Runs a measure takes the quickest of, so an interrupt during one doesn't count.
const RUNS: usize = 3;

/// The calling thread's CPU time for the yardstick.
pub fn measure() -> Duration {
    (0..RUNS)
        .map(|_| {
            let before = of_this_thread();
            black_box(task(black_box(VALUES)));
            of_this_thread() - before
        })
        .min()
        .unwrap_or_default()
}

/// How many times the reference's the yardstick's time `took` is.
pub fn pace(took: Duration) -> f64 {
    took.as_secs_f64() / REFERENCE.as_secs_f64()
}

/// Fills a buffer from a fixed seed, sorts it and hashes it with FNV-1a.
fn task(values: usize) -> u64 {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut buffer: Vec<u64> = (0..values)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        })
        .collect();
    buffer.sort_unstable();
    buffer.iter().fold(0xCBF2_9CE4_8422_2325, |hash, value| {
        (hash ^ value).wrapping_mul(0x0100_0000_01B3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The task stays the one the reference was measured with.
    #[test]
    fn the_task_is_the_one_the_reference_measured() {
        assert_eq!(task(VALUES), 11_656_864_565_817_787_242);
    }

    /// Prints what [`REFERENCE`] holds, measured on this machine:
    /// `cargo test --release -p argonauts-tui --lib -- --ignored --exact
    /// testing::timing::yardstick::tests::the_yardstick_on_this_machine --nocapture`.
    #[test]
    #[ignore = "timed: run alone on an optimized build"]
    fn the_yardstick_on_this_machine() {
        super::super::cpu_time::run_as_interactive();
        let mut took: Vec<Duration> = (0..200).map(|_| measure()).collect();
        took.sort();
        eprintln!(
            "the yardstick: {:?} the median, {:?} the quickest, {:?} the slowest, {:.2} times \
             the reference",
            took[took.len() / 2],
            took[0],
            took[took.len() - 1],
            pace(took[took.len() / 2]),
        );
    }
}
