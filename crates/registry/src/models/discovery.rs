//! Discovery never makes a model call: `agy --output-format json models`, `codex debug
//! models`, Claude Code's aliases, `opencode api model.list` on a private OpenCode server, and
//! each model server's own list: every endpoint you set up, and the servers it knows on this
//! computer whenever they answer, with or without a table. Everything it finds has
//! `discovered = true`, and your own entries win over it.
//!
//! Each server says the window it holds a model to where it can (see [`served`]), and an
//! OpenCode model on such a server is held to the server's window as well as OpenCode's own.

mod claude;
mod http;
mod ollama;
mod opencode;
mod parse;
mod served;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use argonauts_adapter_opencode::private_server::ColdStart;
use argonauts_core::confine::Confine;
use argonauts_core::registry::ModelEntry;
use argonauts_core::runtimes::runtime;
use argonauts_core::settings::Settings;
use argonauts_core::settings::endpoints::known_endpoint_ids;
use tokio::task::JoinSet;

use super::builtin::{CLAUDE_CODE_ALIASES, cli_access, cli_entry, endpoint_settings};
use http::{Server, Unanswered};

pub use claude::{
    ANTHROPIC_API, ANTHROPIC_KEY_ENV, ClaudeSource, FRESH_FOR as CLAUDE_LIST_FRESH_FOR,
    ListedModel, MODELS_PAGE, STATUS_PAGE, claude_code_entries, parse_anthropic_models,
    parse_model_pages,
};
pub use ollama::{Loaded, Shown, parse_ps, parse_show, parse_tags};
pub use parse::{parse_agy_models, parse_codex_models, parse_openai_models};
pub use served::{parse_llama_props, parse_llama_slots, parse_lmstudio_models};

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

/// The CLIs discovery asks, in the order a list of them reads.
const CLIS: [&str; 4] = ["claude-code", "codex", "antigravity-cli", "opencode"];

/// Listing models is quick. A CLI that hangs, say waiting on a login, is given up on.
const CLI_TIMEOUT: Duration = Duration::from_secs(30);
const ENDPOINT_TIMEOUT: Duration = Duration::from_secs(20);

/// A hosted catalog's model list runs to a few MiB at most. A longer one is given up on.
const MAX_MODELS_BYTES: usize = 16 * 1024 * 1024;

/// What discovery found, and each source it skipped with the reason.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Discovery {
    pub models: Vec<ModelEntry>,
    /// Each source that listed nothing, in words, such as "LM Studio: not running".
    pub skipped: Vec<String>,
    /// The CLI or endpoint ids whose source failed, so what they listed before still counts.
    pub failed: Vec<String>,
}

/// Where discovery asks from, beside the settings.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    /// The repo it runs for, where OpenCode is asked, since its config there may add models.
    pub repo: Option<PathBuf>,
    /// How long a fresh OpenCode server is given to list its models.
    pub opencode_wait: ColdStart,
    /// The sandbox OpenCode is asked in, since OpenCode started in a repo loads the plugins
    /// that repo sets up. Without one, OpenCode isn't asked.
    pub confine: Option<Arc<dyn Confine>>,
    /// Whether OpenCode is asked in `repo`, which you trusted: there its config may add models
    /// and its plugins load. Otherwise it is asked from a folder of no project, so opening a
    /// model list never runs code a repo brought.
    pub opencode_in_repo: bool,
}

/// What OpenCode's list leaves out in a repo you haven't trusted: the models its own config
/// adds, when it has one. `None` once you trust the repo, or when it brings no OpenCode config.
pub fn opencode_note(sources: &Sources) -> Option<String> {
    let repo = sources
        .repo
        .as_deref()
        .filter(|_| !sources.opencode_in_repo)?;
    let brings = ["opencode.json", "opencode.jsonc", ".opencode"]
        .iter()
        .any(|name| repo.join(name).exists());
    brings.then(|| {
        "This repo's own OpenCode config is left out until you trust the repo with \
         `argonauts setup`."
            .to_owned()
    })
}

/// Runs each source `registry.discover` allows. A source that fails is skipped.
pub async fn discover(settings: &Settings) -> Vec<ModelEntry> {
    discover_with_reasons(settings, &Sources::default())
        .await
        .models
}

