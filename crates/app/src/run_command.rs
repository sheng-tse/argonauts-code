//! `argonauts run`, for scripts and CI: the same settings and engine through `ConductorApi`,
//! then the app attached, plain lines without a terminal, or back at once with `--detach`.
//! It never opens the guided setup.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::bail;
use argonauts_core::RunId;
use argonauts_core::api::{ConductorApi, RunState, StartRun};
use argonauts_core::event::AgentEvent;
use argonauts_core::events::checkpoint::CheckpointEvent;
use argonauts_core::events::run::{
    ParkReason, RunLifecycle, RunMode, RunOutcome, RunStarted, TriageDecision,
};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::CheckpointId;
use argonauts_core::sanitize::sanitize_for_terminal;
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::{ForcedMode, TriageMiddle, TriageSettings};
use argonauts_core::{Note, NoteLevel};
use argonauts_registry::safety_keys_in;
use serde::Serialize;
use tokio::sync::mpsc;

use crate::ancestry::{AgentMarkers, check_self};
use crate::app_settings::AppSettings;
use crate::env::AppEnv;
use crate::{FirstView, InteractiveOptions, client, startup};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunCommand {
    pub repo: Option<PathBuf>,
    pub task: String,
    /// `--solo` or `--crew`; triage decides when `None`.
    pub mode: Option<RunMode>,
    /// `--set key=value`, in order.
    pub overrides: Vec<String>,
    pub plain: bool,
    pub detach: bool,
    pub screen_reader: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunCommandExit {
    Finished(RunOutcome),
    /// Parked at a checkpoint: `waiting for you: <checkpoint> · argonauts resume <run>`.
    Waiting {
        run: RunId,
        checkpoint: String,
    },
    Detached(RunId),
}

pub async fn run_command(command: RunCommand) -> anyhow::Result<RunCommandExit> {
    let found = AppEnv::discover(command.repo.as_deref())?;
    // Safety keys among the `--set` flags need a person at a terminal. An agent can fake a
    // terminal, so the ancestry check has a say too.
    let at_terminal = found.interactive
        && !found.agent_session
        && check_self(&found.paths, &AgentMarkers::builtin()).is_ok();
    let env = Arc::new(AppEnv {
        interactive: at_terminal,
        ..found
    });
    if !at_terminal {
        refuse_safety_flags(&command.overrides)?;
    }
    // Resolving here turns a bad `--set` into an error before anything starts.
    let settings = AppSettings::load(&env, command.overrides.clone(), at_terminal)?;
    let (conductor, _) = client::for_app(Arc::clone(&env))?;
    let request = start_request(&command, settings.settings());
    let run = conductor.start(request).await?.run;
    if command.detach {
        return Ok(RunCommandExit::Detached(run));
    }
    if at_terminal && !command.plain {
        let options = InteractiveOptions {
            repo: Some(env.repo_root.clone()),
            overrides: command.overrides,
            screen_reader: command.screen_reader,
            guided_setup: false,
        };
        startup::run(options, FirstView::Attach(run.clone())).await?;
        return after_the_app(conductor.as_ref(), &run).await;
    }
    let mut interrupts = interrupts()?;
    let mut out = std::io::stdout();
    follow(conductor.as_ref(), &run, &mut out, &mut interrupts).await
}

/// The safety keys `run` has flags for, besides `--set`, with those flags.
const SAFETY_FLAGS: &[(&str, &str)] = &[("gates.run", "--gate"), ("merge.mode", "--merge")];

/// Resolving refuses every safety key without a terminal. For the ones `run` has a flag for,
/// this names the flag and says what works in scripts and CI.
fn refuse_safety_flags(overrides: &[String]) -> anyhow::Result<()> {
    for (key, value) in overrides.iter().filter_map(|flag| flag.split_once('=')) {
        for safety in safety_keys_in(key, value) {
            if let Some((_, flag)) = SAFETY_FLAGS.iter().find(|(known, _)| *known == safety) {
                bail!(
                    "{safety} is a safety setting, so {flag} and --set {safety} need you at a \
                     terminal. For scripts and CI, set it in .argonauts/config.toml and trust \
                     that file in the app, or set it in ~/.argonauts/config.toml"
                );
            }
        }
    }
    Ok(())
}

fn start_request(command: &RunCommand, settings: &Settings) -> StartRun {
    let (mode, triage) = match command.mode {
        Some(mode) => (mode, None),
        None => {
            let decision = triage_without_estimate(&settings.triage);
            (decision.mode, Some(decision))
        }
    };
    StartRun {
        request: command.task.clone(),
        mode,
        forced: command.mode.is_some(),
        estimate: None,
        triage,
        chat_turn: None,
        continues: None,
        overrides: command.overrides.clone(),
        include_uncommitted: settings.intake.include_uncommitted,
        default_agent: None,
        approval_mode: None,
        front_end: None,
    }
}

/// Without a chat there's no estimate, so only the settings decide: All hands, a forced mode,
/// or what triage does between its thresholds.
fn triage_without_estimate(triage: &TriageSettings) -> TriageDecision {
    if let Some(crew) = argonauts_crew::triage::all_hands(triage) {
        return crew;
    }
    if let Some(forced) = triage.force {
        let mode = match forced {
            ForcedMode::Solo => RunMode::Solo,
            ForcedMode::Crew => RunMode::Crew,
        };
        return TriageDecision {
            mode,
            escalate_to_crew: false,
            rule: "force".to_owned(),
            reason: "triage.force picks the mode".to_owned(),
        };
    }
    let (mode, escalate_to_crew) = match triage.otherwise {
        TriageMiddle::SoloThenCrew => (RunMode::Solo, true),
        TriageMiddle::Solo => (RunMode::Solo, false),
        TriageMiddle::Crew => (RunMode::Crew, false),
    };
    TriageDecision {
        mode,
        escalate_to_crew,
        rule: "otherwise".to_owned(),
        reason: "no estimate from the command line".to_owned(),
    }
}

/// Ctrl+C, a terminate, or a hangup, each as one message.
fn interrupts() -> anyhow::Result<mpsc::Receiver<()>> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let (sender, receiver) = mpsc::channel(4);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = interrupt.recv() => {}
                _ = terminate.recv() => {}
                _ = hangup.recv() => {}
            }
            if sender.send(()).await.is_err() {
                break;
            }
        }
    });
    Ok(receiver)
}

