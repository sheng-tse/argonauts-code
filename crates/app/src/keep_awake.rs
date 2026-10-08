//! Keeping the computer awake. A long run stops when the laptop sleeps, so
//! while anything is working Argonauts holds sleep off by itself, and `/awake` holds it off
//! until you quit or for a time you give. On macOS the hold is the built-in `caffeinate`,
//! told to watch this process, so it ends when Argonauts does however that happens. The
//! screen may still dim, and a closed lid still sleeps unless the laptop is on power.

use std::time::{Duration, Instant};

use argonauts_core::NoteLevel;
use argonauts_core::app::AwakeHold;
use argonauts_core::settings::Span;
use time::OffsetDateTime;

use crate::controller::{Ctx, Effect};

/// One hold on the computer's sleep, which ends when it's dropped.
pub(crate) trait Hold: Send + Sync {}

/// Starts a hold, for this many seconds or until it's dropped. `None` when the computer has
/// no way to hold sleep off that Argonauts knows.
pub(crate) type StartHold = Box<dyn Fn(Option<u64>) -> Option<Box<dyn Hold>> + Send + Sync>;

/// What `/awake` asked for.
enum Asked {
    /// Nothing: awake while anything works, when the settings say so.
    Nothing,
    UntilQuit,
    /// For a time, which the hold ends by itself. The wall time is for the bottom line.
    Until {
        ends: Instant,
        at: OffsetDateTime,
    },
    /// `/awake off`: not even while working, until you ask again.
    Off,
}

pub(crate) struct KeepAwake {
    start: StartHold,
    /// `awake.while_working`, as last told.
    while_working: bool,
    working: bool,
    asked: Asked,
    /// The hold for work under way.
    for_work: Option<Box<dyn Hold>>,
    /// The hold `/awake` asked for.
    for_you: Option<Box<dyn Hold>>,
}

impl KeepAwake {
    /// Never holds anything, as in tests.
    pub(crate) fn never() -> Self {
        Self::with(Box::new(|_| None))
    }

    /// Holds sleep off the way this computer does it.
    pub(crate) fn system() -> Self {
        Self::with(Box::new(system_hold))
    }

    pub(crate) fn with(start: StartHold) -> Self {
        Self {
            start,
            while_working: true,
            working: false,
            asked: Asked::Nothing,
            for_work: None,
            for_you: None,
        }
    }

    /// Whether anything is working now, and what `awake.while_working` says now.
    pub(crate) fn working(&mut self, working: bool, while_working: bool) {
        self.working = working;
        self.while_working = while_working;
        self.settle();
    }

    /// `/awake`: until you quit. Says what it did.
    pub(crate) fn until_quit(&mut self) -> String {
        self.for_you = (self.start)(None);
        if self.for_you.is_none() {
            return CANT.to_owned();
        }
        self.asked = Asked::UntilQuit;
        self.settle();
        "Keeping the computer awake until you quit. /awake off lets it sleep again.".to_owned()
    }

    /// `/awake <time>`: for `span` from `now`, as you wrote it in `time`.
    pub(crate) fn for_a_time(&mut self, span: Duration, time: &str, now: OffsetDateTime) -> String {
        self.for_you = (self.start)(Some(span.as_secs().max(1)));
        if self.for_you.is_none() {
            return CANT.to_owned();
        }
        self.asked = Asked::Until {
            ends: Instant::now() + span,
            at: now + span,
        };
        self.settle();
        format!("Keeping the computer awake for {time}. /awake off lets it sleep again.")
    }

    /// `/awake off`: the computer may sleep, also while agents work.
    pub(crate) fn off(&mut self) -> String {
        self.asked = Asked::Off;
        self.for_you = None;
        self.settle();
        "The computer may sleep again, also while agents work. /awake keeps it awake.".to_owned()
    }

    /// The hold you asked for, for the bottom line. `None` when you asked for none, or its
    /// time is up.
    pub(crate) fn held(&self) -> Option<AwakeHold> {
        match &self.asked {
            Asked::UntilQuit => Some(AwakeHold::UntilQuit),
            Asked::Until { ends, at } if Instant::now() < *ends => {
                Some(AwakeHold::Until { at: *at })
            }
            Asked::Until { .. } | Asked::Nothing | Asked::Off => None,
        }
    }

