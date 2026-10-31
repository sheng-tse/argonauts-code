//! What "Open models" lists and does: every model the servers on this computer have, with what
//! each takes of its memory as `/model` on OpenCode reckons it, then up to three from the
//! catalog that would fit; starting a server that's installed but not running; and downloading
//! a model you chose that isn't here yet.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use argonauts_core::app::ModelChoice;
use argonauts_core::app::model_picker::{ListedFacts, LocalModel};
use argonauts_core::base::names::ModelRef;
use argonauts_core::behaviors::deep_map::{OpenModels, StoppedServer};
use argonauts_core::models::limits::CostEstimate;
use argonauts_core::models::registry::ModelAccess;
use argonauts_core::settings::Settings;
use argonauts_registry::models::suggested::matching;
use serde_json::json;

use super::choose::{embeds, local_name};
use crate::controller::settings::memory_facts::{Reading, annotate};
use crate::setup::probe::{LocalServer, local_servers};

/// OpenCode's routes, which the memory reckoning reads a model's server from.
const OPENCODE: &str = "opencode";
/// How long a server that was just started has to answer.
pub(crate) const START_WAIT: Duration = Duration::from_secs(10);
const ASK_EVERY: Duration = Duration::from_millis(250);

/// The list for `servers`, as `/model` on OpenCode draws it for this computer when `reading`
/// says what its memory holds, and `stopped` when a server is installed but didn't answer.
pub(crate) fn open_models(
    servers: &[LocalServer],
    reading: Option<&Reading>,
    catalog: &[LocalModel],
    stopped: Option<StoppedServer>,
) -> OpenModels {
    let mut choices: Vec<ModelChoice> = servers
        .iter()
        .flat_map(|server| {
            server
                .models
                .iter()
                .filter(|model| !embeds(model))
                .map(move |model| installed(server, model, catalog))
        })
        .collect();
    if let Some(reading) = reading {
        annotate(&mut choices, reading, &[], catalog);
    }
    for choice in &mut choices {
        choice.worker = summarizer(&choice.worker);
    }
    OpenModels {
        machine: reading.map(|reading| reading.machine),
        models: choices,
        stopped,
    }
}

/// An installed model as OpenCode would route it, so the memory reckoning finds its server.
fn installed(server: &LocalServer, model: &str, catalog: &[LocalModel]) -> ModelChoice {
    let window = server
        .windows
        .get(model)
        .copied()
        .or_else(|| window_in_tag(model))
        .or_else(|| matching(catalog, model).map(|known| known.window));
    let name = match window_in_tag(model) {
        Some(_) => format!(
            "{} ({})",
            local_name(model),
            argonauts_core::look::window_size::window_words(window.unwrap_or(0))
        ),
        None => local_name(model),
    };
    ModelChoice {
        worker: ModelRef::new(OPENCODE, Some(format!("{}/{model}", server.id))),
        provider: String::new(),
        access: ModelAccess::Local,
        cost: CostEstimate::Free,
        plan_usage: None,
        efforts: Vec::new(),
        strengths: Vec::new(),
        current: false,
        note: None,
        older: false,
        name,
        description: None,
        recommended: false,
        new: false,
        default_effort: None,
        effort: None,
        alias: false,
        runs: None,
        listed: Some(ListedFacts {
            group: Some(server.name().to_owned()),
            window,
            no_tools: false,
            thinking: false,
            hosted_by: None,
            memory: None,
        }),
    }
}

/// The window a local tag was made with, such as 65,536 for a tag ending in `-64k`.
fn window_in_tag(model: &str) -> Option<u64> {
    let (base, window) = model.rsplit_once('-')?;
    let thousands: u64 = window.strip_suffix(['k', 'K'])?.parse().ok()?;
    base.contains(':').then_some(thousands * 1024)
}

/// OpenCode's route `opencode/ollama/qwen3:8b` as the summarizer that runs it, `ollama/qwen3:8b`.
fn summarizer(route: &ModelRef) -> ModelRef {
    match route
        .model
        .as_deref()
        .and_then(|model| model.split_once('/'))
    {
        Some((runtime, model)) if route.agent == OPENCODE => {
            ModelRef::new(runtime, Some(model.to_owned()))
        }
        _ => route.clone(),
    }
}

/// How a server installed here starts: its own command, an argument vector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Starter {
    pub runtime: &'static str,
    pub name: &'static str,
    pub program: String,
    pub args: Vec<String>,
}

