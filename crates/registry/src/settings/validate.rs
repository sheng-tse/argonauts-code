//! The checks serde can't make. Each error names the key, and the layer that set it.

use std::iter;
use std::path::{Component, Path, PathBuf};

use argonauts_core::names::WorkerRef;
use argonauts_core::settings::approvals::Otherwise;
use argonauts_core::settings::evolution::FIXED_CORE_KEYS;
use argonauts_core::settings::sessions::NativeFanout;
use argonauts_core::settings::{Settings, Span};
use toml::Value;

use super::handoff;
use super::roles::{self, Workers};
use super::set_by::SetBy;
use crate::SettingsError;
use crate::catalog::setting_meta;
use crate::harnesses::enabled_agent;

pub(crate) struct Context<'a> {
    /// The repo, when resolving for one. `agents.*.binary` must be outside it.
    pub repo_root: Option<&'a Path>,
    /// Every leaf key of the resolved settings, with its value.
    pub leaves: &'a [(String, Value)],
    pub set_by: &'a SetBy,
    /// The `[[harness]]` ids your `models.toml` files declare.
    pub harness_ids: &'a [String],
}

/// One thing wrong, and the dotted key it's about, which finds the layer that set it.
#[derive(Debug)]
pub(crate) struct Problem {
    pub key: String,
    pub message: String,
}

impl Problem {
    pub fn new(key: &str, message: String) -> Self {
        Self {
            key: key.to_owned(),
            message,
        }
    }
}

/// Returns warnings. The first problem found is the error.
pub(crate) fn validate(
    settings: &Settings,
    context: &Context<'_>,
) -> Result<Vec<String>, SettingsError> {
    let problems = [
        file_problems(settings),
        budget_lines(settings),
        approvals(settings),
        allowed_values(context),
        binaries(settings, context.repo_root),
        handoff::check(settings),
    ];
    if let Some(problem) = problems.into_iter().flatten().next() {
        return Err(SettingsError::Invalid {
            layer: context.set_by.source(&problem.key),
            message: problem.message,
        });
    }
    let workers = Workers::new(settings).resolving(context.harness_ids, turned_on(context));
    let mut warnings = roles::warnings(settings, &workers);
    warnings.extend(handoff::warnings(settings, &workers));
    Ok(warnings)
}

/// The agents a layer set `enabled = true` for, as `agents_turned_on` reads them later.
fn turned_on(context: &Context<'_>) -> Vec<String> {
    context
        .leaves
        .iter()
        .filter(|(key, value)| {
            *value == Value::Boolean(true) && context.set_by.layer(key).is_some()
        })
        .filter_map(|(key, _)| enabled_agent(key))
        .map(str::to_owned)
        .collect()
}

/// A value serde refused, naming its key and the layer that set it.
pub(crate) fn type_error(err: &toml::de::Error, set_by: &SetBy) -> SettingsError {
    // The text ends with "in `key`" when serde knows the key.
    let text = err.to_string();
    let key = text
        .rsplit_once("in `")
        .and_then(|(_, rest)| rest.split_once('`'))
        .map(|(key, _)| key);
    match key {
        Some(key) => SettingsError::Invalid {
            layer: set_by.source(key),
            message: format!("{key}: {}", plain_type_message(err.message())),
        },
        None => SettingsError::Invalid {
            layer: "the merged settings".to_owned(),
            message: text.trim().to_owned(),
        },
    }
}

/// Serde names the type of a key that takes several forms, not the forms themselves.
fn plain_type_message(message: &str) -> String {
    const FORMS: [(&str, &str); 3] = [
        (
            "FallbackEntry",
            "each entry is \"worker/model\", or a table with a worker and its own rules",
        ),
        (
            "RotateAt",
            "use a token count such as \"200k\" or 150000, or a share of the window such as 0.9",
        ),
        ("NativeFanout", "use \"on\", \"off\", or { max = N }"),
    ];
    FORMS
        .iter()
        .find(|(name, _)| message.contains(&format!("untagged enum {name}")))
        .map_or_else(|| message.to_owned(), |(_, plain)| (*plain).to_owned())
}

