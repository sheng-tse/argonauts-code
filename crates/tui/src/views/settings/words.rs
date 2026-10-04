//! How settings read on screen: layers and scopes in words, values without their TOML
//! quotes, and typed text turned back into TOML.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::settings::SettingScope;
use time::OffsetDateTime;

use crate::format::setting_value;
use crate::views::resume::ago;

/// The four answers to "where to save it", in order, with their files. They answer to their
/// numbers, as every card's options do.
pub const SCOPES: [(SettingScope, &str, &str); 4] = [
    (SettingScope::Session, "Just this session", "until you quit"),
    (
        SettingScope::ProjectLocal,
        "This project, just me",
        ".argonauts/config.local.toml",
    ),
    (
        SettingScope::Project,
        "This project, shared",
        ".argonauts/config.toml",
    ),
    (
        SettingScope::Global,
        "Everywhere",
        "~/.argonauts/config.toml",
    ),
];

/// The layer a value came from, as `/config` names it. The file layers keep their own name
/// beside the words, so it's clear which file to open.
pub fn layer_words(layer: &str) -> String {
    match layer {
        "built-in" => "built-in".to_owned(),
        "global" => "everywhere (global)".to_owned(),
        "project" => "this project".to_owned(),
        "project-local" => "just you (project-local)".to_owned(),
        "preset" => "preset".to_owned(),
        "flag" => "--set".to_owned(),
        "session" => "this session".to_owned(),
        other => other.to_owned(),
    }
}

/// The scope a Reset drops the key from: a settings file or this session. Built-in values,
/// the preset, and `--set` flags can't be reset in the app.
pub fn reset_scope(layer: &str) -> Option<SettingScope> {
    match layer {
        "global" => Some(SettingScope::Global),
        "project" => Some(SettingScope::Project),
        "project-local" => Some(SettingScope::ProjectLocal),
        "session" => Some(SettingScope::Session),
        _ => None,
    }
}

/// Where a Reset takes the value from, for the Reset row.
pub fn reset_note(scope: SettingScope) -> &'static str {
    match scope {
        SettingScope::Session => "drop this session's value",
        SettingScope::ProjectLocal => "remove it from your own project file",
        SettingScope::Project => "remove it from this project's file",
        SettingScope::Global => "remove it from your global file",
    }
}

/// The scopes that save to a file.
const FILE_SCOPES: [SettingScope; 3] = [
    SettingScope::ProjectLocal,
    SettingScope::Project,
    SettingScope::Global,
];

/// Said once over the scopes when `layer` wins over every file, so no row repeats it: a
/// preset, a `--set` flag, or this session's own value. A preset picked in a file
/// (`preset_from`) gives way to that file, so it never wins over every one.
pub fn hidden_from_files(layer: &str, preset_from: Option<&str>) -> Option<String> {
    if !FILE_SCOPES
        .iter()
        .all(|scope| scope.hidden_by_value(layer, preset_from))
    {
        return None;
    }
    Some(match layer {
        "preset" => "The preset sets this, so saving it in a file won't change it. Use /preset \
                     copy to edit the preset, or save it for this session."
            .to_owned(),
        "flag" => "A --set flag sets this until you quit, so saving it in a file won't show \
                   before then."
            .to_owned(),
        "session" => "This session's value wins until you quit, so saving it in a file won't \
                      show before then."
            .to_owned(),
        other => format!("The {other} layer sets this, so saving it in a file won't change it."),
    })
}

/// How fresh the model lists are, for the line under the model picker's title.
pub fn freshness(
    refreshed: Option<OffsetDateTime>,
    refreshing: bool,
    now: OffsetDateTime,
) -> Option<String> {
    match (refreshed, refreshing) {
        (Some(at), false) => Some(format!("Model lists refreshed {}.", ago(at, now))),
        (Some(at), true) => Some(format!(
            "Model lists refreshed {}; checking for newer ones.",
            ago(at, now)
        )),
        (None, true) => Some("Checking each CLI's model list.".to_owned()),
        (None, false) => None,
    }
}