/// Prints the run's events as plain lines until it finishes or parks. The first interrupt asks
/// the run to stop, keeping its work; the second stops watching.
async fn follow(
    conductor: &dyn ConductorApi,
    run: &RunId,
    out: &mut dyn Write,
    interrupts: &mut mpsc::Receiver<()>,
) -> anyhow::Result<RunCommandExit> {
    let mut stream = conductor.subscribe(run, 0).await?;
    let mut titles: HashMap<CheckpointId, String> = HashMap::new();
    let mut stopping = false;
    loop {
        tokio::select! {
            event = stream.events.recv() => {
                let Some(event) = event else { break };
                for line in plain_lines(&event) {
                    writeln!(out, "{}", sanitize_for_terminal(&line))?;
                }
                match event.kind {
                    RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) => {
                        titles.insert(opened.id.clone(), opened.title.clone());
                    }
                    RunEventKind::Run(RunLifecycle::Finished { outcome, .. }) => {
                        return Ok(RunCommandExit::Finished(outcome));
                    }
                    RunEventKind::Run(RunLifecycle::Parked { reason, waiting_on }) => {
                        let fallback = park_words(&reason, &waiting_on, &titles);
                        let checkpoint = stopped_at(conductor, run).await.unwrap_or(fallback);
                        return Ok(RunCommandExit::Waiting { run: run.clone(), checkpoint });
                    }
                    _ => {}
                }
            }
            Some(()) = interrupts.recv() => {
                if stopping {
                    bail!("stopped watching; {run} is stopping on its own · argonauts resume {run}");
                }
                stopping = true;
                writeln!(out, "Stopping {run}. Its agents wrap up and their work is kept.")?;
                conductor.stop(run).await?;
            }
        }
    }
    // The stream closed before the run said it ended, so the run's summary decides.
    match summary_exit(conductor, run).await {
        Some(RunCommandExit::Detached(_)) | None => {
            bail!("{run}'s supervisor stopped before the run ended · argonauts resume {run}")
        }
        Some(exit) => Ok(exit),
    }
}

