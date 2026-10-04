//! What the setup looks at without logging in or calling a model: the agent CLIs you have, local
//! model servers, and a CLI's login state where its adapter has a verified check.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use argonauts_conductor::doctor::versions::newest_tested;
use argonauts_conductor::{AgentStatus, DoctorReport};
use argonauts_core::Adapter;
use argonauts_core::adapter::SetupHints;
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::Settings;
use serde::Deserialize;

/// Each server probe waits at most this long.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// A login check starts the CLI itself, which takes longer than a server takes to answer.
pub(crate) const LOGIN_TIMEOUT: Duration = Duration::from_secs(5);

/// The servers the setup looks for, at their usual addresses when the settings name none.
pub(crate) const LOCAL_SERVERS: &[(&str, &str)] = &[
    ("ollama", "http://localhost:11434/v1"),
    ("lmstudio", "http://127.0.0.1:1234/v1"),
];

/// A server that answered `GET {base_url}/models`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalServer {
    /// `ollama` or `lmstudio`.
    pub id: String,
    pub base_url: String,
    pub models: Vec<String>,
}

impl LocalServer {
    pub(crate) fn name(&self) -> &str {
        match self.id.as_str() {
            "ollama" => "Ollama",
            "lmstudio" => "LM Studio",
            other => other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoginState {
    LoggedIn,
    LoggedOut,
    /// The adapter has no verified check, so the setup claims nothing.
    Unknown,
}

/// One agent CLI as the setup shows it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FoundAgent {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    /// Where the CLI was found. `None` when it isn't installed.
    pub path: Option<PathBuf>,
    pub status: AgentStatus,
    /// `agents.<id>.enabled`.
    pub enabled: bool,
    pub hints: SetupHints,
    pub login: LoginState,
    /// The newest version its adapter was recorded with.
    pub newest_tested: Option<String>,
    /// The kinds of events this version sent that Argonauts doesn't read yet, with counts.
    pub unread: Vec<(String, u64)>,
    /// Built-in plugins a later version loaded that Argonauts doesn't know yet.
    pub builtins: Vec<String>,
}

impl FoundAgent {
    pub(crate) fn installed(&self) -> bool {
        self.status != AgentStatus::NotInstalled
    }

    /// Installed, turned on, and not known to be logged out. An untested version counts, since
    /// it may still work.
    pub(crate) fn ready(&self) -> bool {
        self.installed() && self.enabled && self.login != LoginState::LoggedOut
    }

    pub(crate) fn worker(&self) -> WorkerRef {
        WorkerRef::new(self.id.clone(), None)
    }

    /// What to run to make it ready, when there's one command for it.
    pub(crate) fn fix(&self) -> Option<&str> {
        if !self.installed() {
            self.hints.install.as_deref()
        } else if self.login == LoginState::LoggedOut {
            self.hints.login.as_deref()
        } else {
            None
        }
    }
}

/// Each adapter as the doctor found it, in the adapters' order. Login states come later.
pub(crate) fn found_agents(
    adapters: &[Arc<dyn Adapter>],
    report: &DoctorReport,
    settings: &Settings,
) -> Vec<FoundAgent> {
    adapters
        .iter()
        .map(|adapter| {
            let check = report.agents.iter().find(|check| check.id == adapter.id());
            FoundAgent {
                id: adapter.id().to_owned(),
                name: adapter.display_name().to_owned(),
                version: check.and_then(|check| check.version.clone()),
                path: check.and_then(|check| check.path.clone()),
                status: check.map_or(AgentStatus::NotInstalled, |check| check.status),
                enabled: settings
                    .agents
                    .get(adapter.id())
                    .is_none_or(|agent| agent.enabled),
                hints: adapter.setup_hints(),
                login: LoginState::Unknown,
                newest_tested: newest_tested(adapter.tested_versions()).map(str::to_owned),
                unread: Vec::new(),
                builtins: Vec::new(),
            }
        })
        .collect()
}

/// The login state of every installed agent that's on and whose adapter has a verified check,
/// all at once. One you turned off is never asked.
pub(crate) async fn logins(agents: &[FoundAgent]) -> Vec<(String, LoginState)> {
    let checks = agents
        .iter()
        .filter(|agent| agent.enabled)
        .filter_map(|agent| {
            let program = agent.path.as_ref()?;
            let args = agent.hints.login_check.as_ref()?;
            let id = agent.id.clone();
            Some(async move { (id, login_state(program, args).await) })
        });
    futures::future::join_all(checks).await
}

/// Runs the check: exit 0 is logged in and exit 1 logged out. Anything else, including a check
/// that doesn't finish in time, claims nothing.
pub(crate) async fn login_state(program: &Path, args: &[String]) -> LoginState {
    let child = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status();
    match tokio::time::timeout(LOGIN_TIMEOUT, child).await {
        Ok(Ok(status)) => match status.code() {
            Some(0) => LoginState::LoggedIn,
            Some(1) => LoginState::LoggedOut,
            _ => LoginState::Unknown,
        },
        _ => LoginState::Unknown,
    }
}

/// `endpoints.ollama` and `endpoints.lmstudio` at their resolved `base_url`, or the built-in
/// address. Only servers that list at least one model are returned.
pub(crate) async fn local_servers(settings: &Settings) -> Vec<LocalServer> {
    // Local servers never go through a proxy.
    let Ok(client) = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .no_proxy()
        .build()
    else {
        return Vec::new();
    };
    let probes = LOCAL_SERVERS.iter().map(|(id, builtin)| {
        let base_url = settings
            .endpoints
            .get(*id)
            .and_then(|endpoint| endpoint.base_url.clone())
            .unwrap_or_else(|| (*builtin).to_owned());
        let client = client.clone();
        async move {
            let models = models_at(&client, &base_url).await?;
            Some(LocalServer {
                id: (*id).to_owned(),
                base_url,
                models,
            })
        }
    });
    futures::future::join_all(probes)
        .await
        .into_iter()
        .flatten()
        .filter(|server| !server.models.is_empty())
        .collect()
}

#[derive(Deserialize)]
struct ModelList {
    data: Vec<ModelItem>,
}

#[derive(Deserialize)]
struct ModelItem {
    id: String,
}

async fn models_at(client: &reqwest::Client, base_url: &str) -> Option<Vec<String>> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let list: ModelList = response.json().await.ok()?;
    Some(list.data.into_iter().map(|model| model.id).collect())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use argonauts_testkit::FakeOpenAi;

    use super::*;

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[tokio::test]
    async fn a_login_check_reads_only_exit_zero_and_one() {
        let dir = tempfile::tempdir().unwrap();
        let args = vec!["login".to_owned(), "status".to_owned()];
        let yes = script(dir.path(), "yes", "exit 0");
        let no = script(dir.path(), "no", "exit 1");
        let odd = script(dir.path(), "odd", "exit 7");
        assert_eq!(login_state(&yes, &args).await, LoginState::LoggedIn);
        assert_eq!(login_state(&no, &args).await, LoginState::LoggedOut);
        assert_eq!(login_state(&odd, &args).await, LoginState::Unknown);
        let missing = dir.path().join("missing");
        assert_eq!(login_state(&missing, &args).await, LoginState::Unknown);
    }

    #[tokio::test]
    async fn only_agents_with_a_verified_check_are_asked() {
        let dir = tempfile::tempdir().unwrap();
        let out = script(dir.path(), "codex", "exit 1");
        let agent = |id: &str, check: Option<Vec<String>>| FoundAgent {
            id: id.to_owned(),
            name: id.to_owned(),
            version: Some("1.0.0".to_owned()),
            path: Some(out.clone()),
            status: AgentStatus::Tested,
            enabled: true,
            hints: SetupHints {
                install: None,
                login: Some(format!("{id} login")),
                login_check: check,
            },
            login: LoginState::Unknown,
            newest_tested: None,
            unread: Vec::new(),
            builtins: Vec::new(),
        };
        let agents = vec![
            agent("codex", Some(vec!["login".to_owned(), "status".to_owned()])),
            agent("claude-code", None),
        ];
        let states = logins(&agents).await;
        assert_eq!(states, vec![("codex".to_owned(), LoginState::LoggedOut)]);
    }

    #[tokio::test]
    async fn a_server_is_probed_at_its_resolved_address() {
        let server = FakeOpenAi::start().unwrap();
        server.set_models(&["llama3.2", "qwen3"]);
        let mut settings = Settings::default();
        settings.endpoints.insert(
            "ollama".to_owned(),
            serde_json::from_value(serde_json::json!({ "base_url": server.base_url() })).unwrap(),
        );
        // Nothing listens on port 9 here, so LM Studio's probe fails fast.
        settings.endpoints.insert(
            "lmstudio".to_owned(),
            serde_json::from_value(serde_json::json!({ "base_url": "http://127.0.0.1:9/v1" }))
                .unwrap(),
        );
        let servers = local_servers(&settings).await;
        assert_eq!(
            servers,
            vec![LocalServer {
                id: "ollama".to_owned(),
                base_url: server.base_url(),
                models: vec!["llama3.2".to_owned(), "qwen3".to_owned()],
            }]
        );
    }

    #[tokio::test]
    async fn a_server_with_no_models_is_left_out() {
        let server = FakeOpenAi::start().unwrap();
        server.set_models(&[]);
        let mut settings = Settings::default();
        for (id, _) in LOCAL_SERVERS {
            settings.endpoints.insert(
                (*id).to_owned(),
                serde_json::from_value(serde_json::json!({ "base_url": server.base_url() }))
                    .unwrap(),
            );
        }
        assert!(local_servers(&settings).await.is_empty());
    }
}