/// Why saving in `scope` won't show while `layer` sets the key, and what to do instead.
pub fn hidden_note(layer: &str, preset_from: Option<&str>, scope: SettingScope) -> String {
    let saving = match scope {
        SettingScope::Session => "for this session",
        SettingScope::ProjectLocal => "for you in this project",
        SettingScope::Project => "for this project",
        SettingScope::Global => "everywhere",
    };
    match layer {
        "project" => format!(
            "This project's file sets this, so saving it {saving} won't change this repo. \
             Save it for this project instead."
        ),
        "project-local" => format!(
            "Your own project file sets this, so saving it {saving} won't change this repo. \
             Pick This project, just me instead."
        ),
        "preset" if preset_from.is_some() => format!(
            "The preset sets this, and it's picked in a later file, so saving it {saving} won't \
             change it. Save it where you picked the preset, or later."
        ),
        "preset" => format!(
            "The preset sets this, so saving it {saving} won't change it. \
             Use /preset copy to edit the preset, or save it for this session."
        ),
        "flag" => format!(
            "A --set flag sets this until you quit, so saving it {saving} won't show before \
             then. Save it for this session instead."
        ),
        "session" => format!(
            "This session's value wins until you quit, so saving it {saving} won't show before \
             then. Save it for this session instead."
        ),
        other => format!("The {other} layer sets this, so saving it {saving} won't change it."),
    }
}

/// A value as it reads in a row: plain strings lose their quotes, lists of names their
/// brackets, and an unset value says so.
pub fn value_words(toml: &str) -> String {
    let trimmed = toml.trim();
    if trimmed.is_empty() {
        return "unset".to_owned();
    }
    if let Some(inner) = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        let items: Vec<String> = inner
            .split(',')
            .map(setting_value)
            .filter(|item| !item.is_empty())
            .collect();
        if items.iter().all(|item| !item.contains(['"', '{', '['])) {
            return if items.is_empty() {
                "none".to_owned()
            } else {
                items.join(", ")
            };
        }
    }
    setting_value(trimmed)
}

/// A model key's value with each CLI by its name: `opus on Claude Code`, or `Claude Code` for a
/// CLI that runs its own default.
pub fn workers_words(toml: &str) -> String {
    let words = value_words(toml);
    if matches!(words.as_str(), "unset" | "none") {
        return words;
    }
    words
        .split(", ")
        .map(worker_words)
        .collect::<Vec<_>>()
        .join(", ")
}

/// One worker, `claude-code/claude-opus-5-5`, as `Opus 5.5 on Claude Code`.
pub fn worker_words(worker: &str) -> String {
    match worker.parse::<WorkerRef>() {
        Ok(worker) => DisplayNames::default().worker(&worker),
        Err(_) => worker.to_owned(),
    }
}

/// A row's value in words. An empty one says what it stands for where unset means something,
/// such as the approver picked for each piece of work.
pub fn row_value_words(key: &str, toml: &str) -> String {
    let words = value_words(toml);
    match default_meaning(key) {
        Some(meaning) if matches!(words.as_str(), "unset" | "none") => meaning.to_owned(),
        _ => words,
    }
}

/// What an unset or empty value means for the keys whose default stands for something.
fn default_meaning(key: &str) -> Option<&'static str> {
    let segments: Vec<&str> = key.split('.').collect();
    Some(match segments.as_slice() {
        ["approvals", "approver", "model"] => "cheapest, other company",
        ["approvals", "answered_by"] => "the lead in auto, else you",
        ["handoff", "fallback"] | ["roles", _, "handoff", "fallback"] => "other logged-in CLIs",
        ["roles", _, "sandbox", _] => "same as every agent",
        _ => return None,
    })
}

/// Whether two TOML texts hold the same value, however each was quoted.
pub fn same_value(left: &str, right: &str) -> bool {
    left.trim() == right.trim() || setting_value(left) == setting_value(right)
}

