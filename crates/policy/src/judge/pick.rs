//! The judge when you named none that may check the work: the cheapest model you can run from
//! another provider than every author of the work it judges. The candidates are the models the
//! live lists found for each installed agent, else each agent's cheapest model, else the agent
//! as it's set up, leaving out any whose CLI would refuse to start.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::ModelRef;
use argonauts_core::pricing::builtin_price;
use argonauts_core::registry::provider::{is_known, provider_name};
use argonauts_core::registry::{CostUnit, ModelEntry, ModelRegistry};
use argonauts_core::session_runner::RunnerDirectory;
use argonauts_core::text::sentence_case;

use super::independence;

/// What the pick reads: the agents you can run, and the live model lists when there are any.
#[derive(Clone, Copy)]
pub struct Sources<'a> {
    pub workers: &'a dyn RunnerDirectory,
    pub models: Option<&'a dyn ModelRegistry>,
}

/// Why no judge could be picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoJudge {
    /// An author's provider is unknown, so no candidate can be shown to come from elsewhere.
    UnknownAuthor(ModelRef),
    /// Nothing you can run comes from a provider none of the authors come from.
    NoneElsewhere { providers: Vec<String> },
}

impl NoJudge {
    /// One line for the approval, every time it asks.
    pub fn reason(&self) -> String {
        match self {
            Self::UnknownAuthor(author) => format!(
                "the provider behind {} isn't known, so no judge can be shown to come from \
                 another provider",
                DisplayNames::default().worker(author)
            ),
            Self::NoneElsewhere { providers } => {
                let providers: Vec<String> = providers
                    .iter()
                    .map(|provider| provider_name(provider))
                    .collect();
                format!(
                    "no model you can run comes from another provider than {}, so no judge \
                     can judge this",
                    providers.join(" or ")
                )
            }
        }
    }

    /// The fuller warning, given once, saying how to give auto a judge. Auto asks you
    /// meanwhile, and autopilot lets your rules decide alone, which its own line says.
    pub fn warning(&self, autopilot: bool) -> String {
        let fix = "pick a judge in /approvals, or install and log in to an agent from \
                   another provider";
        if autopilot {
            format!("{}. {}", sentence_case(&self.reason()), sentence_case(fix))
        } else {
            format!(
                "{}. Auto asks you instead until you {fix}",
                sentence_case(&self.reason())
            )
        }
    }
}

/// The cheapest candidate from a provider none of `authors` comes from, which also passes the
/// independence check. Ties go to the agent listed first.
pub fn pick(
    authors: &[ModelRef],
    different_provider: bool,
    sources: Sources<'_>,
) -> Result<ModelRef, NoJudge> {
    pick_besides(authors, different_provider, sources, &[])
}

/// Like [`pick`], leaving out every model of an agent in `failed`, whose judge sessions
/// already failed for this approval.
pub fn pick_besides(
    authors: &[ModelRef],
    different_provider: bool,
    sources: Sources<'_>,
    failed: &[ModelRef],
) -> Result<ModelRef, NoJudge> {
    cheapest(authors, different_provider, sources, failed, |_| true)
}

/// Like [`pick_besides`], for the fallback of a judge whose session failed:
/// only one whose CLI keeps your plugins out of the judge's isolated session, which the role
/// needs, since nobody chose it to see them.
pub fn pick_fallback(
    authors: &[ModelRef],
    different_provider: bool,
    sources: Sources<'_>,
    failed: &[ModelRef],
) -> Result<ModelRef, NoJudge> {
    cheapest(authors, different_provider, sources, failed, |worker| {
        sources.workers.isolates(worker)
    })
}

fn cheapest(
    authors: &[ModelRef],
    different_provider: bool,
    sources: Sources<'_>,
    failed: &[ModelRef],
    fits: impl Fn(&ModelRef) -> bool,
) -> Result<ModelRef, NoJudge> {
    let mut providers: Vec<String> = Vec::new();
    for author in authors {
        let Some(provider) = provider(author, sources) else {
            return Err(NoJudge::UnknownAuthor(author.clone()));
        };
        if !providers.contains(&provider) {
            providers.push(provider);
        }
    }
    let mut best: Option<(f64, ModelRef)> = None;
    for (price, candidate) in candidates(sources) {
        let Some(provider) = provider(&candidate, sources) else {
            continue;
        };
        if failed
            .iter()
            .any(|failed| failed.agent_id() == candidate.agent_id())
            || !fits(&candidate)
            || refuses_to_start(&candidate, sources)
        {
            continue;
        }
        if providers.contains(&provider)
            || independence::check(&candidate, authors, different_provider, sources.workers)
                .is_err()
        {
            continue;
        }
        // Strictly cheaper, so ties keep the order the agents were listed in.
        if best.as_ref().is_none_or(|(cheapest, _)| price < *cheapest) {
            best = Some((price, candidate));
        }
    }
    best.map(|(_, worker)| worker)
        .ok_or(NoJudge::NoneElsewhere { providers })
}

