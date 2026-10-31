//! The names a seat must not read: models, makers, coding tools, the ways a model names
//! itself, signatures and each CLI's marks. One lexicon is built for each council, from the
//! built-in lists, every model set up and each adapter's own marks.

use std::ops::Range;

use argonauts_core::agents::adapter::{CitationForm, IdentityMarks};
use argonauts_core::base::names::ModelRef;
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::models::registry::provider::{is_known, provider_name};
use argonauts_core::runs::events::council::HiddenName;
use argonauts_core::settings::crew::CrewRole;

use crate::engine::host::Host;
use crate::roles;

mod repository;

/// One name, matched without regard to case unless it reads as an ordinary word in lowercase.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Name {
    words: String,
    kind: HiddenName,
    case_sensitive: bool,
    /// A model line, such as `gpt`, takes the version after it too: `GPT-5.6`, `Opus 4.7`.
    takes_version: bool,
}

/// A name found in a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub range: Range<usize>,
    pub kind: HiddenName,
}

/// Makers, as the registry and people name them.
const MAKERS: &[&str] = &[
    "Anthropic",
    "OpenAI",
    "Open AI",
    "Google DeepMind",
    "DeepMind",
    "Google",
    "Meta AI",
    "Mistral AI",
    "Alibaba Cloud",
    "Alibaba",
    "Moonshot AI",
    "Moonshot",
    "Zhipu AI",
    "Zhipu",
    "Z.ai",
    "xAI",
    "Microsoft",
    "NVIDIA",
    "Cohere",
    "AI2",
    "Allen Institute for AI",
    "MiniMax",
    "BigCode",
    "Hugging Face",
    "Shanghai AI Lab",
    "LG AI Research",
];

/// Makers whose name is an ordinary word in lowercase, matched only as written.
const MAKERS_AS_WRITTEN: &[&str] = &["Meta", "IBM", "TII"];

/// Model lines, each of which takes the version written after it.
const MODEL_LINES: &[&str] = &[
    "claude",
    "opus",
    "sonnet",
    "haiku",
    "mythos",
    "chatgpt",
    "gpt",
    "gemini",
    "codegemma",
    "gemma",
    "codeqwen",
    "qwen",
    "qwq",
    "codellama",
    "llama",
    "mixtral",
    "codestral",
    "devstral",
    "ministral",
    "magistral",
    "pixtral",
    "mistral",
    "deepseek",
    "kimi",
    "glm",
    "grok",
    "nemotron",
    "starcoder",
    "olmo",
    "internlm",
    "exaone",
    "smollm",
];

/// Words a model's name carries besides its own, which no seat is told apart by: "Flash" is
/// one model's word, "Pro" every maker's.
const COMMON_MODEL_WORDS: &[&str] = &[
    "chat",
    "code",
    "fast",
    "free",
    "high",
    "instruct",
    "large",
    "latest",
    "lite",
    "low",
    "max",
    "medium",
    "mini",
    "nano",
    "plus",
    "preview",
    "pro",
    "reasoning",
    "small",
    "super",
    "thinking",
    "turbo",
    "ultra",
    "vl",
    "xhigh",
];

/// What a model's id or name may end with that only says which variant runs: an effort or a
/// size. A dash and a token with a digit, as `-R1` or `-K2`, says so too.
const VARIANTS: &[&str] = &[
    "high", "low", "medium", "xhigh", "max", "min", "mini", "nano", "fast", "instruct", "thinking",
    "latest", "preview", "turbo", "lite", "free", "chat", "coder", "pro", "plus",
];

/// Coding tools and the programs that serve models.
const CLIS: &[&str] = &[
    "Claude Code",
    "Codex CLI",
    "Codex",
    "Antigravity",
    "agy",
    "OpenCode",
    "OpenRouter",
    "Ollama",
    "LM Studio",
    "llama.cpp",
];

