//! A direct OpenAI-compatible API (OpenRouter, Ollama, LM Studio, vLLM) as a `SessionRunner`.
//! Endpoints have no tools, so they only take `NoTools` sessions.

mod errors;
mod raw_log;
#[cfg(test)]
mod replay;
mod request;
mod run;
mod session;
mod sse;
mod stream;
mod structured;

use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use argonauts_core::CommandSpec;
use argonauts_core::future::BoxFuture;
use argonauts_core::limits::{HealthBoard, PlanUsageBoard};
use argonauts_core::names::{AgentId, ModelRef};
use argonauts_core::registry::ModelRegistry;
use argonauts_core::session_runner::{
    RunnerCapabilities, RunnerError, RunnerKind, SessionHandle, SessionRunner, SessionSpec,
    SteerSupport, SubagentVisibility,
};
use argonauts_core::settings::agents::{EndpointSettings, StructuredMode};
use tokio::sync::Semaphore;

pub struct EndpointRunner {
    id: String,
    settings: EndpointSettings,
    worker: ModelRef,
    provider: String,
    registry: Arc<dyn ModelRegistry>,
    plan_usage: Arc<dyn PlanUsageBoard>,
    health: Arc<dyn HealthBoard>,
    /// Built on first use and shared, so sessions reuse connections.
    client: OnceLock<reqwest::Client>,
    /// `max_concurrency` requests at a time, across every session of this worker.
    slots: Arc<Semaphore>,
}

impl EndpointRunner {
    /// `id` is the `[endpoints.<id>]` key and `worker` names the model. `settings` must be
    /// `argonauts_registry::endpoint_settings(settings, id)`, your table merged over the known
    /// endpoint, since only that fills in a known endpoint's `base_url` and key variable.
    pub fn new(
        id: &str,
        settings: &EndpointSettings,
        worker: ModelRef,
        provider: String,
        registry: Arc<dyn ModelRegistry>,
        plan_usage: Arc<dyn PlanUsageBoard>,
        health: Arc<dyn HealthBoard>,
    ) -> Self {
        Self {
            id: id.to_owned(),
            settings: settings.clone(),
            worker,
            provider,
            registry,
            plan_usage,
            health,
            client: OnceLock::new(),
            slots: Arc::new(Semaphore::new(settings.max_concurrency.max(1) as usize)),
        }
    }

    pub fn settings(&self) -> &EndpointSettings {
        &self.settings
    }

    fn agent_id(&self) -> AgentId {
        AgentId(self.id.clone())
    }

    fn client(&self) -> Result<reqwest::Client, String> {
        if let Some(client) = self.client.get() {
            return Ok(client.clone());
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|err| format!("couldn't set up HTTP for the {} endpoint: {err}", self.id))?;
        Ok(self.client.get_or_init(|| client).clone())
    }
}

impl SessionRunner for EndpointRunner {
    fn model_ref(&self) -> &ModelRef {
        &self.worker
    }

    fn kind(&self) -> RunnerKind {
        RunnerKind::Endpoint
    }

    fn agent_id(&self) -> AgentId {
        self.agent_id()
    }

    fn provider(&self) -> String {
        self.provider.clone()
    }

    fn capabilities(&self) -> RunnerCapabilities {
        RunnerCapabilities {
            streams_text: true,
            steer: SteerSupport::NextTurnOnly,
            resume: false,
            fork: false,
            structured_output: self.settings.structured_output == StructuredMode::JsonSchema,
            subagents: SubagentVisibility::None,
            native_limit_cap: false,
            take_over: false,
            can_build: false,
            status_tool: false,
        }
    }

    fn start(&self, spec: SessionSpec) -> BoxFuture<'_, Result<SessionHandle, RunnerError>> {
        Box::pin(session::start(self, spec))
    }

    /// Endpoints have no interactive UI to take over.
    fn take_over_command(&self, cli_session: &str, workdir: &Path) -> Option<CommandSpec> {
        let _ = (cli_session, workdir);
        None
    }
}