/// The servers installed here that Open models can start, when none of theirs answered: Ollama
/// by its app on a Mac, else `ollama serve`, and LM Studio by `lms server start`.
pub(crate) fn installed_servers(
    applications: &Path,
    on_path: impl Fn(&str) -> bool,
) -> Vec<Starter> {
    let starter = |runtime, name, program: &str, args: &[&str]| Starter {
        runtime,
        name,
        program: program.to_owned(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
    };
    let mut found = Vec::new();
    if applications.join("Ollama.app").is_dir() {
        found.push(starter("ollama", "Ollama", "open", &["-g", "-a", "Ollama"]));
    } else if on_path("ollama") {
        found.push(starter("ollama", "Ollama", "ollama", &["serve"]));
    }
    if on_path("lms") {
        found.push(starter(
            "lmstudio",
            "LM Studio",
            "lms",
            &["server", "start"],
        ));
    }
    found
}

/// The first server installed here whose runtime didn't answer.
pub(crate) fn stopped(servers: &[LocalServer], installed: &[Starter]) -> Option<StoppedServer> {
    installed
        .iter()
        .find(|starter| !servers.iter().any(|server| server.id == starter.runtime))
        .map(|starter| StoppedServer {
            runtime: starter.runtime.to_owned(),
            name: starter.name.to_owned(),
            tried: false,
        })
}

/// Starts `starter`, which you chose, and waits up to `wait` for its server to answer. Its
/// output goes nowhere and it outlives the app, as the app's own server would.
pub(crate) async fn start(starter: &Starter, settings: &Settings, wait: Duration) -> bool {
    let started = tokio::process::Command::new(&starter.program)
        .args(&starter.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if started.is_err() {
        return false;
    }
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let servers = local_servers(settings).await;
        if servers.iter().any(|server| server.id == starter.runtime) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(ASK_EVERY).await;
    }
}

/// Downloads `tag` into the Ollama at `base_url`, telling `progress` the megabytes done of all
/// as they come.
pub(crate) async fn pull(
    base_url: &str,
    tag: &str,
    progress: &mut (dyn FnMut(u64, u64) + Send),
) -> Result<(), String> {
    let client = argonauts_endpoints::session::runtime_api::local_client(Duration::from_secs(3600))
        .ok_or("no client for this machine's servers")?;
    let root = base_url.trim_end_matches('/').trim_end_matches("/v1");
    let url = format!("{root}/api/pull");
    let mut response = client
        .post(&url)
        .json(&json!({ "model": tag, "stream": true }))
        .send()
        .await
        .map_err(|err| format!("{url}: {err}"))?;
    if !response.status().is_success() {
        return Err(format!("{url} answered {}", response.status()));
    }
    let mut pending = String::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| format!("{url}: {err}"))?
    {
        pending.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(end) = pending.find('\n') {
            let line: String = pending.drain(..=end).collect();
            match pulled(&line) {
                Pulled::Bytes(done, total) => progress(done / MB, total / MB),
                Pulled::Failed(why) => return Err(why),
                Pulled::Other => {}
            }
        }
    }
    Ok(())
}

const MB: u64 = 1_000_000;

#[derive(Debug, PartialEq, Eq)]
enum Pulled {
    Bytes(u64, u64),
    Failed(String),
    Other,
}

