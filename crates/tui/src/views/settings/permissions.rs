//! `/permissions` in plain words: whose rules guard the agents, their internet, whether they
//! see each other's copies and make their own commits, the rest of the sandbox, and the
//! limits, for every agent; then each role, whose own keys win for its agents.

use argonauts_core::app::SettingRow;

use super::flow::{RowAction, Stage};
use super::words::{layer_words, parse_role, role_words, value_words};
use crate::bottom::{Picker, PickerRow};

/// What a `/permissions` key decides, as its row says it. `None` for a key it doesn't list.
pub fn label(key: &str) -> Option<&'static str> {
    if let Some(leaf) = sandbox_leaf(key) {
        return Some(match leaf {
            "mode" => "Whose rules guard the agents",
            "network" => "Internet for agents' commands",
            "agents_see_other_worktrees" => "Agents see each other's worktrees",
            "agents_commit" => "Agents make their own commits",
            "fail_closed" => "Refuse to start without a sandbox",
            "extra_writable" => "Other folders agents may write",
            "proxy_allow" => "More hosts agents may reach",
            "untrusted_repos" => "Repos you haven't trusted",
            "container_image" => "Container image",
            "user_hooks" => "Your own hooks in agents' sessions",
            "isolated_leak" => "A blind role's session that loads your plugins",
            _ => return None,
        });
    }
    Some(match key {
        "limits.protected_paths" => "Protected paths",
        "limits.dependency_allowlist" => "Packages a plan may add",
        "limits.spend_per_day" => "Daily spend cap",
        _ => return None,
    })
}

/// A value as `/permissions` says it, such as `Argonauts' (OS sandbox)` for `sandbox.mode =
/// "os"`. Values without words of their own read as they do everywhere else.
pub fn value_in_words(key: &str, toml: &str) -> String {
    let words = value_words(toml);
    let plain = match (sandbox_leaf(key), words.as_str()) {
        // An empty image refuses container sessions.
        (Some(_), "") => "none",
        (Some("mode"), "os") => "Argonauts' (OS sandbox)",
        (Some("mode"), "container") => "Argonauts' (container)",
        (Some("mode"), "tool") => "each AI tool's own",
        (Some("mode"), "none") => "none",
        (Some("network"), "true") => "on",
        (Some("network"), "false") => "off",
        (Some(_), "true") => "yes",
        (Some(_), "false") => "no",
        _ => return words,
    };
    plain.to_owned()
}

/// One line on what a sandbox value does, for its row among the values.
pub fn value_note(leaf: &str, value: &str) -> &'static str {
    match (leaf, value) {
        ("mode", "os") => "Argonauts' rules, kept by your OS's own sandbox",
        ("mode", "container") => {
            "Argonauts' rules, in a container that holds only the agent's worktree"
        }
        ("mode", "tool") => "Each AI tool's own sandbox and approvals, with Argonauts' off",
        ("mode", "none") => "No protection at all, neither Argonauts' nor the tool's",
        ("network", "true") => "Commands agents run can reach the internet",
        ("network", "false") => "Only each AI tool's own service and the hosts you allow",
        ("agents_see_other_worktrees", "true") => {
            "An agent can read every agent's worktree of the repo"
        }
        ("agents_see_other_worktrees", "false") => "Each agent sees only its own worktree",
        ("agents_commit", "true") => "Agents commit their own work",
        ("agents_commit", "false") => {
            "Git stays read-only to agents, and Argonauts commits for them"
        }
        ("fail_closed", "true") => "A session that can't be sandboxed doesn't start",
        ("fail_closed", "false") => "It starts anyway, unguarded",
        ("isolated_leak", "stop") => {
            "Stopped before it acts, and an agent that keeps your plugins out takes the role"
        }
        ("isolated_leak", "warn") => "It runs on, and a notice says what it loaded",
        _ => "",
    }
}

/// `mode` for `sandbox.mode` and for `roles.owner.sandbox.mode`.
pub fn sandbox_leaf(key: &str) -> Option<&str> {
    if let Some(leaf) = key.strip_prefix("sandbox.") {
        return Some(leaf);
    }
    let (_, rest) = key.strip_prefix("roles.")?.split_once('.')?;
    rest.strip_prefix("sandbox.")
}

/// `owner` for `roles.owner.sandbox.mode`.
fn role_of(key: &str) -> Option<&str> {
    let (role, rest) = key.strip_prefix("roles.")?.split_once('.')?;
    rest.starts_with("sandbox.").then_some(role)
}

