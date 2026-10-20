//! OpenCode's level knowing this machine's memory: each local model with the memory it takes
//! at its window and whether that fits, the best that fits with room for a reviewer marked
//! recommended and listed first, and up to three models not installed that would fit, from
//! the short list kept with the catalog.

use argonauts_core::app::ModelChoice;
use argonauts_core::limits::CostEstimate;
use argonauts_core::model_picker::{Fit, ListedFacts, MachineMemory, ModelMemory};
use argonauts_core::names::ModelRef;
use argonauts_core::registry::{ModelAccess, ModelEntry};
use argonauts_registry::models::suggested::{SUGGESTED, rank};

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

/// OpenCode's choices with what each takes of `reading`'s machine, Default with the window
/// and host of the model it runs, the recommended model first among the local ones, and the
/// models not installed that would fit after them.
pub(crate) fn annotate(choices: &mut Vec<ModelChoice>, reading: &Reading, listed: &[ModelEntry]) {
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
    let mut suggested = suggestions(choices, reading);
    recommend(choices, &mut suggested, machine);
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

/// The heading of the models not installed that would fit, worded for the machine they'd run on.
fn not_installed(machine: &MachineMemory) -> &'static str {
    if machine.mac {
        "Not installed · fits this Mac"
    } else {
        "Not installed · fits this machine"
    }
}

/// Up to three models from the list that aren't installed and would fit, best first.
fn suggestions(choices: &[ModelChoice], reading: &Reading) -> Vec<ModelChoice> {
    let installed: Vec<usize> = reading
        .ollama
        .tags
        .keys()
        .filter_map(|tag| rank(tag))
        .collect();
    if !choices.iter().any(|choice| choice.worker.agent == OPENCODE) {
        return Vec::new();
    }
    SUGGESTED
        .iter()
        .enumerate()
        .filter(|(at, _)| !installed.contains(at))
        // Room beside the loaded models, though the other apps may need closing.
        .filter(|(_, row)| matches!(reading.machine.fit(row.ram), Fit::Fits | Fit::Tight { .. }))
        .take(SUGGESTIONS)
        .map(|(_, row)| ModelChoice {
            worker: ModelRef::new(OPENCODE, Some(format!("{OLLAMA}{}", row.tag))),
            provider: String::new(),
            access: ModelAccess::Local,
            cost: CostEstimate::Free,
            plan_usage: None,
            efforts: Vec::new(),
            strengths: Vec::new(),
            current: false,
            note: None,
            older: false,
            name: row.name.to_owned(),
            description: None,
            recommended: false,
            new: false,
            default_effort: None,
            effort: None,
            alias: false,
            runs: None,
            listed: Some(ListedFacts {
                group: Some(not_installed(&reading.machine).to_owned()),
                window: Some(row.window),
                no_tools: false,
                thinking: row.thinking,
                hosted_by: None,
                memory: Some(ModelMemory {
                    ram: row.ram,
                    fit: Fit::Fits,
                    download: Some(row.download),
                }),
            }),
        })
        .collect()
}

