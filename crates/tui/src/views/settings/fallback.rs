//! `/fallback` by name, as `/permissions` lists its keys: what happens when an agent stops, for
//! every agent, then a row per role whose own settings win for its agents.

use argonauts_core::app::SettingRow;
use argonauts_core::display_names::builtin_cli_name;
use argonauts_core::settings::crew::CrewRole;

use super::flow::{RowAction, Stage};
use super::words::{layer_words, role_words, row_value_words};
use crate::bottom::{Picker, PickerRow};

/// What a fallback key decides, as its row says it: `When an agent stops` for `fallback.mode`, or
/// `Who replaces Codex` for `fallback.replace.codex.with`. `None` for a key it doesn't know.
pub fn label(key: &str) -> Option<String> {
    let rest = fallback_part(key)?;
    if let Some((agent_id, leaf)) = rest
        .strip_prefix("replace.")
        .and_then(|rest| rest.split_once('.'))
    {
        let cli = builtin_cli_name(agent_id).unwrap_or(agent_id);
        return Some(match leaf {
            "with" => format!("Who replaces {cli}"),
            "strategy" => format!("How {cli}'s tasks are shared"),
            "on_return" => format!("When {cli} comes back"),
            "context" => format!("What {cli}'s fallback is told"),
            _ => return None,
        });
    }
    let name = match rest {
        "mode" => "When an agent stops",
        "order" => "Who it falls back to",
        "ask_timeout" => "How long the question waits",
        "allow_api_billing" => "Fall back to a paid API",
        "broken_work" => "Broken work in progress",
        "wrap_up" => "An agent finishing on its allowance",
        "strategy" => "How the tasks are shared",
        "together" => "How many fallbacks at once",
        "pick" => "How their results combine",
        "on_return" => "When the agent comes back",
        "checkpoint" => "Tag the task at each fallback",
        "merge_rule" => "Weaker model's work",
        "unknown_tier" => "A model with no tier counts as",
        "max_fallbacks" => "Fallbacks before it asks you",
        "same_cli" => "Stay on the CLI that stopped",
        "retry.tries" => "Tries again on the same model",
        "retry.wait" => "Wait before it tries again",
        "retry.backoff" => "How that wait grows",
        "context" => "What the fallback is told",
        "summarizer" => "Who summarizes the old session",
        _ => return None,
    };
    Some(name.to_owned())
}

/// `mode` for `fallback.mode` and `fallback.worker.mode`.
fn fallback_part(key: &str) -> Option<&str> {
    let rest = key.strip_prefix("fallback.")?;
    Some(match role_of(key) {
        Some(role) => rest.strip_prefix(role)?.strip_prefix('.')?,
        None => rest,
    })
}

/// `worker` for `fallback.worker.mode`: one of the crew's five roles.
fn role_of(key: &str) -> Option<&str> {
    let (role, _) = key.strip_prefix("fallback.")?.split_once('.')?;
    CrewRole::parse(role).map(|_| role)
}

/// `/fallback`: a row per key for every agent, then a row per role that opens its own. Rows for
/// one role alone, as `/fallback worker` asks for, open that role at once.
pub fn list(picker: &mut Picker) {
    let settings = &picker.flow.settings;
    let mut roles: Vec<String> = Vec::new();
    for row in settings {
        if let Some(role) = role_of(&row.key)
            && !roles.iter().any(|known| known == role)
        {
            roles.push(role.to_owned());
        }
    }
    let every_agent = settings.iter().any(|row| role_of(&row.key).is_none());
    if let ([role], false) = (roles.as_slice(), every_agent) {
        let prefix = format!("fallback.{role}");
        role_list(picker, &prefix);
        return;
    }
    let mut rows = Vec::new();
    let mut actions = Vec::new();
    for (index, row) in settings.iter().enumerate() {
        if role_of(&row.key).is_none() {
            rows.push(PickerRow::new(row_label(&row.key), every_agent_note(row)));
            actions.push(RowAction::OpenKey(index));
        }
    }
    for role in &roles {
        let prefix = format!("fallback.{role}");
        rows.push(PickerRow::new(
            role_name(role),
            role_note(settings, &prefix),
        ));
        actions.push(RowAction::OpenGroup(prefix));
    }
    picker.title = "Fallback".to_owned();
    show(picker, rows, actions);
}

/// One role's keys under `prefix`, such as `fallback.worker`. A key the role leaves unset
/// says what every agent has.
pub fn role_list(picker: &mut Picker, prefix: &str) {
    let settings = &picker.flow.settings;
    let mut rows = Vec::new();
    let mut actions = Vec::new();
    for (index, row) in settings.iter().enumerate() {
        if under(&row.key, prefix) {
            rows.push(PickerRow::new(
                row_label(&row.key),
                role_key_note(settings, row),
            ));
            actions.push(RowAction::OpenKey(index));
        }
    }
    let role = prefix.strip_prefix("fallback.").unwrap_or_default();
    picker.title = format!("Fallback for {}", role_name(role));
    picker.about =
        "What this role sets wins for its agents; the rest follows every agent's".to_owned();
    show(picker, rows, actions);
}

