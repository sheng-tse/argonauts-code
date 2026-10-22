//! What Argonauts Code says about itself at start, and its daily look for a new release.
//!
//! The look runs in the background, at most once a day, so it never slows the start, and never
//! with `ARGONAUTS_DISABLE_UPDATES=1`, in CI, without a terminal, inside an agent's session or
//! with `updates.check` off. On a copy that updates itself with `updates.auto` on, a release
//! whose turn has come is put in place for the next start, never while a run is going. Any
//! other copy hears of it at the next start, once for each version.

use std::io::IsTerminal as _;
use std::path::PathBuf;

use argonauts_core::Paths;
use argonauts_core::log::NoteLevel;
use argonauts_core::settings::Settings;
use argonauts_core::settings::updates::UpdateSettings;
use semver::Version;
use time::{Duration, OffsetDateTime};

use super::install::Install;
use super::manifest::{self, Found};
use super::shell::what_updates;
use super::state::UpdateState;
use super::{DISABLE_ENV, Here, Source, apply, running_version};

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

/// At start: the line that names a newer release this copy can't take itself, once for each
/// version, and the daily look in the background when one is due here.
pub(crate) fn at_start(paths: &Paths, settings: &Settings) -> Vec<(NoteLevel, String)> {
    let Ok(source) = Source::for_home(paths) else {
        return Vec::new();
    };
    let here = Here::this_process(paths, source);
    let mut state = UpdateState::load(&here.state_file);
    let line = out_line(&here, &mut state);
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
    line.map(|line| vec![(NoteLevel::Quiet, line)])
        .unwrap_or_default()
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
/// it looked. A look that can't finish, offline or while a run holds the version, is tried
/// again at the next start.
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
            return;
        }
    };
    let mut state = UpdateState::load(&here.state_file);
    match found {
        Found::Newer(release) if here.install == Install::Shell && updates.auto => {
            if here.layout.linked() != Some(release.version.clone()) {
                if run_going() {
                    return;
                }
                let never = tokio_util::sync::CancellationToken::new();
                if let Err(err) = apply::take(here, &release, &mut |_| {}, &never).await {
                    tracing::debug!("couldn't put {} in place: {err}", release.version);
                    state.out = Some(release.version.clone());
                }
            }
            if state.out.as_ref() != Some(&release.version) {
                state.ready = Some(release.version);
            }
        }
        Found::Newer(release) => state.out = Some(release.version),
        Found::UpToDate | Found::NotYet(_) | Found::Halted(_) => state.out = None,
    }
    state.checked = Some(now);
    if let Err(err) = state.save(&here.state_file) {
        tracing::debug!("couldn't keep when the update check looked: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_update::test_server::{FakeServer, Signer, manifest, release_archive};
    use crate::self_update::{Layout, platform};

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
