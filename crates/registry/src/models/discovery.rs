//! Discovery never makes a model call: `agy --output-format json models`, `codex debug
//! models`, Claude Code's aliases, and `GET {base_url}/models` for each configured endpoint.
//! Everything it finds has `discovered = true`, and your own entries win over it.

mod claude;
mod parse;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use argonauts_core::registry::ModelEntry;
use argonauts_core::settings::Settings;
use tokio::task::JoinSet;

use super::builtin::{CLAUDE_CODE_ALIASES, endpoint_settings, harness_access, harness_entry};

pub use claude::{
    ANTHROPIC_API, ANTHROPIC_KEY_ENV, ClaudeSource, FRESH_FOR as CLAUDE_LIST_FRESH_FOR,
    ListedModel, MODELS_PAGE, STATUS_PAGE, claude_code_entries, parse_anthropic_models,
    parse_model_pages,
};
pub use parse::{parse_agy_models, parse_codex_models, parse_openai_models};

/// A CLI that lists its models without a model call.
struct CliSource {
    adapter: &'static str,
    binary: &'static str,
    args: &'static [&'static str],
    parse: fn(&str) -> Result<Vec<ModelEntry>, String>,
}

/// agy 1.2.13 takes `--output-format` only before the subcommand: `agy models --output-format
/// json` fails with "flags provided but not defined".
const AGY: CliSource = CliSource {
    adapter: "antigravity-cli",
    binary: "agy",
    args: &["--output-format", "json", "models"],
    parse: parse_agy_models,
};

const CODEX: CliSource = CliSource {
    adapter: "codex",
    binary: "codex",
    args: &["debug", "models"],
    parse: parse_codex_models,
};

/// Listing models is quick. A CLI that hangs, say waiting on a login, is given up on.
const CLI_TIMEOUT: Duration = Duration::from_secs(30);
const ENDPOINT_TIMEOUT: Duration = Duration::from_secs(20);

/// A hosted catalog's model list runs to a few MiB at most. A longer one is given up on.
const MAX_MODELS_BYTES: usize = 16 * 1024 * 1024;

/// What discovery found, and each source it skipped with the reason.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Discovery {
    pub models: Vec<ModelEntry>,
    pub skipped: Vec<String>,
    /// The harness or endpoint ids whose source failed, so what they listed before still counts.
    pub failed: Vec<String>,
}

/// Runs each source `registry.discover` allows. A source that fails is skipped.
pub async fn discover(settings: &Settings) -> Vec<ModelEntry> {
    discover_with_reasons(settings).await.models
}

/// Like [`discover`], keeping why each skipped source failed so it can be shown.
pub async fn discover_with_reasons(settings: &Settings) -> Discovery {
    let mut found = Discovery::default();
    if !settings.registry.discover {
        return found;
    }
    if agent_enabled(settings, "claude-code") {
        found.models.extend(CLAUDE_CODE_ALIASES.iter().map(|alias| {
            let mut entry = harness_entry(settings, "claude-code", Some(alias));
            entry.discovered = true;
            entry
        }));
    }
    let (agy, codex, endpoints) = tokio::join!(
        harness_models(settings, &AGY),
        harness_models(settings, &CODEX),
        endpoint_models(settings),
    );
    let harnesses = [
        (AGY.adapter.to_owned(), agy),
        (CODEX.adapter.to_owned(), codex),
    ];
    for (id, result) in harnesses.into_iter().chain(endpoints) {
        match result {
            Ok(models) => found.models.extend(models),
            Err(reason) => {
                found.skipped.push(reason);
                found.failed.push(id);
            }
        }
    }
    found
}

pub(crate) fn agent_enabled(settings: &Settings, adapter: &str) -> bool {
    settings
        .agents
        .get(adapter)
        .is_none_or(|agent| agent.enabled)
}

async fn harness_models(
    settings: &Settings,
    source: &CliSource,
) -> Result<Vec<ModelEntry>, String> {
    if !agent_enabled(settings, source.adapter) {
        return Ok(Vec::new());
    }
    let program = settings
        .agents
        .get(source.adapter)
        .and_then(|agent| agent.binary.clone())
        .unwrap_or_else(|| PathBuf::from(source.binary));
    let stdout = run(&program, source.args).await?;
    let (access, cost) = harness_access(settings, source.adapter);
    Ok((source.parse)(&stdout)?
        .into_iter()
        .map(|mut entry| {
            entry.access = access;
            entry.cost = cost.clone();
            entry
        })
        .collect())
}

