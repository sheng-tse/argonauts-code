//! Claude Code's models. Claude Code can't list them, so they come from Anthropic: `GET
//! /v1/models` when Argonauts Code has an Anthropic key, else Anthropic's public model pages, which
//! need no login. Neither calls a model.
//!
//! The newest model of each line is current. The rest are older and name that newest one as
//! their `successor`, so `/model` can list them under "Older" and a session on one that stops
//! answering can move up.

use argonauts_core::registry::ModelEntry;
use argonauts_core::settings::Settings;
use serde_json::Value;

use super::{ENDPOINT_TIMEOUT, MAX_MODELS_BYTES};
use crate::models::builtin::cli_entry;
use crate::models::endpoint_settings;

/// The models overview: the current lineup with each model's API id and alias. The `.md` form
/// of the page is plain markdown, much smaller than the HTML.
pub const MODELS_PAGE: &str = "https://platform.claude.com/docs/en/models/overview.md";

/// The deprecations page, whose "Model status" table lists every recent model as active,
/// deprecated, or retired.
pub const STATUS_PAGE: &str =
    "https://platform.claude.com/docs/en/about-claude/model-deprecations.md";

pub const ANTHROPIC_API: &str = "https://api.anthropic.com/v1";

/// The variable Anthropic's own tools read the key from.
pub const ANTHROPIC_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// Anthropic's pages change when a model ships, so a list a day old is still good.
pub const FRESH_FOR: time::Duration = time::Duration::days(1);

/// The `anthropic-version` header the models API takes.
const API_VERSION: &str = "2023-06-01";

/// Where Claude Code's model list comes from.
#[derive(Clone, PartialEq, Eq)]
pub enum ClaudeSource {
    /// Anthropic's public pages, as markdown.
    Pages { models: String, status: String },
    /// `GET {base_url}/models` with this key.
    Api { base_url: String, key: String },
}

// The key never goes into a log line.
impl std::fmt::Debug for ClaudeSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pages { models, status } => f
                .debug_struct("Pages")
                .field("models", models)
                .field("status", status)
                .finish(),
            Self::Api { base_url, .. } => f
                .debug_struct("Api")
                .field("base_url", base_url)
                .finish_non_exhaustive(),
        }
    }
}

impl ClaudeSource {
    pub fn public_pages() -> Self {
        Self::Pages {
            models: MODELS_PAGE.to_owned(),
            status: STATUS_PAGE.to_owned(),
        }
    }

    /// The models API when an endpoint you pointed at Anthropic has its key set, or
    /// `ANTHROPIC_API_KEY` is; the public pages otherwise. Keys come only from those
    /// variables, never from a file.
    pub fn for_settings(settings: &Settings) -> Self {
        Self::choose(settings, |name| std::env::var(name).ok())
    }

    fn choose(settings: &Settings, var: impl Fn(&str) -> Option<String>) -> Self {
        let set = |name: &str| var(name).filter(|value| !value.trim().is_empty());
        let endpoint = settings.endpoints.keys().find_map(|id| {
            let endpoint = endpoint_settings(settings, id)?;
            let base_url = endpoint.base_url?;
            if !is_anthropic(&base_url) {
                return None;
            }
            let key = set(endpoint.api_key_env.as_deref()?)?;
            Some((base_url, key))
        });
        match endpoint {
            Some((base_url, key)) => Self::Api { base_url, key },
            None => match set(ANTHROPIC_KEY_ENV) {
                Some(key) => Self::Api {
                    base_url: ANTHROPIC_API.to_owned(),
                    key,
                },
                None => Self::public_pages(),
            },
        }
    }

    /// Claude Code's models, current first. An error says what failed, and the list found
    /// before stays.
    pub async fn list(&self, settings: &Settings) -> Result<Vec<ModelEntry>, String> {
        let client = reqwest::Client::builder()
            .timeout(ENDPOINT_TIMEOUT)
            .build()
            .map_err(|e| format!("couldn't start an HTTP client: {e}"))?;
        let listed = match self {
            Self::Pages { models, status } => {
                let (overview, table) = tokio::join!(
                    fetch(&client, client.get(models)),
                    fetch(&client, client.get(status))
                );
                match (overview, table) {
                    (Err(a), Err(b)) => return Err(format!("{a}; {b}")),
                    (overview, table) => parse_model_pages(
                        overview.as_deref().unwrap_or(""),
                        table.as_deref().unwrap_or(""),
                    ),
                }
            }
            Self::Api { base_url, key } => {
                let url = format!("{}/models?limit=1000", base_url.trim_end_matches('/'));
                let request = client
                    .get(url)
                    .header("x-api-key", key)
                    .header("anthropic-version", API_VERSION);
                parse_anthropic_models(&fetch(&client, request).await?)?
            }
        };
        if listed.is_empty() {
            return Err("Anthropic's model pages listed no model ids".to_owned());
        }
        Ok(claude_code_entries(settings, listed))
    }
}

