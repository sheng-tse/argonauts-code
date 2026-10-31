//! Ollama's own API, which says what its `GET /v1/models` doesn't. `GET /api/tags` names the
//! tags, `POST /api/show` gives each one's most tokens, the cap the tag sets with `num_ctx`, and
//! whether it takes tools or thinks, and `GET /api/ps` the window and memory each loaded model
//! holds. None of them loads a model.
//!
//! A tag with no cap of its own is sized as it loads, from the memory the server has, so its
//! window is known only once `/api/ps` lists it.

use argonauts_core::models::registry::ModelEntry;
use serde_json::{Value, json};
use tokio::task::JoinSet;

use super::http::{Server, Unanswered};
use super::parse::endpoint_entry;

/// What `/api/show` says of one tag.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Shown {
    /// The most tokens the model takes: `<architecture>.context_length`.
    pub most: Option<u64>,
    /// The window the tag holds it to, from `num_ctx` in its parameters.
    pub cap: Option<u64>,
    pub tools: bool,
    pub thinking: bool,
}

/// A model `/api/ps` lists as loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub name: String,
    /// The window it was loaded with.
    pub window: Option<u64>,
    /// The memory it holds, in bytes.
    pub ram_bytes: Option<u64>,
}

/// The tags `/api/tags` lists, in its order.
pub fn parse_tags(json: &str) -> Result<Vec<String>, String> {
    let reply: Value =
        serde_json::from_str(json).map_err(|e| format!("Ollama's tag list isn't JSON: {e}"))?;
    let models = reply["models"]
        .as_array()
        .ok_or("Ollama's tag list has no `models` array")?;
    Ok(models
        .iter()
        .filter_map(|model| model["name"].as_str().or(model["model"].as_str()))
        .map(str::to_owned)
        .collect())
}

/// One tag's facts from `/api/show`.
pub fn parse_show(json: &str) -> Result<Shown, String> {
    let reply: Value =
        serde_json::from_str(json).map_err(|e| format!("Ollama's model facts aren't JSON: {e}"))?;
    let info = &reply["model_info"];
    let most = info["general.architecture"]
        .as_str()
        .and_then(|architecture| info[format!("{architecture}.context_length")].as_u64())
        .or_else(|| {
            // An answer without the architecture still names the length under it.
            info.as_object()?
                .iter()
                .find(|(key, _)| key.ends_with(".context_length"))
                .and_then(|(_, value)| value.as_u64())
        });
    let can = |what: &str| {
        reply["capabilities"]
            .as_array()
            .is_some_and(|list| list.iter().any(|c| c.as_str() == Some(what)))
    };
    Ok(Shown {
        most,
        cap: reply["parameters"].as_str().and_then(num_ctx),
        tools: can("tools"),
        thinking: can("thinking"),
    })
}

/// `num_ctx` in a Modelfile's parameters, one `name value` pair a line.
fn num_ctx(parameters: &str) -> Option<u64> {
    parameters.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        (words.next() == Some("num_ctx"))
            .then(|| words.next()?.parse().ok())
            .flatten()
            .filter(|tokens| *tokens > 0)
    })
}

/// The models `/api/ps` lists as loaded.
pub fn parse_ps(json: &str) -> Result<Vec<Loaded>, String> {
    let reply: Value = serde_json::from_str(json)
        .map_err(|e| format!("Ollama's list of loaded models isn't JSON: {e}"))?;
    let models = reply["models"]
        .as_array()
        .ok_or("Ollama's list of loaded models has no `models` array")?;
    Ok(models
        .iter()
        .filter_map(|model| {
            let name = model["name"].as_str().or(model["model"].as_str())?;
            Some(Loaded {
                name: name.to_owned(),
                window: model["context_length"].as_u64().filter(|n| *n > 0),
                ram_bytes: model["size"].as_u64().filter(|n| *n > 0),
            })
        })
        .collect())
}

/// The entry for `tag` on the endpoint `endpoint`. Its most tokens come from `/api/show`, and
/// its window is the tag's cap, held to the window it was loaded with when it's loaded.
pub fn entry(
    endpoint: &str,
    local: bool,
    tag: &str,
    shown: Option<&Shown>,
    loaded: Option<&Loaded>,
) -> Option<ModelEntry> {
    let mut entry = endpoint_entry(endpoint, local, &json!({ "id": tag }))?;
    if let Some(shown) = shown {
        entry.context_window = shown.most;
        entry.tools = shown.tools;
        entry.thinking = Some(shown.thinking);
        entry.window = shown.cap;
    }
    if let Some(loaded) = loaded {
        entry.window = match (entry.window, loaded.window) {
            (Some(cap), Some(window)) => Some(cap.min(window)),
            (cap, window) => window.or(cap),
        };
        entry.ram_bytes = loaded.ram_bytes;
    }
    Some(entry)
}

