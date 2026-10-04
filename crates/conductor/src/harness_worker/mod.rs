//! One adapter plus model behind the core `Worker` interface. It refuses unsafe sessions, runs
//! the harness in its own process group inside the OS sandbox, records exactly what was sent,
//! streams and logs every line, meters and watches the session, steers and stops it, and
//! classifies how it ended. A two-way protocol agent gets the same treatment, with its pipes
//! handed to the adapter's session driver instead of a line parser.

mod boards;
pub mod capabilities;
mod context;
mod diffs;
mod driven;
pub mod end;
mod group;
pub mod launch;
mod lines;
mod observe;
mod outbox;
pub mod refuse;
pub mod request;
pub mod session;
mod steer;
pub mod stop;

#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use argonauts_core::future::BoxFuture;
use argonauts_core::limits::{HealthBoard, QuotaBoard};
use argonauts_core::names::RoleName;
use argonauts_core::names::{VendorId, WorkerRef};
use argonauts_core::registry::ModelRegistry;
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::SandboxSettings;
use argonauts_core::worker::{
    SessionHandle, SessionSpec, Worker, WorkerCapabilities, WorkerError, WorkerKind,
};
use argonauts_core::{Adapter, CommandSpec, Paths};

use crate::doctor::resolve_binary;
use crate::quota::{Prober, QuotaWatch, has_quota_probe, register_prober};
use crate::sandbox::proxy::Proxy;
use crate::sandbox::{self, SandboxWrap};

pub struct HarnessWorker {
    adapter: Arc<dyn Adapter>,
    worker: WorkerRef,
    family: String,
    settings: Arc<Settings>,
    quota: Arc<dyn QuotaBoard>,
    health: Arc<dyn HealthBoard>,
    registry: Option<Arc<dyn ModelRegistry>>,
    /// The sandbox for the top-level `sandbox.mode`. A role with a mode of its own gets the one
    /// that mode names.
    sandbox: Arc<dyn SandboxWrap>,
    /// `None` uses the process's own, started when a session first needs it.
    proxy: Option<Proxy>,
    home: Option<Paths>,
    /// A blind session said this CLI can't keep your MCP servers and plugins out. Once is enough.
    said_it_cant_isolate: Arc<AtomicBool>,
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
        let sandbox = sandbox::for_settings(&settings.sandbox);
        register_quota_prober(&adapter, &worker, &settings);
        Self {
            adapter,
            worker,
            family,
            settings,
            quota,
            health,
            registry: None,
            sandbox,
            proxy: None,
            home: None,
            said_it_cant_isolate: Arc::default(),
        }
    }

    /// Prices the meter's usage from the registry's entry for this worker. Without one, the
    /// meter uses the vendor's reported cost or `agents.<id>.price`.
    pub fn with_registry(mut self, registry: Arc<dyn ModelRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Runs sessions in this sandbox instead of the one the top-level `sandbox.mode` names.
    pub fn with_sandbox(mut self, sandbox: Arc<dyn SandboxWrap>) -> Self {
        self.sandbox = sandbox;
        self
    }

    /// Sends sandboxed sessions through this proxy instead of the process's own.
    pub fn with_proxy(mut self, proxy: Proxy) -> Self {
        self.proxy = Some(proxy);
        self
    }

    /// Keeps the agent registry, slots, and boards under this home instead of
    /// `ARGONAUTS_HOME`, as tests do.
    pub fn with_home(mut self, home: Paths) -> Self {
        self.home = Some(home);
        self
    }

    /// Probes quota around each session when the adapter has a free probe, since a stream
    /// such as `codex exec` reports none before a limit stops it.
    fn quota_watch(&self) -> Option<QuotaWatch> {
        if !has_quota_probe(self.adapter.as_ref()) {
            return None;
        }
        match self.home() {
            Ok(home) => Some(QuotaWatch::new(self.quota.clone(), &home, self.vendor())),
            Err(err) => {
                tracing::warn!("no quota probes for this session: {err}");
                None
            }
        }
    }

    /// The sandbox rules for `role`'s sessions, its own `[roles.<role>.sandbox]` over
    /// `[sandbox]`, and the sandbox they name.
    pub(crate) fn sandbox_for(&self, role: &RoleName) -> (SandboxSettings, Arc<dyn SandboxWrap>) {
        let rules = self.settings.sandbox_for(role);
        let sandbox = if rules.mode == self.settings.sandbox.mode {
            self.sandbox.clone()
        } else {
            sandbox::for_settings(&rules)
        };
        (rules, sandbox)
    }

    pub fn adapter(&self) -> &Arc<dyn Adapter> {
        &self.adapter
    }

    /// The home this worker was given, else `ARGONAUTS_HOME` or `~/.argonauts`.
    pub fn home(&self) -> Result<Paths, WorkerError> {
        match &self.home {
            Some(home) => Ok(home.clone()),
            None => Paths::from_env().map_err(|err| WorkerError::Io(err.to_string())),
        }
    }
}

