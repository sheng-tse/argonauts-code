//! `start` and `continue_run`: the run directory, `start.json`, `settings.json`, the learned
//! workflow the run uses, the approval token, and a detached run host.

use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use argonauts_core::RunId;
use argonauts_core::base::clock;
use argonauts_core::base::layout::run_files as names;
use argonauts_core::base::paths::create_private_dir_all;
use argonauts_core::runs::api::{ApiError, RunHandle, StartRun};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::run::{RunLifecycle, RunOutcome};
use argonauts_core::runs::journal::RunLock;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_registry::clis::agents_turned_on;
use argonauts_registry::{LearnedWorkflowOverlay, ResolvedSettings, resolve};

use super::Client;
use super::journal_ends;
use super::socket::Connection;
use crate::host::run_files::{self, SettingsRecord, StartRecord};
use crate::learning::{self, learned_workflow::ForRun};
use crate::startup::app_settings;
use crate::startup::trust::TrustStore;

/// How long a new run host gets to answer on its socket.
pub(crate) const WELCOME_TIMEOUT: Duration = Duration::from_secs(10);
const WELCOME_POLL: Duration = Duration::from_millis(50);
/// How much of the run host log's last line an error quotes.
const LAST_LINE_CHARS: usize = 300;

/// Refuses an `approval_mode` above `Supervised`. Returns once the run host's `Welcome`
/// answers on the socket. Not cancel-safe: dropped midway, it leaves a run directory that
/// `list` shows as crashed.
pub(crate) async fn start(client: &Client, request: StartRun) -> Result<RunHandle, ApiError> {
    if request
        .approval_mode
        .is_some_and(|mode| mode != ApprovalMode::Supervised)
    {
        return Err(ApiError::Refused(
            "a run starts supervised; raise its approval mode from the app once it runs".to_owned(),
        ));
    }
    let resolved = resolve_settings(client, &request.overrides, None)?;
    let resolved = if request.request_settings.is_empty() {
        resolved
    } else {
        with_request(client, &request, None)?
    };
    // The learned workflow the Captain-or-crew choice matched, at the version that runs now, over
    // the workflow.
    let learned_workflow = request.estimate.as_ref().and_then(|estimate| {
        learning::learned_workflow::matched(
            &learning::store(&client.env),
            &resolved.settings,
            &estimate.task_type,
            &estimate.risk_labels,
        )
    });
    let overlay = learned_workflow
        .as_ref()
        .and_then(learning::learned_workflow::overlay);
    let learned_workflow = learned_workflow.filter(|_| overlay.is_some());
    let resolved = match overlay {
        Some(overlay) => with_request(client, &request, Some(overlay))?,
        None => resolved,
    };
    start_resolved(client, request, resolved, learned_workflow.as_ref()).await
}

/// Starts a run with settings already resolved, recording `learned_workflow` with it. The replay
/// worker comes here with a candidate version's settings for a run in an isolated repo.
pub(crate) async fn start_resolved(
    client: &Client,
    request: StartRun,
    resolved: ResolvedSettings,
    learned_workflow: Option<&ForRun>,
) -> Result<RunHandle, ApiError> {
    let (run, run_dir) = new_run_dir(client)?;
    if let Some(learned_workflow) = learned_workflow {
        learning::learned_workflow::record(&run_dir, learned_workflow).map_err(run_host_error)?;
    }
    let record = StartRecord {
        run: run.clone(),
        overrides: request.overrides.clone(),
        request,
        interactive: client.interactive,
        replay: learned_workflow.is_some_and(|learned_workflow| learned_workflow.replay.is_some()),
        repo_root: client.env.repo_root.clone(),
        created: clock::now_utc(),
    };
    run_files::write_start(&run_dir, &record).map_err(run_host_error)?;
    let settings = SettingsRecord {
        turned_on: agents_turned_on(&resolved),
        sha256: resolved.sha256,
        settings: resolved.settings,
    };
    run_files::write_settings(&run_dir, &settings).map_err(run_host_error)?;
    let token = run_files::new_token().map_err(run_host_error)?;
    run_files::write_token(&run_dir, &token).map_err(run_host_error)?;
    launch(client, &run, &run_dir).await?;
    Ok(RunHandle {
        events: run_dir.join(names::EVENTS),
        run,
    })
}

/// A follow-up run whose `continues` names `merged`. Refuses a run that isn't merged.
pub(crate) async fn continue_run(
    client: &Client,
    merged: &RunId,
    mut request: StartRun,
) -> Result<RunHandle, ApiError> {
    let run_dir = client.env.run_dir(merged);
    if !run_dir.is_dir() {
        return Err(ApiError::NoRun(merged.clone()));
    }
    if outcome(&run_dir) != Some(RunOutcome::Merged) {
        return Err(ApiError::Refused(format!(
            "run {merged} isn't merged; /resume carries it on"
        )));
    }
    request.continues = Some(merged.clone());
    start(client, request).await
}

