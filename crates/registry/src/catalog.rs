//! What `/config` and the settings pickers know about each key: a description, the values a
//! picker offers, where a change is saved by default, and whether it's a safety key.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use argonauts_core::input::EFFORT_LEVELS;
use argonauts_core::settings::handoff::CrewRole;
use argonauts_core::settings::ui::Theme;
use argonauts_core::settings::{SettingMeta, SettingScope, Settings};
use serde_json::Value as Json;

use crate::resolve::{is_safety_key, key_matches};

/// Keys that are personal choices in a repo, saved just for you by default.
const JUST_YOU: &[&str] = &[
    "main_agent",
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
    "switch.*",
    "limits.spend_per_day",
    "limits.auto_merges_per_day",
];

/// The checkpoints `/approvals` covers, as they appear under `approvals.`.
const CHECKPOINTS: &[(&str, &str)] = &[
    ("goal", "The goal approval"),
    ("plan", "The plan approval"),
    ("merge", "The merge approval"),
    ("stuck", "A stuck task"),
    ("limit", "More limit from the run's reserve"),
    (
        "change_request",
        "A change request no other task's worker can decide",
    ),
    (
        "workflow_promotion",
        "Promoting a change to a learned workflow",
    ),
];

const BOOL: &[&str] = &["true", "false"];

const ON_RETURN: &[&str] = &["ask", "keep", "review", "hand_back", "redo"];
const STRATEGIES: &[&str] = &["first", "spread"];
const CONTEXTS: &[&str] = &["fresh", "note", "note+transcript", "note+summary"];

