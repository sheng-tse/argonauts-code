//! The model lists behind `/model` and `/effort`: only installed CLIs, each model's own effort
//! levels, the background refresh, and the scope question's one note for a session override.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use argonauts_core::app::model_picker::{EffortSlider, ModelsLevel};
use argonauts_core::app::{AppEvent, ModelChoice, ViewData};
use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::input::SlashCommand;
use argonauts_core::models::registry::ModelRegistry as _;
use argonauts_core::settings::Settings;
use argonauts_registry::models::discovery::{claude_code_entries, parse_model_pages};
use argonauts_registry::models::{FileRegistry, ModelCache};
use argonauts_testkit::{FakeDirectory, FakeRunner};

use super::Fixture;
use crate::controller::Internal;
use crate::controller::settings::*;

const CODEX_MODELS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../registry/tests/fixtures/codex/debug-models.json"
);

/// A fake `codex` that lists the recorded models for `codex debug models` and nothing else.
fn fake_codex(dir: &Path) -> PathBuf {
    let path = dir.join("codex");
    let script =
        format!("#!/bin/sh\n[ \"$*\" = \"debug models\" ] || exit 2\ncat '{CODEX_MODELS}'\n");
    argonauts_testkit::write_script(&path, &script).unwrap();
    path
}

const AGY_MODELS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../registry/tests/fixtures/agy/models.json"
);

/// A fake `agy` that lists the recorded models and nothing else.
fn fake_agy(dir: &Path) -> PathBuf {
    let path = dir.join("agy");
    let script = format!(
        "#!/bin/sh\n[ \"$*\" = \"--output-format json models\" ] || exit 2\ncat '{AGY_MODELS}'\n"
    );
    argonauts_testkit::write_script(&path, &script).unwrap();
    path
}

/// Codex and agy installed, each listing its recorded models through a fake.
fn codex_and_agy(fixture: &mut Fixture, dir: &Path) {
    let cache = dir.join("home").join("models-cache.json");
    let mut settings = Settings::default();
    settings.agents.get_mut("codex").unwrap().binary = Some(fake_codex(dir));
    settings.agents.get_mut("antigravity-cli").unwrap().binary = Some(fake_agy(dir));
    settings.agents.get_mut("opencode").unwrap().enabled = false;
    settings.registry.local_servers = false;
    fixture.ctx.services.registry =
        Arc::new(FileRegistry::from_files(&settings, None, None).with_cache(&cache));
    let directory = Arc::new(FakeDirectory::new());
    directory.add(FakeRunner::cli("codex", "openai"));
    directory.add(FakeRunner::cli("antigravity-cli", "google"));
    fixture.ctx.services.workers = directory;
}

/// `/effort`'s slider for the main agent.
fn effort_slider(fixture: &Fixture) -> EffortSlider {
    let command = SlashCommand::Effort {
        role: None,
        level: None,
        ultracode: None,
    };
    match pickers::picker(&fixture.ctx, &command).as_slice() {
        [ViewData::Effort { slider }] => slider.clone(),
        other => panic!("/effort answers with its slider, not {other:?}"),
    }
}

/// Claude Code and Codex installed and agy not, with Codex listing its models through the fake,
/// and the list kept in `cache`.
fn two_installed(fixture: &mut Fixture, dir: &Path) -> PathBuf {
    let cache = dir.join("home").join("models-cache.json");
    fixture.ctx.services.registry = registry(dir, &cache);
    let directory = Arc::new(FakeDirectory::new());
    directory.add(FakeRunner::cli("claude-code", "anthropic"));
    directory.add(FakeRunner::cli("codex", "openai"));
    fixture.ctx.services.workers = directory;
    cache
}

/// agy, OpenCode and the model servers here are off, so discovery never asks a real one.
fn registry(dir: &Path, cache: &Path) -> Arc<FileRegistry> {
    let mut settings = Settings::default();
    settings.agents.get_mut("codex").unwrap().binary = Some(fake_codex(dir));
    discovers_codex_alone(&mut settings);
    Arc::new(FileRegistry::from_files(&settings, None, None).with_cache(cache))
}

