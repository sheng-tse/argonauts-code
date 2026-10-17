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
use argonauts_core::events::approval::ApprovalEvent;
use argonauts_core::events::run::{
    CaptainOrCrewDecision, RunLifecycle, RunMode, RunOutcome, RunStarted, WaitReason,
};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::ApprovalId;
use argonauts_core::sanitize::sanitize_for_terminal;
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::{CaptainOrCrewOtherwise, ForcedMode};
use argonauts_core::{Note, NoteLevel};
use argonauts_registry::safety_keys_in;
use serde::Serialize;
use tokio::sync::mpsc;

use crate::ancestry::{AgentMarkers, check_self};
use crate::app_settings::AppSettings;
use crate::controller::ultra;
use crate::env::AppEnv;
use crate::{FirstView, InteractiveOptions, client, startup};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunCommand {
    pub repo: Option<PathBuf>,
    pub task: String,
    /// `--captain` or `--crew`; the Captain-or-crew rules decide when `None`.
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
    /// Waiting at an approval: `waiting for you: <approval> · argonauts resume <run>`.
    Waiting {
        run: RunId,
        approval: String,
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
const SAFETY_FLAGS: &[(&str, &str)] = &[("checks.run", "--check"), ("merge.mode", "--merge")];

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
    // A task from the command line is real work, so with ultracode on in Captain on a CLI with no
    // workflow of its own it runs as one, every agent on that CLI and model, as in the
    // conversation. `--captain` decides for this task, as "just do it yourself" does.
    let same_model = command.mode.is_none() && ultra::runs_on_the_same_model(settings);
    let (mode, captain_or_crew) = match command.mode {
        Some(mode) => (mode, None),
        None if same_model => (RunMode::Crew, Some(same_model_decision())),
        None => {
            let decision = decision_without_estimate(settings);
            (decision.mode, Some(decision))
        }
    };
    StartRun {
        request: command.task.clone(),
        mode,
        forced: command.mode.is_some(),
        estimate: None,
        captain_or_crew,
        conversation_turn: None,
        continues: None,
        overrides: command.overrides.clone(),
        include_uncommitted: settings.intake.include_uncommitted,
        main_agent: None,
        approval_mode: None,
        outside_agent: None,
        instructions: Vec::new(),
        request_settings: if same_model {
            ultra::same_model(settings)
        } else {
            Vec::new()
        },
        required_steps: Vec::new(),
    }
}

fn same_model_decision() -> CaptainOrCrewDecision {
    CaptainOrCrewDecision {
        mode: RunMode::Crew,
        move_to_crew: false,
        rule: "ultracode".to_owned(),
        reason: "Ultracode is on, so a workflow runs it on the main agent's model".to_owned(),
        captain: None,
    }
}

/// Without a conversation there's no estimate, so only the settings decide: ultracode, a forced
/// mode, or what the rules do between their thresholds.
fn decision_without_estimate(settings: &Settings) -> CaptainOrCrewDecision {
    if let Some(crew) = argonauts_crew::captain_or_crew::ultracode(settings) {
        return crew;
    }
    let captain_or_crew = &settings.captain_or_crew;
    if let Some(forced) = captain_or_crew.force {
        let mode = match forced {
            ForcedMode::Captain => RunMode::Captain,
            ForcedMode::Crew => RunMode::Crew,
        };
        return CaptainOrCrewDecision {
            mode,
            move_to_crew: false,
            rule: "force".to_owned(),
            reason: "captain_or_crew.force picks the mode".to_owned(),
            captain: None,
        };
    }
    let (mode, move_to_crew) = match captain_or_crew.otherwise {
        CaptainOrCrewOtherwise::CaptainThenCrew => (RunMode::Captain, true),
        CaptainOrCrewOtherwise::Captain => (RunMode::Captain, false),
        CaptainOrCrewOtherwise::Crew => (RunMode::Crew, false),
    };
    CaptainOrCrewDecision {
        mode,
        move_to_crew,
        rule: "otherwise".to_owned(),
        reason: "no estimate from the command line".to_owned(),
        captain: None,
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

/// Prints the run's events as plain lines until it finishes or waits. The first interrupt asks
/// the run to stop, keeping its work; the second stops watching.
async fn follow(
    conductor: &dyn ConductorApi,
    run: &RunId,
    out: &mut dyn Write,
    interrupts: &mut mpsc::Receiver<()>,
) -> anyhow::Result<RunCommandExit> {
    let mut stream = conductor.subscribe(run, 0).await?;
    let mut titles: HashMap<ApprovalId, String> = HashMap::new();
    let mut stopping = false;
    loop {
        tokio::select! {
            event = stream.events.recv() => {
                let Some(event) = event else { break };
                for line in plain_lines(&event) {
                    writeln!(out, "{}", sanitize_for_terminal(&line))?;
                }
                match event.kind {
                    RunEventKind::Approval(ApprovalEvent::Opened(opened)) => {
                        titles.insert(opened.id.clone(), opened.title.clone());
                    }
                    RunEventKind::Run(RunLifecycle::Finished { outcome, .. }) => {
                        return Ok(RunCommandExit::Finished(outcome));
                    }
                    RunEventKind::Run(RunLifecycle::Waiting { reason, waiting_on }) => {
                        let fallback = wait_words(&reason, &waiting_on, &titles);
                        let approval = stopped_at(conductor, run).await.unwrap_or(fallback);
                        return Ok(RunCommandExit::Waiting { run: run.clone(), approval });
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
            bail!("{run}'s run host stopped before the run ended · argonauts resume {run}")
        }
        Some(exit) => Ok(exit),
    }
}

/// After the app closes, the run may have ended, be waiting, or still be going.
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
        bail!("{run}'s run host stopped before the run ended · argonauts resume {run}");
    }
    Ok(exit.unwrap_or_else(|| RunCommandExit::Detached(run.clone())))
}

/// What the run's summary says: ended, waiting, or still going. `None` when it crashed or isn't
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
        RunState::Waiting => {
            return Some(RunCommandExit::Waiting {
                run: run.clone(),
                approval: summary.stopped_at,
            });
        }
        RunState::Running | RunState::Pausing => {
            return Some(RunCommandExit::Detached(run.clone()));
        }
        RunState::Crashed | RunState::NeverStarted | RunState::FromAnotherBuild => return None,
    };
    Some(RunCommandExit::Finished(outcome))
}

/// The run summary's words for where it stopped, such as "the plan approval".
async fn stopped_at(conductor: &dyn ConductorApi, run: &RunId) -> Option<String> {
    let runs = conductor.list().await.ok()?;
    runs.into_iter()
        .find(|summary| summary.run == *run)
        .map(|summary| summary.stopped_at)
        .filter(|words| !words.is_empty())
}

fn wait_words(
    reason: &WaitReason,
    waiting_on: &[ApprovalId],
    titles: &HashMap<ApprovalId, String>,
) -> String {
    match reason {
        WaitReason::Approval => {
            let named: Vec<&str> = waiting_on
                .iter()
                .filter_map(|id| titles.get(id).map(String::as_str))
                .collect();
            if named.is_empty() {
                "an approval".to_owned()
            } else {
                named.join(", ")
            }
        }
        WaitReason::Paused => "the pause you asked for".to_owned(),
        WaitReason::AllAgentsOut { .. } => "an agent to come back".to_owned(),
        WaitReason::WaitingForAgent { agent_id, .. } => format!("{agent_id} to come back"),
        WaitReason::NoFallback { .. } => "a fallback".to_owned(),
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
            RunLifecycle::ClosingAnswer(answer) => Some(answer.text.clone()),
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
        RunEventKind::Approval(ApprovalEvent::Opened(opened)) => Some(opened.title.clone()),
        RunEventKind::Note(note) => Some(note_line(note)),
        RunEventKind::Agent(AgentEvent::Message { text, .. }) => {
            Some(format!("{}: {text}", event.source))
        }
        _ => None,
    }
}

/// The note's line, then its detail indented under it, since a log keeps both.
fn note_line(note: &Note) -> String {
    let mut line = match note.level {
        NoteLevel::Info | NoteLevel::Aside | NoteLevel::Quiet => note.line().to_owned(),
        NoteLevel::Warn | NoteLevel::Caution => format!("warning: {}", note.line()),
        NoteLevel::Error => format!("error: {}", note.line()),
    };
    for detail in note.detail().into_iter().flat_map(str::lines) {
        line.push_str("\n  ");
        line.push_str(detail.trim_end());
    }
    line
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
            task_id: None,
            session: None,
            source: "codex".to_owned(),
            kind,
        }
    }

    fn summary(state: RunState, stopped_at: &str) -> RunSummary {
        RunSummary {
            run: run_id(),
            title: "Add a flag".to_owned(),
            mode: RunMode::Captain,
            workflow: "council".to_owned(),
            state,
            stopped_at: stopped_at.to_owned(),
            updated: OffsetDateTime::now_utc(),
            spent: Spend::default(),
            sessions_to_resume: 0,
            waiting: Vec::new(),
            live: false,
            remind: false,
            name: None,
            shape_known: true,
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
    fn without_a_terminal_the_check_and_merge_flags_are_refused_by_name() {
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
        let checks = refusal(r#"checks.run=[{ command = ["true"], name = "true" }]"#);
        assert!(checks.contains("--check and --set checks.run"), "{checks}");
        let table = refusal(r#"merge={ mode = "ask" }"#);
        assert!(table.contains("--merge and --set merge.mode"), "{table}");
    }

    #[test]
    fn other_settings_are_left_to_resolving() {
        let overrides = [
            "ui.bell=false".to_owned(),
            "limits.per_task_usd=2.0".to_owned(),
            "approvals.merge.mode=auto".to_owned(),
        ];
        assert!(refuse_safety_flags(&overrides).is_ok());
    }

    #[test]
    fn a_forced_mode_skips_the_rules() {
        let command = RunCommand {
            task: "Add a flag".to_owned(),
            mode: Some(RunMode::Crew),
            overrides: vec!["ui.bell=false".to_owned()],
            ..RunCommand::default()
        };
        let request = start_request(&command, &Settings::default());
        assert_eq!(request.mode, RunMode::Crew);
        assert!(request.forced);
        assert!(request.captain_or_crew.is_none());
        assert_eq!(request.overrides, vec!["ui.bell=false"]);
        assert_eq!(request.approval_mode, None);
    }

    #[test]
    fn without_a_mode_the_captain_or_crew_settings_decide() {
        let request = start_request(&RunCommand::default(), &Settings::default());
        assert!(!request.forced);
        let captain_or_crew = request.captain_or_crew.unwrap();
        assert_eq!(
            (captain_or_crew.mode, captain_or_crew.move_to_crew),
            (RunMode::Captain, true)
        );
        let mut forced = Settings::default();
        forced.captain_or_crew.force = Some(ForcedMode::Crew);
        assert_eq!(decision_without_estimate(&forced).mode, RunMode::Crew);
    }

    /// The settings `argonauts run` resolves for a fresh repo, with `flags` as `--set` and
    /// `global` as the global settings file.
    fn settings_with(flags: &[&str], global: &str) -> AppSettings {
        let repo = TempRepo::new().unwrap();
        let paths = Paths::new(repo.home());
        let config = paths.global_config();
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, global).unwrap();
        crate::controller::test_support::set_up_with_council(&config);
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

    /// Ultracode sends a task from the command line to the crew as the conversation's
    /// Captain-or-crew choice does, whether a file or `--set` turns it on. `--captain` still runs
    /// it as Captain, and so does the Captain workflow, whose CLI runs its own.
    #[test]
    fn ultracode_sends_a_command_line_task_to_the_crew() {
        let command = RunCommand::default();
        let off = start_request(&command, settings_with(&[], "").settings());
        let captain_or_crew = off.captain_or_crew.unwrap();
        assert_eq!(
            (off.mode, captain_or_crew.rule.as_str()),
            (RunMode::Captain, "otherwise")
        );

        let forced_captain = "[captain_or_crew]\nforce = \"captain\"\n";
        let in_captain = start_request(
            &command,
            settings_with(&["ultracode=true"], forced_captain).settings(),
        );
        assert_eq!(in_captain.mode, RunMode::Captain);
        for settings in [
            settings_with(&["ultracode=true"], ""),
            settings_with(&[], "ultracode = true\n"),
        ] {
            let request = start_request(&command, settings.settings());
            assert_eq!(request.mode, RunMode::Crew);
            assert!(!request.forced);
            let captain_or_crew = request.captain_or_crew.unwrap();
            assert_eq!(
                (
                    captain_or_crew.rule.as_str(),
                    captain_or_crew.reason.as_str()
                ),
                ("ultracode", "Ultracode is on")
            );
            assert!(!captain_or_crew.move_to_crew);

            let captain = RunCommand {
                mode: Some(RunMode::Captain),
                ..RunCommand::default()
            };
            let request = start_request(&captain, settings.settings());
            assert_eq!((request.mode, request.forced), (RunMode::Captain, true));
        }
    }

    /// In Captain on Codex, which has no workflow of its own, ultracode runs a task from the
    /// command line as a workflow with every agent on Codex's model, as the conversation does.
    /// `--captain` still runs it alone, and so does ultracode off.
    #[test]
    fn ultracode_in_captain_on_codex_runs_a_workflow_on_its_model() {
        let captain_on_codex =
            "crew.main = \"codex/gpt-5.6-luna\"\n[captain_or_crew]\nforce = \"captain\"\n";
        let on = settings_with(&["ultracode=true"], captain_on_codex);
        let request = start_request(&RunCommand::default(), on.settings());
        assert_eq!((request.mode, request.forced), (RunMode::Crew, false));
        assert_eq!(request.captain_or_crew.unwrap().rule, "ultracode");
        assert_eq!(
            request.request_settings,
            ultra::same_model(on.settings()),
            "every role on the main agent's model"
        );
        assert!(
            request
                .request_settings
                .contains(&"crew.worker=[\"codex/gpt-5.6-luna\"]".to_owned()),
            "{:?}",
            request.request_settings
        );

        let alone = RunCommand {
            mode: Some(RunMode::Captain),
            ..RunCommand::default()
        };
        let request = start_request(&alone, on.settings());
        assert_eq!(request.mode, RunMode::Captain);
        assert!(request.request_settings.is_empty());

        let off = settings_with(&[], captain_on_codex);
        let request = start_request(&RunCommand::default(), off.settings());
        assert_eq!(request.mode, RunMode::Captain);
        assert!(request.request_settings.is_empty());
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
            ["run-20260930-101500-00ab started: Add a flag (captain)"]
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
            "run-20260930-101500-00ab started: Add a flag (captain)\n2 uncommitted files are left out \
             (intake.include_uncommitted includes them).\ncodex: done here\nFinished: branch_left.\n"
        );
    }

    #[tokio::test]
    async fn a_waiting_run_waits_for_you_at_its_approval() {
        let conductor = FakeConductor::default();
        conductor.set_runs(vec![summary(RunState::Waiting, "the plan approval")]);
        let waiting = event(RunEventKind::Run(RunLifecycle::Waiting {
            reason: WaitReason::Approval,
            waiting_on: Vec::new(),
        }));
        let (result, _) = follow_with(&conductor, vec![waiting.clone()], 0).await;
        assert_eq!(
            result.unwrap(),
            RunCommandExit::Waiting {
                run: run_id(),
                approval: "the plan approval".to_owned(),
            }
        );

        let unlisted = FakeConductor::default();
        let (result, _) = follow_with(&unlisted, vec![waiting], 0).await;
        assert_eq!(
            result.unwrap(),
            RunCommandExit::Waiting {
                run: run_id(),
                approval: "an approval".to_owned(),
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
