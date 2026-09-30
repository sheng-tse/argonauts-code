use std::process::ExitCode;

use argonauts_conductor::{AgentStatus, DoctorReport, doctor};
use argonauts_core::settings::Settings;

use crate::agents;
use crate::args::DoctorArgs;

/// Fails when no agent CLI or no git is usable.
pub async fn run(args: DoctorArgs) -> anyhow::Result<ExitCode> {
    // The CLI still reads 0.0.1's config; doctor ignores its settings until it reports on them.
    let report = doctor(&agents::all(), &Settings::default()).await;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render(&report));
    }
    Ok(if report.ready() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

pub fn render(report: &DoctorReport) -> String {
    format!("{report}\n{}\n", verdict(report))
}

fn verdict(report: &DoctorReport) -> String {
    if !report.ready() {
        return format!(
            "Not ready: install git and at least one of {}.",
            quoted_binaries(report)
        );
    }
    let installed = report
        .agents
        .iter()
        .filter(|agent| agent.status != AgentStatus::NotInstalled)
        .count();
    let total = report.agents.len();
    let any_untested = report.agents.iter().any(|agent| {
        matches!(
            agent.status,
            AgentStatus::Untested | AgentStatus::UnknownVersion
        )
    });
    if any_untested {
        format!(
            "Ready with {installed} of {total} agent CLIs. Untested versions may still work; unknown events are kept and shown raw."
        )
    } else {
        format!("Ready with {installed} of {total} agent CLIs.")
    }
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

    #[test]
    fn all_tested_is_ready() {
        let text = render(&report(
            vec![
                check("a", AgentStatus::Tested, Some("1.0.0")),
                check("b", AgentStatus::NotInstalled, None),
            ],
            Some("2.54.0"),
        ));
        assert!(text.contains("Ready with 1 of 2 agent CLIs.\n"), "{text}");
        assert!(text.contains("cheapest: small (effort low)"), "{text}");
    }

    #[test]
    fn untested_versions_are_called_out() {
        let text = render(&report(
            vec![check("a", AgentStatus::Untested, Some("2.0.0"))],
            Some("2.54.0"),
        ));
        assert!(text.contains("Untested versions may still work"), "{text}");
    }

    #[test]
    fn nothing_installed_is_not_ready() {
        let text = render(&report(
            vec![check("a", AgentStatus::NotInstalled, None)],
            Some("2.54.0"),
        ));
        assert!(text.contains("Not ready"), "{text}");
        let text = render(&report(
            vec![check("a", AgentStatus::Tested, Some("1.0.0"))],
            None,
        ));
        assert!(text.contains("Not ready: install git"), "{text}");
    }
}
