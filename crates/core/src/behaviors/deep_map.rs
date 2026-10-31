//! What Summaries' questions offer and its lines show, as the app counted them:
//! what to summarize, with how long each would take, who may summarize, with what each costs,
//! the embedder, and how far an update is.

use serde::{Deserialize, Serialize};

use crate::app::ModelChoice;
use crate::app::model_picker::{Fit, MachineMemory};
use crate::base::names::ModelRef;
use crate::behaviors::{Behavior, Label};

/// Labels that only group what was done, where a summary says what it was.
const GROUPING: [Label; 2] = [Label::Answer, Label::Shell];

/// Whether an action's summary says more than its label, given as the map shows it: the label
/// only groups what was done, as any structured answer or shell command does, or no rule
/// labelled it. Summaries' list marks such a row new.
pub fn says_more_than(label: &str) -> bool {
    let label = label.trim();
    label.is_empty()
        || label == Behavior::Unlabelled.words()
        || GROUPING
            .iter()
            .any(|grouping| label.starts_with(grouping.words()))
}

/// Which actions an update summarizes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum DeepScope {
    Everything,
    /// The project you're in, by its folder's name.
    Project {
        name: String,
    },
    /// The actions of the last `days` days.
    Days {
        days: u32,
    },
    /// The actions since `at`, as `argonauts behaviors --summaries --since` names it.
    Since {
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
    },
}

/// One CLI's share of the actions, which a hosted summarizer would read too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliCount {
    /// Such as `codex`.
    pub cli: String,
    /// Such as `Codex`.
    pub name: String,
    pub actions: usize,
}

/// One answer to what to summarize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeOffer {
    pub scope: DeepScope,
    pub actions: usize,
    /// The projects they're in.
    pub projects: usize,
    pub by_cli: Vec<CliCount>,
    /// How long the summarizer auto picks would take, in seconds.
    pub seconds: u64,
    /// About how many tokens a hosted summarizer would use.
    pub tokens: u64,
}

/// Where a summarizer runs, and so what it costs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "place", rename_all = "snake_case")]
pub enum OfferPlace {
    /// On a server on this machine: free, and nothing leaves it.
    ThisMachine {
        /// Its weights.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fit: Option<Fit>,
    },
    /// A CLI's model, on your plan with its provider.
    Plan {
        /// Such as `claude-code`.
        cli: String,
        /// Such as `Claude`, as the plan is called.
        plan: String,
        /// Such as `Anthropic`, where the actions would go.
        provider: String,
    },
}

/// One answer to who summarizes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummarizerOffer {
    pub model: ModelRef,
    /// Such as `Qwen3 Coder 30B on Ollama`.
    pub name: String,
    /// Such as `Qwen3 Coder 30B`.
    pub short: String,
    pub place: OfferPlace,
}

impl SummarizerOffer {
    pub fn local(&self) -> bool {
        matches!(self.place, OfferPlace::ThisMachine { .. })
    }
}

/// What turns the summaries into vectors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "embedder", rename_all = "snake_case")]
pub enum EmbedderNote {
    /// Such as `nomic-embed-text on Ollama`.
    Ready { name: String },
    /// Downloaded once you say yes.
    Download { name: String, megabytes: u32 },
    /// No embedding model: word vectors, made from the summaries' words.
    Words,
}

/// Everything Summaries' questions offer, the recommended answers first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeepOffer {
    pub scopes: Vec<ScopeOffer>,
    /// The one auto picks first.
    pub summarizers: Vec<SummarizerOffer>,
    pub embedder: EmbedderNote,
    /// This computer is a Mac, as the questions call it.
    #[serde(default)]
    pub mac: bool,
    /// Who made the map that's there, which another summarizer or prompt would redo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub made_by: Option<MadeBy>,
    /// The summarizer `behaviors.summarizer` names, when it names one rather than auto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub named: Option<ModelRef>,
    /// The embedding model of an endpoint with your key, which auto turns to without Ollama.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyed_embedder: Option<EmbedderOffer>,
    /// What "Open models" lists.
    #[serde(default)]
    pub open: OpenModels,
}