fn discovers_codex_alone(settings: &mut Settings) {
    for cli in ["antigravity-cli", "opencode"] {
        settings.agents.get_mut(cli).unwrap().enabled = false;
    }
    settings.registry.local_servers = false;
}

fn workers(choices: &[ModelChoice]) -> Vec<String> {
    choices.iter().map(|c| c.worker.to_string()).collect()
}

fn efforts<'a>(choices: &'a [ModelChoice], worker: &str) -> &'a [String] {
    &choices
        .iter()
        .find(|c| c.worker.to_string() == worker)
        .unwrap_or_else(|| panic!("{worker} isn't offered"))
        .efforts
}

async fn refresh_lands(fixture: &mut Fixture) {
    let message = tokio::time::timeout(Duration::from_secs(20), fixture.internal.recv()).await;
    assert!(matches!(message, Ok(Some(Internal::ModelsRefreshed))));
}

#[tokio::test]
async fn model_lists_only_installed_clis() {
    let dir = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(false, |_| {});
    two_installed(&mut fixture, dir.path());
    let offered = workers(&models::choices(&fixture.ctx, None));
    assert!(
        offered.contains(&"claude-code/opus".to_owned()),
        "{offered:?}"
    );
    assert!(offered.contains(&"codex".to_owned()), "{offered:?}");
    assert!(
        !offered.iter().any(|w| w.starts_with("antigravity-cli")),
        "{offered:?}"
    );

    let only_claude = Arc::new(FakeDirectory::new());
    only_claude.add(FakeRunner::cli("claude-code", "anthropic"));
    fixture.ctx.services.workers = only_claude;
    let offered = workers(&models::choices(&fixture.ctx, Some(RoleName::Reviewer)));
    assert!(
        !offered.iter().any(|w| w.starts_with("codex")),
        "{offered:?}"
    );
}

#[tokio::test]
async fn a_refresh_brings_the_open_model_list_up_to_date() {
    let dir = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(false, |_| {});
    let cache = two_installed(&mut fixture, dir.path());

    refresh_models(&fixture.ctx, &mut fixture.state);
    let ViewData::Models {
        choices,
        refreshed,
        refreshing,
        update,
        ..
    } = refresh::view(&fixture.ctx, &mut fixture.state, None)
    else {
        panic!("not a model list");
    };
    assert_eq!((refreshed, refreshing, update), (None, true, false));
    assert!(!workers(&choices).iter().any(|w| w.starts_with("codex/")));
    // A model whose levels aren't known yet gets its CLI's usual ones.
    assert_eq!(
        efforts(&choices, "codex"),
        ["low", "medium", "high", "xhigh", "max"]
    );

    refresh_lands(&mut fixture).await;
    fixture.drain();
    models_refreshed(&fixture.ctx, &mut fixture.state).unwrap();
    let updates: Vec<ViewData> = fixture
        .drain()
        .into_iter()
        .filter_map(|event| match event {
            AppEvent::View { data } => Some(*data),
            _ => None,
        })
        .collect();
    let [
        ViewData::Models {
            choices,
            role: None,
            refreshed: Some(refreshed),
            refreshing: false,
            update: true,
            sources: _,
            memory: _,
        },
    ] = updates.as_slice()
    else {
        panic!("one update to the main agent's list: {updates:#?}");
    };
    let offered = workers(choices);
    assert!(
        offered.contains(&"codex/gpt-6-sol".to_owned()),
        "{offered:?}"
    );
    // Each CLI's models stay together, Claude Code's aliases first.
    let last_claude = offered.iter().rposition(|w| w.starts_with("claude-code"));
    let first_codex = offered.iter().position(|w| w.starts_with("codex"));
    assert!(last_claude < first_codex, "{offered:?}");
    // Codex reports levels per model.
    assert_eq!(
        efforts(choices, "codex/gpt-5.5"),
        ["low", "medium", "high", "xhigh"]
    );
    assert!(efforts(choices, "codex/gpt-6-sol").contains(&"ultra".to_owned()));

    // Nothing newer, nothing sent.
    models_refreshed(&fixture.ctx, &mut fixture.state).unwrap();
    assert!(fixture.drain().is_empty());

    // The next start lists the cached models before its own refresh lands.
    let next = registry(dir.path(), &cache);
    assert_eq!(next.refreshed(), Some(*refreshed));
    assert!(next.get(&"codex/gpt-6-sol".parse().unwrap()).is_some());
}

