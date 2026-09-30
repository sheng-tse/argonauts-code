use std::future::Future;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use argonauts_conductor::{
    MergeDecider, MergeRequest, SoloOutcome, SoloRun, StopHandle, budget_problem,
};
use argonauts_core::{Adapter, Config, LoadedConfig, MergeDecision, Paths, RunOutcome, UiConfig};
use argonauts_tui::{
    MergePrompt, UiCommand, UiEvent, UiOptions, clean_line, run_plain_stdio, run_terminal,
    wants_plain,
};
use tokio::sync::mpsc;
use tokio::task::{JoinError, JoinHandle};

use crate::args::RunArgs;
use crate::signals::Interrupts;
use crate::usage::usage;
use crate::{agents, config};

const SCREEN_GRACE: Duration = Duration::from_secs(3);

/// As for a process ended by SIGINT.
const CANCELLED_EXIT: u8 = 130;

pub async fn run(args: RunArgs) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir().context("reading the current directory")?;
    let root = config::repo_root(&cwd)
        .await
        .ok_or_else(|| anyhow!("{} is not inside a git repository", cwd.display()))?;
    let (config, adapter) = config_and_agent(&args, &root)?;
    let paths = Paths::from_env()
        .map_err(|_| anyhow!("could not find your home directory; set ARGONAUTS_HOME"))?;
    let mut interrupts = Interrupts::install().context("catching Ctrl+C")?;

    let (ui_tx, ui_rx) = mpsc::unbounded_channel();
    let (mut screen, mut commands) = Screen::spawn(ui_rx, &config.ui);

    let stop = StopHandle::new();
    let sink = ui_tx.clone();
    let solo = SoloRun::new(args.task_text(), root, adapter, config, paths)
        .on_event(move |event| {
            // A closed screen stops the run elsewhere; the log still gets every event.
            let _ = sink.send(event.into());
        })
        .stop_handle(stop.clone());
    let mut decider = ScreenDecider { events: ui_tx };

    let ended = supervise(
        solo.run(&mut decider),
        &stop,
        &mut interrupts,
        &mut commands,
        &mut screen,
    )
    .await;
    // The run and the decider hold the last senders, so the interface now sees the end.
    drop(decider);
    screen.finish().await;

    match ended {
        Ended::Finished(outcome) => {
            let outcome = outcome?;
            screen.print_outcome(&outcome);
            Ok(exit_code(outcome.outcome))
        }
        Ended::Aborted => {
            eprintln!(
                "Quit without waiting for the agent. Its worktree may hold uncommitted work."
            );
            Ok(ExitCode::from(CANCELLED_EXIT))
        }
    }
}

fn config_and_agent(args: &RunArgs, root: &Path) -> anyhow::Result<(Config, Arc<dyn Adapter>)> {
    let loaded = load_run_config(args, root).map_err(usage)?;
    config::print_warnings(&loaded);
    let config = loaded.config;
    let adapter = agents::by_id(&config.default_agent).map_err(usage)?;
    ensure_installed(adapter.as_ref())?;
    if let Some(problem) = budget_problem(&config, adapter.as_ref()) {
        return Err(usage(anyhow!(problem)));
    }
    Ok((config, adapter))
}

/// Loads the config twice, once to learn which agent runs and again with its flag overrides.
fn load_run_config(args: &RunArgs, root: &Path) -> anyhow::Result<LoadedConfig> {
    let first = config::load(Some(root), args.agent_overrides())?;
    let agent = first.config.default_agent;
    let adapter =
        agents::by_id(&agent).context("pick an agent with --agent or set default_agent")?;
    config::load(
        Some(root),
        args.overrides(&agent, adapter.cheapest_model())?,
    )
}

fn ensure_installed(adapter: &dyn Adapter) -> anyhow::Result<()> {
    which::which(adapter.binary()).map_err(|_| {
        anyhow!(
            "{} (`{}`) is not installed or not on PATH. Run `argonauts doctor` to check.",
            adapter.display_name(),
            adapter.binary()
        )
    })?;
    Ok(())
}

enum Ended {
    Finished(anyhow::Result<SoloOutcome>),
    /// A signal arrived after a stop was already requested, so the run was dropped. Dropping it
    /// kills the agent's process group.
    Aborted,
}

