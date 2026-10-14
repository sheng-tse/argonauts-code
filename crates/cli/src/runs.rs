//! `argonauts run` and `status`.

use std::process::ExitCode;

use anyhow::anyhow;
use argonauts_app::{RunCommand, RunCommandExit, StatusReport};
use argonauts_core::api::RunSummary;
use argonauts_core::events::run::{RunMode, RunOutcome};
use argonauts_core::sanitize::sanitize_for_terminal;

use crate::args::RunArgs;
use crate::text::{count, words};
use crate::usage::usage;

/// Waiting at an approval, so a script can tell waiting for you apart from failing.
pub const WAITING_EXIT: u8 = 3;

/// As for a process ended by SIGINT.
pub const STOPPED_EXIT: u8 = 130;

pub async fn run(args: &RunArgs, set: &[String], screen_reader: bool) -> anyhow::Result<ExitCode> {
    let command = RunCommand {
        repo: None,
        task: args.task_text(),
        mode: mode(args),
        overrides: overrides(args, set)?,
        plain: args.plain,
        detach: args.detach,
        screen_reader,
    };
    let exit = argonauts_app::run_command(command).await?;
    if let Some(line) = closing_line(&exit, args.detach) {
        println!("{line}");
    }
    Ok(exit_code(&exit))
}

fn mode(args: &RunArgs) -> Option<RunMode> {
    if args.captain {
        Some(RunMode::Captain)
    } else if args.crew {
        Some(RunMode::Crew)
    } else {
        None
    }
}

/// The flags as settings overrides. The model and effort flags belong to an agent, so they
/// need its id: `--agent`, or your main agent.
fn overrides(args: &RunArgs, set: &[String]) -> anyhow::Result<Vec<String>> {
    let agent = match &args.agent {
        Some(agent) => agent.clone(),
        None if args.needs_agent() => {
            argonauts_app::current_settings(None, set.to_vec())
                .map_err(usage)?
                .crew
                .main
                .agent
        }
        None => String::new(),
    };
    let cheapest = if args.cheapest {
        argonauts_app::builtin_adapters()
            .iter()
            .find(|adapter| adapter.id() == agent)
            .map(|adapter| adapter.cheapest_model())
            .ok_or_else(|| usage(anyhow!("there's no cheapest model known for {agent}")))?
    } else {
        ("", None)
    };
    args.overrides(set, &agent, cheapest).map_err(usage)
}

/// What the run's own lines didn't say already.
fn closing_line(exit: &RunCommandExit, detach: bool) -> Option<String> {
    match exit {
        RunCommandExit::Finished(_) => None,
        RunCommandExit::Waiting { run, approval } => Some(format!(
            "waiting for you: {} · argonauts resume {run}",
            sanitize_for_terminal(approval)
        )),
        // A script that detached wants the id alone.
        RunCommandExit::Detached(run) if detach => Some(run.to_string()),
        RunCommandExit::Detached(run) => Some(format!("{run} goes on · argonauts resume {run}")),
    }
}

pub fn exit_code(exit: &RunCommandExit) -> ExitCode {
    match exit {
        RunCommandExit::Finished(
            RunOutcome::Merged | RunOutcome::PullRequest | RunOutcome::BranchLeft,
        )
        | RunCommandExit::Detached(_) => ExitCode::SUCCESS,
        RunCommandExit::Finished(RunOutcome::Stopped) => ExitCode::from(STOPPED_EXIT),
        RunCommandExit::Finished(RunOutcome::Failed | RunOutcome::Abandoned) => ExitCode::FAILURE,
        RunCommandExit::Waiting { .. } => ExitCode::from(WAITING_EXIT),
    }
}

pub async fn status(json: bool) -> anyhow::Result<ExitCode> {
    let report = argonauts_app::status(None).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", status_text(&report));
    }
    Ok(ExitCode::SUCCESS)
}