const ANTHROPIC_PAGES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../registry/tests/fixtures/anthropic"
);

/// A cache holding Claude Code's models as Anthropic's saved pages list them, read within the
/// day, so the registry lists them and fetches nothing.
fn cache_with_claude_models(cache: &Path) {
    let page = |name: &str| std::fs::read_to_string(format!("{ANTHROPIC_PAGES}/{name}")).unwrap();
    let listed = parse_model_pages(&page("models-overview.md"), &page("model-deprecations.md"));
    let now = time::OffsetDateTime::now_utc();
    ModelCache {
        refreshed: now,
        models: claude_code_entries(&Settings::default(), listed),
        skipped: Vec::new(),
        claude_listed: Some(now),
        catalog: None,
        sources: Vec::new(),
    }
    .write(cache)
    .unwrap();
}

fn offered<'a>(choices: &'a [ModelChoice], worker: &str) -> &'a ModelChoice {
    choices
        .iter()
        .find(|c| c.worker.to_string() == worker)
        .unwrap_or_else(|| panic!("{worker} isn't offered"))
}

#[tokio::test]
async fn each_cli_lists_its_default_aliases_models_then_older_ones() {
    let dir = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(false, |repo| {
        std::fs::write(repo.global_config(), "crew.main = \"claude-code/opus\"\n").unwrap();
    });
    let cache = dir.path().join("home").join("models-cache.json");
    cache_with_claude_models(&cache);
    two_installed(&mut fixture, dir.path());

    let choices = models::choices(&fixture.ctx, None);
    let claude: Vec<String> = workers(&choices)
        .into_iter()
        .filter(|w| w.starts_with("claude-code"))
        .collect();
    assert_eq!(
        claude[..10],
        [
            "claude-code",
            "claude-code/opus",
            "claude-code/sonnet",
            "claude-code/fable",
            "claude-code/haiku",
            "claude-code/claude-opus-5-5",
            "claude-code/claude-fable-5-1",
            "claude-code/claude-sonnet-5-5",
            "claude-code/claude-haiku-4-5-20251001",
            "claude-code/claude-mythos-5-1",
        ]
    );
    assert_eq!(claude.len(), 5 + 22, "{claude:?}");
    let first_older = choices.iter().position(|c| c.older).unwrap();
    assert_eq!(
        choices[first_older].worker.to_string(),
        "claude-code/claude-sonnet-5"
    );
    assert!(
        choices[first_older..]
            .iter()
            .take_while(|c| c.worker.agent == "claude-code")
            .all(|c| c.older),
        "older models come last in their CLI's group"
    );

    let opus = offered(&choices, "claude-code/opus");
    assert!(opus.current);
    assert_eq!(opus.note.as_deref(), Some("newest Opus"));
    let retired = offered(&choices, "claude-code/claude-3-7-sonnet-20250219");
    assert_eq!(
        (retired.note.as_deref(), retired.older),
        (Some("retired"), true)
    );
    let deprecated = offered(&choices, "claude-code/claude-sonnet-4-5-20250929");
    assert_eq!((deprecated.note.as_deref(), deprecated.older), (None, true));
    assert_eq!(offered(&choices, "claude-code").note, None);

    // Once Codex lists its models, its default names the one it runs.
    refresh_models(&fixture.ctx, &mut fixture.state);
    refresh_lands(&mut fixture).await;
    let choices = models::choices(&fixture.ctx, None);
    let marked = fixture
        .ctx
        .services
        .registry
        .list()
        .into_iter()
        .find(|entry| entry.id.agent == "codex" && entry.cli_default)
        .and_then(|entry| entry.id.model)
        .expect("Codex marks its default");
    assert_eq!(
        offered(&choices, "codex").note,
        Some(format!("runs {marked}"))
    );
    let codex: Vec<&ModelChoice> = choices
        .iter()
        .filter(|c| c.worker.agent == "codex")
        .collect();
    assert_eq!(
        codex[0].worker.to_string(),
        "codex",
        "the default comes first"
    );
}

