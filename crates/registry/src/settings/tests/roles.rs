use std::fs;

use argonauts_core::AuthMode;
use argonauts_core::names::{RoleName, VendorId};
use argonauts_core::settings::handoff::{HandoffMode, HandoffStrategy, OnReturn};
use argonauts_core::settings::roles::RoleTrigger;

use super::{Repo, invalid, layer_of};
use crate::{SettingsSources, resolve};

/// The orchestrator example team, in the TOML the settings take: one worker per role is a
/// one-item `workers` list, and each harness's `auth` lives under `[agents.<id>]`.
const COORDINATED_CREW: &str = r#"
workflow = "orchestrator"

[agents.codex]
auth = "subscription"

[agents.claude-code]
auth = "subscription"

[roles.orchestrator]
workers = ["codex"]

[roles.worker]
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
    assert_eq!(settings.workflow, "orchestrator");
    assert_eq!(settings.review.per_task, 2);
    assert_eq!(settings.agents["codex"].auth, AuthMode::Subscription);
    let role = |name: RoleName| &settings.roles[&name];
    let workers = |name: RoleName| -> Vec<String> {
        role(name).workers.iter().map(ToString::to_string).collect()
    };
    assert_eq!(workers(RoleName::Orchestrator), ["codex"]);
    assert_eq!(workers(RoleName::Worker), ["claude-code/opus"]);
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
        layer_of(&resolved, "roles.worker.workers").as_deref(),
        Some("project")
    );
}

