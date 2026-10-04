//! `ConductorApi` and `Approvals` over the file system and each run's control socket. A control
//! call to a run with no live supervisor goes to `pending-answers.jsonl` under the run lock, and
//! a new supervisor picks it up.

mod approvals;
mod control;
mod journal_ends;
mod launch;
mod list;
pub(crate) mod socket;
mod start;
mod stream;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::Arc;

use argonauts_core::api::{
    ApiError, Approvals, ConductorApi, DiffScope, DiffView, EventStream, RawView, RunHandle,
    RunSummary, StartRun, StatusSummary, SteerAck,
};
use argonauts_core::control::{ClientKind, ControlRequest};
use argonauts_core::events::goal::GoalAction;
use argonauts_core::events::session::QueuedMessage;
use argonauts_core::future::BoxFuture;
use argonauts_core::input::MentionTarget;
use argonauts_core::names::{CheckpointId, MessageId, SessionKey, WorkerRef};
use argonauts_core::{CardId, Paths, RunId};

use crate::env::AppEnv;

pub(crate) use approvals::AppApprovals;
pub(crate) use journal_ends::last_event;
pub(crate) use launch::{Launcher, ProcessLauncher};
pub(crate) use list::summarize;
pub(crate) use start::{resolve_settings, start_resolved};
pub(crate) use stream::supervisor_crashed;

pub(crate) struct Client {
    pub(crate) env: Arc<AppEnv>,
    pub(crate) launcher: Arc<dyn Launcher>,
    /// A person at a terminal vetted this client's overrides. False for the CLI and MCP.
    pub(crate) interactive: bool,
    /// What `Hello` tells the supervisor this client is.
    pub(crate) kind: ClientKind,
}

impl Client {
    pub(crate) fn new(env: Arc<AppEnv>, launcher: Arc<dyn Launcher>, interactive: bool) -> Self {
        Self {
            env,
            launcher,
            interactive,
            kind: ClientKind::App,
        }
    }
}

/// For the CLI and the MCP server: overrides resolve with `interactive = false`, and there is
/// no `Approvals`.
pub(crate) fn conductor(repo_root: &Path) -> anyhow::Result<Arc<dyn ConductorApi>> {
    let paths = Paths::from_env()
        .map_err(|_| anyhow::anyhow!("can't find your home directory; set ARGONAUTS_HOME"))?;
    let launcher = Arc::new(ProcessLauncher::current()?);
    Ok(Arc::new(conductor_client(paths, repo_root, launcher)?))
}

/// The client behind [`conductor`]. It never reads a run's approval token.
fn conductor_client(
    paths: Paths,
    repo_root: &Path,
    launcher: Arc<dyn Launcher>,
) -> anyhow::Result<Client> {
    let env = Arc::new(AppEnv::for_repo(paths, repo_root, false)?);
    Ok(Client {
        kind: ClientKind::Cli,
        ..Client::new(env, launcher, false)
    })
}

/// The app's conductor, and its `Approvals` when it may hold them.
pub(crate) type AppClient = (Arc<dyn ConductorApi>, Option<Arc<dyn Approvals>>);

/// The app's client, and `Approvals` only when a person is at a terminal and no agent is an
/// ancestor.
pub(crate) fn for_app(env: Arc<AppEnv>) -> anyhow::Result<AppClient> {
    let launcher: Arc<dyn Launcher> = Arc::new(ProcessLauncher::current()?);
    let client = Arc::new(Client::new(Arc::clone(&env), launcher, env.interactive));
    let approvals = AppApprovals::for_app(Arc::clone(&client));
    Ok((client, approvals))
}

