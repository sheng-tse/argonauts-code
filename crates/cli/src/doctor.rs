//! `argonauts doctor`: Argonauts Code's report on your agent CLIs and git, and a reminder for each
//! run waiting a long time.

use std::process::ExitCode;

use std::collections::BTreeMap;

use argonauts_app::{DoctorExtras, WaitingReminder};
use argonauts_conductor::{AgentCheck, AgentStatus, DoctorReport, doctor};
use argonauts_core::sanitize::sanitize_for_terminal;
use argonauts_core::settings::Settings;
use argonauts_core::status_tab::{
    CliVersionCheck, VERSION_ROW_NAMES, VERSION_ROW_STATES, VersionStanding, version_row,
};

use crate::args::DoctorArgs;
use crate::text::{bytes, count};

/// Fails when no agent CLI or no git is usable. Waiting runs never fail it.
pub async fn run(args: &DoctorArgs, set: &[String]) -> anyhow::Result<ExitCode> {
    let mut problems = Vec::new();
    // Broken settings shouldn't stop the doctor, which is how you find out what's wrong.
    let settings = argonauts_app::current_settings(None, set.to_vec()).unwrap_or_else(|err| {
        problems.push(format!("! your settings don't resolve: {err:#}"));
        Settings::default()
    });
    // Warnings can quote key names from a repo's file.
    problems.extend(
        argonauts_app::settings_warnings(None, set.to_vec())
            .iter()
            .map(|warning| format!("! {}", sanitize_for_terminal(warning))),
    );
    // The built-in four and every CLI profile your settings turn on, such as Hermes.
    let adapters = argonauts_app::doctor_adapters(None, set.to_vec()).unwrap_or_else(|err| {
        problems.push(format!("! couldn't read your CLI profiles: {err:#}"));
        argonauts_app::builtin_adapters()
    });
    let report = doctor(&adapters, &settings).await;
    let checks = argonauts_app::doctor_version_checks(&adapters, &report);
    let extras = argonauts_app::doctor_extras(None)
        .await
        .unwrap_or_else(|err| {
            problems.push(format!("! couldn't check for waiting runs: {err:#}"));
            DoctorExtras::default()
        });
    problems.extend(argonauts_app::mod_problems(None));
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json(&report, &extras)?)?
        );
    } else {
        print!("{}", render(&report, &checks, &extras, &problems));
    }
    Ok(if report.ready() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The report as it was, plus a `waiting` list.
fn json(report: &DoctorReport, extras: &DoctorExtras) -> anyhow::Result<serde_json::Value> {
    let mut value = serde_json::to_value(report)?;
    let waiting: Vec<serde_json::Value> = extras
        .waiting
        .iter()
        .map(|waiting| {
            serde_json::json!({
                "run": waiting.run.to_string(),
                "days": waiting.days,
                "stopped_at": waiting.stopped_at,
                "worktrees": waiting.worktrees,
                "worktree_bytes": waiting.worktree_bytes,
            })
        })
        .collect();
    if let Some(object) = value.as_object_mut() {
        object.insert("waiting".to_owned(), serde_json::Value::Array(waiting));
    }
    Ok(value)
}

/// How each CLI's version stands, by its id.
pub type VersionChecks = BTreeMap<String, CliVersionCheck>;

/// One row a CLI, as the app's Doctor lists them: its version, how that stands and what
/// there is to update to, then git, the verdict and what needs a look.
pub fn render(
    report: &DoctorReport,
    checks: &VersionChecks,
    extras: &DoctorExtras,
    problems: &[String],
) -> String {
    let mut out = String::new();
    for agent in &report.agents {
        out.push_str(&agent_row(agent, checks.get(&agent.id)));
        out.push('\n');
    }
    out.push_str(&match (&report.git.version, &report.git.problem) {
        (Some(version), _) => format!("git {version}\n"),
        (None, Some(problem)) => format!("git · {problem}\n"),
        (None, None) => "git · not found\n".to_owned(),
    });
    out.push_str(&format!("\n{}\n", verdict(report, checks)));
    for line in problems {
        out.push_str(line);
        out.push('\n');
    }
    for waiting in &extras.waiting {
        out.push_str(&waiting_line(waiting));
        out.push('\n');
    }
    sanitize_for_terminal(&out)
}

/// `! run-03 has been waiting for 9 days, at the plan approval. Waiting runs never expire.`
/// with its worktrees and their size.
fn waiting_line(waiting: &WaitingReminder) -> String {
    let mut line = format!(
        "! {} has been waiting for {}",
        waiting.run,
        count(waiting.days, "day", "days")
    );
    if !waiting.stopped_at.is_empty() {
        line.push_str(&format!(", at {}", waiting.stopped_at));
    }
    line.push_str(". Waiting runs never expire.");
    if waiting.worktrees > 0 {
        let worktrees = u64::try_from(waiting.worktrees).unwrap_or(u64::MAX);
        let verb = if worktrees == 1 { "holds" } else { "hold" };
        line.push_str(&format!(
            " Its {} {verb} {}.",
            count(worktrees, "worktree", "worktrees"),
            bytes(waiting.worktree_bytes)
        ));
    }
    line
}

/// `Codex 0.158.0             older than supported          0.160.0 needed · brew upgrade
/// codex`, with whether it is off and what is wrong with it after.
fn agent_row(agent: &AgentCheck, check: Option<&CliVersionCheck>) -> String {
    if agent.status == AgentStatus::NotInstalled {
        return match &agent.problem {
            Some(problem) => format!("{} · not installed · {problem}", agent.display_name),
            None => format!("{} · not installed", agent.display_name),
        };
    }
    let none = CliVersionCheck::default();
    let check = check.unwrap_or(&none);
    let version = agent.version.as_deref().unwrap_or_default();
    let row = version_row(&agent.display_name, version, check);
    let mut after = Vec::new();
    if !agent.enabled {
        after.push("off".to_owned());
    }
    after.extend(agent.problem.clone());
    if after.is_empty() {
        return row;
    }
    let after = after.join(" · ");
    if check.note().is_some() {
        format!("{row} · {after}")
    } else {
        let width = VERSION_ROW_NAMES + VERSION_ROW_STATES;
        format!("{row:<width$}{after}")
    }
}

fn verdict(report: &DoctorReport, checks: &VersionChecks) -> String {
    if !report.ready() {
        return format!(
            "Not ready: install git and at least one of {}.",
            quoted_binaries(report)
        );
    }
    let usable = report.agents.iter().filter(|agent| agent.usable()).count();
    let total = report.agents.len();
    let off = report.agents.iter().filter(|agent| !agent.enabled).count();
    let mut verdict = format!("Ready with {usable} of {total} agent CLIs.");
    if off > 0 {
        verdict.push_str(&format!(" {} off.", count(off as u64, "is", "are")));
    }
    let any_untested = report.agents.iter().any(|agent| {
        agent.usable()
            && matches!(
                agent.status,
                AgentStatus::Untested | AgentStatus::UnknownVersion
            )
    });
    // A version that must be updated, or a new major one, is said on its own row, and
    // nothing here plays it down.
    let any_needs_a_look = checks.values().any(|check| {
        matches!(
            check.standing,
            VersionStanding::Older | VersionStanding::Broken | VersionStanding::NewMajor
        )
    });
    if any_untested && !any_needs_a_look {
        verdict
            .push_str(" Versions Argonauts Code wasn't checked with may still work; unknown events are kept and shown raw.");
    }
    verdict
}

fn quoted_binaries(report: &DoctorReport) -> String {
    report
        .agents
        .iter()
        .map(|agent| format!("`{}`", agent.binary))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use argonauts_conductor::GitCheck;
    use argonauts_core::RunId;

    use super::*;

    fn check(id: &str, status: AgentStatus, version: Option<&str>) -> AgentCheck {
        AgentCheck {
            id: id.to_owned(),
            display_name: id.to_owned(),
            binary: id.to_owned(),
            path: None,
            version: version.map(str::to_owned),
            status,
            tested_versions: vec!["1.0.0".to_owned()],
            cheapest_model: "small".to_owned(),
            cheapest_effort: Some("low".to_owned()),
            problem: None,
            enabled: true,
        }
    }

    fn report(agents: Vec<AgentCheck>, git: Option<&str>) -> DoctorReport {
        DoctorReport {
            agents,
            git: GitCheck {
                version: git.map(str::to_owned),
                problem: None,
            },
        }
    }

    fn waiting(days: u64, worktrees: usize) -> WaitingReminder {
        WaitingReminder {
            run: RunId::new("run-03").unwrap(),
            days,
            stopped_at: "the plan approval".to_owned(),
            worktrees,
            worktree_bytes: 14 * 1024 * 1024 + 200 * 1024,
        }
    }

    #[test]
    fn all_tested_is_ready() {
        let text = render(
            &report(
                vec![
                    check("a", AgentStatus::Tested, Some("1.0.0")),
                    check("b", AgentStatus::NotInstalled, None),
                ],
                Some("2.54.0"),
            ),
            &VersionChecks::new(),
            &DoctorExtras::default(),
            &[],
        );
        assert!(text.contains("Ready with 1 of 2 agent CLIs.\n"), "{text}");
    }

    #[test]
    fn agents_that_are_off_never_count_as_ready() {
        let mut off = check("claude-code", AgentStatus::Tested, Some("1.0.0"));
        off.enabled = false;
        let mut also_off = check("codex", AgentStatus::Untested, Some("9.0.0"));
        also_off.enabled = false;
        let text = render(
            &report(
                vec![
                    off.clone(),
                    also_off,
                    check("opencode", AgentStatus::Tested, Some("1.0.0")),
                    check("hermes", AgentStatus::NotInstalled, None),
                ],
                Some("2.54.0"),
            ),
            &VersionChecks::new(),
            &DoctorExtras::default(),
            &[],
        );
        assert!(
            text.contains("Ready with 1 of 4 agent CLIs. 2 are off.\n"),
            "{text}"
        );
        assert!(text.contains("claude-code 1.0.0"), "{text}");
        let row = text.lines().next().unwrap_or_default();
        assert!(row.ends_with("off"), "{row}");

        let only_off = report(vec![off], Some("2.54.0"));
        assert!(!only_off.ready());
        let text = render(
            &only_off,
            &VersionChecks::new(),
            &DoctorExtras::default(),
            &[],
        );
        assert!(text.contains("Not ready"), "{text}");
    }

    #[test]
    fn untested_versions_are_called_out() {
        let text = render(
            &report(
                vec![check("a", AgentStatus::Untested, Some("2.0.0"))],
                Some("2.54.0"),
            ),
            &VersionChecks::new(),
            &DoctorExtras::default(),
            &[],
        );
        assert!(
            text.contains("Versions Argonauts Code wasn't checked with may still work"),
            "{text}"
        );
    }

    /// The four rows Doctor lists the CLIs in, drawn from how each version stands.
    #[test]
    fn each_cli_has_the_row_the_apps_doctor_gives_it() {
        let named = |id: &str, name: &str, status: AgentStatus, version: &str| AgentCheck {
            display_name: name.to_owned(),
            ..check(id, status, Some(version))
        };
        let agents = vec![
            named("claude-code", "Claude Code", AgentStatus::Tested, "2.1.296"),
            named("codex", "Codex", AgentStatus::Untested, "0.158.0"),
            named(
                "antigravity-cli",
                "Antigravity CLI",
                AgentStatus::Untested,
                "1.3.3",
            ),
            named("opencode", "OpenCode", AgentStatus::Tested, "1.18.34"),
        ];
        let stands = |standing, works| CliVersionCheck {
            standing,
            works,
            ..CliVersionCheck::default()
        };
        let checks = VersionChecks::from([
            (
                "claude-code".to_owned(),
                CliVersionCheck {
                    newest: Some("2.1.297".to_owned()),
                    newer_is_out: true,
                    update_command: Some("claude update".to_owned()),
                    ..stands(VersionStanding::Checked, true)
                },
            ),
            (
                "codex".to_owned(),
                CliVersionCheck {
                    oldest_supported: Some("0.160.0".to_owned()),
                    update_command: Some("brew upgrade codex".to_owned()),
                    ..stands(VersionStanding::Older, false)
                },
            ),
            (
                "antigravity-cli".to_owned(),
                CliVersionCheck {
                    checked_with: Some("1.3.1".to_owned()),
                    ..stands(VersionStanding::Newer, true)
                },
            ),
            (
                "opencode".to_owned(),
                CliVersionCheck {
                    newest: Some("1.18.34".to_owned()),
                    ..stands(VersionStanding::Checked, true)
                },
            ),
        ]);
        let text = render(
            &report(agents, Some("2.54.0")),
            &checks,
            &DoctorExtras::default(),
            &[],
        );
        let rows: Vec<&str> = text.lines().take(4).collect();
        assert_eq!(
            rows,
            [
                "Claude Code 2.1.296       checked · works               2.1.297 is out · claude update",
                "Codex 0.158.0             older than supported          0.160.0 needed · brew upgrade codex",
                "Antigravity CLI 1.3.3     newer than checked · works    its flags and start-up message match 1.3.1",
                "OpenCode 1.18.34          checked · works               up to date",
            ]
        );
        // A version that must be updated is said on its row, and nothing plays it down.
        assert!(!text.contains("may still work"), "{text}");
        assert!(text.contains("git 2.54.0\n"), "{text}");
    }

    #[test]
    fn nothing_installed_is_not_ready() {
        let empty = DoctorExtras::default();
        let none = report(
            vec![check("a", AgentStatus::NotInstalled, None)],
            Some("2.54.0"),
        );
        assert!(render(&none, &VersionChecks::new(), &empty, &[]).contains("Not ready"));
        let no_git = report(vec![check("a", AgentStatus::Tested, Some("1.0.0"))], None);
        assert!(
            render(&no_git, &VersionChecks::new(), &empty, &[]).contains("Not ready: install git")
        );
    }

    #[test]
    fn doctor_names_each_run_waiting_too_long() {
        assert_eq!(
            waiting_line(&waiting(9, 2)),
            "! run-03 has been waiting for 9 days, at the plan approval. Waiting runs never \
             expire. Its 2 worktrees hold 14.2 MB."
        );
        assert_eq!(
            waiting_line(&waiting(1, 0)),
            "! run-03 has been waiting for 1 day, at the plan approval. Waiting runs never expire."
        );
        let extras = DoctorExtras {
            waiting: vec![waiting(9, 1)],
        };
        let text = render(
            &report(
                vec![check("a", AgentStatus::Tested, Some("1.0.0"))],
                Some("2.54.0"),
            ),
            &VersionChecks::new(),
            &extras,
            &[],
        );
        assert!(text.ends_with("Its 1 worktree holds 14.2 MB.\n"), "{text}");
        let json = json(&report(Vec::new(), None), &extras).unwrap();
        assert_eq!(json["waiting"][0]["run"], "run-03");
        assert_eq!(json["waiting"][0]["days"], 9);
    }
}
