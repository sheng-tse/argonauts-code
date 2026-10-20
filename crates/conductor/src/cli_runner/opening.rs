//! A run session that opens past 100k tokens gets one note naming the MCP server whose tools
//! weigh most in it, once per run and server. Tool search holds a server's tools back until
//! they're called; a server set to load them all, or tool search turned off, puts every one of
//! them in every request.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{LazyLock, Mutex, PoisonError};

use argonauts_core::RunId;
use argonauts_core::events::context::ContextUsage;
use argonauts_core::window_size::tokens_by;

/// A first request past this many tokens is worth a note.
const HEAVY: u64 = 100_000;

/// The runs and servers noted so far in this process.
static NOTED: LazyLock<Mutex<BTreeSet<(RunId, String)>>> =
    LazyLock::new(|| Mutex::new(BTreeSet::new()));

/// The note for the first context breakdown of a session of `run` on `cli`, when the session
/// opened past 100k tokens and an MCP server's loaded tools weigh most in it, and the run
/// hasn't had the note for that server yet. A note only: nothing stops.
pub(super) fn heavy_opening(run: &RunId, cli: &str, usage: &ContextUsage) -> Option<String> {
    if usage.total_tokens <= HEAVY {
        return None;
    }
    let mut by_server: BTreeMap<&str, u64> = BTreeMap::new();
    for tool in usage.mcp_tools.iter().filter(|tool| tool.loaded) {
        *by_server.entry(tool.server.as_str()).or_default() += tool.tokens;
    }
    let (server, tokens) = by_server
        .into_iter()
        .filter(|(_, tokens)| *tokens > 0)
        .max_by_key(|(_, tokens)| *tokens)?;
    let first = NOTED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert((run.clone(), server.to_owned()));
    first.then(|| {
        format!(
            "{server}'s tools filled {} of the {} tokens {cli}'s first request held: the server \
             may set alwaysLoad, or tool search is off.",
            tokens_by(tokens, 1_000),
            tokens_by(usage.total_tokens, 1_000)
        )
    })
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::context::McpTool;

    use super::*;

    fn tool(server: &str, tokens: u64, loaded: bool) -> McpTool {
        McpTool {
            name: format!("mcp__{server}__tool_{tokens}"),
            server: server.to_owned(),
            tokens,
            loaded,
        }
    }

    fn opening(total: u64, tools: Vec<McpTool>) -> ContextUsage {
        ContextUsage {
            total_tokens: total,
            mcp_tools: tools,
            ..ContextUsage::default()
        }
    }

    /// The server whose loaded tools weigh most is named once per run, and a deferred server's
    /// tools, which wait behind tool search, weigh nothing.
    #[test]
    fn a_heavy_first_request_names_the_heaviest_loaded_server_once_per_run() {
        let run = RunId::new("run-20261019-120000-0a1b").unwrap();
        let usage = opening(
            412_000,
            vec![
                tool("atlas", 380_000, true),
                tool("docs", 12_000, true),
                tool("search", 900_000, false),
            ],
        );
        assert_eq!(
            heavy_opening(&run, "Claude Code", &usage).as_deref(),
            Some(
                "atlas's tools filled 380k of the 412k tokens Claude Code's first request held: \
                 the server may set alwaysLoad, or tool search is off."
            )
        );
        assert_eq!(
            heavy_opening(&run, "Claude Code", &usage),
            None,
            "once a run"
        );
        let other = RunId::new("run-20261019-120000-0a1c").unwrap();
        assert!(heavy_opening(&other, "Claude Code", &usage).is_some());
    }

    /// A first request under 100k, or one whose weight isn't an MCP server's, gets no note.
    #[test]
    fn a_light_first_request_or_one_without_loaded_tools_gets_none() {
        let run = RunId::new("run-20261019-120000-0a1d").unwrap();
        let light = opening(18_759, vec![tool("atlas", 153, true)]);
        assert_eq!(heavy_opening(&run, "Claude Code", &light), None);
        let deferred = opening(150_000, vec![tool("atlas", 399_490, false)]);
        assert_eq!(heavy_opening(&run, "Claude Code", &deferred), None);
    }
}
