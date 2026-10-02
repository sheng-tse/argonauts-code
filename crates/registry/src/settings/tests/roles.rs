use std::fs;

use argonauts_core::AuthMode;
use argonauts_core::names::{RoleName, VendorId};
use argonauts_core::settings::handoff::{HandoffMode, HandoffSettings, HandoffStrategy, OnReturn};
use argonauts_core::settings::roles::RoleTrigger;

use super::{Repo, invalid, layer_of};
use crate::{SettingsSources, resolve};

/// The coordinated-crew example team, in the TOML the settings take: one worker per role is a
/// one-item `workers` list, and each harness's `auth` lives under `[agents.<id>]`.
const COORDINATED_CREW: &str = r#"
preset = "coordinated-crew"

[agents.codex]
auth = "subscription"

[agents.claude-code]
auth = "subscription"

[roles.coordinator]
workers = ["codex"]

[roles.owner]
workers = ["claude-code/opus"]

[roles.consultant]
workers = ["claude-code/fable"]
on_demand = true

[roles.reviewer]
blind = true
workers = ["openrouter/<model-a>", "openrouter/<model-b>"]

[roles.adjudicator]
workers = ["codex"]
fresh_session = true
when = "reviewers_disagree"
"#;

#[test]
fn roles_config_parses() {
    let repo = Repo::new();
    repo.project(COORDINATED_CREW);
    let resolved = resolve(&repo.trusted()).unwrap();
    // `openrouter` is a built-in endpoint, so its workers raise no warning.
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
    let settings = &resolved.settings;
    assert_eq!(settings.preset, "coordinated-crew");
    assert_eq!(settings.review.per_card, 2);
    assert_eq!(settings.agents["codex"].auth, AuthMode::Subscription);
    let role = |name: RoleName| &settings.roles[&name];
    let workers = |name: RoleName| -> Vec<String> {
        role(name).workers.iter().map(ToString::to_string).collect()
    };
    assert_eq!(workers(RoleName::Coordinator), ["codex"]);
    assert_eq!(workers(RoleName::Owner), ["claude-code/opus"]);
    assert_eq!(workers(RoleName::Consultant), ["claude-code/fable"]);
    assert!(role(RoleName::Consultant).on_demand);
    assert_eq!(
        workers(RoleName::Reviewer),
        ["openrouter/<model-a>", "openrouter/<model-b>"]
    );
    assert_eq!(role(RoleName::Reviewer).blind, Some(true));
    assert_eq!(
        role(RoleName::Reviewer).workers[1].model.as_deref(),
        Some("<model-b>")
    );
    let adjudicator = role(RoleName::Adjudicator);
    assert_eq!(adjudicator.fresh_session, Some(true));
    assert_eq!(adjudicator.when, Some(RoleTrigger::ReviewersDisagree));
    assert_eq!(
        layer_of(&resolved, "roles.owner.workers").as_deref(),
        Some("project")
    );
}

#[test]
fn judges_adjudicator_and_approver_default_to_fresh_sessions() {
    let repo = Repo::new();
    repo.project(
        "[review]\nper_card = 2\n[roles.judge]\nworkers = [\"codex\"]\n\
         [roles.adjudicator]\nwhen = \"reviewers_disagree\"\n\
         [roles.approver]\nfresh_session = false\n[roles.owner]\nworkers = [\"codex\"]\n",
    );
    let resolved = resolve(&repo.sources()).unwrap();
    let fresh = |role| resolved.settings.roles[&role].fresh_session;
    assert_eq!(fresh(RoleName::Judge), Some(true));
    assert_eq!(fresh(RoleName::Adjudicator), Some(true));
    assert_eq!(fresh(RoleName::Approver), Some(false), "yours wins");
    assert_eq!(fresh(RoleName::Owner), None);
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);

    // An adjudicator settles reviewers who disagree, so one reviewer per card leaves it idle.
    repo.project("[roles.adjudicator]\nworkers = [\"codex\"]\n");
    let idle = resolve(&repo.sources()).unwrap();
    assert_eq!(idle.warnings.len(), 1, "{:?}", idle.warnings);
    assert!(idle.warnings[0].contains("roles.adjudicator"));
    repo.project("[roles.adjudicator]\nworkers = [\"codex\"]\n[roles.reviewer]\ncount = 2\n");
    assert!(resolve(&repo.sources()).unwrap().warnings.is_empty());
}