    /// Starts or ends the hold for work under way, as things stand.
    fn settle(&mut self) {
        // A time that's up leaves things as they were before you asked.
        if matches!(&self.asked, Asked::Until { ends, .. } if Instant::now() >= *ends) {
            self.asked = Asked::Nothing;
            self.for_you = None;
        }
        let wanted = self.working && self.while_working && !matches!(self.asked, Asked::Off);
        match (wanted, self.for_work.is_some()) {
            (true, false) => self.for_work = (self.start)(None),
            (false, true) => self.for_work = None,
            _ => {}
        }
    }
}

const CANT: &str = "Argonauts can keep only a Mac awake so far, so nothing changed.";

/// `/awake`, `/awake <time>` and `/awake off`. The bottom line shows a hold you asked for.
pub(crate) fn command(ctx: &mut Ctx, time: Option<String>) -> anyhow::Result<Vec<Effect>> {
    let said = match time.as_deref() {
        None => ctx.awake.until_quit(),
        Some("off") => ctx.awake.off(),
        Some(time) => match Span::new(time).parse() {
            Ok(Some(span)) if !span.is_zero() => {
                ctx.awake.for_a_time(span, time, OffsetDateTime::now_utc())
            }
            _ => anyhow::bail!("/awake takes a time such as 2h or 45m, or off"),
        },
    };
    ctx.notice(NoteLevel::Info, said);
    Ok(vec![Effect::Status])
}

/// A hold for as long as this process hosts a run, for the supervisor. `None` when the
/// computer has no way Argonauts knows.
pub(crate) fn while_hosting() -> Option<Box<dyn Hold>> {
    system_hold(None)
}

#[cfg(target_os = "macos")]
fn system_hold(secs: Option<u64>) -> Option<Box<dyn Hold>> {
    mac::Caffeinate::start(secs).map(|hold| Box::new(hold) as Box<dyn Hold>)
}

#[cfg(not(target_os = "macos"))]
fn system_hold(_secs: Option<u64>) -> Option<Box<dyn Hold>> {
    None
}

#[cfg(target_os = "macos")]
mod mac {
    use std::process::{Child, Command, Stdio};

    /// By its full path, so a wrapper of the same name on `PATH` is never what runs.
    const CAFFEINATE: &str = "/usr/bin/caffeinate";

    /// A running `caffeinate`, stopped when dropped.
    pub(super) struct Caffeinate(Child);

    impl Caffeinate {
        /// `-i` holds idle sleep off, and `-s` system sleep while on power. `-w` ties it to
        /// this process, so it ends with Argonauts even after a crash. `-t` ends it by
        /// itself.
        pub(super) fn start(secs: Option<u64>) -> Option<Self> {
            let mut command = Command::new(CAFFEINATE);
            command
                .arg("-is")
                .args(["-w", &std::process::id().to_string()]);
            if let Some(secs) = secs {
                command.args(["-t", &secs.to_string()]);
            }
            let child = command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            match child {
                Ok(child) => Some(Self(child)),
                Err(err) => {
                    tracing::warn!("{CAFFEINATE} didn't start, so the computer may sleep: {err}");
                    None
                }
            }
        }
    }

    impl super::Hold for Caffeinate {}

    #[cfg(test)]
    impl Caffeinate {
        pub(super) fn pid(&self) -> u32 {
            self.0.id()
        }
    }

    impl Drop for Caffeinate {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Counts itself while it lives.
    struct Counted(Arc<AtomicUsize>);

    impl Hold for Counted {}

    impl Drop for Counted {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// The times each hold was started for.
    type Started = Arc<Mutex<Vec<Option<u64>>>>;

    /// A keeper whose live holds are counted.
    fn counted() -> (KeepAwake, Arc<AtomicUsize>, Started) {
        let held = Arc::new(AtomicUsize::new(0));
        let started = Started::default();
        let (count, log) = (held.clone(), started.clone());
        let start: StartHold = Box::new(move |secs| {
            log.lock().unwrap().push(secs);
            count.fetch_add(1, Ordering::SeqCst);
            Some(Box::new(Counted(count.clone())) as Box<dyn Hold>)
        });
        (KeepAwake::with(start), held, started)
    }

