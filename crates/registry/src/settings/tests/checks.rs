use argonauts_core::{canonical_json, sha256_hex};

use super::{Repo, invalid, layer_of};
use crate::{SettingsError, SettingsSources, resolve};

/// Every workflow setting, each with its default where it has one (as JSON).
const WORKFLOW_SETTINGS: &[(&str, Option<&str>)] = &[
    // Everything is a default.
    ("triage", None),
    ("triage.solo_max_files", None),
    ("triage.crew_min_files", None),
    ("triage.crew_labels", None),
    ("drafting.blind", Some("true")),
    ("judging.anonymize", Some("true")),
    ("judging.method", Some("\"cross\"")),
    ("critique.rounds", Some("1")),
    ("critique.critic", Some("\"rotate\"")),
    ("spikes.enabled", Some("true")),
    ("spikes.budget_usd", Some("2.0")),
    ("review.assign", Some("\"ring\"")),
    ("review.other_family", Some("true")),
    ("solo.restate_first", Some("false")),
    ("approvals.goal_changes", Some("\"ask\"")),
    ("gates.run", None),
    ("routing.algorithm", Some("\"bandit\"")),
    ("routing.explore_rate", None),
    ("approvals.merge.mode", Some("\"auto\"")),
    ("approvals.merge.decide_with", Some("\"rules\"")),
    ("approvals.plan.mode", Some("\"auto\"")),
    ("approvals.plan.decide_with", Some("\"rules_and_model\"")),
    ("approvals.goal.mode", Some("\"auto\"")),
    ("approvals.goal.decide_with", Some("\"rules_and_model\"")),
    ("approvals.escalation.mode", Some("\"auto\"")),
    (
        "approvals.escalation.decide_with",
        Some("\"rules_and_model\""),
    ),
    ("approvals.budget.mode", Some("\"auto\"")),
    ("approvals.budget.decide_with", Some("\"rules_and_model\"")),
    ("approvals.change_request.mode", Some("\"auto\"")),
    (
        "approvals.change_request.decide_with",
        Some("\"rules_and_model\""),
    ),
    ("approvals.recipe_promotion.mode", Some("\"auto\"")),
    ("approvals.approver.different_family", Some("true")),
    ("sessions.lifetime", Some("\"card\"")),
    ("compaction", Some("\"inherit\"")),
    ("runs.remind_parked_days", Some("7")),
    ("budget.run_usd", Some("20.0")),
    ("budget.per_card_usd", Some("3.0")),
    ("budget.soft_stop", Some("0.85")),
    ("budget.on_hit", Some("\"ask\"")),
    ("watchdogs", None),
    ("handoff.mode", Some("\"auto\"")),
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
    ("sandbox.agents_see_other_copies", Some("false")),
    ("sandbox.agents_commit", Some("true")),
    ("limits", None),
    // The keys the later changes and decisions added.
    ("preset", Some("\"peer-crew\"")),
    ("workflow", Some("\"peer-crew\"")),
    ("default_agent", Some("\"claude-code\"")),
    ("crew", None),
    ("handoff.strategy", Some("\"first\"")),
    ("handoff.race_count", Some("2")),
    ("handoff.on_return", Some("\"ask\"")),
    ("handoff.checkpoint", Some("true")),
    ("handoff.merge_rule", Some("\"upgrade_review\"")),
    ("handoff.unknown_tier", Some("\"weaker\"")),
    ("handoff.max_handoffs", Some("3")),
    ("handoff.context", Some("\"note+transcript\"")),
    ("handoff.replace", Some("{}")),
    ("approvals.merge.rules.allow_debt", Some("false")),
    ("approvals.presence", Some("\"warn\"")),
    (
        "approvals.refuse_ancestor_env",
        Some("[\"OPENCLAW_SHELL\",\"HERMES_SESSION_ID\"]"),
    ),
    ("mcp.start_runs", Some("\"trusted-repos\"")),
    ("mcp.guidance", Some("true")),
    ("mcp.pause", Some("true")),
    ("mcp.share", Some("\"summaries\"")),
    ("mcp.run_budget", Some("\"60m\"")),
    ("mcp.runs_per_day", Some("5")),
    ("notifications.chat.command", Some("[]")),
    (
        "notifications.chat.on",
        Some("[\"needs_you\",\"finished\",\"vendor_down\"]"),
    ),
    ("sessions.rotate_at", Some("\"200k\"")),
    ("workflows.lead_designs", Some("true")),
    ("workflows.allow_composed", Some("true")),
    ("workflows.max_depth", Some("2")),
    ("workflows.adapt_at_phase_ends", Some("true")),
    ("ui.panel.vendor_internals", Some("\"discovered\"")),
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
    // A role's own handoff mode exists once the role has a table.
    let owner = resolve(&SettingsSources {
        overrides: vec!["roles.owner.workers=[\"codex\"]".into()],
        ..SettingsSources::default()
    })
    .unwrap();
    assert!(
        owner
            .entries
            .iter()
            .any(|entry| entry.key == "roles.owner.handoff.mode")
    );
    assert!(
        owner
            .entries
            .iter()
            .any(|entry| entry.key == "roles.owner.workers")
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
    repo.global_preset("peer-crew", "[judging]\nanonymize = false\n");
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
        ("[budget]\nsoft_stop = 0.0\n", "budget.soft_stop"),
        ("[budget]\nsoft_stop = 1.5\n", "budget.soft_stop"),
        ("[budget.lines]\ncards = 0.9\n", "budget.lines"),
        (
            "[budget.lines]\ncards = -0.1\nreview = 0.6\n",
            "budget.lines.cards",
        ),
        ("[critique]\ncritic = \"two critics\"\n", "critique.critic"),
        ("native_fanout = { max = 0 }\n", "native_fanout.max"),
        (
            "[roles.owner]\nnative_fanout = { max = 0 }\n",
            "roles.owner.native_fanout.max",
        ),
        ("[workflows]\nmax_depth = 3\n", "workflows.max_depth"),
        ("[gates]\ntimeout = \"5 minutes\"\n", "gates.timeout"),
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
            "[approvals.approver]\nmodel = \"codex/<model>\"\nsecond_model = \"codex\"\n",
            "approvals.approver.second_model",
        ),
        (
            "[approvals.plan]\notherwise = \"open_pull_request\"\n",
            "approvals.plan.otherwise",
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
    // A model approver makes the same approvals sane.
    repo.project(
        "[approvals.approver]\nmodel = \"codex/<model>\"\nsecond_model = \"claude-code/opus\"\n\
         [approvals.merge]\nmode = \"auto\"\ndecide_with = \"model\"\n\
         [approvals.merge.rules]\nmin_changed_line_coverage = 0.5\n",
    );
    let resolved = resolve(&repo.trusted()).unwrap();
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
    let effort = resolve(&repo.with_flags(&["agents.codex.effort=high"])).unwrap();
    assert_eq!(
        effort.settings.agents["codex"].effort.as_deref(),
        Some("high")
    );
}

#[test]
fn an_unset_approver_model_is_picked_for_each_run_so_nothing_warns() {
    let repo = Repo::new();
    // The defaults decide most checkpoints with rules and a model, and leave the model unset.
    let defaults = resolve(&repo.trusted()).unwrap();
    assert_eq!(defaults.settings.approvals.approver.model, None);
    assert!(defaults.warnings.is_empty(), "{:?}", defaults.warnings);

    repo.project(
        "[approvals.goal]\nmode = \"shadow\"\ndecide_with = \"rules_or_model\"\n\
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
