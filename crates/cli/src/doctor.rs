//! `argonauts doctor`: the conductor's report on your agent CLIs and git, and a reminder for each
//! run parked a long time.

use std::process::ExitCode;

use argonauts_app::{DoctorExtras, ParkedReminder};
use argonauts_conductor::{AgentStatus, DoctorReport, doctor};
use argonauts_core::sanitize::sanitize_for_terminal;
use argonauts_core::settings::Settings;

use crate::args::DoctorArgs;
use crate::text::{bytes, count};

/// Fails when no agent CLI or no git is usable. Parked runs never fail it.
pub async fn run(args: &DoctorArgs, set: &[String]) -> anyhow::Result<ExitCode> {
    let mut problems = Vec::new();
    // Broken settings shouldn't stop the doctor, which is how you find out what's wrong.
    let settings = argonauts_app::current_settings(None, set.to_vec()).unwrap_or_else(|err| {
        problems.push(format!("! your settings don't resolve: {err:#}"));
        Settings::default()
    });
    // The built-in four and every harness profile your settings turn on, such as Hermes.
    let adapters = argonauts_app::doctor_adapters(None, set.to_vec()).unwrap_or_else(|err| {
        problems.push(format!("! couldn't read your CLI profiles: {err:#}"));
        argonauts_app::builtin_adapters()
    });
    let report = doctor(&adapters, &settings).await;
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
        print!("{}", render(&report, &extras, &problems));
    }
    Ok(if report.ready() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The report as it was, plus a `parked` list.
fn json(report: &DoctorReport, extras: &DoctorExtras) -> anyhow::Result<serde_json::Value> {
    let mut value = serde_json::to_value(report)?;
    let parked: Vec<serde_json::Value> = extras
        .parked
        .iter()
        .map(|parked| {
            serde_json::json!({
                "run": parked.run.to_string(),
                "days": parked.days,
                "stopped_at": parked.stopped_at,
                "worktrees": parked.worktrees,
                "worktree_bytes": parked.worktree_bytes,
            })
        })
        .collect();
    if let Some(object) = value.as_object_mut() {
        object.insert("parked".to_owned(), serde_json::Value::Array(parked));
    }
    Ok(value)
}

pub fn render(report: &DoctorReport, extras: &DoctorExtras, problems: &[String]) -> String {
    let mut out = format!("{report}\n{}\n", verdict(report));
    for line in problems {
        out.push_str(line);
        out.push('\n');
    }
    for parked in &extras.parked {
        out.push_str(&parked_line(parked));
        out.push('\n');
    }
    sanitize_for_terminal(&out)
}

/// `! run-03 has been parked for 9 days, at the plan checkpoint. Parked runs never expire.`
/// with its worktrees and their size.
fn parked_line(parked: &ParkedReminder) -> String {
    let mut line = format!(
        "! {} has been waiting for {}",
        parked.run,
        count(parked.days, "day", "days")
    );
    if !parked.stopped_at.is_empty() {
        line.push_str(&format!(", at {}", parked.stopped_at));
    }
    line.push_str(". Waiting runs never expire.");
    if parked.worktrees > 0 {
        let worktrees = u64::try_from(parked.worktrees).unwrap_or(u64::MAX);
        let verb = if worktrees == 1 { "holds" } else { "hold" };
        line.push_str(&format!(
            " Its {} {verb} {}.",
            count(worktrees, "worktree", "worktrees"),
            bytes(parked.worktree_bytes)
        ));
    }
    line
}

fn verdict(report: &DoctorReport) -> String {
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
    if any_untested {
        verdict
            .push_str(" Versions Argonauts wasn't checked with may still work; unknown events are kept and shown raw.");
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
    use argonauts_conductor::{AgentCheck, GitCheck};
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

    fn parked(days: u64, worktrees: usize) -> ParkedReminder {
        ParkedReminder {
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
            &DoctorExtras::default(),
            &[],
        );
        assert!(
            text.contains("Ready with 1 of 4 agent CLIs. 2 are off.\n"),
            "{text}"
        );
        assert!(
            text.contains("claude-code  1.0.0       checked             off"),
            "{text}"
        );

        let only_off = report(vec![off], Some("2.54.0"));
        assert!(!only_off.ready());
        let text = render(&only_off, &DoctorExtras::default(), &[]);
        assert!(text.contains("Not ready"), "{text}");
    }

    #[test]
    fn untested_versions_are_called_out() {
        let text = render(
            &report(
                vec![check("a", AgentStatus::Untested, Some("2.0.0"))],
                Some("2.54.0"),
            ),
            &DoctorExtras::default(),
            &[],
        );
        assert!(
            text.contains("Versions Argonauts wasn't checked with may still work"),
            "{text}"
        );
    }

    #[test]
    fn nothing_installed_is_not_ready() {
        let empty = DoctorExtras::default();
        let none = report(
            vec![check("a", AgentStatus::NotInstalled, None)],
            Some("2.54.0"),
        );
        assert!(render(&none, &empty, &[]).contains("Not ready"));
        let no_git = report(vec![check("a", AgentStatus::Tested, Some("1.0.0"))], None);
        assert!(render(&no_git, &empty, &[]).contains("Not ready: install git"));
    }

    #[test]
    fn doctor_names_each_run_parked_too_long() {
        assert_eq!(
            parked_line(&parked(9, 2)),
            "! run-03 has been waiting for 9 days, at the plan approval. Waiting runs never \
             expire. Its 2 worktrees hold 14.2 MB."
        );
        assert_eq!(
            parked_line(&parked(1, 0)),
            "! run-03 has been waiting for 1 day, at the plan approval. Waiting runs never expire."
        );
        let extras = DoctorExtras {
            parked: vec![parked(9, 1)],
        };
        let text = render(
            &report(
                vec![check("a", AgentStatus::Tested, Some("1.0.0"))],
                Some("2.54.0"),
            ),
            &extras,
            &[],
        );
        assert!(text.ends_with("Its 1 worktree holds 14.2 MB.\n"), "{text}");
        let json = json(&report(Vec::new(), None), &extras).unwrap();
        assert_eq!(json["parked"][0]["run"], "run-03");
        assert_eq!(json["parked"][0]["days"], 9);
    }
}
