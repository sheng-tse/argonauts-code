//! The one line that says usage reports are on, shown once per field list: at the first start,
//! or on stderr at the first `argonauts run`. Nothing is sent until it has shown.
//!
//! "Once" is a marker, `~/.argonauts/state/telemetry-notice-v<N>`, made with `create_new`, so of
//! two processes starting together only the one that made it shows the line. A marker that can't
//! be made leaves the line to show again next time, and nothing is sent meanwhile.

use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::PathBuf;

use argonauts_core::Paths;
use argonauts_core::create_private_dir_all;
use argonauts_core::settings::Settings;

use super::event::FIELDS_VERSION;

/// The line at a start, under the greeting or among the start's notices.
pub const LINE: &str = "Sends one anonymous usage report a day, never your code or prompts \
                        (PRIVACY.md) · argonauts telemetry off stops it";

/// The line `argonauts run` writes to stderr.
pub const RUN_LINE: &str = "argonauts: one anonymous usage report a day, never your code or \
                            prompts (PRIVACY.md). argonauts telemetry off stops it";

/// The command in the line, which the screen draws as a command.
pub const STOP_COMMAND: &str = "argonauts telemetry off";

/// The marker for the field list in use.
pub(crate) fn marker(paths: &Paths) -> PathBuf {
    paths
        .home()
        .join("state")
        .join(format!("telemetry-notice-v{FIELDS_VERSION}"))
}

/// The line has shown for the field list in use, so reports may go.
pub(crate) fn shown(paths: &Paths) -> bool {
    marker(paths).exists()
}

/// Whether this process is the one to show the line now: reports are on, nothing asks for
/// quiet, and it made the marker. A marker that couldn't be made still shows the line, which
/// shows again next time, since nothing is sent without the marker.
pub(crate) fn claim(paths: &Paths, settings: &Settings) -> bool {
    claim_in(paths, settings, &|name| std::env::var(name).ok())
}

/// [`claim`], with the environment `var` reads.
pub(crate) fn claim_in(
    paths: &Paths,
    settings: &Settings,
    var: &dyn Fn(&str) -> Option<String>,
) -> bool {
    if !settings.telemetry.enabled || quiet(var) {
        return false;
    }
    let marker = marker(paths);
    let made = marker
        .parent()
        .map_or(Ok(()), create_private_dir_all)
        .and_then(|()| {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&marker)
                .map(drop)
        });
    match made {
        Ok(()) => true,
        Err(err) if err.kind() == ErrorKind::AlreadyExists => false,
        Err(err) => {
            tracing::debug!("couldn't keep that the usage line showed: {err}");
            true
        }
    }
}

/// No line and no report: in CI, with `DO_NOT_TRACK`, and in an agent's own session.
pub(crate) fn quiet(var: &dyn Fn(&str) -> Option<String>) -> bool {
    let set = |name: &str| {
        var(name).is_some_and(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "" | "0" | "false"
            )
        })
    };
    set("CI") || set("DO_NOT_TRACK") || var("ARGONAUTS_SESSION").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> Settings {
        let mut settings = Settings::default();
        settings.telemetry.enabled = true;
        settings
    }

    /// Outside CI, whatever the tests run in.
    fn claim(paths: &Paths, settings: &Settings) -> bool {
        claim_in(paths, settings, &|_| None)
    }

    #[test]
    fn the_line_shows_once_per_field_list() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        assert!(!shown(&paths));
        assert!(claim(&paths, &on()));
        assert!(shown(&paths));
        assert!(!claim(&paths, &on()), "once");
        assert!(marker(&paths).ends_with("state/telemetry-notice-v3"));
        // A newer field list is a new marker, so the line shows once more.
        std::fs::remove_file(marker(&paths)).unwrap();
        std::fs::write(paths.home().join("state/telemetry-notice-v1"), "").unwrap();
        assert!(claim(&paths, &on()));
        // In CI, no line, and no marker.
        let ci = tempfile::tempdir().unwrap();
        let ci = Paths::new(ci.path());
        assert!(!claim_in(&ci, &on(), &|name| (name == "CI")
            .then(|| "true".to_owned())));
        assert!(!shown(&ci));
    }

    #[test]
    fn of_two_starting_together_one_shows_the_line() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let settings = on();
        let shown: Vec<bool> = std::thread::scope(|scope| {
            let racers: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| claim(&paths, &settings)))
                .collect();
            racers
                .into_iter()
                .map(|racer| racer.join().unwrap())
                .collect()
        });
        assert_eq!(shown.iter().filter(|shown| **shown).count(), 1, "{shown:?}");
    }

    #[test]
    fn reports_off_show_no_line() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        assert!(!claim(
            &paths,
            &Settings {
                telemetry: argonauts_core::settings::telemetry::TelemetrySettings {
                    enabled: false
                },
                ..Settings::default()
            }
        ));
        assert!(!shown(&paths));
    }

    #[test]
    fn ci_do_not_track_and_an_agents_session_are_quiet() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            }
        };
        assert!(quiet(&env(&[("CI", "true")])));
        assert!(quiet(&env(&[("CI", "1")])));
        assert!(!quiet(&env(&[("CI", "false")])));
        assert!(!quiet(&env(&[("CI", "")])));
        assert!(quiet(&env(&[("DO_NOT_TRACK", "1")])));
        assert!(!quiet(&env(&[("DO_NOT_TRACK", "0")])));
        assert!(quiet(&env(&[("ARGONAUTS_SESSION", "")])));
        assert!(!quiet(&env(&[])));
    }
}
