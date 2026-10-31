use std::sync::Arc;

use argonauts_adapter_antigravity_cli::AntigravityCli;
use argonauts_adapter_claude_code::ClaudeCodeAdapter;
use argonauts_adapter_codex::CodexAdapter;
use argonauts_adapter_opencode::{LocalUrls, OpenCodeAdapter};
use argonauts_conductor::doctor::resolve_binary;
use argonauts_core::Adapter;
use argonauts_core::agents::runtimes::RUNTIMES;
use argonauts_core::settings::Settings;
use argonauts_registry::models::endpoint_settings;

use crate::host::clis::RepoModels;

/// Claude Code, Codex, agy, and OpenCode, in the order the setup lists them.
pub fn builtin_adapters() -> Vec<Arc<dyn Adapter>> {
    vec![
        Arc::new(ClaudeCodeAdapter::new()),
        Arc::new(CodexAdapter::new()),
        Arc::new(AntigravityCli::new()),
        Arc::new(OpenCodeAdapter::new()),
    ]
}

/// [`builtin_adapters`] as `settings` set them up for `repo`, which only OpenCode needs.
pub(crate) fn builtin_adapters_for(
    settings: &Settings,
    repo: Option<&RepoModels>,
) -> Vec<Arc<dyn Adapter>> {
    builtin_adapters()
        .into_iter()
        .map(|adapter| match adapter.id() {
            "opencode" => Arc::new(opencode(settings, repo)) as Arc<dyn Adapter>,
            _ => adapter,
        })
        .collect()
}

/// OpenCode as `settings` set it up: told where your local runtimes listen, and given its
/// program for listing its own sessions, found as doctor finds a program it runs outside the
/// sandbox, with the sandbox those lists run in for `repo`. One that can't be found that way
/// lists none.
pub(crate) fn opencode(settings: &Settings, repo: Option<&RepoModels>) -> OpenCodeAdapter {
    let adapter = OpenCodeAdapter::with_local_urls(opencode_local_urls(settings));
    let configured = settings
        .agents
        .get(adapter.id())
        .and_then(|agent| agent.binary.as_deref());
    match resolve_binary(&adapter, configured) {
        Ok(program) => {
            adapter.with_program(program, crate::agents::confine::read_only(settings, repo))
        }
        Err(_) => adapter,
    }
}

/// Your `[endpoints.<runtime>]` addresses for the local runtimes, which OpenCode is told for a
/// local model in place of its own defaults.
pub(crate) fn opencode_local_urls(settings: &Settings) -> LocalUrls {
    RUNTIMES
        .iter()
        .map(|runtime| runtime.id)
        .fold(LocalUrls::new(), |urls, id| {
            match endpoint_settings(settings, id).and_then(|endpoint| endpoint.base_url) {
                Some(base_url) => urls.with(id, base_url),
                None => urls,
            }
        })
}

#[cfg(test)]
mod tests {
    use argonauts_core::models::cheapest::{cheapest_listed, price_of};
    use argonauts_core::models::registry::{ModelRegistry, default_run};
    use argonauts_core::settings::Settings;
    use argonauts_registry::models::discovery::{parse_agy_models, parse_codex_models};
    use argonauts_registry::models::{FileRegistry, ModelCache};

    use super::builtin_adapters;

    /// The registry as it stands once each CLI's own list is read, from the lists recorded in
    /// the registry's fixtures. Claude Code's list is its aliases and the catalog's record of
    /// its picker.
    fn listed(home: &std::path::Path) -> FileRegistry {
        let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/../registry/tests/fixtures");
        let read = |name: &str| std::fs::read_to_string(format!("{fixtures}/{name}")).unwrap();
        let mut models = parse_agy_models(&read("agy/models.json")).unwrap();
        models.extend(parse_codex_models(&read("codex/debug-models.json")).unwrap());
        let cache = home.join("models.json");
        ModelCache {
            refreshed: time::OffsetDateTime::now_utc(),
            models,
            skipped: Vec::new(),
            claude_listed: None,
            catalog: None,
            sources: Vec::new(),
        }
        .write(&cache)
        .unwrap();
        FileRegistry::from_files(&Settings::default(), None, None).with_cache(&cache)
    }

    /// Live tests and recordings run the adapter's own pick, which stands in until a CLI's list
    /// is read, so it must be what the list's prices pick: a cheaper listed model, or a pick
    /// the catalog can't price, fails here.
    #[test]
    fn each_cli_names_the_cheapest_model_its_list_offers() {
        let home = tempfile::tempdir().unwrap();
        let registry = listed(home.path());
        let catalog = registry.catalog();
        for adapter in builtin_adapters() {
            let cli = adapter.id();
            // OpenCode runs whichever provider's model you choose, and names none of its own.
            if cli == "opencode" {
                assert!(cheapest_listed(&registry, cli).is_none());
                continue;
            }
            let (model, effort) = adapter.cheapest_model();
            let runs = catalog
                .runs(&argonauts_core::base::names::ModelRef::new(
                    cli,
                    Some(model.to_owned()),
                ))
                .unwrap_or_else(|| model.to_owned());
            assert!(
                price_of(&catalog, &runs).is_some(),
                "the catalog has no price for {model}, {cli}'s own pick"
            );
            let cheapest = cheapest_listed(&registry, cli)
                .unwrap_or_else(|| panic!("the catalog prices none of the models {cli} lists"));
            assert_eq!(
                (cheapest.model.as_str(), cheapest.effort.as_deref()),
                (model, effort),
                "{cli}'s list offers {} cheaper than {model}, its own pick",
                cheapest.model,
            );
        }
    }

    /// agy's list marks no default, so a session that names no model is priced as the first
    /// model it lists, never a model it stopped offering.
    #[test]
    fn a_cli_named_alone_runs_the_default_its_list_gives() {
        let home = tempfile::tempdir().unwrap();
        let registry = listed(home.path());
        assert_eq!(
            default_run(&registry, "antigravity-cli").as_deref(),
            Some("gemini-3.8-flash-high")
        );
        assert_eq!(
            default_run(&registry, "codex").as_deref(),
            Some("gpt-6-astra")
        );
        assert_eq!(
            default_run(&registry, "claude-code").as_deref(),
            Some("claude-opus-5-5")
        );
    }
}