#[test]
fn unknown_role_workers_warn() {
    let repo = Repo::new();
    repo.project("[roles.critic]\nworkers = [\"my-agent/some-model\", \"opencode\"]\n");
    let resolved = resolve(&repo.sources()).unwrap();
    assert_eq!(resolved.warnings.len(), 1, "{:?}", resolved.warnings);
    assert!(resolved.warnings[0].contains("my-agent/some-model"));
    assert!(resolved.warnings[0].contains("[[harness]]"));
}

/// A `models.toml` holding one `[[harness]]` named `id`.
fn harness_entry(id: &str) -> String {
    format!(
        "[[harness]]\nid = \"{id}\"\ndisplay_name = \"{id}\"\nbinary = \"{id}\"\n\
         family = \"from-model\"\ntransport = \"acp\"\n"
    )
}

#[test]
fn declared_harnesses_count_as_known() {
    let repo = Repo::new();
    repo.project(
        "[roles.critic]\nworkers = [\"my-agent/some-model\", \"home-agent\"]\n\
         [handoff.replace.my-agent]\nwith = [\"codex\"]\n\
         [roles.owner.handoff.replace.home-agent]\nwith = [\"codex\"]\n",
    );
    let unknown = resolve(&repo.sources()).unwrap().warnings;
    assert_eq!(unknown.len(), 4, "{unknown:?}");
    assert!(
        unknown
            .iter()
            .all(|warning| warning.contains("models.toml"))
    );

    // The project's file and your own each declare one. Only the names are read, so the
    // project's counts before you trust it.
    fs::write(repo.dot().join("models.toml"), harness_entry("my-agent")).unwrap();
    fs::write(repo.home().join("models.toml"), harness_entry("home-agent")).unwrap();
    let resolved = resolve(&repo.sources()).unwrap();
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);

    // A file that doesn't parse declares nothing here; loading the harnesses says why.
    fs::write(repo.dot().join("models.toml"), "[[harness]\n").unwrap();
    let broken = resolve(&repo.sources()).unwrap().warnings;
    assert_eq!(broken.len(), 2, "{broken:?}");
    assert!(broken.iter().all(|warning| warning.contains("my-agent")));
}

#[test]
fn a_role_naming_a_harness_that_is_off_warns() {
    let repo = Repo::new();
    let warnings = |text: &str| {
        repo.project(text);
        resolve(&repo.trusted()).unwrap().warnings
    };
    let owner = "[roles.owner]\nworkers = [\"hermes/some-model\"]\n";

    let off = warnings(owner);
    assert_eq!(
        off,
        [
            "roles.owner.workers names hermes/some-model, which is off, so a run finds no harness \
          for it; turn it on with agents.hermes.enabled = true"
        ]
    );
    // Setting other keys for it doesn't turn it on, and an explicit false keeps it off.
    let binary = format!("{owner}[agents.hermes]\nbinary = \"/opt/hermes\"\n");
    assert_eq!(warnings(&binary), off);
    let disabled = format!("{owner}[agents.hermes]\nenabled = false\n");
    assert_eq!(warnings(&disabled), off);

    let on = format!("{owner}[agents.hermes]\nenabled = true\n");
    assert!(warnings(&on).is_empty(), "{:?}", warnings(&on));
    repo.project(owner);
    let flagged = resolve(&repo.with_flags(&["agents.hermes.enabled=true"])).unwrap();
    assert!(flagged.warnings.is_empty(), "{:?}", flagged.warnings);

    let default = warnings("default_agent = \"hermes\"\n");
    assert_eq!(default.len(), 1, "{default:?}");
    assert!(default[0].starts_with("default_agent names hermes, which is off"));

    // Your own [[harness]] of that name runs in its place, on or off.
    fs::write(repo.home().join("models.toml"), harness_entry("hermes")).unwrap();
    assert!(warnings(owner).is_empty());
}