/// Like [`discover`], keeping why each skipped source failed so it can be shown.
pub async fn discover_with_reasons(settings: &Settings, sources: &Sources) -> Discovery {
    let mut found = Discovery::default();
    if !settings.registry.discover {
        return found;
    }
    if agent_enabled(settings, "claude-code") {
        found.models.extend(CLAUDE_CODE_ALIASES.iter().map(|alias| {
            let mut entry = cli_entry(settings, "claude-code", Some(alias));
            entry.discovered = true;
            entry
        }));
    }
    let (agy, codex, opencode, endpoints) = tokio::join!(
        cli_models(settings, &AGY),
        cli_models(settings, &CODEX),
        opencode::models(settings, sources),
        endpoint_models(settings),
    );
    let clis = [
        (AGY.adapter.to_owned(), agy),
        (CODEX.adapter.to_owned(), codex),
        (opencode::ADAPTER.to_owned(), opencode),
    ];
    for (id, result) in clis {
        match result {
            Ok(models) => found.models.extend(models),
            Err(reason) => {
                found.skipped.push(listing_failed(&id, &reason));
                found.failed.push(id);
            }
        }
    }
    let mut not_running = Vec::new();
    for listed in endpoints {
        match listed.result {
            Ok(models) => found.models.extend(models),
            Err(_) if listed.unlisted && listed.not_running => not_running.push(listed.id),
            Err(reason) => {
                found.skipped.push(reason);
                found.failed.push(listed.id);
            }
        }
    }
    hold_to_their_servers(&mut found.models);
    let off: Vec<String> = CLIS
        .into_iter()
        .filter(|cli| !agent_enabled(settings, cli))
        .map(name_of)
        .collect();
    if !off.is_empty() {
        let place = if sources.repo.is_some() {
            "off in this repo"
        } else {
            "off in your settings"
        };
        found
            .skipped
            .insert(0, format!("{}: {place}", off.join(", ")));
    }
    if !not_running.is_empty() {
        found.skipped.push(not_running_line(&not_running));
    }
    found
}

/// A CLI's model listing that failed, in the words the other sources take, such as
/// `OpenCode: not installed` or `Codex: couldn't list its models (exit 2)`: never the command,
/// nor what it printed. A reason that is already a sentence of its own stays as it is.
pub(crate) fn listing_failed(id: &str, why: &str) -> String {
    let name = name_of(id);
    if !why.contains('`') {
        return why.to_owned();
    }
    // `run` writes a failed exit as `(exit status: N)` and a program it couldn't start in the
    // system's own words.
    let code = why
        .split_once("(exit status: ")
        .and_then(|(_, rest)| rest.split_once(')'))
        .and_then(|(code, _)| code.parse::<i32>().ok());
    if code == Some(127) || why.contains("No such file or directory") {
        format!("{name}: not installed")
    } else if why.contains("took longer than") {
        format!("{name}: took too long to list its models")
    } else if let Some((_, sandbox)) = why.split_once("didn't run: ") {
        format!("{name}: couldn't list its models: {sandbox}")
    } else if let Some(code) = code {
        format!("{name}: couldn't list its models (exit {code})")
    } else {
        format!("{name}: couldn't list its models")
    }
}

/// "LM Studio, vLLM: not running".
fn not_running_line(ids: &[String]) -> String {
    let names: Vec<String> = ids.iter().map(|id| name_of(id)).collect();
    format!("{}: not running", names.join(", "))
}

/// The endpoint here that serves an OpenCode provider on this computer, such as `ollama`.
pub fn server_for_opencode(provider: &str) -> Option<&'static str> {
    opencode::local_endpoint(provider)
}

/// What a CLI or a server is called where a list names it.
pub fn name_of(id: &str) -> String {
    argonauts_core::display_names::builtin_cli_name(id).map_or_else(|| id.to_owned(), str::to_owned)
}

