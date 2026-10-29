//! The Behaviors map in Overview › Behaviors (`[behaviors]`): which map the tab shows and how it
//! is drawn, who writes its summaries, what turns them into vectors, and when new
//! actions are added.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The value that leaves a choice to Argonauts Code.
pub const AUTO: &str = "auto";

/// The embedder that needs no model: word counts over the summaries.
pub const WORD_VECTORS: &str = "words";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct BehaviorSettings {
    /// The map the tab shows: `labels`, placed by rule, or `summaries`, placed by a model's summary
    /// of each action. `d` in the tab switches it
    pub map: BehaviorMap,
    /// How the tab draws its map: `auto` draws the world where the app sends the terminal kitty or
    /// iTerm2 pictures, as its environment says, and the braille map elsewhere; `world` or
    /// `braille` always draws that one
    pub view: BehaviorView,
    /// Who writes every summary of Summaries: `auto` picks a model served on this machine,
    /// else the main CLI's cheapest model after asking, or a model such as
    /// `ollama/qwen3-coder:30b`. One summarizer writes a whole map
    pub summarizer: String,
    /// What turns summaries into vectors: `auto` (nomic-embed-text on Ollama, asked before its
    /// download, else an embedding endpoint with a key, else word vectors), `words`, or a model
    /// such as `ollama/nomic-embed-text`
    pub embedder: String,
    /// When Summaries add new actions: `off`, only when you press `d` or run
    /// `argonauts behaviors --summaries`, or `after each run`
    pub update: BehaviorUpdate,
}

impl Default for BehaviorSettings {
    fn default() -> Self {
        Self {
            map: BehaviorMap::Labels,
            view: BehaviorView::Auto,
            summarizer: AUTO.to_owned(),
            embedder: AUTO.to_owned(),
            update: BehaviorUpdate::Off,
        }
    }
}

impl BehaviorSettings {
    pub fn summarizer_is_auto(&self) -> bool {
        is_auto(&self.summarizer)
    }

    pub fn embedder_is_auto(&self) -> bool {
        is_auto(&self.embedder)
    }
}

fn is_auto(value: &str) -> bool {
    let value = value.trim();
    value.is_empty() || value == AUTO
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BehaviorMap {
    #[default]
    Labels,
    Summaries,
}

impl BehaviorMap {
    pub fn as_str(self) -> &'static str {
        match self {
            BehaviorMap::Labels => "labels",
            BehaviorMap::Summaries => "summaries",
        }
    }

    /// The other map, which `d` switches to.
    pub fn other(self) -> BehaviorMap {
        match self {
            BehaviorMap::Labels => BehaviorMap::Summaries,
            BehaviorMap::Summaries => BehaviorMap::Labels,
        }
    }
}

/// `behaviors.view`. The Behaviors tab reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BehaviorView {
    /// The world where the terminal shows pictures, the braille map elsewhere.
    #[default]
    Auto,
    /// The world: the map, the planet and the universe, drawn as pictures.
    World,
    /// The braille map, drawn in text cells.
    Braille,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub enum BehaviorUpdate {
    #[default]
    #[serde(rename = "off")]
    Off,
    #[serde(rename = "after each run")]
    AfterEachRun,
}

impl BehaviorUpdate {
    pub fn as_str(self) -> &'static str {
        match self {
            BehaviorUpdate::Off => "off",
            BehaviorUpdate::AfterEachRun => "after each run",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_label_map_shows_and_nothing_is_summarized_until_you_ask() {
        let behaviors = BehaviorSettings::default();
        assert_eq!(behaviors.map, BehaviorMap::Labels);
        assert_eq!(behaviors.view, BehaviorView::Auto);
        assert!(behaviors.summarizer_is_auto());
        assert!(behaviors.embedder_is_auto());
        assert_eq!(behaviors.update, BehaviorUpdate::Off);
    }

    #[test]
    fn each_key_reads_its_words() {
        let set: BehaviorSettings = toml::from_str(
            "map = \"summaries\"\nsummarizer = \"ollama/qwen3-coder:30b\"\nembedder = \"words\"\n\
             update = \"after each run\"\n",
        )
        .unwrap();
        assert_eq!(set.map.as_str(), "summaries");
        assert_eq!(set.map.other(), BehaviorMap::Labels);
        assert!(!set.summarizer_is_auto());
        assert_eq!(set.embedder, WORD_VECTORS);
        assert_eq!(set.update.as_str(), "after each run");
        assert!(toml::from_str::<BehaviorSettings>("update = \"hourly\"").is_err());
        assert!(toml::from_str::<BehaviorSettings>("map = \"both\"").is_err());
        for (word, view) in [
            ("auto", BehaviorView::Auto),
            ("world", BehaviorView::World),
            ("braille", BehaviorView::Braille),
        ] {
            let set: BehaviorSettings = toml::from_str(&format!("view = \"{word}\"")).unwrap();
            assert_eq!(set.view, view);
        }
        assert!(toml::from_str::<BehaviorSettings>("view = \"planet\"").is_err());
    }
}
