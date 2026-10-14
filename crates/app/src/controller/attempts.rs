//! A turn's worker that notices its step asking again. The step runner starts one session for
//! each try, so every start after the first means the answer before it didn't fit its format.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use argonauts_core::CommandSpec;
use argonauts_core::future::BoxFuture;
use argonauts_core::names::{VendorId, WorkerRef};
use argonauts_core::worker::{
    SessionHandle, SessionSpec, Worker, WorkerCapabilities, WorkerError, WorkerKind,
};

pub(crate) struct Attempts {
    inner: Arc<dyn Worker>,
    started: AtomicU32,
    again: Box<dyn Fn() + Send + Sync>,
}

impl Attempts {
    /// `inner`, calling `again` as each try after the first starts, before its session does.
    pub(crate) fn wrap(
        inner: Arc<dyn Worker>,
        again: impl Fn() + Send + Sync + 'static,
    ) -> Arc<dyn Worker> {
        Arc::new(Self {
            inner,
            started: AtomicU32::new(0),
            again: Box::new(again),
        })
    }
}

impl Worker for Attempts {
    fn worker_ref(&self) -> &WorkerRef {
        self.inner.worker_ref()
    }

    fn kind(&self) -> WorkerKind {
        self.inner.kind()
    }

    fn vendor(&self) -> VendorId {
        self.inner.vendor()
    }

    fn family(&self) -> String {
        self.inner.family()
    }

    fn capabilities(&self) -> WorkerCapabilities {
        self.inner.capabilities()
    }

    fn start(&self, spec: SessionSpec) -> BoxFuture<'_, Result<SessionHandle, WorkerError>> {
        if self.started.fetch_add(1, Ordering::Relaxed) > 0 {
            (self.again)();
        }
        self.inner.start(spec)
    }

    fn takeover_command(&self, vendor_session: &str, workdir: &Path) -> Option<CommandSpec> {
        self.inner.takeover_command(vendor_session, workdir)
    }

    fn cheapest_model(&self) -> Option<String> {
        self.inner.cheapest_model()
    }

    fn instruction_files(&self, workdir: &Path, isolated: bool) -> Vec<String> {
        self.inner.instruction_files(workdir, isolated)
    }

    fn logged_in(&self) -> BoxFuture<'_, Option<bool>> {
        self.inner.logged_in()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use argonauts_core::steps::intake::ChatAnswer;
    use argonauts_core::steps::{SchemaStepRunner, StepRequest, StepRunner, output_schema};
    use argonauts_core::worker::Access;
    use argonauts_testkit::worker::{FakeSession, FakeWorker, session_spec};
    use serde_json::json;

    use super::*;

    /// The first answer holds no JSON, so the runner asks once more, and only that try counts
    /// as asked again.
    #[tokio::test]
    async fn each_try_after_the_first_is_told() {
        let fake = FakeWorker::harness("claude-code", "anthropic")
            .then(FakeSession::completed("It caps each client at ten.").with_vendor_session("v1"))
            .then(FakeSession::structured(
                json!({"answer_markdown": "It caps each client at ten."}),
            ));
        let asked = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&asked);
        let worker = Attempts::wrap(Arc::new(fake), move || {
            counted.fetch_add(1, Ordering::Relaxed);
        });
        let dir = tempfile::tempdir().unwrap();
        let mut spec = session_spec("claude-code", Access::ReadOnly, dir.path());
        spec.json_schema = Some(output_schema::<ChatAnswer>());
        let outcome = SchemaStepRunner
            .run(StepRequest::new(worker, spec), Box::new(|_| {}))
            .await
            .unwrap();
        assert_eq!(outcome.attempts, 2);
        assert_eq!(asked.load(Ordering::Relaxed), 1);
    }
}
