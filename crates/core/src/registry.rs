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

    /// Every catalog layer merged: shipped, fetched, the CLIs' own names, then yours.
    fn catalog(&self) -> ModelCatalog {
        ModelCatalog::default()
    }

    /// The CLI `cli` listed its own models, as Claude Code does when a session starts. The
    /// list counts over the fetched catalog and under your files, and is kept for later runs.
    fn models_reported(&self, cli: &str, models: &[crate::ReportedModel]) {
        let _ = (cli, models);
    }
}

/// The levels `worker`'s model takes, as the catalog or its CLI's own list says: empty for a
/// model that takes no effort setting. `None` when nothing says, and for a CLI's own
/// default, whose model the CLI picks.
pub fn known_efforts(registry: &dyn ModelRegistry, worker: &ModelRef) -> Option<Vec<String>> {
    worker.model.as_ref()?;
    registry
        .catalog()
        .entry(worker)
        .and_then(|entry| entry.efforts.clone())
        .or_else(|| {
            registry
                .get(worker)
                .map(|entry| entry.efforts)
                .filter(|levels| !levels.is_empty())
        })
}

/// The level `worker` runs at with `set` asked for. A model that takes one level runs at it,
/// and one that takes none gets none, so what runs is what `/effort` shows, whatever level was
/// set before. Any other model runs at what's set.
pub fn effort_to_run(
    registry: &dyn ModelRegistry,
    worker: &ModelRef,
    set: Option<String>,
) -> Option<String> {
    match known_efforts(registry, worker).as_deref() {
        Some([]) => None,
        Some([only]) => Some(only.clone()),
        _ => set,
    }
}
