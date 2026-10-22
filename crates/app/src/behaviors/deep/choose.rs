//! Who may summarize, best first: the models served on this machine, which cost nothing and
//! send nothing anywhere, the largest that fits now first; then each CLI's cheapest model,
//! the main CLI's first, which would read the other CLIs' actions too and so asks first.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::model_catalog::display_name_from_id;
use argonauts_core::model_picker::{Fit, MachineMemory};
use argonauts_core::names::ModelRef;

/// The most summarizers the question lists.
pub(crate) const LISTED: usize = 4;

/// A model a server on this machine serves.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LocalModel {
    /// The runtime's id, such as `ollama`.
    pub runtime: String,
    pub base_url: String,
    /// As the server names it, such as `qwen3-coder:30b-64k`.
    pub model: String,
    /// Its weights, where the server says.
    pub bytes: Option<u64>,
    pub fit: Option<Fit>,
}

/// A CLI's cheapest model, run in a session with no tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostedModel {
    pub worker: ModelRef,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Place {
    ThisMachine(LocalModel),
    Hosted(HostedModel),
}

/// One summarizer the question offers.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Candidate {
    pub model: ModelRef,
    /// Such as `Qwen3 Coder 30B on Ollama`.
    pub name: String,
    /// Such as `Haiku 5.5`.
    pub short: String,
    pub place: Place,
}

impl Candidate {
    pub fn local(&self) -> bool {
        matches!(self.place, Place::ThisMachine(_))
    }
}

/// Every summarizer to offer, the one auto picks first: local models that fit now, largest
/// first, then the other local ones, smallest first, then the hosted ones in `hosted`'s order
/// (the main CLI's first). Embedding models can't write a summary and are left out, and of a
/// model served twice under one name, such as with a longer window, the one in memory now is
/// offered, else the first.
pub(crate) fn candidates(
    locals: &[LocalModel],
    hosted: &[HostedModel],
    names: &DisplayNames,
) -> Vec<Candidate> {
    let mut fitting: Vec<&LocalModel> = Vec::new();
    let mut tight: Vec<&LocalModel> = Vec::new();
    for local in locals.iter().filter(|local| !embeds(&local.model)) {
        match local.fit {
            None | Some(Fit::Fits | Fit::Loaded) => fitting.push(local),
            Some(_) => tight.push(local),
        }
    }
    fitting.sort_by_key(|local| std::cmp::Reverse(local.bytes));
    tight.sort_by_key(|local| local.bytes);
    let mut named: Vec<(String, &LocalModel)> = Vec::new();
    for local in fitting.into_iter().chain(tight) {
        let name = local_name(&local.model);
        let same = |(known, kept): &&mut (String, &LocalModel)| {
            *known == name && kept.runtime == local.runtime
        };
        match named.iter_mut().find(same) {
            Some(kept) if local.fit == Some(Fit::Loaded) && kept.1.fit != Some(Fit::Loaded) => {
                kept.1 = local;
            }
            Some(_) => {}
            None => named.push((name, local)),
        }
    }
    let locals = named
        .into_iter()
        .map(|(_, local)| local_candidate(local, names));
    let hosted = hosted.iter().map(|hosted| Candidate {
        model: hosted.worker.clone(),
        name: names.worker(&hosted.worker),
        short: names
            .model(&hosted.worker.agent, hosted.worker.model.as_deref())
            .unwrap_or_else(|| names.cli(&hosted.worker.agent)),
        place: Place::Hosted(hosted.clone()),
    });
    locals.chain(hosted).collect()
}

/// The summarizer `setting` names, among the candidates or not. `None` for `auto`.
pub(crate) fn named(setting: &str, candidates: &[Candidate]) -> Option<ModelRef> {
    let setting = setting.trim();
    if setting.is_empty() || setting == argonauts_core::settings::behaviors::AUTO {
        return None;
    }
    let wanted: ModelRef = setting.parse().ok()?;
    Some(
        candidates
            .iter()
            .find(|candidate| candidate.model == wanted)
            .map_or(wanted, |candidate| candidate.model.clone()),
    )
}

/// A model a server on this machine serves, as the questions offer it.
pub(crate) fn local_candidate(local: &LocalModel, names: &DisplayNames) -> Candidate {
    let short = local_name(&local.model);
    Candidate {
        model: ModelRef::new(local.runtime.clone(), Some(local.model.clone())),
        name: format!("{short} on {}", names.cli(&local.runtime)),
        short,
        place: Place::ThisMachine(local.clone()),
    }
}

/// A local model as people say it: `qwen3-coder:30b-64k` reads "Qwen3 Coder 30B", the window
/// a local tag was made with left out.
pub(crate) fn local_name(model: &str) -> String {
    let without_window = match model.rsplit_once('-') {
        Some((base, window))
            if base.contains(':')
                && window
                    .strip_suffix(['k', 'K'])
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) =>
        {
            base
        }
        _ => model,
    };
    display_name_from_id(without_window)
}

