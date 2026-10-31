//! The time a thread spent on a core, which the timed tests judge instead of the wall clock: a
//! shared runner that stalls the test or runs other jobs beside it stretches the wall clock,
//! never this. On a chip with performance and efficiency cores the same work costs about twice
//! the time on an efficiency core, and a busy machine moves a thread there whatever its class,
//! so each stretch also says which kind of core ran it, and only the performance cores' are
//! judged. Each stretch is judged as the reference Mac would have spent it, by the yardstick
//! timed right after it.

use std::time::Duration;

use nix::libc;

use super::yardstick;

/// The CPU time the calling thread has used so far.
pub fn of_this_thread() -> Duration {
    let mut spent = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `clock_gettime` only writes the `timespec` it is given, which lives for the call.
    let read = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut spent) };
    assert_eq!(read, 0, "the thread's CPU clock can't be read");
    let seconds = u64::try_from(spent.tv_sec).unwrap_or_default();
    let nanos = u32::try_from(spent.tv_nsec).unwrap_or_default();
    Duration::new(seconds, nanos)
}

/// Puts the calling thread in the class the scheduler gives the thread you are watching. macOS
/// may run a thread of a lower class on an efficiency core, where the same work costs about
/// twice the CPU time, so a timed thread asks for this before it is timed.
pub fn run_as_interactive() {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: `pthread_set_qos_class_self_np` only changes the calling thread's class.
        let set = unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0)
        };
        assert_eq!(set, 0, "the thread's class can't be raised");
    }
}

/// A stretch of the calling thread's work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spent {
    pub cpu: Duration,
    /// The process ran nearly all of the stretch on performance cores, or the machine has one
    /// kind of core.
    pub on_performance_cores: bool,
    /// The yardstick's time on the same thread right after the stretch.
    pub yardstick: Duration,
}

impl Spent {
    /// How many times the reference Mac's time the yardstick took after this stretch.
    fn pace(&self) -> f64 {
        yardstick::pace(self.yardstick)
    }

    /// The stretch's CPU time as the reference Mac would have spent it.
    fn as_on_the_reference(&self) -> Duration {
        self.cpu.div_f64(self.pace())
    }
}

/// Times the calling thread's work from when it starts.
pub struct Stopwatch {
    thread: Duration,
    retired: Retired,
}

impl Stopwatch {
    pub fn start() -> Self {
        // The counters are read first, so the time they take stays out of the thread's.
        let retired = Retired::now();
        Self {
            thread: of_this_thread(),
            retired,
        }
    }

    pub fn stop(self) -> Spent {
        let cpu = of_this_thread() - self.thread;
        let retired = Retired::now();
        Spent {
            cpu,
            on_performance_cores: retired.mostly_on_performance_cores_since(self.retired),
            yardstick: yardstick::measure(),
        }
    }
}

/// The instructions the whole process has retired: in all, and on performance cores.
#[derive(Debug, Clone, Copy, Default)]
struct Retired {
    all: u64,
    on_performance_cores: u64,
}

