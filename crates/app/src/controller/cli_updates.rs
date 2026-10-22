//! When you are reminded to update a CLI. Doctor and the Status tab always say how each
//! version stands, quietly, for whoever opens them. Beyond that, start-up says one dim line
//! the first time a newer version is known and warns once when a version stopped reporting
//! something Argonauts Code reads, then says nothing until the next version. Nothing here
//! speaks during a run or on every turn, and nothing here updates a CLI.

use argonauts_conductor::cli_versions::{
    CliVersions, Installed, out_line, startup_warnings, version_check,
};
use argonauts_conductor::doctor::newest::{NEWEST_TIMEOUT, install_source, newest_version};
use std::path::Path;
use std::sync::Arc;

use argonauts_conductor::{AgentCheck, AgentStatus, DoctorReport};
use argonauts_core::Adapter;
use argonauts_core::log::NoteLevel;
use time::OffsetDateTime;

use super::Ctx;

/// What start-up says of the CLIs' versions, each thing once. Nothing when
/// `ui.newer_cli_notice` is off.
pub(crate) fn start_lines(ctx: &Ctx, report: &DoctorReport) -> Vec<(NoteLevel, String)> {
    if !ctx.settings().ui.newer_cli_notice {
        return Vec::new();
    }
    let Ok(adapters) = crate::commands::adapters_here(Some(&ctx.env), ctx.settings.resolved())
    else {
        return Vec::new();
    };
    lines_once(&adapters, &ctx.env.paths.cli_versions(), report)
}

/// A newer version that is out, and anything an installed version stopped reporting, each
/// said the first time only, as `seen_at` remembers. Nothing when what was said couldn't be
/// remembered, since it would be said again at every start.
fn lines_once(
    adapters: &[Arc<dyn Adapter>],
    seen_at: &Path,
    report: &DoctorReport,
) -> Vec<(NoteLevel, String)> {
    let mut seen = CliVersions::load(seen_at);
    let mut lines = Vec::new();
    for check in report.agents.iter().filter(|check| in_use(check)) {
        let Some(adapter) = adapters.iter().find(|adapter| adapter.id() == check.id) else {
            continue;
        };
        let found = version_check(adapter.as_ref(), Installed::of(check), &seen);
        // A version that needs an update is asked about before a run, not named here.
        if !found.standing.needs_update()
            && let Some(line) = out_line(&check.display_name, &found)
            && let Some(newest) = &found.newest
            && seen.first_reminder(&check.id, newest)
        {
            lines.push((NoteLevel::Quiet, line));
        }
        let Some(version) = &check.version else {
            continue;
        };
        let missing = seen.startup_missing(&check.id, version).unwrap_or_default();
        for (field, warning) in startup_warnings(adapter.as_ref(), version, &missing) {
            if seen.first_warning(&check.id, version, field) {
                lines.push((NoteLevel::Warn, warning));
            }
        }
    }
    if lines.is_empty() || seen.save(seen_at).is_err() {
        return Vec::new();
    }
    lines
}

/// Asks each installed CLI's install source for its newest version, in the background, when
/// the last answer is a day old. What comes back is kept for Doctor, the Status tab and the
/// next start; nothing is said now.
pub(crate) fn ask_newest(ctx: &Ctx, report: &DoctorReport) {
    let Ok(adapters) = crate::commands::adapters_here(Some(&ctx.env), ctx.settings.resolved())
    else {
        return;
    };
    let path = ctx.env.paths.cli_versions();
    let seen = CliVersions::load(&path);
    let now = OffsetDateTime::now_utc();
    let due: Vec<_> = report
        .agents
        .iter()
        .filter(|check| in_use(check) && seen.newest_due(&check.id, now))
        .filter_map(|check| {
            let adapter = adapters.iter().find(|adapter| adapter.id() == check.id)?;
            let source = install_source(check.path.as_deref()?, &adapter.install_sources())?;
            Some((check.id.clone(), source))
        })
        .collect();
    if due.is_empty() {
        return;
    }
    tokio::spawn(async move {
        let asked = due
            .into_iter()
            .map(|(id, source)| async move { (id, newest_version(&source, NEWEST_TIMEOUT).await) });
        let answers = futures::future::join_all(asked).await;
        // Read again: a session may have written to the file while the sources answered.
        let mut seen = CliVersions::load(&path);
        for (id, version) in answers {
            seen.asked_newest(&id, version, now);
        }
        if let Err(err) = seen.save(&path) {
            tracing::debug!("couldn't keep the CLIs' newest versions: {err}");
        }
    });
}