#[test]
fn a_summarizer_nobody_knows_or_that_is_off_warns() {
    let repo = Repo::new();
    let warnings = |text: &str| {
        repo.project(text);
        resolve(&repo.trusted()).unwrap().warnings
    };
    assert!(warnings("[handoff]\nsummarizer = \"ollama/llama3.2\"\n").is_empty());
    assert!(warnings("[roles.owner.handoff]\nsummarizer = \"codex\"\n").is_empty());
    assert_eq!(
        warnings("[handoff]\nsummarizer = \"nobody/some-model\"\n"),
        [
            "handoff.summarizer names nobody/some-model, which isn't a harness or endpoint \
          Argonauts knows, and no models.toml declares it as a [[harness]]"
        ]
    );
    let off = warnings("[roles.reviewer.handoff]\nsummarizer = \"hermes\"\n");
    assert_eq!(off.len(), 1, "{off:?}");
    assert!(off[0].starts_with("roles.reviewer.handoff.summarizer names hermes, which is off"));
}

#[test]
fn endpoint_rejected_in_harness_roles() {
    let repo = Repo::new();
    let cases = [
        (
            "[roles.owner]\nworkers = [\"local/some-model\"]\n",
            "roles.owner.workers",
            "owner",
        ),
        (
            "[roles.restater]\nworkers = [\"codex\", \"local\"]\n",
            "roles.restater.workers",
            "restater",
        ),
        (
            "[roles.drafter.handoff]\nfallback = [\"codex\", { worker = \"local/some-model\" }]\n",
            "roles.drafter.handoff.fallback",
            "drafter",
        ),
        (
            "[roles.spike.handoff.replace.codex]\nwith = [\"local/some-model\"]\n",
            "roles.spike.handoff.replace.codex.with",
            "spike",
        ),
        (
            "[roles.default.handoff]\nfallback = [\"local/some-model\"]\n",
            "roles.default.handoff.fallback",
            "default",
        ),
        (
            "default_agent = \"local/some-model\"\n",
            "default_agent",
            "default agent",
        ),
    ];
    for (text, key, role) in cases {
        repo.global(&format!("{text}[endpoints.local]\nlocal = true\n"));
        let (layer, message) = invalid(resolve(&repo.sources()));
        assert_eq!(layer, repo.global_file(), "{text}");
        assert!(message.starts_with(key), "{message}");
        assert!(
            message.contains(role) && message.contains("local"),
            "{message}"
        );
    }

    // Steps with no tools may use an endpoint, and so may the top-level lists, which each
    // handoff filters for the role it's for.
    repo.global(
        "[handoff]\nfallback = [\"local/some-model\"]\n\
         [handoff.replace.codex]\nwith = [\"local/some-model\"]\n\
         [roles.reviewer]\nworkers = [\"local/some-model\"]\n\
         [roles.judge.handoff]\nfallback = [\"local/other-model\"]\n\
         [endpoints.local]\nlocal = true\n",
    );
    let resolved = resolve(&repo.sources()).unwrap();
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
}

/// The example from the handoff options, with placeholders for every model.
const HANDOFF: &str = r#"
[handoff]
mode = "auto"
strategy = "first"
on_return = "ask"
checkpoint = true
merge_rule = "upgrade_review"
unknown_tier = "weaker"
max_handoffs = 3

[handoff.replace.claude-code]
with = ["codex/<model>", "antigravity-cli/<model>"]
strategy = "spread"

[handoff.replace.codex]
with = ["claude-code/sonnet"]

[handoff.replace.antigravity-cli]
with = ["claude-code/sonnet", { worker = "opencode/<model>", max_budget_usd = 2.0 }]

[roles.reviewer.handoff]
fallback = ["codex/<model>"]
mode = "wait"

[roles.owner.handoff.replace.claude-code]
with = ["codex/<model>"]
on_return = "review"
"#;