impl Retired {
    #[cfg(target_os = "macos")]
    fn now() -> Self {
        /// `rusage_info_v6` as macOS lays it out: an id, then 64-bit counters, more room given
        /// than any version fills.
        #[repr(C)]
        struct Usage {
            id: [u8; 16],
            counters: [u64; 64],
        }
        const INSTRUCTIONS: usize = 29;
        const INSTRUCTIONS_ON_PERFORMANCE_CORES: usize = 38;
        const RUSAGE_INFO_V6: libc::c_int = 6;
        if !has_two_kinds_of_core() {
            return Self::default();
        }
        let mut usage = Usage {
            id: [0; 16],
            counters: [0; 64],
        };
        // SAFETY: `proc_pid_rusage` writes at most a `rusage_info_v6` into the buffer, which is
        // larger and lives for the call.
        let read = unsafe {
            libc::proc_pid_rusage(
                libc::getpid(),
                RUSAGE_INFO_V6,
                std::ptr::from_mut(&mut usage).cast(),
            )
        };
        if read != 0 {
            return Self::default();
        }
        Self {
            all: usage.counters[INSTRUCTIONS],
            on_performance_cores: usage.counters[INSTRUCTIONS_ON_PERFORMANCE_CORES],
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn now() -> Self {
        Self::default()
    }

    /// Nine in ten of the instructions retired since `before` were retired on performance
    /// cores. Counters that can't be read count as performance cores.
    fn mostly_on_performance_cores_since(self, before: Self) -> bool {
        let all = self.all.saturating_sub(before.all);
        let fast = self
            .on_performance_cores
            .saturating_sub(before.on_performance_cores);
        all == 0 || u128::from(fast) * 10 >= u128::from(all) * 9
    }
}

#[cfg(target_os = "macos")]
fn has_two_kinds_of_core() -> bool {
    let mut levels: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // SAFETY: `sysctlbyname` writes at most `size` bytes into `levels`, which lives for the call.
    let read = unsafe {
        libc::sysctlbyname(
            c"hw.nperflevels".as_ptr(),
            std::ptr::from_mut(&mut levels).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    read == 0 && levels > 1
}

/// Whether a run has measured enough: `wanted` seconds on performance cores, or four times as
/// many seconds in all, after which [`judge`] judges every second.
pub fn measured(seconds: &[Spent], wanted: usize) -> bool {
    on_performance_cores(seconds).count() >= wanted || seconds.len() >= wanted * 4
}

fn on_performance_cores(seconds: &[Spent]) -> impl Iterator<Item = &Spent> {
    seconds.iter().filter(|second| second.on_performance_cores)
}

/// The middle of the yardstick's paces over a run.
fn pace(seconds: &[Spent]) -> f64 {
    let mut paces: Vec<f64> = seconds.iter().map(Spent::pace).collect();
    paces.sort_by(f64::total_cmp);
    paces.get(paces.len() / 2).copied().unwrap_or(f64::NAN)
}

/// What a second of work costs, judged as the timed tests judge it, each second as the
/// reference Mac would have spent it: the mean must stay under its limit, while a second over
/// the worst-second limit is only reported, since one busy second on a shared runner says
/// little about the code. Only the seconds on performance cores are judged, unless fewer than
/// a quarter of the seconds were. A machine too slow to compare with the reference fails.
pub fn judge(what: &str, seconds: &[Spent], mean_under: Duration, worst_under: Duration) {
    let pace = pace(seconds);
    eprintln!("{what}: the yardstick took {pace:.2} times as long as on the reference Mac");
    assert!(
        pace > 0.0 && pace <= yardstick::SLOWEST,
        "{what}: the yardstick took {pace:.2} times as long as on the reference Mac, too slow \
         to judge against limits set there"
    );
    let fast: Vec<&Spent> = on_performance_cores(seconds).collect();
    let judged = match fast.len() * 4 >= seconds.len() {
        true => fast,
        false => seconds.iter().collect(),
    };
    let count = u32::try_from(judged.len()).unwrap_or(u32::MAX).max(1);
    let here = judged.iter().map(|second| second.cpu).sum::<Duration>() / count;
    let on_the_reference: Vec<Duration> = judged
        .iter()
        .map(|second| second.as_on_the_reference())
        .collect();
    let mean = on_the_reference.iter().sum::<Duration>() / count;
    let worst = on_the_reference.iter().max().copied().unwrap_or_default();
    eprintln!(
        "{what}: {mean:?} of a core a second on average as on the reference Mac ({here:?} \
         here), {worst:?} at most, over {} of {} seconds",
        judged.len(),
        seconds.len()
    );
    if worst >= worst_under {
        eprintln!("warning: {what} took {worst:?} in its busiest second, over {worst_under:?}");
    }
    assert!(mean < mean_under, "{what}: {mean:?} a second on average");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_threads_clock_counts_work_and_not_sleep() {
        let before = of_this_thread();
        std::thread::sleep(Duration::from_millis(30));
        let slept = of_this_thread() - before;
        assert!(slept < Duration::from_millis(20), "{slept:?}");
        let before = of_this_thread();
        let mut sum = 0_u64;
        for n in 0..5_000_000_u64 {
            sum = sum.wrapping_add(std::hint::black_box(n));
        }
        std::hint::black_box(sum);
        assert!(of_this_thread() > before);
    }

    const FAST: Spent = Spent {
        cpu: Duration::from_millis(5),
        on_performance_cores: true,
        yardstick: yardstick::REFERENCE,
    };
    const SLOW: Spent = Spent {
        cpu: Duration::from_millis(12),
        on_performance_cores: false,
        yardstick: yardstick::REFERENCE,
    };

    /// A second of `cpu` on a machine whose yardstick takes `times` the reference's.
    fn on_a_machine(cpu: u64, times: u32) -> Spent {
        Spent {
            cpu: Duration::from_millis(cpu),
            on_performance_cores: true,
            yardstick: yardstick::REFERENCE * times,
        }
    }
    const LIMITS: (Duration, Duration) = (Duration::from_millis(10), Duration::from_millis(30));

    #[test]
    fn seconds_on_efficiency_cores_are_left_out_while_a_quarter_ran_on_performance_cores() {
        judge("mixed", &[FAST, SLOW, SLOW, SLOW], LIMITS.0, LIMITS.1);
    }

    #[test]
    #[should_panic(expected = "a second on average")]
    fn every_second_is_judged_once_too_few_ran_on_performance_cores() {
        judge("slow", &[FAST, SLOW, SLOW, SLOW, SLOW], LIMITS.0, LIMITS.1);
    }

    #[test]
    fn a_machine_half_as_fast_is_judged_as_the_reference_would_spend_it() {
        judge(
            "half as fast",
            &[on_a_machine(16, 2); 4],
            LIMITS.0,
            LIMITS.1,
        );
    }

    #[test]
    #[should_panic(expected = "a second on average")]
    fn the_same_seconds_on_the_reference_fail() {
        judge("as fast", &[on_a_machine(16, 1); 4], LIMITS.0, LIMITS.1);
    }

    #[test]
    #[should_panic(expected = "too slow to judge")]
    fn a_machine_over_three_times_slower_than_the_reference_fails() {
        judge(
            "a quarter as fast",
            &[on_a_machine(1, 4); 4],
            LIMITS.0,
            LIMITS.1,
        );
    }

    #[test]
    fn a_run_measures_until_enough_seconds_ran_on_performance_cores_or_four_times_passed() {
        assert!(!measured(&[FAST, SLOW], 2));
        assert!(measured(&[FAST, SLOW, FAST], 2));
        assert!(!measured(&[SLOW; 7], 2));
        assert!(measured(&[SLOW; 8], 2));
    }

    #[test]
    fn a_stopwatch_counts_the_threads_work() {
        let watch = Stopwatch::start();
        let mut sum = 0_u64;
        for n in 0..5_000_000_u64 {
            sum = sum.wrapping_add(std::hint::black_box(n));
        }
        std::hint::black_box(sum);
        assert!(watch.stop().cpu > Duration::ZERO);
    }
}
