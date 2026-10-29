//! The kept Summaries as the Behaviors map reads them: each summarized action's place and
//! summary by the key the map's records give it, and who summarized them. Reading it calls no
//! model and reads no run's log.

use std::collections::HashMap;

use argonauts_core::Paths;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::ModelRef;
use argonauts_core::registry::provider::provider_name;
use argonauts_core::settings::behaviors::WORD_VECTORS;

use super::action::ActionKey;
use super::choose::local_name;
use super::records::CONVERSATION;
use super::store::{MapMaker, Store};
use crate::adapters::builtin_adapters;

/// One summarized action on Summaries, both axes in 0..1.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct KeptPlace {
    pub x: f32,
    pub y: f32,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct KeptPlaces {
    /// Who summarized them, as the line under the map and the legend name the model:
    /// `Qwen3 Coder 30B (64k)`.
    pub by: String,
    /// They were placed by their words, with no embedding model.
    pub by_words: bool,
    places: HashMap<String, KeptPlace>,
}

impl KeptPlaces {
    pub fn new(by: String, places: impl IntoIterator<Item = (String, KeptPlace)>) -> KeptPlaces {
        KeptPlaces {
            by,
            by_words: false,
            places: places.into_iter().collect(),
        }
    }

    /// The place of the action the map's records key `repo/run/session/call`, or for the main
    /// agent's own turn `repo/conversation/turn/call`.
    pub fn of(&self, key: &str) -> Option<&KeptPlace> {
        self.places.get(key)
    }
}

/// The Summaries that are kept, or `None` before they're made.
pub(crate) fn kept_places(paths: &Paths, names: &DisplayNames) -> Option<KeptPlaces> {
    let store = Store::new(paths);
    let layout = store.layout()?;
    let summaries = store.summaries(&layout.maker.summarizer, &layout.maker.prompt);
    let places: Vec<(String, KeptPlace)> = layout
        .points
        .into_iter()
        .filter_map(|point| {
            let summary = summaries.by_action.get(&point.action)?.clone();
            let place = KeptPlace {
                x: point.x,
                y: point.y,
                summary,
            };
            Some((map_key(&point.action), place))
        })
        .collect();
    let mut kept = KeptPlaces::new(model_name(&layout.maker, names), places);
    kept.by_words = layout.maker.embedder == WORD_VECTORS;
    Some(kept)
}

/// Who made the kept Summaries, by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeptMaker {
    /// Such as `Qwen3 Coder 30B on Ollama`.
    pub summarizer: String,
    /// Such as `nomic-embed-text on Ollama`, or `word vectors`.
    pub embedder: String,
}

/// Who made the Summaries that are kept, or `None` before they're made. Only its maker is read,
/// so `/config` can say what auto came to as it opens.
pub(crate) fn kept_maker(paths: &Paths, names: &DisplayNames) -> Option<KeptMaker> {
    let maker = Store::new(paths).maker()?;
    Some(KeptMaker {
        summarizer: summarizer_of(&maker, names),
        embedder: embedder_name(&maker.embedder, names),
    })
}

/// The summarizer as the legend names a model: `Qwen3 Coder 30B (64k)`.
fn model_name(maker: &MapMaker, names: &DisplayNames) -> String {
    maker
        .summarizer
        .parse::<ModelRef>()
        .map_or(maker.summarizer.clone(), |model| {
            let model = names.running(&model);
            names
                .model(&model.agent, model.model.as_deref())
                .unwrap_or_else(|| names.cli(&model.agent))
        })
}

fn summarizer_of(maker: &MapMaker, names: &DisplayNames) -> String {
    maker
        .summarizer
        .parse::<ModelRef>()
        .map_or(maker.summarizer.clone(), |model| {
            summarizer_name(&model, names)
        })
}

/// As the questions named it: Ollama's model on Ollama, a keyed endpoint's on its provider.
fn embedder_name(embedder: &str, names: &DisplayNames) -> String {
    if embedder == WORD_VECTORS {
        return "word vectors".to_owned();
    }
    match embedder.split_once('/') {
        Some(("ollama", model)) => format!("{model} on {}", names.cli("ollama")),
        Some((endpoint, model)) => format!("{model} on {}", provider_name(endpoint)),
        None => embedder.to_owned(),
    }
}

/// The map's key for an action: a run's as `repo/run/session/call`, a conversation's as
/// `repo/conversation/turn/call` without the folder the store names it by.
fn map_key(action: &ActionKey) -> String {
    let run = action
        .run
        .strip_prefix(CONVERSATION)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(&action.run);
    format!("{}/{run}/{}/{}", action.repo, action.session, action.call)
}

/// As the questions named it: a model on this machine by its name and its server, a CLI's
/// model as the CLI's.
fn summarizer_name(model: &ModelRef, names: &DisplayNames) -> String {
    let on_a_cli = builtin_adapters()
        .iter()
        .any(|adapter| adapter.id() == model.agent);
    match &model.model {
        Some(name) if !on_a_cli => format!("{} on {}", local_name(name), names.cli(&model.agent)),
        _ => names.worker(model),
    }
}

