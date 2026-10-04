//! What `/config` and the settings pickers know about each key: a description, the values a
//! picker offers, where a change is saved by default, and whether it's a safety key.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use argonauts_core::input::EFFORT_LEVELS;
use argonauts_core::settings::{SettingMeta, SettingScope, Settings};
use serde_json::Value as Json;

use crate::resolve::{is_safety_key, key_matches};

/// Keys that are personal choices in a repo, saved just for you by default.
const JUST_YOU: &[&str] = &[
    "default_agent",
    "agents.*.model",
    "agents.*.effort",
    "roles.*.workers",
    "roles.*.effort",
];

/// Keys about you or your machine, saved globally by default. A binary's path stays out of the
/// files your team shares, and the daily caps count across every repo.
const EVERYWHERE: &[&str] = &[
    "agents.*.auth",
    "agents.*.binary",
    "endpoints.*",
    "ui.*",
    "narrator.*",
    "notifications.*",
    "limits.usd_per_day",
    "limits.auto_merges_per_day",
];

/// The checkpoints `/approvals` covers, as they appear under `approvals.`.
const CHECKPOINTS: &[(&str, &str)] = &[
    ("goal", "The goal checkpoint"),
    ("plan", "The plan checkpoint"),
    ("merge", "The merge checkpoint"),
    ("escalation", "A stuck card"),
    ("budget", "More budget from the run's reserve"),
    (
        "change_request",
        "A change request no other card's owner can decide",
    ),
    ("recipe_promotion", "Promoting a recipe change"),
];

const BOOL: &[&str] = &["true", "false"];

const ON_RETURN: &[&str] = &["ask", "keep", "review", "hand_back", "redo"];
const STRATEGIES: &[&str] = &["first", "spread", "race"];
const CONTEXTS: &[&str] = &["fresh", "note", "note+transcript", "note+summary"];

/// What `/handoff` edits, below `handoff.` for every role and below `roles.*.handoff.` for one.
/// Words are quoted for the picker; an empty list takes any value of the type.
const HANDOFF: &[(&str, &str, Choices)] = &[
    (
        "mode",
        "When an agent stops, such as at a usage limit: ask you, hand off to the next fallback, \
         or wait for the vendor",
        Choices::Words(&["ask", "auto", "wait"]),
    ),
    (
        "fallback",
        "Who takes over, in order: an agent, or a table with its own rules. With no list \
         anywhere, the other installed and logged-in CLIs",
        Choices::Any,
    ),
    (
        "ask_timeout",
        "How long the handoff card waits before handing off by itself, or \"none\"",
        Choices::Any,
    ),
    (
        "allow_api_billing",
        "Whether a handoff may move work onto a paid API without asking you",
        Choices::Bool,
    ),
    (
        "return_to_original",
        "Move the card back when the original vendor returns, the older spelling of on_return = \
         \"hand_back\"",
        Choices::Bool,
    ),
    (
        "broken_work",
        "What the new owner does with broken work in progress: continue from it, or restart",
        Choices::Words(&["continue", "restart"]),
    ),
    (
        "strategy",
        "Who takes the cards: the first fallback that's up, spread across them, or a race",
        Choices::Words(STRATEGIES),
    ),
    (
        "race_count",
        "How many attempts run at once in a race",
        Choices::Any,
    ),
    (
        "on_return",
        "When the original vendor returns: ask you, keep the new owner, review its work, hand \
         the card back, or redo it",
        Choices::Words(ON_RETURN),
    ),
    (
        "checkpoint",
        "Tag the card's state at every handoff, so you can go back to it",
        Choices::Bool,
    ),
    (
        "merge_rule",
        "Work a weaker model finished needs a stronger model's review before it merges \
         (upgrade_review), or only shows as debt (none)",
        Choices::Words(&["upgrade_review", "none"]),
    ),
    (
        "unknown_tier",
        "How a model with no tier and too short a track record counts: weaker, or equal",
        Choices::Words(&["weaker", "equal"]),
    ),
    (
        "max_handoffs",
        "Handoffs per card before it escalates to you",
        Choices::Any,
    ),
    (
        "context",
        "What the replacement is told: the card only (fresh), the handoff note, the note and the \
         old session's transcript, or the note and a summary of it",
        Choices::Words(CONTEXTS),
    ),
    (
        "summarizer",
        "Who summarizes the old session for note+summary: harness/model or an endpoint. Unset \
         uses the replacement's harness with its cheapest model",
        Choices::Any,
    ),
    (
        "replace.*.with",
        "Who replaces this vendor, in order: an agent, or a table with its own rules",
        Choices::Any,
    ),
    (
        "replace.*.strategy",
        "How this vendor's cards go to its replacements: the first, spread, or a race",
        Choices::Words(STRATEGIES),
    ),
    (
        "replace.*.on_return",
        "When this vendor returns: ask you, keep, review, hand back, or redo",
        Choices::Words(ON_RETURN),
    ),
    (
        "replace.*.context",
        "What this vendor's replacements are told: fresh, note, note+transcript, or note+summary",
        Choices::Words(CONTEXTS),
    ),
];

/// What `/permissions` edits, below `sandbox.` for every role and below `roles.*.sandbox.` for
/// one.
const SANDBOX: &[(&str, &str, Choices)] = &[
    (
        "mode",
        "Whose rules guard the agents: the OS sandbox, a container, each AI tool's own, or none \
         at all",
        Choices::Words(&["os", "container", "tool", "none"]),
    ),
    (
        "network",
        "Whether commands agents run can reach the network",
        Choices::Bool,
    ),
    (
        "agents_see_other_copies",
        "Whether an agent can read the other agents' copies of the repo, not only its own",
        Choices::Bool,
    ),
    (
        "agents_commit",
        "Whether agents commit their own work. Off, Argonauts commits for them",
        Choices::Bool,
    ),
];

#[derive(Clone, Copy)]
enum Choices {
    Any,
    Bool,
    Words(&'static [&'static str]),
}