/// After the app closes, the run may have ended, parked, or still be going.
async fn after_the_app(
    conductor: &dyn ConductorApi,
    run: &RunId,
) -> anyhow::Result<RunCommandExit> {
    let exit = summary_exit(conductor, run).await;
    if exit.is_none()
        && let Ok(runs) = conductor.list().await
        && runs
            .iter()
            .any(|summary| summary.run == *run && summary.state == RunState::Crashed)
    {
        bail!("{run}'s supervisor stopped before the run ended · argonauts resume {run}");
    }
    Ok(exit.unwrap_or_else(|| RunCommandExit::Detached(run.clone())))
}

/// What the run's summary says: ended, parked, or still going. `None` when it crashed or isn't
/// listed.
async fn summary_exit(conductor: &dyn ConductorApi, run: &RunId) -> Option<RunCommandExit> {
    let runs = conductor.list().await.ok()?;
    let summary = runs.into_iter().find(|summary| summary.run == *run)?;
    let outcome = match summary.state {
        RunState::Merged => RunOutcome::Merged,
        RunState::PullRequest => RunOutcome::PullRequest,
        RunState::BranchLeft => RunOutcome::BranchLeft,
        RunState::Stopped => RunOutcome::Stopped,
        RunState::Failed => RunOutcome::Failed,
        RunState::Abandoned => RunOutcome::Abandoned,
        RunState::Parked => {
            return Some(RunCommandExit::Waiting {
                run: run.clone(),
                checkpoint: summary.stopped_at,
            });
        }
        RunState::Running | RunState::Pausing => {
            return Some(RunCommandExit::Detached(run.clone()));
        }
        RunState::Crashed => return None,
    };
    Some(RunCommandExit::Finished(outcome))
}

/// The run summary's words for where it stopped, such as "the plan checkpoint".
async fn stopped_at(conductor: &dyn ConductorApi, run: &RunId) -> Option<String> {
    let runs = conductor.list().await.ok()?;
    runs.into_iter()
        .find(|summary| summary.run == *run)
        .map(|summary| summary.stopped_at)
        .filter(|words| !words.is_empty())
}

fn park_words(
    reason: &ParkReason,
    waiting_on: &[CheckpointId],
    titles: &HashMap<CheckpointId, String>,
) -> String {
    match reason {
        ParkReason::Checkpoint => {
            let named: Vec<&str> = waiting_on
                .iter()
                .filter_map(|id| titles.get(id).map(String::as_str))
                .collect();
            if named.is_empty() {
                "a checkpoint".to_owned()
            } else {
                named.join(", ")
            }
        }
        ParkReason::Paused => "the pause you asked for".to_owned(),
        ParkReason::AllVendorsOut { .. } => "a vendor to come back".to_owned(),
        ParkReason::WaitingForVendor { vendor, .. } => format!("{vendor} to come back"),
    }
}

/// The lines a script's log wants for one event. Agent text is untrusted, so the caller
/// sanitizes every line.
fn plain_lines(event: &RunEvent) -> Vec<String> {
    let mut lines: Vec<String> = plain_line(event).into_iter().collect();
    if let RunEventKind::Run(RunLifecycle::Started(started)) = &event.kind {
        lines.extend(uncommitted_line(started));
    }
    lines
}

/// Whether the run's base left out the working tree's uncommitted files or took them in.
fn uncommitted_line(started: &RunStarted) -> Option<String> {
    let line = match (started.uncommitted.len(), started.uncommitted_included) {
        (0, _) => return None,
        (1, false) => {
            "1 uncommitted file is left out (intake.include_uncommitted includes it).".to_owned()
        }
        (count, false) => format!(
            "{count} uncommitted files are left out (intake.include_uncommitted includes them)."
        ),
        (1, true) => "1 uncommitted file is included.".to_owned(),
        (count, true) => format!("{count} uncommitted files are included."),
    };
    Some(line)
}

fn plain_line(event: &RunEvent) -> Option<String> {
    match &event.kind {
        RunEventKind::Run(lifecycle) => match lifecycle {
            RunLifecycle::Started(started) => Some(format!(
                "{} started: {} ({})",
                event.run_id,
                started.title,
                name_of(&started.mode)
            )),
            RunLifecycle::Resumed(_) => Some(format!("{} resumed.", event.run_id)),
            RunLifecycle::StopRequested { .. } => Some("Stop requested.".to_owned()),
            RunLifecycle::Finished { outcome, detail } => Some(match detail {
                Some(detail) => format!("Finished: {} · {detail}", name_of(outcome)),
                None => format!("Finished: {}.", name_of(outcome)),
            }),
            _ => None,
        },
        RunEventKind::Phase(change) => {
            let mut line = format!("{} {}", name_of(&change.phase), name_of(&change.status));
            if let Some(summary) = &change.summary {
                line.push_str(" · ");
                line.push_str(summary);
            }
            Some(line)
        }
        RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) => Some(opened.title.clone()),
        RunEventKind::Note(note) => Some(note_line(note)),
        RunEventKind::Agent(AgentEvent::Message { text, .. }) => {
            Some(format!("{}: {text}", event.source))
        }
        _ => None,
    }
}