/// An OpenCode model on a server here is held to the window that server holds the model to,
/// such as the cap an Ollama tag sets, which OpenCode may not know, and takes the memory it
/// holds there.
pub(crate) fn hold_to_their_servers(models: &mut [ModelEntry]) {
    let served: Vec<(String, Option<u64>, Option<u64>)> = models
        .iter()
        .filter(|entry| entry.kind == argonauts_core::registry::RegistryKind::Endpoint)
        .map(|entry| (entry.id.to_string(), entry.window, entry.ram_bytes))
        .collect();
    for entry in models
        .iter_mut()
        .filter(|e| e.id.agent == opencode::ADAPTER)
    {
        let Some((provider, model)) = entry.id.model.as_deref().and_then(|m| m.split_once('/'))
        else {
            continue;
        };
        let Some(endpoint) = opencode::local_endpoint(provider) else {
            continue;
        };
        let id = format!("{endpoint}/{model}");
        let latest = format!("{id}:latest");
        let Some((_, window, ram_bytes)) = served
            .iter()
            .find(|(served, ..)| *served == id || *served == latest)
        else {
            continue;
        };
        if let Some(window) = window {
            entry.window = Some(entry.window.map_or(*window, |held| held.min(*window)));
        }
        entry.ram_bytes = entry.ram_bytes.or(*ram_bytes);
    }
}

pub(crate) fn agent_enabled(settings: &Settings, adapter: &str) -> bool {
    settings
        .agents
        .get(adapter)
        .is_none_or(|agent| agent.enabled)
}

async fn cli_models(settings: &Settings, source: &CliSource) -> Result<Vec<ModelEntry>, String> {
    if !agent_enabled(settings, source.adapter) {
        return Ok(Vec::new());
    }
    let program = settings
        .agents
        .get(source.adapter)
        .and_then(|agent| agent.binary.clone())
        .unwrap_or_else(|| PathBuf::from(source.binary));
    let stdout = run(&program, source.args).await?;
    let (access, cost) = cli_access(settings, source.adapter);
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

/// One endpoint's answer.
struct Listed {
    id: String,
    /// A server here you have no table for, asked because it's one Argonauts Code knows.
    unlisted: bool,
    not_running: bool,
    result: Result<Vec<ModelEntry>, String>,
}

/// The endpoints discovery asks: each of yours, then each known server on this computer you
/// have no table for, unless `registry.local_servers` is off.
fn endpoint_ids(settings: &Settings) -> Vec<(String, bool)> {
    let yours = settings.endpoints.keys().map(|id| (id.clone(), false));
    let unlisted = known_endpoint_ids()
        .filter(|_| settings.registry.local_servers)
        .filter(|id| !settings.endpoints.contains_key(*id))
        .filter(|id| endpoint_settings(settings, id).is_some_and(|endpoint| endpoint.local))
        .map(|id| (id.to_owned(), true));
    yours.chain(unlisted).collect()
}

/// Asks every endpoint at once, so slow ones cost one timeout between them, not one each. The
/// answers come in the order [`endpoint_ids`] gives, whichever answered first.
async fn endpoint_models(settings: &Settings) -> Vec<Listed> {
    endpoint_models_of(settings, endpoint_ids(settings)).await
}

async fn endpoint_models_of(settings: &Settings, ids: Vec<(String, bool)>) -> Vec<Listed> {
    let client = match reqwest::Client::builder().timeout(ENDPOINT_TIMEOUT).build() {
        Ok(client) => client,
        Err(e) => {
            let why = format!("couldn't start an HTTP client: {e}");
            return ids
                .into_iter()
                .map(|(id, unlisted)| Listed {
                    id,
                    unlisted,
                    not_running: false,
                    result: Err(why.clone()),
                })
                .collect();
        }
    };
    let mut results = Vec::new();
    let mut listings = JoinSet::new();
    for (index, (id, unlisted)) in ids.iter().enumerate() {
        let listed = |not_running, result| Listed {
            id: id.clone(),
            unlisted: *unlisted,
            not_running,
            result,
        };
        match Server::new(&client, settings, id) {
            Ok(server) => {
                let listed = listed(false, Ok(Vec::new()));
                let unlisted = *unlisted;
                listings.spawn(async move {
                    let result = endpoint_listing(&server, unlisted).await;
                    let not_running = result.as_ref().is_err_and(|e| e.not_running);
                    let result = result.map_err(|e| e.why);
                    (
                        index,
                        Listed {
                            not_running,
                            result,
                            ..listed
                        },
                    )
                });
            }
            Err(reason) => results.push((index, listed(false, Err(reason)))),
        }
    }
    while let Some(done) = listings.join_next().await {
        match done {
            Ok(result) => results.push(result),
            Err(e) => results.push((
                usize::MAX,
                Listed {
                    id: String::new(),
                    unlisted: false,
                    not_running: false,
                    result: Err(format!("an endpoint listing failed: {e}")),
                },
            )),
        }
    }
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, listed)| listed).collect()
}