#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

    use super::super::records::recorded;
    use super::super::summarize::fakes::Echo;
    use super::super::update::{EmbedWith, update};
    use super::*;

    #[test]
    fn a_runs_key_and_a_conversations_read_as_the_maps_records_give_them() {
        let run = ActionKey {
            repo: "council-3f9a1c2b4d5e".to_owned(),
            run: "run-01".to_owned(),
            session: "s-001".to_owned(),
            call: "t0".to_owned(),
        };
        assert_eq!(map_key(&run), "council-3f9a1c2b4d5e/run-01/s-001/t0");
        let turn = ActionKey {
            repo: "council-3f9a1c2b4d5e".to_owned(),
            run: format!("{CONVERSATION}/c-7"),
            session: "3".to_owned(),
            call: "t1".to_owned(),
        };
        assert_eq!(map_key(&turn), "council-3f9a1c2b4d5e/c-7/3/t1");
    }

    /// The line under the map names the summarizer as the legend names a model, tag and all.
    #[test]
    fn the_map_names_its_summarizer_as_the_legend_does() {
        let maker = MapMaker {
            summarizer: "ollama/qwen3-coder:30b-64k".to_owned(),
            prompt: String::new(),
            embedder: WORD_VECTORS.to_owned(),
        };
        let names = DisplayNames::default();
        assert_eq!(
            Some(model_name(&maker, &names)),
            names.model("ollama", Some("qwen3-coder:30b-64k"))
        );
        assert!(summarizer_of(&maker, &names).starts_with("Qwen3 Coder 30B on "));
    }

    #[test]
    fn a_summarizer_is_named_as_the_questions_name_it() {
        let names = DisplayNames::default();
        let local = "ollama/qwen3-coder:30b-64k".parse().unwrap();
        assert!(summarizer_name(&local, &names).starts_with("Qwen3 Coder 30B on "));
        let hosted: ModelRef = "claude-code/haiku".parse().unwrap();
        assert_eq!(summarizer_name(&hosted, &names), "Haiku on Claude Code");
        let known: ModelRef = "claude-code/claude-haiku-5-5".parse().unwrap();
        let catalog = DisplayNames {
            catalog: argonauts_registry::models::shipped_catalog().clone(),
            ..DisplayNames::default()
        };
        assert_eq!(summarizer_name(&known, &catalog), catalog.worker(&known));
    }

    #[test]
    fn an_embedder_is_named_as_the_questions_name_it() {
        let names = DisplayNames::default();
        let named = |embedder: &str| embedder_name(embedder, &names);
        assert_eq!(
            named("ollama/nomic-embed-text"),
            "nomic-embed-text on Ollama"
        );
        assert_eq!(
            named("openai/text-embedding-3-small"),
            "text-embedding-3-small on OpenAI"
        );
        assert_eq!(named(WORD_VECTORS), "word vectors");
    }

    #[tokio::test]
    async fn the_kept_map_is_read_by_the_maps_keys_with_no_model() {
        let home = tempfile::tempdir().unwrap();
        let run = home.path().join("state/council-3f9a1c2b4d5e/runs/run-01");
        std::fs::create_dir_all(&run).unwrap();
        let line = serde_json::json!({"seq": 0, "ts": "2026-10-17T07:17:01Z", "run_id": "run-01",
            "source": "claude-code", "session": "s-001",
            "kind": {"type": "agent", "data": {"type": "tool_started", "id": "t0",
                "name": "Bash", "input": {"command": "cargo test"}, "parent": null}}});
        std::fs::write(run.join("events.jsonl"), format!("{line}\n")).unwrap();
        let paths = Paths::new(home.path());
        let names = DisplayNames::default();
        assert!(
            kept_places(&paths, &names).is_none(),
            "none before one is made"
        );
        assert_eq!(kept_maker(&paths, &names), None);
        update(
            &recorded(&paths),
            |_| true,
            &Store::new(&paths),
            &Echo::new("ollama/qwen3-coder:30b"),
            EmbedWith::Words,
            &CancellationToken::new(),
            &mut |_| {},
            &mut || {},
        )
        .await
        .unwrap();
        let kept = kept_places(&paths, &names).unwrap();
        assert_eq!(kept.by, "Qwen3 Coder 30B");
        assert!(kept.by_words, "no embedding model answers in a test");
        let maker = kept_maker(&paths, &names).unwrap();
        assert!(
            maker.summarizer.starts_with("Qwen3 Coder 30B on "),
            "{}",
            maker.summarizer
        );
        assert_eq!(maker.embedder, "word vectors");
        let place = kept
            .of("council-3f9a1c2b4d5e/run-01/s-001/t0")
            .expect("the action's place");
        assert!(place.summary.starts_with("purpose of tool Bash"));
        assert!((0.0..=1.0).contains(&place.x) && (0.0..=1.0).contains(&place.y));
    }
}
