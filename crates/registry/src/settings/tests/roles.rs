use std::fs;

use argonauts_core::AuthMode;
use argonauts_core::base::names::{AgentId, RoleName};
use argonauts_core::settings::fallback::{FallbackMode, FallbackStrategy, OnReturn};
use argonauts_core::settings::roles::RoleTrigger;

use super::{Repo, invalid, layer_of};
use crate::{SettingsSources, resolve};

/// The orchestrator example team, in the TOML the settings take: one model per role is a
/// one-item `models` list, and each CLI's `auth` lives under `[agents.<id>]`.
const ORCHESTRATOR: &str = r#"
workflow = "orchestrator"

[agents.codex]
auth = "subscription"

[agents.claude-code]
auth = "subscription"

[roles.orchestrator]
models = ["codex"]

[roles.worker]
models = ["claude-code/opus"]

[roles.consultant]
models = ["claude-code/fable"]
on_demand = true
when = "asked"

[roles.reviewer]
blind = true
models = ["openrouter/<model-a>", "openrouter/<model-b>"]

[roles.judge]
models = ["codex"]
fresh_session = true
"#;

#[test]
fn roles_config_parses() {
    let repo = Repo::new();
    repo.project(ORCHESTRATOR);
    let resolved = resolve(&repo.trusted()).unwrap();
    // `openrouter` is a built-in endpoint, so its models raise no warning.
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
    let settings = &resolved.settings;
    assert_eq!(settings.workflow, "orchestrator");
    assert_eq!(settings.review.per_task, 2);
    assert_eq!(settings.agents["codex"].auth, AuthMode::Subscription);
    let role = |name: RoleName| &settings.roles[&name];
    let workers = |name: RoleName| -> Vec<String> {
        role(name).models.iter().map(ToString::to_string).collect()
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
        role(RoleName::Reviewer).models[1].model.as_deref(),
        Some("<model-b>")
    );
    assert_eq!(role(RoleName::Consultant).when, Some(RoleTrigger::Asked));
    assert_eq!(role(RoleName::Judge).fresh_session, Some(true));
    assert_eq!(
        layer_of(&resolved, "roles.worker.models").as_deref(),
        Some("project")
    );
}

#[test]
fn the_judge_defaults_to_a_fresh_session() {
    let repo = Repo::new();
    repo.project("[roles.judge]\nmodels = [\"codex\"]\n[roles.worker]\nmodels = [\"codex\"]\n");
    let resolved = resolve(&repo.sources()).unwrap();
    let fresh = |role| resolved.settings.roles[&role].fresh_session;
    assert_eq!(fresh(RoleName::Judge), Some(true));
    assert_eq!(fresh(RoleName::Worker), None);
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);

    repo.project("[roles.judge]\nfresh_session = false\n");
    let yours = resolve(&repo.sources()).unwrap();
    assert_eq!(
        yours.settings.roles[&RoleName::Judge].fresh_session,
        Some(false),
        "yours wins"
    );
}

#[test]
fn unknown_role_workers_warn() {
    let repo = Repo::new();
    repo.project("[roles.critic]\nmodels = [\"my-agent/some-model\", \"opencode\"]\n");
    let resolved = resolve(&repo.sources()).unwrap();
    assert_eq!(resolved.warnings.len(), 1, "{:?}", resolved.warnings);
    assert!(resolved.warnings[0].contains("my-agent/some-model"));
    assert!(resolved.warnings[0].contains("[[cli]]"));
}

/// A `models.toml` holding one `[[cli]]` named `id`.
fn cli_entry(id: &str) -> String {
    format!(
        "[[cli]]\nid = \"{id}\"\ndisplay_name = \"{id}\"\nbinary = \"{id}\"\n\
         provider = \"from-model\"\ntransport = \"acp\"\n"
    )
}

#[test]
fn declared_clis_count_as_known() {
    let repo = Repo::new();
    repo.project(
        "[roles.critic]\nmodels = [\"my-agent/some-model\", \"home-agent\"]\n\
         [fallback.replace.my-agent]\nwith = [\"codex\"]\n\
         [fallback.worker.replace.home-agent]\nwith = [\"codex\"]\n",
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
    fs::write(repo.dot().join("models.toml"), cli_entry("my-agent")).unwrap();
    fs::write(repo.home().join("models.toml"), cli_entry("home-agent")).unwrap();
    let resolved = resolve(&repo.sources()).unwrap();
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);

    // A file that doesn't parse declares nothing here; loading the CLIs says why.
    fs::write(repo.dot().join("models.toml"), "[[cli]\n").unwrap();
    let broken = resolve(&repo.sources()).unwrap().warnings;
    assert_eq!(broken.len(), 2, "{broken:?}");
    assert!(broken.iter().all(|warning| warning.contains("my-agent")));
}

