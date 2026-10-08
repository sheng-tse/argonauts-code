//! Dotted settings keys: flattening a recipe's settings into them, matching them against the
//! safety and fixed-core patterns, and the change class each one falls in.

use std::collections::BTreeMap;

use serde_json::Value;

use argonauts_core::settings::SAFETY_KEYS;
use argonauts_core::settings::evolution::FIXED_CORE_KEYS;

use super::ChangeClass;

/// Whether a dotted `key` falls under `pattern`. A trailing `.*` covers the key itself and
/// everything below it, so a whole section set at once counts too; a `*` elsewhere matches one
/// segment. The same rule the settings resolver applies.
pub fn key_matches(pattern: &str, key: &str) -> bool {
    let (pattern, subtree) = match pattern.strip_suffix(".*") {
        Some(prefix) => (prefix, true),
        None => (pattern, false),
    };
    let parts: Vec<&str> = pattern.split('.').collect();
    let segments: Vec<&str> = key.split('.').collect();
    let length_fits = if subtree {
        segments.len() >= parts.len()
    } else {
        segments.len() == parts.len()
    };
    length_fits
        && parts
            .iter()
            .zip(&segments)
            .all(|(part, segment)| *part == "*" || part == segment)
}

/// Whether two patterns could name the same key, such as `critique.*` and `critique.rounds`,
/// or `*` and anything. An `evolvable` entry may itself be a pattern.
pub fn patterns_overlap(a: &str, b: &str) -> bool {
    let split = |pattern: &str| -> (Vec<String>, bool) {
        match pattern.strip_suffix(".*") {
            Some(prefix) => (prefix.split('.').map(str::to_owned).collect(), true),
            None => (pattern.split('.').map(str::to_owned).collect(), false),
        }
    };
    let (a_parts, a_subtree) = split(a);
    let (b_parts, b_subtree) = split(b);
    let shared = a_parts.len().min(b_parts.len());
    let prefix_fits = a_parts[..shared]
        .iter()
        .zip(&b_parts[..shared])
        .all(|(x, y)| x == "*" || y == "*" || x == y);
    let length_fits = match a_parts.len().cmp(&b_parts.len()) {
        std::cmp::Ordering::Equal => true,
        std::cmp::Ordering::Less => a_subtree,
        std::cmp::Ordering::Greater => b_subtree,
    };
    prefix_fits && length_fits
}

pub fn is_safety_key(key: &str) -> bool {
    SAFETY_KEYS.iter().any(|pattern| key_matches(pattern, key))
}

pub fn is_fixed_core_key(key: &str) -> bool {
    FIXED_CORE_KEYS
        .iter()
        .any(|pattern| key_matches(pattern, key))
}

/// Why `key` is out of evolution's reach, if it is.
pub fn guarded(key: &str) -> Option<String> {
    if is_fixed_core_key(key) {
        Some(format!(
            "`{key}` is in the fixed core, which scores and constrains evolution, so nothing \
             but you changes it"
        ))
    } else if is_safety_key(key) {
        Some(format!("`{key}` is a safety key, which only you set"))
    } else {
        None
    }
}

/// Nested tables become dotted keys; lists and scalars are leaves. An empty table stays a leaf,
/// so `limits: {}` still names `limits`.
pub fn flatten(settings: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    for (key, value) in settings {
        flatten_into(key, value, &mut out);
    }
    out
}

fn flatten_into(prefix: &str, value: &Value, out: &mut BTreeMap<String, Value>) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (key, value) in map {
                flatten_into(&format!("{prefix}.{key}"), value, out);
            }
        }
        _ => {
            out.insert(prefix.to_owned(), value.clone());
        }
    }
}

/// The checklist is compared item by item: adding one item is one change.
pub const CHECKLIST_KEY: &str = "critique.checklist";
/// The key a step-graph change is filed under.
pub const WORKFLOW_KEY: &str = "workflow";

const FANOUT_PATTERNS: &[&str] = &["native_fanout.*", "roles.*.native_fanout.*"];

/// The change class a key falls in, as the promotion rules name them.
pub fn class_for_key(key: &str) -> ChangeClass {
    match key {
        CHECKLIST_KEY => ChangeClass::ChecklistItem,
        "critique.rounds" => ChangeClass::CritiqueRounds,
        "critique.critic" => ChangeClass::CriticPolicy,
        "prompts.drafting" => ChangeClass::DraftingPrompt,
        "prompts.build" => ChangeClass::BuildPrompt,
        WORKFLOW_KEY => ChangeClass::StepGraph,
        _ if key_matches("divide.*", key) => ChangeClass::TaskSplit,
        _ if key_matches("crew.*", key) => ChangeClass::CrewComposition,
        _ if key.starts_with("limits.per_task_")
            || FANOUT_PATTERNS
                .iter()
                .any(|pattern| key_matches(pattern, key)) =>
        {
            ChangeClass::TaskBudget
        }
        _ => ChangeClass::Setting,
    }
}

/// The class's name in settings, such as `checklist_item`.
pub fn class_name(class: ChangeClass) -> String {
    match serde_json::to_value(class) {
        Ok(Value::String(name)) => name,
        _ => format!("{class:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_match_like_the_resolver() {
        assert!(key_matches("limits.*", "limits"));
        assert!(key_matches("limits.*", "limits.spend_per_day"));
        assert!(key_matches("agents.*.auth", "agents.codex.auth"));
        assert!(!key_matches("agents.*.auth", "agents.codex.binary"));
        assert!(!key_matches("drafting.blind", "drafting.deadline_min"));
        assert!(key_matches("drafting.blind", "drafting.blind"));
    }

    #[test]
    fn overlapping_patterns() {
        assert!(patterns_overlap("critique.*", "critique.rounds"));
        assert!(patterns_overlap("judging", "judging.*"));
        assert!(patterns_overlap("*", "goal.*"));
        assert!(patterns_overlap("review.assign", "review.*"));
        assert!(!patterns_overlap("critique.rounds", "judging.*"));
        assert!(!patterns_overlap("drafting.deadline_min", "drafting.blind"));
    }

    #[test]
    fn classes_by_key() {
        assert_eq!(class_for_key("divide.max_tasks"), ChangeClass::TaskSplit);
        assert_eq!(
            class_for_key("limits.per_task_usd"),
            ChangeClass::TaskBudget
        );
        assert_eq!(
            class_for_key("roles.worker.native_fanout.max"),
            ChangeClass::TaskBudget
        );
        assert_eq!(class_for_key("crew"), ChangeClass::CrewComposition);
        assert_eq!(class_for_key("experiments.enabled"), ChangeClass::Setting);
        assert_eq!(class_name(ChangeClass::ChecklistItem), "checklist_item");
    }
}
