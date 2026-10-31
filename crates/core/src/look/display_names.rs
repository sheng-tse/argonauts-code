//! What people read for each CLI and model: "Claude Code" and "Opus 5.5" rather than the ids
//! `claude-code` and `claude-opus-5-5`. Ids stay in settings files, logs, and the detail views.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::base::names::ModelRef;
use crate::look::logos::{Logo, LogoMark};
use crate::models::model_catalog::{ModelCatalog, display_name_from_id, shown_name};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayNames {
    /// A CLI's or endpoint's id to its name, as its adapter gives it.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub clis: BTreeMap<String, String>,
    /// Model names, including what an alias such as `opus` runs.
    pub catalog: ModelCatalog,
    /// A CLI's id to the model its settings give it (`agents.<cli>.model`), which a session that
    /// names no model of its own runs.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub configured: BTreeMap<String, String>,
    /// A CLI's id to the model it runs when nothing names one: the one its model list marks,
    /// else the one the catalog's "Default (recommended)" runs. A session is named by it from its
    /// start, as the header is, until the session says which model it runs.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub defaults: BTreeMap<String, String>,
}

impl DisplayNames {
    /// `Claude Code` for `claude-code`. An id nothing names stays as it is.
    pub fn cli(&self, id: &str) -> String {
        let id = id.trim();
        self.clis
            .get(id)
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
            .or_else(|| builtin_cli_name(id))
            .unwrap_or(id)
            .to_owned()
    }

    /// The id of the CLI or endpoint [`DisplayNames::cli`] names `name`, such as `claude-code`
    /// for `Claude Code`.
    pub fn cli_id(&self, name: &str) -> Option<String> {
        let name = name.trim();
        let named = |(_, shown): &(&String, &String)| shown.trim() == name;
        if let Some((id, _)) = self.clis.iter().find(named) {
            return Some(id.clone());
        }
        BUILTIN_CLIS
            .iter()
            .find(|(_, shown)| *shown == name)
            .map(|(id, _)| (*id).to_owned())
            .or_else(|| {
                crate::agents::runtimes::RUNTIMES
                    .iter()
                    .find(|runtime| runtime.name == name)
                    .map(|runtime| runtime.id.to_owned())
            })
    }

    /// `Opus 5.5` for `claude-code` with `claude-opus-5-5` or its alias `opus`, or `None` when
    /// no model is named and the CLI runs its own default.
    pub fn model(&self, cli: &str, model: Option<&str>) -> Option<String> {
        let model = model.map(str::trim).filter(|model| !model.is_empty())?;
        let id = ModelRef::new(cli.trim(), Some(model.to_owned()));
        // An alias reads as the model it runs, so `opus` names the model rather than itself.
        let entry = self.catalog.entry(&id);
        let named = entry
            .and_then(|entry| entry.name.as_deref())
            .map(|name| shown_name(cli, name));
        let resolved = entry.and_then(|entry| entry.id.model.as_deref());
        Some(named.unwrap_or_else(|| display_name_from_id(resolved.unwrap_or(model))))
    }

    /// `Opus 5.5 on Claude Code`, or the CLI alone when no model is named. The name reads as
    /// one phrase, so it sits in sentences and in lines of `·`-joined parts alike.
    pub fn agent(&self, id: &str, model: Option<&str>) -> String {
        let cli = self.cli(id);
        match self.model(id, model) {
            Some(model) => format!("{model} on {cli}"),
            None => cli,
        }
    }

    /// `GPT 5.6 on Codex` for a session of `worker`, named by the model it runs as
    /// [`Self::running`] gives it, so a session that names none reads the same from its start.
    pub fn worker(&self, worker: &ModelRef) -> String {
        let worker = self.running(worker);
        self.agent(&worker.agent, worker.model.as_deref())
    }

    /// The model alone where it runs on its own provider's CLI (Claude models on Claude Code,
    /// GPT on Codex, Gemini on agy), else `<model> on <cli>`. The workflow view's
    /// rows name agents this way, since the pane is narrow and the agent view gives the rest.
    /// A row that draws the CLI's logo names the model as [`Self::after_logo`] does instead.
    pub fn short(&self, worker: &ModelRef) -> String {
        let worker = self.running(worker);
        let cli = self.cli(&worker.agent);
        let Some(model) = self.model(&worker.agent, worker.model.as_deref()) else {
            return cli;
        };
        let at_home = home_cli(&model)
            .and_then(builtin_cli_name)
            .is_some_and(|home| home == cli);
        if at_home {
            model
        } else {
            format!("{model} on {cli}")
        }
    }