/// The checks that need nothing from the other layers, so writing a setting can refuse a value
/// resolving would. An endpoint stays one in every layer above, since TOML can't unset a key.
pub(crate) fn file_problems(settings: &Settings) -> Vec<Problem> {
    let workers = Workers::new(settings);
    [
        numbers(settings),
        spans(settings),
        critic(settings),
        evolvable(settings),
        roles::endpoint_problems(settings, &workers),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The evolution loop may only change keys outside the fixed core and the safety keys: a loop
/// that could move its own thresholds, the gates, or what makes the evidence would learn to game
/// them.
fn evolvable(settings: &Settings) -> Vec<Problem> {
    settings
        .evolution
        .evolvable
        .iter()
        .filter_map(|key| {
            let pattern = FIXED_CORE_KEYS
                .iter()
                .chain(argonauts_core::settings::SAFETY_KEYS)
                .find(|pattern| {
                    crate::resolve::key_matches(pattern, key)
                        || crate::resolve::key_matches(key, pattern.trim_end_matches(".*"))
                })?;
            Some(Problem::new(
                "evolution.evolvable",
                format!(
                    "evolution.evolvable can't list {key}: it falls under {pattern}, which only \
                     you change, never the evolution loop"
                ),
            ))
        })
        .collect()
}

/// A token cap below this is a mistake, most likely `1` meant as the whole window, which the
/// untagged `RotateAt` reads as one token.
const MIN_ROTATE_TOKENS: u64 = 1_000;

fn numbers(settings: &Settings) -> Vec<Problem> {
    let mut problems = Vec::new();
    let soft_stop = settings.budget.soft_stop;
    if soft_stop.is_nan() || soft_stop <= 0.0 || soft_stop > 1.0 {
        problems.push(Problem::new(
            "budget.soft_stop",
            format!("budget.soft_stop must be more than 0 and at most 1 (it's {soft_stop})"),
        ));
    }
    problems.extend(dollars(settings));
    match settings.sessions.rotate_at.tokens(None) {
        Err(message) => problems.push(Problem::new(
            "sessions.rotate_at",
            format!("sessions.{message}"),
        )),
        Ok(Some(count)) if count < MIN_ROTATE_TOKENS => problems.push(Problem::new(
            "sessions.rotate_at",
            format!(
                "sessions.rotate_at is {count} tokens, which would rotate on every call; use a \
                 count such as \"200k\", or a share of the window such as 1.0 for all of it"
            ),
        )),
        Ok(_) => {}
    }
    let depth = settings.workflows.max_depth;
    if depth > 2 {
        problems.push(Problem::new(
            "workflows.max_depth",
            format!("workflows.max_depth is at most 2 (it's {depth})"),
        ));
    }
    let roles = settings.roles.iter().map(|(role, table)| {
        (
            format!("roles.{}.native_fanout", role.as_str()),
            table.native_fanout,
        )
    });
    for (key, fanout) in
        iter::once(("native_fanout".to_owned(), Some(settings.native_fanout))).chain(roles)
    {
        if fanout == Some(NativeFanout::Limit { max: 0 }) {
            problems.push(Problem::new(
                &format!("{key}.max"),
                format!("{key}.max must be at least 1; \"off\" turns subagents off"),
            ));
        }
    }
    problems
}

/// A dollar amount that's NaN, infinite, or below zero never trips a cap, or makes paid work
/// look free.
fn dollars(settings: &Settings) -> Vec<Problem> {
    let budget = &settings.budget;
    let limits = &settings.limits;
    let mut amounts = vec![
        ("budget.run_usd".to_owned(), budget.run_usd),
        ("budget.per_card_usd".to_owned(), budget.per_card_usd),
        ("limits.usd_per_day".to_owned(), limits.usd_per_day),
        (
            "limits.evolution_usd_per_month".to_owned(),
            Some(limits.evolution_usd_per_month),
        ),
    ];
    for (agent, table) in &settings.agents {
        if let Some(price) = &table.price {
            let key = |name: &str| format!("agents.{agent}.price.{name}");
            amounts.push((key("input_per_mtok"), Some(price.input_per_mtok)));
            amounts.push((key("output_per_mtok"), Some(price.output_per_mtok)));
            amounts.push((key("cached_input_per_mtok"), price.cached_input_per_mtok));
        }
    }
    amounts
        .into_iter()
        .filter_map(|(key, amount)| Some((key, amount?)))
        .filter(|(_, amount)| !amount.is_finite() || *amount < 0.0)
        .map(|(key, amount)| {
            Problem::new(
                &key,
                format!("{key} must be a number of dollars, 0 or more (it's {amount})"),
            )
        })
        .collect()
}

fn budget_lines(settings: &Settings) -> Vec<Problem> {
    let Ok(serde_json::Value::Object(shares)) = serde_json::to_value(&settings.budget.lines) else {
        return Vec::new();
    };
    let mut problems = Vec::new();
    let mut sum = 0.0;
    for (name, share) in &shares {
        // NaN, which TOML allows, serializes as null.
        let share = share.as_f64().unwrap_or(f64::NAN);
        if !(0.0..=1.0).contains(&share) {
            let key = format!("budget.lines.{name}");
            problems.push(Problem::new(
                &key,
                format!("{key} is a share from 0 to 1 (it's {share})"),
            ));
        }
        sum += share;
    }
    if sum.is_nan() || (sum - 1.0).abs() > 0.001 {
        problems.push(Problem::new(
            "budget.lines",
            format!("budget.lines shares add up to {sum:.3}, and must add up to 1"),
        ));
    }
    problems
}

fn spans(settings: &Settings) -> Vec<Problem> {
    let fixed: [(&str, &Span); 7] = [
        ("gates.timeout", &settings.gates.timeout),
        ("mcp.run_budget", &settings.mcp.run_budget),
        ("ui.recap_after", &settings.ui.recap_after),
        ("narrator.interval", &settings.narrator.interval),
        ("health.window", &settings.health.window),
        ("health.cooldown", &settings.health.cooldown),
        ("supervisor.park_grace", &settings.supervisor.park_grace),
    ];
    // Handoff tables check their own `ask_timeout`.
    let endpoints = settings
        .endpoints
        .iter()
        .map(|(id, endpoint)| (format!("endpoints.{id}.timeout"), &endpoint.timeout));
    fixed
        .into_iter()
        .map(|(key, span)| (key.to_owned(), span))
        .chain(endpoints)
        .filter_map(|(key, span)| {
            let message = span.parse().err()?;
            Some(Problem::new(&key, format!("{key}: {message}")))
        })
        .collect()
}

fn critic(settings: &Settings) -> Vec<Problem> {
    let critic = &settings.critique.critic;
    if critic == "rotate" || critic.parse::<WorkerRef>().is_ok() {
        return Vec::new();
    }
    vec![Problem::new(
        "critique.critic",
        format!("critique.critic is {critic:?}; use \"rotate\" or a worker such as \"codex\""),
    )]
}

fn approvals(settings: &Settings) -> Vec<Problem> {
    let approvals = &settings.approvals;
    let mut problems = Vec::new();
    let shares = [
        ("approvals.spot_check_sample", approvals.spot_check_sample),
        (
            "approvals.merge.rules.min_changed_line_coverage",
            approvals.merge.rules.min_changed_line_coverage,
        ),
    ];
    for (key, share) in shares {
        if !(0.0..=1.0).contains(&share) {
            problems.push(Problem::new(
                key,
                format!("{key} is a share from 0 to 1 (it's {share})"),
            ));
        }
    }

    let approver = &approvals.approver;
    let second = "approvals.approver.second_model";
    match (&approver.model, &approver.second_model) {
        (None, Some(_)) => problems.push(Problem::new(
            second,
            format!("{second} needs approvals.approver.model, the first approver"),
        )),
        (Some(first), Some(other)) if first.worker == other.worker => {
            problems.push(Problem::new(
                second,
                format!(
                    "{second} must come from another vendor than approvals.approver.model, and \
                     both are {}",
                    first.worker
                ),
            ));
        }
        _ => {}
    }

    for (name, otherwise) in otherwise_by_checkpoint(settings) {
        if otherwise == Otherwise::OpenPullRequest && name != "merge" {
            let key = format!("approvals.{name}.otherwise");
            problems.push(Problem::new(
                &key,
                format!("{key} = \"open_pull_request\" is for the merge checkpoint only"),
            ));
        }
    }
    problems
}

fn otherwise_by_checkpoint(settings: &Settings) -> [(&'static str, Otherwise); 7] {
    let approvals = &settings.approvals;
    [
        ("goal", approvals.goal.otherwise),
        ("plan", approvals.plan.otherwise),
        ("merge", approvals.merge.otherwise),
        ("recipe_promotion", approvals.recipe_promotion.otherwise),
        ("escalation", approvals.escalation.otherwise),
        ("budget", approvals.budget.otherwise),
        ("change_request", approvals.change_request.otherwise),
    ]
}

/// Names that may be your own files, so no fixed list covers them.
const OPEN_NAMES: [&str; 2] = ["preset", "workflow"];

/// Values a key's picker doesn't offer, for the keys a layer set.
fn allowed_values(context: &Context<'_>) -> Vec<Problem> {
    context
        .leaves
        .iter()
        .filter(|(key, _)| {
            !OPEN_NAMES.contains(&key.as_str()) && context.set_by.layer(key).is_some()
        })
        .filter_map(|(key, value)| {
            let meta = setting_meta(key)?;
            let allowed: Vec<Value> = meta
                .allowed
                .iter()
                .filter_map(|text| toml_value(text))
                .collect();
            // A key can take several forms, such as `native_fanout`, and its list may cover only
            // one of them. Serde already checked a value of another form.
            let listed_form = allowed
                .iter()
                .any(|choice| choice.type_str() == value.type_str());
            (listed_form && !allowed.contains(value)).then(|| {
                Problem::new(
                    key,
                    format!(
                        "{key} = {value} isn't allowed; use one of {}",
                        meta.allowed.join(", ")
                    ),
                )
            })
        })
        .collect()
}

fn toml_value(text: &str) -> Option<Value> {
    format!("value = {text}")
        .parse::<toml::Table>()
        .ok()?
        .remove("value")
}

/// `doctor` runs each `agents.*.binary` outside any sandbox, so a repo you clone must not be
/// able to supply one.
fn binaries(settings: &Settings, repo_root: Option<&Path>) -> Vec<Problem> {
    settings
        .agents
        .iter()
        .filter_map(|(id, agent)| {
            let binary = agent.binary.as_ref()?;
            let key = format!("agents.{id}.binary");
            if !binary.is_absolute() {
                return Some(Problem::new(
                    &key,
                    format!(
                        "{key} must be an absolute path, since doctor runs it outside any \
                         sandbox ({} isn't)",
                        binary.display()
                    ),
                ));
            }
            let repo = repo_root?;
            real_path(binary).starts_with(real_path(repo)).then(|| {
                Problem::new(
                    &key,
                    format!(
                        "{key} must be outside the repo, since doctor runs it outside any \
                         sandbox ({} is inside {})",
                        binary.display(),
                        repo.display()
                    ),
                )
            })
        })
        .collect()
}

/// The path with its links followed, so neither a link nor `..` hides where it points. A path
/// that doesn't exist yet resolves through the longest part of it that does.
fn real_path(path: &Path) -> PathBuf {
    if let Ok(real) = path.canonicalize() {
        return real;
    }
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normal.pop();
            }
            Component::CurDir => {}
            other => normal.push(other),
        }
    }
    let mut missing = Vec::new();
    let mut existing = normal.as_path();
    loop {
        if let Ok(real) = existing.canonicalize() {
            return missing
                .iter()
                .rev()
                .fold(real, |path, part| path.join(part));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_owned());
                existing = parent;
            }
            _ => return normal,
        }
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::TokenPrice;
    use argonauts_core::settings::agents::AgentSettings;

    use super::*;

    #[test]
    fn dollar_amounts_must_be_real() {
        assert!(dollars(&Settings::default()).is_empty());
        let mut settings = Settings::default();
        settings.budget.run_usd = Some(f64::NAN);
        settings.budget.per_card_usd = Some(-1.0);
        settings.limits.usd_per_day = Some(f64::INFINITY);
        settings.limits.evolution_usd_per_month = -5.0;
        settings.agents.insert(
            "codex".to_owned(),
            AgentSettings {
                price: Some(TokenPrice {
                    input_per_mtok: 1.0,
                    output_per_mtok: f64::NAN,
                    cached_input_per_mtok: Some(-0.1),
                }),
                ..AgentSettings::default()
            },
        );
        let keys: Vec<String> = dollars(&settings).into_iter().map(|p| p.key).collect();
        assert_eq!(
            keys,
            [
                "budget.run_usd",
                "budget.per_card_usd",
                "limits.usd_per_day",
                "limits.evolution_usd_per_month",
                "agents.codex.price.output_per_mtok",
                "agents.codex.price.cached_input_per_mtok",
            ]
        );
        settings.budget.run_usd = Some(0.0);
        assert!(!dollars(&settings).iter().any(|p| p.key == "budget.run_usd"));
    }

    #[test]
    fn settings_refuse_fixed_core_on_evolvable() {
        assert!(evolvable(&Settings::default()).is_empty());
        for key in [
            "review.per_card",
            "judging.judges",
            "evolution.min_gain_points",
            "memory.max_chars",
            "drafting.blind",
            "approvals.merge.mode",
            "limits.usd_per_day",
            "sandbox.network",
            "merge.mode",
            "review",
        ] {
            let mut settings = Settings::default();
            settings.evolution.evolvable.push(key.to_owned());
            let problems = evolvable(&settings);
            assert_eq!(problems.len(), 1, "{key}");
            assert_eq!(problems[0].key, "evolution.evolvable");
            assert!(problems[0].message.contains(key), "{}", problems[0].message);
        }
        let mut settings = Settings::default();
        settings.evolution.evolvable =
            vec!["critique.rounds".to_owned(), "drafting.drafters".to_owned()];
        assert!(evolvable(&settings).is_empty());

        // Resolving refuses it too, naming the layer that set it.
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("config.toml");
        std::fs::write(
            &global,
            "[evolution]\nevolvable = [\"critique.rounds\", \"review.per_card\"]\n",
        )
        .unwrap();
        let sources = crate::SettingsSources {
            global: Some(global),
            ..crate::SettingsSources::default()
        };
        let err = crate::resolve(&sources).unwrap_err().to_string();
        assert!(err.contains("review.per_card"), "{err}");
    }
}