/// The first stop request stops the run cleanly. A signal after that gives up on it.
async fn supervise(
    run: impl Future<Output = anyhow::Result<SoloOutcome>>,
    stop: &StopHandle,
    interrupts: &mut Interrupts,
    commands: &mut mpsc::UnboundedReceiver<UiCommand>,
    screen: &mut Screen,
) -> Ended {
    tokio::pin!(run);
    let mut commands_open = true;
    loop {
        tokio::select! {
            outcome = &mut run => return Ended::Finished(outcome),
            () = interrupts.recv() => {
                if stop.is_stopped() {
                    return Ended::Aborted;
                }
                stop.stop();
                if screen.plain {
                    eprintln!("Stopping the agent and keeping its work. Press Ctrl+C again to quit at once.");
                }
            }
            command = commands.recv(), if commands_open => match command {
                Some(UiCommand::StopRun) => stop.stop(),
                None => commands_open = false,
            },
            joined = &mut screen.task, if screen.running() => {
                screen.mark_closed_early(joined);
                stop.stop();
                eprintln!("The screen closed. Waiting for the agent to stop; its work so far is kept. Press Ctrl+C to quit at once.");
            }
        }
    }
}

struct Screen {
    task: JoinHandle<anyhow::Result<()>>,
    plain: bool,
    /// Set once the task has ended.
    result: Option<anyhow::Result<()>>,
    /// Whether it ended before the run did, so it missed the end.
    closed_early: bool,
}

impl Screen {
    /// The plain renderer sends no commands, so its receiver comes back already closed.
    fn spawn(
        events: mpsc::UnboundedReceiver<UiEvent>,
        ui: &UiConfig,
    ) -> (Self, mpsc::UnboundedReceiver<UiCommand>) {
        let plain = wants_plain(ui.plain);
        let options = UiOptions::from_config(ui);
        let (command_tx, commands) = mpsc::unbounded_channel();
        let screen = if plain {
            drop(command_tx);
            Self::new(tokio::spawn(run_plain_stdio(events, options)), true)
        } else {
            Self::new(
                tokio::spawn(run_terminal(events, command_tx, options)),
                false,
            )
        };
        (screen, commands)
    }

    fn new(task: JoinHandle<anyhow::Result<()>>, plain: bool) -> Self {
        Self {
            task,
            plain,
            result: None,
            closed_early: false,
        }
    }

    fn running(&self) -> bool {
        self.result.is_none()
    }

    fn mark_closed_early(&mut self, joined: Result<anyhow::Result<()>, JoinError>) {
        self.closed_early = true;
        self.result = Some(screen_result(joined));
    }

    /// The plain renderer may be blocked reading an answer from stdin, so this waits only a
    /// little for the last events.
    async fn finish(&mut self) {
        if !self.running() {
            return;
        }
        match tokio::time::timeout(SCREEN_GRACE, &mut self.task).await {
            Ok(joined) => self.result = Some(screen_result(joined)),
            Err(_) => self.closed_early = true,
        }
    }

    /// After a missed end this goes to stderr, because a stuck plain renderer still holds stdout.
    fn print_outcome(&self, outcome: &SoloOutcome) {
        if let Some(Err(err)) = &self.result {
            eprintln!("warning: {err:#}");
        }
        let to_stderr = self.closed_early || self.running();
        for line in outcome_lines(outcome, self.closed_early) {
            if to_stderr {
                eprintln!("{line}");
            } else {
                println!("{line}");
            }
        }
    }
}

fn screen_result(joined: Result<anyhow::Result<()>, JoinError>) -> anyhow::Result<()> {
    joined.unwrap_or_else(|err| Err(anyhow!("the screen crashed: {err}")))
}

fn outcome_lines(outcome: &SoloOutcome, include_detail: bool) -> Vec<String> {
    let mut lines = Vec::new();
    if include_detail {
        lines.push(format!("run finished: {}", outcome_word(outcome.outcome)));
        // The detail can quote the agent's report or git output about files the agent named.
        lines.push(format!("  {}", clean_line(&outcome.detail)));
    }
    if let Some(branch) = &outcome.branch {
        match &outcome.commit {
            Some(commit) => lines.push(format!("branch: {branch} at {}", short_sha(commit))),
            None => lines.push(format!("branch: {branch}")),
        }
    }
    if let Some(worktree) = &outcome.worktree {
        lines.push(format!("worktree kept: {}", worktree.display()));
    }
    lines.push(format!("log: {}", outcome.event_log.display()));
    lines
}

fn outcome_word(outcome: RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Merged => "merged",
        RunOutcome::BranchLeft => "branch left",
        RunOutcome::Failed => "failed",
        RunOutcome::Cancelled => "cancelled",
    }
}

fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

pub fn exit_code(outcome: RunOutcome) -> ExitCode {
    match outcome {
        RunOutcome::Merged | RunOutcome::BranchLeft => ExitCode::SUCCESS,
        RunOutcome::Failed => ExitCode::FAILURE,
        RunOutcome::Cancelled => ExitCode::from(CANCELLED_EXIT),
    }
}

