//! Argonauts Code's own row at the top of `/update`'s report, in the agent CLIs' columns, and
//! the line under their rows that says how it updates. `/update` is a self-update you asked
//! for, so it takes the newest release on your channel at once, whatever the rollout's
//! schedule, and it never runs while a run is going.

use argonauts_core::base::clock;
use argonauts_core::settings::updates::UpdateSettings;
use tokio_util::sync::CancellationToken;

use super::install::Install;
use super::manifest::{self, Found};
use super::shell::what_updates;
use super::{Here, Off, apply, running_version};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OwnRow {
    /// The version running now, such as `0.1.0`.
    pub version: String,
    /// What became of it, such as `up to date`.
    pub words: String,
}

/// Looks for a newer Argonauts Code on `updates.channel` and, on a copy that updates itself,
/// puts it in place for the next start.
pub(crate) async fn own_row(
    here: Result<Here, Off>,
    updates: &UpdateSettings,
    stop: &CancellationToken,
) -> OwnRow {
    let version = running_version().to_string();
    let here = match here {
        Ok(here) => here,
        Err(off) => {
            return OwnRow {
                version,
                words: off.to_string(),
            };
        }
    };
    let found = tokio::select! {
        found = manifest::check(&here, updates.channel, true, clock::now_utc()) => found,
        () = stop.cancelled() => return OwnRow {
            version,
            words: "stopped".to_owned(),
        },
    };
    let words = match found {
        Err(err) => format!(
            "couldn't check: the {} manifest: {err}",
            updates.channel.as_str()
        ),
        Ok(Found::UpToDate | Found::NotYet(_) | Found::Halted(_)) => "up to date".to_owned(),
        Ok(Found::Newer(release)) if here.install != Install::Shell => {
            format!(
                "{} is out · {}",
                release.version,
                what_updates(here.install)
            )
        }
        // Switched to in the background already, waiting for the next start.
        Ok(Found::Newer(release)) if here.layout.linked() == Some(release.version.clone()) => {
            taken(&release.version)
        }
        Ok(Found::Newer(release)) => match apply::take(&here, &release, &mut |_| {}, stop).await {
            Ok(_) => taken(&release.version),
            Err(apply::ApplyError::Stopped) => "stopped".to_owned(),
            Err(err) => format!("{} couldn't be put in place: {err}", release.version),
        },
    };
    OwnRow { version, words }
}

fn taken(version: &semver::Version) -> String {
    format!("{version} downloaded · takes over at the next start · /release-notes")
}

/// The line under the CLIs' rows: how this copy updates and what changes that.
pub(crate) fn how_it_updates(install: Install, updates: &UpdateSettings) -> String {
    let channel = updates.channel.as_str();
    match install {
        Install::Shell if !updates.check => {
            "Argonauts Code doesn't look for new releases on its own. /config changes that."
                .to_owned()
        }
        Install::Shell if updates.auto => format!(
            "Argonauts Code updates itself on {channel}, automatically. /config changes either."
        ),
        Install::Shell => format!(
            "Argonauts Code says when {channel} has a new release, and argonauts self-update \
             installs it. /config changes either."
        ),
        Install::Npx => "npx runs the newest Argonauts Code each time.".to_owned(),
        Install::Elsewhere => "This copy of Argonauts Code wasn't put here by its installer, so \
                               it doesn't replace itself."
            .to_owned(),
        managed => {
            let manager = managed.manager().unwrap_or_default();
            format!("{manager} installed Argonauts Code, so {manager} updates it.")
        }
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::updates::UpdateChannel;

    use super::*;
    use crate::self_update::test_server::{FakeServer, Signer, manifest, release_archive};
    use crate::self_update::{Layout, platform};

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

    /// A release one patch past this build, served with its archive for this computer.
    fn newer(server: &FakeServer, signer: &Signer) -> semver::Version {
        let mut next = running_version();
        next.patch += 1;
        let mut newer = manifest(&next.to_string(), 7);
        newer.platforms.clear();
        if let Some(platform) = platform() {
            let download = server.release(&next.to_string(), release_archive(&next.to_string()));
            newer.platforms.insert(platform.to_owned(), download);
        }
        server.publish("stable", &newer, signer);
        next
    }

    #[tokio::test]
    async fn a_copy_that_updates_itself_takes_the_newer_release_for_the_next_start() {
        let server = FakeServer::start().await;
        let signer = Signer::new();
        let dir = tempfile::tempdir().unwrap();
        let next = newer(&server, &signer);
        let here = here(&server, &signer, dir.path(), Install::Shell);
        let row = own_row(
            Ok(here.clone()),
            &UpdateSettings::default(),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(row.version, running_version().to_string());
        assert_eq!(
            row.words,
            format!("{next} downloaded · takes over at the next start · /release-notes")
        );
        assert_eq!(here.layout.linked(), Some(next.clone()));
        // Once it's in place, the next /update says so again without downloading it twice.
        let downloads = |server: &FakeServer| {
            server
                .asked()
                .iter()
                .filter(|path| path.ends_with(".tar.xz"))
                .count()
        };
        assert_eq!(downloads(&server), 1);
        let again = own_row(
            Ok(here),
            &UpdateSettings::default(),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(again.words, row.words);
        assert_eq!(downloads(&server), 1);
    }

    #[tokio::test]
    async fn a_copy_a_package_manager_owns_names_its_command() {
        let server = FakeServer::start().await;
        let signer = Signer::new();
        let dir = tempfile::tempdir().unwrap();
        let next = newer(&server, &signer);
        let here = here(&server, &signer, dir.path(), Install::Homebrew);
        let row = own_row(
            Ok(here.clone()),
            &UpdateSettings::default(),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(
            row.words,
            format!("{next} is out · brew upgrade argonauts-code")
        );
        assert_eq!(here.layout.linked(), None, "nothing was put in place");
    }

    #[tokio::test]
    async fn what_went_wrong_reads_in_the_row() {
        let server = FakeServer::start().await;
        let (signer, stranger) = (Signer::new(), Signer::new());
        let dir = tempfile::tempdir().unwrap();
        newer(&server, &stranger);
        let here = here(&server, &signer, dir.path(), Install::Shell);
        let row = own_row(
            Ok(here),
            &UpdateSettings::default(),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(
            row.words,
            "couldn't check: the stable manifest: its signature isn't from a key Argonauts Code \
             trusts"
        );
        let off = own_row(
            Err(Off::Disabled),
            &UpdateSettings::default(),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(
            off.words,
            "updates are off: ARGONAUTS_DISABLE_UPDATES is set"
        );
    }

    #[test]
    fn the_line_under_the_rows_says_how_this_copy_updates() {
        let mut updates = UpdateSettings::default();
        assert_eq!(
            how_it_updates(Install::Shell, &updates),
            "Argonauts Code updates itself on stable, automatically. /config changes either."
        );
        updates.auto = false;
        updates.channel = UpdateChannel::Beta;
        assert_eq!(
            how_it_updates(Install::Shell, &updates),
            "Argonauts Code says when beta has a new release, and argonauts self-update installs \
             it. /config changes either."
        );
        assert_eq!(
            how_it_updates(Install::Homebrew, &updates),
            "Homebrew installed Argonauts Code, so Homebrew updates it."
        );
    }
}