/// Whether a run is under way now, in this folder or any other. A CLI is only updated between
/// runs, so that none switches versions halfway.
pub(crate) fn a_run_is_going(ctx: &Ctx) -> bool {
    !crate::live_registry::live_run_hosts(&ctx.env.paths).is_empty()
}

/// What `/update` says in place of updating while a run is going.
pub(crate) const UPDATE_WAITS: &str = "A run is going, and CLIs are updated between runs so that \
                                       none switches versions halfway. /update once it has ended.";

/// Installed and turned on.
fn in_use(check: &AgentCheck) -> bool {
    check.enabled && check.status != AgentStatus::NotInstalled
}

#[cfg(test)]
mod tests {
    use argonauts_adapter_claude_code::ClaudeCodeAdapter;
    use argonauts_conductor::GitCheck;

    use super::*;

    /// Doctor's report of Claude Code alone, at `version`.
    fn report(version: &str) -> DoctorReport {
        DoctorReport {
            agents: vec![AgentCheck {
                id: "claude-code".to_owned(),
                display_name: "Claude Code".to_owned(),
                binary: "claude".to_owned(),
                path: None,
                version: Some(version.to_owned()),
                status: AgentStatus::Tested,
                tested_versions: Vec::new(),
                cheapest_model: String::new(),
                cheapest_effort: None,
                problem: None,
                enabled: true,
            }],
            git: GitCheck {
                version: None,
                problem: None,
            },
        }
    }

    fn claude() -> Vec<Arc<dyn Adapter>> {
        vec![Arc::new(ClaudeCodeAdapter)]
    }

    fn texts(lines: &[(NoteLevel, String)]) -> Vec<&str> {
        lines.iter().map(|(_, text)| text.as_str()).collect()
    }

    fn newest_is(path: &Path, version: &str) {
        let mut seen = CliVersions::load(path);
        seen.asked_newest(
            "claude-code",
            Some(version.to_owned()),
            OffsetDateTime::now_utc(),
        );
        seen.save(path).unwrap();
    }

    #[test]
    fn a_newer_version_is_named_once_at_start_up_then_never_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cli-versions.json");
        let installed = report("2.1.296");
        // Nothing is known of a newer version yet, so nothing is said.
        assert!(lines_once(&claude(), &path, &installed).is_empty());

        newest_is(&path, "2.1.297");
        let said = lines_once(&claude(), &path, &installed);
        // The line as the versions picture has it, under its two cells of margin.
        let picture = include_str!("../../../tui/tests/golden/overview/versions.txt");
        let drawn = picture.lines().nth(9).unwrap();
        assert_eq!(format!("  {}", said[0].1), drawn);
        assert_eq!(said[0].0, NoteLevel::Quiet);
        // The same version at the next start, and the one after: nothing.
        assert!(lines_once(&claude(), &path, &installed).is_empty());
        assert!(lines_once(&claude(), &path, &installed).is_empty());

        // The next version is named once in its turn.
        newest_is(&path, "2.1.298");
        assert_eq!(
            texts(&lines_once(&claude(), &path, &installed)),
            ["Claude Code 2.1.298 is out · claude update · shown once for this version"]
        );
        assert!(lines_once(&claude(), &path, &installed).is_empty());
        // Once that one is installed, there is nothing newer to name.
        assert!(lines_once(&claude(), &path, &report("2.1.298")).is_empty());
    }

    #[test]
    fn what_a_version_stopped_reporting_is_warned_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cli-versions.json");
        let mut seen = CliVersions::load(&path);
        seen.saw_startup("claude-code", "2.1.297", &["/model"]);
        seen.save(&path).unwrap();
        let installed = report("2.1.297");
        let said = lines_once(&claude(), &path, &installed);
        assert_eq!(
            texts(&said),
            [
                "Claude Code 2.1.297 no longer reports its model, so /context can't size its \
              window until Argonauts Code is updated"
            ]
        );
        assert_eq!(said[0].0, NoteLevel::Warn);
        assert!(lines_once(&claude(), &path, &installed).is_empty());
    }

    #[test]
    fn a_cli_that_is_off_or_missing_is_never_named() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cli-versions.json");
        newest_is(&path, "2.1.297");
        let mut off = report("2.1.296");
        off.agents[0].enabled = false;
        assert!(lines_once(&claude(), &path, &off).is_empty());
        let mut missing = report("2.1.296");
        missing.agents[0].status = AgentStatus::NotInstalled;
        assert!(lines_once(&claude(), &path, &missing).is_empty());
        // Neither marked it as said: turned on, it is still named once.
        assert_eq!(lines_once(&claude(), &path, &report("2.1.296")).len(), 1);
    }
}