/// Every tag the server has, with what `/api/show` and `/api/ps` say of it. A tag whose facts
/// can't be read is still listed, with none.
pub(super) async fn list(server: &Server) -> Result<Vec<ModelEntry>, Unanswered> {
    let tags = parse_tags(&server.get("/api/tags").await?)?;
    let loaded = match server.get("/api/ps").await {
        Ok(json) => parse_ps(&json).unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    let mut asks = JoinSet::new();
    for (index, tag) in tags.iter().enumerate() {
        let server = server.clone();
        let body = json!({ "model": tag });
        asks.spawn(async move {
            let shown = server.post("/api/show", &body).await;
            (index, shown.ok().and_then(|json| parse_show(&json).ok()))
        });
    }
    let mut shown = vec![None; tags.len()];
    while let Some(done) = asks.join_next().await {
        if let Ok((index, facts)) = done {
            shown[index] = facts;
        }
    }
    Ok(tags
        .iter()
        .zip(&shown)
        .filter_map(|(tag, shown)| {
            let loaded = loaded.iter().find(|model| model.name == *tag);
            entry(&server.id, server.local, tag, shown.as_ref(), loaded)
        })
        .collect())
}

/// The window `model` was loaded with, once `/api/ps` lists it. A bare name matches its
/// `:latest` tag.
pub(super) async fn loaded_window(server: &Server, model: &str) -> Option<u64> {
    let loaded = parse_ps(&server.get("/api/ps").await.ok()?).ok()?;
    let latest = format!("{model}:latest");
    loaded
        .iter()
        .find(|loaded| loaded.name == model || loaded.name == latest)
        .and_then(|loaded| loaded.window)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAGS: &str = include_str!("../../../tests/fixtures/ollama/tags.json");
    const SHOW: &str = include_str!("../../../tests/fixtures/ollama/show.json");
    const PS_EMPTY: &str = include_str!("../../../tests/fixtures/ollama/ps-empty.json");
    const PS_LOADED: &str = include_str!("../../../tests/fixtures/ollama/ps-loaded.json");

    /// What `ollama show` answers for `tag`, the recorded tags and the made-up capped ones.
    fn shown(tag: &str) -> Shown {
        let all = super::super::capped_twins::show(&serde_json::from_str(SHOW).unwrap());
        parse_show(&all[tag].to_string()).unwrap()
    }

    #[test]
    fn each_tag_is_listed() {
        assert_eq!(
            parse_tags(TAGS).unwrap(),
            [
                "qwen3-coder:30b",
                "gemma3:12b",
                "gemma2:9b",
                "gemma4:e2b",
                "llama3.2:latest",
            ]
        );
    }

    /// The numbers `ollama show` gives on the machine these were recorded on, and for a tag
    /// made from one with `num_ctx` set.
    #[test]
    fn a_tags_most_tokens_and_cap_come_from_show() {
        let facts = |tag| {
            let shown = shown(tag);
            (shown.most, shown.cap, shown.tools, shown.thinking)
        };
        assert_eq!(
            facts("sample-coder:30b-64k"),
            (Some(262_144), Some(65_536), true, false)
        );
        assert_eq!(facts("qwen3-coder:30b"), (Some(262_144), None, true, false));
        assert_eq!(
            facts("sample-gemma:12b-32k"),
            (Some(131_072), Some(32_768), false, false)
        );
        assert_eq!(facts("gemma3:12b"), (Some(131_072), None, false, false));
        assert_eq!(facts("gemma2:9b"), (Some(8_192), None, false, false));
        assert_eq!(facts("gemma4:e2b"), (Some(131_072), None, true, true));
        assert_eq!(facts("llama3.2:latest"), (Some(131_072), None, true, false));
    }

    #[test]
    fn a_window_is_the_cap_held_to_what_was_loaded() {
        let capped = entry(
            "ollama",
            true,
            "sample-gemma:12b-32k",
            Some(&shown("sample-gemma:12b-32k")),
            None,
        )
        .unwrap();
        assert_eq!(capped.context_window, Some(131_072));
        assert_eq!(capped.window, Some(32_768));
        assert_eq!(capped.session_window(), Some(32_768));
        assert!(!capped.tools);

        // No cap: the most it takes shows, and a session waits for the loaded size.
        let open = entry(
            "ollama",
            true,
            "qwen3-coder:30b",
            Some(&shown("qwen3-coder:30b")),
            None,
        )
        .unwrap();
        assert_eq!(open.context_window, Some(262_144));
        assert_eq!(open.window, None);
        assert_eq!(open.session_window(), None);

        assert!(parse_ps(PS_EMPTY).unwrap().is_empty());
        let loaded = parse_ps(PS_LOADED).unwrap();
        assert_eq!(
            loaded,
            [Loaded {
                name: "llama3.2:latest".to_owned(),
                window: Some(131_072),
                ram_bytes: Some(23_779_979_264),
            }]
        );
        let llama = entry(
            "ollama",
            true,
            "llama3.2:latest",
            Some(&shown("llama3.2:latest")),
            loaded.first(),
        )
        .unwrap();
        assert_eq!(llama.window, Some(131_072));
        assert_eq!(llama.ram_bytes, Some(23_779_979_264));
        assert_eq!(llama.aliases, ["llama3.2"]);
        assert_eq!(llama.session_window(), Some(131_072));
    }

    #[test]
    fn num_ctx_is_read_from_its_own_line() {
        assert_eq!(
            num_ctx("temperature 1\nnum_ctx      65536\nstop \"x\""),
            Some(65_536)
        );
        assert_eq!(num_ctx("num_ctx_extra 4\nnum_ctx 0"), None);
        assert_eq!(num_ctx(""), None);
    }
}