#[test]
fn handoff_settings_validate() {
    let repo = Repo::new();
    repo.project(HANDOFF);
    let resolved = resolve(&repo.sources()).unwrap();
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
    let settings = &resolved.settings;
    let role = |name| Some(&settings.roles[&name].handoff);
    let claude = VendorId("claude-code".to_owned());
    let owner = HandoffSettings::applied(&settings.handoff, role(RoleName::Owner), &claude);
    assert_eq!(owner.candidates.len(), 1);
    assert_eq!(owner.on_return, OnReturn::Review);
    assert_eq!(owner.strategy, HandoffStrategy::Spread);
    assert_eq!(owner.mode, HandoffMode::Auto, "copied from the top level");
    let reviewer = HandoffSettings::applied(&settings.handoff, role(RoleName::Reviewer), &claude);
    assert_eq!(reviewer.mode, HandoffMode::Wait, "the role's own mode wins");
    assert_eq!(reviewer.candidates[0].worker().to_string(), "codex/<model>");
    assert_eq!(
        layer_of(&resolved, "roles.owner.handoff.mode").as_deref(),
        Some("project"),
        "a copy keeps the layer it came from"
    );

    let bad = [
        (
            "[handoff]\nstrategy = \"race\"\nrace_count = 1\n",
            "handoff.race_count",
        ),
        (
            "[roles.owner.handoff]\nmax_handoffs = 0\n",
            "roles.owner.handoff.max_handoffs",
        ),
        (
            "[handoff]\nfallback = [{ worker = \"codex\", max_budget_usd = -1.0 }]\n",
            "handoff.fallback[0].max_budget_usd",
        ),
        (
            "[roles.reviewer.handoff]\nask_timeout = \"soon\"\n",
            "roles.reviewer.handoff.ask_timeout",
        ),
        (
            "[handoff]\nfallback = [{ workr = \"codex\" }]\n",
            "handoff.fallback",
        ),
        ("[handoff]\nstrategy = \"sideways\"\n", "handoff.strategy"),
        ("[handoff]\ncontext = \"transcript\"\n", "handoff.context"),
        (
            "[roles.owner.handoff.replace.codex]\ncontext = \"everything\"\n",
            "roles.owner.handoff.replace",
        ),
        (
            "[roles.owner.handoff]\nsummarizer = \"not a worker\"\n",
            "roles.owner.handoff.summarizer",
        ),
        (
            "[roles.owner.handoff.replace.codex]\nwith = [\"not a worker\"]\n",
            "roles.owner.handoff.replace",
        ),
    ];
    for (text, key) in bad {
        repo.project(text);
        let (layer, message) = invalid(resolve(&repo.sources()));
        assert_eq!(layer, repo.project_file(), "{text}");
        assert!(message.starts_with(key), "{key}: {message}");
    }
    // The same through a flag names the flag.
    repo.project("");
    let (layer, _) = invalid(resolve(&SettingsSources {
        overrides: vec!["handoff.race_count=1".into()],
        ..repo.sources()
    }));
    assert_eq!(layer, "--set handoff.race_count=1");

    // The built-in harness profiles are vendors like any other.
    for vendor in ["hermes", "opencode-acp"] {
        repo.project(&format!(
            "[handoff.replace.{vendor}]\nwith = [\"codex\"]\n\
             [roles.owner.handoff.replace.{vendor}]\nwith = [\"codex\"]\n"
        ));
        let resolved = resolve(&repo.sources()).unwrap_or_else(|err| panic!("{vendor}: {err}"));
        assert!(
            resolved.warnings.is_empty(),
            "{vendor}: {:?}",
            resolved.warnings
        );
    }
    // A vendor nobody knows may be a harness from models.toml, which resolve can't see.
    repo.project("[roles.owner.handoff.replace.nobody]\nwith = [\"codex\"]\n");
    let resolved = resolve(&repo.sources()).unwrap();
    assert!(
        resolved.warnings.iter().any(|warning| warning
            .starts_with("roles.owner.handoff.replace.nobody")
            && warning.contains("models.toml")),
        "{:?}",
        resolved.warnings
    );
}