/// What one endpoint serves. A known server you have no table for, on ports other apps use too,
/// counts only once it says in its own API which server it is; until then it isn't running.
async fn endpoint_listing(server: &Server, unlisted: bool) -> Result<Vec<ModelEntry>, Unanswered> {
    let shared_ports = unlisted && runtime(&server.id).is_some_and(|known| known.common_ports);
    if shared_ports && !served::says_what_it_is(server).await {
        return Err(Unanswered {
            why: format!(
                "nothing on {} answers as {}",
                server.root(),
                name_of(&server.id)
            ),
            not_running: true,
        });
    }
    served::list(server).await
}

/// The window the endpoint `endpoint` loaded `model` with, from a server that sizes a model as
/// it loads it (Ollama and LM Studio). `None` from any other, while it isn't loaded, or when
/// the server doesn't answer. No model is loaded to find out.
pub async fn loaded_window(settings: &Settings, endpoint: &str, model: &str) -> Option<u64> {
    let client = reqwest::Client::builder()
        .timeout(ENDPOINT_TIMEOUT)
        .build()
        .ok()?;
    let server = Server::new(&client, settings, endpoint).ok()?;
    served::loaded_window(&server, model).await
}

/// The sources discovery runs itself, by CLI or endpoint id: agy, Codex and OpenCode while
/// they're on, then each endpoint it asks, yours and the known servers here.
pub fn source_ids(settings: &Settings) -> Vec<String> {
    if !settings.registry.discover {
        return Vec::new();
    }
    [AGY.adapter, CODEX.adapter, opencode::ADAPTER]
        .into_iter()
        .filter(|adapter| agent_enabled(settings, adapter))
        .map(str::to_owned)
        .chain(endpoint_ids(settings).into_iter().map(|(id, _)| id))
        .collect()
}