/// What `/handoff` edits, below `handoff.` for every role and below `roles.*.handoff.` for one.
/// Words are quoted for the picker; an empty list takes any value of the type.
const HANDOFF: &[(&str, &str, Choices)] = &[
    (
        "mode",
        "When an agent stops, such as at a usage limit: ask you, hand off to the next fallback, \
         or wait for the agent",
        Choices::Words(&["ask", "auto", "wait"]),
    ),
    (
        "order",
        "Who takes over, in order: an agent, or a table with its own rules. With no list \
         anywhere, the other installed and logged-in CLIs",
        Choices::Any,
    ),
    (
        "ask_timeout",
        "How long the fallback question waits before handing off by itself, or \"none\"",
        Choices::Any,
    ),
    (
        "allow_api_billing",
        "Whether a fallback may move work onto a paid API without asking you",
        Choices::Bool,
    ),
    (
        "return_to_original",
        "Move the task back when the original agent returns, the older spelling of on_return = \
         \"hand_back\"",
        Choices::Bool,
    ),
    (
        "broken_work",
        "What the new worker does with broken work in progress: continue from it, or restart",
        Choices::Words(&["continue", "restart"]),
    ),
    (
        "wrap_up",
        "An agent finishing on its CLI's wrap-up allowance at a limit: let it finish, or hand \
         its task off at once to save the allowance",
        Choices::Words(&["let_finish", "hand_off"]),
    ),
    (
        "strategy",
        "Who takes a stopped agent's tasks, one fallback each: the first that's up, or spread \
         across them",
        Choices::Words(STRATEGIES),
    ),
    (
        "together",
        "How many fallbacks take over at once. 2 or more do the same task side by side",
        Choices::Any,
    ),
    (
        "pick",
        "How results combine when several take over at once: the first to pass your checks \
         wins, the judge picks the best, or every finding is kept",
        Choices::Words(&["first_passing", "judge", "merge"]),
    ),
    (
        "on_return",
        "When the original agent returns: ask you, keep the new worker, review its work, hand \
         the task back, or redo it",
        Choices::Words(ON_RETURN),
    ),
    (
        "checkpoint",
        "Tag the task's state at every fallback, so you can go back to it",
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
        "max_fallbacks",
        "Fallbacks per task before it comes to you",
        Choices::Any,
    ),
    (
        "context",
        "What the fallback is told: the task only (fresh), the fallback note, the note and the \
         old session's transcript, or the note and a summary of it",
        Choices::Words(CONTEXTS),
    ),
    (
        "summarizer",
        "Who summarizes the old session for note+summary: cli/model or an endpoint. Unset \
         uses the fallback's CLI with its cheapest model",
        Choices::Any,
    ),
    (
        "replace.*.with",
        "Who replaces this agent, in order: an agent, or a table with its own rules",
        Choices::Any,
    ),
    (
        "replace.*.strategy",
        "How this agent's tasks go to its fallbacks: the first that's up, or spread",
        Choices::Words(STRATEGIES),
    ),
    (
        "replace.*.on_return",
        "When this agent returns: ask you, keep, review, hand back, or redo",
        Choices::Words(ON_RETURN),
    ),
    (
        "replace.*.context",
        "What this agent's fallbacks are told: fresh, note, note+transcript, or note+summary",
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
        "agents_see_other_worktrees",
        "Whether an agent can read the other agents' worktrees of the repo, not only its own",
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
        "Extra arguments for the CLI's program on every run",
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
        "approvals.rules.workflow_promotion.change_classes",
        "Learned workflows: the classes of change auto may promote, such as adding a checklist item",
    ),
    (
        "limits.history_min",
        "Past tasks of a task type needed before their median sets a task's limit",
    ),
    (
        "limits.lines.goal_and_plan",
        "Share of the run limit for the goal and the plan",
    ),
    (
        "limits.lines.contracts",
        "Share of the run limit for interface and acceptance-test tasks",
    ),
    (
        "limits.lines.tasks",
        "Share of the run limit for build tasks",
    ),
    ("limits.lines.review", "Share of the run limit for review"),
    (
        "limits.lines.experiments",
        "Share of the run limit for experiments",
    ),
    (
        "limits.lines.integration",
        "Share of the run limit for integration",
    ),
    (
        "limits.lines.reserve",
        "Share of the run limit held in reserve",
    ),
    (
        "concurrency.max_sessions",
        "Most agent sessions running at once",
    ),
    (
        "concurrency.max_per_agent",
        "Most sessions running at once with one agent",
    ),
    ("critique.rounds", "Critique rounds. 0 turns critique off"),
    ("critique.checklist", "What every critique checks"),
    (
        "divide.acceptance_tests_by",
        "Who writes each task's acceptance tests: its reviewer, or nobody",
    ),
    ("divide.max_tasks", "Most tasks a plan may divide into"),
    (
        "divide.predictions",
        "Each worker writes predictions on its task as probabilities, such as passing the checks \
         on the first try",
    ),
    (
        "drafting.by",
        "Who drafts plans: the crew, or the orchestrator",
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
        "Runs a promoted workflow change is watched for as a canary",
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
        "checks.timeout",
        "How long one check command may run, such as \"30m\"",
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
        "How many models from other providers compare the readings",
    ),
    (
        "understand.always_ask",
        "Topics that are always a question, never a majority's silent assumption",
    ),
    (
        "goal.until",
        "The goal: keep going until this is true. /goal sets it for the conversation",
    ),
    (
        "goal.default",
        "Whether a request gets a goal you didn't set: off, or from-request, where the main \
         agent says what done means and the work goes on until it holds",
    ),
    (
        "goal.check",
        "What checks a goal: auto takes a command when the goal names one and else the judge; \
         command, judge, or both",
    ),
    (
        "goal.stuck_after",
        "Rounds in a row that end on the same failure before a goal changes approach once, then \
         stops and says why",
    ),
    (
        "goal.max_rounds",
        "Most rounds a goal goes. Unset, only the goal holding, you, or being stuck ends it",
    ),
    (
        "roles.*.goal",
        "Whether the role works each task as a goal, and who keeps it going: cli for the CLI's \
         own goal, which Codex has, argonauts, or off",
    ),
    (
        "health.window",
        "The span the health counts look back over, such as \"2m\"",
    ),
    (
        "health.down_after",
        "Overload or server errors within the window that mark an agent down",
    ),
    (
        "health.down_sessions",
        "Sessions those errors must come from before an agent counts as down",
    ),
    (
        "judging.method",
        "How plans are judged: cross-judging, one judge, a vote, the orchestrator, or you",
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
        "The most workflow evolution may spend a month, in dollars",
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
        "Most subagents a CLI session may start",
    ),
    (
        "notifications.bell",
        "Ring the terminal bell when something needs you",
    ),
    (
        "review.assign",
        "How reviewers are assigned: a ring, at random, by the orchestrator, or fixed",
    ),
    ("review.per_task", "Reviewers per task"),
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
        "How long this role's session lasts: a task, a run, or the project",
    ),
    (
        "roles.*.native_fanout",
        "Subagents this role's CLI may start: on, off, or a table with a max. On also lets it \
         run a workflow of its own, where its CLI has one",
    ),
    (
        "roles.*.native_fanout.max",
        "Most subagents this role's CLI session may start",
    ),
    (
        "routing.algorithm",
        "How tasks find workers: a bandit that learns, round robin, the orchestrator, or fixed \
         workers",
    ),
    (
        "routing.no_explore_labels",
        "Task labels that never go to a less-proven agent",
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
        "Hosts agents may reach through the proxy, on top of their CLI's API hosts",
    ),
    (
        "sandbox.untrusted_repos",
        "Repos you haven't trusted run in a container, or not at all",
    ),
    (
        "sessions.lifetime",
        "How long an agent session lasts: a task, a run, or the project. A role can override it",
    ),
    (
        "solo.kind",
        "Solo you choose: direct works in your checkout like its CLI, checked works in a \
         worktree with checks and a merge",
    ),
    (
        "experiments.enabled",
        "Small experiments settle what the plans disagree on",
    ),
    (
        "ultracode",
        "Ultracode: a workflow for every real request, small ones included. In solo, Claude \
         Code runs its own, and on Codex or Antigravity CLI one runs on that same model. \
         Questions are still just answered",
    ),
    (
        "solo_or_crew.use_learned",
        "A matching learned workflow decides the mode",
    ),
    (
        "solo_or_crew.otherwise",
        "Between the thresholds: solo, the crew, or solo then the crew",
    ),
    (
        "solo_or_crew.solo_max_files",
        "Solo is picked at this many files or fewer",
    ),
    (
        "solo_or_crew.solo_requires_tests",
        "Solo also needs the repo to have tests",
    ),
    (
        "solo_or_crew.crew_min_files",
        "The crew is picked at this many files or more",
    ),
    (
        "solo_or_crew.crew_labels",
        "Labels that always get the crew, such as security",
    ),
    (
        "solo_or_crew.solo_kind",
        "Solo when the rules pick it: checked, so it can grow into a crew run, direct, or as \
         solo.kind says",
    ),
    (
        "solo_or_crew.read_instructions",
        "Follow what a request says about how to run it, such as who coordinates or reviews",
    ),
    ("ui.bell", "Let the app ring the terminal bell"),
    (
        "ui.recap_after",
        "How long a turn runs before a recap follows it, such as \"2m\"",
    ),
    (
        "ui.takeover",
        "Where an agent's own screen opens when you take over: here, a tmux split, or a new tab",
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
        "Minutes a task may run. A soft stop comes first. Unset, there is no limit",
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
        // Not a key the settings hold: a file may write one list for the whole crew, and the
        // resolver seats everyone in it (`settings::crew::spell_out`).
        entries.push(meta(
            "crew",
            "One list for the whole crew: everyone in it may work, review and judge",
            &[],
        ));
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
/// The keys of a table every role shares and of each role's own: `roles.*.sandbox` for the
/// roles, and `fallback.<role>` for each of the crew's five.
fn per_role_keys(section: &str, table: &[(&str, &str, Choices)]) -> Vec<SettingMeta> {
    let mut entries = Vec::new();
    let mut prefixes = vec![(section.to_owned(), true)];
    match section {
        "fallback" => prefixes.extend(
            CrewRole::ALL
                .into_iter()
                .map(|role| (format!("fallback.{}", role.as_str()), false)),
        ),
        _ => prefixes.push((format!("roles.*.{section}"), false)),
    }
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
            "main_agent",
            "Who you chat with, and who works solo: a CLI, or cli/model",
            &[],
        ),
        meta(
            "roles.*.workers",
            "Who fills this role, in order. Empty lets the router pick from the crew",
            &[],
        ),
        meta(
            "agents.*.model",
            "The model this CLI runs unless a role names one. Unset uses the CLI's default",
            &[],
        ),
        // /effort
        words(
            "agents.*.effort",
            "Reasoning effort for this CLI. Unset uses the CLI's default",
            EFFORT_LEVELS,
        ),
        words(
            "roles.*.effort",
            "Reasoning effort for this role",
            EFFORT_LEVELS,
        ),
        // /preset. The picker adds your own presets from `presets_in`.
        words(
            "workflow",
            "How the crew works: council plans and judges as peers, orchestrator has one \
             orchestrator",
            &presets,
        ),
        // /agents
        meta(
            "crew.orchestrator",
            "Who plans and delegates: \"main\" for the agent you talk to, or a model of its own",
            &[],
        ),
        meta(
            "crew.workers",
            "Who does the tasks, in order: the first that's available",
            &[],
        ),
        meta(
            "crew.reviewers",
            "Who checks a result, in order, never one their own provider made",
            &[],
        ),
        meta("crew.judge", "Who decides and signs off, in order", &[]),
        meta("agents.*.enabled", "Whether this CLI may work at all", BOOL),
        words(
            "agents.*.auth",
            "Your own login (subscription), or an API key from the environment (api-key)",
            &["subscription", "api-key"],
        ),
        meta(
            "agents.*.binary",
            "The CLI's program, as an absolute path outside the repo, when it isn't on PATH",
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
        meta(
            "approvals.autopilot",
            "Autopilot for every approval a run opens that its own mode leaves to auto: \
             nothing waits for you there. One you keep supervised or in auto preview stays yours. \
             Off, each approval keeps its own mode",
            BOOL,
        ),
        meta(
            "approvals.answers",
            "Who answers the crew's questions, Understand's open questions and the go-ahead, \
             and on autopilot the agents' questions meant for you: you, the main agent, or a model you \
             pick such as codex/gpt-5.6, whose answers become assumptions you can review. Unset, \
             the main agent in auto mode and you otherwise. Set to the main agent, it answers in every mode. \
             A model answers on autopilot only, where nothing waits for you; elsewhere the mode \
             decides. Autopilot never has you answer: the model you picked does, else the main agent",
            &[],
        ),
        meta(
            "approvals.decides",
            "Who settles what your rules can't decide: judge, the cheapest model you can run \
             from another provider than the work it judges, or a model you pick such as \
             antigravity-cli/gemini-3.6-flash, which judges in its place",
            &[],
        ),
        meta(
            "approvals.ask_you_about",
            "Topics that come to you even when the main agent answers, matched without case: a \
             question's tag (scope, security, data, cost, detail) or its topic, such as \"log \
             level\", and \"go-ahead\", \"workflow change\", \"stuck goal\", or \"keep \
             going\". Over limit always comes to you. Autopilot answers them too, since \
             nothing waits for you there",
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
                "{label}: supervised waits for you, auto preview also records what auto would do, \
                 auto lets your settings decide, and autopilot never waits: what they can't \
                 approve ends safely and is reported"
            ),
            &["supervised", "auto_preview", "auto", "autopilot"],
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
        // Only a merge opens a pull request, and a stuck task or a limit has nothing to send
        // back. A promotion is decided outside runs, where autopilot never reaches.
        let at_hard_limit: &[&str] = match *checkpoint {
            "workflow_promotion" => continue,
            "merge" => &["stop", "send_back", "open_pull_request", "park"],
            "stuck" | "limit" => &["stop", "park"],
            _ => &["stop", "send_back", "park"],
        };
        entries.push(words(
            &format!("approvals.{checkpoint}.at_hard_limit"),
            &format!(
                "{label}: what a hard limit does on autopilot, where no one is there to ask: \
                 stop ends that part safely and keeps its work, send_back runs another round, \
                 open_pull_request opens one for you, and park leaves the task for you"
            ),
            at_hard_limit,
        ));
    }
    entries.extend([
        meta(
            "approvals.rules.plan.max_score_spread",
            "Plan rules: the largest gap between one judge's two scores, out of 10",
            &[],
        ),
        meta(
            "approvals.rules.plan.min_winner_margin",
            "Plan rules: how far ahead of the runner-up the winning plan must be",
            &[],
        ),
        meta(
            "approvals.rules.merge.min_test_coverage_of_changed_lines",
            "Merge rules: the share of changed lines the tests must cover, from 0 to 1, where \
             coverage is measured",
            &[],
        ),
        meta(
            "approvals.rules.merge.max_changed_lines",
            "Merge rules: the most lines the run's combined diff may have",
            &[],
        ),
        meta(
            "approvals.rules.stuck.max_extra_attempts",
            "Stuck-task rules: extra attempts auto may grant, by an agent from a provider that \
             hasn't tried",
            &[],
        ),
        meta(
            "approvals.rules.limit.use_reserve",
            "Limit rules: whether auto may release the run's reserve, never above a ceiling",
            BOOL,
        ),
        meta(
            "approvals.approver.model_rules",
            "Your rules for the approver model, a file in .argonauts/",
            &[],
        ),
        meta(
            "approvals.approver.different_provider",
            "The approver must come from a provider no author or worker used",
            BOOL,
        ),
        meta(
            "approvals.approver.second_model",
            "A second approver, from another agent, that must also approve",
            &[],
        ),
        meta(
            "approvals.spot_check_sample",
            "Share of auto decisions queued for your spot check, from 0 to 1",
            &[],
        ),
        // /limits. The four you are most likely to set come first.
        meta(
            "limits.spend_per_run",
            "The most a run may spend, in dollars. Unset, there is no limit",
            &[],
        ),
        meta(
            "limits.spend_per_day",
            "The most you spend a day across every run and repo, in dollars. Unset, there is \
             no limit",
            &[],
        ),
        meta(
            "limits.plan_usage",
            "How full a provider's plan window may get before its agents stop being used, \
             from 0 to 1. Unset, a plan is used until it runs out",
            &[],
        ),
        words(
            "limits.when_reached",
            "When a limit you set is reached: the fallbacks take over, or the work stops",
            &["fallback", "stop"],
        ),
        meta("limits.run_tokens", "The most tokens a run may use", &[]),
        meta(
            "limits.run_plan_usage",
            "The most of a subscription window a run may use, from 0 to 1",
            &[],
        ),
        meta(
            "limits.run_minutes",
            "The longest a run may take, in minutes",
            &[],
        ),
        meta(
            "limits.per_task_usd",
            "Each task's limit in dollars, until similar past tasks give a median",
            &[],
        ),
        meta("limits.per_task_tokens", "Each task's limit in tokens", &[]),
        meta(
            "limits.per_task_plan_usage",
            "Each task's limit as a share of a subscription window",
            &[],
        ),
        meta(
            "limits.per_task_minutes",
            "Each task's limit in minutes",
            &[],
        ),
        meta(
            "limits.soft_stop",
            "Share of a limit at which the agent is asked to wrap up",
            &[],
        ),
    ]);
    entries.extend(per_role_keys("fallback", HANDOFF));
    // /permissions
    entries.extend(per_role_keys("sandbox", SANDBOX));
    entries.extend([
        meta(
            "limits.protected_paths",
            "Paths no task may touch, as glob patterns, on top of .argonauts/**",
            &[],
        ),
        meta(
            "limits.dependency_allowlist",
            "Packages a plan may add without asking you",
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
            "The model that narrates, any in the registry. Unset uses the main agent, \
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
            "The palette. Auto follows your terminal's background; dark and light come in \
             daltonized and ANSI forms too, and custom:<name> reads a theme file",
            &Theme::BUILT_IN,
        ),
        meta(
            "ui.accent",
            "The working line's accent color and its shimmer, as \"#rrggbb\". Unset is the warm \
             gold",
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
        // /config phases
        meta(
            "phases",
            "The phases a run starts from. Each workflow sets its own, and /config phases picks \
             another, which wins when saved where the workflow is picked or in a later file",
            &[],
        ),
        meta(
            "mods.commands",
            "Whether a mod file's command may run. Off, mods that read a command draw nothing \
             and say why; mods with a built-in source are untouched",
            BOOL,
        ),
        meta(
            "workflow_size",
            "How big a workflow the orchestrator plans when your request doesn't say: small \
             (under 5 agents), medium (under 10), large, or none for no guideline. A guideline, \
             as in Claude Code: a request can ask for another size",
            &["\"small\"", "\"medium\"", "\"large\"", "\"none\""],
        ),
        words(
            "switch.carry",
            "What a main model on another CLI starts from: a summary the model you leave writes \
             with the recent exchanges in full, the recent exchanges only, or all of them. Each \
             also gets the transcript's path",
            &["summary", "recent", "all"],
        ),
        meta(
            "switch.recent",
            "How many of the latest exchanges a main model on another CLI gets word for word",
            &[],
        ),
        meta(
            "workflows.orchestrator_designs",
            "The main agent sizes the workflow's steps for each task, with more or fewer agents at a \
             step or a step left out, and says what it changed. Off runs it as it is, and \
             \"any\" lets the main agent run another workflow or compose one",
            &["true", "false", "\"any\""],
        ),
        meta(
            "workflows.allow_composed",
            "The main agent may compose new workflows from the step kinds, which are checked before \
             they run",
            BOOL,
        ),
        meta(
            "workflows.max_depth",
            "How deep workflows may nest, at most 2: with 2 a workflow's script may run another \
             as a step, and with 1 none may",
            &[],
        ),
        meta(
            "workflows.adapt_at_phase_ends",
            "The main agent may propose a change at a phase end, through the change request approval",
            BOOL,
        ),
        meta(
            "workflows.final_check",
            "A check from another provider judges the done-when list before Merge",
            BOOL,
        ),
        words(
            "understand.when",
            "When models from several providers read your request first: in every crew run, \
             when the main agent decides, or never",
            &["crew", "main", "off"],
        ),
        meta(
            "goal.checker",
            "Who the judge of a goal is: other_provider, or an agent by name",
            &[],
        ),
        meta(
            "ui.workflow.row_cost",
            "A cost column on the workflow view's agent rows, before the time",
            BOOL,
        ),
        meta(
            "sessions.context",
            "How much context a session may hold: \"largest\" is the largest window each CLI \
             offers for the model, and a token count such as \"200k\" lowers it for cheaper turns",
            &[],
        ),
        meta(
            "sessions.rotate_at",
            "When a session starts fresh with a fallback note: a share of its context window \
             such as 0.9, which is near its end, or a token count such as \"200k\"",
            &[],
        ),
        // The panel under the mode line
        words(
            "ui.panel.subagents",
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
            "approvals.rules.merge.allow_debt",
            "Merge rules: whether auto may merge a task whose debt no strong enough model reviewed",
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
            "mcp.run_limit",
            "The time limit of each run a front end starts, such as \"60m\"",
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
            "What sends a chat message: needs_you, finished, agent_down",
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

    /// The retired words, as a setting's key, value or description
    /// might still say them.
    const OLD_WORDS: [&str; 23] = [
        "card",
        "owner",
        "coordinator",
        "lead",
        "preset",
        "full auto",
        "shadow",
        "gate",
        "parked",
        "escalat",
        "vendor",
        "harness",
        "family",
        "families",
        "company",
        "spike",
        "retro",
        "widget",
        "quota",
        "budget",
        "triage",
        "seat",
        "recipe",
    ];

    /// Whether `word` starts a word of `text`, any ending after it.
    fn speaks(text: &str, word: &str) -> bool {
        let text = text.to_lowercase();
        text.match_indices(word).any(|(at, _)| {
            !text[..at]
                .chars()
                .next_back()
                .is_some_and(|before| before.is_alphanumeric())
        })
    }

    /// Nothing you read or type about a setting uses a retired word: not its key,
    /// not a value it takes, and not what `/config`, the schema and the files Argonauts writes
    /// say of it.
    #[test]
    fn no_setting_speaks_an_old_word() {
        let mut found = Vec::new();
        for meta in settings_catalog() {
            let texts = [&meta.key, &meta.description]
                .into_iter()
                .chain(meta.allowed.iter());
            // A mod is a drawn card, so a card on a tab or above the input is one.
            let drawn = meta.key.starts_with("ui.mods.") || meta.key.starts_with("ui.views.");
            for text in texts {
                for word in OLD_WORDS {
                    if speaks(text, word) && !(drawn && word == "card") {
                        found.push(format!("{} · {word} · {text}", meta.key));
                    }
                }
            }
        }
        assert!(
            found.is_empty(),
            "{} old words:\n{}",
            found.len(),
            found.join("\n")
        );
    }
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
                    ("visibility", _) => "worker",
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

    /// A value to try for keys whose pickers offer no list. A crew role's own fallback key
    /// takes what any role's does, so `fallback.workers.order` reads as `fallback.*.order`.
    fn sample(key: &str) -> &'static str {
        let segments: Vec<&str> = key.split('.').collect();
        let any_role = match segments.as_slice() {
            ["fallback", role, rest @ ..] if CrewRole::named(role).is_some() => {
                format!("fallback.*.{}", rest.join("."))
            }
            _ => key.to_owned(),
        };
        match any_role.as_str() {
            "main_agent" | "approvals.decides" | "approvals.approver.second_model" => {
                "\"codex/gpt-x\""
            }
            "roles.*.workers"
            | "crew.orchestrator"
            | "crew.workers"
            | "crew.reviewers"
            | "crew.judge"
            | "fallback.*.order"
            | "fallback.order"
            | "fallback.replace.*.with"
            | "fallback.*.replace.*.with" => "[\"codex\"]",
            "agents.*.model" => "\"opus\"",
            "agents.*.binary" => "\"/opt/codex/bin/codex\"",
            "agents.*.version_pin" => "\"0.158.0\"",
            "endpoints.*.base_url" => "\"http://localhost:11434/v1\"",
            "endpoints.*.api_key_env" => "\"OLLAMA_API_KEY\"",
            "approvals.approver.model_rules" => "\"approval-rules.md\"",
            "limits.run_tokens" | "limits.per_task_tokens" => "200000",
            "approvals.rules.merge.max_changed_lines"
            | "approvals.rules.stuck.max_extra_attempts"
            | "fallback.together"
            | "fallback.*.together"
            | "fallback.max_fallbacks"
            | "fallback.*.max_fallbacks"
            | "workflows.max_depth"
            | "ui.panel.max_rows"
            | "mcp.runs_per_day"
            | "switch.recent" => "2",
            "limits.protected_paths" | "limits.dependency_allowlist" => "[\"docs/**\"]",
            "approvals.refuse_ancestor_env" => "[\"OTHER_AGENT_SHELL\"]",
            "notifications.chat.command" => "[\"/usr/local/bin/send-chat\", \"{text}\"]",
            "notifications.chat.on" => "[\"finished\"]",
            "narrator.model" | "fallback.summarizer" | "fallback.*.summarizer" => {
                "\"ollama/llama3.2\""
            }
            "approvals.answers" => "\"codex/gpt-5.6\"",
            "narrator.interval"
            | "fallback.*.ask_timeout"
            | "fallback.ask_timeout"
            | "mcp.run_limit" => "\"30s\"",
            "narrator.spend_cap" => "\"0.50 usd\"",
            "ui.accent" => "\"#d4a017\"",
            "ui.keymap.*" => "\"ctrl+left\"",
            "goal.checker" => "\"codex\"",
            "approvals.ask_you_about" | "understand.always_ask" => "[\"security\"]",
            "phases" => "\"solo\"",
            "sessions.rotate_at" | "sessions.context" => "\"200k\"",
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
            "solo_or_crew.force",
            "ultracode",
            "fallback.strategy",
            "fallback.together",
            "fallback.pick",
            "fallback.on_return",
            "fallback.checkpoint",
            "fallback.merge_rule",
            "fallback.unknown_tier",
            "fallback.max_fallbacks",
            "fallback.replace.*.with",
            "fallback.replace.*.strategy",
            "fallback.replace.*.on_return",
            "fallback.context",
            "fallback.summarizer",
            "fallback.replace.*.context",
            "fallback.workers.strategy",
            "fallback.workers.together",
            "fallback.reviewers.pick",
            "fallback.main.replace.*.with",
            "fallback.orchestrator.replace.*.on_return",
            "fallback.judge.context",
            "fallback.workers.summarizer",
            "fallback.reviewers.replace.*.context",
            "roles.*.native_fanout",
            "roles.*.native_fanout.max",
            "sessions.context",
            "sessions.rotate_at",
            "ui.panel.subagents",
            "ui.panel.agents",
            "ui.panel.max_rows",
            "ui.glyphs",
            "ui.ambiguous_width",
            "phases",
            "workflows.orchestrator_designs",
            "workflows.allow_composed",
            "workflows.max_depth",
            "workflows.adapt_at_phase_ends",
            "approvals.rules.merge.allow_debt",
            "approvals.presence",
            "approvals.refuse_ancestor_env",
            "mcp.start_runs",
            "mcp.share",
            "mcp.run_limit",
            "notifications.chat.command",
            "notifications.chat.on",
            "endpoints.*.header_env.*",
            "ui.keymap.*",
            "sandbox.agents_see_other_worktrees",
            "sandbox.agents_commit",
            "roles.*.sandbox.mode",
            "roles.*.sandbox.network",
            "roles.*.sandbox.agents_see_other_worktrees",
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
            "ultracode",
            "fallback.checkpoint",
            "roles.worker.blind",
            "ui.panel.agents",
        ] {
            assert_eq!(allowed(key), BOOL, "{key}");
        }
        assert_eq!(allowed("solo_or_crew.force"), ["\"solo\"", "\"crew\""]);
        assert_eq!(allowed("ui.ambiguous_width"), ["1", "2"]);
        assert_eq!(
            allowed("fallback.workers.replace.codex.on_return"),
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
        assert_eq!(allowed("fallback.context"), contexts);
        assert_eq!(allowed("fallback.workers.context"), contexts);
        assert_eq!(allowed("fallback.replace.codex.context"), contexts);
        let fanout = allowed("roles.worker.native_fanout");
        assert!(fanout.contains(&"\"off\"".to_owned()), "{fanout:?}");
        for open in [
            "sessions.context",
            "sessions.rotate_at",
            "phases",
            "notifications.chat.on",
            "crew.workers",
            "fallback.summarizer",
        ] {
            assert!(allowed(open).is_empty(), "{open}");
        }
        assert!(setting_meta("notifications.chat.command").unwrap().safety);
        assert!(setting_meta("fallback.allow_api_billing").unwrap().safety);
        assert!(!setting_meta("fallback.strategy").unwrap().safety);
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
            "runs.remind_waiting_days",
            "roles.*.on_demand",
        ] {
            assert!(keys.contains(key), "{key}");
        }
    }

    #[test]
    fn default_scopes_are_the_ones_each_setting_is_given() {
        let scope = |key: &str| setting_meta(key).map(|meta| meta.default_scope);
        for key in [
            "main_agent",
            "agents.codex.model",
            "agents.codex.effort",
            "roles.reviewer.workers",
            "roles.worker.effort",
        ] {
            assert_eq!(scope(key), Some(SettingScope::ProjectLocal), "{key}");
        }
        for key in [
            "workflow",
            "crew.workers",
            "approvals.merge.mode",
            "limits.spend_per_run",
            "fallback.workers.mode",
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
            "limits.spend_per_day",
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
        for name in ["network", "agents_see_other_worktrees", "agents_commit"] {
            let every = setting_meta(&format!("sandbox.{name}")).unwrap();
            let one = setting_meta(&format!("roles.worker.sandbox.{name}")).unwrap();
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
        let meta = setting_meta("fallback.reviewers.allow_api_billing").unwrap();
        assert_eq!(meta.key, "fallback.reviewers.allow_api_billing");
        assert!(meta.safety);
        let meta = setting_meta("approvals.merge.mode").unwrap();
        assert!(meta.safety);
        assert_eq!(
            meta.allowed,
            [
                "\"supervised\"",
                "\"auto_preview\"",
                "\"auto\"",
                "\"autopilot\""
            ]
        );
        assert!(!setting_meta("ui.keys").unwrap().safety);
        assert!(
            setting_meta("workflow")
                .unwrap()
                .allowed
                .contains(&"\"orchestrator\"".to_owned())
        );
        assert_eq!(setting_meta("nothing.here"), None);
    }

    #[test]
    fn the_catalog_lists_every_default_key() {
        let catalog = settings_catalog();
        let find = |key: &str| catalog.iter().find(|meta| meta.key == key);
        assert!(find("ui.keys").is_some_and(|meta| !meta.safety));
        assert!(find("approvals.merge.mode").is_some_and(|meta| meta.safety));
        assert!(find("runs.remind_waiting_days").is_some());
        assert!(find("limits.parked_run_expires_days").is_none());
    }
}
