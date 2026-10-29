//! How settings read on screen: layers and scopes in words, values without their TOML
//! quotes, and typed text turned back into TOML.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::model_picker::ModelSource;
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::approvals::{ApprovalMode, Decides};
use time::{OffsetDateTime, UtcOffset};

use crate::format::setting_value;
use crate::views::resume::ago;

/// The four answers to "where to save it", in order, with their files. They answer to their
/// numbers, as every question's options do.
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
        "workflow" => "workflow".to_owned(),
        "flag" => "--set".to_owned(),
        "session" => "this session".to_owned(),
        other => other.to_owned(),
    }
}

/// The scope a Reset drops the key from: a settings file or this session. Built-in values,
/// the workflow, and `--set` flags can't be reset in the app.
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
/// workflow, a `--set` flag, or this session's own value. A workflow picked in a file
/// (`workflow_from`) gives way to that file, so it never wins over every one.
pub fn hidden_from_files(layer: &str, workflow_from: Option<&str>) -> Option<String> {
    if !FILE_SCOPES
        .iter()
        .all(|scope| scope.hidden_by_value(layer, workflow_from))
    {
        return None;
    }
    Some(match layer {
        "workflow" => {
            "The workflow sets this, so saving it in a file won't change it. Use /workflow \
                     copy to edit the workflow, or save it for this session."
                .to_owned()
        }
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

/// How fresh one level's list is, from its own source: "Model list refreshed 3m ago" with
/// "; asking OpenCode…" while it's asked again, or that it couldn't be read and which list
/// shows instead, then what the list leaves out here, when it leaves something out. The first
/// level takes the oldest of its sources. `None` for a level with no source of its own, such
/// as Claude Code's, whose aliases need nothing asked.
pub fn level_freshness(
    sources: &[ModelSource],
    cli: Option<&str>,
    now: OffsetDateTime,
    offset: UtcOffset,
) -> Option<String> {
    let Some(cli) = cli else {
        let oldest = sources.iter().filter_map(|source| source.listed).min()?;
        return Some(format!("Model lists refreshed {}.", ago(oldest, now)));
    };
    let source = sources.iter().find(|source| source.cli == cli)?;
    let fresh = source_freshness(source, cli, now, offset);
    match (fresh, &source.note) {
        (Some(fresh), Some(note)) => Some(format!("{fresh} {note}")),
        (fresh, note) => fresh.or_else(|| note.clone()),
    }
}

fn source_freshness(
    source: &ModelSource,
    cli: &str,
    now: OffsetDateTime,
    offset: UtcOffset,
) -> Option<String> {
    let label = crate::format::agent_label(cli);
    Some(match (source.listed, source.asking, source.failed) {
        (Some(at), true, _) => format!("Model list refreshed {}; asking {label}…", ago(at, now)),
        (None, true, _) => format!("Asking {label} for its models…"),
        (Some(at), false, true) => format!(
            "{label}'s list couldn't be read; showing the one from {}.",
            clock_24(at, offset)
        ),
        (None, false, true) => format!("{label}'s list couldn't be read."),
        (Some(at), false, false) => format!("Model list refreshed {}.", ago(at, now)),
        (None, false, false) => return None,
    })
}

/// What a CLI's row on the first level adds about its list: "asking…" while it's asked again,
/// or "list from 21:40" when the latest ask failed.
pub fn source_state(source: &ModelSource, offset: UtcOffset) -> Option<String> {
    if source.asking {
        return Some("asking…".to_owned());
    }
    match (source.failed, source.listed) {
        (true, Some(at)) => Some(format!("list from {}", clock_24(at, offset))),
        (true, None) => Some("list couldn't be read".to_owned()),
        _ => None,
    }
}

/// `21:40` in the terminal's zone.
fn clock_24(at: OffsetDateTime, offset: UtcOffset) -> String {
    let at = at.to_offset(offset);
    format!("{:02}:{:02}", at.hour(), at.minute())
}

/// Why saving in `scope` won't show while `layer` sets the key, and what to do instead.
pub fn hidden_note(layer: &str, workflow_from: Option<&str>, scope: SettingScope) -> String {
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
        "workflow" if workflow_from.is_some() => format!(
            "The workflow sets this, and it's picked in a later file, so saving it {saving} won't \
             change it. Save it where you picked the workflow, or later."
        ),
        "workflow" => format!(
            "The workflow sets this, so saving it {saving} won't change it. \
             Use /workflow copy to edit the workflow, or save it for this session."
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

/// A word a model setting takes in a model's place, as a sentence says it: `the judge` for
/// `approvals.decides = "judge"`. `None` for anything else, which names a model.
pub fn word_for_a_model(value: &str) -> Option<&'static str> {
    let word = value.trim().trim_matches(['"', '\'']);
    word.eq_ignore_ascii_case(Decides::JUDGE)
        .then_some("the judge")
}

/// One worker, `claude-code/claude-opus-5-5`, as `Opus 5.5 on Claude Code`.
pub fn worker_words(worker: &str) -> String {
    if let Some(word) = word_for_a_model(worker) {
        return word.to_owned();
    }
    match worker.parse::<ModelRef>() {
        Ok(worker) => DisplayNames::default().worker(&worker),
        Err(_) => worker.to_owned(),
    }
}

/// A row's value in words. An empty one says what it stands for where unset means something,
/// such as the judge picked for each piece of work, and a list of approval modes names each as
/// the approval mode row does, `auto preview` for `auto_preview`.
pub fn row_value_words(key: &str, toml: &str) -> String {
    let words = value_words(toml);
    match default_meaning(key) {
        Some(meaning) if matches!(words.as_str(), "unset" | "none") => meaning.to_owned(),
        _ if lists_modes(key) => words
            .split(", ")
            .map(mode_words)
            .collect::<Vec<_>>()
            .join(", "),
        _ => words,
    }
}

/// The `[outside]` lists, whose items are approval modes.
fn lists_modes(key: &str) -> bool {
    matches!(key, "outside.read" | "outside.write" | "outside.ask")
}

/// An approval mode as its row says it, or the word as it is when it names none.
fn mode_words(word: &str) -> String {
    serde_json::from_value::<ApprovalMode>(serde_json::Value::String(word.to_owned()))
        .map(|mode| mode.label().to_owned())
        .unwrap_or_else(|_| word.to_owned())
}

/// What an unset or empty value means for the keys whose default stands for something.
fn default_meaning(key: &str) -> Option<&'static str> {
    let segments: Vec<&str> = key.split('.').collect();
    Some(match segments.as_slice() {
        ["approvals", "decides"] => "cheapest, other provider",
        ["approvals", "answers"] => "you or main, by mode",
        ["approvals", "judge", "second_model"] => "none, one judge",
        ["fallback", "order"] | ["fallback", _, "order"] => "other logged-in CLIs",
        ["roles", _, "sandbox", _] => "same as every agent",
        ["outside", "ask"] => "none, kept to the project",
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

/// A role as rows name it: the main agent, or the role's own name.
pub fn role_words(role: Option<RoleName>) -> String {
    match role {
        None | Some(RoleName::Main) => "Main agent".to_owned(),
        Some(role) => role.as_str().to_owned(),
    }
}

/// The role a key picks models for, as `/model` lists them. `None` for a key that isn't one.
pub fn model_role(key: &str) -> Option<Option<RoleName>> {
    match key {
        "crew.main" => Some(None),
        "narrator.model" => Some(Some(RoleName::Narrator)),
        _ => {
            let role = key.strip_prefix("roles.")?.strip_suffix(".models")?;
            let role = parse_role(role)?;
            Some((role != RoleName::Main).then_some(role))
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
        ["workflow"] => workflow_note(value),
        ["goal", "default"] => match value {
            "off" => "A request has a goal only when you set one",
            "from-request" => {
                "Every request: the main agent says what done means, and the work goes on until it holds"
            }
            _ => "",
        },
        ["goal", "check"] => goal_check_note(value),
        ["roles", _, "goal"] => role_goal_note(value),
        ["fallback", .., "context"] => context_note(value),
        ["sandbox", leaf] | ["roles", _, "sandbox", leaf] => {
            super::permissions::value_note(leaf, value)
        }
        ["approvals", "autopilot"] => match value {
            "true" => "Nothing waits for you at any approval a run opens",
            "false" => "Each approval keeps its own mode",
            _ => "",
        },
        ["approvals", _, "at_hard_limit"] => hard_limit_note(value),
        _ => "",
    }
}

/// What each `at_hard_limit` does in autopilot.
fn hard_limit_note(value: &str) -> &'static str {
    match value {
        "stop" => "End that part safely and keep its work",
        "send_back" => "Run another round, or decline the change",
        "open_pull_request" => "Open a pull request for you to review",
        "wait" => "Leave the approval for you; the run waits",
        _ => "",
    }
}

/// What checks a goal's lines, for each `goal.check`.
fn goal_check_note(value: &str) -> &'static str {
    match value {
        "auto" => "A command when the goal names one, else the judge",
        "command" => "Commands only, by their exit code",
        "judge" => "The judge reads the result against each line",
        "both" => "The commands, then the judge on every line",
        _ => "",
    }
}

/// Whether a role works each task as a goal, and who keeps it going.
fn role_goal_note(value: &str) -> &'static str {
    match value {
        "off" => "A task is done when its agent says so and its checks pass",
        "cli" => "The CLI's own goal, which Codex has; Argonauts Code still runs the check",
        "argonauts" => {
            "Argonauts Code runs the check after each try and sends back what still fails"
        }
        _ => "",
    }
}

/// What a fallback gets for each `fallback.context`.
fn context_note(value: &str) -> &'static str {
    match value {
        "fresh" => "The task only",
        "note" => "The fallback note: why it stopped and what's left",
        "note+transcript" => "The note, and the old session's transcript as a file",
        "note+summary" => "The note, with a summary of the old session",
        _ => "",
    }
}

/// One line on what a built-in workflow does, for `/workflow`.
pub fn workflow_note(name: &str) -> &'static str {
    match name {
        "council" => "Everyone proposes and challenges, the judge decides, then they build",
        "orchestrator" => "The orchestrator plans and delegates, workers build",
        "captain" => "The CLI you talk to does the work itself",
        "maker-checker" => "A worker builds, a reviewer sends it back until it holds",
        "best-of-n" => "Several workers try the same task and the judge picks one",
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

    /// What a list leaves out follows how fresh it is, and stands alone while nothing is known
    /// of when it listed.
    #[test]
    fn a_levels_line_says_what_its_list_leaves_out() {
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::days(20_000);
        let note = "This repo's own OpenCode config is left out until you trust the repo with \
                    `argonauts setup`.";
        let mut source = ModelSource {
            cli: "opencode".to_owned(),
            listed: Some(now),
            asking: false,
            failed: false,
            note: Some(note.to_owned()),
        };
        assert_eq!(
            level_freshness(&[source.clone()], Some("opencode"), now, UtcOffset::UTC),
            Some(format!("Model list refreshed just now. {note}"))
        );
        source.listed = None;
        assert_eq!(
            level_freshness(&[source], Some("opencode"), now, UtcOffset::UTC).as_deref(),
            Some(note)
        );
    }

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
        assert!(hidden_from_files("workflow", None).is_some());
        for layer in ["built-in", "global", "project", "project-local"] {
            assert_eq!(hidden_from_files(layer, None), None, "{layer}");
        }
        // Picked in a file, the workflow gives way to that file and the later ones.
        assert_eq!(hidden_from_files("workflow", Some("global")), None);
        let note = hidden_note("workflow", Some("project"), SettingScope::Global);
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
            row_value_words("approvals.decides", ""),
            "cheapest, other provider"
        );
        // The judge is a word, never a CLI named judge running its default.
        assert_eq!(workers_words("\"judge\""), "the judge");
        assert_eq!(word_for_a_model("codex/gpt-5.6"), None);
        assert_eq!(
            row_value_words("fallback.worker.order", "[]"),
            "other logged-in CLIs"
        );
        assert_eq!(
            row_value_words("roles.worker.sandbox.network", ""),
            "same as every agent"
        );
        assert_eq!(row_value_words("fallback.order", "[\"codex\"]"), "codex");
        assert_eq!(
            row_value_words("outside.ask", "[\"supervised\", \"auto_preview\"]"),
            "supervised, auto preview"
        );
        assert_eq!(
            row_value_words("outside.write", "[\"auto\", \"autopilot\"]"),
            "auto, autopilot"
        );
        assert_eq!(
            row_value_words("outside.read", "[\"auto_preview\"]"),
            "auto preview"
        );
        assert_eq!(
            row_value_words("outside.ask", "[]"),
            "none, kept to the project"
        );
        assert_eq!(row_value_words("fallback.summarizer", ""), "unset");
        assert_eq!(value_words("30.0"), "30.0");
        assert!(same_value("'dark'", "\"dark\""));
    }

    #[test]
    fn fallback_contexts_say_what_the_fallback_gets() {
        for key in [
            "fallback.context",
            "fallback.worker.context",
            "fallback.replace.codex.context",
            "fallback.worker.replace.codex.context",
        ] {
            assert_eq!(value_note(key, "fresh"), "The task only", "{key}");
            for value in ["note", "note+transcript", "note+summary"] {
                assert!(value_note(key, value).starts_with("The "), "{key} {value}");
            }
        }
        assert_eq!(value_note("narrator.context", "fresh"), "");
        assert_eq!(value_note("fallback.strategy", "first"), "");
        assert_eq!(
            value_note("workflow", "captain"),
            "The CLI you talk to does the work itself"
        );
    }

    #[test]
    fn model_roles_from_keys() {
        assert_eq!(model_role("crew.main"), Some(None));
        assert_eq!(model_role("roles.main.models"), Some(None));
        assert_eq!(
            model_role("roles.reviewer.models"),
            Some(Some(RoleName::Reviewer))
        );
        assert_eq!(model_role("approvals.decides"), None);
        assert_eq!(model_role("roles.reviewer.effort"), None);
    }
}