struct ScreenDecider {
    events: mpsc::UnboundedSender<UiEvent>,
}

impl MergeDecider for ScreenDecider {
    async fn decide(&mut self, request: MergeRequest) -> anyhow::Result<MergeDecision> {
        let mut diff = request.diff;
        if request.diff_truncated {
            diff.push_str(&format!(
                "\n… the diff is cut short here. The full diff is in {}\n",
                request.diff_path.display()
            ));
        }
        let (prompt, answer) = MergePrompt::new(request.card_id, request.card, Some(diff));
        self.events
            .send(UiEvent::MergeCardShown(Box::new(prompt)))
            .map_err(|_| anyhow!("the screen is closed"))?;
        answer
            .await
            .map_err(|_| anyhow!("the merge card was closed without an answer"))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use argonauts_core::{CardId, DiffStat, MergeCard, RunId};

    use super::*;

    fn outcome(kind: RunOutcome) -> SoloOutcome {
        SoloOutcome {
            run_id: RunId::new("run-1").unwrap(),
            outcome: kind,
            detail: "Left argonauts/run-1/card-001 as it is.".to_owned(),
            branch: Some("argonauts/run-1/card-001".to_owned()),
            commit: Some("0123456789abcdef".to_owned()),
            worktree: None,
            event_log: PathBuf::from("/home/runs/run-1/events.jsonl"),
        }
    }

    fn request(diff: &str, truncated: bool) -> MergeRequest {
        MergeRequest {
            run_id: RunId::new("run-1").unwrap(),
            card_id: CardId::numbered(1),
            title: "Add hello".to_owned(),
            card: MergeCard {
                branch: "argonauts/run-1/card-001".to_owned(),
                target_branch: Some("main".to_owned()),
                stat: DiffStat::default(),
                gates: Vec::new(),
                report: None,
                budget: None,
                stopped_by: None,
            },
            diff: diff.to_owned(),
            diff_truncated: truncated,
            diff_path: PathBuf::from("/runs/card-001/diff.patch"),
        }
    }

    #[test]
    fn exit_codes_follow_the_outcome() {
        assert_eq!(exit_code(RunOutcome::Merged), ExitCode::SUCCESS);
        assert_eq!(exit_code(RunOutcome::BranchLeft), ExitCode::SUCCESS);
        assert_eq!(exit_code(RunOutcome::Failed), ExitCode::FAILURE);
        assert_eq!(exit_code(RunOutcome::Cancelled), ExitCode::from(130));
    }

    #[test]
    fn outcome_lines_name_the_branch_and_log() {
        let lines = outcome_lines(&outcome(RunOutcome::BranchLeft), false);
        assert_eq!(
            lines,
            vec![
                "branch: argonauts/run-1/card-001 at 0123456",
                "log: /home/runs/run-1/events.jsonl",
            ]
        );
        let lines = outcome_lines(&outcome(RunOutcome::Failed), true);
        assert_eq!(lines[0], "run finished: failed");
        assert_eq!(lines[1], "  Left argonauts/run-1/card-001 as it is.");
    }

    #[test]
    fn outcome_details_are_cleaned() {
        let mut dirty = outcome(RunOutcome::Failed);
        dirty.detail = "It reported: \x1b]0;owned\x07done\x1b[2J".to_owned();
        let lines = outcome_lines(&dirty, true);
        assert_eq!(lines[1], "  It reported: done");
    }

    #[tokio::test]
    async fn decider_passes_the_card_and_answer_through() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut decider = ScreenDecider { events: tx };
        let ui = tokio::spawn(async move {
            let Some(UiEvent::MergeCardShown(prompt)) = rx.recv().await else {
                panic!("expected a merge card");
            };
            let diff = prompt.diff.clone().unwrap();
            prompt.reply.send(MergeDecision::Approve).unwrap();
            diff
        });
        let decision = decider.decide(request("+hi\n", true)).await.unwrap();
        assert_eq!(decision, MergeDecision::Approve);
        let diff = ui.await.unwrap();
        assert!(diff.starts_with("+hi\n"), "{diff}");
        assert!(diff.contains("/runs/card-001/diff.patch"), "{diff}");
    }

    #[tokio::test]
    async fn decider_errors_when_nobody_answers() {
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx);
        let mut decider = ScreenDecider { events: tx };
        let err = decider.decide(request("", false)).await.unwrap_err();
        assert!(err.to_string().contains("screen is closed"), "{err}");

        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut decider = ScreenDecider { events: tx };
        tokio::spawn(async move { drop(rx.recv().await) });
        let err = decider.decide(request("", false)).await.unwrap_err();
        assert!(err.to_string().contains("without an answer"), "{err}");
    }
}
