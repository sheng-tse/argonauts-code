//! What the setup looks at without logging in or calling a model: the agent CLIs you have, local
//! model servers, and a CLI's login state where its adapter has a verified check.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use argonauts_conductor::boards::cli_versions::{CliVersions, Installed, version_check};
use argonauts_conductor::doctor::versions::newest_tested;
use argonauts_conductor::{AgentStatus, DoctorReport};
use argonauts_core::Adapter;
use argonauts_core::agents::adapter::SetupHints;
use argonauts_core::agents::runtimes::{RUNTIMES, RuntimeKind, runtime};
use argonauts_core::app::status_tab::CliVersionCheck;
use argonauts_core::base::names::ModelRef;
use argonauts_core::settings::Settings;
use argonauts_endpoints::session::runtime_api;

/// Each server probe waits at most this long.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// A login check starts the CLI itself, which takes longer than a server takes to answer.
pub(crate) const LOGIN_TIMEOUT: Duration = Duration::from_secs(5);

/// A local runtime that said what it is, with the models it serves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LocalServer {
    /// The runtime's id, such as `llamacpp`.
    pub id: String,
    pub base_url: String,
    pub models: Vec<String>,
    /// The window each model holds, in tokens, where the server says before loading it.
    pub windows: BTreeMap<String, u64>,
    /// The models the server sizes only as it loads them, as Ollama does a tag with no window
    /// of its own, so nothing says before then whether a review fits.
    pub sized_as_loaded: Vec<String>,
}

impl LocalServer {
    pub(crate) fn name(&self) -> &str {
        runtime(&self.id).map_or(self.id.as_str(), |runtime| runtime.name)
    }

