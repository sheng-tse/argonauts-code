//! The models a CLI lists itself, as discovery last read them, and the one nearest a model it
//! no longer lists: a CLI that updates itself may drop a model a setting still names, and the
//! session would only be refused.

use std::cmp::Reverse;

use super::{ModelEntry, ModelRegistry};
use crate::display_names::DisplayNames;
use crate::input::EFFORT_LEVELS;
use crate::log::NO_LONGER_OFFERS;
use crate::model_catalog::{display_name_from_id, shown_name};
use crate::names::ModelRef;

/// Claude Code lists no models of its own: its list comes from Anthropic, and it takes models
/// that list leaves out, so a model missing from it may still run. Its older models move to
/// their successor instead.
const LISTS_NONE_ITSELF: &str = "claude-code";

/// The model `worker`'s CLI runs in place of the one `worker` names when the CLI no longer
/// lists it: the nearest it does list, as agy's `gemini-3.8-flash-low` is for
/// `gemini-3.6-flash-low`. `None` while the CLI lists the model, when nothing was read of its
/// list, and when no listed model is of the same line.
pub fn nearest_listed(registry: &dyn ModelRegistry, worker: &ModelRef) -> Option<ModelRef> {
    let model = worker.model.as_deref()?;
    let listed = listed_by(registry, &worker.agent);
    if listed.is_empty() || listed.iter().any(|entry| names(entry, model)) {
        return None;
    }
    nearest_first(&listed, model).into_iter().next()
}

/// Every other current model of `worker`'s CLI, for a job its model was refused in or a second
/// agent of that CLI in one step: those of its model's line nearest first, then the rest as the
/// registry orders them. A CLI that lists its models offers only those, and one that lists none
/// itself, as Claude Code, the ones the registry knows of it.
pub fn other_models(registry: &dyn ModelRegistry, worker: &ModelRef) -> Vec<ModelRef> {
    let model = worker.model.as_deref().unwrap_or_default();
    let mut models = listed_by(registry, &worker.agent);
    if models.is_empty() {
        models = registry
            .list()
            .into_iter()
            .filter(|entry| entry.id.agent == worker.agent && !entry.retired)
            .filter(|entry| entry.id.model.is_some())
            .collect();
    }
    models.retain(|entry| !names(entry, model));
    let mut others = nearest_first(&models, model);
    for entry in models {
        if !others.contains(&entry.id) {
            others.push(entry.id);
        }
    }
    others
}

/// The line said before `who` starts on `runs`, the model its CLI lists nearest to `gone`:
/// `propose:p3 runs on Gemini 3.8 Flash (Low): Antigravity CLI no longer offers Gemini 3.6
/// Flash (Low)`.
pub fn runs_in_place(
    registry: &dyn ModelRegistry,
    who: &str,
    gone: &ModelRef,
    runs: &ModelRef,
) -> String {
    let names = DisplayNames::default();
    let runs_name = model_name(registry, runs).unwrap_or_else(|| names.worker(runs));
    let was = gone_name(gone, runs, &runs_name, model_name(registry, gone));
    let cli = names.cli(&runs.agent);
    format!("{who} runs on {runs_name}: {cli}{NO_LONGER_OFFERS}{was}")
}

/// The CLI's own name for a model it lists, as agy's `Gemini 3.8 Flash (Low)`, else the name
/// the catalog knows it by.
pub fn model_name(registry: &dyn ModelRegistry, model: &ModelRef) -> Option<String> {
    let own = registry.get(model).and_then(|entry| entry.name);
    own.map(|name| shown_name(&model.agent, &name)).or_else(|| {
        let names = DisplayNames {
            catalog: registry.catalog(),
            ..DisplayNames::default()
        };
        names.model(&model.agent, model.model.as_deref())
    })
}

/// What a model no longer listed is called: the name the catalog knows it by, else the name of
/// the model that runs in its place with its version, as agy names `Gemini 3.8 Flash (Low)`
/// and so once named `Gemini 3.6 Flash (Low)`, else a name from its id.
fn gone_name(gone: &ModelRef, runs: &ModelRef, runs_name: &str, known: Option<String>) -> String {
    let from_id = |model: &ModelRef| display_name_from_id(model.model.as_deref().unwrap_or(""));
    let gone_id = from_id(gone);
    if let Some(known) = known.filter(|known| *known != gone_id) {
        return known;
    }
    let version = |name: &str| {
        name.split(' ')
            .find(|word| word.starts_with(|c: char| c.is_ascii_digit()))
            .map(str::to_owned)
    };
    let alike = match (version(&from_id(runs)), version(&gone_id)) {
        (Some(new), Some(old)) => Some(runs_name.replacen(&new, &old, 1)),
        _ => None,
    };
    alike
        .filter(|alike| alike.replace(['(', ')'], "") == gone_id)
        .unwrap_or(gone_id)
}

