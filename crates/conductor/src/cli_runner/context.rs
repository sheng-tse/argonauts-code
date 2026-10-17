//! Everything one session needs from its runner, owned so the session can run on its own task,
//! and the launch built from it: request, command, session environment, sandbox profile, and
//! the record of what is sent. A resume builds its launch the same way, every flag again.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use argonauts_core::events::session::LaunchRecord;
use argonauts_core::limits::{HealthBoard, PlanUsageBoard};
use argonauts_core::names::AgentId;
use argonauts_core::registry::ModelRegistry;
use argonauts_core::session_runner::{
    RunnerCapabilities, SandboxMode, SandboxProfile, SessionSpec,
};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::{SandboxChoice, SandboxSettings};
use argonauts_core::settings::endpoints::local_endpoint_addresses;
use argonauts_core::{Adapter, CommandSpec, Paths, SessionRequest, create_private_dir_all};

use super::{CliRunner, capabilities, launch, request};
use crate::cli_versions::CliVersions;
use crate::doctor::resolve_binary;
use crate::sandbox::profile::{build_cache_env, build_cache_root, launch_paths, rewritten_files};
use crate::sandbox::proxy::{self, Allowlist, Proxy, Refusal};
use crate::sandbox::{ProfileInput, SandboxWrap, session_profile};

pub struct Context {
    pub spec: SessionSpec,
    pub adapter: Arc<dyn Adapter>,
    pub settings: Arc<Settings>,
    pub registry: Option<Arc<dyn ModelRegistry>>,
    /// The session's role's sandbox rules, and the sandbox they name.
    pub rules: SandboxSettings,
    pub sandbox: Arc<dyn SandboxWrap>,
    pub proxy: Option<Proxy>,
    pub plan_usage: Arc<dyn PlanUsageBoard>,
    pub health: Arc<dyn HealthBoard>,
    pub home: Paths,
    pub agent_id: AgentId,
    pub capabilities: RunnerCapabilities,
    /// Shared by the runner's sessions, so a CLI that can't isolate says so once.
    pub said_it_cant_isolate: Arc<AtomicBool>,
}

/// One process launch.
pub struct Launch {
    pub request: SessionRequest,
    /// What is spawned: the adapter's command in the sandbox.
    pub command: CommandSpec,
    /// The adapter's command before the sandbox wraps it, prompt and values left out.
    pub record: LaunchRecord,
    pub profile: SandboxProfile,
}

impl Context {
    pub fn new(worker: &CliRunner, spec: SessionSpec, home: Paths) -> Self {
        let (rules, sandbox) = worker.sandbox_for(&spec.role);
        Self {
            spec,
            adapter: worker.adapter.clone(),
            settings: worker.settings.clone(),
            registry: worker.registry.clone(),
            rules,
            sandbox,
            proxy: worker.proxy.clone(),
            plan_usage: worker.plan_usage.clone(),
            health: worker.health.clone(),
            home,
            agent_id: worker.worker.agent_id(),
            capabilities: capabilities::capabilities_for(worker.adapter.as_ref()),
            said_it_cant_isolate: worker.said_it_cant_isolate.clone(),
        }
    }

