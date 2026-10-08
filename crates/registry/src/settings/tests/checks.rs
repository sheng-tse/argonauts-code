use argonauts_core::{canonical_json, sha256_hex};

use super::{Repo, invalid, layer_of};
use crate::{SettingsError, SettingsSources, resolve};

/// Every workflow setting, each with its default where it has one (as JSON).
const WORKFLOW_SETTINGS: &[(&str, Option<&str>)] = &[
    // Everything is a default.
    ("solo_or_crew", None),
    ("solo_or_crew.solo_max_files", None),
    ("solo_or_crew.crew_min_files", None),
    ("solo_or_crew.crew_labels", None),
    ("drafting.blind", Some("true")),
    ("judging.anonymize", Some("true")),
    ("judging.method", Some("\"cross\"")),
    ("critique.rounds", Some("1")),
    ("critique.critic", Some("\"rotate\"")),
    ("experiments.enabled", Some("true")),
    ("experiments.limit_usd", Some("2.0")),
    ("review.assign", Some("\"ring\"")),
    ("review.other_provider", Some("true")),
    ("solo.restate_first", Some("false")),
    ("approvals.goal_changes", Some("\"ask\"")),
    ("checks.run", None),
    ("routing.algorithm", Some("\"bandit\"")),
    ("routing.explore_rate", None),
    ("approvals.merge.mode", Some("\"auto\"")),
    ("approvals.merge.decide_with", Some("\"rules\"")),
    ("approvals.plan.mode", Some("\"auto\"")),
    ("approvals.plan.decide_with", Some("\"rules_and_model\"")),
    ("approvals.goal.mode", Some("\"auto\"")),
    ("approvals.goal.decide_with", Some("\"rules_and_model\"")),
    ("approvals.stuck.mode", Some("\"auto\"")),
    ("approvals.stuck.decide_with", Some("\"rules_and_model\"")),
    ("approvals.limit.mode", Some("\"auto\"")),
    ("approvals.limit.decide_with", Some("\"rules_and_model\"")),
    ("approvals.change_request.mode", Some("\"auto\"")),
    (
        "approvals.change_request.decide_with",
        Some("\"rules_and_model\""),
    ),
    ("approvals.workflow_promotion.mode", Some("\"auto\"")),
    ("approvals.approver.different_provider", Some("true")),
    ("sessions.lifetime", Some("\"task\"")),
    ("compaction", Some("\"inherit\"")),
    ("runs.remind_waiting_days", Some("7")),
    ("limits.spend_per_run", Some("null")),
    ("limits.spend_per_day", Some("null")),
    ("limits.plan_usage", Some("null")),
    ("limits.per_task_usd", Some("null")),
    ("limits.soft_stop", Some("0.85")),
    ("limits.when_reached", Some("\"fallback\"")),
    ("watchdogs", None),
    ("fallback.mode", Some("\"auto\"")),
    ("native_fanout", Some("{\"max\":3}")),
    ("concurrency", None),
    ("visibility", None),
    ("evolution", None),
    // ui.verbosity defaults to `full`.
    ("ui.verbosity", Some("\"full\"")),
    ("ui.keys", None),
    ("ui.takeover", None),
    ("ui.keymap", None),
    ("ui.narrator", Some("\"on_demand\"")),
    ("narrator.model", None),
    ("narrator.interval", None),
    ("narrator.spend_cap", None),
    ("sandbox", None),
    ("sandbox.mode", Some("\"os\"")),
    ("sandbox.network", Some("true")),
    ("sandbox.agents_see_other_worktrees", Some("false")),
    ("sandbox.agents_commit", Some("true")),
    ("limits", None),
    // The keys the later changes and decisions added.
    ("workflow", Some("\"council\"")),
    ("workflow", Some("\"council\"")),
    ("main_agent", Some("\"claude-code\"")),
    ("crew", None),
    ("fallback.strategy", Some("\"first\"")),
    ("fallback.together", Some("1")),
    ("fallback.on_return", Some("\"ask\"")),
    ("fallback.checkpoint", Some("true")),
    ("fallback.merge_rule", Some("\"upgrade_review\"")),
    ("fallback.unknown_tier", Some("\"weaker\"")),
    ("fallback.max_fallbacks", Some("3")),
    ("fallback.context", Some("\"note+transcript\"")),
    ("fallback.replace", Some("{}")),
    ("approvals.rules.merge.allow_debt", Some("false")),
    ("approvals.presence", Some("\"warn\"")),
    (
        "approvals.refuse_ancestor_env",
        Some("[\"OPENCLAW_SHELL\",\"HERMES_SESSION_ID\"]"),
    ),
    ("mcp.start_runs", Some("\"trusted-repos\"")),
    ("mcp.guidance", Some("true")),
    ("mcp.pause", Some("true")),
    ("mcp.share", Some("\"summaries\"")),
    ("mcp.run_limit", Some("\"60m\"")),
    ("mcp.runs_per_day", Some("5")),
    ("notifications.chat.command", Some("[]")),
    (
        "notifications.chat.on",
        Some("[\"needs_you\",\"finished\",\"agent_down\"]"),
    ),
    ("sessions.context", Some("\"largest\"")),
    ("sessions.rotate_at", Some("0.9")),
    ("workflows.orchestrator_designs", Some("true")),
    ("workflows.allow_composed", Some("true")),
    ("workflows.max_depth", Some("2")),
    ("workflows.adapt_at_phase_ends", Some("true")),
    ("ui.panel.subagents", Some("\"discovered\"")),
    ("ui.panel.agents", Some("false")),
    ("ui.panel.max_rows", Some("5")),
    ("ui.glyphs", Some("\"unicode\"")),
    ("ui.ambiguous_width", Some("1")),
];

