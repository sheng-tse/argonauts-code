use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

use argonauts_core::base::names::ModelRef;
use argonauts_core::models::registry::ModelRegistry;
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
        "published = \"2026-12-01\"\n\
         [[model]]\nid = \"claude-code/claude-opus-5-5\"\ndescription = \"Fetched\"\n\
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

/// A server on this computer in place of the real one, answering `routes` and counting
/// requests.
fn serve(routes: Vec<(String, String)>) -> (String, mpsc::Receiver<String>) {
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

const NEWER: &str = "version = 1\npublished = \"2026-12-01\"\n\n[[model]]\n\
                     id = \"claude-code/claude-opus-6\"\n\
                     description = \"Shipped after the release\"\nrank = 0\n";

/// A copy signed before the build shipped: what it says is older than the build's own.
const OLDER: &str = "version = 1\npublished = \"2026-09-01\"\n\n[[model]]\n\
                     id = \"claude-code\"\ndescription = \"An older word\"\n";

/// A minisign key made for one test, never one that signs a release.
struct Signer {
    keys: minisign::KeyPair,
}

impl Signer {
    fn new() -> Self {
        Self {
            keys: minisign::KeyPair::generate_unencrypted_keypair().unwrap(),
        }
    }

    fn public(&self) -> String {
        self.keys.pk.to_base64()
    }

    /// The `.minisig` file for `text`.
    fn sign(&self, text: &str) -> String {
        minisign::sign(
            Some(&self.keys.pk),
            &self.keys.sk,
            std::io::Cursor::new(text.as_bytes()),
            Some("argonauts-code model catalog"),
            None,
        )
        .unwrap()
        .into_string()
    }
}

fn source(base: &str, signer: &Signer) -> CatalogSource {
    CatalogSource {
        url: format!("{base}/catalog/models.toml"),
        keys: vec![signer.public()],
    }
}

/// The catalog and its signature where a source at `base` + `prefix` looks for them.
fn signed(prefix: &'static str, text: &str, signer: &Signer) -> Vec<(String, String)> {
    vec![
        (format!("{prefix}/catalog/models.toml"), text.to_owned()),
        (
            format!("{prefix}/catalog/models.toml.minisig"),
            signer.sign(text),
        ),
    ]
}

#[tokio::test]
async fn the_fetch_checks_the_signature_and_skips_an_unchanged_catalog() {
    let signer = Signer::new();
    let (base, requests) = serve(signed("", NEWER, &signer));
    let now = datetime!(2026-10-01 09:00 UTC);
    let (fetched, catalog) = source(&base, &signer).fetch(None, now).await.unwrap();
    assert_eq!(fetched.sha256, sha256_hex(NEWER.as_bytes()));
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
        ["/catalog/models.toml.minisig", "/catalog/models.toml"]
    );

    let later = datetime!(2026-10-02 09:00 UTC);
    let (again, _) = source(&base, &signer)
        .fetch(Some(&fetched), later)
        .await
        .unwrap();
    assert_eq!(again.fetched, later);
    let paths: Vec<String> = requests.try_iter().collect();
    assert_eq!(
        paths,
        ["/catalog/models.toml.minisig"],
        "an unchanged signature needs no download"
    );
}

/// A signed copy published before the catalog in hand rolls nothing back: the fetch refuses
/// it, and layered over the shipped catalog it counts for nothing.
#[tokio::test]
async fn a_signed_copy_older_than_the_catalog_in_hand_is_refused() {
    let signer = Signer::new();
    let mut routes = signed("/older", OLDER, &signer);
    routes.extend(signed("/newer", NEWER, &signer));
    let (base, _) = serve(routes);
    let now = datetime!(2026-12-02 09:00 UTC);
    let older = source(&format!("{base}/older"), &signer)
        .fetch(None, now)
        .await
        .unwrap_err();
    assert!(
        older.contains("was published before the catalog"),
        "{older}"
    );
    let (newer, _) = source(&format!("{base}/newer"), &signer)
        .fetch(None, now)
        .await
        .unwrap();
    // A replay of the older copy after the newer one is refused too.
    let replayed = source(&format!("{base}/older"), &signer)
        .fetch(Some(&newer), now)
        .await
        .unwrap_err();
    assert!(replayed.contains("was published before"), "{replayed}");
    let catalog = layered(Some(&parse_catalog(OLDER).unwrap()), &[], &[], &[]);
    assert_ne!(
        catalog.label(&id("claude-code")).description.as_deref(),
        Some("An older word")
    );
}