    /// The launch for `spec`: this session's own, or a resume of it with a message.
    pub fn launch(&self, spec: &SessionSpec) -> Result<Launch, String> {
        let mut request = request::session_request(
            spec,
            self.adapter.as_ref(),
            &self.settings,
            self.registry.as_deref(),
        )
        .map_err(|err| err.to_string())?;
        request.tool_sandbox_off =
            self.contained(spec.sandbox.mode) || self.rules.mode == SandboxChoice::None;
        if spec.isolated {
            request.builtins_off =
                CliVersions::load(&self.home.cli_versions()).builtins(self.adapter.id());
        }
        let mut command = self.adapter.command(&request).map_err(|err| {
            format!(
                "{} could not build its command: {err:#}",
                self.adapter.display_name()
            )
        })?;
        if command.program == self.adapter.binary()
            && let Some(binary) = self.configured_binary()?
        {
            command.program = binary;
        }
        request::apply_session_env(&mut command, &spec.key, &spec.env);
        let task_home = request::task_home_in_use(&request, &command);
        if let Some(home) = &task_home {
            // Its parents sit in the denied `~/.argonauts`, so the agent couldn't make it.
            create_private_dir_all(home).map_err(|err| {
                format!("could not make the task's home {}: {err}", home.display())
            })?;
        }
        let mut profile = self.profile(spec, &command, task_home);
        if self.contained(profile.mode) {
            make_rewritten_files(self.adapter.id(), &user_home(&self.home))?;
        }
        if self.proxied(&profile) {
            crate::health::goes_through_proxy(&spec.key);
            let (port, url) = self.proxy()?;
            profile.proxy_port = Some(port);
            set_env(&mut command, proxy::session_env(&url));
        }
        // A direct turn asks the app before an edit through its own port, which the network
        // being off must not cut.
        if !profile.network {
            profile.ask_port = spec
                .asking
                .as_ref()
                .and_then(|asking| asking.endpoint.parse::<std::net::SocketAddr>().ok())
                .filter(|addr| addr.ip().is_loopback())
                .map(|addr| addr.port());
        }
        if profile.mode != SandboxMode::None && spec.access.writes() {
            set_env(&mut command, build_cache_env(&build_cache_root()));
        }
        let sandbox_line = match profile.mode {
            SandboxMode::None => "no sandbox".to_owned(),
            SandboxMode::Os | SandboxMode::Container => self.sandbox.describe(&profile),
        };
        let record = launch::launch_record(&command, &request.prompt, &sandbox_line);
        let command = match profile.mode {
            SandboxMode::None => command,
            SandboxMode::Os | SandboxMode::Container => self
                .sandbox
                .wrap(command, &profile)
                .map_err(|err| err.to_string())?,
        };
        Ok(Launch {
            request,
            command,
            record,
            profile,
        })
    }

    /// `agents.<id>.binary`, the CLI you named in place of the one on `PATH`, checked the way
    /// doctor checks it.
    fn configured_binary(&self) -> Result<Option<String>, String> {
        let Some(configured) = self
            .settings
            .agents
            .get(self.adapter.id())
            .and_then(|agent| agent.binary.as_deref())
        else {
            return Ok(None);
        };
        let path = resolve_binary(self.adapter.as_ref(), Some(configured))?;
        Ok(Some(path.display().to_string()))
    }

    /// Whether a session with this mode runs inside one of our sandboxes: a container always,
    /// the OS sandbox only when `sandbox.mode` turns it on.
    fn contained(&self, mode: SandboxMode) -> bool {
        match mode {
            SandboxMode::Os => self.sandbox.mode() == SandboxMode::Os,
            SandboxMode::Container => true,
            SandboxMode::None => false,
        }
    }

    /// A sandboxed session with network off reaches its CLI's hosts through the proxy and
    /// nothing else. Its model API is among them, so even a no-tools session needs it. A
    /// container takes the proxy in through its bridge, or refuses to start where it can't.
    fn proxied(&self, profile: &SandboxProfile) -> bool {
        self.contained(profile.mode) && !profile.network
    }

    /// The proxy's port, and its URL for this CLI's hosts, `sandbox.proxy_allow`, and your
    /// local endpoints.
    fn proxy(&self) -> Result<(u16, String), String> {
        let allow = self.proxy_allows()?;
        let proxy = match &self.proxy {
            Some(proxy) => proxy.clone(),
            None => proxy::shared().map_err(|err| {
                format!("the network proxy didn't start, so the session can't: {err}")
            })?,
        };
        Ok((proxy.port(), proxy.session_url(&allow)))
    }

