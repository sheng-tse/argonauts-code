//! `Approvals`: every call carries the run's token, and the app holds one only when a person is
//! at a terminal, `ARGONAUTS_SESSION` is unset, and no ancestor process is an agent.

use std::sync::Arc;

use argonauts_core::CardId;
use argonauts_core::RunId;
use argonauts_core::api::{ApiError, Approvals, CheckpointAnswer, TakeOverTicket};
use argonauts_core::control::ControlRequest;
use argonauts_core::future::BoxFuture;
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::{ApprovalMode, Presence};
use argonauts_registry::resolve;

use super::Client;
use super::control;
use crate::ancestry::{self, AgentMarkers};
use crate::app_settings;
use crate::env::{AppEnv, SESSION_ENV};
use crate::run_files;
use crate::trust::TrustStore;

pub(crate) struct AppApprovals {
    client: Arc<Client>,
}

impl AppApprovals {
    /// `None` when [`approvals_refusal`] refuses.
    pub(crate) fn for_app(client: Arc<Client>) -> Option<Arc<dyn Approvals>> {
        if approvals_refusal(&client.env).is_some() {
            return None;
        }
        Some(Arc::new(Self { client }))
    }

    /// Checked again on every call, since the settings or the process tree can change while
    /// the app is open.
    fn allowed(&self) -> Result<(), ApiError> {
        match approvals_refusal(&self.client.env) {
            Some(why) => Err(ApiError::Refused(why)),
            None => Ok(()),
        }
    }

    fn token(&self, run: &RunId) -> Result<String, ApiError> {
        let run_dir = self.client.env.run_dir(run);
        if !run_dir.is_dir() {
            return Err(ApiError::NoRun(run.clone()));
        }
        run_files::read_token(&run_dir)
            .map_err(|err| ApiError::Refused(format!("run {run} has no approval token: {err}")))
    }

    async fn send(
        &self,
        run: &RunId,
        request: impl FnOnce(String) -> ControlRequest,
    ) -> Result<control::Reply, ApiError> {
        self.allowed()?;
        let token = self.token(run)?;
        control::call(&self.client, run, request(token)).await
    }
}

/// Why this process may not approve anything, or `None` when it may: no terminal,
/// `ARGONAUTS_SESSION` set, an agent ancestor, or `approvals.presence = "required"` before the
/// passkey flow exists.
pub(crate) fn approvals_refusal(env: &AppEnv) -> Option<String> {
    approving_settings(env).err()
}

/// The settings a person approving here works under, or why this process may not approve.
pub(crate) fn approving_settings(env: &AppEnv) -> Result<Settings, String> {
    if !env.interactive {
        return Err("approving needs you at a terminal".to_owned());
    }
    if env.agent_session {
        return Err(format!(
            "{SESSION_ENV} is set, so this runs inside an agent session, which can't approve"
        ));
    }
    let trust = TrustStore::load(&env.state_dir);
    let settings = match resolve(&app_settings::sources(env, &trust, Vec::new(), true)) {
        Ok(resolved) => resolved.settings,
        Err(err) => return Err(format!("can't read your settings: {err}")),
    };
    if settings.approvals.presence == Presence::Required {
        return Err(
            "approvals.presence is \"required\", which needs passkey approval in the local web \
             UI, and that isn't available yet; set it to \"warn\" to approve here"
                .to_owned(),
        );
    }
    ancestry::check_self(&env.paths, &AgentMarkers::from_settings(&settings))
        .map_err(|err| format!("{err:#}"))?;
    Ok(settings)
}

impl Approvals for AppApprovals {
    fn answer(&self, run: &RunId, answer: CheckpointAnswer) -> BoxFuture<'_, Result<(), ApiError>> {
        let run = run.clone();
        Box::pin(async move {
            self.send(&run, |token| ControlRequest::Answer { answer, token })
                .await?
                .ok()
        })
    }

    fn set_approval_mode(
        &self,
        run: &RunId,
        mode: ApprovalMode,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        let run = run.clone();
        Box::pin(async move {
            self.send(&run, |token| ControlRequest::ApprovalMode { mode, token })
                .await?
                .ok()
        })
    }

    fn reassign_overriding(
        &self,
        run: &RunId,
        card: &CardId,
        to: WorkerRef,
        rule: String,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        let run = run.clone();
        let card = card.clone();
        Box::pin(async move {
            self.send(&run, |token| ControlRequest::Reassign {
                card,
                to,
                rule_override: Some(rule),
                token: Some(token),
            })
            .await?
            .ok()
        })
    }

    fn take_over(
        &self,
        run: &RunId,
        card: &CardId,
    ) -> BoxFuture<'_, Result<TakeOverTicket, ApiError>> {
        let run = run.clone();
        let card = card.clone();
        Box::pin(async move {
            self.send(&run, |token| ControlRequest::TakeOver { card, token })
                .await?
                .take_over()
        })
    }

    fn take_over_ended(&self, run: &RunId, card: &CardId) -> BoxFuture<'_, Result<(), ApiError>> {
        let run = run.clone();
        let card = card.clone();
        Box::pin(async move {
            self.send(&run, |token| ControlRequest::TakeOverEnded { card, token })
                .await?
                .ok()
        })
    }
}