/// Typed text as TOML. A key whose value is a string gets a string unless you typed the
/// quotes; otherwise numbers, booleans, lists, and tables go as typed, and anything else
/// becomes a string.
pub fn toml_text(typed: &str, current: &str) -> String {
    let text = typed.trim();
    let quoted = text.starts_with('"') || text.starts_with('\'');
    if quoted || text.starts_with('[') || text.starts_with('{') {
        return text.to_owned();
    }
    let string_key =
        current.trim_start().starts_with('"') || current.trim_start().starts_with('\'');
    let literal = matches!(text, "true" | "false")
        || text.parse::<i64>().is_ok()
        || text.parse::<f64>().is_ok_and(f64::is_finite);
    if literal && !string_key {
        return text.to_owned();
    }
    quote(text)
}

pub fn quote(text: &str) -> String {
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// A role as rows name it: the default agent, or the role's own name.
pub fn role_words(role: Option<RoleName>) -> String {
    match role {
        None | Some(RoleName::Default) => "Default agent".to_owned(),
        Some(role) => role.as_str().to_owned(),
    }
}

/// The role a key picks workers for, as `/model` lists them. `None` for a key that isn't one.
pub fn model_role(key: &str) -> Option<Option<RoleName>> {
    match key {
        "default_agent" => Some(None),
        "narrator.model" => Some(Some(RoleName::Narrator)),
        "approvals.approver.model" => Some(Some(RoleName::Approver)),
        _ => {
            let role = key.strip_prefix("roles.")?.strip_suffix(".workers")?;
            let role = parse_role(role)?;
            Some((role != RoleName::Default).then_some(role))
        }
    }
}

pub fn parse_role(name: &str) -> Option<RoleName> {
    serde_json::from_value(serde_json::Value::String(name.to_owned())).ok()
}

/// One line on what a value does, for the keys whose values need one.
pub fn value_note(key: &str, value: &str) -> &'static str {
    let segments: Vec<&str> = key.split('.').collect();
    match segments.as_slice() {
        ["preset"] => preset_note(value),
        ["workflows", "style"] => style_note(value),
        ["handoff", .., "context"] | ["roles", _, "handoff", .., "context"] => context_note(value),
        ["sandbox", leaf] | ["roles", _, "sandbox", leaf] => {
            super::permissions::value_note(leaf, value)
        }
        _ => "",
    }
}

/// What each `workflows.style` makes of a crew run, as `/style` lists them.
fn style_note(value: &str) -> &'static str {
    match value {
        "follow_lead" => "As the lead's CLI does: a workflow under Claude Code, a goal under Codex",
        "workflow" => "The lead plans every phase up front, and the run ends with them",
        "goal" => "The crew keeps attempting until a check finds every criterion met",
        _ => "",
    }
}

/// What a replacement gets for each `handoff.context`.
fn context_note(value: &str) -> &'static str {
    match value {
        "fresh" => "The card only",
        "note" => "The handoff note: why it stopped and what's left",
        "note+transcript" => "The note, and the old session's transcript as a file",
        "note+summary" => "The note, with a summary of the old session",
        _ => "",
    }
}

/// One line on what a built-in preset does, for `/preset`.
pub fn preset_note(name: &str) -> &'static str {
    match name {
        "peer-crew" => "Every agent plans and builds as a peer",
        "coordinated-crew" => "One coordinator plans and hands out the work",
        "solo" => "One agent does the whole task",
        "solo-reviewed" => "One agent does it and another reviews",
        "best-of-n" => "Several agents try and the best result wins",
        "draft-and-vote" => "Agents draft plans and vote on one",
        name if name.starts_with("ablate-") => "Turns one part of the crew off, for research",
        _ => "",
    }
}

