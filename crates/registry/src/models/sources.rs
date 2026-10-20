//! Model lists given to the registry beside the sources discovery asks for itself, such as a
//! CLI that lists its models some other way. Discovery asks each one with its own, each is
//! asked again on its own as the others are, and a source that can't answer keeps what it
//! listed before.

use std::fmt::Debug;

use argonauts_core::future::BoxFuture;
use argonauts_core::registry::ModelEntry;
use argonauts_core::settings::Settings;

/// One CLI's model list, asked for as discovery runs.
pub trait ListSource: Send + Sync + Debug {
    /// The CLI whose models these are, such as `opencode`.
    fn cli(&self) -> &str;

    /// Asks for the list. `last` is what this source listed before, which an answer that
    /// can't be read keeps.
    fn list<'a>(&'a self, settings: &'a Settings, last: Vec<ModelEntry>) -> BoxFuture<'a, Answer>;
}

/// What a source answered: the models to list, and why it failed when it did.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Answer {
    pub models: Vec<ModelEntry>,
    /// Set when the source couldn't be read, so `models` is the list kept from before.
    pub failed: Option<String>,
}

impl Answer {
    pub fn listed(models: Vec<ModelEntry>) -> Self {
        Self {
            models,
            failed: None,
        }
    }

    pub fn failed(kept: Vec<ModelEntry>, why: impl Into<String>) -> Self {
        Self {
            models: kept,
            failed: Some(why.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use argonauts_core::names::ModelRef;
    use argonauts_core::registry::ModelRegistry;

    use super::*;
    use crate::models::{FileRegistry, cli_entry};

    /// A CLI that lists its models some other way.
    const CLI: &str = "listed-cli";

    /// What discovery says of the built-in CLIs [`only_the_given_list`] turns off.
    const OFF: &str = "Claude Code, Codex, Antigravity CLI, OpenCode: off in your settings";

    /// Nothing but the given list is asked: every built-in CLI and local server is off.
    fn only_the_given_list() -> Settings {
        let mut settings = Settings::default();
        for agent in settings.agents.values_mut() {
            agent.enabled = false;
        }
        settings.registry.local_servers = false;
        settings
    }

    /// Answers each list with the next of `answers`, or fails once they run out.
    #[derive(Debug)]
    struct Scripted {
        answers: Mutex<Vec<Vec<&'static str>>>,
    }

    impl ListSource for Scripted {
        fn cli(&self) -> &str {
            CLI
        }

        fn list<'a>(
            &'a self,
            settings: &'a Settings,
            last: Vec<ModelEntry>,
        ) -> BoxFuture<'a, Answer> {
            let next = self.answers.lock().unwrap().pop();
            Box::pin(async move {
                match next {
                    Some(routes) => Answer::listed(
                        routes
                            .into_iter()
                            .map(|route| cli_entry(settings, CLI, Some(route)))
                            .collect(),
                    ),
                    None => Answer::failed(last, "the server listed nothing"),
                }
            })
        }
    }

    fn listed(registry: &FileRegistry) -> Vec<String> {
        registry
            .list()
            .into_iter()
            .filter(|entry| entry.id.agent == CLI)
            .filter_map(|entry| entry.id.model)
            .collect()
    }

    #[tokio::test]
    async fn a_source_lists_beside_discovery_and_a_failed_one_keeps_its_last_list() {
        let settings = only_the_given_list();
        let source = Scripted {
            answers: Mutex::new(vec![vec!["ollama/llama3.2:latest"]]),
        };
        let registry =
            FileRegistry::from_files(&settings, None, None).with_source(Arc::new(source));
        registry.discover().await;
        assert_eq!(listed(&registry), ["ollama/llama3.2:latest"]);
        assert_eq!(registry.discovery_skipped(), [OFF]);

        // The second ask fails, so the first list stands, and why is kept.
        registry.discover().await;
        assert_eq!(listed(&registry), ["ollama/llama3.2:latest"]);
        let worker = ModelRef::new(CLI, Some("ollama/llama3.2:latest".to_owned()));
        assert!(registry.get(&worker).is_some());
        assert_eq!(
            registry.discovery_skipped(),
            [OFF.to_owned(), format!("{CLI}: the server listed nothing")]
        );
    }

    #[tokio::test]
    async fn one_source_asked_again_keeps_the_others_and_says_when_it_listed() {
        let settings = only_the_given_list();
        // Answers are taken from the end: first one model, then two, then a failure.
        let source = Scripted {
            answers: Mutex::new(vec![
                vec!["ollama/llama3.2:latest", "ollama/devstral:24b"],
                vec!["ollama/llama3.2:latest"],
            ]),
        };
        let registry =
            FileRegistry::from_files(&settings, None, None).with_source(Arc::new(source));
        assert_eq!(registry.sources(), [CLI]);
        registry.discover().await;
        let (first, failed) = registry.source_listed(CLI);
        assert!(first.is_some() && !failed);

        registry.discover_source(CLI).await;
        assert_eq!(
            listed(&registry),
            ["ollama/llama3.2:latest", "ollama/devstral:24b"]
        );
        let (second, _) = registry.source_listed(CLI);
        assert!(second >= first);

        registry.discover_source(CLI).await;
        assert_eq!(
            listed(&registry).len(),
            2,
            "a failed ask keeps the last list"
        );
        assert_eq!(registry.source_listed(CLI), (second, true));
        // An id that is no source changes nothing.
        registry.discover_source("no-such-cli").await;
        assert_eq!(listed(&registry).len(), 2);
    }
}