/// Descriptions for keys whose doc comment is missing, or doesn't read as one line on its own.
const OWN_WORDS: &[(&str, &str)] = &[
    (
        "agents.*.extra_args",
        "Extra arguments for the harness's program on every run",
    ),
    (
        "agents.*.price.input_per_mtok",
        "Dollars per million uncached input tokens",
    ),
    (
        "agents.*.price.output_per_mtok",
        "Dollars per million output tokens",
    ),
    (
        "agents.*.price.cached_input_per_mtok",
        "Dollars per million cached input tokens. Unset uses the input price",
    ),
    (
        "approvals.pause_auto_when_spot_checks_waiting",
        "Auto approval pauses while this many spot checks wait for you",
    ),
    (
        "approvals.recipe_promotion.rules.change_classes",
        "Recipe rules: the classes of change auto may promote, such as adding a checklist item",
    ),
    (
        "budget.history_min",
        "Past cards of a task type needed before their median sets a card's budget",
    ),
    (
        "budget.lines.goal_and_plan",
        "Share of the run budget for the goal and the plan",
    ),
    (
        "budget.lines.contracts",
        "Share of the run budget for interface and acceptance-test cards",
    ),
    (
        "budget.lines.cards",
        "Share of the run budget for build cards",
    ),
    ("budget.lines.review", "Share of the run budget for review"),
    ("budget.lines.spikes", "Share of the run budget for spikes"),
    (
        "budget.lines.integration",
        "Share of the run budget for integration",
    ),
    (
        "budget.lines.reserve",
        "Share of the run budget held in reserve",
    ),
    (
        "concurrency.max_sessions",
        "Most agent sessions running at once",
    ),
    (
        "concurrency.max_per_vendor",
        "Most sessions running at once with one vendor",
    ),
    ("critique.rounds", "Critique rounds. 0 turns critique off"),
    ("critique.checklist", "What every critique checks"),
    (
        "divide.acceptance_tests_by",
        "Who writes each card's acceptance tests: its reviewer, or nobody",
    ),
    ("divide.max_cards", "Most cards a plan may divide into"),
    (
        "divide.predictions",
        "Each owner writes predictions on its card as probabilities, such as passing the gates \
         on the first try",
    ),
    (
        "drafting.by",
        "Who drafts plans: the crew, or the coordinator",
    ),
    (
        "drafting.drafters",
        "How many drafters. Unset means every crew member",
    ),
    (
        "endpoints.*.max_concurrency",
        "Most requests to this endpoint at once",
    ),
    (
        "endpoints.*.timeout",
        "How long one request may take, such as \"2m\"",
    ),
    (
        "endpoints.*.header_env.*",
        "The environment variable that holds this header's value",
    ),
    (
        "evolution.canary_runs",
        "Runs a promoted recipe change is watched for as a canary",
    ),
    (
        "evolution.replay.tasks",
        "Tasks replayed to test a candidate change",
    ),
    (
        "evolution.replay.runs_per_task",
        "Runs of each replayed task",
    ),
    (
        "gates.timeout",
        "How long one gate command may run, such as \"30m\"",
    ),
    (
        "understand.max_questions",
        "Most questions Understand may ask you about your request",
    ),
    (
        "understand.readers",
        "How many models read your request. Unset means every crew member",
    ),
    (
        "understand.comparers",
        "How many models from other companies compare the readings",
    ),
    (
        "understand.always_ask",
        "Topics that are always a question, never a majority's silent assumption",
    ),
    (
        "goal.max_attempts",
        "Most attempts a goal run makes. The run's budget still caps it",
    ),
    (
        "goal.stuck_after",
        "Attempts in a row that end on the same blocker before a goal is stuck",
    ),
    (
        "goal.idle_after",
        "Attempts in a row that meet no new criterion and change no file before a goal is stuck",
    ),
    (
        "health.window",
        "The span the health counts look back over, such as \"2m\"",
    ),
    (
        "health.down_after",
        "Overload or server errors within the window that mark a vendor down",
    ),
    (
        "health.down_sessions",
        "Sessions those errors must come from before a vendor counts as down",
    ),
    (
        "judging.method",
        "How plans are judged: cross-judging, one judge, a vote, the coordinator, or you",
    ),
    (
        "judging.both_orders",
        "Each judge scores the plans in both orders, so position doesn't sway it",
    ),
    ("judging.rubric", "What judges score plans on"),
    (
        "limits.auto_merges_per_day",
        "Most auto merges a day, across every repo",
    ),
    (
        "limits.evolution_usd_per_month",
        "The most recipe evolution may spend a month, in dollars",
    ),
    (
        "mapping.remap_after_revision",
        "The mapper maps the drafts again after they're revised",
    ),
    (
        "merge.mode",
        "What happens to approved work: ask you to merge it, or leave it on its branch",
    ),
    ("merge.pr_draft", "Open pull requests as drafts"),
    (
        "merge.leave_out",
        "Untracked files left out of Argonauts' commits, such as coverage/ or *.log",
    ),
    (
        "merge.leave_out_builtin",
        "Also leave a built-in list of build and cache output, such as __pycache__/, out of commits",
    ),
    (
        "native_fanout.max",
        "Most subagents a harness session may start",
    ),
    (
        "notifications.bell",
        "Ring the terminal bell when something needs you",
    ),
    (
        "review.assign",
        "How reviewers are assigned: a ring, at random, by the coordinator, or fixed",
    ),
    ("review.per_card", "Reviewers per card"),
    (
        "roles.*.compaction",
        "Compaction for this role, over the crew-wide setting",
    ),
    (
        "roles.*.instructions",
        "Extra instructions for this role's agent",
    ),
    (
        "roles.*.lifetime",
        "How long this role's session lasts: a card, a run, or the project",
    ),
    (
        "roles.*.native_fanout",
        "Subagents this role's harness may start: on, off, or a table with a max",
    ),
    (
        "roles.*.native_fanout.max",
        "Most subagents this role's harness session may start",
    ),
    (
        "routing.algorithm",
        "How cards find owners: a bandit that learns, round robin, the coordinator, or fixed \
         owners",
    ),
    (
        "routing.no_explore_labels",
        "Card labels that never go to a less-proven agent",
    ),
    (
        "sandbox.container_image",
        "The image container sessions run in, with the agent's CLI installed",
    ),
    (
        "sandbox.extra_writable",
        "Paths outside the worktree that agents may write to",
    ),
    (
        "sandbox.proxy_allow",
        "Hosts agents may reach through the proxy, on top of their harness's API hosts",
    ),
    (
        "sandbox.untrusted_repos",
        "Repos you haven't trusted run in a container, or not at all",
    ),
    (
        "sessions.lifetime",
        "How long an agent session lasts: a card, a run, or the project. A role can override it",
    ),
    (
        "solo.style",
        "Solo you choose: direct works in your checkout like its CLI, checked works in a copy \
         with gates and a merge",
    ),
    (
        "spikes.enabled",
        "Small experiments settle what the plans disagree on",
    ),
    (
        "triage.all_hands",
        "All hands: every task goes to the crew, and the lead designs its workflow. Questions \
         are still just answered, and /solo still runs one task solo",
    ),
    ("triage.use_recipes", "A matching recipe decides the mode"),
    (
        "triage.otherwise",
        "What triage does between the thresholds: solo, the crew, or solo then the crew",
    ),
    (
        "triage.solo_max_files",
        "Triage picks solo at this many files or fewer",
    ),
    (
        "triage.solo_requires_tests",
        "Solo also needs the repo to have tests",
    ),
    (
        "triage.crew_min_files",
        "Triage picks the crew at this many files or more",
    ),
    (
        "triage.crew_labels",
        "Labels that always get the crew, such as security",
    ),
    (
        "triage.solo_style",
        "Solo the triage rules pick: checked, so it can grow into a crew run, direct, or as \
         solo.style says",
    ),
    (
        "triage.read_instructions",
        "Follow what a request says about how to run it, such as who coordinates or reviews",
    ),
    ("ui.bell", "Let the app ring the terminal bell"),
    (
        "ui.recap_after",
        "How long a turn runs before a recap follows it, such as \"2m\"",
    ),
    (
        "ui.takeover",
        "Where a vendor's own screen opens when you take over: here, a tmux split, or a new tab",
    ),
    (
        "visibility.drafts_hidden_until_sealed",
        "Drafts stay hidden from other drafters until all are sealed",
    ),
    (
        "visibility.judges_see_authors",
        "Judges see who wrote each plan",
    ),
    (
        "watchdogs.wall_clock_min",
        "Minutes a card may run. A soft stop comes first",
    ),
];