/// `/permissions`: a row per key for every agent, then a row per role that opens its own.
/// Rows for one role alone open that role at once, with every agent's values beside them.
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
    if let [role] = roles.as_slice() {
        let prefix = format!("roles.{role}.sandbox");
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
        rows.push(PickerRow::new(
            role_name(role),
            role_note(settings, &format!("roles.{role}.sandbox")),
        ));
        actions.push(RowAction::OpenGroup(format!("roles.{role}.sandbox")));
    }
    picker.title = "Permissions".to_owned();
    picker.about =
        "For every agent, unless its role sets its own. Pick one, then where to save it".to_owned();
    show(picker, rows, actions);
}

/// One role's keys under `prefix`, such as `roles.owner.sandbox`. A key the role leaves unset
/// says what every agent has.
pub fn role_list(picker: &mut Picker, prefix: &str) {
    let settings = &picker.flow.settings;
    let mut rows = Vec::new();
    let mut actions = Vec::new();
    for (index, row) in settings.iter().enumerate() {
        let under = row
            .key
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('.'));
        if under {
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
    picker.title = format!("Permissions for {}", role_name(role));
    picker.about =
        "What this role sets wins for its agents; the rest follows every agent's".to_owned();
    show(picker, rows, actions);
}

/// A value's title and what it does, once a `/permissions` key is open.
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

fn row_label(key: &str) -> String {
    label(key).map_or_else(|| key.to_owned(), str::to_owned)
}

/// `Default agent` for `default`, else the role's own name.
fn role_name(role: &str) -> String {
    match parse_role(role) {
        Some(role) => role_words(Some(role)),
        None => role.to_owned(),
    }
}

/// `Argonauts' (OS sandbox) · built-in`: the value, then where it comes from.
fn every_agent_note(row: &SettingRow) -> String {
    format!(
        "{} · {}",
        value_in_words(&row.key, &row.value),
        layer_words(&row.layer)
    )
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
    let Some(leaf) = sandbox_leaf(key) else {
        return "unset".to_owned();
    };
    let every = format!("sandbox.{leaf}");
    settings.iter().find(|row| row.key == every).map_or_else(
        || "unset".to_owned(),
        |row| value_in_words(&every, &row.value),
    )
}

/// `same as every agent`, or what the role sets for itself, such as `Its own: rules each AI
/// tool's own · internet off`.
fn role_note(settings: &[SettingRow], prefix: &str) -> String {
    let own: Vec<String> = settings
        .iter()
        .filter(|row| {
            row.key
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('.'))
                && !row.value.trim().is_empty()
        })
        .map(|row| {
            let value = value_in_words(&row.key, &row.value);
            let yes = value == "yes";
            match sandbox_leaf(&row.key) {
                Some("mode") => format!("rules {value}"),
                Some("network") => format!("internet {value}"),
                Some("agents_see_other_worktrees") if yes => "sees other worktrees".to_owned(),
                Some("agents_see_other_worktrees") => "sees only its copy".to_owned(),
                Some("agents_commit") if yes => "commits its work".to_owned(),
                Some("agents_commit") => "Argonauts commits".to_owned(),
                _ => value,
            }
        })
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
    fn keys_and_values_read_in_plain_words() {
        assert_eq!(label("sandbox.mode"), Some("Whose rules guard the agents"));
        assert_eq!(
            label("roles.worker.sandbox.agents_commit"),
            Some("Agents make their own commits")
        );
        assert_eq!(label("limits.spend_per_day"), Some("Daily spend cap"));
        assert_eq!(label("limits.auto_merges_per_day"), None);
        assert_eq!(label("fallback.workers.mode"), None);
        assert_eq!(
            value_in_words("sandbox.mode", "\"os\""),
            "Argonauts' (OS sandbox)"
        );
        assert_eq!(
            value_in_words("roles.worker.sandbox.mode", "\"tool\""),
            "each AI tool's own"
        );
        assert_eq!(value_in_words("sandbox.network", "true"), "on");
        assert_eq!(value_in_words("sandbox.agents_commit", "false"), "no");
        assert_eq!(value_in_words("limits.spend_per_day", "25.0"), "25.0");
        for value in ["os", "container", "tool", "none"] {
            assert!(!value_note("mode", value).is_empty(), "{value}");
        }
    }

    #[test]
    fn only_sandbox_keys_have_a_role() {
        assert_eq!(role_of("roles.worker.sandbox.mode"), Some("worker"));
        assert_eq!(role_of("fallback.workers.mode"), None);
        assert_eq!(role_of("sandbox.mode"), None);
        assert_eq!(
            sandbox_leaf("roles.phase-lead.sandbox.network"),
            Some("network")
        );
        assert_eq!(sandbox_leaf("fallback.workers.mode"), None);
    }
}
