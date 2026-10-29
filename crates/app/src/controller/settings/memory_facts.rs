//! OpenCode's level knowing this computer's memory: each local model with the memory it takes
//! at its window and whether that fits, up to three models from the catalog that aren't
//! installed and would fit, and the best of them all for this computer that fits with room for
//! a reviewer marked recommended and listed first, ranked as the catalog's rule says.

use argonauts_core::app::ModelChoice;
use argonauts_core::limits::CostEstimate;
use argonauts_core::model_picker::{Fit, ListedFacts, LocalModel, MachineMemory, ModelMemory};
use argonauts_core::names::ModelRef;
use argonauts_core::registry::{ModelAccess, ModelEntry};
use argonauts_registry::models::suggested::{Placed, better, download, matching, ranked, room};

use crate::host::memory::Ollama;

const OPENCODE: &str = "opencode";
const OLLAMA: &str = "ollama/";
/// Models not installed that are offered at most.
const SUGGESTIONS: usize = 3;
/// Smaller windows a model that doesn't fit is tried at, largest first.
const SMALLER: &[u64] = &[32_768, 16_384, 8_192];

/// What Ollama and the system said, read in the background.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Reading {
    pub machine: MachineMemory,
    pub ollama: Ollama,
}

/// OpenCode's choices with what each takes of `reading`'s computer, Default with the window
/// and host of the model it runs, the recommended model first among the local ones, and the
/// catalog's `local` models not installed that would fit after them.
pub(crate) fn annotate(
    choices: &mut Vec<ModelChoice>,
    reading: &Reading,
    listed: &[ModelEntry],
    local: &[LocalModel],
) {
    let machine = &reading.machine;
    for choice in choices.iter_mut() {
        let Some(tag) = ollama_tag(choice) else {
            continue;
        };
        let Some(facts) = choice.listed.as_mut() else {
            continue;
        };
        let Some(ram) = reading.ollama.ram(&tag, facts.window) else {
            continue;
        };
        facts.thinking |= reading.ollama.tags.get(&tag).is_some_and(|t| t.thinking);
        let fit = if reading.ollama.loaded.contains_key(&tag) {
            Fit::Loaded
        } else {
            with_smaller_window(machine.fit(ram), |window| {
                let smaller = reading.ollama.ram(&tag, Some(window))?;
                fits(machine, smaller).then_some(smaller)
            })
        };
        facts.memory = Some(ModelMemory {
            ram,
            fit,
            download: None,
        });
    }
    // A hosted model thinks where it takes a level of its own.
    for choice in choices.iter_mut() {
        let takes_levels = !choice.efforts.is_empty();
        if let Some(facts) = choice.listed.as_mut().filter(|f| f.hosted_by.is_some()) {
            facts.thinking |= takes_levels;
        }
    }
    default_facts(choices, listed);
    let mut suggested = suggestions(choices, reading, local);
    recommend(choices, &mut suggested, machine, local);
    order(choices, suggested);
}

/// A model that fits beside the loaded ones and in what is free now.
fn fits(machine: &MachineMemory, ram: u64) -> bool {
    matches!(machine.fit(ram), Fit::Fits)
}

/// A model that won't fit, or only tightly, with the largest smaller window it would fit at.
fn with_smaller_window(fit: Fit, at: impl Fn(u64) -> Option<u64>) -> Fit {
    let smaller = || {
        SMALLER
            .iter()
            .find_map(|&window| Some((window, at(window)?)))
    };
    match fit {
        Fit::TooBig { .. } => Fit::TooBig { at: smaller() },
        Fit::Tight { .. } => Fit::Tight { at: smaller() },
        other => other,
    }
}

/// The Ollama tag an OpenCode choice runs, such as `qwen3-coder:30b-64k`.
fn ollama_tag(choice: &ModelChoice) -> Option<String> {
    (choice.worker.agent == OPENCODE)
        .then(|| choice.worker.model.as_deref()?.strip_prefix(OLLAMA))
        .flatten()
        .map(str::to_owned)
}