    /// Its ports are ones other servers use too, so the setup writes its table once it has
    /// said what it is, and only then may a session reach it.
    pub(crate) fn needs_its_table(&self) -> bool {
        runtime(&self.id).is_some_and(|runtime| runtime.common_ports)
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
    /// The kinds of events this version sent that Argonauts Code doesn't read yet, with counts.
    pub unread: Vec<(String, u64)>,
    /// Built-in plugins a later version loaded that Argonauts Code doesn't know yet.
    pub builtins: Vec<String>,
    /// What Doctor's check of it found wrong, such as flags its `--help` lacks.
    pub problem: Option<String>,
    /// What this version's start-up message stopped reporting, each as a sentence.
    pub lacks: Vec<String>,
    /// How its version stands and what there is to update to.
    pub check: CliVersionCheck,
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

    pub(crate) fn worker(&self) -> ModelRef {
        ModelRef::new(self.id.clone(), None)
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
                problem: check.and_then(|check| check.problem.clone()),
                lacks: Vec::new(),
                // What sessions have shown of this version is added where Doctor reports.
                check: check.map_or_else(CliVersionCheck::default, |check| {
                    version_check(
                        adapter.as_ref(),
                        Installed::of(check),
                        &CliVersions::default(),
                    )
                }),
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

/// Every known runtime, each at your table's `base_url` or else its usual addresses in turn
/// (llama-server's 9931, then 8080), all at once and each within [`PROBE_TIMEOUT`]. A server
/// counts only when it says in its own API which runtime it is, so another app on port 8000 or
/// 8080 is never taken for one, and only when it serves at least one model.
pub(crate) async fn local_servers(settings: &Settings) -> Vec<LocalServer> {
    let Some(client) = runtime_api::local_client(PROBE_TIMEOUT) else {
        return Vec::new();
    };
    let probes = RUNTIMES.iter().map(|runtime| {
        let yours = settings.endpoints.get(runtime.id);
        // Without a table of yours, a server is asked where it listens by default, unless
        // you turned off asking the servers on this computer.
        let addresses: Vec<String> = match yours.and_then(|table| table.base_url.clone()) {
            Some(base_url) => vec![base_url],
            None if !settings.registry.local_servers => Vec::new(),
            None => runtime
                .addresses
                .iter()
                .map(|url| (*url).to_owned())
                .collect(),
        };
        let token = yours
            .and_then(|table| table.api_key_env.as_deref())
            .and_then(|variable| std::env::var(variable).ok());
        let client = client.clone();
        async move {
            let asked = addresses.iter().map(|base_url| {
                runtime_api::identify(&client, runtime, base_url, token.as_deref())
            });
            let answered = tokio::time::timeout(PROBE_TIMEOUT, futures::future::join_all(asked))
                .await
                .ok()?;
            let served = answered.into_iter().flatten().next()?;
            let (windows, sized_as_loaded) = match runtime.kind {
                RuntimeKind::Ollama => {
                    ollama_windows(&client, &served.base_url, &served.models).await
                }
                _ => Default::default(),
            };
            Some(LocalServer {
                id: runtime.id.to_owned(),
                base_url: served.base_url,
                models: served.models,
                windows,
                sized_as_loaded,
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

/// What `/api/show` says of each Ollama tag without loading it: the window a tag holds itself to
/// (`num_ctx`), or that it has none and is sized as it loads. A tag whose facts don't come back
/// is in neither. All are asked at once, within [`PROBE_TIMEOUT`].
async fn ollama_windows(
    client: &reqwest::Client,
    base_url: &str,
    tags: &[String],
) -> (BTreeMap<String, u64>, Vec<String>) {
    let root = base_url.trim_end_matches('/');
    let root = root.strip_suffix("/v1").unwrap_or(root);
    let url = format!("{root}/api/show");
    let asked = tags.iter().map(|tag| {
        let request = client.post(&url).json(&serde_json::json!({ "model": tag }));
        async move {
            let reply = request.send().await.ok()?.error_for_status().ok()?;
            let text = reply.text().await.ok()?;
            let shown = argonauts_registry::models::discovery::parse_show(&text).ok()?;
            Some((tag.clone(), shown.cap))
        }
    });
    let shown = tokio::time::timeout(PROBE_TIMEOUT, futures::future::join_all(asked))
        .await
        .unwrap_or_default();
    let mut windows = BTreeMap::new();
    let mut sized_as_loaded = Vec::new();
    for (tag, cap) in shown.into_iter().flatten() {
        match cap {
            Some(window) => {
                windows.insert(tag, window);
            }
            None => sized_as_loaded.push(tag),
        }
    }
    (windows, sized_as_loaded)
}

#[cfg(test)]
mod tests {

    use argonauts_testkit::{FakeKind, FakeModelServer};

    use super::*;

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        argonauts_testkit::write_script(&path, &format!("#!/bin/sh\n{body}\n")).unwrap();
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
            problem: None,
            lacks: Vec::new(),
            check: Default::default(),
        };
        let agents = vec![
            agent("codex", Some(vec!["login".to_owned(), "status".to_owned()])),
            agent("claude-code", None),
        ];
        let states = logins(&agents).await;
        assert_eq!(states, vec![("codex".to_owned(), LoginState::LoggedOut)]);
    }

    fn table(base_url: &str) -> argonauts_core::settings::agents::EndpointSettings {
        serde_json::from_value(serde_json::json!({ "base_url": base_url })).unwrap()
    }

    /// Nothing listens on port 9 here, so a runtime pointed there is found nowhere, fast.
    fn nowhere(settings: &mut Settings, except: &[&str]) {
        for runtime in RUNTIMES
            .iter()
            .filter(|runtime| !except.contains(&runtime.id))
        {
            settings
                .endpoints
                .insert(runtime.id.to_owned(), table("http://127.0.0.1:9/v1"));
        }
    }

    #[tokio::test]
    async fn a_server_is_probed_at_its_resolved_address() {
        let server = FakeModelServer::start(FakeKind::Ollama, &["llama3.2", "qwen3"]).unwrap();
        let mut settings = Settings::default();
        settings
            .endpoints
            .insert("ollama".to_owned(), table(&server.base_url()));
        nowhere(&mut settings, &["ollama"]);
        let servers = local_servers(&settings).await;
        assert_eq!(
            servers,
            vec![LocalServer {
                id: "ollama".to_owned(),
                base_url: server.base_url(),
                models: vec!["llama3.2".to_owned(), "qwen3".to_owned()],
                // The fake has no `/api/show`, so nothing is known of either tag's window.
                ..Default::default()
            }]
        );
    }

    /// An Ollama that answers `/api/show` as the real one does: `num_ctx` in the parameters of a
    /// tag made with a window of its own, nothing there for one sized as it loads.
    fn ollama_show_server() -> std::net::SocketAddr {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0; length];
                let _ = reader.read_exact(&mut body);
                let asked: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                let parameters = match asked["model"].as_str() {
                    Some("qwen3-coder:30b-64k") => "num_ctx                        65536",
                    _ => "temperature 0.7",
                };
                let reply = serde_json::json!({ "parameters": parameters, "model_info": {} });
                let reply = reply.to_string();
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                     connection: close\r\n\r\n{reply}",
                    reply.len()
                );
            }
        });
        addr
    }

    #[tokio::test]
    async fn an_ollama_tag_has_a_window_only_when_it_sets_its_own() {
        let addr = ollama_show_server();
        let client = runtime_api::local_client(PROBE_TIMEOUT).unwrap();
        let tags = [
            "qwen3-coder:30b-64k".to_owned(),
            "qwen3-coder:30b".to_owned(),
        ];
        let (windows, sized) = ollama_windows(&client, &format!("http://{addr}/v1"), &tags).await;
        assert_eq!(windows, [("qwen3-coder:30b-64k".to_owned(), 65_536)].into());
        assert_eq!(sized, ["qwen3-coder:30b"]);
    }

    /// Off, the setup asks only the servers you have a table for, never one where it listens
    /// by default, such as an Ollama of yours.
    #[tokio::test]
    async fn with_local_servers_off_only_your_tables_are_asked() {
        let server = FakeModelServer::start(FakeKind::Ollama, &["llama3.2"]).unwrap();
        let mut settings = Settings::default();
        settings.registry.local_servers = false;
        assert!(local_servers(&settings).await.is_empty());
        settings
            .endpoints
            .insert("ollama".to_owned(), table(&server.base_url()));
        let found: Vec<String> = local_servers(&settings)
            .await
            .into_iter()
            .map(|server| server.id)
            .collect();
        assert_eq!(found, ["ollama"]);
    }

    #[tokio::test]
    async fn all_four_runtimes_are_found_and_named() {
        let ollama = FakeModelServer::start(FakeKind::Ollama, &["llama3.2"]).unwrap();
        let lm_studio = FakeModelServer::start(FakeKind::LmStudio, &["qwen/qwen3-8b"]).unwrap();
        let llama = FakeKind::LlamaServer {
            slots: 2,
            router: false,
        };
        let llama = FakeModelServer::start(llama, &["qwen3-8b-q4.gguf"]).unwrap();
        let vllm = FakeKind::Vllm {
            max_model_len: 32768,
        };
        let vllm = FakeModelServer::start(vllm, &["Qwen/Qwen3-8B"]).unwrap();
        let mut settings = Settings::default();
        for (id, server) in [
            ("ollama", &ollama),
            ("lmstudio", &lm_studio),
            ("llamacpp", &llama),
            ("vllm", &vllm),
        ] {
            settings
                .endpoints
                .insert(id.to_owned(), table(&server.base_url()));
        }
        let found = local_servers(&settings).await;
        let names: Vec<(&str, &str)> = found
            .iter()
            .map(|server| (server.name(), server.models[0].as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("Ollama", "llama3.2"),
                ("LM Studio", "qwen/qwen3-8b"),
                ("llama-server", "qwen3-8b-q4.gguf"),
                ("vLLM", "Qwen/Qwen3-8B"),
            ]
        );
        let tables: Vec<bool> = found.iter().map(LocalServer::needs_its_table).collect();
        assert_eq!(tables, [false, false, true, true]);
    }

    #[tokio::test]
    async fn another_app_on_a_runtimes_port_is_not_taken_for_it() {
        let other = FakeModelServer::start(FakeKind::Other, &["some-model"]).unwrap();
        let mut settings = Settings::default();
        for runtime in RUNTIMES {
            settings
                .endpoints
                .insert(runtime.id.to_owned(), table(&other.base_url()));
        }
        assert!(local_servers(&settings).await.is_empty());
    }

    #[tokio::test]
    async fn a_server_with_no_models_is_left_out() {
        let server = FakeModelServer::start(FakeKind::Ollama, &[]).unwrap();
        let mut settings = Settings::default();
        settings
            .endpoints
            .insert("ollama".to_owned(), table(&server.base_url()));
        nowhere(&mut settings, &["ollama"]);
        assert!(local_servers(&settings).await.is_empty());
    }
}