#[test]
fn a_role_naming_a_cli_that_is_off_warns() {
    let repo = Repo::new();
    let warnings = |text: &str| {
        repo.project(text);
        resolve(&repo.trusted()).unwrap().warnings
    };
    let worker = "[roles.worker]\nmodels = [\"hermes/some-model\"]\n";

    let off = warnings(worker);
    assert_eq!(
        off,
        [
            "roles.worker.models names hermes/some-model, which is off, so a run finds no CLI \
          for it; turn it on with agents.hermes.enabled = true"
        ]
    );
    // Setting other keys for it doesn't turn it on, and an explicit false keeps it off.
    let binary = format!("{worker}[agents.hermes]\nbinary = \"/opt/hermes\"\n");
    assert_eq!(warnings(&binary), off);
    let disabled = format!("{worker}[agents.hermes]\nenabled = false\n");
    assert_eq!(warnings(&disabled), off);

    let on = format!("{worker}[agents.hermes]\nenabled = true\n");
    assert!(warnings(&on).is_empty(), "{:?}", warnings(&on));
    repo.project(worker);
    let flagged = resolve(&repo.with_flags(&["agents.hermes.enabled=true"])).unwrap();
    assert!(flagged.warnings.is_empty(), "{:?}", flagged.warnings);

    let default = warnings("crew.main = \"hermes\"\n");
    assert_eq!(default.len(), 1, "{default:?}");
    assert!(default[0].starts_with("crew.main names hermes, which is off"));

    // Your own [[cli]] of that name runs in its place, on or off.
    fs::write(repo.home().join("models.toml"), cli_entry("hermes")).unwrap();
    assert!(warnings(worker).is_empty());
}