/// OpenCode's Default says where the model it runs runs, and its window.
fn default_facts(choices: &mut [ModelChoice], listed: &[ModelEntry]) {
    let runs = listed
        .iter()
        .find(|entry| entry.id.agent == OPENCODE && entry.cli_default)
        .map(|entry| entry.id.clone());
    let Some(runs) = runs else {
        return;
    };
    let facts = choices
        .iter()
        .find(|choice| choice.worker == runs)
        .and_then(|choice| choice.listed.clone());
    if let Some(default) = choices
        .iter_mut()
        .find(|choice| choice.worker.agent == OPENCODE && choice.worker.model.is_none())
    {
        default.listed = facts.map(|facts| ListedFacts {
            group: None,
            memory: None,
            ..facts
        });
    }
}

/// The heading of the models not installed that would fit, the same words on every system.
const NOT_INSTALLED: &str = "Not installed · fits this computer";

/// Up to three of the catalog's models that aren't installed and would fit beside the loaded
/// models, though the other apps may need closing, best for this computer first, each at the
/// window it ranks at.
fn suggestions(
    choices: &[ModelChoice],
    reading: &Reading,
    local: &[LocalModel],
) -> Vec<ModelChoice> {
    if !choices.iter().any(|choice| choice.worker.agent == OPENCODE) {
        return Vec::new();
    }
    // What OpenCode lists through Ollama counts too, for when Ollama itself didn't answer.
    let installed: Vec<&str> = reading
        .ollama
        .tags
        .keys()
        .cloned()
        .chain(choices.iter().filter_map(ollama_tag))
        .filter_map(|tag| matching(local, &tag))
        .map(|model| model.tag.as_str())
        .collect();
    let machine = &reading.machine;
    ranked(local, machine.limit)
        .into_iter()
        .filter(|placed| placed.model.tools && !installed.contains(&placed.model.tag.as_str()))
        .filter(|placed| matches!(machine.fit(placed.ram), Fit::Fits | Fit::Tight { .. }))
        .take(SUGGESTIONS)
        .map(|placed| suggestion(&placed, machine))
        .collect()
}

fn suggestion(placed: &Placed<'_>, machine: &MachineMemory) -> ModelChoice {
    let model = placed.model;
    ModelChoice {
        worker: ModelRef::new(OPENCODE, Some(format!("{OLLAMA}{}", model.tag))),
        provider: String::new(),
        access: ModelAccess::Local,
        cost: CostEstimate::Free,
        plan_usage: None,
        efforts: Vec::new(),
        strengths: Vec::new(),
        current: false,
        note: None,
        older: false,
        name: model.name.clone(),
        description: None,
        recommended: false,
        new: false,
        default_effort: None,
        effort: None,
        alias: false,
        runs: None,
        listed: Some(ListedFacts {
            group: Some(NOT_INSTALLED.to_owned()),
            window: Some(placed.window),
            no_tools: false,
            thinking: model.thinking,
            hosted_by: None,
            memory: Some(ModelMemory {
                ram: placed.ram,
                fit: machine.fit(placed.ram),
                download: Some(download(model)),
            }),
        }),
    }
}

/// The local model with tool use that ranks best for this computer, of those that fit now with
/// room for a reviewer: an installed one at the window it's set to, with what it was measured
/// to take, and one not installed at the window it ranks at.
fn recommend(
    choices: &mut [ModelChoice],
    suggested: &mut [ModelChoice],
    machine: &MachineMemory,
    local: &[LocalModel],
) {
    let room = room(machine.limit);
    let candidate = |choice: &ModelChoice| {
        let model = matching(local, &ollama_tag(choice)?)?;
        let facts = choice.listed.as_ref()?;
        let memory = facts.memory.as_ref()?;
        let fits = matches!(memory.fit, Fit::Fits | Fit::Loaded) && memory.ram <= room;
        (fits && !facts.no_tools && model.tools).then(|| Placed {
            model,
            window: facts.window.unwrap_or(model.window),
            ram: memory.ram,
        })
    };
    let best = choices
        .iter()
        .chain(suggested.iter())
        .filter_map(|choice| Some((candidate(choice)?, choice.worker.clone())))
        .min_by(|(a, _), (b, _)| better(a, b))
        .map(|(_, worker)| worker);
    for choice in choices.iter_mut().chain(suggested.iter_mut()) {
        if choice.worker.agent == OPENCODE && choice.worker.model.is_some() {
            choice.recommended = best.as_ref() == Some(&choice.worker);
        }
    }
}