async fn run(program: &Path, args: &[&str]) -> Result<String, String> {
    let shown = format!("{} {}", program.display(), args.join(" "));
    let child = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Out of the repo, in case a CLI keeps state where it runs.
        .current_dir(std::env::temp_dir())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(CLI_TIMEOUT, child)
        .await
        .map_err(|_| format!("`{shown}` took longer than {}s", CLI_TIMEOUT.as_secs()))?
        .map_err(|e| format!("couldn't run `{shown}`: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let why = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        return Err(format!(
            "`{shown}` failed ({}): {}",
            output.status,
            why.trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|_| format!("`{shown}` printed text that isn't UTF-8"))
}

type Listing = (String, Result<Vec<ModelEntry>, String>);

/// Asks every endpoint at once, so slow ones cost one timeout between them, not one each. Each
/// result comes with its endpoint's id.
async fn endpoint_models(settings: &Settings) -> Vec<Listing> {
    let ids: Vec<String> = settings.endpoints.keys().cloned().collect();
    let client = match reqwest::Client::builder().timeout(ENDPOINT_TIMEOUT).build() {
        Ok(client) => client,
        Err(e) => {
            let why = format!("couldn't start an HTTP client: {e}");
            return ids.into_iter().map(|id| (id, Err(why.clone()))).collect();
        }
    };
    let mut results = Vec::new();
    let mut listings = JoinSet::new();
    for (index, id) in ids.iter().enumerate() {
        match models_request(&client, settings, id) {
            Ok(request) => {
                listings.spawn(async move { (index, request.list().await) });
            }
            Err(reason) => results.push((index, Err(reason))),
        }
    }
    while let Some(done) = listings.join_next().await {
        match done {
            Ok(result) => results.push(result),
            Err(e) => results.push((usize::MAX, Err(format!("an endpoint listing failed: {e}")))),
        }
    }
    // In settings order, whichever answered first.
    results.sort_by_key(|(index, _)| *index);
    results
        .into_iter()
        .map(|(index, result)| (ids.get(index).cloned().unwrap_or_default(), result))
        .collect()
}

struct ModelsRequest {
    id: String,
    url: String,
    local: bool,
    request: reqwest::RequestBuilder,
}

/// The key comes only from the variable `api_key_env` names and never goes into a message.
fn models_request(
    client: &reqwest::Client,
    settings: &Settings,
    id: &str,
) -> Result<ModelsRequest, String> {
    let endpoint =
        endpoint_settings(settings, id).ok_or_else(|| format!("endpoint {id} has no settings"))?;
    let base_url = endpoint
        .base_url
        .as_deref()
        .ok_or_else(|| format!("endpoint {id} has no base_url"))?;
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut request = client.get(&url);
    if let Some(key) = endpoint
        .api_key_env
        .as_deref()
        .and_then(|name| std::env::var(name).ok())
        .filter(|key| !key.is_empty())
    {
        request = request.bearer_auth(key);
    }
    for (header, variable) in &endpoint.header_env {
        if let Ok(value) = std::env::var(variable) {
            request = request.header(header.as_str(), value);
        }
    }
    Ok(ModelsRequest {
        id: id.to_owned(),
        url,
        local: endpoint.local,
        request,
    })
}

impl ModelsRequest {
    async fn list(self) -> Result<Vec<ModelEntry>, String> {
        let Self {
            id,
            url,
            local,
            request,
        } = self;
        let failed =
            |e: reqwest::Error| format!("endpoint {id}: GET {url} failed: {}", e.without_url());
        let unread =
            |e: reqwest::Error| format!("endpoint {id}: reading {url} failed: {}", e.without_url());
        let mut reply = request.send().await.map_err(failed)?;
        let status = reply.status();
        if !status.is_success() {
            return Err(format!("endpoint {id}: GET {url} answered {status}"));
        }
        let mut body = Vec::new();
        while let Some(chunk) = reply.chunk().await.map_err(unread)? {
            if body.len() + chunk.len() > MAX_MODELS_BYTES {
                let mib = MAX_MODELS_BYTES / (1024 * 1024);
                return Err(format!("endpoint {id}: GET {url} sent more than {mib} MiB"));
            }
            body.extend_from_slice(&chunk);
        }
        parse_openai_models(&id, local, &String::from_utf8_lossy(&body))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::time::Instant;

    use argonauts_core::AuthMode;
    use argonauts_core::registry::ModelAccess;
    use argonauts_core::settings::agents::{AgentSettings, EndpointSettings};
    use argonauts_testkit::openai::FakeOpenAi;

    use super::*;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

    /// A stand-in CLI that prints `fixture` only when called with exactly `args`.
    fn fake_cli(dir: &Path, name: &str, args: &str, fixture: &str) -> PathBuf {
        let path = dir.join(name);
        let script = format!(
            "#!/bin/sh\n[ \"$*\" = \"{args}\" ] || {{ echo \"unexpected: $*\" >&2; exit 2; }}\n\
             cat '{FIXTURES}/{fixture}'\n"
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn agent(binary: PathBuf) -> AgentSettings {
        AgentSettings {
            binary: Some(binary),
            ..AgentSettings::default()
        }
    }

    #[tokio::test]
    async fn discover_runs_each_source() {
        let dir = tempfile::tempdir().unwrap();
        let server = FakeOpenAi::start().unwrap();
        server.set_models(&["llama3.2:latest", "z-ai/glm-5.3"]);

        let mut settings = Settings::default();
        settings.agents.insert(
            "antigravity-cli".to_owned(),
            agent(fake_cli(
                dir.path(),
                "agy",
                "--output-format json models",
                "agy/1.2.13/models.json",
            )),
        );
        let mut codex = agent(fake_cli(
            dir.path(),
            "codex",
            "debug models",
            "codex/0.158.0/debug-models.json",
        ));
        codex.auth = AuthMode::ApiKey;
        settings.agents.insert("codex".to_owned(), codex);
        settings.endpoints.insert(
            "local-box".to_owned(),
            EndpointSettings {
                base_url: Some(server.base_url()),
                api_key_env: Some("HOME".to_owned()),
                local: true,
                ..EndpointSettings::default()
            },
        );
        settings.endpoints.insert(
            "gone".to_owned(),
            EndpointSettings {
                base_url: Some("http://127.0.0.1:9/v1".to_owned()),
                ..EndpointSettings::default()
            },
        );

        let found = discover_with_reasons(&settings).await;
        let count = |worker: &str| {
            found
                .models
                .iter()
                .filter(|e| e.id.worker == worker)
                .count()
        };
        assert_eq!(count("claude-code"), 4);
        assert_eq!(count("antigravity-cli"), 14);
        assert_eq!(count("codex"), 7);
        assert_eq!(count("local-box"), 2);
        assert!(found.models.iter().all(|e| e.discovered));
        let codex = found
            .models
            .iter()
            .find(|e| e.id.worker == "codex")
            .unwrap();
        assert_eq!(codex.access, ModelAccess::ApiKey);
        let glm = found
            .models
            .iter()
            .find(|e| e.id.to_string() == "local-box/z-ai/glm-5.3")
            .unwrap();
        assert_eq!(
            (glm.family.as_str(), glm.access),
            ("glm", ModelAccess::Local)
        );

        assert_eq!(found.skipped.len(), 1, "{:?}", found.skipped);
        assert!(found.skipped[0].starts_with("endpoint gone: GET http://127.0.0.1:9/v1/models"));
        assert_eq!(found.failed, ["gone"]);
        let home = std::env::var("HOME").unwrap();
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].header("authorization"),
            Some(format!("Bearer {home}").as_str())
        );
        assert!(!found.skipped.iter().any(|reason| reason.contains(&home)));

        settings.agents.insert(
            "codex".to_owned(),
            agent(fake_cli(
                dir.path(),
                "codex-wrong",
                "models",
                "codex/0.158.0/meta.json",
            )),
        );
        settings.endpoints.clear();
        let found = discover_with_reasons(&settings).await;
        assert_eq!(found.skipped.len(), 1, "{:?}", found.skipped);
        assert!(
            found.skipped[0].contains("unexpected: debug models"),
            "{:?}",
            found.skipped
        );
        assert_eq!(found.failed, ["codex"]);

        settings.registry.discover = false;
        assert_eq!(discover(&settings).await, Vec::new());
    }

    /// A models endpoint that answers one request with `body` after `delay`, and gives up
    /// quietly once the reader goes away.
    fn slow_endpoint(delay: Duration, body: Vec<u8>) -> EndpointSettings {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.read(&mut [0; 4096]);
            std::thread::sleep(delay);
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        });
        EndpointSettings {
            base_url: Some(base_url),
            ..EndpointSettings::default()
        }
    }

    /// Settings that list endpoints only, so no real CLI runs.
    fn endpoints_only() -> Settings {
        let mut settings = Settings::default();
        for adapter in ["claude-code", "antigravity-cli", "codex"] {
            let off = AgentSettings {
                enabled: false,
                ..AgentSettings::default()
            };
            settings.agents.insert(adapter.to_owned(), off);
        }
        settings
    }

    #[tokio::test]
    async fn endpoints_are_listed_at_once_in_settings_order() {
        let mut settings = endpoints_only();
        let second = Duration::from_secs(1);
        for id in ["box-a", "box-b", "box-c"] {
            let body = format!(r#"{{"data":[{{"id":"{id}-model"}}]}}"#).into_bytes();
            settings
                .endpoints
                .insert(id.to_owned(), slow_endpoint(second, body));
        }
        let started = Instant::now();
        let found = discover_with_reasons(&settings).await;
        assert!(
            started.elapsed() < Duration::from_millis(2500),
            "three one-second listings took {:?}",
            started.elapsed()
        );
        assert_eq!(found.skipped, Vec::<String>::new());
        let ids: Vec<String> = found.models.iter().map(|e| e.id.to_string()).collect();
        assert_eq!(
            ids,
            [
                "box-a/box-a-model",
                "box-b/box-b-model",
                "box-c/box-c-model"
            ]
        );
    }

    #[tokio::test]
    async fn an_endpoint_with_an_endless_model_list_is_skipped() {
        let mut settings = endpoints_only();
        let body = vec![b' '; MAX_MODELS_BYTES + 1];
        settings
            .endpoints
            .insert("endless".to_owned(), slow_endpoint(Duration::ZERO, body));
        let found = discover_with_reasons(&settings).await;
        assert!(found.models.is_empty());
        assert_eq!(found.skipped.len(), 1, "{:?}", found.skipped);
        assert!(
            found.skipped[0].ends_with("sent more than 16 MiB"),
            "{:?}",
            found.skipped
        );
    }
}
