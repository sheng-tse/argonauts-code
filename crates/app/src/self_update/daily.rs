//! What Argonauts Code says about itself at start, and its daily look for a new release.
//!
//! The look runs in the background, at most once a day, so it never slows the start, and never
//! with `ARGONAUTS_DISABLE_UPDATES=1`, in CI, without a terminal, inside an agent's session or
//! with `updates.check` off. On a copy that updates itself with `updates.auto` on, a release
//! whose turn has come is put in place for the next start, never while a run is going. Any
//! other copy hears of it at the next start, once for each version, and so does a copy whose
//! update failed, with why. `argonauts doctor` shows what the last look came to.

use std::io::IsTerminal as _;
use std::path::PathBuf;

use argonauts_core::Paths;
use argonauts_core::log::{COULDNT_UPDATE_TO, NoteLevel};
use argonauts_core::settings::Settings;
use argonauts_core::settings::updates::UpdateSettings;
use argonauts_core::status_tab::{VERSION_ROW_NAMES, VERSION_ROW_STATES};
use argonauts_tui::format::WallClock;
use semver::Version;
use time::{Duration, OffsetDateTime};

use super::install::{Install, Places};
use super::manifest::{self, Found};
use super::shell::what_updates;
use super::state::{Look, Outcome, UpdateState};
use super::{DISABLE_ENV, Here, Layout, Source, apply, running_version, state_file};

/// How long after one look the next is due.
const DAILY: Duration = Duration::hours(24);

/// The row the first start after an update shows, such as `Updated to 0.1.1 ·
/// /release-notes`, once: never on the very first start, nor after going back to an older
/// version.
pub(crate) fn updated_row(paths: &Paths) -> Option<String> {
    updated_to(&last_version_file(paths), &running_version())
        .map(|version| format!("Updated to {version} · /release-notes"))
}

/// `~/.argonauts/state/last-version`: the version that started last.
fn last_version_file(paths: &Paths) -> PathBuf {
    paths.home().join("state").join("last-version")
}

fn updated_to(file: &std::path::Path, running: &Version) -> Option<Version> {
    let before = std::fs::read_to_string(file)
        .ok()
        .and_then(|text| Version::parse(text.trim()).ok());
    if before.as_ref() != Some(running) {
        if let Some(dir) = file.parent() {
            let _ = argonauts_core::paths::create_private_dir_all(dir);
        }
        if let Err(err) = std::fs::write(file, format!("{running}\n")) {
            tracing::debug!("couldn't keep the version that started: {err}");
        }
    }
    before
        .filter(|before| before < running)
        .map(|_| running.clone())
}

/// At start: the line that names a newer release this copy didn't take, once for each version,
/// and the daily look in the background when one is due here.
pub(crate) fn at_start(paths: &Paths, settings: &Settings) -> Vec<(NoteLevel, String)> {
    let Ok(source) = Source::for_home(paths) else {
        return Vec::new();
    };
    let here = Here::this_process(paths, source);
    let mut state = UpdateState::load(&here.state_file);
    let line = start_line(&here, &mut state);
    if line.is_some()
        && let Err(err) = state.save(&here.state_file)
    {
        tracing::debug!("couldn't keep which release was named: {err}");
    }
    let var = |name: &str| std::env::var(name).ok();
    let terminal = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let now = OffsetDateTime::now_utc();
    if may_look(&settings.updates, &var, terminal) && is_due(&state, now) {
        let updates = settings.updates.clone();
        let paths = paths.clone();
        tokio::spawn(async move {
            let run_going = || !crate::live_registry::live_run_hosts(&paths).is_empty();
            look(&here, &updates, &run_going, now).await;
        });
    }
    line.into_iter().collect()
}

/// Whether this process may look for a release on its own.
fn may_look(
    updates: &UpdateSettings,
    var: &dyn Fn(&str) -> Option<String>,
    terminal: bool,
) -> bool {
    let set = |name: &str| var(name).is_some_and(|value| !value.is_empty() && value != "0");
    let in_ci =
        var("CI").is_some_and(|value| !value.is_empty() && value != "false" && value != "0");
    let in_agent = var(crate::env::SESSION_ENV).is_some();
    updates.check && terminal && !in_ci && !in_agent && !set(DISABLE_ENV)
}

