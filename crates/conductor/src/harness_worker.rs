//! One adapter plus model behind the core `Worker` interface. A placeholder for now: it refuses
//! to start a session, and `SoloRun` still runs solo sessions through the `Supervisor`.

use std::path::Path;
use std::sync::Arc;

use argonauts_core::future::BoxFuture;
use argonauts_core::limits::{HealthBoard, QuotaBoard};
use argonauts_core::names::{VendorId, WorkerRef};
use argonauts_core::registry::ModelRegistry;
use argonauts_core::settings::Settings;
use argonauts_core::worker::{
    SessionHandle, SessionSpec, SteerSupport, SubagentVisibility, Worker, WorkerCapabilities,
    WorkerError, WorkerKind,
};
use argonauts_core::{Adapter, CommandSpec};

pub struct HarnessWorker {
    adapter: Arc<dyn Adapter>,
    worker: WorkerRef,
    family: String,
    settings: Arc<Settings>,
    quota: Arc<dyn QuotaBoard>,
    health: Arc<dyn HealthBoard>,
    registry: Option<Arc<dyn ModelRegistry>>,
}

impl HarnessWorker {
    pub fn new(
        adapter: Arc<dyn Adapter>,
        worker: WorkerRef,
        family: String,
        settings: Arc<Settings>,
        quota: Arc<dyn QuotaBoard>,
        health: Arc<dyn HealthBoard>,
    ) -> Self {
        Self {
            adapter,
            worker,
            family,
            settings,
            quota,
            health,
            registry: None,
        }
    }

    /// Prices the meter's usage from the registry's entry for this worker. Without one, the
    /// meter uses the vendor's reported cost or `agents.<id>.price`.
    pub fn with_registry(mut self, registry: Arc<dyn ModelRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    pub fn adapter(&self) -> &Arc<dyn Adapter> {
        &self.adapter
    }
}

impl Worker for HarnessWorker {
    fn worker_ref(&self) -> &WorkerRef {
        &self.worker
    }

    fn kind(&self) -> WorkerKind {
        WorkerKind::Harness
    }

    fn vendor(&self) -> VendorId {
        self.worker.vendor()
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
        let _ = (
            spec,
            &self.settings,
            &self.quota,
            &self.health,
            &self.registry,
        );
        let name = self.adapter.display_name();
        Box::pin(async move {
            Err(WorkerError::Refused(format!(
                "the {name} harness worker is not implemented yet"
            )))
        })
    }

    fn takeover_command(&self, vendor_session: &str, workdir: &Path) -> Option<CommandSpec> {
        self.adapter.interactive_resume(vendor_session, workdir)
    }
}