/// The keys the settings commands name come first, in picker order, then every other key. Map
/// keys you choose, such as harness ids and role names, are `*`.
pub fn settings_catalog() -> Vec<SettingMeta> {
    catalog().to_vec()
}

/// The catalog entry for a real key such as `agents.codex.model`, matching `*` in the catalog.
pub fn setting_meta(key: &str) -> Option<SettingMeta> {
    let catalog = catalog();
    catalog
        .iter()
        .find(|meta| meta.key == key)
        .or_else(|| catalog.iter().find(|meta| fits(&meta.key, key)))
        .cloned()
}

/// Whether a real dotted `key` is the catalog key `pattern`, where each `*` stands for exactly
/// one name.
pub(crate) fn fits(pattern: &str, key: &str) -> bool {
    let (mut pattern, mut key) = (pattern.split('.'), key.split('.'));
    loop {
        match (pattern.next(), key.next()) {
            (None, None) => return true,
            (Some(part), Some(name)) if part == "*" || part == name => {}
            _ => return false,
        }
    }
}

fn default_scope(key: &str) -> SettingScope {
    let any = |patterns: &[&str]| patterns.iter().any(|pattern| key_matches(pattern, key));
    if any(JUST_YOU) {
        SettingScope::ProjectLocal
    } else if any(EVERYWHERE) {
        SettingScope::Global
    } else {
        SettingScope::Project
    }
}

fn catalog() -> &'static [SettingMeta] {
    static CATALOG: OnceLock<Vec<SettingMeta>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut entries = named();
        let mut seen: BTreeSet<String> = entries.iter().map(|meta| meta.key.clone()).collect();
        for leaf in schema_leaves() {
            if seen.insert(leaf.key.clone()) {
                let description = OWN_WORDS
                    .iter()
                    .find(|(key, _)| *key == leaf.key)
                    .map(|(_, words)| (*words).to_owned())
                    .unwrap_or(leaf.description);
                let allowed: Vec<&str> = leaf.allowed.iter().map(String::as_str).collect();
                entries.push(meta(&leaf.key, &description, &allowed));
            }
        }
        entries
    })
}

fn meta(key: &str, description: &str, allowed: &[&str]) -> SettingMeta {
    SettingMeta {
        key: key.to_owned(),
        description: description.to_owned(),
        allowed: allowed.iter().map(|value| (*value).to_owned()).collect(),
        default_scope: default_scope(key),
        safety: is_safety_key(key),
    }
}

/// A key whose picker offers these words, quoted as TOML strings.
fn words(key: &str, description: &str, choices: &[&str]) -> SettingMeta {
    let quoted: Vec<String> = choices.iter().map(|word| quote(word)).collect();
    let quoted: Vec<&str> = quoted.iter().map(String::as_str).collect();
    meta(key, description, &quoted)
}

/// A word as TOML text.
pub(crate) fn quote(word: &str) -> String {
    toml::Value::String(word.to_owned()).to_string()
}

/// Each key of `table` below `<section>.` for every role, then below `roles.*.<section>.` for one.
fn per_role_keys(section: &str, table: &[(&str, &str, Choices)]) -> Vec<SettingMeta> {
    let mut entries = Vec::new();
    let prefixes = [
        (section.to_owned(), true),
        (format!("roles.*.{section}"), false),
    ];
    for (prefix, for_every_role) in prefixes {
        for (name, description, choices) in table {
            let key = format!("{prefix}.{name}");
            let description = if for_every_role {
                format!("{description}. A role's own setting wins")
            } else {
                (*description).to_owned()
            };
            entries.push(match choices {
                Choices::Any => meta(&key, &description, &[]),
                Choices::Bool => meta(&key, &description, BOOL),
                Choices::Words(choices) => words(&key, &description, choices),
            });
        }
    }
    entries
}