fn is_due(state: &UpdateState, now: OffsetDateTime) -> bool {
    state.checked.is_none_or(|checked| now - checked >= DAILY)
}

/// The one line a start says about a newer release: why the last look couldn't put it in place
/// when it failed, or else that it's out.
fn start_line(here: &Here, state: &mut UpdateState) -> Option<(NoteLevel, String)> {
    failed_line(here, state)
        .map(|line| (NoteLevel::Warn, line))
        .or_else(|| out_line(here, state).map(|line| (NoteLevel::Quiet, line)))
}

/// `Couldn't update to 0.1.1: the download stopped at 61% · argonauts self-update`, once for
/// each version, after a background update failed. Telling it stands for the release's `is
/// out` line too.
fn failed_line(here: &Here, state: &mut UpdateState) -> Option<String> {
    let Some(Outcome::Failed { version, why }) = state.last.as_ref().map(|last| &last.outcome)
    else {
        return None;
    };
    if *version <= here.current || state.told.as_ref() == Some(version) {
        return None;
    }
    let line = format!(
        "{COULDNT_UPDATE_TO}{version}: {why} · {}",
        what_updates(here.install)
    );
    state.told = Some(version.clone());
    Some(line)
}

/// `0.1.1 is out · brew upgrade argonauts-code`, once, for a release the last look found that
/// this copy didn't take itself. npx fetches the newest each time, so it hears nothing.
fn out_line(here: &Here, state: &mut UpdateState) -> Option<String> {
    let out = state.out.clone().filter(|out| *out > here.current)?;
    if state.told.as_ref() == Some(&out) || here.install == Install::Npx {
        return None;
    }
    state.told = Some(out.clone());
    Some(format!("{out} is out · {}", what_updates(here.install)))
}

/// Looks at the channel's manifest and does what its release calls for here, then keeps when
/// it looked and what came of it. A look that can't finish, offline or while a run holds the
/// version, is tried again at the next start.
async fn look(
    here: &Here,
    updates: &UpdateSettings,
    run_going: &(dyn Fn() -> bool + Sync),
    now: OffsetDateTime,
) {
    let found = match manifest::check(here, updates.channel, false, now).await {
        Ok(found) => found,
        Err(err) => {
            tracing::debug!("couldn't look for a new release: {err}");
            let mut state = UpdateState::load(&here.state_file);
            let why = format!("the {} manifest: {err}", updates.channel.as_str());
            keep(&mut state, here, now, Outcome::Unread { why });
            return;
        }
    };
    let mut state = UpdateState::load(&here.state_file);
    let outcome = match found {
        Found::Newer(release) if here.install == Install::Shell && updates.auto => {
            let version = release.version.clone();
            let mut failed = None;
            if here.layout.linked() != Some(version.clone()) {
                if run_going() {
                    return;
                }
                let never = tokio_util::sync::CancellationToken::new();
                if let Err(err) = apply::take(here, &release, &mut |_| {}, &never).await {
                    tracing::debug!("couldn't put {version} in place: {err}");
                    failed = Some(why_not(&err));
                }
            }
            match failed {
                Some(why) => {
                    state.out = Some(version.clone());
                    Outcome::Failed { version, why }
                }
                None => {
                    state.out = None;
                    state.ready = Some(version.clone());
                    Outcome::Ready { version }
                }
            }
        }
        Found::Newer(release) => {
            state.out = Some(release.version.clone());
            Outcome::Out {
                version: release.version,
            }
        }
        Found::NotYet(version) => {
            state.out = None;
            Outcome::Later { version }
        }
        Found::UpToDate | Found::Halted(_) => {
            state.out = None;
            Outcome::UpToDate
        }
    };
    state.checked = Some(now);
    keep(&mut state, here, now, outcome);
}

fn keep(state: &mut UpdateState, here: &Here, at: OffsetDateTime, outcome: Outcome) {
    state.last = Some(Look { at, outcome });
    if let Err(err) = state.save(&here.state_file) {
        tracing::debug!("couldn't keep what the update check came to: {err}");
    }
}