    /// What follows `logo` on a row that names a session of `worker`: the model it runs alone,
    /// since the logo says where it runs, as [`after_logo`] words it; the CLI's name when
    /// nothing says which model that is.
    pub fn after_logo(&self, logo: &Logo, worker: &ModelRef) -> String {
        let worker = self.running(worker);
        match self.model(&worker.agent, worker.model.as_deref()) {
            Some(model) => after_logo(logo, &model),
            None => self.cli(&worker.agent),
        }
    }

    /// The agent with the model a session of it runs now: its own, else the one its CLI's
    /// settings give, else its CLI's default, so a session reads `GPT 5.6 on Codex` from its
    /// start to its end.
    pub fn running(&self, worker: &ModelRef) -> ModelRef {
        if worker.model.is_some() {
            return worker.clone();
        }
        let cli = worker.agent.trim();
        let model = model_for(&self.configured, cli).or_else(|| model_for(&self.defaults, cli));
        match model {
            Some(model) => ModelRef::new(worker.agent.clone(), Some(model.to_owned())),
            None => worker.clone(),
        }
    }

    /// The agent with the model a past record of it ran: its own, else its CLI's default.
    /// Your setting for the CLI doesn't count here: a session Argonauts Code ran with it
    /// recorded that model, and a CLI's own history never saw it. A screen that sums records
    /// by model names them this way, through [`Spellings`], so a record of Codex's default
    /// reads as that model rather than as Codex.
    pub fn recorded(&self, agent: &str, model: Option<&str>) -> ModelRef {
        let own = model.map(str::trim).filter(|model| !model.is_empty());
        let model = own.or_else(|| model_for(&self.defaults, agent.trim()));
        ModelRef::new(agent, model.map(str::to_owned))
    }

    /// Whether [`Self::running`] names `worker` by its CLI's default, a guess that the model
    /// the session reports replaces.
    pub fn by_default(&self, worker: &ModelRef) -> bool {
        let cli = worker.agent.trim();
        worker.model.is_none()
            && model_for(&self.configured, cli).is_none()
            && model_for(&self.defaults, cli).is_some()
    }

    /// A session's model as its CLI and the name it shows under: `haiku` and
    /// `claude-haiku-5-5` are one model, and a session with no model is the one
    /// [`Self::running`] gives. The name is empty when nothing says which model that is.
    pub fn shown_as(&self, agent: &str, model: Option<&str>) -> (String, String) {
        let running = self.running(&ModelRef::new(agent, model.map(str::to_owned)));
        let name = self.model(agent, running.model.as_deref());
        (agent.to_owned(), name.unwrap_or_default())
    }
}

/// One spelling for each model however the records spell it, so a screen that sums by model
/// has one row for it: models are one when they show under one name, and go by the catalog's
/// spelling, else by the first spelling met. A record with no model is its CLI's default, as
/// [`DisplayNames::recorded`] names it, and keeps none where nothing says which that is.
pub struct Spellings<'a> {
    names: &'a DisplayNames,
    /// Each spelling met, to the model it shows as, so a spelling is looked up once however
    /// many records carry it.
    met: BTreeMap<(String, Option<String>), (String, String)>,
    /// Each model's spelling, and whether it's the catalog's.
    chosen: BTreeMap<(String, String), (Option<String>, bool)>,
}

impl<'a> Spellings<'a> {
    pub fn new(names: &'a DisplayNames) -> Self {
        Self {
            names,
            met: BTreeMap::new(),
            chosen: BTreeMap::new(),
        }
    }

    /// Takes in a spelling before any is asked for, so the one chosen doesn't hang on which
    /// record came first.
    pub fn meet(&mut self, agent: &str, model: Option<&str>) {
        self.shown(agent, model);
    }

