//! `start` and `continue_run`: the run directory, `start.json`, `settings.json`, the recipe the
//! run uses, the approval token, and a detached supervisor.

use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use argonauts_core::RunId;
use argonauts_core::api::{ApiError, RunHandle, StartRun};
use argonauts_core::events::RunEventKind;
use argonauts_core::events::run::{RunLifecycle, RunOutcome};
use argonauts_core::journal::RunLock;
use argonauts_core::layout::run_files as names;
use argonauts_core::paths::create_private_dir_all;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_registry::harnesses::agents_turned_on;
use argonauts_registry::{RecipeOverlay, ResolvedSettings, resolve};
use time::OffsetDateTime;

use super::Client;
use super::journal_ends;
use super::socket::Connection;
use crate::app_settings;
use crate::learning::{self, recipe::ForRun};
use crate::run_files::{self, SettingsRecord, StartRecord};
use crate::trust::TrustStore;

/// How long a new supervisor gets to answer on its socket.
pub(crate) const WELCOME_TIMEOUT: Duration = Duration::from_secs(10);
const WELCOME_POLL: Duration = Duration::from_millis(50);
/// How much of the supervisor log's last line an error quotes.
const LAST_LINE_CHARS: usize = 300;

/// Refuses an `approval_mode` above `Supervised`. Returns once the supervisor's `Welcome`
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
    // The recipe triage matched, at the version that runs now, over the preset.
    let recipe = request.estimate.as_ref().and_then(|estimate| {
        learning::recipe::matched(
            &learning::store(&client.env),
            &resolved.settings,
            &estimate.task_type,
            &estimate.risk_labels,
        )
    });
    let overlay = recipe.as_ref().and_then(learning::recipe::overlay);
    let recipe = recipe.filter(|_| overlay.is_some());
    let resolved = match overlay {
        Some(overlay) => with_request(client, &request, Some(overlay))?,
        None => resolved,
    };
    start_resolved(client, request, resolved, recipe.as_ref()).await
}

/// Starts a run with settings already resolved, recording `recipe` with it. The replay worker
/// comes here with a candidate version's settings for a run in an isolated repo.
pub(crate) async fn start_resolved(
    client: &Client,
    request: StartRun,
    resolved: ResolvedSettings,
    recipe: Option<&ForRun>,
) -> Result<RunHandle, ApiError> {
    let (run, run_dir) = new_run_dir(client)?;
    if let Some(recipe) = recipe {
        learning::recipe::record(&run_dir, recipe).map_err(supervisor_error)?;
    }
    let record = StartRecord {
        run: run.clone(),
        overrides: request.overrides.clone(),
        request,
        interactive: client.interactive,
        replay: recipe.is_some_and(|recipe| recipe.replay.is_some()),
        repo_root: client.env.repo_root.clone(),
        created: OffsetDateTime::now_utc(),
    };
    run_files::write_start(&run_dir, &record).map_err(supervisor_error)?;
    let settings = SettingsRecord {
        turned_on: agents_turned_on(&resolved),
        sha256: resolved.sha256,
        settings: resolved.settings,
    };
    run_files::write_settings(&run_dir, &settings).map_err(supervisor_error)?;
    let token = run_files::new_token().map_err(supervisor_error)?;
    run_files::write_token(&run_dir, &token).map_err(supervisor_error)?;
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

/// Starts the run's supervisor and waits for its `Welcome`.
pub(crate) async fn launch(client: &Client, run: &RunId, run_dir: &Path) -> Result<(), ApiError> {
    client
        .launcher
        .launch(&client.env.repo_root, run, run_dir)
        .map_err(supervisor_error)?;
    await_welcome(client, run_dir, WELCOME_TIMEOUT).await
}

/// Polls the run's socket until a supervisor greets. Fails when `timeout` passes, or at once
/// when the supervisor just launched for this run exits first.
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
        let log = run_dir.join(names::SUPERVISOR_LOG);
        if let Some(status) = client.launcher.failed(run_dir) {
            let last = last_line(&log);
            return Err(ApiError::Supervisor(format!(
                "the run's supervisor {status} before it answered{}; see {}",
                last.map(|line| format!(": {line}")).unwrap_or_default(),
                client.env.display(&log),
            )));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(ApiError::Supervisor(format!(
                "the run's supervisor didn't answer within {} s; see {}",
                timeout.as_secs(),
                client.env.display(&log),
            )));
        }
        tokio::time::sleep(WELCOME_POLL).await;
    }
}