/// What a CLI's tool does, in the plain words a relayed message uses for it.
const PLAIN_TOOLS: &[(&str, &str)] = &[
    ("Bash", "shell"),
    ("shell", "shell"),
    ("exec_command", "shell"),
    ("run_shell_command", "shell"),
    ("Read", "read"),
    ("read_file", "read"),
    ("Edit", "edit"),
    ("MultiEdit", "edit"),
    ("edit_file", "edit"),
    ("Write", "write"),
    ("write_file", "write"),
    ("apply_patch", "patch"),
    ("TodoWrite", "to-do list"),
    ("todowrite", "to-do list"),
    ("update_plan", "plan"),
    ("Task", "agent"),
    ("Glob", "file search"),
    ("Grep", "search"),
    ("WebFetch", "web fetch"),
    ("WebSearch", "web search"),
    ("NotebookEdit", "notebook edit"),
    ("run_command", "shell"),
    ("bash", "shell"),
    ("execute", "shell"),
    ("view_file", "read"),
    ("write_to_file", "write"),
    ("find_by_name", "file search"),
    ("glob", "file search"),
    ("grep", "search"),
    ("patch", "patch"),
    ("read", "read"),
    ("edit", "edit"),
    ("write", "write"),
    ("todoread", "to-do list"),
    ("webfetch", "web fetch"),
    ("web_search", "web search"),
    ("invoke_subagent", "agent"),
    ("spawn_agent", "agent"),
    ("Agent", "agent"),
    ("task", "agent"),
    ("StructuredOutput", "answer"),
    ("Workflow", "workflow"),
    ("Skill", "skill"),
    ("ToolSearch", "tool search"),
    ("finish", "finish"),
    ("question", "question"),
    ("wait", "wait"),
    ("close_agent", "agent"),
];

/// The council's lexicon: every name to hide, every CLI's marks, and the topic words that
/// stay as written because the talk may be about them.
#[derive(Debug, Clone, Default)]
pub struct Lexicon {
    names: Vec<Name>,
    tools: Vec<&'static str>,
    row_marks: Vec<&'static str>,
    home_folders: Vec<&'static str>,
    citations: Vec<CitationForm>,
    topic: Vec<String>,
    /// The words of a set-up model's name that tell it apart, such as "Luna" or "Flash": a
    /// model named by one of them is named, and a name runs on through them.
    model_words: Vec<String>,
    /// The names of the models that sit, lowercase, which never become topic words: a seat
    /// your request names would otherwise reach every seat as written.
    sitting: Vec<String>,
}

impl Lexicon {
    /// The built-in makers, model lines and CLIs, with no CLI's marks yet.
    pub fn built_in() -> Self {
        let mut lexicon = Self::default();
        for maker in MAKERS {
            lexicon.add(maker, HiddenName::Maker, false, false);
        }
        for maker in MAKERS_AS_WRITTEN {
            lexicon.add(maker, HiddenName::Maker, true, false);
        }
        for line in MODEL_LINES {
            lexicon.add(line, HiddenName::Model, false, true);
        }
        for cli in CLIS {
            lexicon.add(cli, HiddenName::Cli, false, false);
            // As prose runs its words together: "ClaudeCode", "claude_code".
            if cli.contains(' ') {
                lexicon.add(&cli.replace(' ', ""), HiddenName::Cli, false, false);
                lexicon.add(&cli.replace(' ', "_"), HiddenName::Cli, false, false);
            }
        }
        lexicon
    }

    /// Adds each model set up: its CLI's name, its model's id and display names, and its
    /// maker as the registry tells it through `provider`.
    pub fn with_models(
        mut self,
        models: &[ModelRef],
        names: &DisplayNames,
        provider: &dyn Fn(&ModelRef) -> String,
    ) -> Self {
        for model in models {
            self.add(&names.cli(&model.agent), HiddenName::Cli, false, false);
            self.add(&model.agent, HiddenName::Cli, false, false);
            if let Some(id) = &model.model {
                self.add(id, HiddenName::Model, false, false);
                // A provider's own folder, as `ollama/qwen3-coder`, names the model too.
                if let Some((_, bare)) = id.rsplit_once('/') {
                    self.add(bare, HiddenName::Model, false, false);
                }
            }
            if let Some(shown) = names.model(&model.agent, model.model.as_deref()) {
                self.add(&shown, HiddenName::Model, false, false);
                self.add_model_words(&shown, model.model.as_deref());
            }
            let maker = provider(model);
            if is_known(&maker) {
                self.add(&provider_name(&maker), HiddenName::Maker, false, false);
            }
        }
        self
    }

    /// Adds one CLI's tool names, row marks, home folders and citation form.
    pub fn with_marks(mut self, marks: IdentityMarks) -> Self {
        for tool in marks.tool_names {
            if !self.tools.contains(tool) {
                self.tools.push(tool);
            }
        }
        for mark in marks.row_marks {
            if !self.row_marks.contains(mark) {
                self.row_marks.push(mark);
            }
        }
        for folder in marks.home_folders {
            if !self.home_folders.contains(folder) {
                self.home_folders.push(folder);
            }
        }
        if let Some(citation) = marks.citation
            && !self.citations.contains(&citation)
        {
            self.citations.push(citation);
        }
        // The longest mark first, so `• Ran` wins over `•`.
        self.row_marks
            .sort_by_key(|mark| std::cmp::Reverse(mark.len()));
        self
    }