/// What `cli` lists itself, current models only.
fn listed_by(registry: &dyn ModelRegistry, cli: &str) -> Vec<ModelEntry> {
    if cli == LISTS_NONE_ITSELF {
        return Vec::new();
    }
    registry
        .list()
        .into_iter()
        .filter(|entry| entry.id.agent == cli && entry.discovered && !entry.retired)
        .filter(|entry| entry.id.model.is_some())
        .collect()
}

fn names(entry: &ModelEntry, model: &str) -> bool {
    entry.id.model.as_deref() == Some(model) || entry.aliases.iter().any(|alias| alias == model)
}

/// The listed models of `model`'s line, nearest first: the most words of its name in common, then the
/// closest level, then a newer version before an older one, the closest first. A model of
/// another line, whose first word isn't the same, is never near.
fn nearest_first(listed: &[ModelEntry], model: &str) -> Vec<ModelRef> {
    let asked = Shape::of(model);
    let mut near: Vec<(Nearness, &ModelEntry)> = listed
        .iter()
        .filter_map(|entry| {
            let shape = Shape::of(entry.id.model.as_deref()?);
            (shape.words.first() == asked.words.first()).then(|| (asked.nearness(&shape), entry))
        })
        .collect();
    near.sort_by(|a, b| a.0.cmp(&b.0));
    near.into_iter()
        .map(|(_, entry)| entry.id.clone())
        .collect()
}

/// Smaller is nearer: words in common, the gap between levels, then the version's.
type Nearness = (Reverse<usize>, usize, (bool, Vec<u32>));

/// A model id as its words and its version: `gemini-3.8-flash-low` is `gemini flash low` at
/// `3.8`, and `claude-haiku-5-5` is `claude haiku` at `5.5`.
struct Shape {
    words: Vec<String>,
    version: Vec<u32>,
}

impl Shape {
    fn of(model: &str) -> Self {
        let id = model
            .rsplit('/')
            .next()
            .unwrap_or(model)
            .to_ascii_lowercase();
        let mut shape = Self {
            words: Vec::new(),
            version: Vec::new(),
        };
        for token in id
            .split(['-', '_', '.', ':'])
            .filter(|token| !token.is_empty())
        {
            match token.parse::<u32>() {
                // A release date is no version.
                Ok(number) if token.len() <= 2 => shape.version.push(number),
                Ok(_) => {}
                Err(_) => shape.words.push(token.to_owned()),
            }
        }
        shape
    }

    fn nearness(&self, other: &Self) -> Nearness {
        // The line comes first: a level is a word of its own, weighed apart.
        let shared = other
            .words
            .iter()
            .filter(|word| !EFFORT_LEVELS.contains(&word.as_str()) && self.words.contains(word))
            .count();
        (Reverse(shared), self.level_gap(other), self.distance(other))
    }

    /// How many levels apart the two are, as `low` is one from `medium`. A model named for no
    /// level is as far as can be from one that is.
    fn level_gap(&self, other: &Self) -> usize {
        let level = |shape: &Self| {
            shape
                .words
                .iter()
                .find_map(|word| EFFORT_LEVELS.iter().position(|level| level == word))
        };
        match (level(self), level(other)) {
            (Some(a), Some(b)) => a.abs_diff(b),
            (None, None) => 0,
            _ => EFFORT_LEVELS.len(),
        }
    }