#[tokio::test]
async fn a_cli_model_you_set_is_what_its_default_runs() {
    let dir = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(false, |repo| {
        std::fs::write(
            repo.global_config(),
            "[agents.claude-code]\nmodel = \"sonnet\"\n",
        )
        .unwrap();
    });
    two_installed(&mut fixture, dir.path());
    let choices = models::choices(&fixture.ctx, Some(RoleName::Reviewer));
    assert_eq!(
        offered(&choices, "claude-code").note.as_deref(),
        Some("runs sonnet")
    );
}

#[tokio::test]
async fn effort_offers_the_levels_the_chosen_model_reports() {
    let dir = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(false, |repo| {
        std::fs::write(repo.global_config(), "crew.main = \"codex/gpt-5.5\"\n").unwrap();
    });
    two_installed(&mut fixture, dir.path());
    let (worker, levels) = models::efforts(&fixture.ctx, None);
    assert_eq!(worker.unwrap().to_string(), "codex/gpt-5.5");
    assert_eq!(levels, ["low", "medium", "high", "xhigh", "max"]);

    refresh_models(&fixture.ctx, &mut fixture.state);
    refresh_lands(&mut fixture).await;
    let (_, levels) = models::efforts(&fixture.ctx, None);
    assert_eq!(levels, ["low", "medium", "high", "xhigh"]);
}

/// Claude Code's own picker on 2026-10-01: Default with what it runs, the
/// aliases on their own, then the models newest first with the catalog's words and levels.
#[tokio::test]
async fn the_models_level_reads_like_claude_codes_picker() {
    let dir = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(false, |repo| {
        std::fs::write(
            repo.global_config(),
            "crew.main = \"claude-code/claude-opus-5-5\"\n[roles.main]\neffort = \"max\"\n",
        )
        .unwrap();
    });
    let cache = dir.path().join("home").join("models-cache.json");
    cache_with_claude_models(&cache);
    let mut models = ModelCache::read(&cache).unwrap();
    let mut opus6 = models
        .models
        .iter()
        .find(|entry| entry.id.to_string() == "claude-code/claude-opus-5-5")
        .unwrap()
        .clone();
    opus6.id = "claude-code/claude-opus-6".parse().unwrap();
    // Discovery lists Claude Code's aliases first, as models of their own.
    let aliases = ["opus", "sonnet", "fable", "haiku"].map(|alias| {
        let mut entry = opus6.clone();
        entry.id = format!("claude-code/{alias}").parse().unwrap();
        entry.efforts = Vec::new();
        entry.default_effort = None;
        entry.aliases = Vec::new();
        entry
    });
    models.models.push(opus6);
    models.models.splice(0..0, aliases);
    models.write(&cache).unwrap();
    two_installed(&mut fixture, dir.path());

    let choices = models::choices(&fixture.ctx, None);
    let level = ModelsLevel::new("claude-code", &choices);

    let default = level.default.as_ref().unwrap();
    assert_eq!(default.name, "");
    assert!(default.recommended);
    assert_eq!(
        default.description.as_deref(),
        Some("Best for everyday, complex tasks")
    );
    assert_eq!(default.runs.as_deref(), Some("Opus 5.5"));
    assert_eq!(default.default_effort.as_deref(), Some("xhigh"));

    let aliases: Vec<(&str, Option<&str>)> = level
        .aliases
        .iter()
        .map(|alias| (alias.name.as_str(), alias.runs.as_deref()))
        .collect();
    assert_eq!(
        aliases,
        [
            ("Opus", Some("Opus 5.5")),
            ("Sonnet", Some("Sonnet 5.5")),
            ("Fable", Some("Fable 5.1")),
            ("Haiku", Some("Haiku 4.5")),
        ]
    );
    let haiku = offered(&choices, "claude-code/haiku");
    assert!(haiku.efforts.is_empty(), "Haiku takes no effort setting");

    let names: Vec<&str> = level.models.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(
        names[..17],
        [
            "Opus 6",
            "Opus 5.5",
            "Fable 5.1",
            "Sonnet 5.5",
            "Haiku 4.5",
            "Mythos 5.1",
            "Sonnet 5",
            "Opus 5",
            "Fable 5",
            "Opus 4.8",
            "Opus 4.7",
            "Opus 4.6",
            "Sonnet 4.6",
            "Mythos 5",
            "Opus 4.5",
            "Sonnet 4.5",
            "Mythos Preview",
        ]
    );
    let fresh = &level.models[0];
    assert!(fresh.new, "the catalog doesn't know it yet");
    assert_eq!(fresh.description, None);
    let opus = offered(&choices, "claude-code/claude-opus-5-5");
    assert!(opus.current);
    assert!(!opus.new);
    assert_eq!(
        opus.description.as_deref(),
        Some("For complex work and everyday tasks")
    );
    assert_eq!(opus.default_effort.as_deref(), Some("xhigh"));
    assert_eq!(opus.effort.as_deref(), Some("max"));
    let sonnet = offered(&choices, "claude-code/claude-sonnet-4-6");
    assert_eq!(sonnet.efforts, ["low", "medium", "high", "max"]);
    assert_eq!(sonnet.default_effort.as_deref(), Some("high"));
    assert!(!sonnet.new, "an older model is never new");
    assert!(
        choices
            .iter()
            .all(|choice| !choice.new || choice.worker.model.is_some()),
        "a CLI's default is never new"
    );
}