fn named() -> Vec<SettingMeta> {
    let presets: Vec<String> = crate::presets().into_iter().map(|p| p.name).collect();
    let presets: Vec<&str> = presets.iter().map(String::as_str).collect();
    let mut entries = vec![
        // /model
        meta(
            "default_agent",
            "Who you chat with, and who works solo: a harness, or harness/model",
            &[],
        ),
        meta(
            "roles.*.workers",
            "Who fills this role, in order. Empty lets the router pick from the crew",
            &[],
        ),
        meta(
            "agents.*.model",
            "The model this harness runs unless a role names one. Unset uses the CLI's default",
            &[],
        ),
        // /effort
        words(
            "agents.*.effort",
            "Reasoning effort for this harness. Unset uses the CLI's default",
            EFFORT_LEVELS,
        ),
        words(
            "roles.*.effort",
            "Reasoning effort for this role",
            EFFORT_LEVELS,
        ),
        // /preset. The picker adds your own presets from `presets_in`.
        words(
            "preset",
            "How the crew works: peer-crew plans and judges as peers, coordinated-crew has one \
             coordinator",
            &presets,
        ),
        // /agents
        meta(
            "crew",
            "The harnesses and endpoints that work on crew runs",
            &[],
        ),
        meta(
            "agents.*.enabled",
            "Whether this harness may work at all",
            BOOL,
        ),
        words(
            "agents.*.auth",
            "Your own login (subscription), or an API key from the environment (api-key)",
            &["subscription", "api-key"],
        ),
        meta(
            "agents.*.binary",
            "The harness's program, as an absolute path outside the repo, when it isn't on PATH",
            &[],
        ),
        meta(
            "agents.*.version_pin",
            "The version you pinned. Doctor warns when the installed one differs",
            &[],
        ),
        meta(
            "endpoints.*.base_url",
            "The endpoint's OpenAI-compatible address, such as http://localhost:11434/v1",
            &[],
        ),
        meta(
            "endpoints.*.api_key_env",
            "The environment variable that holds its key, never the key itself",
            &[],
        ),
        words(
            "endpoints.*.api",
            "Which API it speaks",
            &["chat_completions", "responses"],
        ),
        words(
            "endpoints.*.structured_output",
            "How it's asked for checked output: a strict schema, a JSON object, or the prompt",
            &["json_schema", "json_object", "prompt"],
        ),
        meta(
            "endpoints.*.local",
            "A model server on your machine, so nothing leaves it",
            BOOL,
        ),
        meta(
            "endpoints.*.allow_api_billing",
            "Whether work may move onto this paid API without asking you",
            BOOL,
        ),
        // /approvals
        words(
            "approvals.answered_by",
            "Who answers the crew's questions, Understand's open questions and the go-ahead: \
             you, or the lead, whose answers become assumptions you can review. Unset, the lead \
             in auto mode and you otherwise. Set to the lead, it answers in every mode",
            &["you", "lead"],
        ),
        meta(
            "approvals.ask_you_about",
            "Topics that come to you even when the lead answers, matched without case: a \
             question's tag (scope, security, data, cost, detail) or its topic, such as \"log \
             level\", and \"go-ahead\", \"workflow change\", \"stuck goal\", or \"keep \
             going\". Over budget always comes to you",
            &[],
        ),
        words(
            "approvals.goal_changes",
            "When an agent asks to change the approved goal: ask you, allow clarifications, or \
             deny",
            &["ask", "allow_clarifications", "deny"],
        ),
    ];
    for (checkpoint, label) in CHECKPOINTS {
        entries.push(words(
            &format!("approvals.{checkpoint}.mode"),
            &format!(
                "{label}: supervised waits for you, shadow also records what auto would do, \
                 auto lets your settings decide"
            ),
            &["supervised", "shadow", "auto"],
        ));
        entries.push(words(
            &format!("approvals.{checkpoint}.decide_with"),
            &format!("{label}: who decides in auto mode, your rules, your approver model, or both"),
            &["rules", "model", "rules_and_model", "rules_or_model"],
        ));
        let otherwise: &[&str] = if *checkpoint == "merge" {
            &["ask", "send_back", "stop", "open_pull_request"]
        } else {
            &["ask", "send_back", "stop"]
        };
        entries.push(words(
            &format!("approvals.{checkpoint}.otherwise"),
            &format!("{label}: what happens when auto doesn't approve"),
            otherwise,
        ));
    }
    entries.extend([
        meta(
            "approvals.plan.rules.max_score_spread",
            "Plan rules: the largest gap between one judge's two scores, out of 10",
            &[],
        ),
        meta(
            "approvals.plan.rules.min_winner_margin",
            "Plan rules: how far the winning plan must lead the runner-up",
            &[],
        ),
        meta(
            "approvals.merge.rules.min_changed_line_coverage",
            "Merge rules: the share of changed lines the tests must cover, from 0 to 1, where \
             coverage is measured",
            &[],
        ),
        meta(
            "approvals.merge.rules.max_combined_diff_lines",
            "Merge rules: the most lines the run's combined diff may have",
            &[],
        ),
        meta(
            "approvals.escalation.rules.max_extra_attempts",
            "Stuck-card rules: extra attempts auto may grant, by an agent from a family that \
             hasn't tried",
            &[],
        ),
        meta(
            "approvals.budget.rules.use_reserve",
            "Budget rules: whether auto may release the run's reserve, never above a ceiling",
            BOOL,
        ),
        meta(
            "approvals.approver.model",
            "The approver model, any model in the registry. Unset picks the cheapest model from \
             another company than the work it judges",
            &[],
        ),
        meta(
            "approvals.approver.model_rules",
            "Your rules for the approver model, a file in .argonauts/",
            &[],
        ),
        meta(
            "approvals.approver.different_family",
            "The approver must come from a model family no author or owner used",
            BOOL,
        ),
        meta(
            "approvals.approver.second_model",
            "A second approver, from another vendor, that must also approve",
            &[],
        ),
        meta(
            "approvals.spot_check_sample",
            "Share of auto decisions queued for your spot check, from 0 to 1",
            &[],
        ),
        // /budget
        meta(
            "budget.enabled",
            "Whether budgets stop work. Off, the meter still counts",
            BOOL,
        ),
        meta(
            "budget.run_usd",
            "The most a run may spend, in dollars",
            &[],
        ),
        meta("budget.run_tokens", "The most tokens a run may use", &[]),
        meta(
            "budget.run_quota",
            "The most of a subscription window a run may use, from 0 to 1",
            &[],
        ),
        meta(
            "budget.run_minutes",
            "The longest a run may take, in minutes",
            &[],
        ),
        meta(
            "budget.per_card_usd",
            "Each card's budget in dollars, until similar past cards give a median",
            &[],
        ),
        meta(
            "budget.per_card_tokens",
            "Each card's budget in tokens",
            &[],
        ),
        meta(
            "budget.per_card_quota",
            "Each card's budget as a share of a subscription window",
            &[],
        ),
        meta(
            "budget.per_card_minutes",
            "Each card's budget in minutes",
            &[],
        ),
        meta(
            "budget.soft_stop",
            "Share of a budget at which the agent is asked to wrap up",
            &[],
        ),
        words(
            "budget.on_hit",
            "When a card hits its budget: ask you, extend it from the reserve, hand it off, or \
             escalate",
            &["ask", "extend", "handoff", "escalate"],
        ),
    ]);
    entries.extend(per_role_keys("handoff", HANDOFF));
    // /permissions
    entries.extend(per_role_keys("sandbox", SANDBOX));
    entries.extend([
        meta(
            "limits.protected_paths",
            "Paths no card may touch, as glob patterns, on top of .argonauts/**",
            &[],
        ),
        meta(
            "limits.dependency_allowlist",
            "Packages a plan may add without asking you",
            &[],
        ),
        meta(
            "limits.usd_per_day",
            "The most you spend a day across every run and repo, in dollars",
            &[],
        ),
        // /narrator
        words(
            "ui.narrator",
            "The narrator: off, on demand for /ask and s in /status, or live updates",
            &["off", "on_demand", "live"],
        ),
        meta(
            "narrator.model",
            "The model that narrates, any in the registry. Unset uses the default agent, \
             read-only",
            &[],
        ),
        meta(
            "narrator.interval",
            "At most one live update this often, such as \"45s\"",
            &[],
        ),
        meta(
            "narrator.spend_cap",
            "The most the narrator spends per run, such as \"0.50 usd\" or \"200k tokens\"",
            &[],
        ),
        // /theme
        words(
            "ui.theme",
            "The palette. Auto follows your terminal's background",
            &["auto", "dark", "light"],
        ),
        meta(
            "ui.accent",
            "The working line's accent color, as \"#rrggbb\". Unset is the warm gold",
            &[],
        ),
        words(
            "ui.color",
            "Color. Auto follows NO_COLOR and TERM=dumb",
            &["auto", "always", "never"],
        ),
        words(
            "ui.motion",
            "Reduced keeps spinners still and streams text a line at a time",
            &["normal", "reduced"],
        ),
        words(
            "ui.screen",
            "Fullscreen takes the whole terminal and leaves it as it was; inline prints into \
             its scrollback. Takes effect at the next start",
            &["fullscreen", "inline"],
        ),
        // /vim, /verbosity
        words(
            "ui.keys",
            "Key bindings: the usual keys, or vim keys for lists and the prompt",
            &["default", "vim"],
        ),
        words(
            "ui.verbosity",
            "How much the transcript shows",
            &["full", "normal", "quiet"],
        ),
        meta(
            "ui.keymap.*",
            "Binds an action to another key, such as crew_view = \"ctrl+left\"",
            &[],
        ),
    ]);
    entries.extend(later_keys());
    entries
}