/// Waits for the supervisor that holds the run's lock to greet. False when it lets go of the
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
            let log = client.env.display(&run_dir.join(names::SUPERVISOR_LOG));
            return Err(ApiError::Supervisor(format!(
                "the run's supervisor holds its lock but didn't answer within {} s; see {log}",
                WELCOME_TIMEOUT.as_secs()
            )));
        }
        tokio::time::sleep(WELCOME_POLL).await;
    }
}

async fn greeted(client: &Client, run_dir: &Path) -> Result<bool, ApiError> {
    Ok(Connection::connect(run_dir, client.kind)
        .await
        .map_err(supervisor_error)?
        .is_some())
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

/// Your layers and `overrides`, with `recipe` in the preset layer.
pub(crate) fn resolve_settings(
    client: &Client,
    overrides: &[String],
    recipe: Option<RecipeOverlay>,
) -> Result<ResolvedSettings, ApiError> {
    let trust = TrustStore::load(&client.env.state_dir);
    let mut sources =
        app_settings::sources(&client.env, &trust, overrides.to_vec(), client.interactive);
    sources.recipe = recipe;
    resolve(&sources).map_err(|err| ApiError::Refused(err.to_string()))
}

/// Your layers, the request's `overrides`, `recipe` in the preset layer, and what the request
/// itself said about how to run it, above them all.
fn with_request(
    client: &Client,
    request: &StartRun,
    recipe: Option<RecipeOverlay>,
) -> Result<ResolvedSettings, ApiError> {
    let trust = TrustStore::load(&client.env.state_dir);
    let mut sources = app_settings::sources(
        &client.env,
        &trust,
        request.overrides.clone(),
        client.interactive,
    );
    sources.recipe = recipe;
    sources.request = request.request_settings.clone();
    resolve(&sources).map_err(|err| ApiError::Refused(err.to_string()))
}

/// Ids carry the second they were made, so two starts in one second differ only by the
/// random suffix; a clash just draws again.
fn new_run_dir(client: &Client) -> Result<(RunId, PathBuf), ApiError> {
    create_private_dir_all(&client.env.runs_dir()).map_err(supervisor_error)?;
    for _ in 0..8 {
        let run = RunId::generate();
        let run_dir = client.env.run_dir(&run);
        match DirBuilder::new().mode(0o700).create(&run_dir) {
            Ok(()) => return Ok((run, run_dir)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(supervisor_error(err)),
        }
    }
    Err(ApiError::Supervisor(
        "couldn't pick a run id that isn't taken".to_owned(),
    ))
}

pub(crate) fn supervisor_error(err: impl std::fmt::Display) -> ApiError {
    ApiError::Supervisor(format!("{err:#}"))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::Arc;

    use super::super::ProcessLauncher;
    use super::super::test_support::TestHome;
    use super::*;

    #[tokio::test]
    async fn a_supervisor_that_exits_early_fails_the_wait_at_once() {
        let home = TestHome::new();
        let bin = tempfile::tempdir().unwrap();
        let script = bin.path().join("failing-argonauts");
        std::fs::write(
            &script,
            "#!/bin/sh\necho \"argonauts: this run can't be hosted\" >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let client = Client::new(
            home.env(true),
            Arc::new(ProcessLauncher::with_binary(script)),
            true,
        );
        let run = RunId::new("run-20260930-010000-0e01").unwrap();
        let run_dir = home.run_with(&client.env, &run, &[]);

        let began = std::time::Instant::now();
        let started = launch(&client, &run, &run_dir).await;
        let Err(ApiError::Supervisor(message)) = started else {
            panic!("expected the early exit, got {started:?}");
        };
        assert!(
            began.elapsed() < WELCOME_TIMEOUT / 2,
            "{:?}",
            began.elapsed()
        );
        assert!(message.contains("exited"), "{message}");
        assert!(message.contains("this run can't be hosted"), "{message}");
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