#[tokio::test]
async fn codex_names_its_models_and_your_models_toml_wins() {
    let dir = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(false, |_| {});
    let cache = two_installed(&mut fixture, dir.path());
    let yours = dir.path().join("models.toml");
    std::fs::write(
        &yours,
        "[[model]]\nid = \"codex/gpt-6-sol\"\ndescription = \"My usual\"\n\
         efforts = [\"low\", \"high\"]\n",
    )
    .unwrap();
    let mut settings = Settings::default();
    settings.agents.get_mut("codex").unwrap().binary = Some(fake_codex(dir.path()));
    discovers_codex_alone(&mut settings);
    fixture.ctx.services.registry =
        Arc::new(FileRegistry::from_files(&settings, Some(&yours), None).with_cache(&cache));

    refresh_models(&fixture.ctx, &mut fixture.state);
    refresh_lands(&mut fixture).await;
    let choices = models::choices(&fixture.ctx, None);
    let level = ModelsLevel::new("codex", &choices);
    let names: Vec<&str> = level.models.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "GPT 6 Astra",
            "GPT 6 Sol",
            "GPT 6 Luna",
            "GPT 5.6 Sol",
            "GPT 5.6 Terra",
            "GPT 5.6 Luna",
            "GPT 5.5"
        ]
    );
    assert_eq!(
        level.default.as_ref().unwrap().runs.as_deref(),
        Some("GPT 6 Astra")
    );
    let luna = offered(&choices, "codex/gpt-6-luna");
    assert_eq!(
        luna.description.as_deref(),
        Some("Fast and affordable model for easier tasks.")
    );
    assert_eq!(luna.default_effort.as_deref(), Some("medium"));
    assert!(level.models.iter().all(|model| !model.new));
    let sol = offered(&choices, "codex/gpt-6-sol");
    assert_eq!(sol.description.as_deref(), Some("My usual"));
    assert_eq!(sol.efforts, ["low", "high"]);
    assert_eq!(
        sol.default_effort, None,
        "Codex's medium isn't one of the levels you gave it"
    );
}