/// `theme` as `Theme`, for titles.
pub fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_text_becomes_toml() {
        assert_eq!(toml_text("light", "\"dark\""), "\"light\"");
        assert_eq!(toml_text("42", "10"), "42");
        assert_eq!(toml_text("42", "\"10m\""), "\"42\"");
        assert_eq!(toml_text("true", ""), "true");
        assert_eq!(toml_text("\"x\"", "1"), "\"x\"");
        assert_eq!(toml_text("[\"a\"]", ""), "[\"a\"]");
        assert_eq!(toml_text("say \"hi\"", ""), "\"say \\\"hi\\\"\"");
    }

    #[test]
    fn freshness_says_when_the_lists_were_read() {
        let now = time::macros::datetime!(2026-10-01 12:00 UTC);
        let hour_ago = now - time::Duration::hours(1);
        assert_eq!(
            freshness(Some(hour_ago), false, now).as_deref(),
            Some("Model lists refreshed 1h ago.")
        );
        assert_eq!(
            freshness(Some(now), true, now).as_deref(),
            Some("Model lists refreshed just now; checking for newer ones.")
        );
        assert_eq!(
            freshness(None, true, now).as_deref(),
            Some("Checking each CLI's model list.")
        );
        assert_eq!(freshness(None, false, now), None);
    }

    #[test]
    fn a_layer_over_every_file_is_named_once() {
        assert!(hidden_from_files("session", None).is_some());
        assert!(hidden_from_files("flag", None).is_some());
        assert!(hidden_from_files("preset", None).is_some());
        for layer in ["built-in", "global", "project", "project-local"] {
            assert_eq!(hidden_from_files(layer, None), None, "{layer}");
        }
        // Picked in a file, the preset gives way to that file and the later ones.
        assert_eq!(hidden_from_files("preset", Some("global")), None);
        let note = hidden_note("preset", Some("project"), SettingScope::Global);
        assert!(note.contains("picked in a later file"), "{note}");
    }

    #[test]
    fn values_read_without_quotes() {
        assert_eq!(value_words("\"dark\""), "dark");
        assert_eq!(value_words(""), "unset");
        assert_eq!(
            value_words("[\"codex/gpt-5.6\", \"claude-code/opus\"]"),
            "codex/gpt-5.6, claude-code/opus"
        );
        assert_eq!(value_words("[]"), "none");
        assert_eq!(
            row_value_words("approvals.approver.model", ""),
            "cheapest, other company"
        );
        assert_eq!(
            row_value_words("roles.owner.handoff.fallback", "[]"),
            "other logged-in CLIs"
        );
        assert_eq!(
            row_value_words("roles.owner.sandbox.network", ""),
            "same as every agent"
        );
        assert_eq!(row_value_words("handoff.fallback", "[\"codex\"]"), "codex");
        assert_eq!(row_value_words("handoff.summarizer", ""), "unset");
        assert_eq!(value_words("30.0"), "30.0");
        assert!(same_value("'dark'", "\"dark\""));
    }

    #[test]
    fn handoff_contexts_say_what_the_replacement_gets() {
        for key in [
            "handoff.context",
            "roles.owner.handoff.context",
            "handoff.replace.codex.context",
            "roles.owner.handoff.replace.codex.context",
        ] {
            assert_eq!(value_note(key, "fresh"), "The card only", "{key}");
            for value in ["note", "note+transcript", "note+summary"] {
                assert!(value_note(key, value).starts_with("The "), "{key} {value}");
            }
        }
        assert_eq!(value_note("narrator.context", "fresh"), "");
        assert_eq!(value_note("handoff.strategy", "first"), "");
        assert_eq!(
            value_note("preset", "solo"),
            "One agent does the whole task"
        );
    }

    #[test]
    fn model_roles_from_keys() {
        assert_eq!(model_role("default_agent"), Some(None));
        assert_eq!(model_role("roles.default.workers"), Some(None));
        assert_eq!(
            model_role("roles.reviewer.workers"),
            Some(Some(RoleName::Reviewer))
        );
        assert_eq!(
            model_role("approvals.approver.model"),
            Some(Some(RoleName::Approver))
        );
        assert_eq!(model_role("roles.reviewer.effort"), None);
    }
}
