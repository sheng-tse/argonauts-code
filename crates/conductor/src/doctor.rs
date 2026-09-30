//! Backs `argonauts doctor`, which finds the installed agent CLIs and whether their versions are
//! ones the parsers were tested against. It only runs version commands, never a model.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use argonauts_core::settings::Settings;
use argonauts_core::{Adapter, StopSignal, VENDOR_KEY_ENV_VARS};
use serde::Serialize;
use tokio::process::Command;

use crate::git::Git;
use crate::process::signal_group;

pub const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Tested,
    /// Installed, at a version nobody has recorded yet. It may still work.
    Untested,
    UnknownVersion,
    NotInstalled,
}

impl AgentStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Tested => "tested",
            Self::Untested => "untested",
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
    /// Whether git works and at least one agent CLI is installed.
    pub fn ready(&self) -> bool {
        self.git.version.is_some()
            && self
                .agents
                .iter()
                .any(|a| a.status != AgentStatus::NotInstalled)
    }
}

pub async fn doctor(adapters: &[Arc<dyn Adapter>], settings: &Settings) -> DoctorReport {
    let _ = settings;
    let mut agents = Vec::with_capacity(adapters.len());
    for adapter in adapters {
        agents.push(check_agent(adapter.as_ref(), VERSION_TIMEOUT).await);
    }
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
    let (model, effort) = adapter.cheapest_model();
    let mut check = AgentCheck {
        id: adapter.id().to_owned(),
        display_name: adapter.display_name().to_owned(),
        binary: adapter.binary().to_owned(),
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
    };
    let Ok(path) = which::which(adapter.binary()) else {
        return check;
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
    check.status = match &check.version {
        None => AgentStatus::UnknownVersion,
        Some(v) if check.tested_versions.contains(v) => AgentStatus::Tested,
        Some(_) => AgentStatus::Untested,
    };
    check.path = Some(path);
    check
}

/// Runs the version command and returns stdout followed by stderr.
async fn read_version(path: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut command = Command::new(path);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    for name in VENDOR_KEY_ENV_VARS {
        command.env_remove(name);
    }
    let child = command
        .spawn()
        .map_err(|err| format!("could not run {}: {err}", path.display()))?;
    let group = child.id();
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => return Err(format!("could not run {}: {err}", path.display())),
        Err(_) => {
            if let Some(group) = group {
                let _ = signal_group(group, StopSignal::Kill);
            }
            return Err(format!("`{} {}` timed out", path.display(), args.join(" ")));
        }
    };
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(text)
}

fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

impl fmt::Display for DoctorReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let id_width = self
            .agents
            .iter()
            .map(|a| a.id.len())
            .max()
            .unwrap_or(0)
            .max(3);
        for agent in &self.agents {
            let version = agent.version.as_deref().unwrap_or("-");
            write!(
                f,
                "{:id_width$}  {:10}  {:15}",
                agent.id,
                version,
                agent.status.label()
            )?;
            if agent.status != AgentStatus::NotInstalled {
                write!(f, "  cheapest: {}", agent.cheapest_model)?;
                if let Some(effort) = &agent.cheapest_effort {
                    write!(f, " (effort {effort})")?;
                }
            }
            if agent.status == AgentStatus::Untested && !agent.tested_versions.is_empty() {
                write!(f, "  tested with {}", agent.tested_versions.join(", "))?;
            }
            if let Some(problem) = &agent.problem {
                write!(f, "  {problem}")?;
            }
            writeln!(f)?;
        }
        match (&self.git.version, &self.git.problem) {
            (Some(version), _) => writeln!(f, "{:id_width$}  {version}", "git"),
            (None, Some(problem)) => writeln!(f, "{:id_width$}  {problem}", "git"),
            (None, None) => writeln!(f, "{:id_width$}  missing", "git"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::{CommandSpec, SessionRequest, StopPlan, StreamParser, extract_version};

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
        let present = VersionOnly::script(dir.path(), "echo 1.0.1", &["1.0.0"]);
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
        assert!(lines[0].starts_with("fake  -"), "{text}");
        assert!(lines[0].contains("not installed"));
        assert!(!lines[0].contains("cheapest"));
        assert!(lines[1].contains("1.0.1"));
        assert!(lines[1].contains("cheapest: tiny (effort low)"));
        assert!(lines[1].contains("tested with 1.0.0"));
        assert!(lines[2].starts_with("git "));

        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["agents"][0]["status"], "not_installed");
    }
}