/// With no effort set, Codex's models start where its own list says: its default runs
/// gpt-6-astra at low, and gpt-6-luna starts at medium.
#[tokio::test]
async fn codex_models_start_at_the_level_codex_lists_for_each() {
    for (agent, model, level) in [
        ("codex", "gpt-6-astra", "low"),
        ("codex/gpt-6-luna", "gpt-6-luna", "medium"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut fixture = Fixture::new(false, |repo| {
            let config = format!("crew.main = \"{agent}\"\n");
            std::fs::write(repo.global_config(), config).unwrap();
        });
        codex_and_agy(&mut fixture, dir.path());
        refresh_models(&fixture.ctx, &mut fixture.state);
        refresh_lands(&mut fixture).await;

        let choices = models::choices(&fixture.ctx, None);
        let choice = offered(&choices, agent);
        assert_eq!(choice.default_effort.as_deref(), Some(level), "{agent}");
        assert_eq!(choice.effort, None);

        let slider = effort_slider(&fixture);
        assert_eq!(slider.worker.to_string(), agent);
        assert_eq!(slider.level(), Some(level), "{agent}");
        assert!(slider.is_default() && !slider.cli_default, "{agent}");
        assert!(!slider.is_fixed());

        let (header, effort) = crate::controller::header_agent(&fixture.ctx);
        assert_eq!(header.to_string(), format!("codex/{model}"));
        assert_eq!(effort.as_deref(), Some(level), "{agent}");
    }
}

/// agy names each thinking level as a model of its own, so a model's level is fixed by its
/// name: the picker, `/effort`, and the header show it, with nothing to adjust. A model named
/// without one takes none, until a recording shows agy's `--effort` works for it.
#[tokio::test]
async fn an_agy_model_named_for_a_level_runs_at_it_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(false, |repo| {
        std::fs::write(
            repo.global_config(),
            "crew.main = \"antigravity-cli/gemini-3.8-flash-high\"\n",
        )
        .unwrap();
    });
    codex_and_agy(&mut fixture, dir.path());
    refresh_models(&fixture.ctx, &mut fixture.state);
    refresh_lands(&mut fixture).await;

    let choices = models::choices(&fixture.ctx, None);
    for (model, level) in [
        ("gemini-3.8-flash-high", "high"),
        ("gemini-3.6-flash-low", "low"),
        ("gpt-oss-120b-medium", "medium"),
    ] {
        let choice = offered(&choices, &format!("antigravity-cli/{model}"));
        assert_eq!(choice.efforts, [level], "{model}");
        assert_eq!(choice.default_effort.as_deref(), Some(level), "{model}");
    }
    for model in ["claude-opus-4-6-thinking", "claude-sonnet-4-6"] {
        let choice = offered(&choices, &format!("antigravity-cli/{model}"));
        assert!(choice.efforts.is_empty(), "{model}: {:?}", choice.efforts);
    }

    let slider = effort_slider(&fixture);
    assert!(slider.is_fixed());
    assert_eq!(slider.level(), Some("high"));
    assert!(!slider.cli_default);

    let (header, effort) = crate::controller::header_agent(&fixture.ctx);
    assert_eq!(header.to_string(), "antigravity-cli/gemini-3.8-flash-high");
    assert_eq!(effort.as_deref(), Some("high"));
}

/// `haiku` runs Haiku 5.5, which takes an effort, so the header names it and keeps the level
/// your settings give Claude Code.
#[tokio::test]
async fn haiku_is_haiku_5_5_and_the_header_keeps_its_effort() {
    let mut fixture = Fixture::new(false, |repo| {
        let config = "crew.main = \"claude-code\"\n\n[agents.claude-code]\nmodel = \"haiku\"\n\
                      effort = \"low\"\n";
        std::fs::write(repo.global_config(), config).unwrap();
    });
    refresh_models(&fixture.ctx, &mut fixture.state);
    let (header, effort) = crate::controller::header_agent(&fixture.ctx);
    assert_eq!(header.to_string(), "claude-code/haiku");
    assert_eq!(effort.as_deref(), Some("low"));
    let names = argonauts_core::look::display_names::DisplayNames {
        catalog: fixture.ctx.services.registry.catalog(),
        ..Default::default()
    };
    assert_eq!(
        names.model("claude-code", Some("haiku")).as_deref(),
        Some("Haiku 5.5")
    );
}

/// The main agent's conversation session, as a turn builds it, with `/fast` on or off.
fn conversation_session(
    fixture: &Fixture,
    fast: bool,
) -> argonauts_core::agents::session_runner::SessionSpec {
    use crate::controller::turns::turn::session_spec;
    use argonauts_core::base::names::SessionKey;

    conversation_turn(fixture, fast, |launch| {
        let dir = fixture.ctx.env.state_dir.join("conversation-test");
        session_spec(&fixture.ctx, launch, SessionKey::numbered(1), dir)
    })
}

