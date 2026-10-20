//! OpenCode's models, as `opencode api model.list` gives them: each with its name, window, tool
//! use, variants and price, and no model call. OpenCode 2 answers through a background service
//! it keeps for the whole machine, which adds every folder it's asked about to its own list, so
//! each ask runs on a private server of its own instead (`--standalone`). A fresh one lists
//! nothing for its first seconds, so it's asked again each second for up to 15 s.
//!
//! OpenCode 1.x has no `opencode api` and no service: there `opencode models` gives the ids
//! alone, one `provider/model` a line.
//!
//! The list comes in the order `/model` shows it: the models on this computer first, those
//! with tool use before those without and then by name; then each provider you connected in
//! OpenCode; then OpenCode Zen's, newest first.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use argonauts_adapter_opencode::models::{self as listing, ListedModel, ZEN};
use argonauts_adapter_opencode::private_server::{Listing, PrivateServer};
use argonauts_core::extract_version;
use argonauts_core::model_catalog::display_name_from_id;
use argonauts_core::registry::{CostUnit, ModelAccess, ModelEntry};
use argonauts_core::runtimes::runtime;
use argonauts_core::settings::Settings;

use super::parse::plain_text;
use super::{Sources, agent_enabled, run};
use crate::models::builtin::{cli_access, cli_entry, cost};

pub(super) const ADAPTER: &str = "opencode";

/// The endpoint here that serves an OpenCode provider on this computer, such as `ollama`.
/// OpenCode routes each known runtime under the id its endpoint has here.
pub(super) fn local_endpoint(provider: &str) -> Option<&'static str> {
    runtime(provider).map(|runtime| runtime.id)
}

/// OpenCode 1.x answers on its own; anything later, or a version that can't be read, goes
/// through a private server.
fn answers_alone(version: Option<&str>) -> bool {
    version
        .and_then(|version| version.split('.').next())
        .and_then(|major| major.parse::<u32>().ok())
        .is_some_and(|major| major < 2)
}

/// Where OpenCode is asked from: a repo you trusted, whose own config may add models, else a
/// folder of no project, where no repo's plugins load.
fn workdir(sources: &Sources) -> PathBuf {
    sources
        .repo
        .as_deref()
        .filter(|_| sources.opencode_in_repo)
        .map_or_else(std::env::temp_dir, Path::to_owned)
}

/// Every model OpenCode can use here, with the one it runs by default marked, asked inside
/// `sources.confine` from the folder [`workdir`] picks. Asks nothing while OpenCode is off, and refuses
/// to ask a server outside a sandbox.
pub(super) async fn models(
    settings: &Settings,
    sources: &Sources,
) -> Result<Vec<ModelEntry>, String> {
    if !agent_enabled(settings, ADAPTER) {
        return Ok(Vec::new());
    }
    let program = settings
        .agents
        .get(ADAPTER)
        .and_then(|agent| agent.binary.clone())
        .unwrap_or_else(|| PathBuf::from(ADAPTER));
    let version = run(&program, &["--version"])
        .await
        .ok()
        .and_then(|printed| extract_version(&printed));
    let (access, _) = cli_access(settings, ADAPTER);
    if answers_alone(version.as_deref()) {
        let printed = run(&program, &["models"]).await?;
        let listed = listing::parse_model_lines(&printed);
        return Ok(with_access(entries(settings, &listed, None), access));
    }
    let Some(confine) = &sources.confine else {
        return Err("OpenCode is asked only inside a sandbox, and none was given".to_owned());
    };
    let workdir = workdir(sources);
    let server =
        PrivateServer::new(&program, version.as_deref(), &workdir).confined(Arc::clone(confine));
    let wait = sources.opencode_wait;
    let answer = listing::ask(&server, &workdir, wait, Vec::new()).await;
    let listed = match answer.listing {
        Listing::Listed(listed) => listed,
        Listing::StillEmpty { asks, .. } => {
            return Err(format!(
                "OpenCode listed no models in {}s ({asks} asks)",
                wait.up_to.as_secs()
            ));
        }
        Listing::Failed { why, .. } => return Err(why),
    };
    Ok(with_access(
        entries(settings, &listed, answer.default.as_deref()),
        access,
    ))
}

/// The registry's entries for what OpenCode listed, in the order `/model` shows them, with
/// the one `default` names marked as what Default runs.
pub(super) fn entries(
    settings: &Settings,
    listed: &[ListedModel],
    default: Option<&str>,
) -> Vec<ModelEntry> {
    let mut ordered: Vec<&ListedModel> = listed.iter().collect();
    ordered.sort_by(|a, b| {
        let place = |model: &ListedModel| match () {
            _ if model.local() => 0,
            _ if model.provider == ZEN => 2,
            _ => 1,
        };
        place(a).cmp(&place(b)).then_with(|| {
            if a.local() {
                (!a.tools, name(a)).cmp(&(!b.tools, name(b)))
            } else {
                a.provider
                    .cmp(&b.provider)
                    .then(b.released.cmp(&a.released))
            }
        })
    });
    ordered
        .into_iter()
        .map(|model| entry(settings, model, default == Some(model.route.as_str())))
        .collect()
}

