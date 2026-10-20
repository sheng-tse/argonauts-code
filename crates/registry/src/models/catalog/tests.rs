use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

use argonauts_core::names::ModelRef;
use argonauts_core::registry::ModelRegistry;
use argonauts_core::settings::Settings;
use time::macros::datetime;

use super::*;
use crate::models::files::parse_models_text;
use crate::models::{FileRegistry, ModelCache};

const SHIPPED_HASH: &str = include_str!("../../../catalog/models.toml.sha256");
const CODEX: &str = include_str!("../../../tests/fixtures/codex/debug-models.json");
const AGY: &str = include_str!("../../../tests/fixtures/agy/models.json");

fn id(text: &str) -> ModelRef {
    text.parse().unwrap()
}

#[test]
fn the_shipped_catalog_parses_and_matches_its_published_hash() {
    let catalog = parse_catalog(SHIPPED).unwrap();
    assert_eq!(catalog.version, CATALOG_VERSION);
    assert_eq!(shipped_catalog(), &catalog);
    assert_eq!(
        SHIPPED_HASH.trim(),
        sha256_hex(SHIPPED.as_bytes()),
        "update catalog/models.toml.sha256 after editing the catalog"
    );
}

/// The models as Claude Code's picker lists them.
#[test]
fn the_shipped_catalog_reads_like_claude_codes_picker() {
    let catalog = shipped_catalog();
    let default = catalog.label(&id("claude-code"));
    assert!(default.recommended);
    assert_eq!(
        default.description.as_deref(),
        Some("Best for everyday, complex tasks")
    );
    assert_eq!(
        catalog
            .entry(&id("claude-code"))
            .and_then(|entry| entry.runs.as_deref()),
        Some("claude-opus-5-5")
    );

    let mut models: Vec<&CatalogEntry> = catalog
        .models
        .iter()
        .filter(|entry| entry.id.agent == "claude-code" && entry.description.is_some())
        .filter(|entry| entry.id.model.is_some())
        .collect();
    models.sort_by_key(|entry| entry.rank);
    let rows: Vec<(String, &str)> = models
        .iter()
        .map(|entry| {
            let label = catalog.label(&entry.id);
            (label.name, entry.description.as_deref().unwrap_or_default())
        })
        .collect();
    let rows: Vec<(&str, &str)> = rows.iter().map(|(n, d)| (n.as_str(), *d)).collect();
    assert_eq!(
        rows,
        [
            ("Opus 5.5", "For complex work and everyday tasks"),
            ("Fable 5.1", "For your toughest challenges"),
            ("Sonnet 5.5", "Most efficient for simpler tasks"),
            ("Haiku 5.5", "Fastest for quick answers"),
            ("Haiku 4.5", "Fastest for quick answers"),
            ("Sonnet 5", "Efficient for routine tasks"),
            ("Opus 5", "Best for everyday, complex tasks"),
            (
                "Fable 5",
                "Most capable for your hardest and longest-running tasks"
            ),
            ("Opus 4.8", "Best for everyday, complex tasks"),
            ("Opus 4.7", "Best for everyday, complex tasks"),
            ("Opus 4.6", "Best for everyday, complex tasks"),
            ("Sonnet 4.6", "Efficient for routine tasks"),
        ]
    );

    let opus = catalog.entry(&id("claude-code/opus")).unwrap();
    assert_eq!(opus.default_effort.as_deref(), Some("xhigh"));
    for entry in &models[1..] {
        let expected = match entry.id.model.as_deref() {
            Some("claude-haiku-4-5-20251001") => None,
            Some("claude-haiku-5-5") => Some("medium"),
            _ => Some("high"),
        };
        assert_eq!(entry.default_effort.as_deref(), expected, "{}", entry.id);
    }
    // `haiku` runs Haiku 5.5, which takes every level; Haiku 4.5 takes none.
    let haiku = catalog.entry(&id("claude-code/haiku")).unwrap();
    assert_eq!(haiku.id.model.as_deref(), Some("claude-haiku-5-5"));
    assert_eq!(
        haiku.efforts.as_deref().map(<[String]>::len),
        Some(5),
        "Haiku 5.5 takes low to max"
    );
    let older = catalog.entry(&id("claude-code/claude-haiku-4-5")).unwrap();
    assert_eq!(
        older.efforts.as_deref(),
        Some(&[][..]),
        "Haiku 4.5 takes no effort"
    );
    for alias in ["opus", "sonnet", "fable", "haiku"] {
        assert!(
            catalog.is_alias(&id(&format!("claude-code/{alias}"))),
            "{alias}"
        );
    }
}