    /// The hosts this CLI's sessions may reach through the proxy.
    fn proxy_allows(&self) -> Result<Allowlist, String> {
        Ok(
            Allowlist::for_cli(self.adapter.id(), &self.rules.proxy_allow)?
                .with_local_endpoints(local_endpoint_addresses(&self.settings)),
        )
    }

    /// The latest tunnel the proxy refused this CLI's sessions since `since`.
    pub(super) fn last_proxy_refusal(&self, since: Instant) -> Option<Refusal> {
        let allow = self.proxy_allows().ok()?;
        let proxy = match &self.proxy {
            Some(proxy) => proxy.clone(),
            None => proxy::shared().ok()?,
        };
        proxy.refused_since(&allow, since).pop()
    }

    /// The session's profile, with what its launch names in the session folder readable, and the
    /// home it keeps for the task writable.
    fn profile(
        &self,
        spec: &SessionSpec,
        command: &CommandSpec,
        task_home: Option<PathBuf>,
    ) -> SandboxProfile {
        let user_home = user_home(&self.home);
        let extra_writable = extra_writable(&self.rules, &user_home, &spec.workdir);
        let input = ProfileInput {
            access: spec.access,
            agent_id: self.adapter.id(),
            workdir: &spec.workdir,
            session_dir: &spec.session_dir,
            argonauts_home: self.home.home(),
            user_home: &user_home,
            // A read-only run session's workdir is the base worktree already.
            base_worktree: None,
            status_file: spec.status_file.as_deref(),
            extra_writable: &extra_writable,
            sees_other_worktrees: self.rules.agents_see_other_worktrees,
            commits: self.rules.agents_commit,
        };
        // The engine may ask for less network than the role allows, never more.
        let mut profile = SandboxProfile {
            mode: spec.sandbox.mode,
            network: spec.sandbox.network && self.rules.network,
            ..session_profile(&spec.sandbox, &input)
        };
        for path in launch_paths(command, &spec.session_dir) {
            if !profile.readable.contains(&path) {
                profile.readable.push(path);
            }
        }
        // It sits under ~/.argonauts, which is closed to reads too, so it needs both. Hermes
        // 0.19.0 stopped at start with EPERM on its own home when it was only writable.
        if let Some(home) = task_home {
            if !profile.readable.contains(&home) {
                profile.readable.push(home.clone());
            }
            if !profile.writable.contains(&home) {
                profile.writable.push(home);
            }
        }
        profile
    }
}

