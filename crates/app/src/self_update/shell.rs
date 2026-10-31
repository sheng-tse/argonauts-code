//! `argonauts self-update` in a shell: updates this copy from its channel's newest release, to
//! a version you name, or back to the one before, and prints each step as it's done. A copy a
//! package manager installed is that manager's to update, so it gets the manager's command.

use std::io::{IsTerminal as _, Write as _};
use std::process::ExitCode;

use argonauts_core::Paths;
use argonauts_core::base::clock;
use argonauts_core::settings::updates::UpdateChannel;
use argonauts_tui::base::theme::{ColorEnv, Role, Theme};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use semver::Version;

use super::apply::{self, Step, Switched};
use super::install::{Install, RELEASES};
use super::manifest::{self, Found};
use super::{Here, Source};

/// What `argonauts self-update` was asked to do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Asked {
    /// Only say whether a newer version is out.
    pub check: bool,
    /// Follow this channel from now on.
    pub channel: Option<UpdateChannel>,
    /// Install this version rather than the newest.
    pub version: Option<String>,
    /// Go back to the version before.
    pub rollback: bool,
}

/// The column the step names take, as `  Downloaded  ` fills it.
const STEP_WIDTH: usize = 12;

/// Runs `argonauts self-update`, with `overrides` as `--set` gives them.
pub async fn self_update(asked: Asked, overrides: Vec<String>) -> anyhow::Result<ExitCode> {
    let mut out = Printer::stdout();
    let source = match Source::from_env() {
        Ok(source) => source,
        Err(off) => {
            out.line(plain(&format!("Argonauts Code isn't updated here: {off}.")));
            return Ok(ExitCode::FAILURE);
        }
    };
    if !asked.check {
        crate::startup::ancestry::refuse_inside_agent()?;
    }
    let paths = Paths::from_env()?;
    let settings = crate::commands::current_settings(None, overrides).unwrap_or_default();
    let here = Here::this_process(&paths, source);
    let channel = asked.channel.unwrap_or(settings.updates.channel);
    if asked.check {
        return check(&here, channel, &mut out).await;
    }
    // A package manager's releases are the only ones its install gets, and none is a beta, so
    // a channel asked for there changes nothing and is kept nowhere.
    if here.install.manager().is_some()
        && let Some(lines) = not_ours(here.install, &out.theme)
    {
        out.lines(lines);
        if asked.channel.is_some() {
            out.line(plain(&format!(
                "It follows {}'s releases, so there's no channel to choose.",
                here.install.manager().unwrap_or_default()
            )));
            return Ok(ExitCode::FAILURE);
        }
        return Ok(ExitCode::SUCCESS);
    }
    if !crate::host::live_registry::live_run_hosts(&paths).is_empty() {
        out.line(plain(RUN_GOING));
        return Ok(ExitCode::FAILURE);
    }
    if let Some(asked_for) = asked.channel
        && asked_for != settings.updates.channel
    {
        argonauts_registry::write_setting(
            &paths.global_config(),
            "updates.channel",
            &format!("\"{}\"", asked_for.as_str()),
        )?;
        out.line(plain(&format!(
            "From now on Argonauts Code follows {}.",
            asked_for.as_str()
        )));
    }
    if let Some(lines) = not_ours(here.install, &out.theme) {
        out.lines(lines);
        return Ok(ExitCode::SUCCESS);
    }
    let result = if asked.rollback {
        roll_back(&here, &mut out)
    } else {
        update(&here, channel, asked.version.as_deref(), &mut out).await
    };
    Ok(match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            out.line(plain(&format!("Couldn't update Argonauts Code: {why}.")));
            ExitCode::FAILURE
        }
    })
}

/// Why nothing switches while a run goes.
const RUN_GOING: &str = "A run is going, and Argonauts Code switches versions between runs. Run \
                         this again once it has ended.";