fn is_anthropic(base_url: &str) -> bool {
    let host = base_url
        .split("://")
        .nth(1)
        .unwrap_or(base_url)
        .split(['/', ':'])
        .next()
        .unwrap_or("");
    host == "api.anthropic.com"
}

async fn fetch(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
) -> Result<String, String> {
    let request = request
        .build()
        .map_err(|e| format!("a bad request for Claude's models: {e}"))?;
    let url = format!(
        "{}{}",
        request.url().origin().ascii_serialization(),
        request.url().path()
    );
    let mut reply = client
        .execute(request)
        .await
        .map_err(|e| format!("GET {url} failed: {}", e.without_url()))?;
    let status = reply.status();
    if !status.is_success() {
        return Err(format!("GET {url} answered {status}"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = reply
        .chunk()
        .await
        .map_err(|e| format!("reading {url} failed: {}", e.without_url()))?
    {
        if body.len() + chunk.len() > MAX_MODELS_BYTES {
            let mib = MAX_MODELS_BYTES / (1024 * 1024);
            return Err(format!("GET {url} sent more than {mib} MiB"));
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| format!("GET {url} sent text that isn't UTF-8"))
}

/// One model as Anthropic lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedModel {
    pub id: String,
    /// The shorter id that points at it, such as `claude-haiku-4-5`.
    pub alias: Option<String>,
    /// In the overview's lineup, which is current by definition.
    pub lineup: bool,
    pub retired: bool,
    /// Its context window in tokens, where the overview says.
    pub context_window: Option<u64>,
}

/// The overview's lineup, then every other model the status table lists, in its order. Either
/// page may be missing (empty), and the other still gives a list.
pub fn parse_model_pages(overview: &str, status: &str) -> Vec<ListedModel> {
    let ids = table_row(overview, "Claude API ID");
    let aliases = table_row(overview, "Claude API alias");
    let windows = context_windows(overview);
    let mut listed: Vec<ListedModel> = ids
        .iter()
        .enumerate()
        .map(|(at, id)| ListedModel {
            id: id.clone(),
            alias: aliases.get(at).filter(|alias| *alias != id).cloned(),
            lineup: true,
            retired: false,
            // Only a row with a window for every model lines up with the ids.
            context_window: windows
                .get(at)
                .copied()
                .flatten()
                .filter(|_| windows.len() == ids.len()),
        })
        .collect();
    for (id, state) in status_rows(status) {
        let retired = state.eq_ignore_ascii_case("retired");
        match listed.iter_mut().find(|model| model.id == id) {
            Some(model) => model.retired = retired,
            None => listed.push(ListedModel {
                id,
                alias: None,
                lineup: false,
                retired,
                context_window: None,
            }),
        }
    }
    listed
}

/// The ids in backticks in the overview table's row that starts with `name`, one per model
/// column.
fn table_row(markdown: &str, name: &str) -> Vec<String> {
    let Some(row) = markdown.lines().find(|line| {
        line.trim_start()
            .strip_prefix('|')
            .is_some_and(|rest| cell_text(rest.split('|').next().unwrap_or("")) == name)
    }) else {
        return Vec::new();
    };
    row.split('|')
        .skip(2)
        .filter_map(|cell| {
            let cell = cell.trim();
            let id = cell.strip_prefix('`')?.split('`').next()?;
            is_model_id(id).then(|| id.to_owned())
        })
        .collect()
}

/// The overview table's "Context window" row, one cell per model column: `1M tokens` is
/// 1,000,000 and `200K tokens` 200,000. A cell that reads otherwise is `None`.
fn context_windows(markdown: &str) -> Vec<Option<u64>> {
    let Some(row) = markdown.lines().find(|line| {
        line.trim_start()
            .strip_prefix('|')
            .is_some_and(|rest| cell_text(rest.split('|').next().unwrap_or("")) == "Context window")
    }) else {
        return Vec::new();
    };
    let cells: Vec<&str> = row.split('|').skip(2).map(str::trim).collect();
    // The row ends with `|`, which leaves one empty piece after the last cell.
    let cells = cells.strip_suffix(&[""]).unwrap_or(&cells);
    cells
        .iter()
        .map(|cell| {
            let size = cell.split_whitespace().next()?;
            let (digits, scale) = match size.char_indices().last()? {
                (at, 'M' | 'm') => (&size[..at], 1_000_000),
                (at, 'K' | 'k') => (&size[..at], 1_000),
                _ => return None,
            };
            digits.parse::<u64>().ok()?.checked_mul(scale)
        })
        .collect()
}

/// A cell's text without its link: `[Retirement](https://…)` reads as `Retirement`.
fn cell_text(cell: &str) -> &str {
    let cell = cell.trim();
    cell.strip_prefix('[')
        .and_then(|rest| rest.split(']').next())
        .unwrap_or(cell)
}

/// (id, state) for each row of the "Model status" table.
fn status_rows(markdown: &str) -> Vec<(String, String)> {
    let Some(start) = markdown
        .lines()
        .position(|line| line.trim_start().starts_with("## Model status"))
    else {
        return Vec::new();
    };
    markdown
        .lines()
        .skip(start + 1)
        .take_while(|line| !line.trim_start().starts_with("## "))
        .filter_map(|line| {
            let mut cells = line.trim().strip_prefix('|')?.split('|');
            let id = cells.next()?.trim().trim_matches('`');
            let state = cells.next()?.trim();
            is_model_id(id).then(|| (id.to_owned(), state.to_owned()))
        })
        .collect()
}

fn is_model_id(text: &str) -> bool {
    text.starts_with("claude-")
        && text.len() > "claude-".len()
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.'))
}

/// `GET /v1/models`: `data` holds each model's `id`, newest first.
pub fn parse_anthropic_models(json: &str) -> Result<Vec<ListedModel>, String> {
    let reply: Value = serde_json::from_str(json)
        .map_err(|e| format!("Anthropic's model list isn't JSON: {e}"))?;
    let data = reply["data"]
        .as_array()
        .ok_or("Anthropic's model list has no `data` array")?;
    Ok(data
        .iter()
        .filter_map(|model| model["id"].as_str())
        .filter(|id| is_model_id(id))
        .map(|id| ListedModel {
            id: id.to_owned(),
            alias: None,
            lineup: false,
            retired: false,
            context_window: None,
        })
        .collect())
}

/// What a model id says of itself: its line (`opus`, `sonnet`, …) and version, from
/// `claude-opus-4-5-20251101` and the older `claude-3-7-sonnet-20250219` alike. A dated
/// snapshot's date doesn't count as version.
fn line_and_version(id: &str) -> (String, Vec<u32>) {
    let mut line = None;
    let mut version = Vec::new();
    for part in id.trim_start_matches("claude-").split(['-', '.']) {
        match part.parse::<u32>() {
            Ok(_) if part.len() >= 8 => {}
            Ok(number) => version.push(number),
            Err(_) if line.is_none() => line = Some(part.to_owned()),
            Err(_) => {}
        }
    }
    (line.unwrap_or_else(|| "claude".to_owned()), version)
}

/// Registry entries for Claude Code. The current one of each line is its lineup model, or
/// its newest when the lineup has none (the API names no lineup). Every other model is older,
/// with that one as its `successor`.
pub fn claude_code_entries(settings: &Settings, listed: Vec<ListedModel>) -> Vec<ModelEntry> {
    let mut current: Vec<(String, String)> = listed
        .iter()
        .filter(|model| model.lineup && !model.retired)
        .map(|model| (line_and_version(&model.id).0, model.id.clone()))
        .collect();
    let mut newest: Vec<(String, Vec<u32>, String)> = Vec::new();
    for model in listed.iter().filter(|model| !model.retired) {
        let (line, version) = line_and_version(&model.id);
        if current.iter().any(|(f, _)| *f == line) {
            continue;
        }
        match newest.iter_mut().find(|(f, ..)| *f == line) {
            Some(slot) if version > slot.1 => {
                slot.1 = version;
                slot.2 = model.id.clone();
            }
            Some(_) => {}
            None => newest.push((line, version, model.id.clone())),
        }
    }
    current.extend(newest.into_iter().map(|(line, _, id)| (line, id)));

    let is_current = |id: &str| current.iter().any(|(_, current)| current == id);
    let (now, older): (Vec<ListedModel>, Vec<ListedModel>) =
        listed.into_iter().partition(|model| is_current(&model.id));
    now.into_iter()
        .chain(older)
        .map(|model| {
            let mut entry = cli_entry(settings, "claude-code", Some(&model.id));
            entry.discovered = true;
            entry.context_window = model.context_window;
            entry.aliases = model.alias.into_iter().collect();
            entry.retired = model.retired;
            if !is_current(&model.id) {
                let line = line_and_version(&model.id).0;
                entry.successor = current
                    .iter()
                    .find(|(f, _)| *f == line)
                    .map(|(_, id)| id.clone());
            }
            entry
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use argonauts_core::settings::agents::EndpointSettings;
    use time::OffsetDateTime;

    use super::*;

    const PAGES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/anthropic");

    /// Written to the shape Anthropic documents for `GET /v1/models`, not recorded, since
    /// tests never call the API.
    const API_REPLY: &str = include_str!("../../../tests/fixtures/anthropic/models-api-shape.json");

    fn page(name: &str) -> String {
        std::fs::read_to_string(format!("{PAGES}/{name}")).unwrap()
    }

    fn ids(entries: &[ModelEntry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| entry.id.model.clone().unwrap_or_default())
            .collect()
    }

    fn entry<'a>(entries: &'a [ModelEntry], id: &str) -> &'a ModelEntry {
        entries
            .iter()
            .find(|entry| entry.id.model.as_deref() == Some(id))
            .unwrap_or_else(|| panic!("{id} isn't listed"))
    }

    #[test]
    fn the_saved_pages_give_the_lineup_then_every_older_model() {
        let listed = parse_model_pages(&page("models-overview.md"), &page("model-deprecations.md"));
        let entries = claude_code_entries(&Settings::default(), listed);
        let ids = ids(&entries);
        assert_eq!(
            ids[..5],
            [
                "claude-fable-5-1",
                "claude-opus-5-5",
                "claude-sonnet-5-5",
                "claude-haiku-4-5-20251001",
                // Not in the lineup, but the newest of its line.
                "claude-mythos-5-1",
            ]
        );
        assert_eq!(ids.len(), 22, "{ids:?}");
        assert!(
            entries
                .iter()
                .all(|e| e.discovered && e.id.agent == "claude-code")
        );

        let haiku = entry(&entries, "claude-haiku-4-5-20251001");
        assert_eq!(haiku.aliases, ["claude-haiku-4-5"]);
        assert_eq!(haiku.successor, None);
        // The overview says how large each lineup model's window is, and no older one's.
        assert_eq!(haiku.context_window, Some(200_000));
        for id in ["claude-fable-5-1", "claude-opus-5-5", "claude-sonnet-5-5"] {
            assert_eq!(entry(&entries, id).context_window, Some(1_000_000), "{id}");
        }
        assert_eq!(entry(&entries, "claude-opus-4-8").context_window, None);

        let older = entry(&entries, "claude-opus-4-8");
        assert_eq!(older.successor.as_deref(), Some("claude-opus-5-5"));
        assert!(!older.retired);
        let deprecated = entry(&entries, "claude-sonnet-4-5-20250929");
        assert_eq!(deprecated.successor.as_deref(), Some("claude-sonnet-5-5"));
        assert!(!deprecated.retired);
        let retired = entry(&entries, "claude-3-7-sonnet-20250219");
        assert!(retired.retired);
        assert_eq!(retired.successor.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(
            entry(&entries, "claude-mythos-preview")
                .successor
                .as_deref(),
            Some("claude-mythos-5-1")
        );
    }

    #[test]
    fn either_page_alone_still_gives_a_list() {
        let lineup = parse_model_pages(&page("models-overview.md"), "");
        assert_eq!(lineup.len(), 4);
        assert!(lineup.iter().all(|model| model.lineup));

        let status = claude_code_entries(
            &Settings::default(),
            parse_model_pages("", &page("model-deprecations.md")),
        );
        // With no lineup, each line's newest is current.
        let current: Vec<String> = status
            .iter()
            .filter(|e| e.successor.is_none())
            .map(|e| e.id.model.clone().unwrap_or_default())
            .collect();
        assert_eq!(
            current,
            [
                "claude-fable-5-1",
                "claude-mythos-5-1",
                "claude-opus-5-5",
                "claude-sonnet-5-5",
                "claude-haiku-4-5-20251001",
            ]
        );
        assert!(parse_model_pages("", "").is_empty());
        assert!(parse_model_pages("<html>not markdown</html>", "").is_empty());
    }

    #[test]
    fn the_models_api_reply_lists_its_ids_newest_first() {
        let listed = parse_anthropic_models(API_REPLY).unwrap();
        let entries = claude_code_entries(&Settings::default(), listed);
        assert_eq!(
            ids(&entries),
            [
                "claude-opus-5-5",
                "claude-sonnet-5-5",
                "claude-haiku-4-5-20251001",
                "claude-opus-4-8",
                "claude-sonnet-4-6",
            ]
        );
        assert_eq!(
            entry(&entries, "claude-opus-4-8").successor.as_deref(),
            Some("claude-opus-5-5")
        );
        assert!(parse_anthropic_models("{}").is_err());
        assert!(parse_anthropic_models("not json").is_err());
    }

    #[test]
    fn a_key_in_the_environment_or_an_anthropic_endpoint_picks_the_api() {
        let none = |_: &str| None;
        assert_eq!(
            ClaudeSource::choose(&Settings::default(), none),
            ClaudeSource::public_pages()
        );
        let blank = |_: &str| Some("  ".to_owned());
        assert_eq!(
            ClaudeSource::choose(&Settings::default(), blank),
            ClaudeSource::public_pages()
        );

        let env_key = |name: &str| (name == ANTHROPIC_KEY_ENV).then(|| "sk-env".to_owned());
        assert_eq!(
            ClaudeSource::choose(&Settings::default(), env_key),
            ClaudeSource::Api {
                base_url: ANTHROPIC_API.to_owned(),
                key: "sk-env".to_owned(),
            }
        );

        let mut settings = Settings::default();
        settings.endpoints.insert(
            "anthropic".to_owned(),
            EndpointSettings {
                base_url: Some("https://api.anthropic.com/v1/".to_owned()),
                api_key_env: Some("MY_ANTHROPIC_KEY".to_owned()),
                ..EndpointSettings::default()
            },
        );
        let endpoint_key = |name: &str| (name == "MY_ANTHROPIC_KEY").then(|| "sk-mine".to_owned());
        let source = ClaudeSource::choose(&settings, endpoint_key);
        assert_eq!(
            source,
            ClaudeSource::Api {
                base_url: "https://api.anthropic.com/v1/".to_owned(),
                key: "sk-mine".to_owned(),
            }
        );
        assert!(!format!("{source:?}").contains("sk-mine"));
        assert!(!is_anthropic("https://api.anthropic.com.example.org/v1"));
    }

    /// A server that answers each request with the body `route` gives its path, or 404, and
    /// keeps every request line and header block it got.
    fn serve(
        route: impl Fn(&str) -> Option<Vec<u8>> + Send + 'static,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (seen, requests) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut buffer = [0; 8192];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
                let _ = seen.send(request);
                let (status, body) = match route(&path) {
                    Some(body) => ("200 OK", body),
                    None => ("404 Not Found", Vec::new()),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        (base, requests)
    }

    #[tokio::test]
    async fn the_pages_are_read_over_http_and_one_missing_page_is_enough() {
        let (base, _requests) = serve(|path| match path {
            "/models.md" => Some(page("models-overview.md").into_bytes()),
            "/status.md" => Some(page("model-deprecations.md").into_bytes()),
            _ => None,
        });
        let source = ClaudeSource::Pages {
            models: format!("{base}/models.md"),
            status: format!("{base}/status.md"),
        };
        let found = source.list(&Settings::default()).await.unwrap();
        assert_eq!(found.len(), 22);

        let half = ClaudeSource::Pages {
            models: format!("{base}/models.md"),
            status: format!("{base}/gone.md"),
        };
        assert_eq!(half.list(&Settings::default()).await.unwrap().len(), 4);

        let neither = ClaudeSource::Pages {
            models: format!("{base}/gone.md"),
            status: format!("{base}/gone.md"),
        };
        let why = neither.list(&Settings::default()).await.unwrap_err();
        assert!(why.contains("404"), "{why}");
    }

    #[tokio::test]
    async fn the_api_is_asked_with_the_key_in_its_header() {
        let (base, requests) = serve(|path| {
            path.starts_with("/v1/models")
                .then(|| API_REPLY.as_bytes().to_vec())
        });
        let source = ClaudeSource::Api {
            base_url: format!("{base}/v1"),
            key: "sk-test".to_owned(),
        };
        let found = source.list(&Settings::default()).await.unwrap();
        assert_eq!(found.len(), 5);
        let request = requests.recv().unwrap().to_ascii_lowercase();
        assert!(
            request.starts_with("get /v1/models?limit=1000 "),
            "{request}"
        );
        assert!(request.contains("x-api-key: sk-test"), "{request}");
        assert!(
            request.contains("anthropic-version: 2023-06-01"),
            "{request}"
        );
    }

    /// Claude Code alone, so no real CLI or server runs.
    fn claude_only() -> Settings {
        let mut settings = Settings::default();
        settings.registry.local_servers = false;
        for agent in ["codex", "antigravity-cli", "opencode"] {
            settings.agents.get_mut(agent).unwrap().enabled = false;
        }
        settings
    }

    fn older_claude(registry: &crate::models::FileRegistry) -> usize {
        use argonauts_core::registry::ModelRegistry as _;
        registry
            .list()
            .iter()
            .filter(|e| e.id.agent == "claude-code" && e.successor.is_some())
            .count()
    }

    #[tokio::test]
    async fn the_registry_reads_anthropic_once_a_day_and_keeps_the_list_between() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        use argonauts_core::registry::ModelRegistry as _;

        use crate::models::{FileRegistry, ModelCache};

        let up = Arc::new(AtomicBool::new(true));
        let serving = Arc::clone(&up);
        let (base, requests) = serve(move |path| {
            if !serving.load(Ordering::SeqCst) {
                return None;
            }
            match path {
                "/models.md" => Some(page("models-overview.md").into_bytes()),
                "/status.md" => Some(page("model-deprecations.md").into_bytes()),
                _ => None,
            }
        });
        let source = ClaudeSource::Pages {
            models: format!("{base}/models.md"),
            status: format!("{base}/status.md"),
        };
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("models-cache.json");
        let open = || {
            FileRegistry::from_files(&claude_only(), None, None)
                .with_cache(&cache)
                .with_claude_source(source.clone())
        };

        let first = open();
        assert_eq!(older_claude(&first), 0);
        first.discover().await;
        assert_eq!(requests.try_iter().count(), 2);
        assert_eq!(older_claude(&first), 17);
        let listed = ModelCache::read(&cache).unwrap().claude_listed;
        assert!(listed.is_some());

        // Within the day neither this run nor the next asks again, and both still list them.
        first.discover().await;
        let next = open();
        assert_eq!(older_claude(&next), 17);
        next.discover().await;
        assert_eq!(requests.try_iter().count(), 0);
        assert_eq!(older_claude(&next), 17);
        assert_eq!(ModelCache::read(&cache).unwrap().claude_listed, listed);

        // A day on, it asks again, and a failed read keeps the list it had.
        let mut stale = ModelCache::read(&cache).unwrap();
        stale.claude_listed = Some(OffsetDateTime::now_utc() - time::Duration::hours(25));
        stale.write(&cache).unwrap();
        up.store(false, Ordering::SeqCst);
        let later = open();
        later.discover().await;
        assert_eq!(requests.try_iter().count(), 2);
        assert_eq!(older_claude(&later), 17);
        let skipped = later.discovery_skipped();
        assert!(
            skipped
                .iter()
                .any(|why| why.starts_with("Claude Code's models: ")),
            "{skipped:?}"
        );

        // With no source, as in tests and `registry.discover = false`, nothing is asked.
        let none = FileRegistry::from_files(&claude_only(), None, None);
        none.discover().await;
        assert_eq!(requests.try_iter().count(), 0);
    }

    #[test]
    fn lines_and_versions_read_from_both_id_styles() {
        assert_eq!(
            line_and_version("claude-opus-4-5-20251101"),
            ("opus".to_owned(), vec![4, 5])
        );
        assert_eq!(
            line_and_version("claude-3-7-sonnet-20250219"),
            ("sonnet".to_owned(), vec![3, 7])
        );
        assert_eq!(
            line_and_version("claude-mythos-preview"),
            ("mythos".to_owned(), vec![])
        );
        assert_eq!(
            line_and_version("claude-2.1"),
            ("claude".to_owned(), vec![2, 1])
        );
    }
}