/// Whether `candidate`'s CLI would refuse the judge's session before it starts, as OpenCode
/// does with no model set.
fn refuses_to_start(candidate: &ModelRef, sources: Sources<'_>) -> bool {
    sources
        .workers
        .get(candidate)
        .is_ok_and(|runner| runner.start_refusal().is_some())
}

/// Every model the pick may choose, with its price per million tokens in and out together.
/// A model without a known price costs infinity, so it's picked only when nothing priced fits.
fn candidates(sources: Sources<'_>) -> Vec<(f64, ModelRef)> {
    let listed: Vec<ModelEntry> = sources
        .models
        .map(|models| models.list())
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry.discovered && !entry.retired && entry.id.model.is_some())
        .collect();
    let mut found = Vec::new();
    for available in sources.workers.available() {
        let Ok(worker) = sources.workers.get(&available) else {
            continue;
        };
        let live: Vec<&ModelEntry> = listed
            .iter()
            .filter(|entry| entry.id.agent == available.agent)
            .collect();
        if !live.is_empty() {
            found.extend(
                live.into_iter()
                    .map(|entry| (entry_price(entry), entry.id.clone())),
            );
            continue;
        }
        let candidate = match worker.cheapest_model() {
            Some(model) => ModelRef {
                agent: available.agent.clone(),
                model: Some(model),
            },
            None => available,
        };
        found.push((price(&candidate, sources), candidate));
    }
    found
}

fn price(worker: &ModelRef, sources: Sources<'_>) -> f64 {
    if let Some(entry) = sources.models.and_then(|models| models.get(worker)) {
        let priced = entry_price(&entry);
        if priced.is_finite() {
            return priced;
        }
    }
    worker
        .model
        .as_deref()
        .and_then(builtin_price)
        .map_or(f64::INFINITY, |price| {
            price.input_per_mtok + price.output_per_mtok
        })
}

fn entry_price(entry: &ModelEntry) -> f64 {
    if entry.cost.unit == CostUnit::None {
        return 0.0;
    }
    match (entry.cost.input_per_mtok, entry.cost.output_per_mtok) {
        (Some(input), Some(output)) if (input + output).is_finite() => input + output,
        _ => entry
            .id
            .model
            .as_deref()
            .and_then(builtin_price)
            .map_or(f64::INFINITY, |price| {
                price.input_per_mtok + price.output_per_mtok
            }),
    }
}

