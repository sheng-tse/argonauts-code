//! One adapter plus model behind the core `SessionRunner` interface. It refuses unsafe sessions,
//! runs the CLI in its own process group inside the OS sandbox, records exactly what was sent,
//! streams and logs every line, meters and watches the session, steers and stops it, and classifies
//! how it ended. A two-way protocol agent gets the same treatment, with its pipes passed to the
//! adapter's session driver instead of a line parser.

mod boards;
pub mod capabilities;
mod context;
mod context_watch;
mod diffs;
mod driven;
pub mod end;
mod group;
pub mod launch;
mod lines;
mod observe;
mod outbox;
pub mod refuse;
mod report_tool;
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

use argonauts_core::cheapest::cheapest_listed;
use argonauts_core::future::BoxFuture;
use argonauts_core::limits::{HealthBoard, PlanUsageBoard};
use argonauts_core::names::RoleName;
use argonauts_core::names::{AgentId, ModelRef};
use argonauts_core::registry::ModelRegistry;
use argonauts_core::session_runner::{
    RunnerCapabilities, RunnerError, RunnerKind, SessionHandle, SessionRunner, SessionSpec,
};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::SandboxSettings;
use argonauts_core::{Adapter, CommandSpec, Paths};

use crate::doctor::resolve_binary;
use crate::health::{DisableScope, disable_scope};
use crate::plan_usage::{PlanUsageWatch, Prober, has_plan_usage_probe, register_prober};
use crate::sandbox::proxy::Proxy;
use crate::sandbox::{self, SandboxWrap};

pub struct CliRunner {
    adapter: Arc<dyn Adapter>,
    worker: ModelRef,
    provider: String,
    settings: Arc<Settings>,
    plan_usage: Arc<dyn PlanUsageBoard>,
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

impl CliRunner {
    pub fn new(
        adapter: Arc<dyn Adapter>,
        worker: ModelRef,
        provider: String,
        settings: Arc<Settings>,
        plan_usage: Arc<dyn PlanUsageBoard>,
        health: Arc<dyn HealthBoard>,
    ) -> Self {
        let sandbox = sandbox::for_settings(&settings.sandbox);
        register_plan_usage_prober(&adapter, &worker, &settings);
        Self {
            adapter,
            worker,
            provider,
            settings,
            plan_usage,
            health,
            registry: None,
            sandbox,
            proxy: None,
            home: None,
            said_it_cant_isolate: Arc::default(),
        }
    }

    /// Prices the meter's usage from the registry's entry for this runner. Without one, the
    /// meter uses the agent's reported cost or `agents.<id>.price`.
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