/// Asks one of [`source_ids`] again, as [`discover_with_reasons`] asks them all. `None` for an
/// id that isn't one.
pub async fn discover_source(
    settings: &Settings,
    sources: &Sources,
    id: &str,
) -> Option<Result<Vec<ModelEntry>, String>> {
    if !source_ids(settings).iter().any(|known| known == id) {
        return None;
    }
    if let Some(source) = [&AGY, &CODEX]
        .into_iter()
        .find(|source| source.adapter == id)
    {
        return Some(cli_models(settings, source).await);
    }
    if id == opencode::ADAPTER {
        return Some(opencode::models(settings, sources).await);
    }
    let client = match reqwest::Client::builder().timeout(ENDPOINT_TIMEOUT).build() {
        Ok(client) => client,
        Err(e) => return Some(Err(format!("couldn't start an HTTP client: {e}"))),
    };
    let unlisted = endpoint_ids(settings)
        .into_iter()
        .any(|(known, unlisted)| known == id && unlisted);
    Some(match Server::new(&client, settings, id) {
        Ok(server) => endpoint_listing(&server, unlisted).await.map_err(|e| e.why),
        Err(reason) => Err(reason),
    })
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

    /// The command and what it printed stay out of what discovery says it skipped.
    #[test]
    fn a_failed_listing_reads_like_the_other_sources() {
        let failed = |why: &str| listing_failed("opencode", why);
        assert_eq!(
            failed(
                "`/opt/opencode api --server http://127.0.0.1:9 model.list` failed (exit status: \
                 127): GUARD: blocked"
            ),
            "OpenCode: not installed"
        );
        assert_eq!(
            failed("couldn't run `opencode serve --stdio`: No such file or directory (os error 2)"),
            "OpenCode: not installed"
        );
        assert_eq!(
            failed("`opencode api model.list` failed (exit status: 2): unknown command api"),
            "OpenCode: couldn't list its models (exit 2)"
        );
        assert_eq!(
            failed("`opencode api model.list` took longer than 15s"),
            "OpenCode: took too long to list its models"
        );
        assert_eq!(
            failed("`opencode serve` didn't run: the sandbox doesn't work here: no sandbox-exec"),
            "OpenCode: couldn't list its models: the sandbox doesn't work here: no sandbox-exec"
        );
        assert_eq!(
            failed("OpenCode listed no models in 15s (16 asks)"),
            "OpenCode listed no models in 15s (16 asks)"
        );
        assert_eq!(
            listing_failed(
                "codex",
                "`codex debug models` failed (exit status: 1): boom"
            ),
            "Codex: couldn't list its models (exit 1)"
        );
    }

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

    /// A fake CLI that prints `fixture` only when called with exactly `args`.
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
        settings.registry.local_servers = false;
        settings.agents.get_mut("opencode").unwrap().enabled = false;
        settings.agents.insert(
            "antigravity-cli".to_owned(),
            agent(fake_cli(
                dir.path(),
                "agy",
                "--output-format json models",
                "agy/models.json",
            )),
        );
        let mut codex = agent(fake_cli(
            dir.path(),
            "codex",
            "debug models",
            "codex/debug-models.json",
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

        let found = discover_with_reasons(&settings, &Sources::default()).await;
        let count = |worker: &str| found.models.iter().filter(|e| e.id.agent == worker).count();
        assert_eq!(count("claude-code"), 4);
        assert_eq!(count("antigravity-cli"), 14);
        assert_eq!(count("codex"), 7);
        assert_eq!(count("local-box"), 2);
        assert!(found.models.iter().all(|e| e.discovered));
        let codex = found.models.iter().find(|e| e.id.agent == "codex").unwrap();
        assert_eq!(codex.access, ModelAccess::ApiKey);
        let glm = found
            .models
            .iter()
            .find(|e| e.id.to_string() == "local-box/z-ai/glm-5.3")
            .unwrap();
        assert_eq!(
            (glm.provider.as_str(), glm.access),
            ("zhipu", ModelAccess::Local)
        );

        assert_eq!(found.skipped.len(), 2, "{:?}", found.skipped);
        assert_eq!(found.skipped[0], "OpenCode: off in your settings");
        assert!(found.skipped[1].starts_with("endpoint gone: GET http://127.0.0.1:9/v1/models"));
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
                "codex/meta.json",
            )),
        );
        settings.endpoints.clear();
        let found = discover_with_reasons(&settings, &Sources::default()).await;
        assert_eq!(found.skipped.len(), 2, "{:?}", found.skipped);
        // Said as the other sources say theirs, without the command or what it printed.
        assert!(
            found.skipped[1].starts_with("Codex: couldn't list its models (exit "),
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

    /// Settings that list your endpoints only, so no real CLI or server here is asked.
    fn endpoints_only() -> Settings {
        let mut settings = Settings::default();
        settings.registry.local_servers = false;
        for adapter in CLIS {
            let off = AgentSettings {
                enabled: false,
                ..AgentSettings::default()
            };
            settings.agents.insert(adapter.to_owned(), off);
        }
        settings
    }

    /// What discovery skipped beyond the CLIs `endpoints_only` turns off.
    fn failures(found: &Discovery) -> &[String] {
        assert_eq!(
            found.skipped.first().map(String::as_str),
            Some("Claude Code, Codex, Antigravity CLI, OpenCode: off in your settings")
        );
        &found.skipped[1..]
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
        let found = discover_with_reasons(&settings, &Sources::default()).await;
        assert!(
            started.elapsed() < Duration::from_millis(2500),
            "three one-second listings took {:?}",
            started.elapsed()
        );
        assert_eq!(failures(&found), Vec::<String>::new());
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
        let found = discover_with_reasons(&settings, &Sources::default()).await;
        assert!(found.models.is_empty());
        let failed = failures(&found);
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert!(failed[0].ends_with("sent more than 16 MiB"), "{failed:?}");
    }

    /// A fake Ollama on loopback that answers `/api/tags`, `/api/show` and `/api/ps` with the
    /// answers recorded from a real one, for as many requests as come.
    fn fake_ollama(ps: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let read =
            |name: &str| std::fs::read_to_string(format!("{FIXTURES}/ollama/{name}")).unwrap();
        let (tags, show, ps) = (read("tags.json"), read("show.json"), read(ps));
        let show: serde_json::Value = serde_json::from_str(&show).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let (line, body) = read_request(&mut stream);
                let answer = if line.starts_with("GET /api/tags ") {
                    tags.clone()
                } else if line.starts_with("GET /api/ps ") {
                    ps.clone()
                } else if line.starts_with("POST /api/show ") {
                    let asked: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                    let tag = asked["model"].as_str().unwrap_or_default();
                    show[tag].to_string()
                } else {
                    String::new()
                };
                let status = if answer.is_empty() || answer == "null" {
                    "404 Not Found"
                } else {
                    "200 OK"
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        base_url
    }

    /// One HTTP request's first line and body: the head, then as much body as its
    /// Content-Length says.
    fn read_request(stream: &mut std::net::TcpStream) -> (String, String) {
        let mut request = Vec::new();
        let mut buffer = [0; 8192];
        while let Ok(read @ 1..) = stream.read(&mut buffer) {
            request.extend_from_slice(&buffer[..read]);
            let text = String::from_utf8_lossy(&request);
            let Some((head, body)) = text.split_once("\r\n\r\n") else {
                continue;
            };
            let length = head.lines().find_map(|line| {
                let line = line.to_ascii_lowercase();
                line.strip_prefix("content-length:")?
                    .trim()
                    .parse::<usize>()
                    .ok()
            });
            if body.len() >= length.unwrap_or(0) {
                break;
            }
        }
        let text = String::from_utf8_lossy(&request);
        let line = text.lines().next().unwrap_or_default().to_owned();
        let body = text.split_once("\r\n\r\n").map_or("", |(_, body)| body);
        (line, body.to_owned())
    }

    /// A fake `opencode` 2.0.32 that answers `model.list` and `model.default` on a private
    /// server with the recorded answers, and fails anything else. Its server writes the folder
    /// it was started in to `asked-from` in `dir`.
    fn fake_opencode(dir: &Path) -> PathBuf {
        let path = dir.join("opencode");
        let asked_from = dir.join("asked-from");
        let asked_from = asked_from.display();
        // The calls name the repo after the first four words.
        let script = format!(
            "#!/bin/sh\ncase \"$1 $2 $3 $4\" in\n\
             '--version   ') echo 2.0.32 ;;\n\
             'serve --stdio --port 0') pwd -P > '{asked_from}'; \
             echo '{{\"url\":\"http://127.0.0.1:9\"}}'; cat > /dev/null ;;\n\
             'api --server http://127.0.0.1:9 model.list') cat '{FIXTURES}/opencode/model-list.json' ;;\n\
             'api --server http://127.0.0.1:9 model.default') \
             cat '{FIXTURES}/opencode/model-default.json' ;;\n\
             *) echo \"unexpected: $*\" >&2; exit 2 ;;\nesac\n"
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn ollama_at(base_url: String) -> EndpointSettings {
        EndpointSettings {
            base_url: Some(base_url),
            local: true,
            ..EndpointSettings::default()
        }
    }

    /// OpenCode started in a repo loads the plugins its config sets up, so it is asked from a
    /// folder of no project until you trust the repo, and the list says what it leaves out
    /// when the repo brings OpenCode config of its own.
    #[tokio::test]
    async fn opencode_is_asked_in_a_repo_only_once_you_trust_it() {
        let dir = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let mut settings = endpoints_only();
        settings
            .agents
            .insert("opencode".to_owned(), agent(fake_opencode(dir.path())));
        let asked_from = || {
            let folder = std::fs::read_to_string(dir.path().join("asked-from")).unwrap();
            PathBuf::from(folder.trim())
        };
        let mut sources = Sources {
            repo: Some(repo.path().to_owned()),
            confine: Some(Arc::new(argonauts_core::confine::Unconfined)),
            ..Sources::default()
        };
        let repo_folder = repo.path().canonicalize().unwrap();
        discover_with_reasons(&settings, &sources).await;
        assert_ne!(asked_from(), repo_folder);
        assert_eq!(opencode_note(&sources), None, "the repo brings no config");
        std::fs::write(repo.path().join("opencode.json"), "{}").unwrap();
        assert_eq!(
            opencode_note(&sources).as_deref(),
            Some(
                "This repo's own OpenCode config is left out until you trust the repo with \
                 `argonauts setup`."
            )
        );

        sources.opencode_in_repo = true;
        discover_with_reasons(&settings, &sources).await;
        assert_eq!(asked_from(), repo_folder);
        assert_eq!(opencode_note(&sources), None);
    }

    /// Ollama's windows come from its own API, and an OpenCode model on it is held to the cap
    /// its tag sets, which OpenCode doesn't know.
    #[tokio::test]
    async fn a_local_model_is_held_to_its_servers_window() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = endpoints_only();
        settings
            .endpoints
            .insert("ollama".to_owned(), ollama_at(fake_ollama("ps-empty.json")));
        settings
            .agents
            .insert("opencode".to_owned(), agent(fake_opencode(dir.path())));
        let sources = Sources {
            repo: Some(dir.path().to_owned()),
            confine: Some(Arc::new(argonauts_core::confine::Unconfined)),
            ..Sources::default()
        };
        let found = discover_with_reasons(&settings, &sources).await;
        assert_eq!(
            found.skipped,
            ["Claude Code, Codex, Antigravity CLI: off in this repo"]
        );
        let entry = |id: &str| {
            found
                .models
                .iter()
                .find(|e| e.id.to_string() == id)
                .unwrap_or_else(|| panic!("{id} wasn't found"))
        };
        let windows = |id: &str| {
            let e = entry(id);
            (e.context_window, e.window, e.session_window())
        };
        assert_eq!(
            found
                .models
                .iter()
                .filter(|e| e.id.agent == "ollama")
                .count(),
            7
        );
        assert_eq!(
            found
                .models
                .iter()
                .filter(|e| e.id.agent == "opencode")
                .count(),
            17
        );

        assert_eq!(
            windows("ollama/qwen3-coder:30b-64k"),
            (Some(262_144), Some(65_536), Some(65_536))
        );
        assert_eq!(
            windows("ollama/qwen3-coder:30b"),
            (Some(262_144), None, None)
        );
        assert_eq!(
            windows("ollama/gemma3:12b-32k"),
            (Some(131_072), Some(32_768), Some(32_768))
        );
        assert_eq!(windows("ollama/gemma2:9b"), (Some(8_192), None, None));
        assert!(entry("ollama/qwen3-coder:30b").tools);
        assert!(!entry("ollama/gemma3:12b").tools);

        // OpenCode says 131,072 for the 32k tag; the tag holds it to 32,768.
        assert_eq!(
            windows("opencode/ollama/gemma3:12b-32k"),
            (Some(131_072), Some(32_768), Some(32_768))
        );
        assert_eq!(
            windows("opencode/ollama/qwen3-coder:30b-64k"),
            (Some(65_536), Some(65_536), Some(65_536))
        );
        // No cap anywhere: the most it takes shows, and a session waits for the loaded size.
        assert_eq!(
            windows("opencode/ollama/qwen3-coder:30b"),
            (Some(262_144), None, None)
        );
        assert_eq!(
            windows("opencode/opencode/deepseek-v4.1-flash-free"),
            (Some(200_000), None, Some(200_000))
        );
        assert!(entry("opencode/opencode/nemotron-3.5-super-vl-free").cli_default);
        assert_eq!(found.models.iter().filter(|e| e.cli_default).count(), 1);
    }

    /// A loaded model's window is the one it was loaded with, and so is its memory.
    #[tokio::test]
    async fn a_loaded_model_is_held_to_its_loaded_window() {
        let mut settings = endpoints_only();
        let base_url = fake_ollama("ps-loaded.json");
        settings
            .endpoints
            .insert("ollama".to_owned(), ollama_at(base_url));
        let found = discover_with_reasons(&settings, &Sources::default()).await;
        let llama = found
            .models
            .iter()
            .find(|e| e.id.to_string() == "ollama/llama3.2:latest")
            .unwrap();
        assert_eq!(llama.window, Some(131_072));
        assert_eq!(llama.ram_bytes, Some(23_779_979_264));
        assert_eq!(
            loaded_window(&settings, "ollama", "llama3.2").await,
            Some(131_072)
        );
        assert_eq!(loaded_window(&settings, "ollama", "gemma2:9b").await, None);
    }

    /// The servers Argonauts Code knows on this computer are asked with no table for them, and
    /// one that isn't listening is said to be not running rather than failed.
    #[tokio::test]
    async fn the_servers_here_are_asked_without_a_table() {
        let mut settings = endpoints_only();
        settings.registry.local_servers = true;
        let unlisted = |ids: &[&str]| -> Vec<(String, bool)> {
            ids.iter().map(|id| ((*id).to_owned(), true)).collect()
        };
        assert_eq!(
            endpoint_ids(&settings),
            unlisted(&["ollama", "lmstudio", "llamacpp", "vllm"])
        );
        settings.endpoints.insert(
            "ollama".to_owned(),
            ollama_at("http://127.0.0.1:9/v1".to_owned()),
        );
        let mut yours_first = vec![("ollama".to_owned(), false)];
        yours_first.extend(unlisted(&["lmstudio", "llamacpp", "vllm"]));
        assert_eq!(endpoint_ids(&settings), yours_first);
        settings.registry.local_servers = false;
        assert_eq!(endpoint_ids(&settings), [("ollama".to_owned(), false)]);

        // Nothing listens on port 9: a table's server failed, one asked unlisted isn't running.
        let listed = endpoint_models_of(&settings, vec![("ollama".to_owned(), true)]).await;
        assert!(listed[0].not_running, "{:?}", listed[0].result);
        let listed = endpoint_models_of(&settings, vec![("ollama".to_owned(), false)]).await;
        let found = discover_with_reasons(&settings, &Sources::default()).await;
        assert!(listed[0].not_running);
        assert_eq!(found.failed, ["ollama"]);
        assert!(
            failures(&found)[0].starts_with("endpoint ollama: GET http://127.0.0.1:9/api/tags")
        );
        assert_eq!(
            not_running_line(&["lmstudio".to_owned(), "vllm".to_owned()]),
            "LM Studio, vLLM: not running"
        );
    }

    /// vLLM's port is one other apps use too, so a server there without a table counts only
    /// when its list says vLLM owns the models; with your table it's listed as it answers.
    #[tokio::test]
    async fn a_server_on_a_shared_port_counts_once_it_says_what_it_is() {
        use argonauts_testkit::{FakeKind, FakeModelServer};

        let at = |server: &FakeModelServer| {
            let mut settings = endpoints_only();
            settings.endpoints.insert(
                "vllm".to_owned(),
                EndpointSettings {
                    base_url: Some(server.base_url()),
                    local: true,
                    ..EndpointSettings::default()
                },
            );
            settings
        };
        let vllm = FakeModelServer::start(
            FakeKind::Vllm {
                max_model_len: 32_768,
            },
            &["qwen3"],
        )
        .unwrap();
        let other = FakeModelServer::start(FakeKind::Other, &["qwen3"]).unwrap();

        let listed = endpoint_models_of(&at(&vllm), vec![("vllm".to_owned(), true)]).await;
        let models = listed[0].result.as_ref().unwrap();
        assert_eq!(models[0].id.to_string(), "vllm/qwen3");

        let listed = endpoint_models_of(&at(&other), vec![("vllm".to_owned(), true)]).await;
        assert!(listed[0].not_running, "{:?}", listed[0].result);
        let listed = endpoint_models_of(&at(&other), vec![("vllm".to_owned(), false)]).await;
        assert!(
            listed[0].result.is_ok(),
            "your table's server is taken as it is"
        );
    }

    #[test]
    fn a_source_is_named_as_lists_name_it() {
        assert_eq!(name_of("antigravity-cli"), "Antigravity CLI");
        assert_eq!(name_of("ollama"), "Ollama");
        assert_eq!(name_of("lmstudio"), "LM Studio");
        assert_eq!(name_of("llamacpp"), "llama-server");
        assert_eq!(name_of("my-box"), "my-box");
    }
}
