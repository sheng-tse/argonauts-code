//! A direct OpenAI-compatible API (OpenRouter, Ollama, LM Studio, vLLM) as a `Worker`.
//! Endpoints have no tools, so they only take `NoTools` sessions.

use std::path::Path;
use std::sync::Arc;

use argonauts_core::CommandSpec;
use argonauts_core::future::BoxFuture;
use argonauts_core::limits::{HealthBoard, QuotaBoard};
use argonauts_core::names::{VendorId, WorkerRef};
use argonauts_core::registry::ModelRegistry;
use argonauts_core::settings::agents::EndpointSettings;
use argonauts_core::worker::{
    SessionHandle, SessionSpec, SteerSupport, SubagentVisibility, Worker, WorkerCapabilities,
    WorkerError, WorkerKind,
};

pub struct EndpointWorker {
    id: String,
    settings: EndpointSettings,
    worker: WorkerRef,
    family: String,
    registry: Arc<dyn ModelRegistry>,
    quota: Arc<dyn QuotaBoard>,
    health: Arc<dyn HealthBoard>,
}

impl EndpointWorker {
    /// `id` is the `[endpoints.<id>]` key; `worker` names the model.
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
        }
    }

    pub fn settings(&self) -> &EndpointSettings {
        &self.settings
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
        VendorId(self.id.clone())
    }

    fn family(&self) -> String {
        self.family.clone()
    }

    fn capabilities(&self) -> WorkerCapabilities {
        WorkerCapabilities {
            streams_text: false,
            steer: SteerSupport::NextTurnOnly,
            resume: false,
            structured_output: false,
            subagents: SubagentVisibility::None,
            native_budget_cap: false,
            takeover: false,
            can_build: false,
            status_tool: false,
        }
    }

    fn start(&self, spec: SessionSpec) -> BoxFuture<'_, Result<SessionHandle, WorkerError>> {
        let _ = (spec, &self.registry, &self.quota, &self.health);
        let id = self.id.clone();
        Box::pin(async move {
            Err(WorkerError::Refused(format!(
                "the {id} endpoint worker is not implemented yet"
            )))
        })
    }

    /// Endpoints have no interactive UI to take over.
    fn takeover_command(&self, vendor_session: &str, workdir: &Path) -> Option<CommandSpec> {
        let _ = (vendor_session, workdir);
        None
    }
}