/// What `then` makes of a question's turn on the main agent, with `/fast` on or off.
fn conversation_turn<T>(
    fixture: &Fixture,
    fast: bool,
    then: impl FnOnce(&crate::controller::turns::turn::Launch<'_>) -> T,
) -> T {
    use crate::controller::turns::turn::{Launch, TurnStep};
    use argonauts_core::base::names::{ConversationId, TurnId};

    let (conversation, turn) = (ConversationId::numbered(1), TurnId::numbered(1));
    let step = TurnStep::Ask { run_status: None };
    let worker = fixture.ctx.settings().crew.main.clone();
    let launch = Launch {
        conversation: &conversation,
        title: "",
        turn: &turn,
        step: &step,
        text: "what does main do?",
        pictures: Vec::new(),
        handle: Arc::new(FakeRunner::cli(&worker.to_string(), "gemini")),
        worker,
        resume: None,
        fork: false,
        excerpt: None,
        notes: Vec::new(),
        shell: None,
        fast,
        workdir: fixture.ctx.env.repo_root.clone(),
        outside: Default::default(),
    };
    then(&launch)
}

/// A level set before a model fixed at one was picked stays set, but never reaches the model:
/// the header and the conversation's session say high on gemini-3.8-flash-high whether `low` is the
/// main role's or agy's own, as `/effort` and `/model` do.
#[tokio::test]
async fn a_level_set_before_never_reaches_a_model_fixed_at_one() {
    for set in [
        "[roles.main]\neffort = \"low\"\n",
        "[agents.antigravity-cli]\neffort = \"low\"\n",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut fixture = Fixture::new(false, |repo| {
            let config = format!("crew.main = \"antigravity-cli/gemini-3.8-flash-high\"\n{set}");
            std::fs::write(repo.global_config(), config).unwrap();
        });
        codex_and_agy(&mut fixture, dir.path());
        refresh_models(&fixture.ctx, &mut fixture.state);
        refresh_lands(&mut fixture).await;

        let (header, effort) = crate::controller::header_agent(&fixture.ctx);
        assert_eq!(header.to_string(), "antigravity-cli/gemini-3.8-flash-high");
        assert_eq!(effort.as_deref(), Some("high"), "{set}");
        let spec = conversation_session(&fixture, false);
        assert_eq!(
            spec.model.to_string(),
            "antigravity-cli/gemini-3.8-flash-high"
        );
        assert_eq!(spec.effort.as_deref(), Some("high"), "{set}");
        assert_eq!(effort_slider(&fixture).level(), Some("high"));
    }
}

/// A main model agy no longer lists runs as the nearest it does, at that model's level, the
/// header names that one, and the turn says so first in the line a run's job says it in.
#[tokio::test]
async fn a_main_model_agy_no_longer_lists_runs_its_nearest_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(false, |repo| {
        let config = "crew.main = \"antigravity-cli/gemini-3.6-flash-low\"\n";
        std::fs::write(repo.global_config(), config).unwrap();
    });
    codex_and_agy(&mut fixture, dir.path());
    agy_after_an_update(dir.path());
    refresh_models(&fixture.ctx, &mut fixture.state);
    refresh_lands(&mut fixture).await;
    fixture.drain();

    let spec = conversation_session(&fixture, false);
    assert_eq!(
        spec.model.to_string(),
        "antigravity-cli/gemini-3.8-flash-low"
    );
    assert_eq!(spec.effort.as_deref(), Some("low"));
    let (header, effort) = crate::controller::header_agent(&fixture.ctx);
    assert_eq!(header, spec.model);
    assert_eq!(effort, spec.effort);

    conversation_turn(&fixture, false, |launch| {
        crate::controller::turns::turn::say_in_place(&fixture.ctx, launch);
    });
    let notices: Vec<_> = fixture
        .drain()
        .into_iter()
        .filter_map(|event| match event {
            AppEvent::Notice { level, text } => Some((level, text)),
            _ => None,
        })
        .collect();
    let line = "This conversation runs on Gemini 3.8 Flash (Low): agy no longer \
                offers Gemini 3.6 Flash (Low)";
    assert_eq!(
        notices,
        [(argonauts_core::NoteLevel::Warn, line.to_owned())]
    );
}