/// What the picker calls it: OpenCode's own name, else the one its id gives.
fn name(model: &ListedModel) -> String {
    model
        .own_name()
        .map_or_else(|| display_name_from_id(model.model_id()), str::to_owned)
}

/// One listed model. Its own name counts only when it isn't the id again, so the pickers' rule
/// for names from ids names the rest. Its window is OpenCode's `limit.context`; a server here
/// that holds the model to less says so through [`super::hold_to_their_servers`].
fn entry(settings: &Settings, model: &ListedModel, runs_by_default: bool) -> ModelEntry {
    let mut entry = cli_entry(settings, ADAPTER, Some(&model.route));
    entry.discovered = true;
    entry.name = model.own_name().and_then(plain_text);
    entry.tools = model.tools;
    entry.context_window = model.context;
    // A level reaches OpenCode inside the model as `model#variant`, so the variants are its
    // levels, and a model with none takes none.
    entry.variants = Some(model.variants.clone());
    entry.efforts = model.variants.clone();
    entry.retired = model.deprecated;
    entry.cli_default = runs_by_default;
    if model.local() {
        entry.access = ModelAccess::Local;
        entry.cost = cost(CostUnit::None);
    } else if model.free {
        entry.cost = cost(CostUnit::None);
    } else if let Some(price) = &model.price {
        entry.cost = price.clone();
    }
    entry.listed_cost = model.local() || model.free || model.price.is_some();
    entry
}

