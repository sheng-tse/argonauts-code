//! Keeping the computer awake. A long run stops when the laptop sleeps, so while anything is
//! working Argonauts Code holds sleep off by itself, and `/awake` holds it off until you quit
//! or for a time you give. On macOS the hold is the built-in `caffeinate`, told to watch this
//! process; on Linux it is `systemd-inhibit` around a wait for this process. Either way it
//! ends when Argonauts Code does, however that happens, and runs in its own process group,
//! so a Ctrl+C typed into a program the app handed the terminal to never reaches it.
//!
//! `/awake` keeps the screen on too, since you asked in order to watch. The hold for work
//! under way leaves the screen to your own settings unless `awake.screen_while_working` adds
//! it. A closed lid still sleeps.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, AwakeHold};
use argonauts_core::settings::Span;
use argonauts_core::settings::awake::AwakeSettings;
use time::OffsetDateTime;

use crate::controller::{Ctx, Effect};

/// One hold on the computer's sleep, which ends when it's dropped.
pub(crate) trait Hold: Send + Sync {}

/// What a hold keeps awake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Keeps {
    /// The computer; the screen dims and locks as your settings say.
    Computer,
    ComputerAndScreen,
}

impl Keeps {
    fn while_working(settings: &AwakeSettings) -> Self {
        if settings.screen_while_working {
            Keeps::ComputerAndScreen
        } else {
            Keeps::Computer
        }
    }
}

/// Starts a hold, for this many seconds or until it's dropped. `None` when the computer has
/// no way to hold sleep off that Argonauts Code knows.
pub(crate) type StartHold = Box<dyn Fn(Keeps, Option<u64>) -> Option<Box<dyn Hold>> + Send + Sync>;

/// What `/awake` says it did: what's held, how to undo it, and what it can't do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Said {
    pub text: String,
    pub undo: Option<String>,
    pub caveat: Option<String>,
}

impl Said {
    fn held(text: String) -> Self {
        Self {
            text,
            undo: Some("/awake again lets them sleep.".to_owned()),
            caveat: Some(LID.to_owned()),
        }
    }

    fn cant() -> Self {
        Self {
            text: CANT.to_owned(),
            undo: None,
            caveat: None,
        }
    }
}

/// No hold keeps a laptop awake through a closed lid, so the message says so. On a Mac the
/// hold covers a closed lid while it's on power.
#[cfg(target_os = "linux")]
const LID: &str = "Closing the lid still puts it to sleep.";
#[cfg(not(target_os = "linux"))]
const LID: &str = "On battery, closing the lid still puts it to sleep.";

const CANT: &str = "Argonauts Code keeps a computer awake with caffeinate on a Mac or \
                    systemd-inhibit on Linux, and this one has neither, so nothing changed.";

/// What `/awake` asked for, with the hold that keeps it.
#[derive(Default)]
enum Asked {
    /// Nothing: awake while anything works, when the settings say so.
    #[default]
    Nothing,
    UntilQuit {
        _hold: Box<dyn Hold>,
    },
    /// For a time, which the hold ends by itself. The wall time is for the bottom line.
    Until {
        ends: Instant,
        at: OffsetDateTime,
        _hold: Box<dyn Hold>,
    },
}

impl Asked {
    fn held(&self) -> Option<AwakeHold> {
        match self {
            Asked::UntilQuit { .. } => Some(AwakeHold::UntilQuit),
            Asked::Until { ends, at, .. } if Instant::now() < *ends => {
                Some(AwakeHold::Until { at: *at })
            }
            Asked::Until { .. } | Asked::Nothing => None,
        }
    }
}

/// The hold `/awake` asked for. The app keeps one for as long as it runs and hands it to
/// every screen it opens, so the hold lasts when `/cd` opens the app in another repo and
/// still ends with the app.
#[derive(Clone, Default)]
pub(crate) struct YourHold(Arc<Mutex<Asked>>);

impl YourHold {
    fn lock(&self) -> MutexGuard<'_, Asked> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub(crate) struct KeepAwake {
    start: StartHold,
    /// Whether this computer has a way to hold sleep off at all.
    able: bool,
    /// `awake.while_working`, as last told.
    while_working: bool,
    /// What the hold for work under way keeps awake, from `awake.screen_while_working`.
    work_keeps: Keeps,
    working: bool,
    /// The hold for work under way, with what it keeps awake.
    for_work: Option<(Keeps, Box<dyn Hold>)>,
    yours: YourHold,
}

