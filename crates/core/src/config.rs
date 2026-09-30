//! Layered configuration. Built-in defaults come first, then the global file, the shared project
//! file, your own project file, and `--set key=value` overrides, with later layers winning.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::adapter::KNOWN_AGENT_IDS;
use crate::budget::{BudgetLimit, BudgetUnit, DEFAULT_SOFT_RATIO, TokenPrice, WorkLimits};
use crate::hash::{canonical_json, sha256_hex};
use crate::paths::{global_config_path, project_config_path, project_local_config_path};
use crate::pricing::builtin_price;
use crate::session::AuthMode;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Config {
    /// Adapter id used when a run doesn't name one.
    pub default_agent: String,
    /// Per-agent settings, keyed by adapter id.
    pub agents: BTreeMap<String, AgentConfig>,
    /// Per-card spending limits.
    pub budget: BudgetConfig,
    /// Commands that must pass before a card can merge.
    pub gates: GatesConfig,
    pub merge: MergeConfig,
    pub solo: SoloConfig,
    pub ui: UiConfig,
}

impl Default for Config {
    fn default() -> Self {
        let agents = KNOWN_AGENT_IDS
            .iter()
            .map(|id| ((*id).to_owned(), AgentConfig::default()))
            .collect();
        Self {
            default_agent: "claude-code".to_owned(),
            agents,
            budget: BudgetConfig::default(),
            gates: GatesConfig::default(),
            merge: MergeConfig::default(),
            solo: SoloConfig::default(),
            ui: UiConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct AgentConfig {
    /// Model id or alias. Unset uses the CLI's default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reasoning effort, such as `low`. Unset uses the CLI's default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub auth: AuthMode,
    /// Token prices for estimating dollars when the CLI reports only tokens. Unset uses the
    /// built-in price for `model`, if there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<TokenPrice>,
}

impl AgentConfig {
    pub fn effective_price(&self) -> Option<TokenPrice> {
        self.price
            .or_else(|| self.model.as_deref().and_then(builtin_price))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct BudgetConfig {
    pub unit: BudgetUnit,
    /// The limit per card, in `unit`.
    pub per_card: f64,
    /// Share of the limit at which the agent is asked to wrap up.
    pub soft_ratio: f64,
    /// Most minutes the agent may work on a card. 0 means no limit.
    pub max_minutes: f64,
    /// Most tool calls the agent may start on a card. 0 means no limit.
    pub max_tool_calls: u64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            unit: BudgetUnit::Usd,
            per_card: 3.0,
            soft_ratio: DEFAULT_SOFT_RATIO,
            max_minutes: 60.0,
            max_tool_calls: 300,
        }
    }
}

impl BudgetConfig {
    pub fn per_card_limit(&self) -> BudgetLimit {
        BudgetLimit::from_unit(self.unit, self.per_card)
    }

    /// Zero limits become `None`.
    pub fn work_limits(&self) -> WorkLimits {
        WorkLimits {
            max_minutes: (self.max_minutes > 0.0).then_some(self.max_minutes),
            max_tool_calls: (self.max_tool_calls > 0).then_some(self.max_tool_calls),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SoloConfig {
    /// Have the agent restate the goal for approval before building. Not supported yet, so it
    /// must stay `false`; the task itself is recorded as the goal.
    pub restate_first: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct GatesConfig {
    /// Commands run in the card's worktree, each as an argument vector, such as `["cargo", "test"]`.
    pub commands: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct MergeConfig {
    /// `ask` shows the merge card. `leave` keeps the card branch without asking.
    pub mode: MergeMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MergeMode {
    #[default]
    Ask,
    Leave,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct UiConfig {
    /// How much of each agent's work the transcript shows.
    pub verbosity: Verbosity,
    pub keys: KeyScheme,
    /// Append-only plain text output instead of the full screen.
    pub plain: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Verbosity {
    /// Every tool call and message.
    Full,
    /// Finished steps collapse to one line.
    #[default]
    Normal,
    /// Phases and checkpoints only.
    Quiet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum KeyScheme {
    #[default]
    #[serde(rename = "default")]
    Standard,
    Vim,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("invalid config in {layer}: {message}")]
    Layer {
        /// A file path or `--set budget.unit=yen`.
        layer: String,
        message: String,
    },
    #[error("invalid --set {raw:?}: {reason}")]
    Override { raw: String, reason: String },
    #[error("invalid config: {0}")]
    Invalid(String),
    #[error("could not serialize config: {0}")]
    Serialize(String),
}

/// A `None` file is skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigSources {
    pub global: Option<PathBuf>,
    pub project: Option<PathBuf>,
    /// Your own project file, which wins over the shared one.
    pub project_local: Option<PathBuf>,
    /// `key=value`, applied in order.
    pub overrides: Vec<String>,
}

impl ConfigSources {
    /// The global file plus `<repo_root>/.argonauts/config.toml` and `config.local.toml`, with
    /// `overrides` on top.
    pub fn standard(repo_root: Option<&Path>, overrides: Vec<String>) -> Self {
        Self {
            global: global_config_path().ok(),
            project: repo_root.map(project_config_path),
            project_local: repo_root.map(project_local_config_path),
            overrides,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerInfo {
    /// `defaults`, `global`, `project`, or `project-local`.
    pub name: String,
    pub path: Option<PathBuf>,
    /// The file existed and was applied.
    pub applied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Override {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedConfig {
    pub config: Config,
    pub layers: Vec<LayerInfo>,
    pub overrides: Vec<Override>,
    /// Problems that did not stop loading, such as unknown keys.
    pub warnings: Vec<String>,
}

impl LoadedConfig {
    pub fn hash(&self) -> String {
        self.config.resolved_hash()
    }
}

impl Config {
    /// Resolves every layer in `sources`. Missing files are skipped; unreadable or invalid ones fail.
    pub fn load(sources: &ConfigSources) -> Result<LoadedConfig, ConfigError> {
        let mut merged = Config::default().to_toml_table()?;
        let layers = apply_file_layers(&mut merged, sources)?;
        let overrides = apply_overrides(&mut merged, &sources.overrides)?;
        let config = config_from_table(&merged, "config")?;
        config.validate()?;
        let warnings = config.load_warnings(&merged)?;
        Ok(LoadedConfig {
            config,
            layers,
            overrides,
            warnings,
        })
    }

    /// Parses one TOML layer on top of the built-in defaults.
    pub fn from_toml_str(text: &str) -> Result<Config, ConfigError> {
        let layer = parse_toml(text, "config")?;
        let mut merged = Config::default().to_toml_table()?;
        merge_tables(&mut merged, layer);
        let config = config_from_table(&merged, "config")?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.default_agent.trim().is_empty() {
            return invalid("default_agent is empty".to_owned());
        }
        validate_budget(&self.budget)?;
        if self.solo.restate_first {
            return invalid(
                "solo.restate_first = true isn't supported yet; the task is recorded as the goal"
                    .to_owned(),
            );
        }
        validate_gates(&self.gates)?;
        validate_prices(&self.agents)
    }

    /// Unknown keys in `merged`, and a `default_agent` no adapter answers to.
    fn load_warnings(&self, merged: &toml::Table) -> Result<Vec<String>, ConfigError> {
        let mut warnings: Vec<String> = unknown_keys(merged, &self.to_toml_table()?)
            .into_iter()
            .map(|key| format!("unknown config key `{key}` was ignored"))
            .collect();
        if !KNOWN_AGENT_IDS.contains(&self.default_agent.as_str()) {
            warnings.push(format!(
                "default_agent `{}` is not a known adapter ({})",
                self.default_agent,
                KNOWN_AGENT_IDS.join(", ")
            ));
        }
        Ok(warnings)
    }

    /// The defaults when `id` has no entry.
    pub fn agent(&self, id: &str) -> AgentConfig {
        self.agents.get(id).cloned().unwrap_or_default()
    }

    pub fn json_schema() -> Value {
        schemars::schema_for!(Config).to_value()
    }

    pub fn to_json(&self) -> Value {
        // Map keys are all strings and non-finite floats become null, so this can't fail.
        serde_json::to_value(self).expect("Config always serializes to JSON")
    }

    /// Two runs with the same hash ran with the same settings.
    pub fn resolved_hash(&self) -> String {
        sha256_hex(canonical_json(&self.to_json()).as_bytes())
    }

    pub fn to_toml_string(&self) -> Result<String, ConfigError> {
        toml::to_string_pretty(self).map_err(|e| ConfigError::Serialize(e.to_string()))
    }

    fn to_toml_table(&self) -> Result<toml::Table, ConfigError> {
        match toml::Value::try_from(self) {
            Ok(toml::Value::Table(table)) => Ok(table),
            Ok(_) => Err(ConfigError::Serialize("config is not a table".to_owned())),
            Err(e) => Err(ConfigError::Serialize(e.to_string())),
        }
    }
}

/// Merges the global and both project files into `merged`, returning every layer including the
/// defaults.
fn apply_file_layers(
    merged: &mut toml::Table,
    sources: &ConfigSources,
) -> Result<Vec<LayerInfo>, ConfigError> {
    let mut layers = vec![LayerInfo {
        name: "defaults".to_owned(),
        path: None,
        applied: true,
    }];
    let files = [
        ("global", &sources.global),
        ("project", &sources.project),
        ("project-local", &sources.project_local),
    ];
    for (name, path) in files {
        let Some(path) = path else { continue };
        let table = read_toml_file(path)?;
        layers.push(LayerInfo {
            name: name.to_owned(),
            path: Some(path.clone()),
            applied: table.is_some(),
        });
        if let Some(table) = table {
            merge_tables(merged, table);
            config_from_table(merged, &path.display().to_string())?;
        }
    }
    Ok(layers)
}

fn apply_overrides(
    merged: &mut toml::Table,
    raw_overrides: &[String],
) -> Result<Vec<Override>, ConfigError> {
    let mut overrides = Vec::with_capacity(raw_overrides.len());
    for raw in raw_overrides {
        let (record, segments, value) = parse_override(raw)?;
        set_dotted(merged, &segments, value).map_err(|reason| ConfigError::Override {
            raw: raw.clone(),
            reason,
        })?;
        config_from_table(merged, &format!("--set {raw}"))?;
        overrides.push(record);
    }
    Ok(overrides)
}

fn invalid(message: String) -> Result<(), ConfigError> {
    Err(ConfigError::Invalid(message))
}

fn validate_budget(budget: &BudgetConfig) -> Result<(), ConfigError> {
    if budget.unit != BudgetUnit::None && !(budget.per_card.is_finite() && budget.per_card > 0.0) {
        return invalid(format!(
            "budget.per_card must be a positive number, got {}; use budget.unit = \"none\" for no limit",
            budget.per_card
        ));
    }
    if !(budget.soft_ratio > 0.0 && budget.soft_ratio <= 1.0) {
        return invalid(format!(
            "budget.soft_ratio must be above 0 and at most 1, got {}",
            budget.soft_ratio
        ));
    }
    if !(budget.max_minutes.is_finite() && budget.max_minutes >= 0.0) {
        return invalid(format!(
            "budget.max_minutes must be 0 or more, got {}",
            budget.max_minutes
        ));
    }
    Ok(())
}

fn validate_gates(gates: &GatesConfig) -> Result<(), ConfigError> {
    for (i, command) in gates.commands.iter().enumerate() {
        if command
            .first()
            .is_none_or(|program| program.trim().is_empty())
        {
            return invalid(format!("gates.commands[{i}] has no program"));
        }
    }
    Ok(())
}

fn validate_prices(agents: &BTreeMap<String, AgentConfig>) -> Result<(), ConfigError> {
    for (id, agent) in agents {
        let Some(price) = &agent.price else { continue };
        let rates = [
            Some(price.input_per_mtok),
            Some(price.output_per_mtok),
            price.cached_input_per_mtok,
        ];
        if rates
            .into_iter()
            .flatten()
            .any(|rate| !(rate.is_finite() && rate >= 0.0))
        {
            return invalid(format!(
                "agents.{id}.price has a negative or non-finite rate"
            ));
        }
    }
    Ok(())
}

fn read_toml_file(path: &Path) -> Result<Option<toml::Table>, ConfigError> {
    match fs::read_to_string(path) {
        Ok(text) => parse_toml(&text, &path.display().to_string()).map(Some),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::Read {
            path: path.to_owned(),
            source,
        }),
    }
}

fn parse_toml(text: &str, layer: &str) -> Result<toml::Table, ConfigError> {
    text.parse::<toml::Table>().map_err(|e| ConfigError::Layer {
        layer: layer.to_owned(),
        message: e.to_string(),
    })
}

/// Deserializes the merged table, blaming `layer` if it fails.
fn config_from_table(merged: &toml::Table, layer: &str) -> Result<Config, ConfigError> {
    toml::Value::Table(merged.clone())
        .try_into::<Config>()
        .map_err(|e| ConfigError::Layer {
            layer: layer.to_owned(),
            message: e.to_string(),
        })
}

/// Merges `overlay` into `base`. Tables merge key by key; any other value replaces.
fn merge_tables(base: &mut toml::Table, overlay: toml::Table) {
    for (key, value) in overlay {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(existing)), toml::Value::Table(incoming)) => {
                merge_tables(existing, incoming);
            }
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

/// Splits `key=value` into key segments and a TOML value. Text that isn't valid TOML is a string.
fn parse_override(raw: &str) -> Result<(Override, Vec<String>, toml::Value), ConfigError> {
    let fail = |reason: &str| ConfigError::Override {
        raw: raw.to_owned(),
        reason: reason.to_owned(),
    };
    let (key, value) = raw
        .split_once('=')
        .ok_or_else(|| fail("expected key=value"))?;
    let (key, value) = (key.trim(), value.trim());
    let segments: Vec<String> = key.split('.').map(|s| s.trim().to_owned()).collect();
    if segments.iter().any(String::is_empty) {
        return Err(fail("the key has an empty part"));
    }
    let parsed = format!("v = {value}")
        .parse::<toml::Table>()
        .ok()
        .and_then(|mut table| table.remove("v"));
    let toml_value = parsed.unwrap_or_else(|| toml::Value::String(value.to_owned()));
    let record = Override {
        key: key.to_owned(),
        value: value.to_owned(),
    };
    Ok((record, segments, toml_value))
}

fn set_dotted(table: &mut toml::Table, key: &[String], value: toml::Value) -> Result<(), String> {
    let Some((last, parents)) = key.split_last() else {
        return Err("empty key".to_owned());
    };
    let mut current = table;
    for (depth, part) in parents.iter().enumerate() {
        let entry = current
            .entry(part.clone())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        current = match entry {
            toml::Value::Table(inner) => inner,
            _ => return Err(format!("`{}` is not a table", key[..=depth].join("."))),
        };
    }
    current.insert(last.clone(), value);
    Ok(())
}

/// Dotted keys present in `input` but missing from `resolved`.
fn unknown_keys(input: &toml::Table, resolved: &toml::Table) -> Vec<String> {
    let mut found = Vec::new();
    collect_unknown(input, resolved, "", &mut found);
    found
}

fn collect_unknown(
    input: &toml::Table,
    resolved: &toml::Table,
    prefix: &str,
    found: &mut Vec<String>,
) {
    for (key, value) in input {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match (value, resolved.get(key)) {
            (_, None) => found.push(path),
            (toml::Value::Table(inner), Some(toml::Value::Table(known))) => {
                collect_unknown(inner, known, &path, found);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn sources_with(
        global: Option<&str>,
        project: Option<&str>,
        overrides: &[&str],
    ) -> (tempfile::TempDir, ConfigSources) {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, body: Option<&str>| {
            let path = dir.path().join(name);
            if let Some(body) = body {
                fs::write(&path, body).unwrap();
            }
            Some(path)
        };
        let sources = ConfigSources {
            global: write("global.toml", global),
            project: write("project.toml", project),
            project_local: None,
            overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
        };
        (dir, sources)
    }

    #[test]
    fn defaults() {
        let config = Config::default();
        assert_eq!(config.default_agent, "claude-code");
        assert_eq!(config.agents.len(), 3);
        assert_eq!(config.agent("codex").auth, AuthMode::Subscription);
        assert_eq!(config.budget.per_card_limit(), BudgetLimit::Usd(3.0));
        assert_eq!(config.budget.soft_ratio, 0.85);
        assert!(config.gates.commands.is_empty());
        assert_eq!(config.merge.mode, MergeMode::Ask);
        assert_eq!(config.ui.verbosity, Verbosity::Normal);
        assert_eq!(config.ui.keys, KeyScheme::Standard);
        assert_eq!(
            config.budget.work_limits(),
            WorkLimits {
                max_minutes: Some(60.0),
                max_tool_calls: Some(300),
            }
        );
        assert!(!config.solo.restate_first);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn zero_work_limits_mean_none() {
        let config = Config::from_toml_str(
            "[budget]
max_minutes = 0
max_tool_calls = 0",
        )
        .unwrap();
        assert_eq!(config.budget.work_limits(), WorkLimits::default());
    }

    #[test]
    fn effective_price_falls_back_to_the_built_in_table() {
        let mut agent = AgentConfig {
            model: Some("gpt-5.6-luna".into()),
            ..AgentConfig::default()
        };
        assert_eq!(agent.effective_price().unwrap().input_per_mtok, 0.20);
        agent.price = Some(TokenPrice {
            input_per_mtok: 9.0,
            output_per_mtok: 9.0,
            cached_input_per_mtok: None,
        });
        assert_eq!(agent.effective_price().unwrap().input_per_mtok, 9.0);
        assert_eq!(AgentConfig::default().effective_price(), None);
    }

    #[test]
    fn missing_files_leave_the_defaults() {
        let (_dir, sources) = sources_with(None, None, &[]);
        let loaded = Config::load(&sources).unwrap();
        assert_eq!(loaded.config, Config::default());
        assert!(loaded.warnings.is_empty());
        assert_eq!(loaded.layers.len(), 3);
        assert!(loaded.layers.iter().skip(1).all(|l| !l.applied));
    }

    #[test]
    fn later_layers_win_and_tables_merge() {
        let global = r#"
            default_agent = "codex"
            [agents.codex]
            model = "gpt-5.6-luna"
            effort = "low"
            [ui]
            keys = "vim"
        "#;
        let project = r#"
            [agents.codex]
            effort = "high"
            [budget]
            per_card = 5
            [gates]
            commands = [["cargo", "test"]]
        "#;
        let (_dir, sources) = sources_with(Some(global), Some(project), &[]);
        let loaded = Config::load(&sources).unwrap();
        let config = &loaded.config;
        assert_eq!(config.default_agent, "codex");
        let codex = config.agent("codex");
        assert_eq!(codex.model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(codex.effort.as_deref(), Some("high"));
        assert_eq!(config.ui.keys, KeyScheme::Vim);
        assert_eq!(config.budget.per_card, 5.0);
        assert_eq!(config.budget.unit, BudgetUnit::Usd);
        assert_eq!(config.gates.commands, [["cargo", "test"]]);
        assert!(loaded.layers.iter().all(|l| l.applied));
    }

    #[test]
    fn your_own_project_file_wins_over_the_shared_one() {
        let (dir, mut sources) = sources_with(
            Some("[ui]\nkeys = \"vim\""),
            Some("default_agent = \"codex\"\n[agents.codex]\neffort = \"low\""),
            &["agents.codex.model=gpt-5.6-luna"],
        );
        let local = dir.path().join("config.local.toml");
        fs::write(&local, "[agents.codex]\neffort = \"high\"\nmodel = \"x\"").unwrap();
        sources.project_local = Some(local);
        let loaded = Config::load(&sources).unwrap();
        let codex = loaded.config.agent("codex");
        assert_eq!(
            loaded.config.default_agent, "codex",
            "the shared file still applies"
        );
        assert_eq!(codex.effort.as_deref(), Some("high"));
        assert_eq!(
            codex.model.as_deref(),
            Some("gpt-5.6-luna"),
            "--set still wins"
        );
        assert_eq!(loaded.config.ui.keys, KeyScheme::Vim);
        let names: Vec<&str> = loaded.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["defaults", "global", "project", "project-local"]);
        assert!(loaded.layers.iter().all(|l| l.applied));
    }

    #[test]
    fn overrides_apply_last_and_parse_types() {
        let (_dir, sources) = sources_with(
            Some("default_agent = \"codex\""),
            None,
            &[
                "default_agent=antigravity-cli",
                "budget.unit=tokens",
                "budget.per_card=200000",
                "ui.plain=true",
                "agents.claude-code.model=haiku",
                r#"gates.commands=[["test", "-f", "hello.txt"]]"#,
                "merge.mode = leave",
            ],
        );
        let loaded = Config::load(&sources).unwrap();
        let config = &loaded.config;
        assert_eq!(config.default_agent, "antigravity-cli");
        assert_eq!(config.budget.per_card_limit(), BudgetLimit::Tokens(200_000));
        assert!(config.ui.plain);
        assert_eq!(config.agent("claude-code").model.as_deref(), Some("haiku"));
        assert_eq!(config.gates.commands, [["test", "-f", "hello.txt"]]);
        assert_eq!(config.merge.mode, MergeMode::Leave);
        assert_eq!(loaded.overrides[0].key, "default_agent");
        assert_eq!(loaded.overrides[0].value, "antigravity-cli");
        assert_eq!(loaded.overrides.len(), 7);
    }

    #[test]
    fn bad_overrides_are_rejected() {
        for raw in ["no-equals", ".a=1", "a..b=1", "ui.plain.x=1"] {
            let (_dir, sources) = sources_with(None, None, &[raw]);
            let err = Config::load(&sources).unwrap_err();
            assert!(matches!(err, ConfigError::Override { .. }), "{raw}: {err}");
        }
    }

    #[test]
    fn wrong_types_name_the_layer() {
        let (_dir, sources) = sources_with(None, Some("[budget]\nunit = \"yen\""), &[]);
        let err = Config::load(&sources).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("project.toml"), "{text}");

        let (_dir, sources) = sources_with(None, None, &["ui.verbosity=loud"]);
        let err = Config::load(&sources).unwrap_err().to_string();
        assert!(err.contains("--set ui.verbosity=loud"), "{err}");
    }

    #[test]
    fn broken_toml_is_an_error() {
        let (_dir, sources) = sources_with(Some("default_agent = "), None, &[]);
        assert!(matches!(
            Config::load(&sources),
            Err(ConfigError::Layer { .. })
        ));
    }

    #[test]
    fn unreadable_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let sources = ConfigSources {
            global: Some(dir.path().to_owned()),
            ..ConfigSources::default()
        };
        assert!(matches!(
            Config::load(&sources),
            Err(ConfigError::Read { .. })
        ));
    }

    #[test]
    fn unknown_keys_warn_but_load() {
        let body =
            "preset = \"peer-crew\"\n[triage]\nsolo_max_files = 3\n[agents.codex]\nflavor = \"x\"";
        let (_dir, sources) = sources_with(None, Some(body), &["default_agent=gemini"]);
        let loaded = Config::load(&sources).unwrap();
        let warnings = loaded.warnings.join("\n");
        assert!(warnings.contains("`preset`"), "{warnings}");
        assert!(warnings.contains("`triage`"), "{warnings}");
        assert!(warnings.contains("`agents.codex.flavor`"), "{warnings}");
        assert!(warnings.contains("default_agent `gemini`"), "{warnings}");
    }

    #[test]
    fn validation_rules() {
        let cases = [
            "default_agent = \"\"",
            "[budget]\nper_card = 0",
            "[budget]\nper_card = -1",
            "[budget]\nsoft_ratio = 0",
            "[budget]\nsoft_ratio = 1.5",
            "[gates]\ncommands = [[]]",
            "[gates]\ncommands = [[\"\"]]",
            "[agents.codex.price]\ninput_per_mtok = -1.0\noutput_per_mtok = 1.0",
            "[budget]\nmax_minutes = -5",
            "[solo]\nrestate_first = true",
        ];
        for body in cases {
            assert!(
                matches!(Config::from_toml_str(body), Err(ConfigError::Invalid(_))),
                "{body}"
            );
        }
        let no_budget = Config::from_toml_str("[budget]\nunit = \"none\"\nper_card = 0").unwrap();
        assert_eq!(no_budget.budget.per_card_limit(), BudgetLimit::None);
    }

    #[test]
    fn prices_parse() {
        let config = Config::from_toml_str(
            "[agents.codex.price]\ninput_per_mtok = 1.25\noutput_per_mtok = 10\ncached_input_per_mtok = 0.125",
        )
        .unwrap();
        let price = config.agent("codex").price.unwrap();
        assert_eq!(price.output_per_mtok, 10.0);
        assert_eq!(price.cached_input_per_mtok, Some(0.125));
    }

    #[test]
    fn hash_is_stable_and_sensitive() {
        let a = Config::default();
        let b = Config::default();
        assert_eq!(a.resolved_hash(), b.resolved_hash());
        assert_eq!(a.resolved_hash().len(), 64);
        let mut c = Config::default();
        c.budget.per_card = 4.0;
        assert_ne!(a.resolved_hash(), c.resolved_hash());
        let loaded = Config::load(&ConfigSources::default()).unwrap();
        assert_eq!(loaded.hash(), a.resolved_hash());
    }

    #[test]
    fn toml_round_trip() {
        let mut config = Config::default();
        config.gates.commands = vec![vec!["cargo".into(), "test".into()]];
        config.agents.get_mut("codex").unwrap().model = Some("gpt-5.6-luna".into());
        let text = config.to_toml_string().unwrap();
        assert!(text.contains("[agents.codex]"), "{text}");
        assert_eq!(Config::from_toml_str(&text).unwrap(), config);
    }

    #[test]
    fn schema_describes_and_accepts_the_defaults() {
        let schema = Config::json_schema();
        let props = schema["properties"].as_object().unwrap();
        for key in ["default_agent", "agents", "budget", "gates", "merge", "ui"] {
            assert!(props.contains_key(key), "missing {key}");
        }
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.is_valid(&Config::default().to_json()));
        let mut bad = Config::default().to_json();
        bad["budget"]["unit"] = "yen".into();
        assert!(!validator.is_valid(&bad));
        bad = Config::default().to_json();
        bad["ui"]["keys"] = "emacs".into();
        assert!(!validator.is_valid(&bad));
    }

    #[test]
    fn standard_sources_point_at_the_repo() {
        let sources = ConfigSources::standard(Some(Path::new("/r")), vec!["a=1".into()]);
        assert_eq!(
            sources.project.as_deref(),
            Some(Path::new("/r/.argonauts/config.toml"))
        );
        assert_eq!(
            sources.project_local.as_deref(),
            Some(Path::new("/r/.argonauts/config.local.toml"))
        );
        assert_eq!(sources.overrides, ["a=1"]);
        let outside = ConfigSources::standard(None, vec![]);
        assert!(outside.project.is_none() && outside.project_local.is_none());
    }
}
