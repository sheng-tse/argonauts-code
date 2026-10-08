//! Backs `argonauts doctor`, which finds the installed agent CLIs and judges them by version
//! range and by the flags their `--help` offers. It only runs version, help, and login-status
//! commands, never a model.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use argonauts_core::Adapter;
use argonauts_core::settings::Settings;
use serde::Serialize;

use crate::git::Git;
use crate::quota::{BinaryError, find_unsandboxed};

pub mod always_on;
mod command;
pub mod details;
pub mod roles;
pub mod update;
pub mod versions;

pub use details::{DoctorDetails, doctor_details};

use versions::{VersionFit, probe_flags_within, version_fit};

pub const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Tested,
    /// A later patch of the newest version recorded, which should work.
    Newer,
    /// Installed, at a version nobody has recorded yet. It may still work.
    Untested,
    UnknownVersion,
    NotInstalled,
}

impl AgentStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Tested => "checked",
            Self::Newer => "newer than checked",
            Self::Untested => "not checked",
            Self::UnknownVersion => "unknown version",
            Self::NotInstalled => "not installed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentCheck {
    pub id: String,
    pub display_name: String,
    pub binary: String,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    pub status: AgentStatus,
    pub tested_versions: Vec<String>,
    /// Used for live tests.
    pub cheapest_model: String,
    pub cheapest_effort: Option<String>,
    pub problem: Option<String>,
    /// `agents.<id>.enabled`. One that's off is listed but never counts as ready.
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitCheck {
    pub version: Option<String>,
    pub problem: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    pub agents: Vec<AgentCheck>,
    pub git: GitCheck,
}

impl DoctorReport {
    /// Whether git works and at least one agent CLI is installed and on.
    pub fn ready(&self) -> bool {
        self.git.version.is_some() && self.agents.iter().any(AgentCheck::usable)
    }
}

impl AgentCheck {
    /// Installed and on.
    pub fn usable(&self) -> bool {
        self.enabled && self.status != AgentStatus::NotInstalled
    }
}

pub async fn doctor(adapters: &[Arc<dyn Adapter>], settings: &Settings) -> DoctorReport {
    let checks = adapters.iter().map(|adapter| {
        let binary = settings
            .agents
            .get(adapter.id())
            .and_then(|agent| agent.binary.as_deref());
        let enabled = settings
            .agents
            .get(adapter.id())
            .is_none_or(|agent| agent.enabled);
        async move {
            AgentCheck {
                enabled,
                ..check_agent_at(adapter.as_ref(), binary, VERSION_TIMEOUT).await
            }
        }
    });
    let agents = futures::future::join_all(checks).await;
    let git = match Git::new().version().await {
        Ok(version) => GitCheck {
            version: Some(version),
            problem: None,
        },
        Err(err) => GitCheck {
            version: None,
            problem: Some(err.to_string()),
        },
    };
    DoctorReport { agents, git }
}

pub async fn check_agent(adapter: &dyn Adapter, timeout: Duration) -> AgentCheck {
    check_agent_at(adapter, None, timeout).await
}