impl KeepAwake {
    /// Never holds anything, as in tests.
    pub(crate) fn never() -> Self {
        Self {
            able: false,
            ..Self::with(Box::new(|_, _| None))
        }
    }

    /// Holds sleep off the way this computer does it.
    pub(crate) fn system() -> Self {
        Self {
            able: system_can_hold(),
            ..Self::with(Box::new(system_hold))
        }
    }

    pub(crate) fn with(start: StartHold) -> Self {
        Self {
            start,
            able: true,
            while_working: true,
            work_keeps: Keeps::Computer,
            working: false,
            for_work: None,
            yours: YourHold::default(),
        }
    }

    /// Keeps the hold you asked for in `yours`, which outlives this screen.
    pub(crate) fn sharing(mut self, yours: YourHold) -> Self {
        self.yours = yours;
        self
    }

    /// Whether anything is working now, and what `[awake]` says now.
    pub(crate) fn working(&mut self, working: bool, settings: &AwakeSettings) {
        self.working = working;
        self.told(settings);
        self.settle();
    }

    /// A bare `/awake`: on until you quit when you hold nothing, back to the default when you
    /// do.
    pub(crate) fn switch(&mut self, settings: &AwakeSettings) -> Said {
        if self.held().is_some() {
            self.back_to_default(settings)
        } else {
            self.until_quit()
        }
    }

    /// `/awake on`: the computer and its screen, until you quit. Says what it did.
    pub(crate) fn until_quit(&mut self) -> Said {
        let Some(hold) = self.start_yours(None) else {
            return Said::cant();
        };
        *self.yours.lock() = Asked::UntilQuit { _hold: hold };
        self.settle();
        Said::held("Keeping the computer and its screen awake until you quit.".to_owned())
    }

    /// `/awake <time>`: for `span` from `now`, as you wrote it in `time`.
    pub(crate) fn for_a_time(&mut self, span: Duration, time: &str, now: OffsetDateTime) -> Said {
        let Some(hold) = self.start_yours(Some(span.as_secs().max(1))) else {
            return Said::cant();
        };
        *self.yours.lock() = Asked::Until {
            ends: Instant::now() + span,
            at: now + span,
            _hold: hold,
        };
        self.settle();
        Said::held(format!(
            "Keeping the computer and its screen awake for {time}."
        ))
    }

    /// `/awake off`, or a bare `/awake` while you hold one: back to the default, so work under
    /// way still holds sleep off when `[awake]` says so.
    pub(crate) fn back_to_default(&mut self, settings: &AwakeSettings) -> Said {
        if !self.able {
            return Said::cant();
        }
        self.told(settings);
        *self.yours.lock() = Asked::Nothing;
        self.settle();
        let still = match (settings.while_working, settings.screen_while_working) {
            (false, _) => "",
            (true, false) => " While agents work, the computer still stays awake.",
            (true, true) => " While agents work, the computer and its screen still stay awake.",
        };
        Said {
            text: format!("The computer and its screen may sleep again.{still}"),
            undo: Some("/awake turns it on.".to_owned()),
            caveat: None,
        }
    }

    /// The hold you asked for, for the bottom line. `None` when you asked for none, or its
    /// time is up.
    pub(crate) fn held(&self) -> Option<AwakeHold> {
        self.yours.lock().held()
    }

    /// Whether a run that goes on after you quit keeps the computer awake: its run host
    /// holds sleep off as `[awake]` says, on a computer that has a way to.
    pub(crate) fn runs_keep_awake(&self, settings: &AwakeSettings) -> bool {
        self.able && settings.while_working
    }

    fn start_yours(&self, secs: Option<u64>) -> Option<Box<dyn Hold>> {
        if !self.able {
            return None;
        }
        (self.start)(Keeps::ComputerAndScreen, secs)
    }

    fn told(&mut self, settings: &AwakeSettings) {
        self.while_working = settings.while_working;
        self.work_keeps = Keeps::while_working(settings);
    }

    /// Starts or ends the hold for work under way, as things stand.
    fn settle(&mut self) {
        // A time that's up leaves things as they were before you asked.
        {
            let mut yours = self.yours.lock();
            if matches!(&*yours, Asked::Until { .. }) && yours.held().is_none() {
                *yours = Asked::Nothing;
            }
        }
        let wanted = self.working && self.while_working;
        let held = self.for_work.as_ref().map(|(keeps, _)| *keeps);
        match (wanted, held) {
            (false, _) => self.for_work = None,
            // A changed setting swaps the hold for one that keeps what it now says.
            (true, held) if held != Some(self.work_keeps) => {
                self.for_work = None;
                self.for_work =
                    (self.start)(self.work_keeps, None).map(|hold| (self.work_keeps, hold));
            }
            (true, _) => {}
        }
    }
}