/// Starts the run's run host and waits for its `Welcome`.
pub(crate) async fn launch(client: &Client, run: &RunId, run_dir: &Path) -> Result<(), ApiError> {
    client
        .launcher
        .launch(&client.env.repo_root, run, run_dir)
        .map_err(run_host_error)?;
    await_welcome(client, run_dir, WELCOME_TIMEOUT).await
}

/// Polls the run's socket until a run host greets. Fails when `timeout` passes, or at once
/// when the run host just launched for this run exits first.
pub(crate) async fn await_welcome(
    client: &Client,
    run_dir: &Path,
    timeout: Duration,
) -> Result<(), ApiError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if greeted(client, run_dir).await? {
            return Ok(());
        }
        let log = run_dir.join(names::RUN_HOST_LOG);
        if let Some(status) = client.launcher.failed(run_dir) {
            return Err(match run_host_last_words(run_dir) {
                // Its own last words say why it couldn't go on.
                Some(why) => ApiError::Refused(why),
                None => ApiError::RunHost(format!(
                    "the run's run host {status} before it answered; see {}",
                    client.env.display(&log),
                )),
            });
        }
        // It took what it was launched for, a stop or a wake, did it and left: nothing is
        // left for it to say.
        if client.launcher.exited_cleanly(run_dir) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(ApiError::RunHost(format!(
                "the run's run host didn't answer within {} s; see {}",
                timeout.as_secs(),
                client.env.display(&log),
            )));
        }
        tokio::time::sleep(WELCOME_POLL).await;
    }
}

/// Waits for the run host that holds the run's lock to greet. False when it lets go of the
/// lock without answering, as one does on its way out, so the caller starts the next one.
pub(crate) async fn await_holder(client: &Client, run_dir: &Path) -> Result<bool, ApiError> {
    let deadline = tokio::time::Instant::now() + WELCOME_TIMEOUT;
    loop {
        if greeted(client, run_dir).await? {
            return Ok(true);
        }
        if !RunLock::is_held(run_dir) {
            // It may have bound its socket just before letting go of the lock.
            return greeted(client, run_dir).await;
        }
        if tokio::time::Instant::now() >= deadline {
            let log = client.env.display(&run_dir.join(names::RUN_HOST_LOG));
            return Err(ApiError::RunHost(format!(
                "the run's run host holds its lock but didn't answer within {} s; see {log}",
                WELCOME_TIMEOUT.as_secs()
            )));
        }
        tokio::time::sleep(WELCOME_POLL).await;
    }
}

async fn greeted(client: &Client, run_dir: &Path) -> Result<bool, ApiError> {
    Ok(Connection::connect(run_dir, client.kind)
        .await
        .map_err(run_host_error)?
        .is_some())
}

/// What the run's run host said last before it exited, without the program's name in front.
pub(crate) fn run_host_last_words(run_dir: &Path) -> Option<String> {
    let line = last_line(&run_dir.join(names::RUN_HOST_LOG))?;
    let words = line.strip_prefix("argonauts: ").unwrap_or(&line).trim();
    (!words.is_empty()).then(|| words.to_owned())
}

/// The log's last line, cut short, with control characters dropped.
fn last_line(log: &Path) -> Option<String> {
    let text = std::fs::read_to_string(log).ok()?;
    let line = text.lines().rev().find(|line| !line.trim().is_empty())?;
    Some(
        line.chars()
            .filter(|c| !c.is_control())
            .take(LAST_LINE_CHARS)
            .collect(),
    )
}

/// How the run finished, if its last event says so.
pub(crate) fn outcome(run_dir: &Path) -> Option<RunOutcome> {
    match journal_ends::last_event(&run_dir.join(names::EVENTS))?.kind {
        RunEventKind::Run(RunLifecycle::Finished { outcome, .. }) => Some(outcome),
        _ => None,
    }
}

/// Your layers and `overrides`, with `learned_workflow` in the workflow layer.
pub(crate) fn resolve_settings(
    client: &Client,
    overrides: &[String],
    learned_workflow: Option<LearnedWorkflowOverlay>,
) -> Result<ResolvedSettings, ApiError> {
    let trust = TrustStore::load(&client.env.state_dir);
    let mut sources =
        app_settings::sources(&client.env, &trust, overrides.to_vec(), client.interactive);
    sources.learned_workflow = learned_workflow;
    resolve(&sources).map_err(|err| ApiError::Refused(err.to_string()))
}