#[tokio::test]
async fn a_catalog_another_key_signed_or_one_needing_a_newer_build_is_refused() {
    let signer = Signer::new();
    let stranger = Signer::new();
    let mut routes = signed("/forged", NEWER, &stranger);
    routes.extend(signed("/newer", "version = 2\n", &signer));
    routes.push(("/garbled/catalog/models.toml".to_owned(), NEWER.to_owned()));
    routes.push((
        "/garbled/catalog/models.toml.minisig".to_owned(),
        "not a signature".to_owned(),
    ));
    routes.push(("/unsigned/catalog/models.toml".to_owned(), NEWER.to_owned()));
    let (base, _) = serve(routes);
    let now = datetime!(2026-10-01 09:00 UTC);
    for (path, why) in [
        ("forged", "isn't signed by a key this Argonauts Code trusts"),
        (
            "garbled",
            "isn't signed by a key this Argonauts Code trusts",
        ),
        ("newer", "newer than this Argonauts Code reads"),
        ("unsigned", "answered 404"),
    ] {
        let error = source(&format!("{base}/{path}"), &signer)
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
        signature: String::new(),
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
    let signer = Signer::new();
    let (base, requests) = serve(signed("", NEWER, &signer));
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("models-cache.json");
    let settings = quiet_settings();
    let opus6 = id("claude-code/claude-opus-6");

    let registry = FileRegistry::from_files(&settings, None, None)
        .with_cache(&cache)
        .with_catalog_source(source(&base, &signer));
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
        .with_catalog_source(source(&base, &signer));
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
        .with_catalog_source(source(&gone, &signer));
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

/// The shipped catalog prices GPT-6.1-Sol as the built-in table does.
#[test]
fn the_shipped_catalog_prices_the_newest_sol() {
    let catalog = shipped_catalog();
    let price = catalog
        .prices
        .iter()
        .find(|price| price.model == "gpt-6.1-sol")
        .expect("a price for gpt-6.1-sol");
    let builtin =
        argonauts_core::models::pricing::builtin_price("gpt-6.1-sol").expect("a built-in price");
    assert_eq!(price.input.0, builtin.input_per_mtok);
    assert_eq!(price.output.0, builtin.output_per_mtok);
    assert_eq!(
        price.cache_read.map(|read| read.0),
        builtin.cached_input_per_mtok
    );
}

/// Every model the shipped catalog describes or ranks has a list price in it, and a newer
/// catalog's price replaces the older one's. A model it gives only a window, as one of agy's,
/// is listed for the Context tab alone.
#[test]
fn every_shipped_model_has_a_list_price() {
    let catalog = shipped_catalog();
    for entry in &catalog.models {
        let Some(model) = entry.id.model.as_deref() else {
            continue;
        };
        let window_only = CatalogEntry {
            window: entry.window,
            ..CatalogEntry::new(entry.id.clone())
        };
        if *entry == window_only {
            continue;
        }
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

const SAMPLE_LOCAL: &str = "version = 1\npublished = \"2026-12-01\"\n\n[[local]]\ntag = \"sample:14b\"\nname = \"Sample 14B\"\n\
                            window = 65536\nweights_gb = 9.0\ndownload_gb = 9.0\n\
                            context_kib_per_token = 32.0\nreleased = \"2026-09\"\n\
                            coding = { score = 45.0, source = \"a sample leaderboard\", \
                            checked = \"2026-10-20\" }\n";

/// The shipped catalog's local models each use tools and run at a window it ranks them at.
#[test]
fn the_shipped_catalog_lists_local_models_it_can_rank() {
    let local = &shipped_catalog().local;
    assert!(!local.is_empty());
    for model in local {
        assert!(model.tools, "{}", model.tag);
        assert!(
            crate::models::suggested::ranked_window(model.window).is_some(),
            "{}",
            model.tag
        );
    }
}

/// A served catalog brings a local model that shipped after the release, and its facts win
/// over the shipped ones for a model both list.
#[test]
fn a_fetched_catalog_adds_local_models_and_its_facts_win() {
    let shipped = shipped_catalog().local.len();
    let newer = parse_catalog(SAMPLE_LOCAL).unwrap();
    let layered = layered(Some(&newer), &[], &[], &[]);
    assert_eq!(layered.local.len(), shipped + 1);
    let sample = layered
        .local
        .iter()
        .find(|model| model.tag == "sample:14b")
        .unwrap();
    assert_eq!(sample.coding.as_ref().unwrap().score.0, 45.0);

    let mut again = newer.clone();
    again.local[0].name = "Sample 14B, again".to_owned();
    let mut twice = newer;
    twice.merge(again);
    assert_eq!(twice.local.len(), 1);
    assert_eq!(twice.local[0].name, "Sample 14B, again");
}

#[test]
fn a_local_model_with_a_bad_month_score_or_tag_is_refused() {
    for (from, to, why) in [
        (
            "released = \"2026-09\"",
            "released = \"September\"",
            "isn't a month",
        ),
        ("score = 45.0", "score = 145.0", "out of 100"),
        (
            "tag = \"sample:14b\"",
            "tag = \"sample 14b\"",
            "must be one word",
        ),
        ("window = 65536", "window = 0", "window must be"),
    ] {
        let error = parse_catalog(&SAMPLE_LOCAL.replace(from, to)).unwrap_err();
        assert!(
            error.contains("[[local]]") && error.contains(why),
            "{error}"
        );
    }
}

/// A very first start, offline, finds its window in the shipped catalog: on a CLI's default,
/// by an alias, or by the model's own id. A model no maker's page gives has none.
#[test]
fn the_shipped_catalog_gives_each_cli_model_its_window() {
    let catalog = shipped_catalog();
    for (worker, window) in [
        ("claude-code", 1_000_000),
        ("claude-code/opus", 1_000_000),
        ("claude-code/claude-haiku-4-5", 200_000),
        ("codex", 872_000),
        ("codex/gpt-5.5", 272_000),
        ("antigravity-cli", 1_048_576),
        ("antigravity-cli/gpt-oss-120b-medium", 131_072),
    ] {
        assert_eq!(catalog.window(&id(worker)), Some(window), "{worker}");
    }
    for unknown in [
        "claude-code/claude-mythos-preview",
        "antigravity-cli/gemini-3.6-pro",
    ] {
        assert_eq!(catalog.window(&id(unknown)), None, "{unknown}");
    }
}