#[test]
fn every_workflow_setting_has_a_key() {
    let resolved = resolve(&SettingsSources::default()).unwrap();
    let json = serde_json::to_value(&resolved.settings).unwrap();
    for (key, default) in WORKFLOW_SETTINGS {
        let found = key
            .split('.')
            .try_fold(&json, |value, segment| value.get(segment));
        let Some(found) = found else {
            panic!("{key} has no key in the settings")
        };
        if let Some(default) = default {
            let expected: serde_json::Value = serde_json::from_str(default).unwrap();
            assert_eq!(found, &expected, "{key}");
        }
    }
    // A role's own fallback mode exists once the role has a fallback table.
    let owner = resolve(&SettingsSources {
        overrides: vec!["fallback.workers.together=2".into()],
        ..SettingsSources::default()
    })
    .unwrap();
    assert!(
        owner
            .entries
            .iter()
            .any(|entry| entry.key == "fallback.workers.mode")
    );
    assert!(
        owner
            .entries
            .iter()
            .any(|entry| entry.key == "fallback.workers.together")
    );
}

#[test]
fn resolved_hash_recorded() {
    let repo = Repo::new();
    repo.project("[critique]\nrounds = 2\n");
    let first = resolve(&repo.sources()).unwrap();
    assert_eq!(first.sha256, resolve(&repo.sources()).unwrap().sha256);
    let json = serde_json::to_value(&first.settings).unwrap();
    assert_eq!(first.sha256, sha256_hex(canonical_json(&json).as_bytes()));
    assert_ne!(
        first.sha256,
        resolve(&SettingsSources::default()).unwrap().sha256
    );

    let flagged = resolve(&repo.with_flags(&["critique.rounds=3"])).unwrap();
    assert_ne!(flagged.sha256, first.sha256);
    repo.local("[ui]\nbell = false\n");
    let local = resolve(&repo.sources()).unwrap();
    assert_ne!(local.sha256, first.sha256);
    repo.global_preset("council", "[judging]\nanonymize = false\n");
    assert_ne!(resolve(&repo.sources()).unwrap().sha256, local.sha256);
}