/// A model OpenCode reaches through a hosted provider is paid as your OpenCode is set up.
fn with_access(mut models: Vec<ModelEntry>, access: ModelAccess) -> Vec<ModelEntry> {
    for entry in &mut models {
        if entry.access != ModelAccess::Local {
            entry.access = access;
        }
    }
    models
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODEL_LIST: &str = include_str!("../../../tests/fixtures/opencode/model-list.json");
    const MODEL_DEFAULT: &str = include_str!("../../../tests/fixtures/opencode/model-default.json");

    fn listed() -> Vec<ModelEntry> {
        let listed = listing::parse_model_list(MODEL_LIST).unwrap();
        let default = listing::parse_model_default(MODEL_DEFAULT).unwrap();
        entries(&Settings::default(), &listed, default.as_deref())
    }

    fn find<'a>(models: &'a [ModelEntry], id: &str) -> &'a ModelEntry {
        models
            .iter()
            .find(|e| e.id.to_string() == id)
            .unwrap_or_else(|| panic!("{id} isn't listed"))
    }

    /// A settings change reads the cached list again, and what OpenCode's own listing decided
    /// stays: a model on this computer is local, and a free model costs nothing.
    #[test]
    fn the_cached_list_keeps_what_opencode_listed() {
        use argonauts_core::AuthMode;
        use argonauts_core::registry::ModelRegistry;
        use time::OffsetDateTime;

        use crate::models::{FileRegistry, ModelCache};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models-cache.json");
        let cache = ModelCache {
            refreshed: OffsetDateTime::now_utc(),
            models: listed(),
            skipped: Vec::new(),
            claude_listed: None,
            catalog: None,
            sources: Vec::new(),
        };
        cache.write(&path).unwrap();

        for auth in [AuthMode::Subscription, AuthMode::ApiKey] {
            let mut settings = Settings::default();
            settings.agents.entry(ADAPTER.to_owned()).or_default().auth = auth;
            let models: Vec<ModelEntry> = FileRegistry::from_files(&settings, None, None)
                .with_cache(&path)
                .list()
                .into_iter()
                .filter(|e| e.id.agent == ADAPTER && e.discovered)
                .collect();
            let local = models.iter().filter(|e| e.access == ModelAccess::Local);
            let free = models
                .iter()
                .filter(|e| e.access != ModelAccess::Local && e.cost.unit == CostUnit::None);
            assert_eq!(models.len(), 17, "{auth:?}");
            assert_eq!(local.clone().count(), 7, "{auth:?}");
            assert!(local.into_iter().all(|e| e.cost.unit == CostUnit::None));
            assert_eq!(free.count(), 10, "{auth:?}");
        }
    }

    /// OpenCode's own names stand as it writes them, joined words and all, wherever a model is
    /// named: its picker row, a line that names the agent, and a note about a model it lists.
    #[test]
    fn opencodes_names_read_as_opencode_writes_them() {
        use argonauts_core::names::ModelRef;

        let catalog = crate::models::catalog::cli_layer(&listed());
        let id = |model: &str| ModelRef::new(ADAPTER, Some(model.to_owned()));
        let label = |model: &str| catalog.label(&id(model)).name;
        assert_eq!(
            label("opencode/mimo-v2.6-flash-free"),
            "MiMo-V2.6-Flash Free"
        );
        assert_eq!(label("opencode/glm-5.3-flash-free"), "GLM-5.3-Flash Free");
        assert_eq!(label("ollama/qwen3-coder:30b-64k"), "Qwen3 Coder 30B (64k)");
        let names = argonauts_core::display_names::DisplayNames {
            catalog: catalog.clone(),
            ..Default::default()
        };
        assert_eq!(
            names.worker(&id("opencode/mimo-v2.6-flash-free")),
            "MiMo-V2.6-Flash Free on OpenCode"
        );
    }

    #[test]
    fn every_model_opencode_can_use_is_listed() {
        let models = listed();
        assert_eq!(models.len(), 17);
        assert_eq!(
            models
                .iter()
                .filter(|e| e.access == ModelAccess::Local)
                .count(),
            7
        );
        assert!(
            models
                .iter()
                .all(|e| e.discovered && e.id.agent == "opencode")
        );

        let qwen = find(&models, "opencode/ollama/qwen3-coder:30b-64k");
        assert_eq!(qwen.name.as_deref(), Some("Qwen3 Coder 30B (64k)"));
        assert_eq!(qwen.context_window, Some(65_536));
        assert_eq!(qwen.window, None, "until its server says");
        assert_eq!(qwen.provider, "alibaba");
        assert!(qwen.tools);
        assert_eq!(qwen.variants, Some(Vec::new()));
        assert_eq!(qwen.cost.unit, CostUnit::None);

        // Found by OpenCode itself, so named by the pickers' rule.
        let gemma = find(&models, "opencode/ollama/gemma3:12b-32k");
        assert_eq!(gemma.name, None);
        assert_eq!(gemma.context_window, Some(131_072));
        assert!(!gemma.tools);

        let deepseek = find(&models, "opencode/opencode/deepseek-v4.1-flash-free");
        assert_eq!(deepseek.name.as_deref(), Some("DeepSeek V4.1 Flash Free"));
        assert_eq!(deepseek.context_window, Some(200_000));
        let levels = ["low", "high", "max"].map(str::to_owned).to_vec();
        assert_eq!(deepseek.variants.as_ref(), Some(&levels));
        assert_eq!(deepseek.efforts, levels);
        assert_eq!(deepseek.access, ModelAccess::Subscription);
        assert_eq!(deepseek.cost.unit, CostUnit::None, "free on Zen");
    }

    #[test]
    fn the_default_is_the_model_opencode_names() {
        let defaults: Vec<String> = listed()
            .iter()
            .filter(|e| e.cli_default)
            .map(|e| e.id.to_string())
            .collect();
        assert_eq!(defaults, ["opencode/opencode/nemotron-3.5-super-vl-free"]);
    }

    #[test]
    fn local_models_come_first_with_tool_use_then_by_name_and_zen_last() {
        let models = listed();
        let local: Vec<&ModelEntry> = models
            .iter()
            .take_while(|e| e.access == ModelAccess::Local)
            .collect();
        assert_eq!(local.len(), 7, "every local model before the hosted ones");
        let tools_then_not: Vec<bool> = local.iter().map(|e| e.tools).collect();
        let mut sorted = tools_then_not.clone();
        sorted.sort_by_key(|tools| !tools);
        assert_eq!(tools_then_not, sorted, "tool use first");
        assert!(models[7..].iter().all(|e| {
            e.id.model
                .as_deref()
                .is_some_and(|m| m.starts_with("opencode/"))
        }));
    }

    #[test]
    fn opencode_one_lists_ids_alone() {
        let lines = "ollama/qwen3-coder:30b-64k\nopencode/big-pickle\n\nsome notice here\n";
        let listed = listing::parse_model_lines(lines);
        let models = entries(&Settings::default(), &listed, None);
        let ids: Vec<String> = models.iter().map(|e| e.id.to_string()).collect();
        assert_eq!(
            ids,
            [
                "opencode/ollama/qwen3-coder:30b-64k",
                "opencode/opencode/big-pickle"
            ]
        );
        assert_eq!(models[0].access, ModelAccess::Local);
        assert!(answers_alone(Some("1.18.34")));
        assert!(!answers_alone(Some("2.0.32")));
        assert!(!answers_alone(None));
    }

    #[test]
    fn each_known_runtime_is_its_own_endpoint() {
        assert_eq!(local_endpoint("ollama"), Some("ollama"));
        assert_eq!(local_endpoint("llamacpp"), Some("llamacpp"));
        assert_eq!(local_endpoint("vllm"), Some("vllm"));
        assert_eq!(local_endpoint("openrouter"), None);
    }
}
