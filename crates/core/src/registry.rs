//! The model registry and CLI manifests, read from `models.toml` in `~/.argonauts/` and a
//! project's `.argonauts/`, so new models need no code change.

pub mod acp;
pub mod cli;
pub mod listed;
pub mod provider;

pub use cli::CliManifest;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::future::BoxFuture;
use crate::limits::CostEstimate;
use crate::model_catalog::ModelCatalog;
use crate::names::{ModelRef, RoleName};
use crate::session_runner::RunnerKind;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RegistryFile {
    #[serde(rename = "model")]
    pub models: Vec<ModelEntry>,
    #[serde(rename = "cli")]
    pub clis: Vec<CliManifest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelEntry {
    pub id: ModelRef,
    /// What review and judging rules compare, such as `claude`, `gpt`, `gemini`.
    pub provider: String,
    pub kind: RegistryKind,
    pub access: ModelAccess,
    #[serde(default)]
    pub tools: bool,
    pub cost: CostModel,
    /// A prior on what it's good at, until track records say more.
    #[serde(default)]
    pub strengths: Vec<String>,
    #[serde(default)]
    pub good_for: Vec<RoleName>,
    /// In tokens: the largest window its CLI offers for it, which a session gets unless
    /// `sessions.context` lowers it.
    #[serde(default)]
    pub context_window: Option<u64>,
    /// Reasoning effort levels the model accepts, from `input::EFFORT_LEVELS`. Empty when
    /// unknown, and then `/effort` offers every level.
    #[serde(default)]
    pub efforts: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// The CLI rejects it; `successor` names the model that replaces it.
    #[serde(default)]
    pub retired: bool,
    #[serde(default)]
    pub successor: Option<String>,
    #[serde(default)]
    pub discovered: bool,
    /// The model the CLI runs when you name none, as the CLI's own list marks it. Only
    /// discovery sets it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cli_default: bool,
    /// The effort the CLI runs this model at when you name none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
    #[serde(default)]
    pub launch: Option<LaunchHints>,
    /// Your own strength rating, 1 to 5, which decides who counts as weaker after a fallback.
    /// Track records decide when it's unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<u8>,
    /// The CLI's own name for it, such as Codex's `GPT-6.1-Sol`. The pickers derive one from
    /// the id when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The CLI's own one-line description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// In tokens: the window its server holds it to, from the server's own answer, such as the
    /// cap a local tag sets or the size the server loaded it with. It can be less than
    /// `context_window`, the most the model can take, and stays unset until a server says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
    /// The memory it holds once loaded, in bytes, for a model that runs on this computer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_bytes: Option<u64>,
    /// Whether it can think before it answers, where its server or CLI says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<bool>,
    /// The variants its CLI lists for it, such as OpenCode's `low`, `high` and `max`, which a
    /// launch names after the model as `model#high`. Empty when the CLI lists none, and unset
    /// when it doesn't say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variants: Option<Vec<String>>,
    /// The cost comes from its CLI's own listing of it, as OpenCode's free and priced models'
    /// do, so it stays when you change how that CLI is paid.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub listed_cost: bool,
}

impl ModelEntry {
    /// The window a session on it is held to: the smaller of what its server holds it to and
    /// the most its CLI offers. A model a server on this computer found for itself with no cap
    /// of its own is sized as it loads, from the memory the server has, so it has none until
    /// the server says what it loaded.
    pub fn session_window(&self) -> Option<u64> {
        let sized_as_it_loads = self.discovered && self.access == ModelAccess::Local;
        match (self.window, self.context_window) {
            (Some(held), most) => Some(most.map_or(held, |most| most.min(held))),
            (None, _) if sized_as_it_loads => None,
            (None, most) => most,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RegistryKind {
    Cli,
    Endpoint,
}

impl From<RegistryKind> for RunnerKind {
    fn from(kind: RegistryKind) -> Self {
        match kind {
            RegistryKind::Cli => RunnerKind::Cli,
            RegistryKind::Endpoint => RunnerKind::Endpoint,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ModelAccess {
    /// Your plan's usage, through the official CLI.
    Subscription,
    ApiKey,
    Local,
}

/// Prices are dollars per million tokens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CostModel {
    pub unit: CostUnit,
    #[serde(default)]
    pub input_per_mtok: Option<f64>,
    #[serde(default)]
    pub output_per_mtok: Option<f64>,
    #[serde(default)]
    pub cached_input_per_mtok: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CostUnit {
    PlanUsage,
    Usd,
    /// Free, local.
    None,
}

/// Launch hints the adapter reads, so new open-weight setups need no code change.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct LaunchHints {
    /// Codex: run with `--oss`.
    pub oss: bool,
    /// Codex: `--local-provider ollama|lmstudio`.
    pub local_provider: Option<String>,
    /// OpenCode: `provider/model` for `--model`.
    pub provider_model: Option<String>,
}

/// The model a session of `worker` runs, spelled one way, so the same model counts once
/// however a session named it: the model an alias names, and for a worker that names none
/// the one its CLI's own list marks as the default, else the one the catalog's default runs.
/// A name the registry doesn't know stays as it is, without the variant a CLI adds in
/// brackets, such as `[1m]`. `None` only for a CLI named alone whose default nothing knows.
pub fn model_run(registry: &dyn ModelRegistry, worker: &ModelRef) -> Option<String> {
    let named = worker
        .model
        .as_deref()
        .map(|model| model.split('[').next().unwrap_or(model).trim())
        .filter(|model| !model.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            registry
                .list()
                .into_iter()
                .find(|entry| entry.cli_default && entry.id.agent == worker.agent)
                .and_then(|entry| entry.id.model)
        });
    let id = ModelRef::new(worker.agent.clone(), named.clone());
    registry.catalog().runs(&id).or(named)
}

/// The model `cli` runs when a session names none, as its own list says: the one the list
/// marks as the default, else the one the catalog's default runs, else the first model a list
/// that marks none names, as agy's does. `None` before any list is known.
pub fn default_run(registry: &dyn ModelRegistry, cli: &str) -> Option<String> {
    model_run(registry, &ModelRef::new(cli, None)).or_else(|| {
        registry
            .list()
            .into_iter()
            .filter(|entry| entry.id.agent == cli && !entry.retired)
            .find_map(|entry| entry.id.model)
    })
}

pub trait ModelRegistry: Send + Sync {
    /// Matches aliases too.
    fn get(&self, worker: &ModelRef) -> Option<ModelEntry>;
    /// Falls back to the agent's default provider.
    fn provider(&self, worker: &ModelRef) -> String;
    fn list(&self) -> Vec<ModelEntry>;
    fn estimate(&self, worker: &ModelRef, input_tokens: u64, output_tokens: u64) -> CostEstimate;
    /// Runs discovery (`agy models`, `codex debug models`, Claude Code aliases, `GET /models`)
    /// and returns what it found. Never makes a model call.
    fn discover(&self) -> BoxFuture<'_, Vec<ModelEntry>>;
    /// When discovery last finished, here or in an earlier run whose list was cached. `None`
    /// before the first.
    fn refreshed(&self) -> Option<OffsetDateTime> {
        None
    }

    /// The window the server running `worker` loaded it with, for a server that sizes a model
    /// as it loads it, such as Ollama. `None` while it isn't loaded or nobody can say. Asking
    /// loads nothing.
    fn loaded_window(&self, _worker: &ModelRef) -> BoxFuture<'_, Option<u64>> {
        Box::pin(async { None })
    }

    /// Every catalog layer merged: shipped, fetched, the CLIs' own names, then yours.
    fn catalog(&self) -> ModelCatalog {
        ModelCatalog::default()
    }

    /// The CLI `cli` listed its own models, as Claude Code does when a session starts. The
    /// list counts over the fetched catalog and under your files, and is kept for later runs.
    fn models_reported(&self, cli: &str, models: &[crate::ReportedModel]) {
        let _ = (cli, models);
    }

    /// The model lists discovery reads by running or asking something, by CLI or endpoint
    /// id, such as `codex` or `ollama`. Claude Code's aliases need nothing run, so they aren't
    /// one.
    fn sources(&self) -> Vec<String> {
        Vec::new()
    }

    /// Asks the one source `cli` again and keeps what it lists, as discovery does for every
    /// source. A source that fails keeps its last list. Never makes a model call.
    fn discover_source<'a>(&'a self, cli: &'a str) -> BoxFuture<'a, ()> {
        let _ = cli;
        Box::pin(async {})
    }

    /// When the source `cli` last listed its models, and whether its latest ask failed.
    fn source_listed(&self, cli: &str) -> (Option<OffsetDateTime>, bool) {
        let _ = cli;
        (self.refreshed(), false)
    }

    /// What the source `cli`'s list leaves out here and why, such as a repo's own OpenCode
    /// config before you trust the repo.
    fn source_note(&self, cli: &str) -> Option<String> {
        let _ = cli;
        None
    }
}

/// The levels `worker`'s model takes, as the catalog or its CLI's own list says: empty for a
/// model that takes no effort setting. `None` when nothing says, and for a CLI's own
/// default, whose model the CLI picks.
pub fn known_efforts(registry: &dyn ModelRegistry, worker: &ModelRef) -> Option<Vec<String>> {
    worker.model.as_ref()?;
    let entry = registry.get(worker);
    // A CLI that lists each model's variants, as OpenCode does, says which levels the launch
    // may name, and that a model with none takes none.
    if let Some(variants) = entry.as_ref().and_then(|entry| entry.variants.clone()) {
        return Some(variants);
    }
    registry
        .catalog()
        .entry(worker)
        .and_then(|entry| entry.efforts.clone())
        .or_else(|| {
            entry
                .map(|entry| entry.efforts)
                .filter(|levels| !levels.is_empty())
        })
}

/// The level `worker` runs at with `set` asked for. A model that takes one level runs at it,
/// and one that takes none gets none, so what runs is what `/effort` shows, whatever level was
/// set before. A level the model doesn't take moves to the nearest one it does. Any other
/// model runs at what's set.
pub fn effort_to_run(
    registry: &dyn ModelRegistry,
    worker: &ModelRef,
    set: Option<String>,
) -> Option<String> {
    match known_efforts(registry, worker).as_deref() {
        Some([]) => None,
        Some([only]) => Some(only.clone()),
        Some(levels) => set.map(|set| nearest_level(&set, levels).unwrap_or(set)),
        None => set,
    }
}

/// `asked` when `levels` has it, else the level of `levels` closest to it in
/// [`EFFORT_LEVELS`](crate::input::EFFORT_LEVELS) order, a tie going to the lower one. `None`
/// when neither `asked` nor any of `levels` is a level that order knows.
pub fn nearest_level(asked: &str, levels: &[String]) -> Option<String> {
    if levels.iter().any(|level| level == asked) {
        return Some(asked.to_owned());
    }
    let rank = |level: &str| {
        crate::input::EFFORT_LEVELS
            .iter()
            .position(|known| *known == level)
    };
    let asked = rank(asked)?;
    levels
        .iter()
        .filter_map(|level| Some((rank(level)?, level)))
        .min_by_key(|&(at, _)| (at.abs_diff(asked), at))
        .map(|(_, level)| level.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL_QWEN: &str = r#"
        [[model]]
        id = "opencode/ollama/qwen3-coder:30b-64k"
        provider = "qwen"
        kind = "cli"
        access = "local"
        tools = true
        cost = { unit = "none" }
        context_window = 262144
        window = 65536
        ram_bytes = 26843545600
        thinking = false
        variants = []
    "#;

    /// One OpenCode model per variant list: none, and DeepSeek's low, high and max.
    struct OpenCodeList(Vec<ModelEntry>);

    impl ModelRegistry for OpenCodeList {
        fn get(&self, worker: &ModelRef) -> Option<ModelEntry> {
            self.0.iter().find(|entry| entry.id == *worker).cloned()
        }
        fn provider(&self, _: &ModelRef) -> String {
            String::new()
        }
        fn list(&self) -> Vec<ModelEntry> {
            self.0.clone()
        }
        fn estimate(&self, _: &ModelRef, _: u64, _: u64) -> CostEstimate {
            CostEstimate::Unknown
        }
        fn discover(&self) -> crate::future::BoxFuture<'_, Vec<ModelEntry>> {
            Box::pin(async { Vec::new() })
        }
    }

    fn opencode(model: &str, variants: &[&str]) -> ModelEntry {
        let mut entry = local_qwen();
        entry.id = ModelRef::new("opencode", Some(model.to_owned()));
        entry.variants = Some(variants.iter().map(|&v| v.to_owned()).collect());
        entry
    }

    fn worker(model: &str) -> ModelRef {
        ModelRef::new("opencode", Some(model.to_owned()))
    }

    #[test]
    fn a_launch_names_only_a_level_the_model_lists() {
        let registry = OpenCodeList(vec![
            opencode("ollama/qwen3-coder:30b-64k", &[]),
            opencode("opencode/deepseek-v4.1-flash-free", &["low", "high", "max"]),
        ]);
        let qwen = worker("ollama/qwen3-coder:30b-64k");
        let deepseek = worker("opencode/deepseek-v4.1-flash-free");
        assert_eq!(effort_to_run(&registry, &qwen, Some("high".into())), None);
        let run = |set: &str| effort_to_run(&registry, &deepseek, Some(set.into()));
        assert_eq!(run("high").as_deref(), Some("high"));
        assert_eq!(run("medium").as_deref(), Some("low"), "a tie goes lower");
        assert_eq!(run("xhigh").as_deref(), Some("high"));
        assert_eq!(run("ultra").as_deref(), Some("max"));
        assert_eq!(effort_to_run(&registry, &deepseek, None), None);
        assert_eq!(
            known_efforts(&registry, &qwen),
            Some(Vec::new()),
            "listed with no variants, so it takes none"
        );
    }

    #[test]
    fn the_nearest_level_keeps_one_it_has() {
        let levels: Vec<String> = ["none", "minimal", "low"].map(String::from).to_vec();
        assert_eq!(nearest_level("none", &levels).as_deref(), Some("none"));
        assert_eq!(nearest_level("max", &levels).as_deref(), Some("low"));
        assert_eq!(nearest_level("turbo", &levels), None);
    }

    fn local_qwen() -> ModelEntry {
        let file: RegistryFile = toml::from_str(LOCAL_QWEN).expect("a models.toml entry");
        file.models.into_iter().next().expect("one model")
    }

    #[test]
    fn what_a_server_says_of_a_model_is_read() {
        let entry = local_qwen();
        assert_eq!(entry.context_window, Some(262_144));
        assert_eq!(entry.window, Some(65_536));
        assert_eq!(entry.ram_bytes, Some(25 * 1024 * 1024 * 1024));
        assert_eq!(entry.thinking, Some(false));
        assert_eq!(entry.variants, Some(Vec::new()));
    }

    #[test]
    fn a_session_is_held_to_the_smaller_window() {
        let mut entry = local_qwen();
        assert_eq!(entry.session_window(), Some(65_536));
        entry.window = Some(300_000);
        assert_eq!(entry.session_window(), Some(262_144));

        // A tag with no cap, found on this computer, is sized as it loads.
        entry.window = None;
        entry.discovered = true;
        assert_eq!(entry.session_window(), None);
        // Your own entry for it says its window.
        entry.discovered = false;
        assert_eq!(entry.session_window(), Some(262_144));
        // A hosted model's window is the most its CLI offers.
        entry.discovered = true;
        entry.access = ModelAccess::Subscription;
        assert_eq!(entry.session_window(), Some(262_144));
        entry.context_window = None;
        assert_eq!(entry.session_window(), None);
    }

    #[test]
    fn unset_facts_are_left_out_of_what_is_written() {
        let mut entry = local_qwen();
        let written = serde_json::to_value(&entry).expect("json");
        assert_eq!(written["window"], 65_536);
        assert_eq!(written["variants"], serde_json::json!([]));
        assert_eq!(
            serde_json::from_value::<ModelEntry>(written).expect("read back"),
            entry
        );

        entry.window = None;
        entry.ram_bytes = None;
        entry.thinking = None;
        entry.variants = None;
        let written = serde_json::to_value(&entry).expect("json");
        for key in ["window", "ram_bytes", "thinking", "variants"] {
            assert!(written.get(key).is_none(), "{key} is written unset");
        }
        assert_eq!(
            serde_json::from_value::<ModelEntry>(written).expect("read back"),
            entry
        );
    }
}