/// One line of Ollama's pull, such as `{"status":"pulling 6a0746a1ec1a","total":17000,
/// "completed":4100}`.
fn pulled(line: &str) -> Pulled {
    let Ok(said) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return Pulled::Other;
    };
    if let Some(error) = said.get("error").and_then(|error| error.as_str()) {
        return Pulled::Failed(error.to_owned());
    }
    match (
        said.get("completed").and_then(|n| n.as_u64()),
        said.get("total").and_then(|n| n.as_u64()),
    ) {
        (Some(done), Some(total)) => Pulled::Bytes(done, total),
        _ => Pulled::Other,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use argonauts_core::app::model_picker::{Fit, MachineMemory};

    use super::*;
    use crate::host::memory::{Held, Ollama, Tag};

    const GB: u64 = 1 << 30;

    fn ollama(models: &[&str]) -> LocalServer {
        LocalServer {
            id: "ollama".to_owned(),
            base_url: "http://127.0.0.1:11434".to_owned(),
            models: models.iter().map(|model| (*model).to_owned()).collect(),
            ..LocalServer::default()
        }
    }

    fn reading() -> Reading {
        let tag = |gb: u64| Tag {
            size: gb * GB,
            per_token: None,
            thinking: false,
        };
        let mut tags = BTreeMap::new();
        tags.insert("sample-coder:30b-64k".to_owned(), tag(25));
        tags.insert("gemma3:12b".to_owned(), tag(17));
        tags.insert("nomic-embed-text:latest".to_owned(), tag(0));
        let mut loaded = BTreeMap::new();
        loaded.insert(
            "sample-coder:30b-64k".to_owned(),
            Held {
                bytes: 25 * GB,
                window: Some(65_536),
            },
        );
        Reading {
            machine: MachineMemory {
                total: 64 * GB,
                limit: 52 * GB,
                in_use: 25 * GB,
                free: 18 * GB,
            },
            ollama: Ollama {
                tags,
                loaded,
                measured: BTreeMap::new(),
            },
        }
    }

    #[test]
    fn every_model_a_server_has_is_listed_as_its_summarizer_with_what_it_takes() {
        let server = ollama(&[
            "gemma3:12b",
            "nomic-embed-text:latest",
            "sample-coder:30b-64k",
        ]);
        let open = open_models(&[server], Some(&reading()), &[], None);
        let listed: Vec<(String, &str)> = open
            .models
            .iter()
            .map(|choice| (choice.worker.to_string(), choice.name.as_str()))
            .collect();
        assert_eq!(
            listed,
            [
                ("ollama/gemma3:12b".to_owned(), "Gemma3 12B"),
                (
                    "ollama/sample-coder:30b-64k".to_owned(),
                    "Sample Coder 30B (64k)"
                ),
            ]
        );
        let memory = |at: usize| {
            let facts = open.models[at].listed.as_ref().unwrap();
            facts.memory.clone().unwrap()
        };
        assert_eq!((memory(0).ram, memory(0).fit), (17 * GB, Fit::Fits));
        assert_eq!(memory(1).fit, Fit::Loaded);
        assert_eq!(open.models[1].listed.as_ref().unwrap().window, Some(65_536));
        assert_eq!(open.machine.map(|machine| machine.limit), Some(52 * GB));
    }

    #[test]
    fn without_a_reading_the_models_are_listed_without_their_memory() {
        let open = open_models(&[ollama(&["qwen3:8b"])], None, &[], None);
        assert_eq!(open.models.len(), 1);
        assert!(open.models[0].listed.as_ref().unwrap().memory.is_none());
        assert!(open.machine.is_none());
    }

    #[test]
    fn an_installed_server_that_didnt_answer_is_offered_to_start_by_its_own_command() {
        let apps = tempfile::tempdir().unwrap();
        let none = |_: &str| false;
        assert!(installed_servers(apps.path(), none).is_empty());
        let only_ollama = |program: &str| program == "ollama";
        let found = installed_servers(apps.path(), only_ollama);
        assert_eq!(
            (found[0].program.as_str(), found[0].args.clone()),
            ("ollama", vec!["serve".to_owned()])
        );
        std::fs::create_dir(apps.path().join("Ollama.app")).unwrap();
        let found = installed_servers(apps.path(), only_ollama);
        assert_eq!(
            (found[0].program.as_str(), found[0].args.join(" ")),
            ("open", "-g -a Ollama".to_owned())
        );
        let answering = [ollama(&["qwen3:8b"])];
        assert_eq!(stopped(&answering, &found), None);
        let stopped = stopped(&[], &found).unwrap();
        assert_eq!(
            (stopped.runtime.as_str(), stopped.name.as_str()),
            ("ollama", "Ollama")
        );
    }

    #[tokio::test]
    async fn a_server_that_never_answers_is_given_up_on_after_the_wait() {
        let mut settings = Settings::default();
        settings.registry.local_servers = false;
        let starter = Starter {
            runtime: "ollama",
            name: "Ollama",
            program: "true".to_owned(),
            args: Vec::new(),
        };
        assert!(!start(&starter, &settings, Duration::from_millis(300)).await);
        let missing = Starter {
            program: "/nowhere/ollama".to_owned(),
            ..starter
        };
        assert!(!start(&missing, &settings, Duration::from_millis(300)).await);
    }

    #[test]
    fn a_pull_reads_its_bytes_and_its_error() {
        assert_eq!(
            pulled(r#"{"status":"pulling 6a07","total":17000000000,"completed":4100000000}"#),
            Pulled::Bytes(4_100_000_000, 17_000_000_000)
        );
        assert_eq!(
            pulled(r#"{"status":"verifying sha256 digest"}"#),
            Pulled::Other
        );
        assert_eq!(
            pulled(r#"{"error":"pull model manifest: file does not exist"}"#),
            Pulled::Failed("pull model manifest: file does not exist".to_owned())
        );
    }
}