#[test]
fn a_catalog_with_a_bad_level_or_a_control_character_is_refused() {
    for (text, why) in [
        (
            "version = 99",
            "its format 99 is newer than this Argonauts Code reads",
        ),
        (
            "[[model]]\nid = \"codex/x\"\nefforts = [\"turbo\"]",
            "effort \"turbo\" isn't one of",
        ),
        (
            "[[model]]\nid = \"codex/x\"\nefforts = [\"low\"]\ndefault_effort = \"high\"",
            "isn't one of its efforts",
        ),
        (
            "[[model]]\nid = \"codex/x\"\ndescription = \"Fast\\u001b[31m\"",
            "holds a control character",
        ),
        (
            "[[model]]\nid = \"codex/x\"\nname = \"  \"",
            "name is empty",
        ),
        (
            "[[effort]]\nlevel = \"max\"\ndescription = \"line one\\nline two\"",
            "holds a control character",
        ),
    ] {
        let error = parse_catalog(text).unwrap_err();
        assert!(error.contains(why), "{text}: {error}");
    }
}

#[test]
fn codex_and_agy_name_their_own_models_in_their_own_order() {
    let mut discovered = crate::models::discovery::parse_codex_models(CODEX).unwrap();
    discovered.extend(crate::models::discovery::parse_agy_models(AGY).unwrap());
    let catalog = layered(None, &discovered, &[], &[]);

    let sol = catalog.label(&id("codex/gpt-6-sol"));
    assert_eq!(sol.name, "GPT 6 Sol", "spelled as its id spells it");
    assert_eq!(
        sol.description.as_deref(),
        Some("Previous generation workhorse model.")
    );
    assert!(!sol.new);
    let entry = catalog.entry(&id("codex/gpt-5.5")).unwrap();
    assert_eq!(
        entry.efforts.as_deref(),
        Some(&["low", "medium", "high", "xhigh"].map(String::from)[..])
    );
    assert_eq!(entry.default_effort.as_deref(), Some("medium"));

    let mut codex: Vec<ModelRef> = discovered
        .iter()
        .filter(|entry| entry.id.agent == "codex")
        .map(|entry| entry.id.clone())
        .rev()
        .collect();
    codex.sort_by(|a, b| catalog.newest_first(a, b));
    let names: Vec<String> = codex.iter().map(|id| catalog.label(id).name).collect();
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
        ],
        "Codex's own picker order"
    );

    let flash = catalog.label(&id("antigravity-cli/gemini-3.8-flash-high"));
    assert_eq!(flash.name, "Gemini 3.8 Flash (High)");
    let entry = catalog
        .entry(&id("antigravity-cli/gemini-3.8-flash-high"))
        .unwrap();
    assert_eq!(entry.rank, Some(1));
    // Its level is in its name, so it takes that one only.
    assert_eq!(entry.efforts.as_deref(), Some(&["high".to_owned()][..]));
    assert_eq!(entry.default_effort.as_deref(), Some("high"));
    // A model agy lists without a level takes none.
    let thinking = catalog
        .entry(&id("antigravity-cli/claude-opus-4-6-thinking"))
        .unwrap();
    assert_eq!(
        (&thinking.efforts, &thinking.default_effort),
        (&Some(Vec::new()), &None)
    );
}

#[test]
fn your_models_toml_wins_over_every_other_layer() {
    let text = r#"
        [[model]]
        id = "claude-code/claude-opus-5-5"
        description = "My usual"
        default_effort = "max"
        tier = 5

        [[model]]
        id = "codex/gpt-6-sol"
        name = "Sol"

        [[model]]
        id = "ollama/qwen3-coder:30b"
        provider = "alibaba"
        kind = "endpoint"
        access = "local"
        cost = { unit = "none" }
        description = "Runs on the laptop"
        efforts = ["low", "high"]

        [[effort]]
        level = "max"
        description = "Only for proofs"
    "#;
    let yours = parse_models_text(std::path::Path::new("models.toml"), text).unwrap();
    assert_eq!(yours.tiers.len(), 1, "a description can rate the model too");
    assert_eq!(yours.models.len(), 1);
    assert_eq!(yours.catalog.models.len(), 2);

    let discovered = crate::models::discovery::parse_codex_models(CODEX).unwrap();
    let fetched: ModelCatalog = parse_catalog(
        "[[model]]\nid = \"claude-code/claude-opus-5-5\"\ndescription = \"Fetched\"\n\
         [[model]]\nid = \"codex/gpt-6-sol\"\nname = \"Fetched Sol\"\n",
    )
    .unwrap();
    let catalog = layered(Some(&fetched), &discovered, &[], &[yours]);

    let opus = catalog.entry(&id("claude-code/claude-opus-5-5")).unwrap();
    assert_eq!(opus.description.as_deref(), Some("My usual"));
    assert_eq!(opus.default_effort.as_deref(), Some("max"));
    assert_eq!(
        opus.rank,
        Some(1),
        "the rest still comes from the shipped catalog"
    );
    assert_eq!(catalog.label(&id("codex/gpt-6-sol")).name, "Sol");
    let qwen = catalog.entry(&id("ollama/qwen3-coder:30b")).unwrap();
    assert_eq!(qwen.description.as_deref(), Some("Runs on the laptop"));
    assert_eq!(
        catalog
            .effort_note(&id("claude-code/opus"), "max")
            .as_deref(),
        Some("Only for proofs")
    );

    let fetched_only = layered(Some(&fetched), &[], &[], &[]);
    assert_eq!(
        fetched_only
            .label(&id("claude-code/claude-opus-5-5"))
            .description
            .as_deref(),
        Some("Fetched"),
        "the fetched catalog wins over the shipped one"
    );
}