#[test]
fn rotate_at_validated_by_resolve() {
    let repo = Repo::new();
    for good in ["\"200k\"", "150000", "0.9", "1.0", "\"1k\"", "\"1m\""] {
        repo.project(&format!("[sessions]\nrotate_at = {good}\n"));
        let resolved = resolve(&repo.sources()).unwrap_or_else(|err| panic!("{good}: {err}"));
        assert_eq!(
            layer_of(&resolved, "sessions.rotate_at").as_deref(),
            Some("project")
        );
    }
    for bad in ["0", "1", "999", "\"5\"", "1.5", "\"lots\"", "true"] {
        repo.project(&format!("[sessions]\nrotate_at = {bad}\n"));
        let (layer, message) = invalid(resolve(&repo.sources()));
        assert_eq!(layer, repo.project_file(), "{bad}");
        assert!(
            message.starts_with("sessions.rotate_at"),
            "{bad}: {message}"
        );
    }
    repo.project("[sessions]\nrotate_at = 1\n");
    let (_, message) = invalid(resolve(&repo.sources()));
    assert!(message.contains("1.0"), "a whole window is 1.0: {message}");
    repo.project("");
    let (layer, _) = invalid(resolve(&repo.with_flags(&["sessions.rotate_at=0"])));
    assert_eq!(layer, "--set sessions.rotate_at=0");
}

#[test]
fn agent_binary_must_be_absolute_outside_repo() {
    let repo = Repo::new();
    let binary = |path: &str| format!("[agents.codex]\nbinary = {path:?}\n");

    repo.project(&binary("bin/codex"));
    let (layer, message) = invalid(resolve(&repo.trusted()));
    assert_eq!(layer, repo.project_file());
    assert!(message.starts_with("agents.codex.binary"), "{message}");
    assert!(message.contains("absolute"), "{message}");

    let root = repo.root().display().to_string();
    for inside in [
        format!("{root}/tools/codex"),
        format!("{root}/tools/../../repo/bin/codex"),
    ] {
        repo.project(&binary(&inside));
        let (_, message) = invalid(resolve(&repo.trusted()));
        assert!(message.contains("outside the repo"), "{inside}: {message}");
    }
    // A link out of the repo counts where it points.
    let tools = repo.root().join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    let link = repo.outside().join("tools-link");
    std::os::unix::fs::symlink(&tools, &link).unwrap();
    repo.project(&binary(&format!("{}/codex", link.display())));
    let (_, message) = invalid(resolve(&repo.trusted()));
    assert!(message.contains("outside the repo"), "{message}");

    let outside = repo.outside().join("bin/codex");
    repo.project(&binary(&outside.display().to_string()));
    let resolved = resolve(&repo.trusted()).unwrap();
    assert_eq!(
        resolved.settings.agents["codex"].binary.as_ref(),
        Some(&outside)
    );

    // Your global file is held to the same rule.
    repo.project("");
    repo.global(&binary("codex"));
    let (layer, _) = invalid(resolve(&repo.sources()));
    assert_eq!(layer, repo.global_file());
}

#[test]
fn global_only_keys_refused_from_project_files() {
    let repo = Repo::new();
    let chat = "[notifications.chat]\ncommand = [\"/usr/bin/notify\", \"{text}\"]\n";
    repo.global(chat);
    let resolved = resolve(&repo.sources()).unwrap();
    assert_eq!(resolved.settings.notifications.chat.command.len(), 2);
    repo.global("");
    for trusted in [false, true] {
        repo.project(chat);
        let sources = SettingsSources {
            project_trusted: trusted,
            ..repo.sources()
        };
        let err = resolve(&sources).unwrap_err();
        assert!(
            matches!(&err, SettingsError::Refused(message)
                if message.contains("notifications.chat.command")
                    && message.contains(&repo.project_file())),
            "{err}"
        );
    }
    repo.project("");
    repo.local(chat);
    assert!(resolve(&repo.trusted()).is_err());
}