/// The keys added for workflows, sessions, the panel, handoff debt, and front ends.
fn later_keys() -> Vec<SettingMeta> {
    vec![
        // /config workflow
        meta(
            "workflow",
            "The workflow a run starts from. Each preset sets its own, and /config workflow picks \
             another, which wins when saved where the preset is picked or in a later file",
            &[],
        ),
        meta(
            "workflows.lead_designs",
            "The lead picks or composes a workflow for each task. Off runs the preset's workflow \
             as it is",
            BOOL,
        ),
        meta(
            "workflows.allow_composed",
            "The lead may compose new workflows from the step kinds, which are checked before \
             they run",
            BOOL,
        ),
        meta(
            "workflows.max_depth",
            "How deep workflows may nest, at most 2",
            &[],
        ),
        meta(
            "workflows.adapt_at_phase_ends",
            "The lead may propose a change at a phase end, through the change request checkpoint",
            BOOL,
        ),
        words(
            "workflows.style",
            "How a crew run works: as a workflow, phases that end when they finish; as a goal, \
             attempts until a check from another company finds every criterion met; or as the \
             lead's CLI does",
            &["follow_lead", "workflow", "goal"],
        ),
        words(
            "workflows.style_by_lead.*",
            "What follow_lead picks for this CLI. Any CLI not listed leads a workflow",
            &["workflow", "goal"],
        ),
        meta(
            "workflows.final_check",
            "A check from another company judges the done-when list before Merge",
            BOOL,
        ),
        words(
            "understand.when",
            "When models from several companies read your request first: in every crew run, \
             when the lead decides, or never",
            &["crew", "lead", "off"],
        ),
        meta(
            "goal.checker",
            "Who checks a goal: other_company, or a worker by name",
            &[],
        ),
        meta(
            "ui.workflow.row_cost",
            "A cost column on the workflow view's agent rows, before the time",
            BOOL,
        ),
        meta(
            "sessions.rotate_at",
            "When a session starts fresh with a handoff note: a token count such as \"200k\", or \
             a share of the context window such as 0.9",
            &[],
        ),
        // The panel under the mode line
        words(
            "ui.panel.vendor_internals",
            "What an agent's own subagents or threads show: each one, a summary, or nothing",
            &["discovered", "summary", "hidden"],
        ),
        meta(
            "ui.panel.agents",
            "Also list running agents under each workflow row",
            BOOL,
        ),
        meta(
            "ui.panel.max_rows",
            "Rows in the panel under the mode line",
            &[],
        ),
        words(
            "ui.glyphs",
            "Symbols: unicode, or ascii for fonts that draw ● or ✔ at the wrong width",
            &["unicode", "ascii"],
        ),
        meta(
            "ui.ambiguous_width",
            "Cells for East Asian ambiguous-width characters",
            &["1", "2"],
        ),
        meta(
            "approvals.merge.rules.allow_debt",
            "Merge rules: whether auto may merge a card whose debt no strong enough model reviewed",
            BOOL,
        ),
        // Always-on agents and front ends
        words(
            "approvals.presence",
            "While an always-on agent runs as you: no check, a warning, or a passkey for every \
             approval",
            &["off", "warn", "required"],
        ),
        meta(
            "approvals.refuse_ancestor_env",
            "Variables that mark another agent's shell. A caller with one in its ancestry is \
             refused every approval",
            &[],
        ),
        words(
            "mcp.start_runs",
            "Whether front ends may start runs, in repos you trust",
            &["trusted-repos", "off"],
        ),
        meta(
            "mcp.guidance",
            "Front ends may send guidance. It never raises autonomy",
            BOOL,
        ),
        meta("mcp.pause", "Front ends may pause runs", BOOL),
        words(
            "mcp.share",
            "How much of a run front ends see: summaries, or status only. Never diffs, file \
             contents, prompts, or raw logs",
            &["summaries", "status-only"],
        ),
        meta(
            "mcp.run_budget",
            "The time budget of each run a front end starts, such as \"60m\"",
            &[],
        ),
        meta(
            "mcp.runs_per_day",
            "Runs front ends may start per day, across every repo",
            &[],
        ),
        meta(
            "notifications.chat.command",
            "A program and its arguments that sends you a chat message, with {text} as one whole \
             argument. Empty is off. Only your global settings may set it",
            &[],
        ),
        meta(
            "notifications.chat.on",
            "What sends a chat message: needs_you, finished, vendor_down",
            &[],
        ),
    ]
}

/// A leaf key the settings schema knows, with `*` for each map key.
struct SchemaLeaf {
    key: String,
    /// The field's doc comment as one line, or empty.
    description: String,
    /// The TOML text of each value an enum or a boolean takes. Empty when any value goes.
    allowed: Vec<String>,
}