impl ConductorApi for Client {
    /// Not cancel-safe: a caller that drops it midway leaves a run directory that `list` shows
    /// as crashed, and `/resume` recovers it.
    fn start(&self, request: StartRun) -> BoxFuture<'_, Result<RunHandle, ApiError>> {
        Box::pin(start::start(self, request))
    }

    fn subscribe(
        &self,
        run: &RunId,
        from_seq: u64,
    ) -> BoxFuture<'_, Result<EventStream, ApiError>> {
        let run = run.clone();
        Box::pin(async move { stream::subscribe(self, &run, from_seq, false).await })
    }

    fn reattach(&self, run: &RunId) -> BoxFuture<'_, Result<EventStream, ApiError>> {
        let run = run.clone();
        Box::pin(async move { stream::subscribe(self, &run, 0, true).await })
    }

    fn steer(
        &self,
        run: &RunId,
        session: &SessionKey,
        text: String,
    ) -> BoxFuture<'_, Result<SteerAck, ApiError>> {
        let request = ControlRequest::Steer {
            session: session.clone(),
            text,
        };
        let run = run.clone();
        Box::pin(async move { control::call(self, &run, request).await?.steered() })
    }

    fn mention(
        &self,
        run: &RunId,
        target: MentionTarget,
        text: String,
    ) -> BoxFuture<'_, Result<MessageId, ApiError>> {
        let run = run.clone();
        let request = ControlRequest::Mention { target, text };
        Box::pin(async move { control::call(self, &run, request).await?.queued() })
    }

    fn guidance(&self, run: &RunId, text: String) -> BoxFuture<'_, Result<MessageId, ApiError>> {
        let run = run.clone();
        let request = ControlRequest::Guidance {
            text,
            front_end: None,
        };
        Box::pin(async move { control::call(self, &run, request).await?.queued() })
    }

    fn guidance_from_front_end(
        &self,
        run: &RunId,
        text: String,
        client: String,
    ) -> BoxFuture<'_, Result<MessageId, ApiError>> {
        let run = run.clone();
        let request = ControlRequest::Guidance {
            text,
            front_end: Some(client),
        };
        Box::pin(async move { control::call(self, &run, request).await?.queued() })
    }

    fn take_back(
        &self,
        run: &RunId,
        messages: Vec<MessageId>,
    ) -> BoxFuture<'_, Result<Vec<QueuedMessage>, ApiError>> {
        let run = run.clone();
        let request = ControlRequest::TakeBack { messages };
        Box::pin(async move { control::call(self, &run, request).await?.taken_back() })
    }

    fn pause(&self, run: &RunId, now: bool) -> BoxFuture<'_, Result<(), ApiError>> {
        let run = run.clone();
        let request = ControlRequest::Pause {
            now,
            front_end: None,
        };
        Box::pin(async move { control::call(self, &run, request).await?.ok() })
    }

    fn pause_from_front_end(
        &self,
        run: &RunId,
        now: bool,
        client: String,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        let run = run.clone();
        let request = ControlRequest::Pause {
            now,
            front_end: Some(client),
        };
        Box::pin(async move { control::call(self, &run, request).await?.ok() })
    }

    fn cancel_pause(&self, run: &RunId) -> BoxFuture<'_, Result<(), ApiError>> {
        let run = run.clone();
        Box::pin(async move {
            control::call(self, &run, ControlRequest::CancelPause)
                .await?
                .ok()
        })
    }

    fn resume(&self, run: &RunId) -> BoxFuture<'_, Result<(), ApiError>> {
        let run = run.clone();
        Box::pin(async move { control::resume(self, &run).await })
    }

    fn stop(&self, run: &RunId) -> BoxFuture<'_, Result<(), ApiError>> {
        let run = run.clone();
        Box::pin(async move { control::call(self, &run, ControlRequest::Stop).await?.ok() })
    }

    fn stop_session(
        &self,
        run: &RunId,
        session: &SessionKey,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        let run = run.clone();
        let request = ControlRequest::StopSession {
            session: session.clone(),
        };
        Box::pin(async move { control::call(self, &run, request).await?.ok() })
    }

    fn reassign(
        &self,
        run: &RunId,
        card: &CardId,
        to: WorkerRef,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        let run = run.clone();
        let request = ControlRequest::Reassign {
            card: card.clone(),
            to,
            rule_override: None,
            token: None,
        };
        Box::pin(async move { control::call(self, &run, request).await?.ok() })
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<RunSummary>, ApiError>> {
        Box::pin(async move { list::list(self) })
    }

    fn continue_run(
        &self,
        merged: &RunId,
        request: StartRun,
    ) -> BoxFuture<'_, Result<RunHandle, ApiError>> {
        let merged = merged.clone();
        Box::pin(async move { start::continue_run(self, &merged, request).await })
    }

    fn status(&self, run: &RunId) -> BoxFuture<'_, Result<StatusSummary, ApiError>> {
        let run = run.clone();
        Box::pin(async move { control::status(self, &run).await })
    }

    fn diff(
        &self,
        run: &RunId,
        card: &CardId,
        scope: DiffScope,
    ) -> BoxFuture<'_, Result<DiffView, ApiError>> {
        let run = run.clone();
        let card = card.clone();
        Box::pin(async move { control::diff(self, &run, &card, scope).await })
    }

    fn raw(&self, run: &RunId, session: &SessionKey) -> BoxFuture<'_, Result<RawView, ApiError>> {
        let run = run.clone();
        let session = session.clone();
        Box::pin(async move { control::raw(self, &run, &session).await })
    }

    fn revert(
        &self,
        run: &RunId,
        card: &CardId,
        checkpoint: u32,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        let run = run.clone();
        let request = ControlRequest::Revert {
            card: card.clone(),
            checkpoint,
        };
        Box::pin(async move { control::call(self, &run, request).await?.ok() })
    }

    fn spot_check(
        &self,
        run: &RunId,
        id: &CheckpointId,
        correct: bool,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        let run = run.clone();
        let id = id.clone();
        Box::pin(async move { control::spot_check(self, &run, &id, correct).await })
    }

    fn goal(&self, run: &RunId, action: GoalAction) -> BoxFuture<'_, Result<(), ApiError>> {
        let run = run.clone();
        Box::pin(async move {
            control::call(self, &run, ControlRequest::Goal { action })
                .await?
                .ok()
        })
    }
}
