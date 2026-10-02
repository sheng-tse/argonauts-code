//! `/handoff` by name, as `/permissions` lists its keys: what happens when an agent stops, for
//! every agent, then a row per role whose own settings win for its agents.

use argonauts_core::app::SettingRow;
use argonauts_core::display_names::builtin_harness_name;

use super::flow::{RowAction, Stage};
use super::words::{layer_words, parse_role, role_words, row_value_words};
use crate::bottom::{Picker, PickerRow};

/// What a handoff key decides, as its row says it: `When an agent stops` for `handoff.mode`, or
/// `Who replaces Codex` for `handoff.replace.codex.with`. `None` for a key it doesn't know.
pub fn label(key: &str) -> Option<String> {
    let rest = handoff_part(key)?;
    if let Some((vendor, leaf)) = rest
        .strip_prefix("replace.")
        .and_then(|rest| rest.split_once('.'))
    {
        let vendor = builtin_harness_name(vendor).unwrap_or(vendor);
        return Some(match leaf {
            "with" => format!("Who replaces {vendor}"),
            "strategy" => format!("How {vendor}'s cards are shared"),
            "on_return" => format!("When {vendor} comes back"),
            "context" => format!("What {vendor}'s replacement is told"),
            _ => return None,
        });
    }
    let name = match rest {
        "mode" => "When an agent stops",
        "fallback" => "Who takes over",
        "ask_timeout" => "How long the card waits",
        "allow_api_billing" => "Hand off to a paid API",
        "return_to_original" => "Hand back when it returns",
        "broken_work" => "Broken work in progress",
        "strategy" => "How the cards are shared",
        "race_count" => "Attempts in a race",
        "on_return" => "When the agent comes back",
        "checkpoint" => "Tag the card at each handoff",
        "merge_rule" => "Weaker model's work",
        "unknown_tier" => "A model with no tier counts as",
        "max_handoffs" => "Handoffs before it asks you",
        "context" => "What the replacement is told",
        "summarizer" => "Who summarizes the old session",
        _ => return None,
    };
    Some(name.to_owned())
}

/// `mode` for `handoff.mode` and `roles.owner.handoff.mode`.
fn handoff_part(key: &str) -> Option<&str> {
    key.strip_prefix("handoff.").or_else(|| {
        let (_, rest) = key.strip_prefix("roles.")?.split_once('.')?;
        rest.strip_prefix("handoff.")
    })
}

/// `owner` for `roles.owner.handoff.mode`.
fn role_of(key: &str) -> Option<&str> {
    let (role, rest) = key.strip_prefix("roles.")?.split_once('.')?;
    rest.starts_with("handoff.").then_some(role)
}

/// `/handoff`: a row per key for every agent, then a row per role that opens its own. Rows for
/// one role alone, as `/handoff owner` asks for, open that role at once.
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
        let prefix = format!("roles.{role}.handoff");
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
        let prefix = format!("roles.{role}.handoff");
        rows.push(PickerRow::new(
            role_name(role),
            role_note(settings, &prefix),
        ));
        actions.push(RowAction::OpenGroup(prefix));
    }
    picker.title = "Handoff".to_owned();
    show(picker, rows, actions);
}

/// One role's keys under `prefix`, such as `roles.owner.handoff`. A key the role leaves unset
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
    let role = prefix
        .strip_prefix("roles.")
        .and_then(|rest| rest.split('.').next())
        .unwrap_or_default();
    picker.title = format!("Handoff for {}", role_name(role));
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

fn role_name(role: &str) -> String {
    match parse_role(role) {
        Some(role) => role_words(Some(role)),
        None => role.to_owned(),
    }
}

/// A value as a handoff row says it: `yes` for `true`, `hand back` for `hand_back`.
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
    let Some(rest) = handoff_part(key) else {
        return "unset".to_owned();
    };
    let every = format!("handoff.{rest}");
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
            label("handoff.mode").as_deref(),
            Some("When an agent stops")
        );
        assert_eq!(
            label("roles.owner.handoff.context").as_deref(),
            Some("What the replacement is told")
        );
        assert_eq!(
            label("handoff.replace.codex.with").as_deref(),
            Some("Who replaces Codex")
        );
        assert_eq!(label("handoff.unheard_of"), None);
        assert_eq!(label("sandbox.mode"), None);
    }

    #[test]
    fn only_handoff_keys_under_a_role_have_one() {
        assert_eq!(role_of("roles.owner.handoff.mode"), Some("owner"));
        assert_eq!(role_of("roles.owner.sandbox.mode"), None);
        assert_eq!(role_of("handoff.mode"), None);
    }
}