#[test]
fn values_serde_accepts_are_still_checked() {
    let repo = Repo::new();
    let cases = [
        ("[limits]\nsoft_stop = 0.0\n", "limits.soft_stop"),
        ("[limits]\nsoft_stop = 1.5\n", "limits.soft_stop"),
        ("[limits.lines]\ntasks = 0.9\n", "limits.lines"),
        (
            "[limits.lines]\ntasks = -0.1\nreview = 0.6\n",
            "limits.lines.tasks",
        ),
        ("[critique]\ncritic = \"two critics\"\n", "critique.critic"),
        ("native_fanout = { max = 0 }\n", "native_fanout.max"),
        (
            "[roles.worker]\nnative_fanout = { max = 0 }\n",
            "roles.worker.native_fanout.max",
        ),
        ("[workflows]\nmax_depth = 3\n", "workflows.max_depth"),
        ("[checks]\ntimeout = \"5 minutes\"\n", "checks.timeout"),
        (
            "[endpoints.local]\ntimeout = \"soon\"\n",
            "endpoints.local.timeout",
        ),
        (
            "[agents.codex]\neffort = \"extreme\"\n",
            "agents.codex.effort",
        ),
        (
            "[approvals.approver]\nsecond_model = \"codex\"\n",
            "approvals.approver.second_model",
        ),
        (
            "[approvals]\ndecides = \"codex/<model>\"\n[approvals.approver]\nsecond_model = \"codex\"\n",
            "approvals.approver.second_model",
        ),
        (
            "[approvals.plan]\notherwise = \"open_pull_request\"\n",
            "approvals.plan.otherwise",
        ),
        // Only a merge opens a pull request, and a budget has nothing to send back.
        (
            "[approvals.plan]\nat_hard_limit = \"open_pull_request\"\n",
            "approvals.plan.at_hard_limit",
        ),
        (
            "[approvals.limit]\nat_hard_limit = \"send_back\"\n",
            "approvals.limit.at_hard_limit",
        ),
        (
            "[approvals]\nanswers = \"the main agent\"\n",
            "approvals.answers",
        ),
        (
            "[approvals]\nspot_check_sample = 2.0\n",
            "approvals.spot_check_sample",
        ),
    ];
    for (text, key) in cases {
        repo.project(text);
        let (layer, message) = invalid(resolve(&repo.trusted()));
        assert_eq!(layer, repo.project_file(), "{text}");
        assert!(message.starts_with(key), "{key}: {message}");
    }
    // A model approver makes the same approvals sane, and full auto takes a picked answerer
    // and a merge that opens a pull request at a hard limit.
    repo.project(
        "[approvals]\nautopilot = true\nanswers = \"opencode/glm-5\"\n\
         decides = \"codex/<model>\"\n\
         [approvals.approver]\nsecond_model = \"claude-code/opus\"\n\
         [approvals.merge]\nmode = \"auto\"\ndecide_with = \"model\"\n\
         at_hard_limit = \"open_pull_request\"\n\
         [approvals.rules.merge]\nmin_test_coverage_of_changed_lines = 0.5\n",
    );
    let resolved = resolve(&repo.trusted()).unwrap();
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
    let effort = resolve(&repo.with_flags(&["agents.codex.effort=high"])).unwrap();
    assert_eq!(
        effort.settings.agents["codex"].effort.as_deref(),
        Some("high")
    );
}