/// A key's title and what it does, once it's open.
pub fn heading(settings: &[SettingRow], row: &SettingRow) -> (String, String) {
    let label = row_label(&row.key);
    let title = match role_of(&row.key) {
        Some(role) => format!("{label} · {}", role_name(role)),
        None => label,
    };
    let mut about = row.description.trim().trim_end_matches('.').to_owned();
    if role_of(&row.key).is_some() && row.value.trim().is_empty() {
        let every = inherited(settings, &row.key);
        about = format!("{about}. Unset, so the same as every agent: {every}");
    }
    (title, about)
}

fn show(picker: &mut Picker, rows: Vec<PickerRow>, actions: Vec<RowAction>) {
    picker.flow.stage = Stage::Keys;
    picker.enter_hint.clear();
    picker.set_rows(rows);
    picker.flow.actions = actions;
}

fn under(key: &str, prefix: &str) -> bool {
    key.strip_prefix(prefix)
        .is_some_and(|rest| rest.starts_with('.'))
}

fn row_label(key: &str) -> String {
    label(key).unwrap_or_else(|| key.to_owned())
}

/// A crew role as a row names it: `Main agent` as the other pickers do, else its people,
/// such as `workers`.
fn role_name(role: &str) -> String {
    match CrewRole::parse(role) {
        Some(CrewRole::Main) => role_words(None),
        Some(crew) => crew.plural().to_owned(),
        None => role.to_owned(),
    }
}

/// A value as a fallback row says it: `yes` for `true`, `give back` for `give_back`.
fn value_words(row: &SettingRow) -> String {
    let words = row_value_words(&row.key, &row.value);
    match words.as_str() {
        "true" => "yes".to_owned(),
        "false" => "no".to_owned(),
        _ if !words.contains(['/', '"', '{', '[']) => words.replace('_', " "),
        _ => words,
    }
}

/// `auto · built-in`: the value, then where it comes from.
fn every_agent_note(row: &SettingRow) -> String {
    format!("{} · {}", value_words(row), layer_words(&row.layer))
}

fn role_key_note(settings: &[SettingRow], row: &SettingRow) -> String {
    if row.value.trim().is_empty() {
        format!("same as every agent: {}", inherited(settings, &row.key))
    } else {
        every_agent_note(row)
    }
}

/// What every agent has for a role's key, from the top-level row the answer carries.
fn inherited(settings: &[SettingRow], key: &str) -> String {
    let Some(rest) = fallback_part(key) else {
        return "unset".to_owned();
    };
    let every = format!("fallback.{rest}");
    settings
        .iter()
        .find(|row| row.key == every)
        .map_or_else(|| "unset".to_owned(), value_words)
}

/// `same as every agent`, or what the role sets for itself, such as `Its own: When an agent
/// stops: ask`.
fn role_note(settings: &[SettingRow], prefix: &str) -> String {
    let own: Vec<String> = settings
        .iter()
        .filter(|row| under(&row.key, prefix) && !row.value.trim().is_empty())
        .map(|row| format!("{}: {}", row_label(&row.key), value_words(row)))
        .collect();
    if own.is_empty() {
        "same as every agent".to_owned()
    } else {
        format!("Its own: {}", own.join(" · "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_read_as_names_for_every_agent_and_each_role() {
        assert_eq!(
            label("fallback.mode").as_deref(),
            Some("When an agent stops")
        );
        assert_eq!(
            label("fallback.worker.context").as_deref(),
            Some("What the fallback is told")
        );
        assert_eq!(
            label("fallback.replace.codex.with").as_deref(),
            Some("Who replaces Codex")
        );
        assert_eq!(label("fallback.unheard_of"), None);
        assert_eq!(label("sandbox.mode"), None);
    }

    #[test]
    fn only_fallback_keys_under_a_role_have_one() {
        assert_eq!(role_of("fallback.worker.mode"), Some("worker"));
        assert_eq!(role_of("roles.worker.sandbox.mode"), None);
        assert_eq!(role_of("fallback.mode"), None);
        assert_eq!(role_of("fallback.replace.codex"), None);
        assert_eq!(fallback_part("fallback.worker.mode"), Some("mode"));
        assert_eq!(
            fallback_part("fallback.reviewer.replace.codex.with"),
            Some("replace.codex.with")
        );
        assert_eq!(fallback_part("fallback.mode"), Some("mode"));
    }
}