/// The recommended model first among the local ones, then the ones not installed, then the
/// rest as they came.
fn order(choices: &mut Vec<ModelChoice>, suggested: Vec<ModelChoice>) {
    let local = |choice: &ModelChoice| ollama_tag(choice).is_some();
    if let Some(first) = choices.iter().position(local)
        && let Some(at) = choices.iter().position(|c| local(c) && c.recommended)
    {
        let recommended = choices.remove(at);
        choices.insert(first, recommended);
    }
    // After the local models, or after OpenCode's Default where there are none.
    let after = choices
        .iter()
        .rposition(local)
        .or_else(|| choices.iter().position(|c| c.worker.agent == OPENCODE))
        .map_or(choices.len(), |at| at + 1);
    choices.splice(after..after, suggested);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use argonauts_core::settings::Settings;
    use argonauts_registry::models::cli_entry;

    use argonauts_core::model_picker::{CodingScore, Figure};

    use super::*;
    use crate::host::memory::{Held, Tag};

    const GIB: u64 = 1 << 30;

    fn local(tag: &str, window: u64) -> ModelChoice {
        ModelChoice {
            worker: ModelRef::new(OPENCODE, Some(format!("{OLLAMA}{tag}"))),
            provider: String::new(),
            access: ModelAccess::Local,
            cost: CostEstimate::Free,
            plan_usage: None,
            efforts: Vec::new(),
            strengths: Vec::new(),
            current: false,
            note: None,
            older: false,
            name: String::new(),
            description: None,
            recommended: false,
            new: false,
            default_effort: None,
            effort: None,
            alias: false,
            runs: None,
            listed: Some(ListedFacts {
                group: Some("Ollama".to_owned()),
                window: Some(window),
                ..ListedFacts::default()
            }),
        }
    }

    /// A made-up catalog: a 30B coder that wants a roomy computer, a 14B, an 8B and a 7B
    /// ranked by their scores, and a tiny model with no score.
    fn catalog() -> Vec<LocalModel> {
        let model =
            |tag: &str, weights: f64, kib: f64, window: u64, score: Option<f64>| LocalModel {
                tag: tag.to_owned(),
                name: format!("Sample {tag}"),
                window,
                weights_gb: Figure(weights),
                download_gb: Figure(weights),
                context_kib_per_token: Figure(kib),
                tools: true,
                thinking: false,
                released: Some("2025-07".to_owned()),
                coding: score.map(|score| CodingScore {
                    score: Figure(score),
                    source: "a sample leaderboard".to_owned(),
                    checked: "2026-10-20".to_owned(),
                }),
                replaced_by: None,
            };
        vec![
            model("sample-coder:30b", 19.0, 96.0, 65_536, Some(51.0)),
            model("sample:14b", 9.0, 32.0, 65_536, Some(45.0)),
            model("sample:8b", 6.0, 32.0, 32_768, Some(40.0)),
            model("sample:7b", 5.0, 32.0, 32_768, Some(30.0)),
            model("tiny:latest", 2.0, 64.0, 32_768, None),
        ]
    }

    fn routes(choices: &[ModelChoice]) -> Vec<&str> {
        choices
            .iter()
            .filter_map(|choice| choice.worker.model.as_deref())
            .collect()
    }

    fn memory(choice: &ModelChoice) -> &ModelMemory {
        choice.listed.as_ref().unwrap().memory.as_ref().unwrap()
    }

    /// A 16 GB computer with nothing loaded: the 30B coder and the tiny model at 128k won't fit.
    fn small_computer() -> Reading {
        let tags = BTreeMap::from([
            (
                "sample-coder:30b-64k".to_owned(),
                Tag {
                    size: 18 * GIB,
                    per_token: Some(98_304),
                    thinking: false,
                },
            ),
            (
                "tiny:latest".to_owned(),
                Tag {
                    size: 2 * GIB,
                    per_token: Some(114_688),
                    thinking: false,
                },
            ),
        ]);
        Reading {
            machine: MachineMemory {
                limit: 11 * GIB,
                total: 16 * GIB,
                in_use: 0,
                free: 8 * GIB,
            },
            ollama: Ollama {
                tags,
                ..Ollama::default()
            },
        }
    }

    /// The 64 GB computer with the 30B coder loaded at 64k.
    fn roomy_computer(free_gb: u64) -> Reading {
        let mut reading = small_computer();
        reading.machine = MachineMemory {
            limit: 52 * GIB,
            total: 64 * GIB,
            in_use: 25 * GIB,
            free: free_gb * GIB,
        };
        reading.ollama.loaded.insert(
            "sample-coder:30b-64k".to_owned(),
            Held {
                bytes: 25 * GIB,
                window: Some(65_536),
            },
        );
        reading
    }

    #[test]
    fn a_small_computer_marks_what_wont_fit_and_recommends_the_best_that_does() {
        let default = ModelChoice {
            worker: ModelRef::new(OPENCODE, None),
            listed: None,
            ..local("unused", 0)
        };
        let mut choices = vec![
            default,
            local("sample-coder:30b-64k", 65_536),
            local("tiny:latest", 131_072),
        ];
        annotate(&mut choices, &small_computer(), &[], &catalog());
        assert_eq!(
            memory(&choices[1]).fit,
            Fit::TooBig { at: None },
            "even at 32k it needs 21 GB"
        );
        let Fit::TooBig {
            at: Some((window, ram)),
        } = memory(&choices[2]).fit
        else {
            panic!("{:?}", memory(&choices[2]).fit);
        };
        assert_eq!(window, 32_768);
        assert_eq!(ram, 2 * GIB + 114_688 * 32_768);

        // The best scores that fit two thirds of 11 GB, at 32k.
        assert_eq!(
            routes(&choices),
            [
                "ollama/sample-coder:30b-64k",
                "ollama/tiny:latest",
                "ollama/sample:8b",
                "ollama/sample:7b"
            ]
        );
        let best = &choices[3];
        assert!(best.recommended && !choices[1].recommended);
        let facts = best.listed.as_ref().unwrap();
        assert_eq!(facts.group.as_deref(), Some(NOT_INSTALLED));
        assert_eq!(facts.window, Some(32_768));
        assert_eq!(memory(best).download, Some(6 * GIB));
        assert_eq!(memory(best).ram, 7 * GIB);
    }

    #[test]
    fn the_loaded_model_that_ranks_best_comes_first() {
        let mut choices = vec![
            local("tiny:latest", 131_072),
            local("sample-coder:30b-64k", 65_536),
        ];
        let mut entry = cli_entry(&Settings::default(), OPENCODE, None);
        entry.cli_default = false;
        annotate(&mut choices, &roomy_computer(18), &[entry], &catalog());
        assert!(choices[0].recommended, "{:?}", choices[0].worker);
        assert_eq!(memory(&choices[0]).fit, Fit::Loaded);
        // The tiny model at 128k takes 16 GB, under the 18 GB free.
        assert_eq!(memory(&choices[1]).fit, Fit::Fits);
        assert_eq!(
            routes(&choices[2..]),
            ["ollama/sample:14b", "ollama/sample:8b", "ollama/sample:7b"]
        );
    }

    /// A model not installed that needs more than is free now says so, and isn't recommended.
    #[test]
    fn a_suggestion_says_when_it_would_squeeze_other_apps() {
        let mut choices = vec![local("tiny:latest", 32_768)];
        annotate(&mut choices, &roomy_computer(10), &[], &catalog());
        let fourteen = choices
            .iter()
            .find(|choice| choice.worker.model.as_deref() == Some("ollama/sample:14b"))
            .expect("the 14B is offered");
        assert!(matches!(memory(fourteen).fit, Fit::Tight { .. }));
        assert!(!fourteen.recommended);
        assert!(choices.iter().any(|choice| choice.recommended));
    }

    /// Where Ollama didn't answer, what OpenCode lists through it isn't offered again.
    #[test]
    fn a_model_opencode_lists_is_not_offered_when_ollama_is_quiet() {
        let mut reading = roomy_computer(18);
        reading.ollama = Ollama::default();
        let mut choices = vec![local("sample:14b", 65_536)];
        annotate(&mut choices, &reading, &[], &catalog());
        assert_eq!(
            routes(&choices),
            [
                "ollama/sample:14b",
                "ollama/sample-coder:30b",
                "ollama/sample:8b",
                "ollama/sample:7b"
            ]
        );
    }
}