/// Your layers, the request's `overrides`, `learned_workflow` in the workflow layer, and what the
/// request itself said about how to run it, above them all.
fn with_request(
    client: &Client,
    request: &StartRun,
    learned_workflow: Option<LearnedWorkflowOverlay>,
) -> Result<ResolvedSettings, ApiError> {
    let trust = TrustStore::load(&client.env.state_dir);
    let mut sources = app_settings::sources(
        &client.env,
        &trust,
        request.overrides.clone(),
        client.interactive,
    );
    sources.learned_workflow = learned_workflow;
    sources.request = request.request_settings.clone();
    resolve(&sources).map_err(|err| ApiError::Refused(err.to_string()))
}

/// Ids carry the second they were made, so two starts in one second are different only by the
/// random suffix; a clash just draws again.
fn new_run_dir(client: &Client) -> Result<(RunId, PathBuf), ApiError> {
    create_private_dir_all(&client.env.runs_dir()).map_err(run_host_error)?;
    for _ in 0..8 {
        let run = RunId::generate();
        let run_dir = client.env.run_dir(&run);
        match DirBuilder::new().mode(0o700).create(&run_dir) {
            Ok(()) => return Ok((run, run_dir)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(run_host_error(err)),
        }
    }
    Err(ApiError::RunHost(
        "couldn't pick a run id that isn't taken".to_owned(),
    ))
}

pub(crate) fn run_host_error(err: impl std::fmt::Display) -> ApiError {
    ApiError::RunHost(format!("{err:#}"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::ProcessLauncher;
    use super::super::test_support::TestHome;
    use super::*;

    #[tokio::test]
    async fn a_run_host_that_exits_early_fails_the_wait_at_once() {
        let home = TestHome::new();
        let bin = tempfile::tempdir().unwrap();
        let script = bin.path().join("failing-argonauts");
        argonauts_testkit::write_script(
            &script,
            "#!/bin/sh\necho \"argonauts: this run can't be hosted\" >&2\nexit 1\n",
        )
        .unwrap();
        let client = Client::new(
            home.env(true),
            Arc::new(ProcessLauncher::with_binary(script)),
            true,
        );
        let run = RunId::new("run-20260930-010000-0e01").unwrap();
        let run_dir = home.run_with(&client.env, &run, &[]);

        let began = std::time::Instant::now();
        let started = launch(&client, &run, &run_dir).await;
        // Its own last words are the reason, with nothing of the plumbing around them.
        assert_eq!(
            started,
            Err(ApiError::Refused("this run can't be hosted".to_owned()))
        );
        assert!(
            began.elapsed() < WELCOME_TIMEOUT / 2,
            "{:?}",
            began.elapsed()
        );
    }

    #[tokio::test]
    async fn a_run_host_that_exits_without_a_word_points_at_its_log() {
        let home = TestHome::new();
        let bin = tempfile::tempdir().unwrap();
        let script = bin.path().join("silent-argonauts");
        argonauts_testkit::write_script(&script, "#!/bin/sh\nexit 1\n").unwrap();
        let client = Client::new(
            home.env(true),
            Arc::new(ProcessLauncher::with_binary(script)),
            true,
        );
        let run = RunId::new("run-20260930-010000-0e02").unwrap();
        let run_dir = home.run_with(&client.env, &run, &[]);

        let Err(ApiError::RunHost(message)) = launch(&client, &run, &run_dir).await else {
            panic!("expected the early exit");
        };
        assert!(message.contains("exited"), "{message}");
        assert!(message.contains(names::RUN_HOST_LOG), "{message}");
    }

    #[tokio::test]
    async fn waiting_on_a_lock_holder_stops_when_it_lets_go() {
        let home = TestHome::new();
        let client = Arc::new(Client::new(
            home.env(true),
            Arc::new(ProcessLauncher::with_binary("/bin/false".into())),
            true,
        ));
        let run = RunId::new("run-20260930-010000-0e02").unwrap();
        let run_dir = home.run_with(&client.env, &run, &[]);
        let lock = RunLock::try_acquire(&run_dir).unwrap().unwrap();
        let waiting = {
            let client = client.clone();
            let run_dir = run_dir.clone();
            tokio::spawn(async move { await_holder(&client, &run_dir).await })
        };
        tokio::time::sleep(WELCOME_POLL * 3).await;
        drop(lock);
        let answered = tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .expect("the wait ends once the lock is free")
            .unwrap();
        assert_eq!(answered, Ok(false));
    }
}