/// `--check`: whether a newer version is out on `channel`, and what installs it.
async fn check(here: &Here, channel: UpdateChannel, out: &mut Printer) -> anyhow::Result<ExitCode> {
    let found = manifest::check(here, channel, true, clock::now_utc()).await;
    let line = match found {
        Ok(Found::Newer(release)) => format!(
            "Argonauts Code {} is out ({}) · {}",
            release.version,
            channel.as_str(),
            what_updates(here.install)
        ),
        Ok(Found::Halted(version)) => format!(
            "Argonauts Code {version} is on hold on {}, so {} stays.",
            channel.as_str(),
            here.current
        ),
        Ok(Found::UpToDate | Found::NotYet(_)) => format!(
            "Argonauts Code {} is up to date ({}).",
            here.current,
            channel.as_str()
        ),
        Err(why) => {
            out.line(plain(&format!(
                "Couldn't check for a newer Argonauts Code: {why}."
            )));
            return Ok(ExitCode::FAILURE);
        }
    };
    out.line(plain(&line));
    Ok(ExitCode::SUCCESS)
}

/// What puts a newer version in place for a copy installed the way `install` was, as the
/// words after `0.1.1 is out · ` say it.
pub(super) fn what_updates(install: Install) -> String {
    match install {
        Install::Shell => "argonauts self-update".to_owned(),
        Install::Npx => "npx fetches it on its next run".to_owned(),
        Install::Elsewhere => RELEASES.to_owned(),
        managed => managed.command().unwrap_or_default().to_owned(),
    }
}

/// What to do instead for a copy Argonauts Code doesn't replace itself, or `None` for one it
/// does.
fn not_ours(install: Install, theme: &Theme) -> Option<Vec<Line<'static>>> {
    match install {
        Install::Shell => None,
        Install::Npx => Some(vec![plain(
            "npx runs the newest Argonauts Code each time, so there's nothing to update.",
        )]),
        Install::Elsewhere => Some(vec![
            plain(
                "This copy of Argonauts Code wasn't put here by its installer, so it doesn't \
                 replace itself.",
            ),
            plain(&format!("Every release, with its installer: {RELEASES}")),
        ]),
        managed => Some(managed_lines(
            managed.manager().unwrap_or_default(),
            managed.command().unwrap_or_default(),
            theme,
        )),
    }
}

/// `Homebrew installed Argonauts Code, so Homebrew updates it:` and the command under it.
pub(crate) fn managed_lines(manager: &str, command: &str, theme: &Theme) -> Vec<Line<'static>> {
    vec![
        plain(&format!(
            "{manager} installed Argonauts Code, so {manager} updates it:"
        )),
        Line::from(Span::styled(
            format!("  {command}"),
            theme.style(Role::Code),
        )),
    ]
}

async fn update(
    here: &Here,
    channel: UpdateChannel,
    version: Option<&str>,
    out: &mut Printer,
) -> Result<(), String> {
    let release = match version {
        Some(version) => {
            let version = Version::parse(version.trim_start_matches('v'))
                .map_err(|_| format!("{version} isn't a version, such as 0.1.2"))?;
            if version == here.current {
                out.line(plain(&format!("Argonauts Code is {version} already.")));
                return Ok(());
            }
            manifest::named(here, &version)
                .await
                .map_err(|err| format!("the manifest for {version}: {err}"))?
        }
        None => match manifest::check(here, channel, true, clock::now_utc()).await {
            Ok(Found::Newer(release)) => release,
            Ok(Found::Halted(version)) => {
                out.line(plain(&format!(
                    "Argonauts Code {version} is on hold on {}, so {} stays.",
                    channel.as_str(),
                    here.current
                )));
                return Ok(());
            }
            Ok(Found::UpToDate | Found::NotYet(_)) => {
                out.line(plain(&format!(
                    "Argonauts Code {} is up to date ({}).",
                    here.current,
                    channel.as_str()
                )));
                return Ok(());
            }
            Err(err) => return Err(format!("the {} manifest: {err}", channel.as_str())),
        },
    };
    if here.layout.linked().as_ref() == Some(&release.version) {
        out.line(plain(&format!(
            "Argonauts Code {} is in place already, and takes over at the next start.",
            release.version
        )));
        return Ok(());
    }
    let named = version.is_none().then_some(channel);
    out.line(header(&here.current, &release.version, named, &out.theme));
    let theme = out.theme.clone();
    // Ctrl+C ends the process, and the next update sweeps what it staged.
    let never = tokio_util::sync::CancellationToken::new();
    apply::take(
        here,
        &release,
        &mut |step| {
            out.line(step_line(&step, &theme));
        },
        &never,
    )
    .await
    .map_err(|err| err.to_string())?;
    out.line(updated_line(&here.current, &out.theme));
    Ok(())
}