/// Makes the files the CLI writes afresh in a read-only folder of its, empty and executable,
/// where the folder exists and they don't: the session may write them but can't add a name there.
fn make_rewritten_files(agent_id: &str, user_home: &Path) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    for path in rewritten_files(agent_id, user_home) {
        if !path.parent().is_some_and(Path::is_dir) {
            continue;
        }
        let made = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&path);
        match made {
            Err(err) if err.kind() != std::io::ErrorKind::AlreadyExists => {
                return Err(format!("could not make {}: {err}", path.display()));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Sets each pair over whatever the command had for that name.
fn set_env(command: &mut CommandSpec, pairs: Vec<(String, String)>) {
    for (name, value) in pairs {
        command.env_set.retain(|(set, _)| *set != name);
        command.env_remove.retain(|removed| *removed != name);
        command.env_set.push((name, value));
    }
}

/// `HOME`, else the folder holding `~/.argonauts`.
fn user_home(home: &Paths) -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home.home().parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `sandbox.extra_writable` made absolute: `~/` from your home, and anything relative from the
/// session's workdir.
fn extra_writable(rules: &SandboxSettings, user_home: &Path, workdir: &Path) -> Vec<PathBuf> {
    rules
        .extra_writable
        .iter()
        .map(|entry| match entry.strip_prefix("~/") {
            Some(rest) => user_home.join(rest),
            None if Path::new(entry).is_absolute() => PathBuf::from(entry),
            None => workdir.join(entry),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use argonauts_core::names::{ModelRef, RoleName};
    use argonauts_core::settings::agents::EndpointSettings;

    use super::*;
    use crate::cli_runner::testing::{FakeDriver, TestAdapter, TestDriver, spec_in};
    use argonauts_core::session_runner::Access;

    #[test]
    fn a_missing_rewritten_file_is_made_where_its_folder_is() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join(".gemini/antigravity-cli/bin");
        make_rewritten_files("antigravity-cli", home.path()).unwrap();
        assert!(!bin.exists(), "made agy's folders though agy isn't there");
        std::fs::create_dir_all(&bin).unwrap();
        make_rewritten_files("antigravity-cli", home.path()).unwrap();
        let shim = bin.join("agentapi");
        let meta = std::fs::metadata(&shim).unwrap();
        assert!(meta.is_file() && meta.len() == 0);
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
        // agy's own file stays as it is.
        std::fs::write(&shim, "#!/bin/sh\n").unwrap();
        make_rewritten_files("antigravity-cli", home.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&shim).unwrap(), "#!/bin/sh\n");
        make_rewritten_files("codex", home.path()).unwrap();
    }

    #[test]
    fn set_env_replaces_earlier_values() {
        let mut command = CommandSpec::new("x", "/w");
        command
            .env_set
            .push(("CARGO_HOME".into(), "/home/me/.cargo".into()));
        command.env_remove.push("XDG_CACHE_HOME".into());
        set_env(
            &mut command,
            vec![
                ("CARGO_HOME".into(), "/tmp/c/cargo".into()),
                ("XDG_CACHE_HOME".into(), "/tmp/c/xdg".into()),
            ],
        );
        assert_eq!(
            command.env_set,
            [
                ("CARGO_HOME".to_owned(), "/tmp/c/cargo".to_owned()),
                ("XDG_CACHE_HOME".to_owned(), "/tmp/c/xdg".to_owned())
            ]
        );
        assert!(command.env_remove.is_empty());
    }

    #[test]
    fn a_home_kept_for_the_task_is_made_readable_and_writable() {
        use argonauts_core::layout;

        let keeping = TestAdapter::new("true")
            .with_driver(FakeDriver::default())
            .keeping_its_home_for_the_task();
        let h = TestDriver::new(keeping);
        // Run folders sit in ~/.argonauts, which sessions can't read.
        let run_dir = h
            .worker
            .home()
            .unwrap()
            .home()
            .join("state/repo/runs/run-7");
        let dir = tempfile::tempdir().unwrap();
        let mut spec = spec_in(dir.path(), dir.path(), "hi");
        spec.session_dir = layout::session_dir(&run_dir, &spec.key);
        spec.run = Some(argonauts_core::RunId::new("run-7").unwrap());
        spec.task = Some(argonauts_core::TaskId::new("c-001").unwrap());
        let home = request::task_home(&spec, "fake").unwrap();

        let ctx = Context::new(&h.worker, spec.clone(), h.worker.home().unwrap());
        let launch = ctx.launch(&spec).unwrap();
        assert!(home.is_dir());
        let writable = &launch.profile.writable;
        assert!(writable.contains(&home), "{writable:?}");
        assert!(crate::sandbox::profile::can_read(
            &launch.profile,
            &home.join("config.yaml")
        ));
        assert!(crate::sandbox::profile::can_write(
            &launch.profile,
            &home.join("state.db")
        ));

        // A home kept for the session leaves the task's folder alone.
        std::fs::remove_dir_all(&home).unwrap();
        let h = TestDriver::new(TestAdapter::new("true").with_driver(FakeDriver::default()));
        let ctx = Context::new(&h.worker, spec.clone(), h.worker.home().unwrap());
        let launch = ctx.launch(&spec).unwrap();
        assert!(!home.exists());
        assert!(!launch.profile.writable.contains(&home));
    }

    /// Claims a sandbox's mode and runs the command as it is.
    struct Pretend(SandboxMode);

    impl SandboxWrap for Pretend {
        fn mode(&self) -> SandboxMode {
            self.0
        }
        fn wrap(
            &self,
            command: CommandSpec,
            _profile: &SandboxProfile,
        ) -> Result<CommandSpec, crate::sandbox::SandboxError> {
            Ok(command)
        }
        fn describe(&self, _profile: &SandboxProfile) -> String {
            "pretend".into()
        }
    }

    fn env_of<'a>(command: &'a CommandSpec, name: &str) -> Option<&'a str> {
        command
            .env_set
            .iter()
            .find(|(set, _)| set == name)
            .map(|(_, value)| value.as_str())
    }

    /// A session that went out through the proxy and failed on the network names what the
    /// proxy refused it since it started. Any other failure, and one refused nothing, keeps the
    /// CLI's own words.
    #[test]
    fn a_proxied_session_that_failed_on_the_network_names_what_the_proxy_refused() {
        use crate::cli_runner::session::{name_proxy_refusal, not_started};
        use crate::sandbox::proxy::tests::{connect_as, to_loopback};
        use argonauts_core::ErrorCategory;
        use argonauts_core::events::session::EndReason;

        let dir = tempfile::tempdir().unwrap();
        let proxy = Proxy::with_resolver(to_loopback(1, Arc::default())).unwrap();
        let h = TestDriver::new(TestAdapter::new("true").with_driver(FakeDriver::default()));
        let worker = h
            .worker
            .with_sandbox(Arc::new(Pretend(SandboxMode::Os)))
            .with_proxy(proxy.clone());
        let spec = spec_in(dir.path(), dir.path(), "hi");
        let ctx = Context::new(&worker, spec, worker.home().unwrap());
        let started = Instant::now();
        let ended = |category, since| {
            let reason = EndReason::CliError {
                category,
                message: "API Error: Connection error.".to_owned(),
            };
            let mut end = not_started(reason, started, None);
            name_proxy_refusal(&ctx, &mut end, since);
            match end.reason {
                EndReason::CliError { message, .. } => message,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(
            ended(ErrorCategory::Network, started),
            "API Error: Connection error."
        );

        let url = proxy.session_url(&ctx.proxy_allows().unwrap());
        let head = connect_as(&proxy, &url, "statsig.example.test:443");
        assert!(head.starts_with("HTTP/1.1 403 "), "{head}");
        assert_eq!(
            ended(ErrorCategory::Network, started),
            "the sandbox's proxy refused statsig.example.test, which isn't on the session's \
             list\nAPI Error: Connection error."
        );
        assert_eq!(
            ended(ErrorCategory::Server, started),
            "API Error: Connection error."
        );
        assert_eq!(
            ended(ErrorCategory::Timeout, Instant::now()),
            "API Error: Connection error."
        );
    }

    #[test]
    fn a_sandboxed_session_reaches_the_network_only_through_the_proxy() {
        let dir = tempfile::tempdir().unwrap();
        let proxy = Proxy::start().unwrap();
        let mut settings = Settings::default();
        settings.sandbox.proxy_allow = vec!["mirror.example".into()];
        settings.endpoints.insert(
            "vllm".into(),
            EndpointSettings {
                base_url: Some("http://localhost:8000/v1".into()),
                local: true,
                ..EndpointSettings::default()
            },
        );
        let local = local_endpoint_addresses(&settings);
        assert!(local.contains(&("localhost".into(), 8000)), "{local:?}");
        let h = TestDriver::with_settings(
            TestAdapter::new("true").with_driver(FakeDriver::default()),
            settings,
        );
        let worker = h
            .worker
            .with_sandbox(Arc::new(Pretend(SandboxMode::Os)))
            .with_proxy(proxy.clone());
        let mut spec = spec_in(dir.path(), dir.path(), "hi");
        let home = worker.home().unwrap();

        // Even a no-tools session needs its model API, which only the proxy reaches.
        for access in [Access::Build, Access::NoTools] {
            spec.access = access;
            let launch = Context::new(&worker, spec.clone(), home.clone())
                .launch(&spec)
                .unwrap();
            assert_eq!(launch.profile.proxy_port, Some(proxy.port()));
            // Your local endpoints come along, at exactly their host and port.
            let allow = Allowlist::for_cli("fake", &["mirror.example".into()])
                .unwrap()
                .with_local_endpoints(local.clone());
            assert!(allow.is_local("localhost", 8000) && !allow.is_local("localhost", 8001));
            let url = proxy.session_url(&allow);
            for name in ["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY", "http_proxy"] {
                assert_eq!(env_of(&launch.command, name), Some(url.as_str()), "{name}");
            }
            assert_eq!(env_of(&launch.command, "NO_PROXY"), Some(""));
            assert!(!launch.record.env_set.is_empty());
        }

        // `sandbox.network = true` opens the network itself, so no proxy.
        spec.sandbox.network = true;
        let launch = Context::new(&worker, spec.clone(), home.clone())
            .launch(&spec)
            .unwrap();
        assert_eq!(launch.profile.proxy_port, None);
        assert_eq!(env_of(&launch.command, "HTTPS_PROXY"), None);

        // Nor without a sandbox.
        spec.sandbox.network = false;
        let unsandboxed =
            TestDriver::new(TestAdapter::new("true").with_driver(FakeDriver::default()));
        let launch = Context::new(&unsandboxed.worker, spec.clone(), home)
            .launch(&spec)
            .unwrap();
        assert_eq!(launch.profile.proxy_port, None);
        assert_eq!(env_of(&launch.command, "HTTPS_PROXY"), None);
    }

    /// macOS won't start the CLI's own sandbox inside ours, so the request says when ours holds
    /// the session, network or not, and the CLI keeps its own only when nothing of ours does.
    #[test]
    fn a_cli_in_our_sandbox_is_told_to_keep_its_own_off() {
        let dir = tempfile::tempdir().unwrap();
        let proxy = Proxy::start().unwrap();
        let own_off = |sandbox: Option<SandboxMode>, spec: &SessionSpec| {
            let h = TestDriver::new(fake());
            let worker = match sandbox {
                Some(mode) => h.worker.with_sandbox(Arc::new(Pretend(mode))),
                None => h.worker,
            }
            .with_proxy(proxy.clone());
            launch(&worker, spec).request.tool_sandbox_off
        };
        let mut spec = spec_in(dir.path(), dir.path(), "hi");
        assert!(own_off(Some(SandboxMode::Os), &spec));
        spec.sandbox.network = true;
        assert!(own_off(Some(SandboxMode::Os), &spec));
        // `sandbox.mode = "tool"` wraps nothing, so the CLI's own sandbox stays on.
        assert!(!own_off(None, &spec));
        spec.sandbox.mode = SandboxMode::None;
        assert!(!own_off(Some(SandboxMode::Os), &spec));
        spec.sandbox.mode = SandboxMode::Container;
        assert!(own_off(Some(SandboxMode::Container), &spec));
    }

    fn fake() -> TestAdapter {
        TestAdapter::new("true").with_driver(FakeDriver::default())
    }

    fn launch(worker: &CliRunner, spec: &SessionSpec) -> Launch {
        Context::new(worker, spec.clone(), worker.home().unwrap())
            .launch(spec)
            .unwrap()
    }

    /// Settings whose top-level `[sandbox]` has `mode`, and whose worker role has its own.
    fn worker_mode(top: SandboxChoice, worker: SandboxChoice) -> Settings {
        let mut settings = Settings::default();
        settings.sandbox.mode = top;
        settings
            .roles
            .entry(RoleName::Worker)
            .or_default()
            .sandbox
            .mode = Some(worker);
        settings
    }

    fn with_settings(settings: Settings) -> TestDriver {
        let h = TestDriver::with_settings(fake(), Settings::default());
        TestDriver {
            worker: CliRunner::new(
                h.worker.adapter().clone(),
                ModelRef::new("fake", None),
                "fake".into(),
                Arc::new(settings),
                h.plan_usage.clone(),
                h.health.clone(),
            )
            .with_home(Paths::new(h.home.path())),
            ..h
        }
    }

    /// `tool` leaves each CLI's own protection on and wraps nothing, as before Argonauts Code had a
    /// sandbox; `none` turns the CLI's off too. Each role can pick its own.
    #[test]
    fn tool_keeps_the_clis_own_sandbox_and_none_turns_it_off() {
        let dir = tempfile::tempdir().unwrap();
        let h = with_settings(worker_mode(SandboxChoice::Tool, SandboxChoice::None));
        let mut spec = spec_in(dir.path(), dir.path(), "hi");
        spec.sandbox.mode = SandboxMode::None;

        let tool = launch(&h.worker, &spec);
        assert!(!tool.request.tool_sandbox_off);
        assert_eq!(tool.record.sandbox.as_deref(), Some("no sandbox"));
        assert_eq!(tool.command.program, tool.record.program);

        spec.role = RoleName::Worker;
        let none = launch(&h.worker, &spec);
        assert!(none.request.tool_sandbox_off);
        assert_eq!(none.record.sandbox.as_deref(), Some("no sandbox"));
        assert_eq!(none.command.program, none.record.program);
    }

    /// A role's own `sandbox.mode` picks its sandbox over the top-level one, either way.
    #[test]
    fn a_roles_own_mode_picks_its_sandbox() {
        let h = with_settings(worker_mode(SandboxChoice::Tool, SandboxChoice::Os));
        let (rules, sandbox) = h.worker.sandbox_for(&RoleName::Worker);
        assert_eq!(rules.mode, SandboxChoice::Os);
        assert_eq!(sandbox.mode(), SandboxMode::Os);
        let (rules, sandbox) = h.worker.sandbox_for(&RoleName::Main);
        assert_eq!(rules.mode, SandboxChoice::Tool);
        assert_eq!(sandbox.mode(), SandboxMode::None);

        // A worker given a sandbox keeps it for every role on the top-level mode.
        let h =
            TestDriver::with_settings(fake(), worker_mode(SandboxChoice::Os, SandboxChoice::None));
        let worker = h.worker.with_sandbox(Arc::new(Pretend(SandboxMode::Os)));
        let (_, sandbox) = worker.sandbox_for(&RoleName::Main);
        assert_eq!(sandbox.describe(&SandboxProfile::default()), "pretend");
        let (rules, sandbox) = worker.sandbox_for(&RoleName::Worker);
        assert_eq!(rules.mode, SandboxChoice::None);
        assert_eq!(sandbox.mode(), SandboxMode::None);
    }

    /// With `sandbox.network` on, a session's commands reach the internet themselves, with no
    /// proxy between. A role's own `network = false` sends it through the proxy, whatever the
    /// engine asked.
    #[test]
    fn network_on_skips_the_proxy_and_a_role_can_turn_it_off() {
        let dir = tempfile::tempdir().unwrap();
        let proxy = Proxy::start().unwrap();
        let mut settings = Settings::default();
        assert!(settings.sandbox.network);
        settings
            .roles
            .entry(RoleName::Worker)
            .or_default()
            .sandbox
            .network = Some(false);
        let h = TestDriver::with_settings(fake(), settings);
        let worker = h
            .worker
            .with_sandbox(Arc::new(Pretend(SandboxMode::Os)))
            .with_proxy(proxy.clone());
        let mut spec = spec_in(dir.path(), dir.path(), "hi");
        spec.sandbox.network = true;

        let open = launch(&worker, &spec);
        assert!(open.profile.network);
        assert_eq!(open.profile.proxy_port, None);
        assert_eq!(env_of(&open.command, "HTTPS_PROXY"), None);

        spec.role = RoleName::Worker;
        let closed = launch(&worker, &spec);
        assert!(!closed.profile.network);
        assert_eq!(closed.profile.proxy_port, Some(proxy.port()));
        assert!(env_of(&closed.command, "HTTPS_PROXY").is_some());
    }

    #[test]
    fn a_container_session_without_network_gets_the_proxy_too() {
        let dir = tempfile::tempdir().unwrap();
        let proxy = Proxy::start().unwrap();
        let h = TestDriver::new(TestAdapter::new("true").with_driver(FakeDriver::default()));
        let worker = h
            .worker
            .with_sandbox(Arc::new(Pretend(SandboxMode::Container)))
            .with_proxy(proxy.clone());
        let mut spec = spec_in(dir.path(), dir.path(), "hi");
        spec.sandbox.mode = SandboxMode::Container;
        let home = worker.home().unwrap();
        let launch = Context::new(&worker, spec.clone(), home)
            .launch(&spec)
            .unwrap();
        assert_eq!(launch.profile.proxy_port, Some(proxy.port()));
        let allow = Allowlist::for_cli("fake", &[])
            .unwrap()
            .with_local_endpoints(local_endpoint_addresses(&Settings::default()));
        assert_eq!(
            env_of(&launch.command, "HTTPS_PROXY"),
            Some(proxy.session_url(&allow).as_str())
        );
    }

    /// With the network off, a direct turn's profile names the app's port for its questions, so
    /// the sandbox lets it through as it lets the proxy through. With network on it needs none.
    #[test]
    fn a_direct_turn_without_network_keeps_its_way_to_ask() {
        let dir = tempfile::tempdir().unwrap();
        let proxy = Proxy::start().unwrap();
        let h = TestDriver::new(TestAdapter::new("true").with_driver(FakeDriver::default()));
        let worker = h
            .worker
            .with_sandbox(Arc::new(Pretend(SandboxMode::Os)))
            .with_proxy(proxy.clone());
        let mut spec = spec_in(dir.path(), dir.path(), "hi");
        spec.asking = Some(argonauts_core::session_runner::Asking {
            asks: argonauts_core::session::Asks::LikeCli,
            endpoint: "127.0.0.1:41234".to_owned(),
            token: "t".to_owned(),
            tools: vec!["argonauts_start_workflow".to_owned()],
            standing: None,
            roles: Vec::new(),
        });
        let off = launch(&worker, &spec);
        assert_eq!(off.profile.proxy_port, Some(proxy.port()));
        assert_eq!(off.profile.ask_port, Some(41234));
        spec.sandbox.network = true;
        let on = launch(&worker, &spec);
        assert_eq!(on.profile.ask_port, None);
    }

    #[test]
    fn a_bad_proxy_allow_entry_stops_the_launch() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings.sandbox.proxy_allow = vec!["https://mirror.example/".into()];
        let h = TestDriver::with_settings(
            TestAdapter::new("true").with_driver(FakeDriver::default()),
            settings,
        );
        let worker = h
            .worker
            .with_sandbox(Arc::new(Pretend(SandboxMode::Os)))
            .with_proxy(Proxy::start().unwrap());
        let spec = spec_in(dir.path(), dir.path(), "hi");
        let home = worker.home().unwrap();
        let err = Context::new(&worker, spec.clone(), home)
            .launch(&spec)
            .err()
            .unwrap();
        assert!(err.contains("sandbox.proxy_allow"), "{err}");
    }

    #[test]
    fn extra_writable_paths_are_made_absolute() {
        let rules = SandboxSettings {
            extra_writable: vec!["~/.cache/cargo".into(), "/opt/x".into(), "out".into()],
            ..SandboxSettings::default()
        };
        let paths = extra_writable(&rules, Path::new("/home/me"), Path::new("/w"));
        assert_eq!(
            paths,
            [
                PathBuf::from("/home/me/.cache/cargo"),
                PathBuf::from("/opt/x"),
                PathBuf::from("/w/out")
            ]
        );
    }
}