/// The best-ranked local model with tool use that fits with room for a reviewer: a third of
/// what models may use left over.
fn recommend(choices: &mut [ModelChoice], suggested: &mut [ModelChoice], machine: &MachineMemory) {
    let room = machine.limit / 3 * 2;
    let candidate = |choice: &ModelChoice| {
        let tag = ollama_tag(choice)?;
        let facts = choice.listed.as_ref()?;
        let memory = facts.memory.as_ref()?;
        let fits = matches!(memory.fit, Fit::Fits | Fit::Loaded) && memory.ram <= room;
        (fits && !facts.no_tools).then(|| rank(&tag)).flatten()
    };
    let best = choices
        .iter()
        .chain(suggested.iter())
        .filter_map(|choice| Some((candidate(choice)?, choice.worker.clone())))
        .min_by_key(|(rank, _)| *rank)
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

    /// A 16 GB Mac with nothing loaded: Qwen3 Coder 30B (64k) and Llama3.2 at 128k won't fit.
    fn small_mac() -> Reading {
        let tags = BTreeMap::from([
            (
                "qwen3-coder:30b-64k".to_owned(),
                Tag {
                    size: 18 * GIB,
                    per_token: Some(98_304),
                    thinking: false,
                },
            ),
            (
                "llama3.2:latest".to_owned(),
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
                mac: true,
            },
            ollama: Ollama {
                tags,
                ..Ollama::default()
            },
        }
    }

    #[test]
    fn a_small_mac_marks_what_wont_fit_and_recommends_one_that_does() {
        let default = ModelChoice {
            worker: ModelRef::new(OPENCODE, None),
            listed: None,
            ..local("unused", 0)
        };
        let mut choices = vec![
            default,
            local("qwen3-coder:30b-64k", 65_536),
            local("llama3.2:latest", 131_072),
        ];
        annotate(&mut choices, &small_mac(), &[]);
        let fit = |at: usize| {
            choices[at]
                .listed
                .as_ref()
                .unwrap()
                .memory
                .as_ref()
                .unwrap()
                .fit
        };
        assert_eq!(
            fit(1),
            Fit::TooBig { at: None },
            "even at 32k it needs 21 GB"
        );
        let Fit::TooBig {
            at: Some((window, ram)),
        } = fit(2)
        else {
            panic!("{:?}", fit(2));
        };
        assert_eq!(window, 32_768);
        assert_eq!(ram, 2 * GIB + 114_688 * 32_768);

        let names: Vec<String> = choices.iter().map(ModelChoice::display_name).collect();
        assert_eq!(
            names,
            [
                "Default",
                "Qwen3 Coder 30B 64K",
                "Llama3.2",
                "Qwen3 8B",
                "Qwen2.5 Coder 7B"
            ]
        );
        let qwen8 = &choices[3];
        assert!(qwen8.recommended && !choices[1].recommended);
        let facts = qwen8.listed.as_ref().unwrap();
        assert_eq!(
            facts.group.as_deref(),
            Some("Not installed · fits this Mac")
        );
        assert_eq!(facts.memory.as_ref().unwrap().download, Some(6 * GIB));
    }

    #[test]
    fn the_models_not_installed_fit_this_machine_on_a_machine_that_isnt_a_mac() {
        let mut reading = small_mac();
        reading.machine.mac = false;
        let mut choices = vec![local("llama3.2:latest", 131_072)];
        annotate(&mut choices, &reading, &[]);
        let headings: Vec<&str> = choices
            .iter()
            .filter_map(|choice| choice.listed.as_ref()?.group.as_deref())
            .filter(|group| group.starts_with("Not installed"))
            .collect();
        assert!(!headings.is_empty(), "{choices:?}");
        assert!(
            headings
                .iter()
                .all(|group| *group == "Not installed · fits this machine"),
            "{headings:?}"
        );
    }

    #[test]
    fn the_loaded_model_that_fits_with_room_for_a_reviewer_comes_first() {
        let mut reading = small_mac();
        reading.machine = MachineMemory {
            limit: 52 * GIB,
            total: 64 * GIB,
            in_use: 25 * GIB,
            free: 18 * GIB,
            mac: true,
        };
        reading.ollama.loaded.insert(
            "qwen3-coder:30b-64k".to_owned(),
            Held {
                bytes: 25 * GIB,
                window: Some(65_536),
            },
        );
        let mut choices = vec![
            local("llama3.2:latest", 131_072),
            local("qwen3-coder:30b-64k", 65_536),
        ];
        let mut entry = cli_entry(&Settings::default(), OPENCODE, None);
        entry.cli_default = false;
        annotate(&mut choices, &reading, &[entry]);
        assert!(choices[0].recommended, "{:?}", choices[0].worker);
        assert_eq!(
            choices[0]
                .listed
                .as_ref()
                .unwrap()
                .memory
                .as_ref()
                .unwrap()
                .fit,
            Fit::Loaded
        );
        // Llama3.2 at 128k takes 16 GB, under the 18 GB free.
        assert_eq!(
            choices[1]
                .listed
                .as_ref()
                .unwrap()
                .memory
                .as_ref()
                .unwrap()
                .fit,
            Fit::Fits
        );
        let suggested: Vec<&str> = choices[2..]
            .iter()
            .filter_map(|choice| choice.worker.model.as_deref())
            .collect();
        assert_eq!(
            suggested,
            [
                "ollama/qwen3.5:27b",
                "ollama/devstral:24b",
                "ollama/gpt-oss:20b"
            ]
        );
    }
}