fn roll_back(here: &Here, out: &mut Printer) -> Result<(), String> {
    let switched = apply::roll_back(&here.layout).map_err(|err| err.to_string())?;
    let from = switched
        .kept
        .clone()
        .unwrap_or_else(|| here.current.clone());
    out.line(header(&from, &switched.to, None, &out.theme));
    out.line(step_line(&Step::Switched(switched.clone()), &out.theme));
    out.line(plain(&format!(
        "Went back. Open sessions keep {} until they end.",
        here.current
    )));
    Ok(())
}

/// `Argonauts Code 0.1.0 →  0.1.1 (stable)`, bold. The arrow is two cells wide in some fonts,
/// so two spaces keep it off the version after it.
pub(crate) fn header(
    from: &Version,
    to: &Version,
    channel: Option<UpdateChannel>,
    theme: &Theme,
) -> Line<'static> {
    let mut text = format!("Argonauts Code {from} →  {to}");
    if let Some(channel) = channel {
        text.push_str(&format!(" ({})", channel.as_str()));
    }
    Line::from(Span::styled(
        text,
        theme.style(Role::Text).add_modifier(Modifier::BOLD),
    ))
}

/// One step as it's done: its name in the success colour, then what it found.
pub(crate) fn step_line(step: &Step, theme: &Theme) -> Line<'static> {
    let (name, words) = match step {
        Step::Downloaded { file, size } => (
            "Downloaded",
            format!("{file} · {} · sha256 and signature match", megabytes(*size)),
        ),
        Step::Checked { said } => ("Checked", format!("the new binary runs: {said}")),
        Step::Switched(Switched { link, to, kept }) => {
            let mut words = format!("{link} →  versions/{to}");
            if let Some(kept) = kept {
                words.push_str(&format!(" · {kept} is kept for --rollback"));
            }
            ("Switched", words)
        }
    };
    Line::from(vec![
        Span::styled(format!("  {name:<STEP_WIDTH$}"), theme.style(Role::Success)),
        Span::styled(words, theme.style(Role::Text)),
    ])
}

/// `Updated. Open sessions keep 0.1.0 until they end. What's new: /release-notes`.
pub(crate) fn updated_line(running: &Version, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("Updated. Open sessions keep {running} until they end. What's new: "),
            theme.style(Role::Text),
        ),
        Span::styled("/release-notes", theme.style(Role::Code)),
    ])
}

/// A download's size in megabytes, as `23.4 MB`.
fn megabytes(bytes: u64) -> String {
    // A tenth of a megabyte is as near as the line needs.
    #[allow(clippy::cast_precision_loss)]
    let megabytes = bytes as f64 / 1_000_000.0;
    format!("{megabytes:.1} MB")
}

fn plain(text: &str) -> Line<'static> {
    Line::from(text.to_owned())
}

/// Prints lines to stdout as they come, in colour on a terminal and as plain text otherwise.
struct Printer {
    theme: Theme,
    colour: bool,
}

impl Printer {
    fn stdout() -> Self {
        let colour = std::io::stdout().is_terminal();
        let theme = if colour {
            Theme::detect(&ColorEnv::from_process())
        } else {
            Theme::dark()
        };
        Self { theme, colour }
    }

    fn line(&mut self, line: Line<'_>) {
        let mut stdout = std::io::stdout().lock();
        let written = if self.colour {
            argonauts_tui::terminal::write_line(&mut stdout, &line, u16::MAX)
        } else {
            let text: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            write!(
                stdout,
                "{}",
                argonauts_core::safety::sanitize::sanitize_for_terminal(&text)
            )
        };
        let _ = written.and_then(|()| writeln!(stdout));
    }

    fn lines(&mut self, lines: Vec<Line<'_>>) {
        for line in lines {
            self.line(line);
        }
    }
}

