//! What the deeper map's choices are made from on this machine, without calling a model: the
//! local servers and the models they serve, with each Ollama model's size and whether it fits
//! in memory now, each installed CLI's cheapest model, and the embedding model Ollama has.

use std::path::Path;
use std::sync::Arc;

use argonauts_core::Adapter;
use argonauts_core::cheapest::cheapest_listed;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::ModelRef;
use argonauts_core::registry::ModelRegistry;
use argonauts_core::registry::provider::provider_name;
use argonauts_core::settings::Settings;

use super::choose::{HostedModel, LocalModel, embeds, fit};
use super::embed::{KeyedEndpoint, NOMIC};
use crate::adapters::builtin_adapters;
use crate::host::memory::{ask_ollama, machine};
use crate::setup::probe::local_servers;

/// The CLIs a summarizer may run on, after the main one, in the order they're offered.
const HOSTED_ORDER: &[&str] = &["claude-code", "codex", "antigravity-cli"];

/// What this machine offers.
#[derive(Debug, Clone, Default)]
pub(crate) struct Gathered {
    pub locals: Vec<LocalModel>,
    pub hosted: Vec<HostedModel>,
    /// The Ollama that answered, by its base URL ending in `/v1`, and whether it has the
    /// embedding model.
    pub ollama: Option<(String, bool)>,
    /// An OpenAI endpoint with your key, which embeds when no Ollama answered.
    pub keyed: Option<KeyedEndpoint>,
    pub names: DisplayNames,
}

pub(crate) async fn gather(settings: &Settings, registry: &Arc<dyn ModelRegistry>) -> Gathered {
    let names = DisplayNames {
        catalog: registry.catalog(),
        clis: builtin_adapters()
            .iter()
            .map(|adapter| (adapter.id().to_owned(), adapter.display_name().to_owned()))
            .collect(),
        ..DisplayNames::default()
    };
    let servers = local_servers(settings).await;
    let mut locals = Vec::new();
    let mut ollama = None;
    let mut memory = None;
    for server in &servers {
        let (tags, loaded) = if server.id == "ollama" {
            match ask_ollama(&server.base_url).await {
                Some(read) => {
                    let has_nomic = read.tags.keys().any(|tag| tag.starts_with(NOMIC));
                    ollama = Some((v1(&server.base_url), has_nomic));
                    if memory.is_none() {
                        memory = machine(read.in_use()).await;
                    }
                    (Some(read.tags), read.loaded)
                }
                None => (None, Default::default()),
            }
        } else {
            (None, Default::default())
        };
        for model in server.models.iter().filter(|model| !embeds(model)) {
            let bytes = tags
                .as_ref()
                .and_then(|tags| tags.get(model))
                .map(|tag| tag.size);
            locals.push(LocalModel {
                runtime: server.id.clone(),
                base_url: v1(&server.base_url),
                model: model.clone(),
                bytes,
                fit: fit(memory.as_ref(), bytes, loaded.contains_key(model)),
            });
        }
    }
    Gathered {
        locals,
        hosted: hosted(settings, registry),
        ollama,
        keyed: KeyedEndpoint::find(settings),
        names,
    }
}

/// Each installed CLI's cheapest model, the main CLI's first.
fn hosted(settings: &Settings, registry: &Arc<dyn ModelRegistry>) -> Vec<HostedModel> {
    let adapters = builtin_adapters();
    let main = settings.crew.main.agent.clone();
    let mut order: Vec<String> = vec![main];
    order.extend(HOSTED_ORDER.iter().map(|cli| (*cli).to_owned()));
    order.dedup();
    let mut seen = Vec::new();
    let mut hosted = Vec::new();
    for cli in order {
        if seen.contains(&cli) {
            continue;
        }
        seen.push(cli.clone());
        let Some(adapter) = adapters.iter().find(|adapter| adapter.id() == cli) else {
            continue;
        };
        if !crate::host::services::installed(settings, adapter) {
            continue;
        }
        hosted.push(HostedModel {
            worker: cheapest(registry.as_ref(), adapter),
        });
    }
    hosted
}

/// The cheapest model the CLI lists, else the one its adapter names.
fn cheapest(registry: &dyn ModelRegistry, adapter: &Arc<dyn Adapter>) -> ModelRef {
    let model = cheapest_listed(registry, adapter.id())
        .map(|cheapest| cheapest.model)
        .unwrap_or_else(|| adapter.cheapest_model().0.to_owned());
    ModelRef::new(adapter.id(), Some(model))
}

/// The lowest level the CLI's cheapest model takes, which its sessions run at.
pub(crate) fn cheapest_effort(
    registry: &dyn ModelRegistry,
    adapter: &dyn Adapter,
) -> Option<String> {
    cheapest_listed(registry, adapter.id())
        .and_then(|cheapest| cheapest.effort)
        .or_else(|| adapter.cheapest_model().1.map(str::to_owned))
}

/// Where a CLI's actions would go, and what its plan is called.
pub(crate) fn plan_and_provider(
    registry: &dyn ModelRegistry,
    worker: &ModelRef,
) -> (String, String) {
    let plan = match worker.agent.as_str() {
        "claude-code" => "Claude",
        "codex" => "ChatGPT",
        "antigravity-cli" => "Google AI",
        other => other,
    };
    let provider = registry.provider(worker);
    let provider = if argonauts_core::registry::provider::is_known(&provider) {
        provider_name(&provider)
    } else {
        plan.to_owned()
    };
    (plan.to_owned(), provider)
}

/// A server's OpenAI-style API, which every local runtime serves under `/v1`.
fn v1(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/v1") {
        base.to_owned()
    } else {
        format!("{base}/v1")
    }
}

/// Where a recorded action's folder state lives, for the scope `This project only`.
pub(crate) fn project_of(state_dir: &Path) -> Option<String> {
    let name = state_dir.file_name()?.to_string_lossy().into_owned();
    Some(super::records::project_name(&name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_servers_api_is_its_v1() {
        assert_eq!(v1("http://127.0.0.1:11434"), "http://127.0.0.1:11434/v1");
        assert_eq!(v1("http://127.0.0.1:1234/v1/"), "http://127.0.0.1:1234/v1");
    }

    #[test]
    fn a_project_is_named_by_its_state_folder() {
        assert_eq!(
            project_of(Path::new("/h/.argonauts/state/council-3f9a1c2b4d5e")).as_deref(),
            Some("council")
        );
    }
}