fn note_line(note: &Note) -> String {
    match note.level {
        NoteLevel::Info => note.text.clone(),
        NoteLevel::Warn => format!("warning: {}", note.text),
        NoteLevel::Error => format!("error: {}", note.text),
    }
}

/// An enum's snake-case name, as its serde form spells it.
fn name_of(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use argonauts_core::api::RunSummary;
    use argonauts_core::{Paths, Spend};
    use argonauts_testkit::TempRepo;
    use time::OffsetDateTime;

    use super::*;
    use crate::client::test_support::run_started;
    use crate::controller::test_support::{Call, FakeConductor};

    fn run_id() -> RunId {
        RunId::new("run-001").unwrap()
    }

    fn event(kind: RunEventKind) -> RunEvent {
        RunEvent {
            seq: 0,
            ts: OffsetDateTime::now_utc(),
            run_id: run_id(),
            card_id: None,
            session: None,
            source: "codex".to_owned(),
            kind,
        }
    }

    fn summary(state: RunState, stopped_at: &str) -> RunSummary {
        RunSummary {
            run: run_id(),
            title: "Add a flag".to_owned(),
            mode: RunMode::Solo,
            preset: "peer-crew".to_owned(),
            state,
            stopped_at: stopped_at.to_owned(),
            updated: OffsetDateTime::now_utc(),
            spent: Spend::default(),
            sessions_to_resume: 0,
            waiting: Vec::new(),
            live: false,
            remind: false,
        }
    }

    /// Follows run-001 while its events are pushed, then `interrupts` Ctrl+Cs or the stream's
    /// end. Returns the result and what was printed.
    async fn follow_with(
        conductor: &FakeConductor,
        events: Vec<RunEvent>,
        interrupts: usize,
    ) -> (anyhow::Result<RunCommandExit>, String) {
        let run = conductor.start(start_request(&RunCommand::default(), &Settings::default()));
        let run = run.await.unwrap().run;
        let (interrupt, mut received) = mpsc::channel(4);
        let mut out = Vec::new();
        let feed = async {
            let subscribed = || {
                conductor
                    .calls()
                    .iter()
                    .any(|call| matches!(call, Call::Subscribe { .. }))
            };
            while !subscribed() {
                tokio::task::yield_now().await;
            }
            for event in events {
                assert!(conductor.push(&run, event).await);
            }
            for _ in 0..interrupts {
                interrupt.send(()).await.unwrap();
                tokio::task::yield_now().await;
            }
            if interrupts == 0 {
                conductor.end(&run, false);
            }
        };
        let (result, ()) = tokio::join!(follow(conductor, &run, &mut out, &mut received), feed);
        (result, String::from_utf8(out).unwrap())
    }

    #[test]
    fn without_a_terminal_the_gate_and_merge_flags_are_refused_by_name() {
        let refusal = |flag: &str| {
            refuse_safety_flags(&["ui.bell=false".to_owned(), flag.to_owned()])
                .expect_err(flag)
                .to_string()
        };
        let merge = refusal("merge.mode=\"leave\"");
        assert!(
            merge.starts_with(
                "merge.mode is a safety setting, so --merge and --set merge.mode need you"
            ),
            "{merge}"
        );
        assert!(merge.contains(".argonauts/config.toml"), "{merge}");
        let gates = refusal(r#"gates.run=[{ command = ["true"], name = "true" }]"#);
        assert!(gates.contains("--gate and --set gates.run"), "{gates}");
        let table = refusal(r#"merge={ mode = "ask" }"#);
        assert!(table.contains("--merge and --set merge.mode"), "{table}");
    }

    #[test]
    fn other_settings_are_left_to_resolving() {
        let overrides = [
            "ui.bell=false".to_owned(),
            "budget.per_card_usd=2.0".to_owned(),
            "approvals.merge.mode=auto".to_owned(),
        ];
        assert!(refuse_safety_flags(&overrides).is_ok());
    }

    #[test]
    fn a_forced_mode_skips_triage() {
        let command = RunCommand {
            task: "Add a flag".to_owned(),
            mode: Some(RunMode::Crew),
            overrides: vec!["ui.bell=false".to_owned()],
            ..RunCommand::default()
        };
        let request = start_request(&command, &Settings::default());
        assert_eq!(request.mode, RunMode::Crew);
        assert!(request.forced);
        assert!(request.triage.is_none());
        assert_eq!(request.overrides, vec!["ui.bell=false"]);
        assert_eq!(request.approval_mode, None);
    }

    #[test]
    fn without_a_mode_the_triage_settings_decide() {
        let request = start_request(&RunCommand::default(), &Settings::default());
        assert!(!request.forced);
        let triage = request.triage.unwrap();
        assert_eq!(
            (triage.mode, triage.escalate_to_crew),
            (RunMode::Solo, true)
        );
        let forced = TriageSettings {
            force: Some(ForcedMode::Crew),
            ..TriageSettings::default()
        };
        assert_eq!(triage_without_estimate(&forced).mode, RunMode::Crew);
    }

    /// The settings `argonauts run` resolves for a fresh repo, with `flags` as `--set` and
    /// `global` as the global settings file.
    fn settings_with(flags: &[&str], global: &str) -> AppSettings {
        let repo = TempRepo::new().unwrap();
        let paths = Paths::new(repo.home());
        let config = paths.global_config();
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, global).unwrap();
        let mut env = AppEnv::for_repo(paths, repo.root(), false).unwrap();
        env.agent_session = false;
        env.user_home = Some(repo.user_home().to_owned());
        let flags = flags.iter().map(|flag| (*flag).to_owned()).collect();
        AppSettings::load(&env, flags, false).unwrap()
    }

    #[test]
    fn the_intake_setting_decides_whether_uncommitted_files_come_along() {
        let command = RunCommand::default();
        let by_default = settings_with(&[], "");
        assert!(!start_request(&command, by_default.settings()).include_uncommitted);

        let by_flag = settings_with(&["intake.include_uncommitted=true"], "");
        assert!(start_request(&command, by_flag.settings()).include_uncommitted);

        let by_file = settings_with(&[], "[intake]\ninclude_uncommitted = true\n");
        assert!(start_request(&command, by_file.settings()).include_uncommitted);
    }

    /// All hands sends a task from the command line to the crew as the chat's triage does,
    /// whether a file or `--set` turns it on, and over a preset's forced solo. `--solo` still
    /// runs it solo.
    #[test]
    fn all_hands_sends_a_command_line_task_to_the_crew() {
        let command = RunCommand::default();
        let off = start_request(&command, settings_with(&[], "").settings());
        let triage = off.triage.unwrap();
        assert_eq!(
            (off.mode, triage.rule.as_str()),
            (RunMode::Solo, "otherwise")
        );

        let forced_solo = "[triage]\nforce = \"solo\"\n";
        for settings in [
            settings_with(&["triage.all_hands=true"], ""),
            settings_with(&[], "[triage]\nall_hands = true\n"),
            settings_with(&["triage.all_hands=true"], forced_solo),
        ] {
            let request = start_request(&command, settings.settings());
            assert_eq!(request.mode, RunMode::Crew);
            assert!(!request.forced);
            let triage = request.triage.unwrap();
            assert_eq!(
                (triage.rule.as_str(), triage.reason.as_str()),
                ("all_hands", "All hands is on")
            );
            assert!(!triage.escalate_to_crew);

            let solo = RunCommand {
                mode: Some(RunMode::Solo),
                ..RunCommand::default()
            };
            let request = start_request(&solo, settings.settings());
            assert_eq!((request.mode, request.forced), (RunMode::Solo, true));
        }
    }

    fn started_with(uncommitted: &[&str], included: bool) -> RunEvent {
        let mut event = run_started(0, "Add a flag", None);
        let RunEventKind::Run(RunLifecycle::Started(started)) = &mut event.kind else {
            unreachable!()
        };
        started.uncommitted = uncommitted.iter().map(|path| (*path).to_owned()).collect();
        started.uncommitted_included = included;
        event
    }

    #[test]
    fn the_started_lines_say_what_the_base_left_out() {
        let lines =
            |uncommitted: &[&str], included| plain_lines(&started_with(uncommitted, included));
        assert_eq!(
            lines(&[], false),
            ["run-20260930-101500-00ab started: Add a flag (solo)"]
        );
        assert_eq!(
            lines(&["a.rs"], false)[1],
            "1 uncommitted file is left out (intake.include_uncommitted includes it)."
        );
        assert_eq!(
            lines(&["a.rs", "b.rs", "c.rs"], false)[1],
            "3 uncommitted files are left out (intake.include_uncommitted includes them)."
        );
        assert_eq!(
            lines(&["a.rs", "b.rs"], true)[1],
            "2 uncommitted files are included."
        );
    }

    #[tokio::test]
    async fn plain_lines_follow_the_run_until_it_finishes() {
        let conductor = FakeConductor::default();
        let events = vec![
            started_with(&["a.rs", "b.rs"], false),
            event(RunEventKind::Agent(AgentEvent::Message {
                text: "done \u{1b}]0;title\u{7}here".to_owned(),
                parent: None,
            })),
            event(RunEventKind::Run(RunLifecycle::Finished {
                outcome: RunOutcome::BranchLeft,
                detail: None,
            })),
        ];
        let (result, out) = follow_with(&conductor, events, 0).await;
        assert_eq!(
            result.unwrap(),
            RunCommandExit::Finished(RunOutcome::BranchLeft)
        );
        assert_eq!(
            out,
            "run-20260930-101500-00ab started: Add a flag (solo)\n2 uncommitted files are left out \
             (intake.include_uncommitted includes them).\ncodex: done here\nFinished: branch_left.\n"
        );
    }

    #[tokio::test]
    async fn a_parked_run_waits_for_you_at_its_checkpoint() {
        let conductor = FakeConductor::default();
        conductor.set_runs(vec![summary(RunState::Parked, "the plan checkpoint")]);
        let parked = event(RunEventKind::Run(RunLifecycle::Parked {
            reason: ParkReason::Checkpoint,
            waiting_on: Vec::new(),
        }));
        let (result, _) = follow_with(&conductor, vec![parked.clone()], 0).await;
        assert_eq!(
            result.unwrap(),
            RunCommandExit::Waiting {
                run: run_id(),
                checkpoint: "the plan checkpoint".to_owned(),
            }
        );

        let unlisted = FakeConductor::default();
        let (result, _) = follow_with(&unlisted, vec![parked], 0).await;
        assert_eq!(
            result.unwrap(),
            RunCommandExit::Waiting {
                run: run_id(),
                checkpoint: "a checkpoint".to_owned(),
            }
        );
    }

    #[tokio::test]
    async fn a_stream_that_ends_early_is_read_from_the_summary() {
        let conductor = FakeConductor::default();
        conductor.set_runs(vec![summary(RunState::Merged, "")]);
        let (result, _) = follow_with(&conductor, Vec::new(), 0).await;
        assert_eq!(
            result.unwrap(),
            RunCommandExit::Finished(RunOutcome::Merged)
        );

        let crashed = FakeConductor::default();
        crashed.set_runs(vec![summary(RunState::Crashed, "")]);
        let (result, _) = follow_with(&crashed, Vec::new(), 0).await;
        let err = result.unwrap_err().to_string();
        assert!(err.contains("argonauts resume run-001"), "{err}");
    }

    #[tokio::test]
    async fn the_first_interrupt_stops_the_run_and_the_second_stops_watching() {
        let conductor = FakeConductor::default();
        let (result, out) = follow_with(&conductor, Vec::new(), 2).await;
        assert!(out.starts_with("Stopping run-001."), "{out}");
        assert!(conductor.calls().contains(&Call::Stop(run_id())));
        let err = result.unwrap_err().to_string();
        assert!(err.starts_with("stopped watching"), "{err}");
    }

    #[tokio::test]
    async fn after_the_app_a_running_run_goes_on() {
        let conductor = FakeConductor::default();
        conductor.set_runs(vec![summary(RunState::Running, "")]);
        assert_eq!(
            after_the_app(&conductor, &run_id()).await.unwrap(),
            RunCommandExit::Detached(run_id())
        );
        conductor.set_runs(vec![summary(RunState::Crashed, "")]);
        assert!(after_the_app(&conductor, &run_id()).await.is_err());
    }
}