    /// Adds the models that sit as names, which stay names wherever the topic names them.
    pub fn with_seats(mut self, models: &[ModelRef], names: &DisplayNames) -> Self {
        for model in models {
            let mut called: Vec<String> = Vec::new();
            if let Some(id) = &model.model {
                called.push(id.clone());
                if let Some((_, bare)) = id.rsplit_once('/') {
                    called.push(bare.to_owned());
                }
            }
            called.extend(names.model(&model.agent, model.model.as_deref()));
            for name in called {
                self.add(&name, HiddenName::Model, false, false);
                let name = name.trim().to_ascii_lowercase();
                if !self.sitting.contains(&name) {
                    self.sitting.push(name);
                }
            }
        }
        self
    }

    /// Keeps as written every name `text` holds, such as your request's or a path in the
    /// repository: "the Codex adapter" stays when the talk is about it. A seat's model never
    /// does.
    pub fn with_topic(mut self, text: &str) -> Self {
        let hits = self.find_all(text);
        for hit in hits {
            let words = text[hit.range].to_ascii_lowercase();
            if !self.topic.contains(&words) && !self.sitting.contains(&words) {
                self.topic.push(words);
            }
        }
        self
    }

    /// A model's name as drawn after its maker's logo, "5.6 Luna" for "GPT 5.6 Luna", and each
    /// word of it that tells it apart, matched as written so "flash" stays an ordinary word.
    fn add_model_words(&mut self, shown: &str, id: Option<&str>) {
        let maker_word = id
            .or(Some(shown))
            .and_then(argonauts_core::look::display_names::maker_of)
            .and_then(|maker| maker.word);
        let after_logo = maker_word.and_then(|word| {
            let (first, rest) = shown.split_once([' ', '-'])?;
            first.eq_ignore_ascii_case(word).then(|| rest.trim())
        });
        if let Some(after_logo) = after_logo.filter(|rest| !rest.is_empty()) {
            self.add(after_logo, HiddenName::Model, false, false);
        }
        for word in shown.split([' ', '-']) {
            let lower = word.to_ascii_lowercase();
            let distinct = word.chars().any(|c| c.is_ascii_alphabetic())
                && !word.chars().any(|c| c.is_ascii_digit())
                && !MODEL_LINES.contains(&lower.as_str())
                && !COMMON_MODEL_WORDS.contains(&lower.as_str())
                && maker_word.is_none_or(|maker| !maker.eq_ignore_ascii_case(word));
            if distinct {
                self.add(word, HiddenName::Model, true, false);
                if !self.model_words.contains(&lower) {
                    self.model_words.push(lower);
                }
            }
        }
    }

    fn add(&mut self, words: &str, kind: HiddenName, case_sensitive: bool, takes_version: bool) {
        let words = words.trim();
        // A name shorter than three letters would hide ordinary words.
        let letters = words.chars().filter(char::is_ascii_alphanumeric).count();
        if letters < 3 || !words.is_ascii() {
            return;
        }
        if self
            .names
            .iter()
            .any(|name| name.words.eq_ignore_ascii_case(words))
        {
            return;
        }
        self.names.push(Name {
            words: words.to_owned(),
            kind,
            case_sensitive,
            takes_version,
        });
        // The longest first, so `Claude Code` wins over `Claude`.
        self.names
            .sort_by_key(|name| std::cmp::Reverse(name.words.len()));
    }

    /// Every name `text` holds outside the topic words, in order and never overlapping.
    pub fn find(&self, text: &str) -> Vec<Hit> {
        self.find_all(text)
            .into_iter()
            .filter(|hit| !self.is_topic(&text[hit.range.clone()]))
            .collect()
    }

    /// Every name `text` holds, topic words included. A model naming itself, a guess at who
    /// runs a seat and a signature give a seat away even when the talk is about that name.
    pub fn every_name(&self, text: &str) -> Vec<Hit> {
        self.find_all(text)
    }

    /// Whether `words` is one of the topic words that stay.
    pub fn is_topic(&self, words: &str) -> bool {
        let words = words.to_ascii_lowercase();
        self.topic.contains(&words)
    }