/// `/awake`, `/awake on`, `/awake <time>` and `/awake off`. The bottom line shows a hold you
/// asked for.
pub(crate) fn command(ctx: &mut Ctx, time: Option<String>) -> anyhow::Result<Vec<Effect>> {
    let settings = ctx.settings().awake.clone();
    let said = match time.as_deref() {
        None => ctx.awake.switch(&settings),
        Some("on") => ctx.awake.until_quit(),
        Some("off") => ctx.awake.back_to_default(&settings),
        Some(time) => match Span::new(time).parse() {
            Ok(Some(span)) if !span.is_zero() => {
                ctx.awake.for_a_time(span, time, OffsetDateTime::now_utc())
            }
            _ => anyhow::bail!("/awake takes on, off, or a time such as 2h or 45m"),
        },
    };
    match said {
        Said {
            text,
            undo: None,
            caveat: None,
        } => ctx.notice(NoteLevel::Info, text),
        Said { text, undo, caveat } => ctx.send(AppEvent::Switched { text, undo, caveat }),
    }
    Ok(vec![Effect::Status])
}

/// A hold for as long as this process hosts a run, for the run host, keeping what `[awake]`
/// says work under way keeps. `None` when the computer has no way Argonauts Code knows.
pub(crate) fn while_hosting(settings: &AwakeSettings) -> Option<Box<dyn Hold>> {
    system_hold(Keeps::while_working(settings), None)
}

#[cfg(target_os = "macos")]
fn system_can_hold() -> bool {
    std::path::Path::new(mac::CAFFEINATE).exists()
}

#[cfg(target_os = "macos")]
fn system_hold(keeps: Keeps, secs: Option<u64>) -> Option<Box<dyn Hold>> {
    mac::Caffeinate::start(keeps, secs).map(|hold| Box::new(hold) as Box<dyn Hold>)
}

#[cfg(target_os = "linux")]
fn system_can_hold() -> bool {
    linux::program().is_some()
}