    /// The spelling `model` of `agent` is summed under.
    pub fn of(&mut self, agent: &str, model: Option<&str>) -> Option<String> {
        let shown = self.shown(agent, model);
        self.chosen.get(&shown)?.0.clone()
    }

    /// The model `model` of `agent` shows as, met for the first time if it's new.
    fn shown(&mut self, agent: &str, model: Option<&str>) -> (String, String) {
        let key = (agent.to_owned(), model.map(str::to_owned));
        if let Some(shown) = self.met.get(&key) {
            return shown.clone();
        }
        let recorded = self.names.recorded(agent, model);
        let name = self.names.model(agent, recorded.model.as_deref());
        let shown = (agent.to_owned(), name.unwrap_or_default());
        self.met.insert(key, shown.clone());
        if shown.1.is_empty() {
            return shown;
        }
        let spelled = match self.names.catalog.runs(&recorded) {
            Some(known) => (Some(known), true),
            None => (recorded.model, false),
        };
        let chosen = self
            .chosen
            .entry(shown.clone())
            .or_insert_with(|| spelled.clone());
        if spelled.1 && !chosen.1 {
            *chosen = spelled;
        }
        shown
    }
}

fn model_for<'a>(models: &'a BTreeMap<String, String>, cli: &str) -> Option<&'a str> {
    models
        .get(cli)
        .map(|model| model.trim())
        .filter(|model| !model.is_empty())
}

/// The CLI made by the provider that makes `model`, by the model's name.
fn home_cli(model: &str) -> Option<&'static str> {
    maker_of(model)?.cli
}

/// A maker of models, known by how its models' names and ids start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Maker {
    /// As the registry names providers: `anthropic`, `openai`, `google`.
    pub id: &'static str,
    /// The word its models' names start with where they name it, as "GPT" in "GPT 5.6 Luna".
    pub word: Option<&'static str>,
    /// The CLI it makes, where its models are at home.
    pub cli: Option<&'static str>,
    /// Its own logo, where the app ships one.
    pub logo: Option<LogoMark>,
    /// The words its model lines start with, lower case.
    lines: &'static [&'static str],
}

/// The makers a name or a logo says something about. A model the table doesn't know has no
/// maker here, so it keeps its whole name and shows its server's logo or colour dot.
const MAKERS: &[Maker] = &[
    Maker {
        id: "anthropic",
        word: Some("Claude"),
        cli: Some("claude-code"),
        logo: None,
        lines: &["claude", "opus", "sonnet", "haiku", "fable", "mythos"],
    },
    Maker {
        id: "openai",
        word: Some("GPT"),
        cli: Some("codex"),
        logo: Some(LogoMark::OpenAi),
        lines: &["gpt", "o3", "o4", "codex"],
    },
    Maker {
        id: "google",
        word: Some("Gemini"),
        cli: Some("antigravity-cli"),
        logo: None,
        lines: &["gemini", "gemma", "codegemma"],
    },
    Maker {
        id: "alibaba",
        word: None,
        cli: None,
        logo: Some(LogoMark::Qwen),
        lines: &["qwen", "qwq", "codeqwen"],
    },
    Maker {
        id: "moonshot",
        word: None,
        cli: None,
        logo: Some(LogoMark::Kimi),
        lines: &["kimi"],
    },
    Maker {
        id: "mistral",
        word: None,
        cli: None,
        logo: Some(LogoMark::Mistral),
        lines: &[
            "mistral",
            "mixtral",
            "codestral",
            "devstral",
            "ministral",
            "magistral",
            "pixtral",
        ],
    },
    Maker {
        id: "meta",
        word: None,
        cli: None,
        logo: Some(LogoMark::Meta),
        lines: &["llama", "codellama"],
    },
];

/// A maker by its own id, as the registry names providers: `openai`, `mistral`.
pub fn maker(id: &str) -> Option<&'static Maker> {
    MAKERS.iter().find(|maker| maker.id == id)
}