/// Every leaf key of [`Settings`], including the ones the defaults leave unset. A key that takes
/// a word or a table, such as `native_fanout`, is a leaf and its table's keys are leaves too.
/// Arrays count as one value, as they do in `resolve`.
fn schema_leaves() -> Vec<SchemaLeaf> {
    let schema = schemars::schema_for!(Settings);
    let root = schema.as_value();
    let mut leaves = Vec::new();
    Schema { root }.walk("", root, None, &mut leaves);
    leaves
}

struct Schema<'a> {
    root: &'a Json,
}

impl Schema<'_> {
    fn walk(&self, key: &str, node: &Json, doc: Option<&str>, out: &mut Vec<SchemaLeaf>) {
        let branches = self.branches(node);
        let (tables, values): (Vec<&Json>, Vec<&Json>) =
            branches.into_iter().partition(|branch| is_table(branch));
        let values: Vec<&Json> = values.into_iter().filter(|value| !is_null(value)).collect();
        if !key.is_empty() && (tables.is_empty() || !values.is_empty()) {
            out.push(SchemaLeaf {
                key: key.to_owned(),
                description: doc.map(one_line).unwrap_or_default(),
                allowed: allowed(&values),
            });
        }
        let child = |name: &str| {
            if key.is_empty() {
                name.to_owned()
            } else {
                format!("{key}.{name}")
            }
        };
        for table in tables {
            if let Some(properties) = table.get("properties").and_then(Json::as_object) {
                for (name, property) in properties {
                    self.walk(&child(name), property, description(property), out);
                }
            }
            if let Some(map) = table
                .get("additionalProperties")
                .filter(|map| map.is_object())
            {
                // A map's own doc comment describes its entries too.
                self.walk(&child("*"), map, description(map).or(doc), out);
            }
        }
    }

    /// The alternatives a value may take, with references followed and unions flattened.
    fn branches<'a>(&'a self, node: &'a Json) -> Vec<&'a Json> {
        let node = self.follow(node);
        let unions: Vec<&Json> = ["anyOf", "oneOf", "allOf"]
            .iter()
            .filter_map(|union| node.get(*union).and_then(Json::as_array))
            .flatten()
            .collect();
        if unions.is_empty() {
            return vec![node];
        }
        unions
            .into_iter()
            .flat_map(|branch| self.branches(branch))
            .collect()
    }

    fn follow<'a>(&'a self, mut node: &'a Json) -> &'a Json {
        // Settings has no recursive types, so a chain of references ends; the limit guards it.
        for _ in 0..16 {
            let Some(target) = node.get("$ref").and_then(Json::as_str) else {
                break;
            };
            match target
                .strip_prefix('#')
                .and_then(|pointer| self.root.pointer(pointer))
            {
                Some(found) => node = found,
                None => break,
            }
        }
        node
    }
}

fn description(node: &Json) -> Option<&str> {
    node.get("description").and_then(Json::as_str)
}

/// A struct or a map. An empty struct, such as a checkpoint with no rules, has no keys at all.
fn is_table(node: &Json) -> bool {
    node.get("type").and_then(Json::as_str) == Some("object")
        || node.get("properties").is_some()
        || node
            .get("additionalProperties")
            .is_some_and(Json::is_object)
}

fn is_null(node: &Json) -> bool {
    node.get("type").and_then(Json::as_str) == Some("null")
}

/// The words and booleans a value may be, or none when one branch takes any value of its type.
fn allowed(values: &[&Json]) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for value in values {
        let texts: Vec<String> = if let Some(choices) = value.get("enum").and_then(Json::as_array) {
            choices.iter().filter_map(toml_text).collect()
        } else if let Some(choice) = value.get("const") {
            toml_text(choice).into_iter().collect()
        } else if is_boolean(value) {
            BOOL.iter().map(|word| (*word).to_owned()).collect()
        } else {
            return Vec::new();
        };
        for text in texts {
            if !found.contains(&text) {
                found.push(text);
            }
        }
    }
    found
}

/// `boolean`, or `["boolean", "null"]` for an optional one.
fn is_boolean(node: &Json) -> bool {
    match node.get("type") {
        Some(Json::String(kind)) => kind == "boolean",
        Some(Json::Array(kinds)) => {
            kinds.iter().any(|kind| kind == "boolean")
                && kinds.iter().all(|kind| kind == "boolean" || kind == "null")
        }
        _ => false,
    }
}