/// The last line of `argonauts update` when Argonauts Code itself is behind, such as `Argonauts
/// Code 0.1.1 is out · argonauts self-update`. Nothing when it's current, when the check is off
/// here, or when the manifest can't be read: the agent CLIs' update doesn't fail over it.
pub async fn behind_line(overrides: Vec<String>) -> Option<String> {
    let source = Source::from_env().ok()?;
    let paths = Paths::from_env().ok()?;
    let settings = crate::commands::current_settings(None, overrides).unwrap_or_default();
    let here = Here::this_process(&paths, source);
    let found = manifest::check(&here, settings.updates.channel, true, clock::now_utc())
        .await
        .ok()?;
    let Found::Newer(release) = found else {
        return None;
    };
    Some(format!(
        "Argonauts Code {} is out · {}",
        release.version,
        what_updates(here.install)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::golden::{assert_looks, drawn_rows, picture_rows, theme};

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    /// The picture's rows from a shell install, from its example numbers, then from a
    /// Homebrew install.
    #[test]
    fn self_update_is_its_picture() {
        let theme = theme();
        let running = v("0.1.0");
        let to = v("0.1.1");
        let mut lines = vec![header(&running, &to, Some(UpdateChannel::Stable), &theme)];
        for step in [
            Step::Downloaded {
                file: "argonauts-aarch64-apple-darwin.tar.xz".to_owned(),
                size: 23_400_000,
            },
            Step::Checked {
                said: "argonauts-code 0.1.1".to_owned(),
            },
            Step::Switched(Switched {
                link: "~/.local/bin/argonauts".to_owned(),
                to: to.clone(),
                kept: Some(running.clone()),
            }),
        ] {
            lines.push(step_line(&step, &theme));
        }
        lines.push(updated_line(&running, &theme));
        lines.push(Line::default());
        lines.extend(managed_lines(
            "Homebrew",
            Install::Homebrew.command().unwrap(),
            &theme,
        ));
        crate::testing::golden::dump("self_update", &lines);
        assert_eq!(
            drawn_rows(&lines),
            picture_rows(include_str!("../../tests/golden/self_update.txt"))
        );
        assert_looks(
            &theme,
            &lines,
            include_str!("../../tests/golden/self_update.html"),
        );
    }

    #[test]
    /// Each way of installing gets its own line from `self-update`, and its own words after
    /// `0.1.1 is out · `.
    fn each_install_method_says_who_updates_it() {
        let theme = theme();
        let said = |install| not_ours(install, &theme).map(|lines| drawn_rows(&lines));
        assert_eq!(said(Install::Shell), None);
        for (install, manager, command) in [
            (Install::Homebrew, "Homebrew", "brew upgrade argonauts-code"),
            (Install::Npm, "npm", "npm install -g argonauts-code@latest"),
            (Install::Pnpm, "pnpm", "pnpm add -g argonauts-code@latest"),
            (Install::Bun, "Bun", "bun add -g argonauts-code@latest"),
            (
                Install::Cargo,
                "cargo",
                "cargo install argonauts-code --locked",
            ),
        ] {
            assert_eq!(
                said(install).unwrap(),
                [
                    format!("{manager} installed Argonauts Code, so {manager} updates it:"),
                    format!("  {command}"),
                ]
            );
            assert_eq!(what_updates(install), command);
        }
        assert_eq!(
            said(Install::Npx).unwrap(),
            ["npx runs the newest Argonauts Code each time, so there's nothing to update."]
        );
        let elsewhere = said(Install::Elsewhere).unwrap();
        assert_eq!(
            elsewhere[1],
            format!("Every release, with its installer: {RELEASES}")
        );
        assert_eq!(what_updates(Install::Shell), "argonauts self-update");
        assert_eq!(what_updates(Install::Elsewhere), RELEASES);
    }

    #[test]
    fn going_back_names_the_version_it_leaves_for_the_next_rollback() {
        let theme = theme();
        let line = step_line(
            &Step::Switched(Switched {
                link: "~/.local/bin/argonauts".to_owned(),
                to: v("0.1.0"),
                kept: Some(v("0.1.1")),
            }),
            &theme,
        );
        assert_eq!(
            drawn_rows(&[line]),
            [
                "  Switched    ~/.local/bin/argonauts →  versions/0.1.0 · 0.1.1 is kept for --rollback"
            ]
        );
        assert_eq!(megabytes(23_400_000), "23.4 MB");
    }
}