/// Full auto's settings that wouldn't do what you meant say so: a picked answerer no agent
/// provides, or one the approver can't check, and a checkpoint the switch leaves with you. A
/// checkpoint kept supervised in the repo stays supervised under a global switch.
#[test]
fn autopilot_settings_that_miss_say_so() {
    use argonauts_core::settings::approvals::{Answerer, ApprovalMode};

    let repo = Repo::new();
    for (word, answerer) in [("You", Answerer::You), ("Main", Answerer::Main)] {
        repo.project(&format!("[approvals]\nanswers = \"{word}\"\n"));
        let resolved = resolve(&repo.trusted()).unwrap();
        assert_eq!(resolved.settings.approvals.answers, Some(answerer));
        assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
    }

    repo.project("[approvals]\nanswers = \"me\"\n");
    let resolved = resolve(&repo.trusted()).unwrap();
    assert!(
        resolved
            .warnings
            .iter()
            .any(|warning| warning.starts_with("approvals.answers names me")
                && warning.ends_with("the main agent answers in its place")),
        "{:?}",
        resolved.warnings
    );

    repo.project("[approvals]\nanswers = \"codex/gpt-5.6\"\ndecides = \"codex/gpt-5.6-luna\"\n");
    let resolved = resolve(&repo.trusted()).unwrap();
    assert!(
        resolved
            .warnings
            .iter()
            .any(|warning| warning.contains("both come from openai")),
        "{:?}",
        resolved.warnings
    );

    repo.global("[approvals]\nautopilot = true\n");
    repo.project("[approvals.merge]\nmode = \"supervised\"\n");
    let resolved = resolve(&repo.trusted()).unwrap();
    let approvals = &resolved.settings.approvals;
    assert_eq!(
        approvals.run_mode(approvals.merge.mode),
        ApprovalMode::Supervised
    );
    assert_eq!(
        approvals.run_mode(approvals.plan.mode),
        ApprovalMode::Autopilot
    );
    assert!(
        resolved.warnings.iter().any(|warning| warning
            .contains("approvals.merge.mode = \"supervised\" keeps that approval for you")),
        "{:?}",
        resolved.warnings
    );
}

#[test]
fn an_unset_approver_model_is_picked_for_each_run_so_nothing_warns() {
    let repo = Repo::new();
    // The defaults decide most checkpoints with rules and a model, and leave the model unset.
    let defaults = resolve(&repo.trusted()).unwrap();
    assert_eq!(defaults.settings.approvals.approver_model(), None);
    assert!(defaults.warnings.is_empty(), "{:?}", defaults.warnings);

    repo.project(
        "[approvals.goal]\nmode = \"auto_preview\"\ndecide_with = \"rules_or_model\"\n\
         [approvals.merge]\nmode = \"auto\"\ndecide_with = \"model\"\n",
    );
    let resolved = resolve(&repo.trusted()).unwrap();
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
}

#[test]
fn the_older_sandbox_off_still_reads_as_the_tools_own() {
    use argonauts_core::names::RoleName;
    use argonauts_core::settings::approvals::SandboxChoice;

    let repo = Repo::new();
    repo.project("[sandbox]\nmode = \"off\"\n[roles.reviewer.sandbox]\nmode = \"off\"\n");
    let resolved = resolve(&repo.trusted()).unwrap();
    assert_eq!(resolved.settings.sandbox.mode, SandboxChoice::Tool);
    let reviewer = resolved.settings.sandbox_for(&RoleName::Reviewer);
    assert_eq!(reviewer.mode, SandboxChoice::Tool);

    // Other words the picker doesn't offer are still refused.
    repo.project("[sandbox]\nmode = \"loose\"\n");
    let (layer, _) = invalid(resolve(&repo.trusted()));
    assert_eq!(layer, repo.project_file());
}

#[test]
fn a_gate_written_as_a_string_shows_the_table_to_write_instead() {
    let repo = Repo::new();
    repo.local("[limits]\nspend_per_run = 1.0\n\n[checks]\nrun = [\"python3 -m unittest -q\"]\n");
    // Untrusted, the gates are a safety key left out, so the file resolves.
    let resolved = resolve(&repo.sources()).unwrap();
    assert!(
        resolved.warnings.iter().any(|warning| warning.starts_with(
            "checks.run in .argonauts/config.local.toml is a safety setting and is ignored"
        )),
        "{:?}",
        resolved.warnings
    );
    let (layer, message) = invalid(resolve(&repo.trusted()));
    assert_eq!(layer, ".argonauts/config.local.toml");
    assert_eq!(
        message,
        "checks.run can't be a list of strings; it takes a list of checks, each a [[checks.run]] \
         table with a name and a command. Write it like this:\n\
         [[checks.run]]\n\
         name = \"python3\"\n\
         command = [\"python3\", \"-m\", \"unittest\", \"-q\"]"
    );
}