/// The maker of a model by its name or its id: "Mythos 5.1" and `claude-mythos-5-1` are
/// Anthropic's, `ollama/qwen3-coder:30b` is Alibaba's. A line matches only up to a digit or a
/// separator, so `o3-mini` is OpenAI's and `olmo` nobody's here.
pub fn maker_of(model: &str) -> Option<&'static Maker> {
    let model = model.trim().to_ascii_lowercase();
    let name = model.rsplit('/').next().unwrap_or(&model);
    MAKERS.iter().find(|maker| {
        maker.lines.iter().any(|line| {
            name.strip_prefix(line)
                .is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_alphabetic()))
        })
    })
}

/// A model's name as drawn right after `logo`, on its row or under a heading that shows it.
/// After the logo of the maker's own CLI the name drops the maker's word: "GPT 5.6 Luna" reads
/// "5.6 Luna" after Codex's, "Gemini 3.8 Flash" "3.8 Flash" after agy's, and a bare "GPT 5.6"
/// reads "5.6". A host runs other makers' models, so a model it serves keeps its whole name
/// whichever logo its row shows, and so does a name with nothing after the maker's word.
///
/// Only for a logo whose picture is drawn: a colour dot, a sentence, a log or a screen
/// reader keeps the whole name, as do colours, totals and search.
pub fn after_logo(logo: &Logo, name: &str) -> String {
    let name = name.trim();
    let word = logo
        .mark
        .and_then(maker_of_cli_logo)
        .and_then(|maker| maker.word);
    let rest = word.and_then(|word| {
        let (first, rest) = name.split_once(|c: char| c.is_whitespace() || c == '-')?;
        first.eq_ignore_ascii_case(word).then(|| rest.trim_start())
    });
    match rest {
        Some(rest) if !rest.is_empty() => rest.to_owned(),
        _ => name.to_owned(),
    }
}

/// Two capitals for a CLI or endpoint where its name would vanish for lack of room, as in `● CC`
/// beside the colour dot: never nothing. The ones Argonauts Code knows by `id` have their own
/// pair; any other takes its `name`'s initials, or its id's first two letters.
pub fn letters(id: &str, name: &str) -> String {
    let known = match id.trim() {
        "claude-code" | "claude" => Some("CC"),
        "codex" => Some("CX"),
        "antigravity-cli" => Some("AG"),
        "opencode" => Some("OC"),
        "openrouter" => Some("OR"),
        "ollama" => Some("OL"),
        _ => None,
    };
    if let Some(known) = known {
        return known.to_owned();
    }
    let words: Vec<&str> = name.split_whitespace().collect();
    let pair: String = match words.as_slice() {
        [first, second, ..] => first
            .chars()
            .take(1)
            .chain(second.chars().take(1))
            .collect(),
        [only] => only.chars().take(2).collect(),
        [] => id.trim().chars().take(2).collect(),
    };
    pair.to_uppercase()
}

/// The maker whose own CLI `mark` is the logo of: Claude Code's, Codex's or agy's.
fn maker_of_cli_logo(mark: LogoMark) -> Option<&'static Maker> {
    MAKERS
        .iter()
        .find(|maker| maker.cli.and_then(LogoMark::of_agent) == Some(mark))
}

/// `name` in at most `cells` cells, cut at a word when it's longer: the whole words that fit,
/// then `…`, so "Nemotron 3.5 Super VL Free" in 14 cells reads "Nemotron 3.5…". A word ends
/// at a space or a `-`, as in OpenCode's "GLM-5.3-Flash Free". A first word longer than the
/// room is cut inside, since a name needs at least its start to be told apart.
///
/// `…` counts one cell. Fonts that draw it wider spill into the blank a column keeps after
/// each name, so a column passes its width less that blank.
pub fn cut_at_word(name: &str, cells: usize) -> String {
    const MARK: &str = "…";
    let name = name.trim();
    if name.width() <= cells {
        return name.to_owned();
    }
    let Some(room) = cells.checked_sub(MARK.width()) else {
        return String::new();
    };
    let at_word = name
        .match_indices([' ', '-'])
        .map(|(at, _)| name[..at].trim_end_matches([' ', '-']))
        .rfind(|kept| !kept.is_empty() && kept.width() <= room);
    let kept = at_word.map(str::to_owned).unwrap_or_else(|| {
        let mut used = 0;
        name.chars()
            .take_while(|c| {
                used += c.width().unwrap_or(0);
                used <= room
            })
            .collect()
    });
    format!("{kept}{MARK}")
}