/// Why a release couldn't be put in place, in words that follow `Couldn't update to 0.1.1:`.
/// A download that ended early says how far it got.
fn why_not(err: &apply::ApplyError) -> String {
    match err {
        apply::ApplyError::Size { want, got } if got < want => {
            format!("the download stopped at {}%", got * 100 / want)
        }
        err => err.to_string(),
    }
}

/// `argonauts doctor`'s row for Argonauts Code itself, in the agent CLIs' columns: the version
/// running, what its last look for a new release came to, and when.
pub fn doctor_row(paths: &Paths) -> String {
    let layout = Layout::for_home(paths);
    let exe = std::env::current_exe().unwrap_or_default();
    let install = Install::detect(&exe, &Places::from_env(&layout));
    let state = UpdateState::load(&state_file(paths));
    look_row(
        &running_version(),
        state.last.as_ref(),
        install,
        &WallClock::local(),
    )
}

/// What `argonauts doctor --json` says of Argonauts Code itself: the version running, and what
/// its last look for a new release came to and when, the release it found and why it failed or
/// couldn't read, or null before the first look.
pub fn doctor_json(paths: &Paths) -> serde_json::Value {
    let state = UpdateState::load(&state_file(paths));
    serde_json::json!({
        "running": running_version().to_string(),
        "last_look": state.last,
    })
}

/// `Argonauts Code 0.2.0      update to 0.2.1 failed        last looked 10:41 AM · the download
/// stopped at 61% · argonauts self-update`. A release that runs now, put here since that look,
/// reads as up to date.
fn look_row(running: &Version, last: Option<&Look>, install: Install, clock: &WallClock) -> String {
    let name = format!("Argonauts Code {running}");
    let Some(last) = last else {
        return format!("{name:<VERSION_ROW_NAMES$}no update check yet");
    };
    let command = what_updates(install);
    let (state, detail) = match &last.outcome {
        Outcome::Ready { version } | Outcome::Out { version } | Outcome::Failed { version, .. }
            if version <= running =>
        {
            ("up to date".to_owned(), None)
        }
        Outcome::UpToDate => ("up to date".to_owned(), None),
        Outcome::Later { version } => (
            "up to date".to_owned(),
            Some(format!("{version} reaches this copy later in its rollout")),
        ),
        Outcome::Ready { version } => (
            format!("{version} downloaded"),
            Some("takes over at the next start".to_owned()),
        ),
        Outcome::Out { version } => (format!("{version} is out"), Some(command)),
        Outcome::Failed { version, why } => (
            format!("update to {version} failed"),
            Some(format!("{why} · {command}")),
        ),
        Outcome::Unread { why } => ("couldn't check".to_owned(), Some(why.clone())),
    };
    let mut row = format!(
        "{name:<VERSION_ROW_NAMES$}{state:<VERSION_ROW_STATES$}last looked {}",
        when(last.at, clock)
    );
    if let Some(detail) = detail {
        row.push_str(" · ");
        row.push_str(&detail);
    }
    row
}