/// The likeliest slip when writing a gate by hand: one bracket pair, or an inline table.
#[test]
fn a_gate_written_as_one_table_resolves_untrusted_and_shows_the_list_to_write() {
    let repo = Repo::new();
    let expected = "checks.run is written as a single table, but it takes a list of checks, \
                    each a [[checks.run]] table with two brackets. Write it like this:\n\
                    [[checks.run]]\n\
                    name = \"unittest\"\n\
                    command = [\"python3\", \"-m\", \"unittest\", \"-q\"]";
    for text in [
        "[checks.run]\nname = \"unittest\"\ncommand = [\"python3\", \"-m\", \"unittest\", \"-q\"]\n",
        "[checks]\nrun = { name = \"unittest\", command = [\"python3\", \"-m\", \"unittest\", \"-q\"] }\n",
    ] {
        repo.local(text);
        // Untrusted, its safety settings are left out and leave no empty table behind.
        let resolved = resolve(&repo.sources()).unwrap_or_else(|err| panic!("{text}: {err}"));
        assert!(
            resolved.warnings.iter().any(|warning| warning.starts_with(
                "checks.run.command in .argonauts/config.local.toml is a safety setting"
            )),
            "{:?}",
            resolved.warnings
        );
        let (layer, message) = invalid(resolve(&repo.trusted()));
        assert_eq!(layer, ".argonauts/config.local.toml", "{text}");
        assert_eq!(message, expected, "{text}");
    }

    // An empty table sets no key, and the error still names the file, not the built-ins.
    repo.local("[checks]\nrun = {}\n");
    let (layer, message) = invalid(resolve(&repo.trusted()));
    assert_eq!(layer, ".argonauts/config.local.toml");
    assert!(
        message.starts_with("checks.run is written as a single table"),
        "{message}"
    );

    // Without a name, the check is named for its command.
    repo.local("[checks.run]\ncommand = \"cargo test\"\n");
    let (_, message) = invalid(resolve(&repo.trusted()));
    assert!(
        message.ends_with("[[checks.run]]\nname = \"test\"\ncommand = [\"cargo\", \"test\"]"),
        "{message}"
    );
}