#[cfg(target_os = "linux")]
fn system_hold(keeps: Keeps, secs: Option<u64>) -> Option<Box<dyn Hold>> {
    linux::Inhibit::start(keeps, secs).map(|hold| Box::new(hold) as Box<dyn Hold>)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn system_can_hold() -> bool {
    false
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn system_hold(_keeps: Keeps, _secs: Option<u64>) -> Option<Box<dyn Hold>> {
    None
}

/// What `systemd-inhibit` holds and runs: a wait that ends when the process `pid` does, or
/// after `secs`. Sleep alone leaves the screen to your settings; idle too keeps it from
/// dimming and locking. The tools go by their full paths, so a wrapper of the same name on
/// `PATH` is never what runs.
#[cfg(any(test, target_os = "linux"))]
fn inhibit_args(keeps: Keeps, secs: Option<u64>, pid: u32) -> Vec<String> {
    let (what, why) = match keeps {
        Keeps::Computer => ("sleep", "Keeping the computer awake"),
        Keeps::ComputerAndScreen => ("idle:sleep", "Keeping the computer and its screen awake"),
    };
    let mut args = vec![
        format!("--what={what}"),
        "--who=Argonauts Code".to_owned(),
        format!("--why={why}"),
        "--mode=block".to_owned(),
    ];
    if let Some(secs) = secs {
        args.extend(["/usr/bin/timeout".to_owned(), secs.to_string()]);
    }
    args.extend([
        "/usr/bin/tail".to_owned(),
        format!("--pid={pid}"),
        "-f".to_owned(),
        "/dev/null".to_owned(),
    ]);
    args
}

#[cfg(target_os = "macos")]
mod mac {
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};

    use super::Keeps;

    /// By its full path, so a wrapper of the same name on `PATH` is never what runs.
    pub(super) const CAFFEINATE: &str = "/usr/bin/caffeinate";

    /// A running `caffeinate`, stopped when dropped.
    pub(super) struct Caffeinate(Child);

    impl Caffeinate {
        /// `-i` holds idle sleep off, `-s` system sleep while on power, and `-d` keeps the
        /// display on. `-w` ties it to this process, so it ends with Argonauts Code even
        /// after a crash. `-t` ends it by itself.
        pub(super) fn start(keeps: Keeps, secs: Option<u64>) -> Option<Self> {
            let mut command = Command::new(CAFFEINATE);
            command
                .arg(flags(keeps))
                .args(["-w", &std::process::id().to_string()]);
            if let Some(secs) = secs {
                command.args(["-t", &secs.to_string()]);
            }
            let child = command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
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

    pub(super) fn flags(keeps: Keeps) -> &'static str {
        match keeps {
            Keeps::Computer => "-is",
            Keeps::ComputerAndScreen => "-dis",
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

#[cfg(target_os = "linux")]
mod linux {
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};

    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;

    use super::Keeps;

    /// Where `systemd-inhibit` lives; a merged `/usr` has both.
    const PLACES: [&str; 2] = ["/usr/bin/systemd-inhibit", "/bin/systemd-inhibit"];

    /// `systemd-inhibit`, where systemd started the computer and so logind takes holds.
    pub(super) fn program() -> Option<&'static str> {
        if !Path::new("/run/systemd/system").exists() {
            return None;
        }
        PLACES.into_iter().find(|place| Path::new(place).exists())
    }

    /// A running `systemd-inhibit` and the wait it holds sleep off around, in a process group
    /// of their own, stopped together when dropped.
    pub(super) struct Inhibit(Child);

    impl Inhibit {
        pub(super) fn start(keeps: Keeps, secs: Option<u64>) -> Option<Self> {
            let program = program()?;
            let child = Command::new(program)
                .args(super::inhibit_args(keeps, secs, std::process::id()))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn();
            match child {
                Ok(child) => Some(Self(child)),
                Err(err) => {
                    tracing::warn!("{program} didn't start, so the computer may sleep: {err}");
                    None
                }
            }
        }
    }

    impl super::Hold for Inhibit {}

    impl Drop for Inhibit {
        fn drop(&mut self) {
            if let Ok(pid) = i32::try_from(self.0.id()) {
                let _ = killpg(Pid::from_raw(pid), Signal::SIGTERM);
            }
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

    /// What each hold was started to keep awake, and for how long.
    type Started = Arc<Mutex<Vec<(Keeps, Option<u64>)>>>;

    /// A keeper whose live holds are counted.
    fn counted() -> (KeepAwake, Arc<AtomicUsize>, Started) {
        let held = Arc::new(AtomicUsize::new(0));
        let started = Started::default();
        let (count, log) = (held.clone(), started.clone());
        let start: StartHold = Box::new(move |keeps, secs| {
            log.lock().unwrap().push((keeps, secs));
            count.fetch_add(1, Ordering::SeqCst);
            Some(Box::new(Counted(count.clone())) as Box<dyn Hold>)
        });
        (KeepAwake::with(start), held, started)
    }

    fn settings(while_working: bool, screen_while_working: bool) -> AwakeSettings {
        AwakeSettings {
            while_working,
            screen_while_working,
        }
    }

    const HELD_UNTIL_QUIT: &str = "Keeping the computer and its screen awake until you quit.";
    const BACK: &str = "The computer and its screen may sleep again. While agents work, the computer still \
         stays awake.";

    #[test]
    fn it_holds_sleep_off_only_while_something_works() {
        let (mut awake, held, started) = counted();
        let on = settings(true, false);
        assert_eq!(held.load(Ordering::SeqCst), 0);
        awake.working(true, &on);
        awake.working(true, &on);
        assert_eq!(held.load(Ordering::SeqCst), 1, "one hold, however often");
        assert_eq!(awake.held(), None, "the bottom line names only your own");
        awake.working(false, &on);
        assert_eq!(held.load(Ordering::SeqCst), 0);

        // `awake.while_working = false`, and then true again while it works.
        awake.working(true, &settings(false, false));
        assert_eq!(held.load(Ordering::SeqCst), 0);
        awake.working(true, &on);
        assert_eq!(held.load(Ordering::SeqCst), 1);
        // Work under way leaves the screen to your own settings.
        assert_eq!(
            *started.lock().unwrap(),
            [(Keeps::Computer, None), (Keeps::Computer, None)]
        );
    }

    #[test]
    fn work_under_way_keeps_the_screen_on_only_when_the_setting_adds_it() {
        let (mut awake, held, started) = counted();
        awake.working(true, &settings(true, true));
        assert_eq!(*started.lock().unwrap(), [(Keeps::ComputerAndScreen, None)]);
        // Turning the setting off mid-work swaps the hold for one without the screen.
        awake.working(true, &settings(true, false));
        assert_eq!(held.load(Ordering::SeqCst), 1);
        assert_eq!(
            started.lock().unwrap().last(),
            Some(&(Keeps::Computer, None))
        );
        assert_eq!(started.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_bare_awake_switches_and_off_goes_back_to_the_default() {
        let (mut awake, held, started) = counted();
        let on = settings(true, false);
        let said = awake.switch(&on);
        assert_eq!(
            said,
            Said {
                text: HELD_UNTIL_QUIT.into(),
                undo: Some("/awake again lets them sleep.".into()),
                caveat: Some(LID.into()),
            }
        );
        assert_eq!(held.load(Ordering::SeqCst), 1);
        assert_eq!(awake.held(), Some(AwakeHold::UntilQuit));
        assert_eq!(*started.lock().unwrap(), [(Keeps::ComputerAndScreen, None)]);
        awake.working(true, &on);
        awake.working(false, &on);
        assert_eq!(held.load(Ordering::SeqCst), 1, "yours stays when work ends");

        // The second one lets them sleep, and work under way still holds sleep off.
        awake.working(true, &on);
        let said = awake.switch(&on);
        assert_eq!(said.text, BACK);
        assert_eq!(said.undo.as_deref(), Some("/awake turns it on."));
        assert_eq!(said.caveat, None);
        assert_eq!(held.load(Ordering::SeqCst), 1, "the hold for work stays");
        assert_eq!(awake.held(), None);
        awake.working(false, &on);
        assert_eq!(held.load(Ordering::SeqCst), 0);

        // A third turns it on again, and `/awake off` is the same as the second.
        assert_eq!(awake.switch(&on).text, HELD_UNTIL_QUIT);
        assert_eq!(awake.back_to_default(&on).text, BACK);
        assert_eq!(held.load(Ordering::SeqCst), 0);
        // So is a bare `/awake` while a time runs.
        awake.for_a_time(Duration::from_secs(60), "1m", OffsetDateTime::now_utc());
        assert_eq!(awake.switch(&on).text, BACK);
        assert_eq!(held.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn on_always_holds_and_off_says_what_the_settings_still_hold() {
        let (mut awake, held, _) = counted();
        assert_eq!(awake.until_quit().text, HELD_UNTIL_QUIT);
        assert_eq!(
            awake.until_quit().text,
            HELD_UNTIL_QUIT,
            "on is never a switch"
        );
        assert_eq!(
            held.load(Ordering::SeqCst),
            1,
            "the second replaced the first"
        );
        assert_eq!(
            awake.back_to_default(&settings(false, false)).text,
            "The computer and its screen may sleep again."
        );
        assert_eq!(
            awake.back_to_default(&settings(true, true)).text,
            "The computer and its screen may sleep again. While agents work, the computer and \
             its screen still stay awake."
        );
        // Off leaves work under way to the settings it was told.
        awake.working(true, &settings(false, false));
        assert_eq!(held.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_time_is_given_to_the_hold_and_reaches_the_bottom_line() {
        let (mut awake, held, started) = counted();
        let now = time::macros::datetime!(2026-10-07 13:45 UTC);
        let said = awake.for_a_time(Duration::from_secs(2 * 3600), "2h", now);
        assert_eq!(
            said.text,
            "Keeping the computer and its screen awake for 2h."
        );
        assert_eq!(said.undo.as_deref(), Some("/awake again lets them sleep."));
        assert_eq!(
            *started.lock().unwrap(),
            [(Keeps::ComputerAndScreen, Some(7200))]
        );
        assert_eq!(held.load(Ordering::SeqCst), 1);
        let until = time::macros::datetime!(2026-10-07 15:45 UTC);
        assert_eq!(awake.held(), Some(AwakeHold::Until { at: until }));
    }

    #[test]
    fn your_hold_lasts_into_the_next_screen_and_ends_with_the_app() {
        let (first, held, _) = counted();
        let yours = YourHold::default();
        let mut first = first.sharing(yours.clone());
        first.until_quit();
        // `/cd` closes this screen and opens the next with the same hold.
        drop(first);
        assert_eq!(held.load(Ordering::SeqCst), 1);
        let next = KeepAwake::never().sharing(yours.clone());
        assert_eq!(next.held(), Some(AwakeHold::UntilQuit));
        drop(next);
        drop(yours);
        assert_eq!(held.load(Ordering::SeqCst), 0, "quitting ends it");
    }

    #[test]
    fn the_command_says_what_it_did_and_work_under_way_holds_sleep_off_too() {
        use argonauts_core::app::AppMode;
        use argonauts_core::names::TurnId;

        use crate::controller::test_support::{CliOptions, TestCtx};

        let (mut parts, _) = TestCtx::new(CliOptions::default());
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

        command(ctx, None).unwrap();
        assert_eq!(held.load(Ordering::SeqCst), 0);
        command(ctx, Some("on".to_owned())).unwrap();
        command(ctx, Some("off".to_owned())).unwrap();
        assert_eq!(held.load(Ordering::SeqCst), 0);
        command(ctx, Some("45m".to_owned())).unwrap();
        assert_eq!(held.load(Ordering::SeqCst), 1);
        assert!(matches!(ctx.awake.held(), Some(AwakeHold::Until { .. })));
        assert!(command(ctx, Some("soon".to_owned())).is_err());

        let mut said = Vec::new();
        while let Ok(event) = parts.events.try_recv() {
            if let AppEvent::Switched { text, undo, .. } = event {
                said.push((text, undo.unwrap_or_default()));
            }
        }
        let line = |text: &str, undo: &str| (text.to_owned(), undo.to_owned());
        let again = "/awake again lets them sleep.";
        assert_eq!(
            said,
            [
                line(HELD_UNTIL_QUIT, again),
                line(BACK, "/awake turns it on."),
                line(HELD_UNTIL_QUIT, again),
                line(BACK, "/awake turns it on."),
                line("Keeping the computer and its screen awake for 45m.", again),
            ]
        );
    }

    #[test]
    fn a_run_left_going_keeps_the_computer_awake_only_where_it_can() {
        let (awake, _, _) = counted();
        assert!(awake.runs_keep_awake(&settings(true, false)));
        assert!(!awake.runs_keep_awake(&settings(false, false)));
        assert!(!KeepAwake::never().runs_keep_awake(&settings(true, false)));
    }

    #[test]
    fn linux_holds_sleep_around_a_wait_for_this_process() {
        assert_eq!(
            inhibit_args(Keeps::Computer, None, 4242),
            [
                "--what=sleep",
                "--who=Argonauts Code",
                "--why=Keeping the computer awake",
                "--mode=block",
                "/usr/bin/tail",
                "--pid=4242",
                "-f",
                "/dev/null",
            ]
        );
        assert_eq!(
            inhibit_args(Keeps::ComputerAndScreen, Some(7200), 4242)[..6],
            [
                "--what=idle:sleep",
                "--who=Argonauts Code",
                "--why=Keeping the computer and its screen awake",
                "--mode=block",
                "/usr/bin/timeout",
                "7200",
            ]
        );
    }

    /// The real thing on a Mac: `caffeinate` runs while the hold lives, in a process group of
    /// its own, and ends with it.
    #[cfg(target_os = "macos")]
    #[test]
    fn on_a_mac_the_hold_is_caffeinate_in_its_own_group_and_it_ends_when_dropped() {
        use nix::unistd::{Pid, getpgid};

        let alive = |pid: u32| {
            std::process::Command::new("/bin/kill")
                .args(["-0", &pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        };
        assert!(system_can_hold());
        assert_eq!(mac::flags(Keeps::Computer), "-is");
        assert_eq!(mac::flags(Keeps::ComputerAndScreen), "-dis");
        let hold = mac::Caffeinate::start(Keeps::Computer, Some(30)).expect("caffeinate starts");
        let pid = hold.pid();
        assert!(alive(pid));
        let own = Pid::from_raw(i32::try_from(pid).unwrap());
        assert_eq!(
            getpgid(Some(own)),
            Ok(own),
            "a Ctrl+C for the terminal never reaches it"
        );
        drop(hold);
        assert!(!alive(pid));
    }

    #[test]
    fn a_computer_with_no_way_to_hold_sleep_off_says_so() {
        let mut awake = KeepAwake::never();
        let on = settings(true, false);
        assert_eq!(awake.until_quit(), Said::cant());
        assert_eq!(awake.switch(&on), Said::cant());
        assert_eq!(awake.back_to_default(&on), Said::cant());
        assert_eq!(awake.held(), None);
        awake.working(true, &on);
        assert_eq!(awake.held(), None);
    }
}