/// Like [`check_agent`], running `binary` (from `agents.<id>.binary`) when given. `Tested` means
/// the version is in the tested range and `--help` offers every flag the adapter passes.
pub async fn check_agent_at(
    adapter: &dyn Adapter,
    binary: Option<&Path>,
    timeout: Duration,
) -> AgentCheck {
    let (model, effort) = adapter.cheapest_model();
    let mut check = AgentCheck {
        id: adapter.id().to_owned(),
        display_name: adapter.display_name().to_owned(),
        binary: binary.map_or_else(
            || adapter.binary().to_owned(),
            |path| path.display().to_string(),
        ),
        path: None,
        version: None,
        status: AgentStatus::NotInstalled,
        tested_versions: adapter
            .tested_versions()
            .iter()
            .map(|v| (*v).to_owned())
            .collect(),
        cheapest_model: model.to_owned(),
        cheapest_effort: effort.map(str::to_owned),
        problem: None,
        enabled: true,
    };
    let path = match find_binary(adapter, binary) {
        Ok(path) => path,
        // Not installed says it all, but a setting that points nowhere or a refusal needs why.
        Err(BinaryProblem::NotInstalled(_)) => return check,
        Err(problem) => {
            check.problem = Some(problem.to_string());
            return check;
        }
    };
    match read_version(&path, adapter.version_args(), timeout).await {
        Ok(output) => {
            check.version = adapter.parse_version(&output);
            if check.version.is_none() {
                check.problem = Some(format!(
                    "could not find a version in {:?}",
                    first_line(&output)
                ));
            }
        }
        Err(problem) => check.problem = Some(problem),
    }
    if let Some(version) = &check.version {
        let flags = probe_flags_within(&path, adapter.id(), timeout).await;
        check.status = match version_fit(version, adapter.tested_versions()) {
            _ if !flags.complete() => AgentStatus::Untested,
            VersionFit::Tested => AgentStatus::Tested,
            VersionFit::Newer => AgentStatus::Newer,
            VersionFit::Untested => AgentStatus::Untested,
        };
        if !flags.missing.is_empty() {
            check.problem = Some(format!(
                "--help lacks flags Argonauts passes: {}",
                flags.missing.join(", ")
            ));
        } else if let Some(problem) = flags.problem {
            check.problem = Some(problem);
        }
    } else {
        check.status = AgentStatus::UnknownVersion;
    }
    check.path = Some(path);
    check
}

/// The version of the CLI `adapter` runs, or `None` when it can't be found or read.
pub async fn installed_version(adapter: &dyn Adapter, binary: Option<&Path>) -> Option<String> {
    let path = find_binary(adapter, binary).ok()?;
    let output = read_version(&path, adapter.version_args(), VERSION_TIMEOUT)
        .await
        .ok()?;
    adapter.parse_version(&output)
}

/// The binary doctor and the quota probe run: `configured` (from `agents.<id>.binary`) when set,
/// else the adapter's binary on `PATH`. Both run outside the sandbox, so [`find_unsandboxed`]
/// refuses one a repo or an agent could plant.
pub fn resolve_binary(adapter: &dyn Adapter, configured: Option<&Path>) -> Result<PathBuf, String> {
    find_binary(adapter, configured).map_err(|problem| problem.to_string())
}

#[derive(Debug, thiserror::Error)]
enum BinaryProblem {
    #[error("{0} is not installed")]
    NotInstalled(String),
    #[error("agents.{id}.binary is {}, which {why}", path.display())]
    Setting {
        id: String,
        path: PathBuf,
        why: &'static str,
    },
    #[error(transparent)]
    Refused(BinaryError),
}

fn find_binary(adapter: &dyn Adapter, configured: Option<&Path>) -> Result<PathBuf, BinaryProblem> {
    let Some(path) = configured else {
        return find_unsandboxed(adapter.binary(), None).map_err(|err| match err {
            BinaryError::NotInstalled(name) => BinaryProblem::NotInstalled(name),
            refused => BinaryProblem::Refused(refused),
        });
    };
    let setting = |why| BinaryProblem::Setting {
        id: adapter.id().to_owned(),
        path: path.to_path_buf(),
        why,
    };
    let program = path.to_str().ok_or_else(|| setting("isn't valid UTF-8"))?;
    // A name is looked up on `PATH`, but a relative path would depend on where you run from.
    if program.contains('/') && !path.is_absolute() {
        return Err(setting("isn't an absolute path"));
    }
    find_unsandboxed(program, None).map_err(|err| match err {
        BinaryError::NotInstalled(_) => setting("doesn't exist"),
        refused => BinaryProblem::Refused(refused),
    })
}

/// Runs the version command and returns stdout followed by stderr, whatever its exit status.
async fn read_version(path: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    command::ask(path, args, timeout)
        .await
        .map(|answer| answer.text)
}

fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