#[test]
fn every_kind_of_wrong_value_says_what_the_key_takes_in_plain_words() {
    let repo = Repo::new();
    let cases = [
        // A string where the list of checks goes, and several of them.
        (
            "[checks]\nrun = \"cargo test\"\n",
            "checks.run can't be \"cargo test\"; it takes a list of checks, each a [[checks.run]] \
             table with a name and a command. Write it like this:\n[[checks.run]]\n\
             name = \"test\"\n\
             command = [\"cargo\", \"test\"]",
        ),
        (
            "[checks]\nsetup = [\"npm ci\", \"npm run build\"]\n",
            "checks.setup can't be a list of 2 strings; it takes a list of checks, each a \
             [[checks.setup]] table with a name and a command. Write it like this:\n\
             [[checks.setup]]\n\
             name = \"ci\"\ncommand = [\"npm\", \"ci\"]\n\n[[checks.setup]]\n\
             name = \"build\"\ncommand = [\"npm\", \"run\", \"build\"]",
        ),
        (
            "[checks]\nrun = [{ name = \"lint\", command = [\"make\", \"lint\"] }, \"make test\"]\n",
            "checks.run can't hold \"make test\"; each entry is a table, written [[checks.run]] with \
             its settings under it",
        ),
        (
            "[[checks.run]]\nname = \"tests\"\n",
            "each [[checks.run]] table needs both a name and a command. Write it like this:\n\
             [[checks.run]]\nname = \"test\"\ncommand = [\"make\", \"test\"]",
        ),
        (
            "[[checks.run]]\nname = \"tests\"\ncommand = \"cargo test\"\n",
            "checks.run.command can't be \"cargo test\"; it takes a list, such as \
             [\"cargo\", \"test\"]",
        ),
        // The wrong type.
        (
            "[limits]\nspend_per_run = \"1\"\n",
            "limits.spend_per_run can't be \"1\"; it takes a number. Write it without quotes: \
             spend_per_run = 1",
        ),
        // A hard limit, which shares the table with the run and task limits.
        (
            "[limits]\nspend_per_run = 5.0\nauto_merges_per_day = \"3\"\n",
            "limits.auto_merges_per_day can't be \"3\"; it takes a whole number, 0 or more. \
             Write it without quotes: auto_merges_per_day = 3",
        ),
        (
            "[drafting]\nblind = \"yes\"\n",
            "drafting.blind can't be \"yes\"; it takes true or false",
        ),
        (
            "[critique]\nrounds = \"many\"\n",
            "critique.rounds can't be \"many\"; it takes a whole number, 0 or more",
        ),
        (
            "crew = \"claude-code\"\n",
            "crew can't be \"claude-code\"; it is a table, written [crew] with its settings under it",
        ),
        (
            "[crew]\nworkers = [1]\n",
            "crew.workers can't hold 1; each entry takes text in quotes",
        ),
        (
            "[crew]\njudge = 3\n",
            "crew.judge can't be 3; it takes a list",
        ),
        (
            "ui = 3\n",
            "ui can't be 3; it is a table, written [ui] with its settings under it",
        ),
        // A value of the right type the key doesn't take.
        (
            "[critique]\nrounds = -1\n",
            "critique.rounds can't be -1; it takes a whole number, 0 or more",
        ),
        (
            "[ui]\nkeys = \"emacs\"\n",
            "ui.keys can't be \"emacs\"; it takes \"default\" or \"vim\"",
        ),
        (
            "[ui]\nkeys = 3\n",
            "ui.keys can't be 3; it takes \"default\" or \"vim\"",
        ),
        // Text that isn't TOML.
        (
            "[checks\n",
            "line 1 isn't valid TOML (unclosed table, expected `]`): [checks",
        ),
        (
            "[limits]\nspend_per_run = 1\nspend_per_run = 2\n",
            "line 3 sets spend_per_run a second time; each key goes in a table once",
        ),
    ];
    for (text, expected) in cases {
        repo.local(text);
        let (layer, message) = invalid(resolve(&repo.trusted()));
        assert_eq!(layer, ".argonauts/config.local.toml", "{text}");
        assert_eq!(message, expected, "{text}");
    }
}

/// Who decides when you don't is written as the settings template writes it. The keys it
/// replaced are no longer settings, and say so.
#[test]
fn approvals_read_as_the_template_writes_them_and_the_old_keys_are_unknown() {
    let repo = Repo::new();
    repo.project(
        "[approvals]\nanswers = \"main\"\ndecides = \"judge\"\nwhen_declined = \"decide\"\n\
         ask_limit = 2\n\
         [approvals.rules.merge]\nmin_test_coverage_of_changed_lines = 0.8\n\
         max_changed_lines = 400\n",
    );
    let resolved = resolve(&repo.trusted()).unwrap();
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
    assert_eq!(
        resolved.settings.approvals.rules.merge.max_changed_lines,
        400
    );

    repo.project(
        "[approvals]\nanswered_by = \"main\"\n\
         [approvals.approver]\nmodel = \"codex/gpt-5.6\"\n\
         [approvals.merge.rules]\nmax_combined_diff_lines = 400\n",
    );
    let resolved = resolve(&repo.trusted()).unwrap();
    for old in [
        "approvals.answered_by",
        "approvals.approver.model",
        "approvals.merge.rules.max_combined_diff_lines",
    ] {
        let warned = format!("unknown setting {old} in .argonauts/config.toml, ignored");
        assert!(
            resolved.warnings.contains(&warned),
            "{old}: {:?}",
            resolved.warnings
        );
    }
}

#[test]
fn an_unknown_key_is_a_warning_naming_the_file_from_the_repo() {
    let repo = Repo::new();
    repo.project("[checks]\nruns = []\n");
    let resolved = resolve(&repo.trusted()).unwrap();
    assert_eq!(
        resolved.warnings,
        vec!["unknown setting checks.runs in .argonauts/config.toml, ignored".to_owned()]
    );
}
