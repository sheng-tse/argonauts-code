//! A direct OpenAI-compatible API (OpenRouter, Ollama, LM Studio, vLLM) as a `Worker`.
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
use argonauts_core::limits::{HealthBoard, QuotaBoard};
use argonauts_core::names::{VendorId, WorkerRef};
use argonauts_core::registry::ModelRegistry;
use argonauts_core::settings::agents::{EndpointSettings, StructuredMode};
use argonauts_core::worker::{
    SessionHandle, SessionSpec, SteerSupport, SubagentVisibility, Worker, WorkerCapabilities,
    WorkerError, WorkerKind,
};
use tokio::sync::Semaphore;

pub struct EndpointWorker {
    id: String,
    settings: EndpointSettings,
    worker: WorkerRef,
    family: String,
    registry: Arc<dyn ModelRegistry>,
    quota: Arc<dyn QuotaBoard>,
    health: Arc<dyn HealthBoard>,
    /// Built on first use and shared, so sessions reuse connections.
    client: OnceLock<reqwest::Client>,
    /// `max_concurrency` requests at a time, across every session of this worker.
    slots: Arc<Semaphore>,
}

impl EndpointWorker {
    /// `id` is the `[endpoints.<id>]` key and `worker` names the model. `settings` must be
    /// `argonauts_registry::endpoint_settings(settings, id)`, your table merged over the preset,
    /// since only that fills in a preset's `base_url` and key variable.
    pub fn new(
        id: &str,
        settings: &EndpointSettings,
        worker: WorkerRef,
        family: String,
        registry: Arc<dyn ModelRegistry>,
        quota: Arc<dyn QuotaBoard>,
        health: Arc<dyn HealthBoard>,
    ) -> Self {
        Self {
            id: id.to_owned(),
            settings: settings.clone(),
            worker,
            family,
            registry,
            quota,
            health,
            client: OnceLock::new(),
            slots: Arc::new(Semaphore::new(settings.max_concurrency.max(1) as usize)),
        }
    }

    pub fn settings(&self) -> &EndpointSettings {
        &self.settings
    }

    fn vendor_id(&self) -> VendorId {
        VendorId(self.id.clone())
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

impl Worker for EndpointWorker {
    fn worker_ref(&self) -> &WorkerRef {
        &self.worker
    }

    fn kind(&self) -> WorkerKind {
        WorkerKind::Endpoint
    }

    fn vendor(&self) -> VendorId {
        self.vendor_id()
    }

    fn family(&self) -> String {
        self.family.clone()
    }

    fn capabilities(&self) -> WorkerCapabilities {
        WorkerCapabilities {
            streams_text: true,
            steer: SteerSupport::NextTurnOnly,
            resume: false,
            structured_output: self.settings.structured_output == StructuredMode::JsonSchema,
            subagents: SubagentVisibility::None,
            native_budget_cap: false,
            takeover: false,
            can_build: false,
            status_tool: false,
        }
    }

    fn start(&self, spec: SessionSpec) -> BoxFuture<'_, Result<SessionHandle, WorkerError>> {
        Box::pin(session::start(self, spec))
    }

    /// Endpoints have no interactive UI to take over.
    fn takeover_command(&self, vendor_session: &str, workdir: &Path) -> Option<CommandSpec> {
        let _ = (vendor_session, workdir);
        None
    }
}