    /// Probes plan usage around each session when the adapter has a free probe, since a stream
    /// such as `codex exec` reports none before a limit stops it. A stream that reports plan usage
    /// itself, as Claude Code's does, needs no probe while it runs.
    fn plan_usage_watch(&self) -> Option<PlanUsageWatch> {
        if !has_plan_usage_probe(self.adapter.as_ref()) || self.adapter.stream_reports_plan_usage()
        {
            return None;
        }
        match self.home() {
            Ok(home) => Some(PlanUsageWatch::new(
                self.plan_usage.clone(),
                &home,
                self.agent_id(),
            )),
            Err(err) => {
                tracing::warn!("no plan usage probes for this session: {err}");
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

    /// The home this runner was given, else `ARGONAUTS_HOME` or `~/.argonauts`.
    pub fn home(&self) -> Result<Paths, RunnerError> {
        match &self.home {
            Some(home) => Ok(home.clone()),
            None => Paths::from_env().map_err(|err| RunnerError::Io(err.to_string())),
        }
    }
}

/// So a plan usage probe at app start runs the binary doctor checks: `agents.<id>.binary`, else the
/// adapter's binary on `PATH`. The probe runs outside the sandbox, so a binary the repo could
/// plant is refused.
fn register_plan_usage_prober(adapter: &Arc<dyn Adapter>, worker: &ModelRef, settings: &Settings) {
    let agent = settings.agents.get(&worker.agent);
    // A CLI that bills an API key has no plan, and a probe would read whichever
    // subscription happens to be signed in.
    if agent.is_some_and(|agent| agent.auth == argonauts_core::AuthMode::ApiKey) {
        return;
    }
    let configured = agent.and_then(|agent| agent.binary.as_deref());
    // Only the model you set for the CLI, never one a runner was built for such as a cheap
    // judge, since the last prober registered wins. With none set the CLI picks its own.
    let model = agent.and_then(|agent| agent.model.clone());
    match resolve_binary(adapter.as_ref(), configured) {
        Ok(binary) => register_prober(
            &worker.agent_id(),
            Prober {
                adapter: adapter.clone(),
                binary,
                workdir: std::env::temp_dir(),
                model,
            },
        ),
        Err(problem) => tracing::debug!(
            "{}'s plan usage can't be probed: {problem}",
            adapter.display_name()
        ),
    }
}

impl SessionRunner for CliRunner {
    fn model_ref(&self) -> &ModelRef {
        &self.worker
    }

    fn kind(&self) -> RunnerKind {
        RunnerKind::Cli
    }

    fn agent_id(&self) -> AgentId {
        self.worker.agent_id()
    }

    fn provider(&self) -> String {
        self.provider.clone()
    }

    fn capabilities(&self) -> RunnerCapabilities {
        capabilities::capabilities_for(self.adapter.as_ref())
    }

    fn start(&self, spec: SessionSpec) -> BoxFuture<'_, Result<SessionHandle, RunnerError>> {
        Box::pin(async move {
            let mut handle = session::start(self, spec).await?;
            if let Some(watch) = self.plan_usage_watch() {
                handle.ended = watch.relay(handle.ended);
            }
            Ok(handle)
        })
    }

    /// What [`session::start`] would refuse for any session: the agent disabled for new work,
    /// or OpenCode with no model to run but its hosted default.
    fn start_refusal(&self) -> Option<String> {
        let health = self.health.state(&self.worker.agent_id());
        if let Some(reason) = health.disabled
            && disable_scope(&reason) == DisableScope::Every
        {
            let disabled = RunnerError::Disabled {
                model: self.worker.clone(),
                reason,
            };
            return Some(disabled.to_string());
        }
        request::start_refusal(
            &self.worker,
            self.adapter.as_ref(),
            &self.settings,
            self.registry.as_deref(),
        )
    }

    fn take_over_command(&self, cli_session: &str, workdir: &Path) -> Option<CommandSpec> {
        self.adapter.interactive_resume(cli_session, workdir)
    }

    /// The cheapest model the CLI's own list names, else the adapter's own pick while no list
    /// is known.
    fn cheapest_model(&self) -> Option<String> {
        self.registry
            .as_deref()
            .and_then(|registry| cheapest_listed(registry, self.adapter.id()))
            .map(|cheapest| cheapest.model)
            .or_else(|| Some(self.adapter.cheapest_model().0.to_owned()))
    }

    fn instruction_files(&self, workdir: &Path, isolated: bool) -> Vec<String> {
        self.adapter.instruction_files(workdir, isolated)
    }

    fn logged_in(&self) -> BoxFuture<'_, Option<bool>> {
        Box::pin(login_state(
            self.adapter.as_ref(),
            &self.worker,
            &self.settings,
        ))
    }
}

/// How long the CLI's own login check may take before it claims nothing.
const LOGIN_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Runs the adapter's verified login check as the app's start check does: exit 0 is logged in,
/// exit 1 logged out, and anything else, a check that's slow, or an agent that signs in with
/// an API key claims nothing. It runs without agent keys and calls no model.
async fn login_state(
    adapter: &dyn Adapter,
    worker: &ModelRef,
    settings: &Settings,
) -> Option<bool> {
    let agent = settings.agents.get(&worker.agent);
    if agent.is_some_and(|agent| agent.auth == argonauts_core::AuthMode::ApiKey) {
        return None;
    }
    let args = adapter.setup_hints().login_check?;
    let binary = resolve_binary(adapter, agent.and_then(|agent| agent.binary.as_deref())).ok()?;
    let mut command = tokio::process::Command::new(binary);
    command
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    for name in argonauts_core::PROVIDER_KEY_ENV_VARS {
        command.env_remove(name);
    }
    match tokio::time::timeout(LOGIN_CHECK_TIMEOUT, command.status()).await {
        Ok(Ok(status)) => match status.code() {
            Some(0) => Some(true),
            Some(1) => Some(false),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod plan_usage_probe_tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use argonauts_core::limits::{PlanUsageReading, PlanUsageSource, PlanUsageStatus};
    use argonauts_core::settings::approvals::SandboxChoice;
    use argonauts_core::{SessionRequest, StopPlan, StreamParser};
    use argonauts_testkit::services::MemoryHealthBoard;
    use time::OffsetDateTime;

    use super::testing::{TestAdapter, collect, spec_in};
    use super::*;
    use crate::plan_usage::FilePlanUsageBoard;
    use crate::sandbox::Unsandboxed;

    /// The test agent, with a free plan usage probe that logs each run.
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
        fn plan_usage_probe(&self, workdir: &Path, _: Option<&str>) -> Option<CommandSpec> {
            let script = format!("echo x >> {}; echo used=0.4", self.log.display());
            Some(CommandSpec::new("/bin/sh", workdir).args(["-c", &script]))
        }
        fn parse_plan_usage_probe(&self, stdout: &str) -> Option<PlanUsageReading> {
            let used = stdout.trim().strip_prefix("used=")?.parse().ok()?;
            Some(PlanUsageReading {
                agent_id: AgentId("probed".into()),
                status: PlanUsageStatus::Allowed,
                used: Some(used),
                resets_at: None,
                window: None,
                source: PlanUsageSource::UsageProbe,
                observed_at: OffsetDateTime::now_utc(),
                detail: None,
                wrap_up: false,
            })
        }
    }

    #[tokio::test]
    async fn plan_usage_is_probed_after_a_session_of_an_agent_with_a_probe() {
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
        let worker_ref = ModelRef::new("probed", None);
        let agent_id = worker_ref.agent_id();
        let prober = Prober {
            adapter: adapter.clone(),
            binary: PathBuf::from("/bin/sh"),
            workdir: root.clone(),
            model: None,
        };
        let plan_usage = Arc::new(FilePlanUsageBoard::new(&home).with_prober(&agent_id, prober));
        let mut settings = Settings::default();
        settings.sandbox.mode = SandboxChoice::Tool;
        let worker = CliRunner::new(
            adapter,
            worker_ref.clone(),
            "fake".into(),
            Arc::new(settings),
            plan_usage.clone(),
            Arc::new(MemoryHealthBoard::default()),
        )
        .with_sandbox(Arc::new(Unsandboxed))
        .with_home(home);

        let mut spec = spec_in(&work, &session, "hi");
        spec.model = worker_ref;
        collect(worker.start(spec).await.unwrap()).await;
        let deadline = Instant::now() + Duration::from_secs(10);
        while plan_usage.get(&agent_id).is_none() {
            assert!(Instant::now() < deadline, "no probe after the session");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(plan_usage.get(&agent_id).unwrap().used, Some(0.4));
        let probes = std::fs::read_to_string(&log).unwrap();
        assert_eq!(probes.lines().count(), 1);
    }
}