impl fmt::Display for DoctorReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name_width = self
            .agents
            .iter()
            .map(|a| a.display_name.chars().count())
            .max()
            .unwrap_or(0)
            .max(3);
        for agent in &self.agents {
            let version = agent.version.as_deref().unwrap_or("-");
            write!(
                f,
                "{:name_width$}  {:10}  {:18}",
                agent.display_name,
                version,
                agent.status.label()
            )?;
            if !agent.enabled {
                write!(f, "  off")?;
            }
            if agent.status != AgentStatus::NotInstalled {
                write!(f, "  cheapest: {}", agent.cheapest_model)?;
                if let Some(effort) = &agent.cheapest_effort {
                    write!(f, " (effort {effort})")?;
                }
            }
            if agent.status == AgentStatus::Untested && !agent.tested_versions.is_empty() {
                write!(f, "  checked with {}", agent.tested_versions.join(", "))?;
            }
            if let Some(problem) = &agent.problem {
                write!(f, "  {problem}")?;
            }
            writeln!(f)?;
        }
        match (&self.git.version, &self.git.problem) {
            (Some(version), _) => writeln!(f, "{:name_width$}  {version}", "git"),
            (None, Some(problem)) => writeln!(f, "{:name_width$}  {problem}", "git"),
            (None, None) => writeln!(f, "{:name_width$}  missing", "git"),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use argonauts_core::adapter::Transport;
    use argonauts_core::settings::sessions::Compaction;
    use argonauts_core::{
        CommandSpec, LaunchTweak, SessionRequest, SetupHints, StopPlan, StreamParser,
        extract_version,
    };

    /// A harness for doctor's tests: any adapter id, a scripted binary or none at all, and the
    /// compaction, login check, and transport a test needs.
    pub(crate) struct FakeHarness {
        id: &'static str,
        binary: &'static str,
        tested: &'static [&'static str],
        compaction: Option<fn(Compaction) -> LaunchTweak>,
        login_check: Option<Vec<String>>,
        transport: Transport,
        update: Option<&'static [&'static str]>,
    }

    impl FakeHarness {
        /// Not installed.
        pub(crate) fn named(id: &'static str) -> Self {
            Self {
                id,
                binary: "argonauts-no-such-cli",
                tested: &[],
                compaction: None,
                login_check: None,
                transport: Transport::Lines,
                update: None,
            }
        }

        /// Installed as a shell script with `body`, which sees the arguments as `$@`.
        pub(crate) fn script(
            id: &'static str,
            dir: &Path,
            body: &str,
            tested: &'static [&'static str],
        ) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.join(format!("{id}-cli"));
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            Self {
                binary: Box::leak(path.display().to_string().into_boxed_str()),
                tested,
                ..Self::named(id)
            }
        }

        pub(crate) fn with_compaction(mut self, tweak: fn(Compaction) -> LaunchTweak) -> Self {
            self.compaction = Some(tweak);
            self
        }

        pub(crate) fn with_login_check(mut self, args: &[&str]) -> Self {
            self.login_check = Some(args.iter().map(|a| (*a).to_owned()).collect());
            self
        }

        pub(crate) fn with_update(mut self, args: &'static [&'static str]) -> Self {
            self.update = Some(args);
            self
        }

        pub(crate) fn acp(mut self) -> Self {
            self.transport = Transport::Acp;
            self
        }
    }

    impl Adapter for FakeHarness {
        fn id(&self) -> &'static str {
            self.id
        }
        fn display_name(&self) -> &'static str {
            self.id
        }
        fn binary(&self) -> &'static str {
            self.binary
        }
        fn command(&self, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
            Ok(CommandSpec::new(self.binary, &req.workdir))
        }
        fn parser(&self) -> Box<dyn StreamParser> {
            unimplemented!("doctor never parses sessions")
        }
        fn version_args(&self) -> &'static [&'static str] {
            &["--version"]
        }
        fn parse_version(&self, output: &str) -> Option<String> {
            extract_version(output)
        }
        fn tested_versions(&self) -> &'static [&'static str] {
            self.tested
        }
        fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
            ("tiny", None)
        }
        fn stop_plan(&self) -> StopPlan {
            StopPlan::default()
        }
        fn compaction(&self, setting: Compaction) -> LaunchTweak {
            match self.compaction {
                Some(tweak) => tweak(setting),
                None if setting == Compaction::Inherit => LaunchTweak::default(),
                None => LaunchTweak {
                    effect: argonauts_core::TweakEffect::NotPossible {
                        note: "the fake has no compaction control".to_owned(),
                    },
                    ..LaunchTweak::default()
                },
            }
        }
        fn update_args(&self) -> Option<&'static [&'static str]> {
            self.update
        }
        fn setup_hints(&self) -> SetupHints {
            SetupHints {
                login_check: self.login_check.clone(),
                login: Some(format!("Run `{} login`.", self.id)),
                ..SetupHints::default()
            }
        }
        fn transport(&self) -> Transport {
            self.transport
        }
    }

    /// A Codex-like CLI whose `--version` and `--help` come from the test.
    fn codex_like(dir: &Path, version: &str, help: &str) -> FakeHarness {
        let body = format!(
            "case \"$*\" in\n  --version) echo 'codex-cli {version}' ;;\n  --help) echo 'Commands: exec' ;;\n  'exec --help') printf '%s\\n' '{help}' ;;\nesac"
        );
        FakeHarness::script("codex", dir, &body, &["0.158.0", "0.158.3"])
    }

    #[tokio::test]
    async fn doctor_judges_versions_by_help_flags_and_ranges() {
        let all_flags = versions::required_flags("codex")
            .iter()
            .map(|flag| format!("  {flag} <value>"))
            .collect::<Vec<_>>()
            .join("\n");
        let without_schema = all_flags.replace("--output-schema", "--output-scheme");

        let dir = tempfile::tempdir().unwrap();
        let newer_patch = codex_like(dir.path(), "0.158.9", &all_flags);
        let check = check_agent(&newer_patch, VERSION_TIMEOUT).await;
        assert_eq!(check.version.as_deref(), Some("0.158.9"));
        assert_eq!(check.status, AgentStatus::Newer, "{check:?}");
        assert_eq!(check.problem, None);

        let dir = tempfile::tempdir().unwrap();
        let lacking = codex_like(dir.path(), "0.158.3", &without_schema);
        let check = check_agent(&lacking, VERSION_TIMEOUT).await;
        assert_eq!(check.status, AgentStatus::Untested);
        assert_eq!(
            check.problem.as_deref(),
            Some("--help lacks flags Argonauts passes: --output-schema")
        );

        for (version, why) in [
            ("0.159.0", "the next minor"),
            ("0.157.9", "before the oldest"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let outside = codex_like(dir.path(), version, &all_flags);
            let check = check_agent(&outside, VERSION_TIMEOUT).await;
            assert_eq!(check.status, AgentStatus::Untested, "{why}");
            assert_eq!(check.problem, None, "{why}");
        }

        // `agents.<id>.binary` wins over PATH, and a missing one says so.
        let dir = tempfile::tempdir().unwrap();
        let configured = codex_like(dir.path(), "0.158.0", &all_flags);
        let path = PathBuf::from(configured.binary);
        let named = FakeHarness {
            tested: &["0.158.0"],
            ..FakeHarness::named("codex")
        };
        let check = check_agent_at(&named, Some(&path), VERSION_TIMEOUT).await;
        assert_eq!(check.status, AgentStatus::Tested, "{check:?}");
        assert_eq!(check.path.as_deref(), Some(path.as_path()));
        let gone = dir.path().join("gone");
        let check = check_agent_at(&named, Some(&gone), VERSION_TIMEOUT).await;
        assert_eq!(check.status, AgentStatus::NotInstalled);
        assert!(check.problem.unwrap().contains("doesn't exist"));
        let relative = Path::new("bin/codex");
        let check = check_agent_at(&named, Some(relative), VERSION_TIMEOUT).await;
        assert_eq!(check.status, AgentStatus::NotInstalled);
        assert_eq!(
            check.problem.as_deref(),
            Some("agents.codex.binary is bin/codex, which isn't an absolute path")
        );
    }

    struct VersionOnly {
        binary: &'static str,
        tested: &'static [&'static str],
    }

    impl VersionOnly {
        fn script(dir: &Path, body: &str, tested: &'static [&'static str]) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.join("fake-cli");
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            let binary: &'static str = Box::leak(path.display().to_string().into_boxed_str());
            Self { binary, tested }
        }
    }

    impl Adapter for VersionOnly {
        fn id(&self) -> &'static str {
            "fake"
        }
        fn display_name(&self) -> &'static str {
            "Fake"
        }
        fn binary(&self) -> &'static str {
            self.binary
        }
        fn command(&self, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
            Ok(CommandSpec::new(self.binary, &req.workdir))
        }
        fn parser(&self) -> Box<dyn StreamParser> {
            unimplemented!("doctor never parses sessions")
        }
        fn version_args(&self) -> &'static [&'static str] {
            &["--version"]
        }
        fn parse_version(&self, output: &str) -> Option<String> {
            extract_version(output)
        }
        fn tested_versions(&self) -> &'static [&'static str] {
            self.tested
        }
        fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
            ("tiny", Some("low"))
        }
        fn stop_plan(&self) -> StopPlan {
            StopPlan::default()
        }
    }

    #[tokio::test]
    async fn tested_version() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = VersionOnly::script(dir.path(), "echo \"fake-cli $1 1.2.3\"", &["1.2.3"]);
        let check = check_agent(&adapter, VERSION_TIMEOUT).await;
        assert_eq!(check.status, AgentStatus::Tested);
        assert_eq!(check.version.as_deref(), Some("1.2.3"));
        assert_eq!(check.path.as_deref(), Some(Path::new(adapter.binary)));
        assert_eq!(check.cheapest_model, "tiny");
        assert_eq!(check.cheapest_effort.as_deref(), Some("low"));
        assert_eq!(check.problem, None);
    }

    #[tokio::test]
    async fn untested_version_from_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = VersionOnly::script(dir.path(), "echo 'v2.0.0' >&2", &["1.2.3"]);
        let check = check_agent(&adapter, VERSION_TIMEOUT).await;
        assert_eq!(check.status, AgentStatus::Untested);
        assert_eq!(check.version.as_deref(), Some("2.0.0"));
    }

    #[tokio::test]
    async fn unreadable_and_hanging_versions() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = VersionOnly::script(dir.path(), "echo 'no idea'", &[]);
        let check = check_agent(&adapter, VERSION_TIMEOUT).await;
        assert_eq!(check.status, AgentStatus::UnknownVersion);
        assert!(check.problem.unwrap().contains("no idea"));

        let dir = tempfile::tempdir().unwrap();
        let adapter = VersionOnly::script(dir.path(), "sleep 30", &[]);
        let check = check_agent(&adapter, Duration::from_millis(200)).await;
        assert_eq!(check.status, AgentStatus::UnknownVersion);
        assert!(check.problem.unwrap().contains("timed out"));
    }

    #[tokio::test]
    async fn missing_cli_and_report_rendering() {
        let missing = VersionOnly {
            binary: "argonauts-no-such-cli",
            tested: &["1.0.0"],
        };
        let dir = tempfile::tempdir().unwrap();
        // 1.1.0 is past the tested range, which ends before the next minor after 1.0.0.
        let present = VersionOnly::script(dir.path(), "echo 1.1.0", &["1.0.0"]);
        let adapters: Vec<Arc<dyn Adapter>> = vec![Arc::new(missing), Arc::new(present)];
        let report = doctor(&adapters, &Settings::default()).await;
        assert_eq!(report.agents[0].status, AgentStatus::NotInstalled);
        assert_eq!(report.agents[0].path, None);
        assert_eq!(report.agents[1].status, AgentStatus::Untested);
        assert!(report.git.version.is_some());
        assert!(report.ready());

        let text = report.to_string();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("Fake  -"), "{text}");
        assert!(lines[0].contains("not installed"));
        assert!(!lines[0].contains("cheapest"));
        assert!(lines[1].contains("1.1.0"));
        assert!(lines[1].contains("cheapest: tiny (effort low)"));
        assert!(lines[1].contains("checked with 1.0.0"));
        assert!(lines[2].starts_with("git "));

        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["agents"][0]["status"], "not_installed");
    }
}