/// The CLIs and endpoints Argonauts Code knows without asking an adapter, by id. The first id
/// with a name is the one the name stands for.
const BUILTIN_CLIS: &[(&str, &str)] = &[
    ("claude-code", "Claude Code"),
    ("claude", "Claude Code"),
    ("codex", "Codex"),
    ("antigravity-cli", "agy"),
    ("opencode", "OpenCode"),
    ("openrouter", "OpenRouter"),
];

/// The names of the CLIs and endpoints Argonauts Code knows without asking an adapter.
pub fn builtin_cli_name(id: &str) -> Option<&'static str> {
    BUILTIN_CLIS
        .iter()
        .find(|(known, _)| *known == id)
        .map(|(_, name)| *name)
        .or_else(|| crate::agents::runtimes::runtime(id).map(|runtime| runtime.name))
}

#[cfg(test)]
mod tests {
    use crate::look::logos::logo_for;
    use crate::models::model_catalog::CatalogEntry;

    use super::*;

    fn id(text: &str) -> ModelRef {
        text.parse().unwrap()
    }

    /// A CLI's list that joins a name's words reads the way the rest of Argonauts Code writes it.
    #[test]
    fn a_model_is_spelled_one_way_whoever_named_it() {
        let mut luna = CatalogEntry::new(id("codex/gpt-5.6-luna"));
        luna.name = Some("GPT-5.6-Luna".to_owned());
        let names = DisplayNames {
            catalog: ModelCatalog {
                models: vec![luna],
                ..ModelCatalog::default()
            },
            ..DisplayNames::default()
        };
        let from_the_cli = names.worker(&id("codex/gpt-5.6-luna"));
        let from_its_id = DisplayNames::default().worker(&id("codex/gpt-5.6-luna"));
        assert_eq!(from_the_cli, "GPT 5.6 Luna on Codex");
        assert_eq!(from_its_id, from_the_cli);
        let label = names.catalog.label(&id("codex/gpt-5.6-luna"));
        assert_eq!(label.name, "GPT 5.6 Luna");
    }

    fn names() -> DisplayNames {
        let mut opus = CatalogEntry::new(id("claude-code/claude-opus-5-5"));
        opus.aliases = Some(vec!["opus".to_owned()]);
        let mut sol = CatalogEntry::new(id("codex/gpt-6.1-sol"));
        sol.name = Some("GPT-6.1 Sol".to_owned());
        DisplayNames {
            clis: BTreeMap::from([("my-cli".to_owned(), "My CLI".to_owned())]),
            catalog: ModelCatalog {
                models: vec![opus, sol],
                ..ModelCatalog::default()
            },
            configured: BTreeMap::from([("codex".to_owned(), "gpt-6.1-sol".to_owned())]),
            defaults: BTreeMap::new(),
        }
    }

    /// The alias, the catalog's id and a dated id are one model under the catalog's spelling,
    /// whichever comes first, and a CLI named alone whose default nothing says keeps no model.
    #[test]
    fn one_model_spelled_three_ways_has_one_spelling() {
        let names = names();
        let mut spellings = Spellings::new(&names);
        let claude = "claude-code";
        let ways = ["claude-opus-5-5-20261001", "opus", "claude-opus-5-5"];
        for way in ways {
            spellings.meet(claude, Some(way));
        }
        for way in ways {
            assert_eq!(
                spellings.of(claude, Some(way)).as_deref(),
                Some("claude-opus-5-5"),
                "{way}"
            );
        }
        // Met before any spelling the catalog knows, a dated id goes by itself until one comes.
        let mut alone = Spellings::new(&names);
        let dated = "claude-opus-5-5-20261001";
        assert_eq!(alone.of(claude, Some(dated)).as_deref(), Some(dated));
        assert_eq!(names.shown_as(claude, Some(dated)).1, "Opus 5.5");
        assert_eq!(
            spellings.of("codex", Some("gpt-6.1-sol")).as_deref(),
            Some("gpt-6.1-sol")
        );
        assert_eq!(spellings.of(claude, None), None);
        assert_eq!(
            spellings.of("codex", Some("gpt-5.6")).as_deref(),
            Some("gpt-5.6")
        );
    }