#[test]
fn a_bad_description_in_your_file_says_which_entry() {
    let text = "[[model]]\nid = \"codex/gpt-6-sol\"\ndefault_effort = \"turbo\"\n";
    let error = parse_models_text(std::path::Path::new("models.toml"), text).unwrap_err();
    let error = error.to_string();
    assert!(error.contains("[[model]] codex/gpt-6-sol"), "{error}");
    assert!(error.contains("turbo"), "{error}");
}

/// A server on this machine in place of GitHub, answering `routes` and counting requests.
fn serve(routes: Vec<(&'static str, String)>) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (seen, requests) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut buffer = [0; 8192];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
            let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
            let _ = seen.send(path.clone());
            let body = routes
                .iter()
                .find(|(route, _)| *route == path)
                .map(|(_, body)| body.clone());
            let status = if body.is_some() {
                "200 OK"
            } else {
                "404 Not Found"
            };
            let body = body.unwrap_or_default();
            let head = format!(
                "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body.as_bytes());
        }
    });
    (base, requests)
}

const NEWER: &str = "version = 1\n\n[[model]]\nid = \"claude-code/claude-opus-6\"\n\
                     description = \"Shipped after the release\"\nrank = 0\n";

fn source(base: &str) -> CatalogSource {
    CatalogSource {
        url: format!("{base}/catalog/models.toml"),
    }
}

#[tokio::test]
async fn the_fetch_checks_the_hash_and_skips_an_unchanged_catalog() {
    let hash = sha256_hex(NEWER.as_bytes());
    let (base, requests) = serve(vec![
        ("/catalog/models.toml", NEWER.to_owned()),
        (
            "/catalog/models.toml.sha256",
            format!("{hash}  models.toml\n"),
        ),
    ]);
    let now = datetime!(2026-10-01 09:00 UTC);
    let (fetched, catalog) = source(&base).fetch(None, now).await.unwrap();
    assert_eq!(fetched.sha256, hash);
    assert_eq!(fetched.fetched, now);
    assert_eq!(
        catalog
            .label(&id("claude-code/claude-opus-6"))
            .description
            .as_deref(),
        Some("Shipped after the release")
    );
    let paths: Vec<String> = requests.try_iter().collect();
    assert_eq!(
        paths,
        ["/catalog/models.toml.sha256", "/catalog/models.toml"]
    );

    let later = datetime!(2026-10-02 09:00 UTC);
    let (again, _) = source(&base).fetch(Some(&fetched), later).await.unwrap();
    assert_eq!(again.fetched, later);
    let paths: Vec<String> = requests.try_iter().collect();
    assert_eq!(
        paths,
        ["/catalog/models.toml.sha256"],
        "an unchanged hash needs no download"
    );
}

#[tokio::test]
async fn a_catalog_that_fails_its_hash_or_needs_a_newer_build_is_refused() {
    let (base, _) = serve(vec![
        ("/bad/catalog/models.toml", NEWER.to_owned()),
        ("/bad/catalog/models.toml.sha256", "0".repeat(64)),
        ("/newer/catalog/models.toml", "version = 2\n".to_owned()),
        (
            "/newer/catalog/models.toml.sha256",
            sha256_hex(b"version = 2\n"),
        ),
        ("/nohash/catalog/models.toml", NEWER.to_owned()),
        ("/nohash/catalog/models.toml.sha256", "not found".to_owned()),
    ]);
    let now = datetime!(2026-10-01 09:00 UTC);
    for (path, why) in [
        ("bad", "doesn't match the hash published with it"),
        ("newer", "newer than this Argonauts Code reads"),
        ("nohash", "holds no SHA-256"),
        ("missing", "answered 404"),
    ] {
        let error = source(&format!("{base}/{path}"))
            .fetch(None, now)
            .await
            .unwrap_err();
        assert!(error.contains(why), "{path}: {error}");
    }
}

