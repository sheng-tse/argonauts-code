//! The mutation rules, checked before anything is spent on a candidate.

use serde_json::Value;

use argonauts_core::settings::SAFETY_KEYS;
use argonauts_core::settings::Settings;
use argonauts_core::settings::evolution::FIXED_CORE_KEYS;
use argonauts_core::settings::sessions::{FanoutSwitch, NativeFanout};
use argonauts_core::workflows::workflow::{Count, Step};

use crate::learned_workflows::{
    Change, ChangeClass, LoadedLearnedWorkflow, PROTECTED_KINDS, diff, guarded, key_matches,
    patterns_overlap, protected_counts, validate,
};

/// Config errors in `evolution.evolvable`: keys that are fixed core or safety keys.
pub fn check_evolvable(keys: &[String]) -> Vec<String> {
    let mut errors = Vec::new();
    for key in keys {
        if let Some(pattern) = FIXED_CORE_KEYS
            .iter()
            .find(|pattern| patterns_overlap(pattern, key))
        {
            errors.push(format!(
                "`evolution.evolvable` lists `{key}`, which reaches the fixed core (`{pattern}`); \
                 evolution never changes what scores it"
            ));
        } else if let Some(pattern) = SAFETY_KEYS
            .iter()
            .find(|pattern| patterns_overlap(pattern, key))
        {
            errors.push(format!(
                "`evolution.evolvable` lists `{key}`, which reaches the safety key `{pattern}`; \
                 only you change those"
            ));
        }
    }
    errors
}

/// The mutation rules: exactly one change, on the evolvable list, nothing from the fixed core,
/// no check, reviewer, judge, or approval removed or weakened, no limit above your ceilings.
pub fn check_mutation(
    parent: &LoadedLearnedWorkflow,
    child: &LoadedLearnedWorkflow,
    settings: &Settings,
) -> std::result::Result<Change, String> {
    let (old, new) = (&parent.learned_workflow, &child.learned_workflow);
    if new.name != old.name || new.parent != Some(old.version) {
        return Err(format!(
            "{} v{} doesn't name {} v{} as its parent",
            new.name, new.version, old.name, old.version
        ));
    }
    let problems = validate(new);
    if !problems.is_empty() {
        return Err(format!(
            "the new version breaks a rule for learned workflows: {}",
            problems.join("; ")
        ));
    }
    let changes = diff(parent, child).map_err(|error| error.to_string())?;
    let change = match changes.as_slice() {
        [] => return Err("it changes nothing".to_owned()),
        [one] => one.clone(),
        many => {
            let mut keys: Vec<&str> = many.iter().map(|change| change.key.as_str()).collect();
            keys.dedup();
            return Err(format!(
                "it changes {} things ({}), and a candidate changes exactly one",
                many.len(),
                keys.join(", ")
            ));
        }
    };
    if let Some(reason) = guarded(&change.key) {
        return Err(reason);
    }
    let evolvable = &settings.evolution.evolvable;
    if !check_evolvable(evolvable).is_empty() {
        return Err(
            "`evolution.evolvable` lists a fixed-core or safety key, so nothing evolves until \
             that's fixed"
                .to_owned(),
        );
    }
    if !evolvable
        .iter()
        .any(|pattern| key_matches(pattern, &change.key))
    {
        return Err(format!(
            "`{}` isn't on your `evolution.evolvable` list",
            change.key
        ));
    }
    if let Some(reason) = weakens(&change)
        .or_else(|| over_ceiling(&change, settings))
        .or_else(|| mixes_providers(&change))
    {
        return Err(reason);
    }
    Ok(change)
}

/// Removing or weakening a check, reviewer, judge, or approval step. Adding one is fine.
fn weakens(change: &Change) -> Option<String> {
    if change.class != ChangeClass::StepGraph {
        return None;
    }
    let by_name = |value: &Option<Value>| matches!(value, Some(Value::String(_)));
    if by_name(&change.before) || by_name(&change.after) {
        return Some(
            "it swaps the whole workflow by name, so its steps can't be checked for a removed \
             check or reviewer"
                .to_owned(),
        );
    }
    let before = protected_counts(change.before.as_ref());
    let after = protected_counts(change.after.as_ref());
    for ((kind, was), (_, now)) in before.iter().zip(&after) {
        if now < was {
            return Some(format!(
                "it removes a `{}` step; checks, reviewers, judges, and approvals can be added, \
                 never removed",
                kind_name(*kind)
            ));
        }
    }
    let (Some(old), Some(new)) = (&change.before, &change.after) else {
        return None;
    };
    let (Ok(old), Ok(new)) = (
        serde_json::from_value::<Step>(old.clone()),
        serde_json::from_value::<Step>(new.clone()),
    ) else {
        return None;
    };
    if old.kind != new.kind || !PROTECTED_KINDS.contains(&old.kind) {
        return None;
    }
    weakened_step(&old, &new).map(|what| format!("it {what} on a `{}` step", kind_name(old.kind)))
}