    fn find_all(&self, text: &str) -> Vec<Hit> {
        let bytes = text.as_bytes();
        let mut hits = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            if !text.is_char_boundary(at) || (at > 0 && is_word_byte(bytes[at - 1])) {
                at += 1;
                continue;
            }
            match self.name_at(text, at) {
                Some(hit) => {
                    at = hit.range.end;
                    hits.push(hit);
                }
                None => at += 1,
            }
        }
        hits
    }

    fn name_at(&self, text: &str, at: usize) -> Option<Hit> {
        let bytes = text.as_bytes();
        self.names.iter().find_map(|name| {
            let end = at + name.words.len();
            let found = bytes.get(at..end)?;
            let same = if name.case_sensitive {
                found == name.words.as_bytes()
            } else {
                found.eq_ignore_ascii_case(name.words.as_bytes())
            };
            let next = bytes.get(end).copied();
            // A model line may run straight into its version, as `gpt5`.
            let runs_on = next.is_some_and(is_word_byte)
                && !(name.takes_version && next.is_some_and(|byte| byte.is_ascii_digit()));
            if !same || runs_on {
                return None;
            }
            let end = if name.takes_version {
                end + version_after(&text[end..])
            } else {
                end
            };
            let end = if name.kind == HiddenName::Model {
                self.rest_of_model(text, end)
            } else {
                end
            };
            Some(Hit {
                range: at..end,
                kind: name.kind,
            })
        })
    }

    /// Where a model's name found up to `end` really ends: on through the words of a set-up
    /// model's name, " Luna" after "GPT-5.6", and a variant, as `-high` or `-R1`.
    fn rest_of_model(&self, text: &str, mut end: usize) -> usize {
        loop {
            let rest = &text[end..];
            let Some(after) = rest.strip_prefix([' ', '-']) else {
                return end;
            };
            let token_len = after
                .bytes()
                .take_while(|byte| byte.is_ascii_alphanumeric())
                .count();
            let token = &after[..token_len];
            if token.is_empty()
                || after
                    .as_bytes()
                    .get(token_len)
                    .copied()
                    .is_some_and(is_word_byte)
            {
                return end;
            }
            let lower = token.to_ascii_lowercase();
            let model_word = self.model_words.contains(&lower)
                && (rest.starts_with('-') || token.starts_with(|c: char| c.is_ascii_uppercase()));
            let variant = rest.starts_with('-')
                && token.len() <= 12
                && (VARIANTS.contains(&lower.as_str())
                    || token.bytes().any(|b| b.is_ascii_digit()));
            if !(model_word || variant) {
                return end;
            }
            end += 1 + token_len;
        }
    }

    /// The plain words for `tool` when it is one of a CLI's tools, such as `shell` for `Bash`.
    pub fn plain_tool(&self, tool: &str) -> Option<&'static str> {
        if !self.tools.contains(&tool) {
            return None;
        }
        Some(
            PLAIN_TOOLS
                .iter()
                .find(|(name, _)| *name == tool)
                .map_or("a tool", |(_, plain)| plain),
        )
    }

    /// The tool names that read as code, never as an ordinary word, so they go wherever they
    /// are written: `TodoWrite`, `apply_patch`.
    pub fn distinct_tools(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.tools.iter().copied().filter(|tool| {
            tool.contains('_') || tool.chars().skip(1).any(|c| c.is_ascii_uppercase())
        })
    }

    /// Every CLI's tool names, as a call such as `Bash(npm test)` writes them.
    pub fn tools(&self) -> &[&'static str] {
        &self.tools
    }

    pub fn row_marks(&self) -> &[&'static str] {
        &self.row_marks
    }

    pub fn home_folders(&self) -> &[&'static str] {
        &self.home_folders
    }

    pub fn citations(&self) -> &[CitationForm] {
        &self.citations
    }
}

/// The lexicon a run's seats, drafts and checks are read through: the built-in names, every
/// model the crew and the council set up with its CLI's own marks, and as topic words the
/// names your request uses and those in the repository's paths and identifiers.
pub fn of_run(host: &Host) -> Lexicon {
    let settings = host.settings();
    let workers = &host.services().workers;
    let mut models: Vec<ModelRef> = settings
        .council
        .seats
        .iter()
        .map(|seat| seat.model.clone())
        .collect();
    for role in CrewRole::ALL {
        models.extend(settings.models_for(role));
    }
    models.extend(workers.available());
    let mut seen = Vec::new();
    models.retain(|model| {
        let new = !seen.contains(model);
        seen.push(model.clone());
        new
    });
    let provider = |model: &ModelRef| roles::provider(host, model);
    let sitting: Vec<ModelRef> = super::seats::seat(settings, &DisplayNames::default())
        .unwrap_or_default()
        .into_iter()
        .map(|seat| seat.model)
        .collect();
    let mut lexicon = Lexicon::built_in()
        .with_models(&models, &DisplayNames::default(), &provider)
        .with_seats(&sitting, &DisplayNames::default());
    for model in &models {
        if let Ok(runner) = workers.get(model) {
            lexicon = lexicon.with_marks(runner.identity_marks());
        }
    }
    let request = host.read(|state| {
        state
            .run
            .started
            .as_ref()
            .map(|started| started.request.clone())
    });
    // The run's base checkout stays at one commit, so its words are searched once a run.
    let checkout = &host.repo().base_worktree;
    if checkout.is_dir() {
        for word in repository::words(checkout, &lexicon).iter() {
            lexicon = lexicon.with_topic(word);
        }
    }
    lexicon.with_topic(&request.unwrap_or_default())
}

