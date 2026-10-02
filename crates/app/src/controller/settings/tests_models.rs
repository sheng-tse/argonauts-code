//! The model lists behind `/model` and `/effort`: only installed CLIs, each model's own effort
//! levels, the background refresh, and the scope card's one note for a session override.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use argonauts_core::app::{AppEvent, ModelChoice, ViewData};
use argonauts_core::model_picker::ModelsLevel;
use argonauts_core::names::RoleName;
use argonauts_core::registry::ModelRegistry as _;
use argonauts_core::settings::ui::NarratorMode;
use argonauts_core::settings::{SettingScope, Settings};
use argonauts_registry::models::discovery::{claude_code_entries, parse_model_pages};
use argonauts_registry::models::{FileRegistry, ModelCache};
use argonauts_testkit::{FakeDirectory, FakeWorker};

use super::tests::Fixture;
use super::*;
use crate::controller::Internal;

const CODEX_MODELS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../registry/tests/fixtures/codex/0.158.0/debug-models.json"
);

/// A stand-in `codex` that lists the recorded models for `codex debug models` and nothing else.
fn fake_codex(dir: &Path) -> PathBuf {
    let path = dir.join("codex");
    let script =
        format!("#!/bin/sh\n[ \"$*\" = \"debug models\" ] || exit 2\ncat '{CODEX_MODELS}'\n");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Claude Code and Codex installed and agy not, with Codex listing its models through the fake,
/// and the list kept in `cache`.
fn two_installed(fixture: &mut Fixture, dir: &Path) -> PathBuf {
    let cache = dir.join("home").join("models-cache.json");
    fixture.ctx.services.registry = registry(dir, &cache);
    let directory = Arc::new(FakeDirectory::new());
    directory.add(FakeWorker::harness("claude-code", "claude"));
    directory.add(FakeWorker::harness("codex", "gpt"));
    fixture.ctx.services.workers = directory;
    cache
}

/// agy is off here, so discovery never runs a real one.
fn registry(dir: &Path, cache: &Path) -> Arc<FileRegistry> {
    let mut settings = Settings::default();
    settings.agents.get_mut("codex").unwrap().binary = Some(fake_codex(dir));
    settings.agents.get_mut("antigravity-cli").unwrap().enabled = false;
    Arc::new(FileRegistry::from_files(&settings, None, None).with_cache(cache))
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
    only_claude.add(FakeWorker::harness("claude-code", "claude"));
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
    // A model whose levels aren't known yet gets its harness's usual ones.
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
        },
    ] = updates.as_slice()
    else {
        panic!("one update to the default agent's list: {updates:#?}");
    };
    let offered = workers(choices);
    assert!(
        offered.contains(&"codex/gpt-6-sol".to_owned()),
        "{offered:?}"
    );
    // Each harness's models stay together, Claude Code's aliases first.
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
    "/../registry/tests/fixtures/anthropic/2026-10-01"
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
        std::fs::write(
            repo.global_config(),
            "default_agent = \"claude-code/opus\"\n",
        )
        .unwrap();
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
            .take_while(|c| c.worker.worker == "claude-code")
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
        .find(|entry| entry.id.worker == "codex" && entry.cli_default)
        .and_then(|entry| entry.id.model)
        .expect("Codex marks its default");
    assert_eq!(
        offered(&choices, "codex").note,
        Some(format!("runs {marked}"))
    );
    let codex: Vec<&ModelChoice> = choices
        .iter()
        .filter(|c| c.worker.worker == "codex")
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
        std::fs::write(repo.global_config(), "default_agent = \"codex/gpt-5.5\"\n").unwrap();
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

#[tokio::test]
async fn the_scope_card_names_a_session_override_once() {
    let mut fixture = Fixture::new(false, |_| {});
    narrator_mode(&mut fixture.ctx, &mut fixture.state, NarratorMode::Live)
        .await
        .unwrap();
    let id = fixture.ctx.next_choice_id();
    let card = scope::scope_question(
        &fixture.ctx,
        id,
        "ui.narrator",
        "\"quiet\"",
        SettingScope::Global,
    );
    let said = |text: &str| text.contains("override sets this until you quit");
    assert_eq!(
        card.body.iter().filter(|line| said(line)).count(),
        1,
        "{card:#?}"
    );
    assert!(
        card.options
            .iter()
            .all(|option| option.detail.as_deref().is_none_or(|detail| !said(detail))),
        "{card:#?}"
    );
}

/// Claude Code's own picker on 2026-10-01: Default with what it runs, the
/// aliases on their own, then the models newest first with the catalog's words and levels.
#[tokio::test]
async fn the_models_level_reads_like_claude_codes_picker() {
    let dir = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(false, |repo| {
        std::fs::write(
            repo.global_config(),
            "default_agent = \"claude-code/claude-opus-5-5\"\n[roles.default]\neffort = \"max\"\n",
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
    settings.agents.get_mut("antigravity-cli").unwrap().enabled = false;
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
            "GPT-6-Astra",
            "GPT-6-Sol",
            "GPT-6-Luna",
            "GPT-5.6-Sol",
            "GPT-5.6-Terra",
            "GPT-5.6-Luna",
            "GPT-5.5"
        ]
    );
    assert_eq!(
        level.default.as_ref().unwrap().runs.as_deref(),
        Some("GPT-6-Astra")
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