    #[test]
    fn a_cli_s_name_turns_back_into_its_id() {
        let mut names = DisplayNames::default();
        assert_eq!(names.cli_id("Claude Code").as_deref(), Some("claude-code"));
        assert_eq!(names.cli_id("agy").as_deref(), Some("antigravity-cli"));
        assert_eq!(names.cli_id("Claude"), None);
        names
            .clis
            .insert("my-cli".to_owned(), "Team Box".to_owned());
        assert_eq!(names.cli_id("Team Box").as_deref(), Some("my-cli"));
        for id in [
            "claude-code",
            "codex",
            "antigravity-cli",
            "opencode",
            "my-cli",
        ] {
            assert_eq!(names.cli_id(&names.cli(id)).as_deref(), Some(id));
        }
    }

    #[test]
    fn a_name_that_would_vanish_leaves_two_letters() {
        assert_eq!(letters("claude-code", "Claude Code"), "CC");
        assert_eq!(letters("codex", "Codex"), "CX");
        assert_eq!(letters("antigravity-cli", "agy"), "AG");
        assert_eq!(letters("opencode", "OpenCode"), "OC");
        assert_eq!(letters("my-endpoint", "Team Box"), "TB");
        assert_eq!(letters("vllm", "vllm"), "VL");
        assert_eq!(letters("lm", ""), "LM");
    }

    #[test]
    fn a_session_runs_the_model_its_settings_give() {
        let names = names();
        // Codex names no model, so it runs the one its settings give, and is named by it.
        assert_eq!(names.worker(&id("codex")), "GPT 6.1 Sol on Codex");
        // A model of its own wins, and a CLI without one in its settings runs its default.
        assert_eq!(names.running(&id("codex/gpt-5.6")), id("codex/gpt-5.6"));
        assert_eq!(names.running(&id("claude-code")), id("claude-code"));
        assert!(!names.by_default(&id("codex")));
    }

    /// A record that names no model ran its CLI's default: your setting is for the sessions
    /// Argonauts Code starts, which record the model they run, and a CLI's own history never
    /// saw it. Summed by model, such records join that model's row rather than the CLI's.
    #[test]
    fn a_record_with_no_model_ran_its_clis_default() {
        let mut names = names();
        names.defaults = BTreeMap::from([("codex".to_owned(), "gpt-5.6".to_owned())]);
        assert_eq!(names.recorded("codex", None), id("codex/gpt-5.6"));
        assert_eq!(names.recorded("codex", Some(" ")), id("codex/gpt-5.6"));
        assert_eq!(
            names.recorded("codex", Some("gpt-6.1-sol")),
            id("codex/gpt-6.1-sol")
        );
        assert_eq!(names.recorded("claude-code", None), id("claude-code"));
        // A session started now runs the model its settings give instead.
        assert_eq!(names.running(&id("codex")), id("codex/gpt-6.1-sol"));

        let mut spellings = Spellings::new(&names);
        for model in [None, Some("gpt-5.6")] {
            spellings.meet("codex", model);
        }
        assert_eq!(spellings.of("codex", None).as_deref(), Some("gpt-5.6"));
        assert_eq!(
            spellings.of("codex", Some("gpt-5.6")).as_deref(),
            Some("gpt-5.6")
        );
        assert_eq!(spellings.of("claude-code", None), None);
        // With no default known the record keeps no model, and reads as its CLI alone.
        let unknown = DisplayNames::default();
        assert_eq!(Spellings::new(&unknown).of("codex", None), None);
    }

    /// The workflow view's rows name a model alone on its own provider's CLI.
    #[test]
    fn a_model_on_its_own_providers_cli_goes_by_its_name_alone() {
        let names = names();
        assert_eq!(names.short(&id("claude-code/claude-opus-5-5")), "Opus 5.5");
        assert_eq!(names.short(&id("codex/gpt-6.1-sol")), "GPT 6.1 Sol");
        assert_eq!(
            names.short(&id("antigravity-cli/claude-opus-5-5")),
            "Opus 5.5 on agy"
        );
        assert_eq!(
            names.short(&id("my-cli/gpt-6.1-sol")),
            "GPT 6.1 Sol on My CLI"
        );
        // A session that names no model runs the one its settings give; with none known it
        // reads as its CLI.
        assert_eq!(names.short(&id("codex")), "GPT 6.1 Sol");
        assert_eq!(names.short(&id("claude-code")), "Claude Code");
        assert_eq!(
            names.short(&id("claude-code/claude-mythos-5-1")),
            "Mythos 5.1"
        );
    }