/// A byte that belongs to a word or an identifier, so a name inside `codex_adapter` or
/// `metadata` is no name.
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// How many bytes of `rest` are a model's version, as ` 4.7`, `-5.6` or `3`: a space or a dash
/// and then digits and dots, or digits at once.
fn version_after(rest: &str) -> usize {
    let bytes = rest.as_bytes();
    let start = match bytes.first() {
        Some(b' ' | b'-') if bytes.get(1).is_some_and(u8::is_ascii_digit) => 1,
        Some(byte) if byte.is_ascii_digit() => 0,
        _ => return 0,
    };
    let mut end = start;
    while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
        end += 1;
    }
    // A sentence's full stop after the version isn't part of it.
    while end > start && bytes[end - 1] == b'.' {
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hidden(lexicon: &Lexicon, text: &str) -> Vec<String> {
        lexicon
            .find(text)
            .into_iter()
            .map(|hit| text[hit.range].to_owned())
            .collect()
    }

    #[test]
    fn makers_models_and_clis_are_found_whole_and_longest_first() {
        let lexicon = Lexicon::built_in();
        assert_eq!(
            hidden(
                &lexicon,
                "I ran it with Claude Code's Bash tool, as GPT-5.6 would."
            ),
            ["Claude Code", "GPT-5.6"]
        );
        assert_eq!(
            hidden(
                &lexicon,
                "Opus 4.7 and gemini3 agree; OpenAI and Google DeepMind too."
            ),
            ["Opus 4.7", "gemini3", "OpenAI", "Google DeepMind"]
        );
    }

    #[test]
    fn identifiers_and_ordinary_words_are_no_names() {
        let lexicon = Lexicon::built_in();
        assert!(
            hidden(
                &lexicon,
                "codex_adapter reads the metadata; meta tags stay."
            )
            .is_empty()
        );
        assert_eq!(hidden(&lexicon, "Meta's model"), ["Meta"]);
    }

    #[test]
    fn a_topic_word_stays_as_written() {
        let lexicon = Lexicon::built_in().with_topic("Fix the Codex adapter's retries");
        assert!(hidden(&lexicon, "The codex adapter retries twice.").is_empty());
        assert_eq!(hidden(&lexicon, "Claude would retry once."), ["Claude"]);
    }

    #[test]
    fn each_model_set_up_adds_its_names_and_maker() {
        let models: Vec<ModelRef> = vec![
            "codex/gpt-5.6-luna".parse().unwrap(),
            "opencode/ollama/qwen3-coder:30b".parse().unwrap(),
        ];
        let provider = |model: &ModelRef| match model.agent.as_str() {
            "codex" => "openai".to_owned(),
            _ => "alibaba".to_owned(),
        };
        let lexicon = Lexicon::built_in().with_models(&models, &DisplayNames::default(), &provider);
        assert_eq!(
            hidden(&lexicon, "gpt-5.6-luna and qwen3-coder:30b both pass"),
            ["gpt-5.6-luna", "qwen3-coder:30b"]
        );
    }

    #[test]
    fn a_clis_marks_give_its_tools_plain_words() {
        let marks = IdentityMarks {
            tool_names: &["Bash", "TodoWrite", "Frobnicate"],
            row_marks: &["⏺", "⎿"],
            home_folders: &["~/.claude"],
            citation: None,
        };
        let lexicon = Lexicon::built_in().with_marks(marks);
        assert_eq!(lexicon.plain_tool("Bash"), Some("shell"));
        assert_eq!(lexicon.plain_tool("Frobnicate"), Some("a tool"));
        assert_eq!(lexicon.plain_tool("Grep"), None, "not this CLI's tool");
        let distinct: Vec<&str> = lexicon.distinct_tools().collect();
        assert_eq!(distinct, ["TodoWrite"]);
    }
}