/// An embedding model, which writes no text.
pub(crate) fn embeds(model: &str) -> bool {
    model.to_ascii_lowercase().contains("embed")
}

/// Where a local model of `bytes` stands on `machine`, when both are known.
pub(crate) fn fit(
    machine: Option<&MachineMemory>,
    bytes: Option<u64>,
    loaded: bool,
) -> Option<Fit> {
    if loaded {
        return Some(Fit::Loaded);
    }
    Some(machine?.fit(bytes?))
}

#[cfg(test)]
pub(crate) mod sample {
    use super::*;

    const GB: u64 = 1_000_000_000;

    /// The page's machine: Qwen3 Coder 30B and Gemma 3 12B on Ollama, and Claude Code and
    /// Codex with their cheapest models.
    pub fn page_models() -> (Vec<LocalModel>, Vec<HostedModel>) {
        let local = |model: &str, gb: u64| LocalModel {
            runtime: "ollama".to_owned(),
            base_url: "http://127.0.0.1:11434/v1".to_owned(),
            model: model.to_owned(),
            bytes: Some(gb * GB),
            fit: Some(Fit::Fits),
        };
        let locals = vec![
            local("gemma3:12b-32k", 8),
            local("nomic-embed-text:latest", 0),
            local("qwen3-coder:30b-64k", 18),
        ];
        let hosted = ["claude-code/haiku", "codex/gpt-6-luna"]
            .iter()
            .map(|worker| HostedModel {
                worker: worker.parse().unwrap(),
            })
            .collect();
        (locals, hosted)
    }
}

#[cfg(test)]
mod tests {
    use super::sample::page_models;
    use super::*;

    #[test]
    fn auto_picks_the_largest_local_model_that_fits_and_hosted_ones_come_after() {
        let (locals, hosted) = page_models();
        let names = DisplayNames::default();
        let offered = candidates(&locals, &hosted, &names);
        let listed: Vec<&str> = offered.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            listed,
            [
                "Qwen3 Coder 30B on Ollama",
                "Gemma3 12B on Ollama",
                "Haiku on Claude Code",
                "GPT 6 Luna on Codex",
            ]
        );
        let picked = offered.first().unwrap();
        assert!(picked.local());
        assert_eq!(picked.model.to_string(), "ollama/qwen3-coder:30b-64k");
    }

    #[test]
    fn with_no_local_model_auto_is_the_main_clis_cheapest() {
        let (_, hosted) = page_models();
        let offered = candidates(&[], &hosted, &DisplayNames::default());
        let picked = offered.first().unwrap();
        assert!(!picked.local());
        assert_eq!(picked.model.to_string(), "claude-code/haiku");
    }

    #[test]
    fn a_model_that_doesnt_fit_now_comes_after_the_ones_that_do() {
        let (mut locals, hosted) = page_models();
        locals[2].fit = Some(Fit::Tight { at: None });
        let offered = candidates(&locals, &hosted, &DisplayNames::default());
        assert_eq!(offered[0].model.to_string(), "ollama/gemma3:12b-32k");
        assert_eq!(offered[1].model.to_string(), "ollama/qwen3-coder:30b-64k");
    }

    #[test]
    fn a_named_summarizer_wins_over_auto() {
        let (locals, hosted) = page_models();
        let offered = candidates(&locals, &hosted, &DisplayNames::default());
        assert_eq!(named("auto", &offered), None);
        assert_eq!(named(" ", &offered), None);
        assert_eq!(
            named("codex/gpt-6-luna", &offered).map(|m| m.to_string()),
            Some("codex/gpt-6-luna".to_owned())
        );
    }

    #[test]
    fn a_model_served_twice_under_one_name_is_offered_once_the_one_in_memory() {
        let (mut locals, hosted) = page_models();
        let mut base = locals[0].clone();
        base.model = "qwen3-coder:30b".to_owned();
        locals.push(base.clone());
        let offered = candidates(&locals, &hosted, &DisplayNames::default());
        let qwen: Vec<String> = offered
            .iter()
            .filter(|c| c.short == "Qwen3 Coder 30B")
            .map(|c| c.model.to_string())
            .collect();
        assert_eq!(qwen, ["ollama/qwen3-coder:30b-64k"]);
        base.fit = Some(Fit::Loaded);
        locals.push(base);
        let offered = candidates(&locals, &hosted, &DisplayNames::default());
        let qwen: Vec<String> = offered
            .iter()
            .filter(|c| c.short == "Qwen3 Coder 30B")
            .map(|c| c.model.to_string())
            .collect();
        assert_eq!(qwen, ["ollama/qwen3-coder:30b"]);
    }

    #[test]
    fn local_names_leave_out_the_window() {
        assert_eq!(local_name("qwen3-coder:30b-64k"), "Qwen3 Coder 30B");
        assert_eq!(local_name("qwen3-coder:30b"), "Qwen3 Coder 30B");
        assert!(embeds("nomic-embed-text:latest"));
    }
}