fn toml_text(value: &Json) -> Option<String> {
    match value {
        Json::String(word) => Some(quote(word)),
        Json::Bool(flag) => Some(flag.to_string()),
        Json::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// A doc comment's first paragraph as one line, without Markdown code marks or a final period.
fn one_line(doc: &str) -> String {
    let paragraph = doc.split("\n\n").next().unwrap_or_default();
    let line = paragraph.split_whitespace().collect::<Vec<_>>().join(" ");
    line.replace('`', "").trim_end_matches('.').to_owned()
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::GLOBAL_ONLY_KEYS;
    use toml::{Table, Value};

    use super::*;
    use crate::resolve::leaves;

    /// A real key for a catalog key, with each `*` filled in.
    fn concrete(key: &str) -> String {
        let mut segments: Vec<&str> = key.split('.').collect();
        for at in 0..segments.len() {
            if segments[at] == "*" {
                segments[at] = match (segments[0], at) {
                    (_, at) if at > 0 && segments[at - 1] == "replace" => "codex",
                    ("agents", _) => "codex",
                    ("endpoints", 1) => "ollama",
                    ("endpoints", _) => "X_TEAM",
                    ("roles", _) => "reviewer",
                    ("visibility", _) => "owner",
                    _ => "crew_view",
                };
            }
        }
        segments.join(".")
    }

    /// Settings with `key` set to the TOML text `value`, or why they don't deserialize.
    fn settings_with(key: &str, value: &str) -> Result<Table, String> {
        let value: Value = format!("value = {value}")
            .parse::<Table>()
            .map_err(|err| err.to_string())?
            .remove("value")
            .ok_or("no value")?;
        let mut table = Table::try_from(Settings::default()).map_err(|err| err.to_string())?;
        let segments: Vec<&str> = key.split('.').collect();
        let mut current = &mut table;
        for segment in &segments[..segments.len() - 1] {
            current = current
                .entry((*segment).to_owned())
                .or_insert_with(|| Value::Table(Table::new()))
                .as_table_mut()
                .ok_or("not a table")?;
        }
        current.insert(segments[segments.len() - 1].to_owned(), value);
        let settings: Settings = Value::Table(table)
            .try_into()
            .map_err(|err: toml::de::Error| err.to_string())?;
        Table::try_from(settings).map_err(|err| err.to_string())
    }

    /// A value to try for keys whose pickers offer no list.
    fn sample(key: &str) -> &'static str {
        match key {
            "default_agent" | "approvals.approver.model" | "approvals.approver.second_model" => {
                "\"codex/gpt-x\""
            }
            "roles.*.workers"
            | "crew"
            | "roles.*.handoff.fallback"
            | "handoff.fallback"
            | "handoff.replace.*.with"
            | "roles.*.handoff.replace.*.with" => "[\"codex\"]",
            "agents.*.model" => "\"opus\"",
            "agents.*.binary" => "\"/opt/codex/bin/codex\"",
            "agents.*.version_pin" => "\"0.158.0\"",
            "endpoints.*.base_url" => "\"http://localhost:11434/v1\"",
            "endpoints.*.api_key_env" => "\"OLLAMA_API_KEY\"",
            "approvals.approver.model_rules" => "\"approval-rules.md\"",
            "budget.run_tokens" | "budget.per_card_tokens" => "200000",
            "approvals.merge.rules.max_combined_diff_lines"
            | "approvals.escalation.rules.max_extra_attempts"
            | "handoff.race_count"
            | "roles.*.handoff.race_count"
            | "handoff.max_handoffs"
            | "roles.*.handoff.max_handoffs"
            | "workflows.max_depth"
            | "ui.panel.max_rows"
            | "mcp.runs_per_day" => "2",
            "limits.protected_paths" | "limits.dependency_allowlist" => "[\"docs/**\"]",
            "approvals.refuse_ancestor_env" => "[\"OTHER_AGENT_SHELL\"]",
            "notifications.chat.command" => "[\"/usr/local/bin/send-chat\", \"{text}\"]",
            "notifications.chat.on" => "[\"finished\"]",
            "narrator.model" | "handoff.summarizer" | "roles.*.handoff.summarizer" => {
                "\"ollama/llama3.2\""
            }
            "narrator.interval"
            | "roles.*.handoff.ask_timeout"
            | "handoff.ask_timeout"
            | "mcp.run_budget" => "\"30s\"",
            "narrator.spend_cap" => "\"0.50 usd\"",
            "ui.accent" => "\"#d4a017\"",
            "ui.keymap.*" => "\"ctrl+left\"",
            "goal.checker" => "\"codex\"",
            "approvals.ask_you_about" | "understand.always_ask" => "[\"security\"]",
            "workflow" => "\"solo\"",
            "sessions.rotate_at" => "\"200k\"",
            _ => "0.5",
        }
    }

    #[test]
    fn every_named_key_is_real_and_its_values_are_accepted() {
        for meta in named() {
            let key = concrete(&meta.key);
            let values: Vec<&str> = if meta.allowed.is_empty() {
                vec![sample(&meta.key)]
            } else {
                meta.allowed.iter().map(String::as_str).collect()
            };
            for value in values {
                let table = settings_with(&key, value)
                    .unwrap_or_else(|err| panic!("{key} = {value}: {err}"));
                assert!(
                    leaves(&table).iter().any(|(leaf, _)| leaf == &key),
                    "{key} isn't a settings key"
                );
            }
            assert!(!meta.description.is_empty(), "{}", meta.key);
            assert!(!meta.description.ends_with('.'), "{}", meta.key);
        }
    }

    #[test]
    fn settings_catalog_covers_every_key() {
        let catalog = settings_catalog();
        let keys: BTreeSet<&str> = catalog.iter().map(|meta| meta.key.as_str()).collect();
        assert_eq!(keys.len(), catalog.len(), "a key is listed twice");

        // The schema knows every key, set by default or not, and the defaults agree with it.
        let schema: Vec<String> = schema_leaves().into_iter().map(|leaf| leaf.key).collect();
        for key in &schema {
            assert!(keys.contains(key.as_str()), "{key}");
        }
        let named: Vec<String> = named().into_iter().map(|meta| meta.key).collect();
        for (key, _) in OWN_WORDS {
            let key = (*key).to_owned();
            assert!(schema.contains(&key) && !named.contains(&key), "{key}");
        }
        let defaults = Table::try_from(Settings::default()).unwrap();
        for (key, _) in leaves(&defaults) {
            let matching: Vec<&str> = keys
                .iter()
                .copied()
                .filter(|pattern| fits(pattern, &key))
                .collect();
            assert_eq!(matching.len(), 1, "{key}: {matching:?}");
        }
        for key in [
            "roles.*.count",
            "agents.*.price.input_per_mtok",
            "agents.*.price.output_per_mtok",
            "agents.*.price.cached_input_per_mtok",
            "triage.force",
            "triage.all_hands",
            "handoff.strategy",
            "handoff.race_count",
            "handoff.on_return",
            "handoff.checkpoint",
            "handoff.merge_rule",
            "handoff.unknown_tier",
            "handoff.max_handoffs",
            "handoff.replace.*.with",
            "handoff.replace.*.strategy",
            "handoff.replace.*.on_return",
            "handoff.context",
            "handoff.summarizer",
            "handoff.replace.*.context",
            "roles.*.handoff.strategy",
            "roles.*.handoff.replace.*.with",
            "roles.*.handoff.replace.*.on_return",
            "roles.*.handoff.context",
            "roles.*.handoff.summarizer",
            "roles.*.handoff.replace.*.context",
            "roles.*.native_fanout",
            "roles.*.native_fanout.max",
            "sessions.rotate_at",
            "ui.panel.vendor_internals",
            "ui.panel.agents",
            "ui.panel.max_rows",
            "ui.glyphs",
            "ui.ambiguous_width",
            "workflow",
            "workflows.lead_designs",
            "workflows.allow_composed",
            "workflows.max_depth",
            "workflows.adapt_at_phase_ends",
            "approvals.merge.rules.allow_debt",
            "approvals.presence",
            "approvals.refuse_ancestor_env",
            "mcp.start_runs",
            "mcp.share",
            "mcp.run_budget",
            "notifications.chat.command",
            "notifications.chat.on",
            "endpoints.*.header_env.*",
            "ui.keymap.*",
            "sandbox.agents_see_other_copies",
            "sandbox.agents_commit",
            "roles.*.sandbox.mode",
            "roles.*.sandbox.network",
            "roles.*.sandbox.agents_see_other_copies",
            "roles.*.sandbox.agents_commit",
        ] {
            assert!(keys.contains(key), "{key}");
        }
        assert!(!keys.iter().any(|key| key.contains("claude-code")));

        for meta in &catalog {
            let key = &meta.key;
            let description = &meta.description;
            assert!(!description.is_empty(), "{key} has no description");
            assert!(
                !description.ends_with('.')
                    && !description.starts_with('.')
                    && !description.contains(['\n', '`']),
                "{key}: {description:?}"
            );
            assert_eq!(meta.safety, is_safety_key(key), "{key}");
            assert_eq!(setting_meta(&concrete(key)).as_ref(), Some(meta), "{key}");
            if GLOBAL_ONLY_KEYS.iter().any(|only| key_matches(only, key)) {
                assert_eq!(meta.default_scope, SettingScope::Global, "{key}");
                assert!(meta.safety, "{key}");
            }
        }

        let allowed = |key: &str| setting_meta(key).unwrap().allowed;
        for key in [
            "mcp.pause",
            "triage.all_hands",
            "handoff.checkpoint",
            "roles.owner.blind",
            "ui.panel.agents",
        ] {
            assert_eq!(allowed(key), BOOL, "{key}");
        }
        assert_eq!(allowed("triage.force"), ["\"solo\"", "\"crew\""]);
        assert_eq!(allowed("ui.ambiguous_width"), ["1", "2"]);
        assert_eq!(
            allowed("roles.owner.handoff.replace.codex.on_return"),
            [
                "\"ask\"",
                "\"keep\"",
                "\"review\"",
                "\"hand_back\"",
                "\"redo\""
            ]
        );
        let contexts = [
            "\"fresh\"",
            "\"note\"",
            "\"note+transcript\"",
            "\"note+summary\"",
        ];
        assert_eq!(allowed("handoff.context"), contexts);
        assert_eq!(allowed("roles.owner.handoff.context"), contexts);
        assert_eq!(allowed("handoff.replace.codex.context"), contexts);
        let fanout = allowed("roles.owner.native_fanout");
        assert!(fanout.contains(&"\"off\"".to_owned()), "{fanout:?}");
        for open in [
            "sessions.rotate_at",
            "workflow",
            "notifications.chat.on",
            "crew",
            "handoff.summarizer",
        ] {
            assert!(allowed(open).is_empty(), "{open}");
        }
        assert!(setting_meta("notifications.chat.command").unwrap().safety);
        assert!(setting_meta("handoff.allow_api_billing").unwrap().safety);
        assert!(!setting_meta("handoff.strategy").unwrap().safety);
        assert!(
            setting_meta("endpoints.ollama.header_env.X_TEAM")
                .unwrap()
                .safety
        );
    }

    #[test]
    fn each_key_is_listed_once_and_every_default_key_is_there() {
        let catalog = settings_catalog();
        let keys: BTreeSet<&str> = catalog.iter().map(|meta| meta.key.as_str()).collect();
        assert_eq!(keys.len(), catalog.len(), "a key is listed twice");
        let defaults = Table::try_from(Settings::default()).unwrap();
        for (key, _) in leaves(&defaults) {
            assert!(setting_meta(&key).is_some(), "{key}");
        }
        for key in [
            "agents.*.binary",
            "runs.remind_parked_days",
            "roles.*.on_demand",
        ] {
            assert!(keys.contains(key), "{key}");
        }
    }

    #[test]
    fn default_scopes_are_the_ones_each_setting_is_given() {
        let scope = |key: &str| setting_meta(key).map(|meta| meta.default_scope);
        for key in [
            "default_agent",
            "agents.codex.model",
            "agents.codex.effort",
            "roles.reviewer.workers",
            "roles.owner.effort",
        ] {
            assert_eq!(scope(key), Some(SettingScope::ProjectLocal), "{key}");
        }
        for key in [
            "preset",
            "crew",
            "approvals.merge.mode",
            "budget.run_usd",
            "roles.owner.handoff.mode",
            "sandbox.network",
            "limits.protected_paths",
            "agents.codex.enabled",
        ] {
            assert_eq!(scope(key), Some(SettingScope::Project), "{key}");
        }
        for key in [
            "agents.codex.auth",
            "agents.codex.binary",
            "endpoints.ollama.base_url",
            "ui.theme",
            "ui.keys",
            "ui.keymap.crew_view",
            "narrator.model",
            "notifications.os",
            "limits.usd_per_day",
            "limits.auto_merges_per_day",
        ] {
            assert_eq!(scope(key), Some(SettingScope::Global), "{key}");
        }
    }

    #[test]
    fn sandbox_keys_are_safety_keys_for_every_role_and_for_one() {
        let modes = ["\"os\"", "\"container\"", "\"tool\"", "\"none\""];
        for key in ["sandbox.mode", "roles.reviewer.sandbox.mode"] {
            let meta = setting_meta(key).unwrap();
            assert_eq!(meta.allowed, modes, "{key}");
            assert!(meta.safety, "{key}");
            assert_eq!(meta.default_scope, SettingScope::Project, "{key}");
        }
        for name in ["network", "agents_see_other_copies", "agents_commit"] {
            let every = setting_meta(&format!("sandbox.{name}")).unwrap();
            let one = setting_meta(&format!("roles.owner.sandbox.{name}")).unwrap();
            assert_eq!(one.key, format!("roles.*.sandbox.{name}"));
            for meta in [&every, &one] {
                assert_eq!(meta.allowed, BOOL, "{}", meta.key);
                assert!(meta.safety, "{}", meta.key);
            }
            assert!(every.description.ends_with("A role's own setting wins"));
        }
    }

    #[test]
    fn real_keys_find_their_entry() {
        let meta = setting_meta("roles.reviewer.handoff.allow_api_billing").unwrap();
        assert_eq!(meta.key, "roles.*.handoff.allow_api_billing");
        assert!(meta.safety);
        let meta = setting_meta("approvals.merge.mode").unwrap();
        assert!(meta.safety);
        assert_eq!(meta.allowed, ["\"supervised\"", "\"shadow\"", "\"auto\""]);
        assert!(!setting_meta("ui.keys").unwrap().safety);
        assert!(
            setting_meta("preset")
                .unwrap()
                .allowed
                .contains(&"\"coordinated-crew\"".to_owned())
        );
        assert_eq!(setting_meta("nothing.here"), None);
    }

    #[test]
    fn the_catalog_lists_every_default_key() {
        let catalog = settings_catalog();
        let find = |key: &str| catalog.iter().find(|meta| meta.key == key);
        assert!(find("ui.keys").is_some_and(|meta| !meta.safety));
        assert!(find("approvals.merge.mode").is_some_and(|meta| meta.safety));
        assert!(find("runs.remind_parked_days").is_some());
        assert!(find("limits.parked_run_expires_days").is_none());
    }
}