    #[test]
    fn it_holds_sleep_off_only_while_something_works() {
        let (mut awake, held, _) = counted();
        assert_eq!(held.load(Ordering::SeqCst), 0);
        awake.working(true, true);
        awake.working(true, true);
        assert_eq!(held.load(Ordering::SeqCst), 1, "one hold, however often");
        assert_eq!(awake.held(), None, "the bottom line names only your own");
        awake.working(false, true);
        assert_eq!(held.load(Ordering::SeqCst), 0);

        // `awake.while_working = false`, and then true again while it works.
        awake.working(true, false);
        assert_eq!(held.load(Ordering::SeqCst), 0);
        awake.working(true, true);
        assert_eq!(held.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn awake_holds_until_you_quit_and_off_ends_every_hold() {
        let (mut awake, held, started) = counted();
        let said = awake.until_quit();
        assert!(
            said.starts_with("Keeping the computer awake until you quit."),
            "{said}"
        );
        assert_eq!(held.load(Ordering::SeqCst), 1);
        assert_eq!(awake.held(), Some(AwakeHold::UntilQuit));
        awake.working(true, true);
        awake.working(false, true);
        assert_eq!(held.load(Ordering::SeqCst), 1, "yours stays when work ends");

        awake.working(true, true);
        let said = awake.off();
        assert!(said.starts_with("The computer may sleep again"), "{said}");
        assert_eq!(held.load(Ordering::SeqCst), 0, "not even while agents work");
        assert_eq!(awake.held(), None);
        // Asking again brings both back.
        awake.until_quit();
        assert_eq!(held.load(Ordering::SeqCst), 2);
        assert!(started.lock().unwrap().iter().all(Option::is_none));
    }

    #[test]
    fn a_time_is_given_to_the_hold_and_reaches_the_bottom_line() {
        let (mut awake, held, started) = counted();
        let now = time::macros::datetime!(2026-10-07 13:45 UTC);
        let said = awake.for_a_time(Duration::from_secs(2 * 3600), "2h", now);
        assert_eq!(
            said,
            "Keeping the computer awake for 2h. /awake off lets it sleep again."
        );
        assert_eq!(*started.lock().unwrap(), [Some(7200)]);
        assert_eq!(held.load(Ordering::SeqCst), 1);
        let until = time::macros::datetime!(2026-10-07 15:45 UTC);
        assert_eq!(awake.held(), Some(AwakeHold::Until { at: until }));
    }

    #[test]
    fn the_command_says_what_it_did_and_work_under_way_holds_sleep_off_too() {
        use argonauts_core::app::AppMode;
        use argonauts_core::names::TurnId;

        use crate::controller::test_support::{HarnessOptions, TestCtx};

        let (mut parts, _) = TestCtx::new(HarnessOptions::default());
        let (awake, held, _) = counted();
        parts.ctx.awake = awake;
        let ctx = &mut parts.ctx;
        let effects = command(ctx, None).unwrap();
        assert!(
            matches!(effects.as_slice(), [Effect::Status]),
            "{effects:?}"
        );
        assert_eq!(held.load(Ordering::SeqCst), 1);
        assert_eq!(ctx.awake.held(), Some(AwakeHold::UntilQuit));

        // An answer under way holds one more, and its end lets that one go.
        let turn = TurnId::numbered(1);
        ctx.set_mode(AppMode::Answering { turn });
        assert_eq!(held.load(Ordering::SeqCst), 2);
        ctx.set_mode(AppMode::Idle);
        assert_eq!(held.load(Ordering::SeqCst), 1);

        command(ctx, Some("off".to_owned())).unwrap();
        assert_eq!(held.load(Ordering::SeqCst), 0);
        command(ctx, Some("45m".to_owned())).unwrap();
        assert_eq!(held.load(Ordering::SeqCst), 1);
        assert!(matches!(ctx.awake.held(), Some(AwakeHold::Until { .. })));
        assert!(command(ctx, Some("soon".to_owned())).is_err());

        let mut said = Vec::new();
        while let Ok(event) = parts.events.try_recv() {
            said.push(event);
        }
        let said = format!("{said:?}");
        for line in [
            "Keeping the computer awake until you quit.",
            "The computer may sleep again, also while agents work.",
            "Keeping the computer awake for 45m.",
        ] {
            assert!(said.contains(line), "{line}: {said}");
        }
    }

    /// The real thing on a Mac: `caffeinate` runs while the hold lives, and ends with it.
    #[cfg(target_os = "macos")]
    #[test]
    fn on_a_mac_the_hold_is_caffeinate_and_it_ends_when_dropped() {
        let alive = |pid: u32| {
            std::process::Command::new("/bin/kill")
                .args(["-0", &pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        };
        let hold = mac::Caffeinate::start(Some(30)).expect("caffeinate starts");
        let pid = hold.pid();
        assert!(alive(pid));
        drop(hold);
        assert!(!alive(pid));
    }

    #[test]
    fn a_computer_with_no_way_to_hold_sleep_off_says_so() {
        let mut awake = KeepAwake::never();
        assert_eq!(awake.until_quit(), CANT);
        assert_eq!(awake.held(), None);
        awake.working(true, true);
        assert_eq!(awake.held(), None);
    }
}