    /// How far `other`'s version is from this one: a newer one comes before an older, since a
    /// CLI drops a model when a later one replaces it, then the closest.
    fn distance(&self, other: &Self) -> (bool, Vec<u32>) {
        let older = other.version < self.version;
        let width = self.version.len().max(other.version.len());
        let part = |version: &[u32], at: usize| version.get(at).copied().unwrap_or(0);
        let gap = (0..width)
            .map(|at| part(&self.version, at).abs_diff(part(&other.version, at)))
            .collect();
        (older, gap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::CostEstimate;
    use crate::model_catalog::ModelCatalog;

    struct Listed(Vec<ModelEntry>);

    impl ModelRegistry for Listed {
        fn get(&self, worker: &ModelRef) -> Option<ModelEntry> {
            self.0.iter().find(|entry| entry.id == *worker).cloned()
        }
        fn provider(&self, _: &ModelRef) -> String {
            String::new()
        }
        fn list(&self) -> Vec<ModelEntry> {
            self.0.clone()
        }
        fn estimate(&self, _: &ModelRef, _: u64, _: u64) -> CostEstimate {
            CostEstimate::Unknown
        }
        fn discover(&self) -> crate::future::BoxFuture<'_, Vec<ModelEntry>> {
            Box::pin(async { Vec::new() })
        }
        fn catalog(&self) -> ModelCatalog {
            ModelCatalog::default()
        }
    }

    fn entry(cli: &str, model: &str, discovered: bool) -> ModelEntry {
        let mut entry: ModelEntry = serde_json::from_value(serde_json::json!({
            "id": format!("{cli}/{model}"),
            "provider": "",
            "kind": "cli",
            "access": "subscription",
            "cost": { "unit": "plan_usage" },
        }))
        .unwrap();
        entry.discovered = discovered;
        entry
    }

    fn agy_today() -> Listed {
        Listed(
            [
                "gemini-3.8-flash-high",
                "gemini-3.8-flash-medium",
                "gemini-3.8-flash-low",
                "claude-sonnet-4-6-thinking",
                "gpt-oss-120b-medium",
            ]
            .into_iter()
            .map(|model| entry("antigravity-cli", model, true))
            .collect(),
        )
    }

    fn agy(model: &str) -> ModelRef {
        ModelRef::new("antigravity-cli", Some(model.to_owned()))
    }

    #[test]
    fn a_model_the_cli_no_longer_lists_runs_the_nearest_of_its_line() {
        let registry = agy_today();
        assert_eq!(
            nearest_listed(&registry, &agy("gemini-3.6-flash-low")),
            Some(agy("gemini-3.8-flash-low"))
        );
        // The level is a word like any other, so the line's other levels come after.
        assert_eq!(
            nearest_listed(&registry, &agy("gemini-3.6-flash-high")),
            Some(agy("gemini-3.8-flash-high"))
        );
    }

    #[test]
    fn a_listed_model_or_one_of_no_known_line_stays_as_it_is() {
        let registry = agy_today();
        assert_eq!(
            nearest_listed(&registry, &agy("gemini-3.8-flash-low")),
            None
        );
        assert_eq!(nearest_listed(&registry, &agy("qwen-3-coder")), None);
        assert_eq!(
            nearest_listed(&registry, &ModelRef::new("antigravity-cli", None)),
            None
        );
    }

    #[test]
    fn a_cli_whose_list_was_never_read_keeps_its_model() {
        let registry = Listed(vec![entry(
            "antigravity-cli",
            "gemini-3.8-flash-low",
            false,
        )]);
        assert_eq!(
            nearest_listed(&registry, &agy("gemini-3.6-flash-low")),
            None
        );
        // Claude Code's list is Anthropic's, and it runs models that list leaves out.
        let claude = Listed(vec![entry("claude-code", "claude-haiku-5-5", true)]);
        let older = ModelRef::new("claude-code", Some("claude-haiku-4-5".to_owned()));
        assert_eq!(nearest_listed(&claude, &older), None);
    }

    #[test]
    fn a_newer_version_comes_before_an_older_one_and_the_closest_first() {
        let registry = Listed(
            [
                "gemini-3.5-flash-low",
                "gemini-4.0-flash-low",
                "gemini-3.8-flash-low",
            ]
            .into_iter()
            .map(|model| entry("antigravity-cli", model, true))
            .collect(),
        );
        assert_eq!(
            other_models(&registry, &agy("gemini-3.6-flash-low")),
            vec![
                agy("gemini-3.8-flash-low"),
                agy("gemini-4.0-flash-low"),
                agy("gemini-3.5-flash-low"),
            ]
        );
    }

    /// A refused model, and a second agent of a CLI in one step, both go to the CLI's other
    /// models, its line first.
    #[test]
    fn another_model_of_a_cli_is_of_its_line_first_then_its_other_models() {
        let registry = agy_today();
        assert_eq!(
            other_models(&registry, &agy("gemini-3.8-flash-low")),
            vec![
                agy("gemini-3.8-flash-medium"),
                agy("gemini-3.8-flash-high"),
                agy("claude-sonnet-4-6-thinking"),
                agy("gpt-oss-120b-medium"),
            ]
        );
        // A CLI that lists none itself offers what the registry knows of it.
        let claude = |model: &str| ModelRef::new("claude-code", Some(model.to_owned()));
        let known = Listed(vec![
            entry("claude-code", "haiku", false),
            entry("claude-code", "sonnet", false),
        ]);
        assert_eq!(
            other_models(&known, &claude("haiku")),
            vec![claude("sonnet")]
        );
    }
}