#[test]
fn a_cached_catalog_counts_only_while_it_matches_its_hash() {
    let fetched = FetchedCatalog {
        fetched: datetime!(2026-10-01 09:00 UTC),
        sha256: sha256_hex(NEWER.as_bytes()),
        text: NEWER.to_owned(),
    };
    assert!(fetched.catalog().is_ok());
    let edited = FetchedCatalog {
        text: NEWER.replace("after", "before"),
        ..fetched
    };
    assert_eq!(
        edited.catalog().unwrap_err(),
        "the catalog doesn't match its hash"
    );
}

/// Every CLI and local server off, so nothing but the catalog is fetched and no CLI is run.
fn quiet_settings() -> Settings {
    let mut settings = Settings::default();
    settings.registry.local_servers = false;
    for agent in settings.agents.values_mut() {
        agent.enabled = false;
    }
    settings
}

#[tokio::test]
async fn the_registry_fetches_the_catalog_once_a_day_and_keeps_it_in_the_cache() {
    let hash = sha256_hex(NEWER.as_bytes());
    let (base, requests) = serve(vec![
        ("/catalog/models.toml", NEWER.to_owned()),
        ("/catalog/models.toml.sha256", hash.clone()),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("models-cache.json");
    let settings = quiet_settings();
    let opus6 = id("claude-code/claude-opus-6");

    let registry = FileRegistry::from_files(&settings, None, None)
        .with_cache(&cache)
        .with_catalog_source(source(&base));
    assert!(
        registry.catalog().label(&opus6).new,
        "only the shipped one so far"
    );
    registry.discover().await;
    assert_eq!(
        registry.catalog().label(&opus6).description.as_deref(),
        Some("Shipped after the release")
    );
    let kept = ModelCache::read(&cache).unwrap().catalog.unwrap();
    assert_eq!(kept.sha256, hash);
    assert_eq!(requests.try_iter().count(), 2);

    // The next start reads it from the cache, and within the day fetches nothing.
    let next = FileRegistry::from_files(&settings, None, None)
        .with_cache(&cache)
        .with_catalog_source(source(&base));
    assert!(!next.catalog().label(&opus6).new);
    next.discover().await;
    assert_eq!(requests.try_iter().count(), 0);

    // A failed fetch keeps the catalog fetched before and says why.
    let mut stale = ModelCache::read(&cache).unwrap();
    stale.catalog.as_mut().unwrap().fetched -= CATALOG_FRESH_FOR;
    stale.write(&cache).unwrap();
    let (gone, _) = serve(Vec::new());
    let offline = FileRegistry::from_files(&settings, None, None)
        .with_cache(&cache)
        .with_catalog_source(source(&gone));
    offline.discover().await;
    assert!(!offline.catalog().label(&opus6).new);
    let skipped = offline.discovery_skipped();
    assert!(
        skipped
            .iter()
            .any(|why| why.starts_with("Argonauts Code's model catalog: GET")),
        "{skipped:?}"
    );
}

/// Every model the shipped catalog lists has a list price in it, and a newer catalog's price
/// replaces the older one's.
#[test]
fn every_shipped_model_has_a_list_price() {
    let catalog = shipped_catalog();
    for entry in &catalog.models {
        let Some(model) = entry.id.model.as_deref() else {
            continue;
        };
        assert!(
            catalog
                .prices
                .iter()
                .any(|price| model.starts_with(price.model.as_str())),
            "{model} has no price"
        );
    }
    let newer = parse_catalog(
        "version = 1\n[[price]]\nmodel = \"claude-opus-5-5\"\ninput = 3.0\noutput = 15.0\n",
    )
    .unwrap();
    let mut merged = catalog.clone();
    merged.merge(newer);
    let opus: Vec<_> = merged
        .prices
        .iter()
        .filter(|price| price.model == "claude-opus-5-5")
        .collect();
    assert_eq!(opus.len(), 1);
    assert_eq!(opus[0].input.0, 3.0);
    assert!(
        parse_catalog("[[price]]\nmodel = \"x\"\ninput = -1.0\noutput = 1.0\n").is_err(),
        "a negative price is refused"
    );
}