/// So a quota probe at app start runs the binary doctor checks: `agents.<id>.binary`, else the
/// adapter's binary on `PATH`. The probe runs outside the sandbox, so a binary the repo could
/// plant is refused. It names the model this worker's sessions use, as they do.
fn register_quota_prober(adapter: &Arc<dyn Adapter>, worker: &WorkerRef, settings: &Settings) {
    let agent = settings.agents.get(&worker.worker);
    let configured = agent.and_then(|agent| agent.binary.as_deref());
    let model = worker
        .model
        .clone()
        .or_else(|| agent.and_then(|agent| agent.model.clone()));
    match resolve_binary(adapter.as_ref(), configured) {
        Ok(binary) => register_prober(
            &worker.vendor(),
            Prober {
                adapter: adapter.clone(),
                binary,
                workdir: std::env::temp_dir(),
                model,
            },
        ),
        Err(problem) => tracing::debug!(
            "{}'s quota can't be probed: {problem}",
            adapter.display_name()
        ),
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
        capabilities::capabilities_for(self.adapter.as_ref())
    }

    fn start(&self, spec: SessionSpec) -> BoxFuture<'_, Result<SessionHandle, WorkerError>> {
        Box::pin(async move {
            let mut handle = session::start(self, spec).await?;
            if let Some(watch) = self.quota_watch() {
                handle.ended = watch.relay(handle.ended);
            }
            Ok(handle)
        })
    }

    fn takeover_command(&self, vendor_session: &str, workdir: &Path) -> Option<CommandSpec> {
        self.adapter.interactive_resume(vendor_session, workdir)
    }

    fn cheapest_model(&self) -> Option<String> {
        Some(self.adapter.cheapest_model().0.to_owned())
    }

    fn instruction_files(&self, workdir: &Path, isolated: bool) -> Vec<String> {
        self.adapter.instruction_files(workdir, isolated)
    }
}

#[cfg(test)]
mod quota_probe_tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use argonauts_core::limits::{QuotaReading, QuotaSource, QuotaStatus};
    use argonauts_core::settings::approvals::SandboxChoice;
    use argonauts_core::{SessionRequest, StopPlan, StreamParser};
    use argonauts_testkit::services::MemoryHealthBoard;
    use time::OffsetDateTime;

    use super::testing::{TestAdapter, collect, spec_in};
    use super::*;
    use crate::quota::FileQuotaBoard;
    use crate::sandbox::Unsandboxed;

    /// The test agent, with a free quota probe that logs each run.
    struct Probed {
        agent: TestAdapter,
        log: PathBuf,
    }

    impl Adapter for Probed {
        fn id(&self) -> &'static str {
            self.agent.id()
        }
        fn display_name(&self) -> &'static str {
            self.agent.display_name()
        }
        fn binary(&self) -> &'static str {
            self.agent.binary()
        }
        fn command(&self, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
            self.agent.command(req)
        }
        fn parser(&self) -> Box<dyn StreamParser> {
            self.agent.parser()
        }
        fn session_parser(&self, req: &SessionRequest) -> Box<dyn StreamParser> {
            self.agent.session_parser(req)
        }
        fn version_args(&self) -> &'static [&'static str] {
            self.agent.version_args()
        }
        fn parse_version(&self, output: &str) -> Option<String> {
            self.agent.parse_version(output)
        }
        fn tested_versions(&self) -> &'static [&'static str] {
            self.agent.tested_versions()
        }
        fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
            self.agent.cheapest_model()
        }
        fn stop_plan(&self) -> StopPlan {
            self.agent.stop_plan()
        }
        fn quota_probe(&self, workdir: &Path, _: Option<&str>) -> Option<CommandSpec> {
            let script = format!("echo x >> {}; echo used=0.4", self.log.display());
            Some(CommandSpec::new("/bin/sh", workdir).args(["-c", &script]))
        }
        fn parse_quota_probe(&self, stdout: &str) -> Option<QuotaReading> {
            let used = stdout.trim().strip_prefix("used=")?.parse().ok()?;
            Some(QuotaReading {
                vendor: VendorId("probed".into()),
                status: QuotaStatus::Allowed,
                used: Some(used),
                resets_at: None,
                window: None,
                source: QuotaSource::UsageProbe,
                observed_at: OffsetDateTime::now_utc(),
                detail: None,
                wrap_up: false,
            })
        }
    }

    #[tokio::test]
    async fn quota_is_probed_after_a_session_of_a_vendor_with_a_probe() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (work, session) = (root.join("work"), root.join("session"));
        std::fs::create_dir_all(&work).unwrap();
        let home = Paths::new(root.join("home"));
        let log = root.join("probes.log");
        let adapter: Arc<dyn Adapter> = Arc::new(Probed {
            agent: TestAdapter::new("true"),
            log: log.clone(),
        });
        let worker_ref = WorkerRef::new("probed", None);
        let vendor = worker_ref.vendor();
        let prober = Prober {
            adapter: adapter.clone(),
            binary: PathBuf::from("/bin/sh"),
            workdir: root.clone(),
            model: None,
        };
        let quota = Arc::new(FileQuotaBoard::new(&home).with_prober(&vendor, prober));
        let mut settings = Settings::default();
        settings.sandbox.mode = SandboxChoice::Tool;
        let worker = HarnessWorker::new(
            adapter,
            worker_ref.clone(),
            "fake".into(),
            Arc::new(settings),
            quota.clone(),
            Arc::new(MemoryHealthBoard::default()),
        )
        .with_sandbox(Arc::new(Unsandboxed))
        .with_home(home);

        let mut spec = spec_in(&work, &session, "hi");
        spec.worker = worker_ref;
        collect(worker.start(spec).await.unwrap()).await;
        let deadline = Instant::now() + Duration::from_secs(10);
        while quota.get(&vendor).is_none() {
            assert!(Instant::now() < deadline, "no probe after the session");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(quota.get(&vendor).unwrap().used, Some(0.4));
        let probes = std::fs::read_to_string(&log).unwrap();
        assert_eq!(probes.lines().count(), 1);
    }
}