/// What a change to a protected step loosens, if anything. Only more of it is allowed.
fn weakened_step(old: &Step, new: &Step) -> Option<&'static str> {
    let fixed = |count: &Option<Count>| match count {
        None => Some(1),
        Some(Count::Fixed(n)) => Some(*n),
        Some(Count::Param(_)) => None,
    };
    if old.count != new.count {
        match (fixed(&old.count), fixed(&new.count)) {
            (Some(was), Some(now)) if now >= was => {}
            _ => return Some("lowers the count"),
        }
    }
    if old.blind && !new.blind {
        return Some("turns off blindness");
    }
    if old.different_provider_from.is_some()
        && old.different_provider_from != new.different_provider_from
    {
        return Some("drops the other-provider rule");
    }
    if old.role != new.role || old.roles != new.roles || old.models != new.models {
        return Some("replaces who does it");
    }
    if new.when.is_some() && old.when != new.when {
        return Some("makes it conditional");
    }
    if old.task != new.task {
        return Some("changes what it's asked");
    }
    if old.input != new.input {
        return Some("changes what it checks");
    }
    if old.approval != new.approval || old.workflow != new.workflow || old.params != new.params {
        return Some("changes what it runs");
    }
    None
}

fn kind_name(kind: argonauts_core::workflows::workflow::StepKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{kind:?}"))
}

/// A per-task limit or fan-out cap above the ceilings you set.
fn over_ceiling(change: &Change, settings: &Settings) -> Option<String> {
    if change.class != ChangeClass::TaskLimit {
        return None;
    }
    let after = change.after.as_ref()?;
    let key = change.key.as_str();
    let limit = &settings.limits.spend;
    let ceilings: Vec<(&str, Option<f64>)> = match key {
        "limits.per_task_usd" => vec![
            ("limits.spend_per_run", limit.spend_per_run),
            ("limits.spend_per_day", settings.limits.spend_per_day),
        ],
        "limits.per_task_tokens" => {
            vec![("limits.run_tokens", limit.run_tokens.map(|t| t as f64))]
        }
        "limits.per_task_plan_usage" => vec![("limits.run_plan_usage", limit.run_plan_usage)],
        "limits.per_task_minutes" => vec![("limits.run_minutes", limit.run_minutes)],
        _ => vec![("native_fanout", fanout_ceiling(settings.native_fanout))],
    };
    match after {
        Value::Number(number) => {
            let value = number.as_f64()?;
            if value <= 0.0 && key.starts_with("limits.") {
                return Some(format!(
                    "it sets `{key}` to {value}, and a limit must be above 0"
                ));
            }
            ceilings.into_iter().find_map(|(ceiling, limit)| {
                let limit = limit?;
                (value > limit).then(|| {
                    format!(
                        "it raises `{key}` to {value}, above your ceiling `{ceiling}` of {limit}"
                    )
                })
            })
        }
        Value::String(switch) if switch == "off" => None,
        _ if key.starts_with("limits.") => Some(format!("`{key}` must be a number, not {after}")),
        _ => ceilings
            .into_iter()
            .find(|(_, limit)| limit.is_some())
            .map(|(ceiling, _)| format!("it lifts `{key}` past your cap `{ceiling}`")),
    }
}

/// The most subagents your settings allow a task, or `None` when the CLI decides.
fn fanout_ceiling(fanout: NativeFanout) -> Option<f64> {
    match fanout {
        NativeFanout::Limit { max } => Some(f64::from(max)),
        NativeFanout::Switch(FanoutSwitch::Off) => Some(0.0),
        NativeFanout::Switch(FanoutSwitch::On) => None,
    }
}

/// A crew whose members share a model provider, or one that loses its second provider.
fn mixes_providers(change: &Change) -> Option<String> {
    if change.class != ChangeClass::CrewComposition {
        return None;
    }
    let providers = |value: &Option<Value>| -> Option<Vec<String>> {
        match value {
            Some(Value::Array(members)) => members
                .iter()
                .map(|member| member.as_str().map(provider_of))
                .collect(),
            None => Some(Vec::new()),
            _ => None,
        }
    };
    let Some(after) = providers(&change.after) else {
        return Some("`crew` must be a list of workers".to_owned());
    };
    for (i, provider) in after.iter().enumerate() {
        if after[..i].contains(provider) {
            return Some(format!(
                "it puts two agents from `{provider}` in the crew; crew members keep \
                 distinct providers"
            ));
        }
    }
    let before = providers(&change.before).unwrap_or_default();
    let distinct = |providers: &[String]| {
        let mut unique: Vec<&String> = providers.iter().collect();
        unique.sort();
        unique.dedup();
        unique.len()
    };
    if distinct(&before) >= 2 && distinct(&after) < 2 {
        return Some(
            "it leaves the crew a single provider, so nobody can review across providers"
                .to_owned(),
        );
    }
    None
}

/// A worker's model provider without the registry: the agent stands for its maker, and an
/// endpoint's model id such as `openrouter/z-ai/glm-5` names its maker after the endpoint. The
/// engine checks providers with the registry again when the run starts.
fn provider_of(worker: &str) -> String {
    match worker.split_once('/') {
        Some((_, model)) if model.contains('/') => {
            model.split('/').next().unwrap_or(model).to_owned()
        }
        Some((agent, _)) => agent.to_owned(),
        None => worker.to_owned(),
    }
}