    /// The rows of a workflow's Review pane, Codex's model list under its logo, and a legend
    /// of models from three CLIs, each name as drawn after its logo.
    #[test]
    fn a_name_after_its_makers_own_cli_logo_drops_the_makers_word() {
        let claude = logo_for("claude-code", None);
        let codex = logo_for("codex", None);
        let agy = logo_for("antigravity-cli", None);
        let local = logo_for("opencode", Some("ollama/qwen3-coder:30b"));
        let zen = logo_for("opencode", Some("opencode/nemotron-3.5-super-vl-free"));
        for (logo, name, drawn) in [
            (&codex, "GPT 5.6 Luna", "5.6 Luna"),
            (&claude, "Sonnet 5.5", "Sonnet 5.5"),
            (&local, "Qwen3 Coder 30B (64k)", "Qwen3 Coder 30B (64k)"),
            (
                &zen,
                "Nemotron 3.5 Super VL Free",
                "Nemotron 3.5 Super VL Free",
            ),
            (&codex, "Default (recommended)", "Default (recommended)"),
            (&codex, "GPT 6.1 Sol", "6.1 Sol"),
            (&codex, "GPT 6 Astra", "6 Astra"),
            (&codex, "GPT 5.6", "5.6"),
            (&claude, "Haiku 5.5", "Haiku 5.5"),
            (&codex, "GPT 6 Luna", "6 Luna"),
            (&agy, "Gemini 3.8 Flash", "3.8 Flash"),
            (&agy, "Gemini 3.8 Flash High", "3.8 Flash High"),
            (&claude, "Claude 3.5 Sonnet", "3.5 Sonnet"),
            (&codex, "GPT", "GPT"),
            // agy isn't the CLI of the makers of these, so they stay whole after its logo.
            (&agy, "Sonnet 4.6", "Sonnet 4.6"),
            (&agy, "GPT OSS 120B (Medium)", "GPT OSS 120B (Medium)"),
        ] {
            assert_eq!(after_logo(logo, name), drawn, "{name}");
        }
    }

    /// A host's row names a model whole, whether it shows the host's logo or the maker's, and
    /// a colour dot changes nothing.
    #[test]
    fn a_name_after_a_hosts_logo_or_a_dot_stays_whole() {
        let maker_on_a_host = Logo {
            agent: crate::base::names::AgentId("opencode".to_owned()),
            mark: Some(LogoMark::OpenAi),
        };
        let openrouter = logo_for("openrouter", Some("openai/gpt-5.6"));
        for logo in [
            &maker_on_a_host,
            &openrouter,
            &logo_for("opencode", None),
            &Logo::dot("codex"),
        ] {
            assert_eq!(after_logo(logo, "GPT 5.6"), "GPT 5.6", "{logo:?}");
        }
    }

    /// After a logo nothing says which CLI runs the model, since the logo already does.
    #[test]
    fn a_worker_after_its_logo_is_named_by_its_model_alone() {
        let names = names();
        let after = |worker: &str| {
            let worker = id(worker);
            let logo = logo_for(&worker.agent, worker.model.as_deref());
            names.after_logo(&logo, &worker)
        };
        assert_eq!(after("codex/gpt-6.1-sol"), "6.1 Sol");
        assert_eq!(after("antigravity-cli/claude-opus-5-5"), "Opus 5.5");
        assert_eq!(after("claude-code/opus"), "Opus 5.5");
        assert_eq!(after("my-cli/gpt-6.1-sol"), "GPT 6.1 Sol");
        assert_eq!(after("codex"), "6.1 Sol");
        assert_eq!(after("claude-code"), "Claude Code");
        assert_eq!(
            names.short(&id("antigravity-cli/claude-opus-5-5")),
            "Opus 5.5 on agy"
        );
    }