/// `10:41 AM` today, `yesterday 10:41 AM`, then `Oct 23, 10:41 AM`.
fn when(at: OffsetDateTime, clock: &WallClock) -> String {
    let today = clock.now().date();
    let time = clock.clock(at);
    let day = clock.to_local(at).date();
    if day == today {
        time.to_string()
    } else if today.previous_day() == Some(day) {
        format!("yesterday {time}")
    } else {
        format!("{} {}, {time}", &day.month().to_string()[..3], day.day())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_update::test_server::{FakeServer, Signer, manifest, release_archive};
    use crate::self_update::{Layout, platform};

    #[test]
    fn the_doctors_json_says_when_the_last_look_was_and_what_it_came_to() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("argonauts"));
        let json = doctor_json(&paths);
        assert_eq!(json["running"], running_version().to_string());
        assert!(json["last_look"].is_null(), "{json}");
        let mut state = UpdateState::default();
        state.last = Some(Look {
            at: time::macros::datetime!(2026-10-26 10:41 UTC),
            outcome: Outcome::Failed {
                version: Version::parse("9.0.1").unwrap(),
                why: "the download stopped at 61%".to_owned(),
            },
        });
        std::fs::create_dir_all(paths.home()).unwrap();
        state.save(&state_file(&paths)).unwrap();
        let json = doctor_json(&paths);
        assert_eq!(json["last_look"]["at"], "2026-10-26T10:41:00Z");
        assert_eq!(json["last_look"]["outcome"]["kind"], "failed");
        assert_eq!(json["last_look"]["outcome"]["version"], "9.0.1");
        assert_eq!(
            json["last_look"]["outcome"]["why"],
            "the download stopped at 61%"
        );
    }

    #[test]
    fn the_first_start_after_an_update_says_so_once() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state/last-version");
        let v = |text: &str| Version::parse(text).unwrap();
        assert_eq!(updated_to(&file, &v("0.1.0")), None, "the very first start");
        assert_eq!(updated_to(&file, &v("0.1.0")), None);
        assert_eq!(updated_to(&file, &v("0.1.1")), Some(v("0.1.1")));
        assert_eq!(updated_to(&file, &v("0.1.1")), None, "once");
        assert_eq!(
            updated_to(&file, &v("0.1.0")),
            None,
            "going back isn't an update"
        );
        let paths = Paths::new(dir.path().join("argonauts"));
        assert_eq!(updated_row(&paths), None);
        std::fs::write(last_version_file(&paths), "0.0.1\n").unwrap();
        assert_eq!(
            updated_row(&paths),
            Some(format!("Updated to {} · /release-notes", running_version()))
        );
    }

    #[test]
    fn the_look_stays_quiet_in_ci_in_agents_and_without_a_terminal() {
        let on = UpdateSettings::default();
        let none = |_: &str| None;
        assert!(may_look(&on, &none, true));
        assert!(!may_look(&on, &none, false), "without a terminal");
        let ci = |name: &str| (name == "CI").then(|| "true".to_owned());
        assert!(!may_look(&on, &ci, true), "in CI");
        let ci_off = |name: &str| (name == "CI").then(|| "false".to_owned());
        assert!(may_look(&on, &ci_off, true));
        let agent = |name: &str| (name == crate::env::SESSION_ENV).then(|| "s-1".to_owned());
        assert!(!may_look(&on, &agent, true), "inside an agent's session");
        let off = |name: &str| (name == DISABLE_ENV).then(|| "1".to_owned());
        assert!(!may_look(&on, &off, true));
        let unchecked = UpdateSettings {
            check: false,
            ..UpdateSettings::default()
        };
        assert!(!may_look(&unchecked, &none, true));
    }

    #[test]
    fn a_look_is_due_once_a_day() {
        let now = OffsetDateTime::now_utc();
        let mut state = UpdateState::default();
        assert!(is_due(&state, now));
        state.checked = Some(now - Duration::hours(3));
        assert!(!is_due(&state, now));
        state.checked = Some(now - Duration::hours(25));
        assert!(is_due(&state, now));
    }

    fn here(server: &FakeServer, signer: &Signer, dir: &std::path::Path, install: Install) -> Here {
        Here {
            source: server.source(vec![signer.public()]),
            layout: Layout::under(&dir.join("home"), None, None),
            install,
            state_file: dir.join("update.json"),
            current: running_version(),
            platform: platform(),
        }
    }

    fn served_newer(server: &FakeServer, signer: &Signer) -> Version {
        let mut next = running_version();
        next.patch += 1;
        let mut newer = manifest(&next.to_string(), 3);
        newer.platforms.clear();
        if let Some(platform) = platform() {
            let download = server.release(&next.to_string(), release_archive(&next.to_string()));
            newer.platforms.insert(platform.to_owned(), download);
        }
        server.publish("stable", &newer, signer);
        next
    }

    #[tokio::test]
    async fn a_copy_that_updates_itself_takes_the_release_in_the_background() {
        let server = FakeServer::start().await;
        let signer = Signer::new();
        let dir = tempfile::tempdir().unwrap();
        let next = served_newer(&server, &signer);
        let here = here(&server, &signer, dir.path(), Install::Shell);
        let now = OffsetDateTime::now_utc();
        // Not while a run is going: the look is tried again next time.
        look(&here, &UpdateSettings::default(), &|| true, now).await;
        assert_eq!(here.layout.linked(), None);
        assert_eq!(UpdateState::load(&here.state_file).checked, None);

        look(&here, &UpdateSettings::default(), &|| false, now).await;
        assert_eq!(here.layout.linked(), Some(next.clone()));
        let mut state = UpdateState::load(&here.state_file);
        assert_eq!(state.ready, Some(next));
        assert!(state.checked.is_some());
        assert_eq!(out_line(&here, &mut state), None, "nothing to tell");
    }

    #[tokio::test]
    async fn a_failed_update_says_why_once_instead_of_the_release_being_out() {
        let server = FakeServer::start().await;
        let signer = Signer::new();
        let dir = tempfile::tempdir().unwrap();
        let mut next = running_version();
        next.patch += 1;
        let mut newer = manifest(&next.to_string(), 3);
        newer.platforms.clear();
        if let Some(platform) = platform() {
            // The archive's size and sha256 are the whole one's, but the server sends 61% of it
            // and closes, as a dropped connection behind a proxy does.
            let archive = release_archive(&next.to_string());
            let download = server.release(&next.to_string(), archive.clone());
            let path = &download.url[download.url.find("/download/").unwrap()..];
            let part = (archive.len() * 61).div_ceil(100);
            server.put(path, archive[..part].to_vec());
            newer.platforms.insert(platform.to_owned(), download);
        }
        server.publish("stable", &newer, &signer);
        let here = here(&server, &signer, dir.path(), Install::Shell);
        let now = OffsetDateTime::now_utc();
        look(&here, &UpdateSettings::default(), &|| false, now).await;
        assert_eq!(here.layout.linked(), None, "nothing took over");

        let mut state = UpdateState::load(&here.state_file);
        assert!(state.checked.is_some());
        assert_eq!(state.ready, None);
        if platform().is_none() {
            return;
        }
        assert_eq!(
            state.last,
            Some(Look {
                at: now,
                outcome: Outcome::Failed {
                    version: next.clone(),
                    why: "the download stopped at 61%".to_owned(),
                },
            })
        );
        assert_eq!(
            start_line(&here, &mut state),
            Some((
                NoteLevel::Warn,
                format!(
                    "Couldn't update to {next}: the download stopped at 61% · argonauts self-update"
                )
            ))
        );
        assert_eq!(
            start_line(&here, &mut state),
            None,
            "said once, and the release isn't named again as out"
        );
    }

    #[tokio::test]
    async fn a_look_that_cant_read_the_manifest_is_kept_and_tried_again() {
        let server = FakeServer::start().await;
        let signer = Signer::new();
        let dir = tempfile::tempdir().unwrap();
        let here = here(&server, &signer, dir.path(), Install::Shell);
        let now = OffsetDateTime::now_utc();
        look(&here, &UpdateSettings::default(), &|| false, now).await;
        let mut state = UpdateState::load(&here.state_file);
        assert_eq!(state.checked, None, "due again at the next start");
        let last = state.last.clone().expect("the look is kept");
        assert_eq!(last.at, now);
        let Outcome::Unread { why } = last.outcome else {
            panic!("{last:?}");
        };
        assert!(why.starts_with("the stable manifest: "), "{why}");
        assert_eq!(start_line(&here, &mut state), None, "a start says nothing");
    }

    /// Each thing the last look can come to, as `argonauts doctor` shows it.
    #[test]
    fn the_doctor_row_shows_the_last_look() {
        let at = time::macros::datetime!(2026-10-24 10:41 -7);
        let clock = WallClock::pinned(time::macros::datetime!(2026-10-24 18:05 -7));
        let running = Version::new(0, 2, 0);
        let next = Version::new(0, 2, 1);
        let row = |outcome: Outcome, install: Install| {
            look_row(&running, Some(&Look { at, outcome }), install, &clock)
        };
        let rows = [
            look_row(&running, None, Install::Shell, &clock),
            row(Outcome::UpToDate, Install::Shell),
            row(
                Outcome::Later {
                    version: next.clone(),
                },
                Install::Shell,
            ),
            row(
                Outcome::Ready {
                    version: next.clone(),
                },
                Install::Shell,
            ),
            row(
                Outcome::Out {
                    version: next.clone(),
                },
                Install::Homebrew,
            ),
            row(
                Outcome::Failed {
                    version: next.clone(),
                    why: "the download stopped at 61%".to_owned(),
                },
                Install::Shell,
            ),
            row(
                Outcome::Unread {
                    why: "the stable manifest: its signature isn't from a key Argonauts Code \
                          trusts"
                        .to_owned(),
                },
                Install::Shell,
            ),
        ];
        let since = look_row(
            &next,
            Some(&Look {
                at,
                outcome: Outcome::Failed {
                    version: next.clone(),
                    why: "the download stopped at 61%".to_owned(),
                },
            }),
            Install::Shell,
            &clock,
        );
        assert_eq!(
            since, "Argonauts Code 0.2.1      up to date                    last looked 10:41 AM",
            "the release that failed runs now"
        );
        assert_eq!(
            rows.join("\n"),
            "\
Argonauts Code 0.2.0      no update check yet
Argonauts Code 0.2.0      up to date                    last looked 10:41 AM
Argonauts Code 0.2.0      up to date                    last looked 10:41 AM · 0.2.1 reaches this copy later in its rollout
Argonauts Code 0.2.0      0.2.1 downloaded              last looked 10:41 AM · takes over at the next start
Argonauts Code 0.2.0      0.2.1 is out                  last looked 10:41 AM · brew upgrade argonauts-code
Argonauts Code 0.2.0      update to 0.2.1 failed        last looked 10:41 AM · the download stopped at 61% · argonauts self-update
Argonauts Code 0.2.0      couldn't check                last looked 10:41 AM · the stable manifest: its signature isn't from a key Argonauts Code trusts"
        );
    }

    #[test]
    fn the_doctor_row_names_the_day_when_it_isnt_today() {
        let clock = WallClock::pinned(time::macros::datetime!(2026-10-24 9:00 -7));
        assert_eq!(
            when(time::macros::datetime!(2026-10-23 22:58 -7), &clock),
            "yesterday 10:58 PM"
        );
        assert_eq!(
            when(time::macros::datetime!(2026-10-08 6:41 -7), &clock),
            "Oct 8, 6:41 AM"
        );
        // Kept in UTC, shown in the clock's zone.
        assert_eq!(
            when(time::macros::datetime!(2026-10-24 15:30 UTC), &clock),
            "8:30 AM"
        );
    }

    #[tokio::test]
    async fn any_other_copy_hears_of_the_release_once_at_the_next_start() {
        let server = FakeServer::start().await;
        let signer = Signer::new();
        let dir = tempfile::tempdir().unwrap();
        let next = served_newer(&server, &signer);
        let notify = UpdateSettings {
            auto: false,
            ..UpdateSettings::default()
        };
        for (install, command) in [
            (Install::Shell, "argonauts self-update"),
            (Install::Homebrew, "brew upgrade argonauts-code"),
        ] {
            let _ = std::fs::remove_file(dir.path().join("update.json"));
            let here = here(&server, &signer, dir.path(), install);
            look(&here, &notify, &|| false, OffsetDateTime::now_utc()).await;
            assert_eq!(here.layout.linked(), None, "{install:?} replaced nothing");
            let mut state = UpdateState::load(&here.state_file);
            assert_eq!(
                out_line(&here, &mut state),
                Some(format!("{next} is out · {command}"))
            );
            assert_eq!(out_line(&here, &mut state), None, "once for each version");
        }
        let mut state = UpdateState::default();
        state.out = Some(next);
        let npx = here(&server, &signer, dir.path(), Install::Npx);
        assert_eq!(out_line(&npx, &mut state), None, "npx hears nothing");
    }
}