#[test]
fn judges_adjudicator_and_approver_default_to_fresh_sessions() {
    let repo = Repo::new();
    repo.project(
        "[review]\nper_task = 2\n[roles.judge]\nworkers = [\"codex\"]\n\
         [roles.adjudicator]\nwhen = \"reviewers_disagree\"\n\
         [roles.approver]\nfresh_session = false\n[roles.worker]\nworkers = [\"codex\"]\n",
    );
    let resolved = resolve(&repo.sources()).unwrap();
    let fresh = |role| resolved.settings.roles[&role].fresh_session;
    assert_eq!(fresh(RoleName::Judge), Some(true));
    assert_eq!(fresh(RoleName::Adjudicator), Some(true));
    assert_eq!(fresh(RoleName::Approver), Some(false), "yours wins");
    assert_eq!(fresh(RoleName::Worker), None);
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
         [fallback.replace.my-agent]\nwith = [\"codex\"]\n\
         [fallback.workers.replace.home-agent]\nwith = [\"codex\"]\n",
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
    let owner = "[roles.worker]\nworkers = [\"hermes/some-model\"]\n";

    let off = warnings(owner);
    assert_eq!(
        off,
        [
            "roles.worker.workers names hermes/some-model, which is off, so a run finds no CLI \
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

    let default = warnings("main_agent = \"hermes\"\n");
    assert_eq!(default.len(), 1, "{default:?}");
    assert!(default[0].starts_with("main_agent names hermes, which is off"));

    // Your own [[harness]] of that name runs in its place, on or off.
    fs::write(repo.home().join("models.toml"), harness_entry("hermes")).unwrap();
    assert!(warnings(owner).is_empty());
}

/// Several fallbacks at once are for work that several can do side by side. In the table of
/// a seat that takes one at a time, the setting warns and is read as 1.
#[test]
fn together_warns_in_the_table_of_a_seat_that_takes_one_fallback() {
    let repo = Repo::new();
    let warnings = |text: &str| {
        repo.project(text);
        resolve(&repo.trusted()).unwrap().warnings
    };
    for table in ["fallback", "fallback.workers", "fallback.reviewers"] {
        assert!(warnings(&format!("[{table}]\ntogether = 2\n")).is_empty());
    }
    assert!(warnings("[fallback.judge]\ntogether = 1\n").is_empty());
    assert_eq!(
        warnings("[fallback.main]\ntogether = 2\n"),
        [
            "fallback.main.together is 2, and the main role goes to one fallback at a time, \
             so it's read as 1; several at once are for what the workers build and plan, and \
             for reviews"
        ]
    );
    for (table, seat) in [
        ("orchestrator", "the orchestrator role"),
        ("judge", "the judge role"),
    ] {
        let said = warnings(&format!("[fallback.{table}]\ntogether = 3\n"));
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(
            said[0].starts_with(&format!(
                "fallback.{table}.together is 3, and {seat} goes to one fallback at a time"
            )),
            "{said:?}"
        );
    }
}

#[test]
fn a_summarizer_nobody_knows_or_that_is_off_warns() {
    let repo = Repo::new();
    let warnings = |text: &str| {
        repo.project(text);
        resolve(&repo.trusted()).unwrap().warnings
    };
    assert!(warnings("[fallback]\nsummarizer = \"ollama/llama3.2\"\n").is_empty());
    assert!(warnings("[fallback.workers]\nsummarizer = \"codex\"\n").is_empty());
    assert_eq!(
        warnings("[fallback]\nsummarizer = \"nobody/some-model\"\n"),
        [
            "fallback.summarizer names nobody/some-model, which isn't a CLI or endpoint \
          Argonauts knows, and no models.toml declares it as a [[harness]]"
        ]
    );
    let off = warnings("[fallback.reviewers]\nsummarizer = \"hermes\"\n");
    assert_eq!(off.len(), 1, "{off:?}");
    assert!(off[0].starts_with("fallback.reviewers.summarizer names hermes, which is off"));
}

#[test]
fn endpoint_rejected_in_harness_roles() {
    let repo = Repo::new();
    let cases = [
        (
            "[roles.worker]\nworkers = [\"local/some-model\"]\n",
            "roles.worker.workers",
            "worker",
        ),
        (
            "[roles.restater]\nworkers = [\"codex\", \"local\"]\n",
            "roles.restater.workers",
            "restater",
        ),
        (
            "main_agent = \"local/some-model\"\n",
            "main_agent",
            "main agent",
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

    // Steps with no tools may use an endpoint, and so may the fallback lists, which each
    // fallback filters for the role it's for.
    repo.global(
        "[fallback]\norder = [\"local/some-model\"]\n\
         [fallback.replace.codex]\nwith = [\"local/some-model\"]\n\
         [roles.reviewer]\nworkers = [\"local/some-model\"]\n\
         [fallback.judge]\norder = [\"local/other-model\"]\n\
         [fallback.main]\norder = [\"local/some-model\"]\n\
         [endpoints.local]\nlocal = true\n",
    );
    let resolved = resolve(&repo.sources()).unwrap();
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);

    // The workers' own lists only ever serve roles that need tools, so an endpoint there is
    // never picked. It loads, and says so.
    repo.global(
        "[fallback.workers]\norder = [\"codex\", { worker = \"local/some-model\" }]\n\
         [fallback.workers.replace.codex]\nwith = [\"local/some-model\"]\n\
         [endpoints.local]\nlocal = true\n",
    );
    let resolved = resolve(&repo.sources()).unwrap();
    let said: Vec<&str> = resolved.warnings.iter().map(String::as_str).collect();
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(said[0].starts_with("fallback.workers.order names local/some-model, an endpoint"));
    assert!(said[1].starts_with("fallback.workers.replace.codex.with names local/some-model"));
}

/// The example from the handoff options, with placeholders for every model.
const HANDOFF: &str = r#"
[fallback]
mode = "auto"
strategy = "first"
on_return = "ask"
checkpoint = true
merge_rule = "upgrade_review"
unknown_tier = "weaker"
max_fallbacks = 3

[fallback.replace.claude-code]
with = ["codex/<model>", "antigravity-cli/<model>"]
strategy = "spread"

[fallback.replace.codex]
with = ["claude-code/sonnet"]

[fallback.replace.antigravity-cli]
with = ["claude-code/sonnet", { worker = "opencode/<model>", max_spend_usd = 2.0 }]

[fallback.reviewers]
order = ["codex/<model>"]
mode = "wait"

[fallback.workers.replace.claude-code]
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
    let claude = VendorId("claude-code".to_owned());
    let owner = settings.fallback.applied_to(RoleName::Worker, &claude);
    assert_eq!(owner.candidates.len(), 1);
    assert_eq!(owner.on_return, OnReturn::Review);
    assert_eq!(owner.strategy, HandoffStrategy::Spread);
    assert_eq!(owner.mode, HandoffMode::Auto, "copied from the top level");
    // The spike follows the workers, and the critic the reviewers.
    assert_eq!(
        settings.fallback.applied_to(RoleName::Spike, &claude),
        owner
    );
    let reviewer = settings.fallback.applied_to(RoleName::Critic, &claude);
    assert_eq!(reviewer.mode, HandoffMode::Wait, "the role's own mode wins");
    assert_eq!(reviewer.candidates[0].worker().to_string(), "codex/<model>");
    assert_eq!(
        layer_of(&resolved, "fallback.workers.mode").as_deref(),
        Some("project"),
        "a copy keeps the layer it came from"
    );

    let bad = [
        ("[fallback]\ntogether = 0\n", "fallback.together"),
        (
            "[fallback.workers]\ntogether = 2\npick = \"merge\"\n",
            "fallback.workers.pick",
        ),
        (
            "[fallback.reviewers]\ntogether = 2\npick = \"first_passing\"\n",
            "fallback.reviewers.pick",
        ),
        ("[fallback]\npick = \"best\"\n", "fallback.pick"),
        (
            "[fallback.reviewers]\npick = \"best\"\n",
            "fallback.reviewers.pick",
        ),
        (
            "[fallback.workers]\nmax_fallbacks = 0\n",
            "fallback.workers.max_fallbacks",
        ),
        (
            "[fallback]\norder = [{ worker = \"codex\", max_spend_usd = -1.0 }]\n",
            "fallback.order[0].max_spend_usd",
        ),
        (
            "[fallback.reviewers]\nask_timeout = \"soon\"\n",
            "fallback.reviewers.ask_timeout",
        ),
        (
            "[fallback]\norder = [{ workr = \"codex\" }]\n",
            "fallback.order",
        ),
        ("[fallback]\nstrategy = \"sideways\"\n", "fallback.strategy"),
        ("[fallback]\ncontext = \"transcript\"\n", "fallback.context"),
        (
            "[fallback.workers.replace.codex]\ncontext = \"everything\"\n",
            "fallback.workers.replace",
        ),
        (
            "[fallback.workers]\nsummarizer = \"not a worker\"\n",
            "fallback.workers.summarizer",
        ),
        (
            "[fallback.workers.replace.codex]\nwith = [\"not a worker\"]\n",
            "fallback.workers.replace",
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
        overrides: vec!["fallback.together=0".into()],
        ..repo.sources()
    }));
    assert_eq!(layer, "--set fallback.together=0");

    // The built-in harness profiles are vendors like any other.
    for vendor in ["hermes", "opencode-acp"] {
        repo.project(&format!(
            "[fallback.replace.{vendor}]\nwith = [\"codex\"]\n\
             [fallback.workers.replace.{vendor}]\nwith = [\"codex\"]\n"
        ));
        let resolved = resolve(&repo.sources()).unwrap_or_else(|err| panic!("{vendor}: {err}"));
        assert!(
            resolved.warnings.is_empty(),
            "{vendor}: {:?}",
            resolved.warnings
        );
    }
    // A vendor nobody knows may be a harness from models.toml, which resolve can't see.
    repo.project("[fallback.workers.replace.nobody]\nwith = [\"codex\"]\n");
    let resolved = resolve(&repo.sources()).unwrap();
    assert!(
        resolved.warnings.iter().any(|warning| warning
            .starts_with("fallback.workers.replace.nobody")
            && warning.contains("models.toml")),
        "{:?}",
        resolved.warnings
    );
}