fn status_text(report: &StatusReport) -> String {
    if report.live.is_empty() && report.runs.is_empty() {
        return "Nothing is running.\n".to_owned();
    }
    let mut out = String::new();
    if !report.live.is_empty() {
        out.push_str("Running across your repos:\n");
        for live in &report.live {
            out.push_str(&format!(
                "  {} · {} · {}\n",
                live.run,
                live.repo_root.display(),
                count(u64::from(live.sessions), "session", "sessions")
            ));
        }
    }
    if !report.runs.is_empty() {
        out.push_str("This repo:\n");
        for run in &report.runs {
            out.push_str(&format!("  {}\n", run_line(run)));
        }
    }
    sanitize_for_terminal(&out)
}

/// `run-03 · waiting · the plan approval · Add a flag`
pub(crate) fn run_line(run: &RunSummary) -> String {
    let mut parts = vec![run.run.to_string(), words(&run.state)];
    if !run.stopped_at.is_empty() {
        parts.push(run.stopped_at.clone());
    }
    parts.push(run.title.clone());
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use argonauts_app::LiveRunHost;
    use argonauts_core::api::RunState;
    use argonauts_core::{RunId, Spend};

    use super::*;

    fn run_id() -> RunId {
        RunId::new("run-03").unwrap()
    }

    #[test]
    fn exit_codes_follow_how_the_run_ended() {
        let finished = |outcome| exit_code(&RunCommandExit::Finished(outcome));
        assert_eq!(finished(RunOutcome::Merged), ExitCode::SUCCESS);
        assert_eq!(finished(RunOutcome::PullRequest), ExitCode::SUCCESS);
        assert_eq!(finished(RunOutcome::BranchLeft), ExitCode::SUCCESS);
        assert_eq!(finished(RunOutcome::Failed), ExitCode::FAILURE);
        assert_eq!(finished(RunOutcome::Abandoned), ExitCode::FAILURE);
        assert_eq!(finished(RunOutcome::Stopped), ExitCode::from(130));
        let waiting = RunCommandExit::Waiting {
            run: run_id(),
            approval: "the plan approval".to_owned(),
        };
        assert_eq!(exit_code(&waiting), ExitCode::from(WAITING_EXIT));
        assert_eq!(
            exit_code(&RunCommandExit::Detached(run_id())),
            ExitCode::SUCCESS
        );
    }

    #[test]
    fn a_waiting_run_says_how_to_resume_it() {
        let waiting = RunCommandExit::Waiting {
            run: run_id(),
            approval: "the plan\u{1b}[2J approval".to_owned(),
        };
        assert_eq!(
            closing_line(&waiting, false).unwrap(),
            "waiting for you: the plan approval · argonauts resume run-03"
        );
        let detached = RunCommandExit::Detached(run_id());
        assert_eq!(closing_line(&detached, true).unwrap(), "run-03");
        assert_eq!(
            closing_line(&detached, false).unwrap(),
            "run-03 goes on · argonauts resume run-03"
        );
        assert_eq!(
            closing_line(&RunCommandExit::Finished(RunOutcome::Merged), false),
            None
        );
    }

    #[test]
    fn status_lists_live_run_hosts_and_this_repos_runs() {
        assert_eq!(
            status_text(&StatusReport::default()),
            "Nothing is running.\n"
        );
        let report = StatusReport {
            live: vec![LiveRunHost {
                pid: 42,
                start_time: 0,
                repo_root: "/code/app".into(),
                run: run_id(),
                sessions: 2,
            }],
            runs: vec![RunSummary {
                run: run_id(),
                title: "Add a flag".to_owned(),
                mode: RunMode::Crew,
                workflow: "council".to_owned(),
                state: RunState::Waiting,
                stopped_at: "the plan approval".to_owned(),
                updated: time::OffsetDateTime::UNIX_EPOCH,
                spent: Spend::default(),
                sessions_to_resume: 0,
                waiting: Vec::new(),
                live: false,
                remind: false,
                name: None,
            }],
        };
        assert_eq!(
            status_text(&report),
            "Running across your repos:\n  run-03 · /code/app · 2 sessions\nThis repo:\n  \
             run-03 · waiting · the plan approval · Add a flag\n"
        );
    }
}
