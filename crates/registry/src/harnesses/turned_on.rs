//! Built-in profiles that stay off until you turn them on with `agents.<id>.enabled = true`.

use crate::ResolvedSettings;

/// A missing `[agents.<id>]` table, or one without `enabled`, leaves these off.
const OFF_BY_DEFAULT: &[&str] = &["hermes"];

/// The agents whose `enabled = true` a settings layer set, rather than the built-in default.
/// `harness_manifests` and `builtin_harnesses` take this list.
pub fn agents_turned_on(resolved: &ResolvedSettings) -> Vec<String> {
    resolved
        .entries
        .iter()
        .filter(|entry| entry.layer != "built-in" && entry.value == "true")
        .filter_map(|entry| enabled_agent(&entry.key))
        .map(str::to_owned)
        .collect()
}

/// The `<id>` of an `agents.<id>.enabled` key.
pub(crate) fn enabled_agent(key: &str) -> Option<&str> {
    let id = key.strip_prefix("agents.")?.strip_suffix(".enabled")?;
    (!id.is_empty()).then_some(id)
}

/// Whether the built-in profile `id` is available.
pub(crate) fn builtin_counts(id: &str, turned_on: &[String]) -> bool {
    !OFF_BY_DEFAULT.contains(&id) || turned_on.iter().any(|on| on == id)
}