/// Every model the servers on this computer have, as `/model` on OpenCode lists them with what
/// each takes of its memory, then up to three from the catalog that would fit. Each `worker`
/// is the summarizer itself, such as `ollama/qwen3-coder:30b`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenModels {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<MachineMemory>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ModelChoice>,
    /// A server installed here that isn't running, which Open models offers to start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<StoppedServer>,
}

impl OpenModels {
    /// Whether the question has anything to open.
    pub fn any(&self) -> bool {
        !self.models.is_empty() || self.stopped.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoppedServer {
    /// Such as `ollama`.
    pub runtime: String,
    /// Such as `Ollama`.
    pub name: String,
    /// It was started just now and didn't answer within the wait.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tried: bool,
}

/// An embedding model the embedder's picker offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbedderOffer {
    /// Such as `openai/text-embedding-3-small`.
    pub model: ModelRef,
    /// Such as `text-embedding-3-small on OpenAI`.
    pub name: String,
}

/// The summarizer of the map that's there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MadeBy {
    pub model: ModelRef,
    /// Such as `Qwen3 Coder 30B on Ollama`.
    pub name: String,
    /// Its dots.
    pub actions: usize,
    /// Recorded actions it has no summary of yet, which an update would add.
    #[serde(default)]
    pub new_actions: usize,
    /// Its summaries were written with an older prompt.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub prompt_changed: bool,
}

impl MadeBy {
    /// Whether summarizing with `summarizer` would write every summary again.
    pub fn redone_by(&self, summarizer: &ModelRef) -> bool {
        self.prompt_changed || &self.model != summarizer
    }
}

/// The answers to Summaries' questions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeepStart {
    pub scope: DeepScope,
    pub summarizer: ModelRef,
    /// You said yes to downloading the embedding model.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub download_embedder: bool,
    /// You said yes to summarizing a map made by another summarizer or prompt again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub redo: bool,
    /// You said yes to downloading the summarizer, which isn't on this computer yet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub download_summarizer: bool,
}

/// How far an update is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeepProgress {
    pub done: usize,
    pub total: usize,
    /// At the pace so far, once a request came back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds_left: Option<u64>,
    /// Such as `Qwen3 Coder 30B on Ollama`.
    pub summarizer: String,
    /// Such as `Qwen3 Coder 30B`, for a line that names it alone.
    #[serde(default)]
    pub short: String,
    /// It makes the map again, with another summarizer or a newer prompt.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub redo: bool,
    /// It downloads the summarizer first: `done` and `total` count megabytes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub downloading: bool,
}

/// What an update did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeepDone {
    /// Summaries written now.
    pub written: usize,
    /// Actions given an identical action's summary, with no request.
    pub reused: usize,
    pub already: usize,
    /// Dots on the map, and how many are new.
    pub placed: usize,
    pub new_places: usize,
    /// It stopped when asked, keeping what it wrote.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stopped: bool,
    /// Why the summarizer stopped answering, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
    /// Why the embedder wasn't used, when word vectors took its place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedder_failed: Option<String>,
    /// Placed by word vectors, as no embedding model was there.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub by_words: bool,
    /// The folders and logs of the records that couldn't be read, each with why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unread: Vec<String>,
    /// A whole new map that wasn't swapped in, since it had fewer dots than the map there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fewer: Option<FewerDots>,
}

/// The dots of a new map that would have replaced a bigger one, and of the map it kept.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FewerDots {
    pub made: usize,
    pub shown: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_summary_says_more_than_a_label_that_only_groups() {
        assert!(says_more_than("returned its structured answer"));
        assert!(says_more_than("ran a shell command and read a source file"));
        assert!(says_more_than(""));
        assert!(says_more_than("no label fits"));
        assert!(!says_more_than(
            "checked git status or diff and ran a shell command"
        ));
        assert!(!says_more_than("ran the test suite"));
    }
}