/// Several fallbacks at once are for work that several can do side by side. In the table of
/// a role that takes one at a time, the setting warns and is read as 1.
#[test]
fn together_warns_in_the_table_of_a_role_that_takes_one_fallback() {
    let repo = Repo::new();
    let warnings = |text: &str| {
        repo.project(text);
        resolve(&repo.trusted()).unwrap().warnings
    };
    for table in ["fallback", "fallback.worker", "fallback.reviewer"] {
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
    for (table, role) in [
        ("orchestrator", "the orchestrator role"),
        ("judge", "the judge role"),
    ] {
        let said = warnings(&format!("[fallback.{table}]\ntogether = 3\n"));
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(
            said[0].starts_with(&format!(
                "fallback.{table}.together is 3, and {role} goes to one fallback at a time"
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
    assert!(warnings("[fallback.worker]\nsummarizer = \"codex\"\n").is_empty());
    assert_eq!(
        warnings("[fallback]\nsummarizer = \"nobody/some-model\"\n"),
        [
            "fallback.summarizer names nobody/some-model, which isn't a CLI or endpoint \
          Argonauts Code knows, and no models.toml declares it as a [[cli]]"
        ]
    );
    let off = warnings("[fallback.reviewer]\nsummarizer = \"hermes\"\n");
    assert_eq!(off.len(), 1, "{off:?}");
    assert!(off[0].starts_with("fallback.reviewer.summarizer names hermes, which is off"));
}

#[test]
fn endpoint_rejected_in_cli_roles() {
    let repo = Repo::new();
    let cases = [
        (
            "[roles.worker]\nmodels = [\"local/some-model\"]\n",
            "roles.worker.models",
            "worker",
        ),
        (
            "[roles.reader]\nmodels = [\"codex\", \"local\"]\n",
            "roles.reader.models",
            "reader",
        ),
        (
            "crew.main = \"local/some-model\"\n",
            "crew.main",
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
         [roles.reviewer]\nmodels = [\"local/some-model\"]\n\
         [fallback.judge]\norder = [\"local/other-model\"]\n\
         [fallback.main]\norder = [\"local/some-model\"]\n\
         [endpoints.local]\nlocal = true\n",
    );
    let resolved = resolve(&repo.sources()).unwrap();
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);

    // The workers' own lists only ever serve roles that need tools, so an endpoint there is
    // never picked. It loads, and says so.
    repo.global(
        "[fallback.worker]\norder = [\"codex\", { model = \"local/some-model\" }]\n\
         [fallback.worker.replace.codex]\nwith = [\"local/some-model\"]\n\
         [endpoints.local]\nlocal = true\n",
    );
    let resolved = resolve(&repo.sources()).unwrap();
    let said: Vec<&str> = resolved.warnings.iter().map(|w| w.text.as_str()).collect();
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(said[0].starts_with("fallback.worker.order names local/some-model, an endpoint"));
    assert!(said[1].starts_with("fallback.worker.replace.codex.with names local/some-model"));
}

/// The example from the fallback options, with placeholders for every model.
const FALLBACK: &str = r#"
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
with = ["claude-code/sonnet", { model = "opencode/<model>", max_spend_usd = 2.0 }]

[fallback.reviewer]
order = ["codex/<model>"]
mode = "wait"

[fallback.worker.replace.claude-code]
with = ["codex/<model>"]
on_return = "review"
"#;

#[test]
fn fallback_settings_validate() {
    let repo = Repo::new();
    repo.project(FALLBACK);
    let resolved = resolve(&repo.sources()).unwrap();
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
    let settings = &resolved.settings;
    let claude = AgentId("claude-code".to_owned());
    let worker = settings.fallback.applied_to(RoleName::Worker, &claude);
    assert_eq!(worker.candidates.len(), 1);
    assert_eq!(worker.on_return, OnReturn::Review);
    assert_eq!(worker.strategy, FallbackStrategy::Spread);
    assert_eq!(worker.mode, FallbackMode::Auto, "copied from the top level");
    // The experiment follows the workers, and the critic the reviewers.
    assert_eq!(
        settings.fallback.applied_to(RoleName::Experiment, &claude),
        worker
    );
    let reviewer = settings.fallback.applied_to(RoleName::Critic, &claude);
    assert_eq!(
        reviewer.mode,
        FallbackMode::Wait,
        "the role's own mode wins"
    );
    assert_eq!(reviewer.candidates[0].model().to_string(), "codex/<model>");
    assert_eq!(
        layer_of(&resolved, "fallback.worker.mode").as_deref(),
        Some("project"),
        "a copy keeps the layer it came from"
    );

    let bad = [
        ("[fallback]\ntogether = 0\n", "fallback.together"),
        (
            "[fallback.worker]\ntogether = 2\npick = \"merge\"\n",
            "fallback.worker.pick",
        ),
        (
            "[fallback.reviewer]\ntogether = 2\npick = \"first_passing\"\n",
            "fallback.reviewer.pick",
        ),
        ("[fallback]\npick = \"best\"\n", "fallback.pick"),
        (
            "[fallback.reviewer]\npick = \"best\"\n",
            "fallback.reviewer.pick",
        ),
        (
            "[fallback.worker]\nmax_fallbacks = 0\n",
            "fallback.worker.max_fallbacks",
        ),
        (
            "[fallback]\norder = [{ model = \"codex\", max_spend_usd = -1.0 }]\n",
            "fallback.order[0].max_spend_usd",
        ),
        (
            "[fallback.reviewer]\nask_timeout = \"soon\"\n",
            "fallback.reviewer.ask_timeout",
        ),
        (
            "[fallback]\norder = [{ workr = \"codex\" }]\n",
            "fallback.order",
        ),
        ("[fallback]\nstrategy = \"sideways\"\n", "fallback.strategy"),
        ("[fallback]\ncontext = \"transcript\"\n", "fallback.context"),
        (
            "[fallback.worker.replace.codex]\ncontext = \"everything\"\n",
            "fallback.worker.replace",
        ),
        (
            "[fallback.worker]\nsummarizer = \"not a worker\"\n",
            "fallback.worker.summarizer",
        ),
        (
            "[fallback.worker.replace.codex]\nwith = [\"not a worker\"]\n",
            "fallback.worker.replace",
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

    // The built-in CLI profiles are agents like any other.
    for agent_id in ["hermes", "opencode-acp"] {
        repo.project(&format!(
            "[fallback.replace.{agent_id}]\nwith = [\"codex\"]\n\
             [fallback.worker.replace.{agent_id}]\nwith = [\"codex\"]\n"
        ));
        let resolved = resolve(&repo.sources()).unwrap_or_else(|err| panic!("{agent_id}: {err}"));
        assert!(
            resolved.warnings.is_empty(),
            "{agent_id}: {:?}",
            resolved.warnings
        );
    }
    // An agent nobody knows may be a CLI from models.toml, which resolve can't see.
    repo.project("[fallback.worker.replace.nobody]\nwith = [\"codex\"]\n");
    let resolved = resolve(&repo.sources()).unwrap();
    assert!(
        resolved.warnings.iter().any(|warning| warning
            .starts_with("fallback.worker.replace.nobody")
            && warning.contains("models.toml")),
        "{:?}",
        resolved.warnings
    );
}