    #[test]
    fn a_long_name_is_cut_at_a_word() {
        for (name, cells, drawn) in [
            ("3.8 Flash High", 14, "3.8 Flash High"),
            ("Gemini 3.8 Flash High", 16, "Gemini 3.8…"),
            ("Nemotron 3.5 Super VL Free", 14, "Nemotron 3.5…"),
            ("GLM-5.3-Flash Free", 12, "GLM-5.3…"),
            ("Qwen3 Coder 30B (64k)", 20, "Qwen3 Coder 30B…"),
            ("Supercalifragilistic", 8, "Superca…"),
            ("Opus 5.5", 1, "…"),
            ("Opus 5.5", 0, ""),
        ] {
            assert_eq!(cut_at_word(name, cells), drawn, "{name} in {cells}");
            assert!(cut_at_word(name, cells).width() <= cells);
        }
    }

    /// One table says who makes a model, by its name or its id, for every rule that asks.
    #[test]
    fn a_model_is_known_by_its_makers_lines() {
        let maker = |model: &str| maker_of(model).map(|maker| maker.id);
        for (model, made_by) in [
            ("Mythos 5.1", Some("anthropic")),
            ("claude-mythos-5-1", Some("anthropic")),
            ("Opus 5.5", Some("anthropic")),
            ("GPT 5.6 Luna", Some("openai")),
            ("o3-mini", Some("openai")),
            ("Gemini 3.8 Flash", Some("google")),
            ("ollama/qwen3-coder:30b", Some("alibaba")),
            ("opencode/kimi-k2", Some("moonshot")),
            ("devstral-small-2", Some("mistral")),
            ("llama3.1:8b", Some("meta")),
            ("olmo2", None),
            ("phind-codellama", None),
            ("GLM-5.3-Flash Free", None),
            ("", None),
        ] {
            assert_eq!(maker(model), made_by, "{model}");
        }
    }

    /// A CLI that names no model runs its default, so its sessions read as that model from
    /// their start, as the header does. The settings' model still wins.
    #[test]
    fn a_session_with_no_model_runs_its_clis_default() {
        let mut names = names();
        names.defaults = BTreeMap::from([
            ("claude-code".to_owned(), "claude-opus-5-5".to_owned()),
            ("codex".to_owned(), "gpt-5.6".to_owned()),
        ]);
        assert_eq!(names.worker(&id("claude-code")), "Opus 5.5 on Claude Code");
        assert!(names.by_default(&id("claude-code")));
        assert_eq!(names.worker(&id("codex")), "GPT 6.1 Sol on Codex");
        assert!(!names.by_default(&id("codex")));
        assert!(!names.by_default(&id("claude-code/claude-haiku-4-5")));
        assert!(!names.by_default(&id("my-cli")));
    }

    #[test]
    fn an_agent_reads_as_its_cli_and_model() {
        let names = names();
        assert_eq!(
            names.agent("claude-code", Some("claude-opus-5-5")),
            "Opus 5.5 on Claude Code"
        );
        assert_eq!(
            names.worker(&id("codex/gpt-6.1-sol")),
            "GPT 6.1 Sol on Codex"
        );
        assert_eq!(
            names.worker(&id("my-cli/some-model")),
            "Some Model on My CLI"
        );
    }

    #[test]
    fn an_alias_reads_as_the_model_it_runs() {
        assert_eq!(
            names().worker(&id("claude-code/opus")),
            "Opus 5.5 on Claude Code"
        );
        assert_eq!(
            DisplayNames::default().worker(&id("claude-code/opus")),
            "Opus on Claude Code"
        );
    }

    #[test]
    fn a_cli_default_and_an_unknown_cli_leave_out_what_isnt_known() {
        let names = DisplayNames::default();
        assert_eq!(names.worker(&id("claude-code")), "Claude Code");
        assert_eq!(names.agent("codex", Some("  ")), "Codex");
        assert_eq!(names.agent(" gpu-box ", None), "gpu-box");
        // The local runtimes are known by the names they go by.
        assert_eq!(names.agent("vllm", None), "vLLM");
        assert_eq!(names.agent("llamacpp", None), "llama-server");
        assert_eq!(
            names.worker(&id("ollama/qwen3-coder:30b")),
            "Qwen3 Coder 30B on Ollama"
        );
    }
}