/// The model's provider as its agent reports it, else as the model lists know it. `None` when
/// neither knows.
pub fn provider(worker: &ModelRef, sources: Sources<'_>) -> Option<String> {
    let known = |provider: String| {
        let provider = provider.trim().to_lowercase();
        (is_known(&provider) && provider != "from-model").then_some(provider)
    };
    sources
        .workers
        .get(worker)
        .ok()
        .and_then(|agent| known(agent.provider()))
        .or_else(|| {
            sources
                .models
                .and_then(|models| known(models.provider(worker)))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::registry::{CostModel, ModelAccess, RegistryKind};
    use argonauts_testkit::services::StaticRegistry;
    use argonauts_testkit::{FakeDirectory, FakeRunner};

    fn r(text: &str) -> ModelRef {
        text.parse().unwrap()
    }

    fn crew() -> FakeDirectory {
        let workers = FakeDirectory::new();
        // Haiku 4.5, priced above the other two; haiku itself now runs the cheaper Haiku 5.5.
        workers.add(
            FakeRunner::cli("claude-code", "anthropic").with_cheapest_model("claude-haiku-4-5"),
        );
        workers.add(FakeRunner::cli("codex", "openai").with_cheapest_model("gpt-6-luna"));
        workers.add(
            FakeRunner::cli("antigravity-cli", "google")
                .with_cheapest_model("gemini-3.8-flash-low"),
        );
        workers
    }

    fn picked(
        authors: &[&str],
        workers: &FakeDirectory,
        models: Option<&StaticRegistry>,
    ) -> Result<ModelRef, NoJudge> {
        let authors: Vec<ModelRef> = authors.iter().map(|a| r(a)).collect();
        let sources = Sources {
            workers,
            models: models.map(|m| m as &dyn ModelRegistry),
        };
        pick(&authors, true, sources)
    }

    fn live(id: &str, provider: &str, input: f64, output: f64) -> ModelEntry {
        ModelEntry {
            id: r(id),
            provider: provider.to_owned(),
            kind: RegistryKind::Cli,
            access: ModelAccess::Subscription,
            tools: true,
            cost: CostModel {
                unit: argonauts_core::registry::CostUnit::Usd,
                input_per_mtok: Some(input),
                output_per_mtok: Some(output),
                cached_input_per_mtok: None,
            },
            strengths: Vec::new(),
            good_for: Vec::new(),
            context_window: None,
            efforts: Vec::new(),
            aliases: Vec::new(),
            retired: false,
            successor: None,
            discovered: true,
            cli_default: false,
            default_effort: None,
            launch: None,
            tier: None,
            name: None,
            description: None,
        }
    }

    #[test]
    fn the_cheapest_agent_of_another_provider_judges() {
        let workers = crew();
        // Of Haiku 4.5, luna, and flash, luna is the cheapest; flash is the cheapest that isn't
        // OpenAI's.
        assert_eq!(
            picked(&["claude-code/opus"], &workers, None),
            Ok(r("codex/gpt-6-luna"))
        );
        assert_eq!(
            picked(&["codex/gpt-6-sol"], &workers, None),
            Ok(r("antigravity-cli/gemini-3.8-flash-low"))
        );
        assert_eq!(
            picked(&["codex/gpt-6-sol", "antigravity-cli"], &workers, None),
            Ok(r("claude-code/claude-haiku-4-5"))
        );
    }

    #[test]
    fn a_cli_that_would_refuse_to_start_never_judges() {
        let workers = crew();
        workers.add(
            FakeRunner::cli("opencode", "alibaba")
                .with_cheapest_model("qwen-tiny")
                .refusing_to_start("OpenCode has no model set"),
        );
        let models = StaticRegistry::default();
        models.add(live("opencode/qwen-tiny", "alibaba", 0.01, 0.02));
        assert_eq!(
            picked(&["claude-code/opus"], &workers, Some(&models)),
            Ok(r("codex/gpt-6-luna"))
        );
        let others = ["claude-code/opus", "codex/gpt-6-sol", "antigravity-cli"];
        assert!(matches!(
            picked(&others, &workers, Some(&models)),
            Err(NoJudge::NoneElsewhere { .. })
        ));
    }

    #[test]
    fn the_live_model_lists_win_over_each_agents_cheapest() {
        let workers = crew();
        let models = StaticRegistry::default();
        models.add(live("antigravity-cli/gemini-tiny", "google", 0.01, 0.02));
        models.add(live("antigravity-cli/gemini-big", "google", 5.0, 20.0));
        let mut retired = live("codex/gpt-old", "openai", 0.001, 0.001);
        retired.retired = true;
        models.add(retired);
        assert_eq!(
            picked(&["claude-code/opus"], &workers, Some(&models)),
            Ok(r("antigravity-cli/gemini-tiny"))
        );
    }

    #[test]
    fn an_open_weight_model_counts_as_its_makers() {
        let workers = FakeDirectory::new();
        workers.add(FakeRunner::cli("codex", "openai"));
        workers.add(FakeRunner::endpoint("openrouter/gpt-oss-120b", "openai"));
        assert_eq!(
            picked(&["codex"], &workers, None),
            Err(NoJudge::NoneElsewhere {
                providers: vec!["openai".to_owned()]
            })
        );
    }

    #[test]
    fn nothing_from_elsewhere_or_an_unknown_author_names_why() {
        let workers = FakeDirectory::new();
        workers.add(FakeRunner::cli("claude-code", "anthropic"));
        let none = picked(&["claude-code/opus"], &workers, None).unwrap_err();
        assert!(none.reason().contains("another provider than Anthropic"));
        assert!(none.warning(false).starts_with("No model"));
        assert!(none.warning(false).contains("pick a judge in /approvals"));
        assert!(!none.warning(false).contains("approvals.judge"));
        // Autopilot never asks you, so its warning only says what gives it a judge.
        let autopilot = none.warning(true);
        assert!(!autopilot.contains("asks you"), "{autopilot}");
        assert!(
            autopilot.contains(". Pick a judge in /approvals"),
            "{autopilot}"
        );
        let unknown = picked(&["mystery"], &workers, None).unwrap_err();
        assert_eq!(unknown, NoJudge::UnknownAuthor(r("mystery")));
        // A worker the app has no name for keeps its spelling.
        let unknown = NoJudge::UnknownAuthor(r("my-cli/fast"));
        assert!(
            unknown
                .warning(false)
                .starts_with("The provider behind Fast on my-cli isn't known"),
            "{}",
            unknown.warning(false)
        );
    }

    #[test]
    fn with_no_author_the_cheapest_of_all_judges() {
        assert_eq!(picked(&[], &crew(), None), Ok(r("codex/gpt-6-luna")));
    }
}