/// The fake `agy` lists what it does after an update that dropped its 3.6 and 3.7 models.
fn agy_after_an_update(dir: &Path) {
    let mut today: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(AGY_MODELS).unwrap()).unwrap();
    let models = today
        .pointer_mut("/command/data/models")
        .and_then(serde_json::Value::as_array_mut)
        .unwrap();
    models.retain(|model| {
        let id = model["id"].as_str().unwrap_or_default();
        !id.starts_with("gemini-3.6") && !id.starts_with("gemini-3.7")
    });
    let listed = dir.join("agy-models-today.json");
    std::fs::write(&listed, today.to_string()).unwrap();
    let script = format!(
        "#!/bin/sh\n[ \"$*\" = \"--output-format json models\" ] || exit 2\ncat '{}'\n",
        listed.display()
    );
    argonauts_testkit::write_script(&dir.join("agy"), &script).unwrap();
}

/// `/fast` on a model fixed at one level runs its line's lowest instead, as agy lists
/// gemini-3.8-flash-low beside gemini-3.8-flash-high. One already at its line's lowest, or one
/// that takes no effort setting, has nothing faster.
#[tokio::test]
async fn fast_on_an_agy_model_fixed_at_one_level_runs_its_lowest_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(false, |repo| {
        let config = "crew.main = \"antigravity-cli/gemini-3.8-flash-high\"\n";
        std::fs::write(repo.global_config(), config).unwrap();
    });
    codex_and_agy(&mut fixture, dir.path());
    refresh_models(&fixture.ctx, &mut fixture.state);
    refresh_lands(&mut fixture).await;

    let fast = |model: &str| {
        let worker: ModelRef = format!("antigravity-cli/{model}").parse().unwrap();
        crate::controller::turns::turn::fast_choice(&fixture.ctx, &worker)
            .map(|(worker, level)| (worker.to_string(), level))
    };
    let low = |model: &str| Some((format!("antigravity-cli/{model}"), "low".to_owned()));
    assert_eq!(fast("gemini-3.8-flash-high"), low("gemini-3.8-flash-low"));
    assert_eq!(fast("gemini-3.1-pro-high"), low("gemini-3.1-pro-low"));
    for model in [
        "gemini-3.8-flash-low",
        "gpt-oss-120b-medium",
        "claude-opus-4-6-thinking",
    ] {
        assert_eq!(fast(model), None, "{model}");
    }

    let spec = conversation_session(&fixture, true);
    assert_eq!(
        spec.model.to_string(),
        "antigravity-cli/gemini-3.8-flash-low"
    );
    assert_eq!(spec.effort.as_deref(), Some("low"));
}

/// Haiku 4.5 takes no effort setting, but the main agent's `/effort` still opens with
/// nothing to slide, since Ultracode lives there. A role on Haiku 4.5 lists the roles' efforts
/// as before.
#[test]
fn effort_on_a_main_agent_without_levels_still_offers_ultracode() {
    let fixture = Fixture::new(false, |repo| {
        let config = "crew.main = \"claude-code/claude-haiku-4-5\"\n\
                      ultracode = true\n\
                      [roles.reviewer]\nmodels = [\"claude-code/claude-haiku-4-5\"]\n";
        std::fs::write(repo.global_config(), config).unwrap();
    });
    let slider = effort_slider(&fixture);
    assert!(slider.stops.is_empty(), "{slider:?}");
    assert_eq!((slider.level(), slider.role), (None, None));
    assert_eq!(
        slider.worker,
        "claude-code/claude-haiku-4-5".parse().unwrap()
    );
    assert!(slider.model.starts_with("Haiku"), "{}", slider.model);
    assert_eq!(
        slider.ultracode,
        Some(argonauts_core::app::model_picker::UltracodeToggle::new(
            true
        ))
    );

    let command = SlashCommand::Effort {
        role: Some(RoleName::Reviewer),
        level: None,
        ultracode: None,
    };
    match pickers::picker(&fixture.ctx, &command).as_slice() {
        [ViewData::Settings { command, .. }] => assert_eq!(command.as_deref(), Some("effort")),
        other => panic!("a role on Haiku 4.5 lists the roles' efforts, not {other:?}"),
    }
}
