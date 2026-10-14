//! The judge when you named none that may check the work: the cheapest model you can run from
//! another company than every author of the work it judges. The candidates are the models the
//! live lists found for each installed agent, else each agent's cheapest model, else the agent
//! as it's set up.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::WorkerRef;
use argonauts_core::pricing::builtin_price;
use argonauts_core::registry::{CostUnit, ModelEntry, ModelRegistry, company_of};
use argonauts_core::text::sentence_case;
use argonauts_core::worker::WorkerDirectory;

use super::independence;

/// What the pick reads: the agents you can run, and the live model lists when there are any.
#[derive(Clone, Copy)]
pub struct Sources<'a> {
    pub workers: &'a dyn WorkerDirectory,
    pub models: Option<&'a dyn ModelRegistry>,
}

/// Why no approver could be picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoApprover {
    /// An author's family is unknown, so no candidate can be shown to come from elsewhere.
    UnknownAuthor(WorkerRef),
    /// Nothing you can run comes from a company none of the authors come from.
    NoneElsewhere { companies: Vec<String> },
}

impl NoApprover {
    /// One line for the checkpoint, every time it asks.
    pub fn reason(&self) -> String {
        match self {
            Self::UnknownAuthor(author) => format!(
                "the provider behind {} isn't known, so no approver can be shown to come from \
                 another provider",
                DisplayNames::default().worker(author)
            ),
            Self::NoneElsewhere { companies } => {
                let companies: Vec<String> = companies
                    .iter()
                    .map(|company| company_name(company))
                    .collect();
                format!(
                    "no model you can run comes from another provider than {}, so no approver \
                     can judge this",
                    companies.join(" or ")
                )
            }
        }
    }

    /// The fuller warning, given once, saying how to give auto an approver. Auto asks you
    /// meanwhile, and full auto lets your rules decide alone, which its own line says.
    pub fn warning(&self, autopilot: bool) -> String {
        let fix = "pick an approver in /approvals, or install and log in to an agent from \
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

/// A company as people write it, `OpenAI` for `openai`.
fn company_name(company: &str) -> String {
    match company {
        "openai" => "OpenAI".to_owned(),
        "xai" => "xAI".to_owned(),
        "deepseek" => "DeepSeek".to_owned(),
        other => sentence_case(other),
    }
}

/// The cheapest candidate from a company none of `authors` comes from, which also passes the
/// independence check. Ties go to the agent listed first.
pub fn pick(
    authors: &[WorkerRef],
    different_provider: bool,
    sources: Sources<'_>,
) -> Result<WorkerRef, NoApprover> {
    pick_besides(authors, different_provider, sources, &[])
}

/// Like [`pick`], leaving out every agent of a vendor in `failed`, whose approver sessions
/// already failed for this checkpoint.
pub fn pick_besides(
    authors: &[WorkerRef],
    different_provider: bool,
    sources: Sources<'_>,
    failed: &[WorkerRef],
) -> Result<WorkerRef, NoApprover> {
    cheapest(authors, different_provider, sources, failed, |_| true)
}

/// Like [`pick_besides`], for an agent that takes over from an approver whose session failed:
/// only one whose CLI keeps your plugins out of the approver's isolated session, which the role
/// needs, since nobody chose it to see them.
pub fn pick_stand_in(
    authors: &[WorkerRef],
    different_provider: bool,
    sources: Sources<'_>,
    failed: &[WorkerRef],
) -> Result<WorkerRef, NoApprover> {
    cheapest(authors, different_provider, sources, failed, |worker| {
        sources.workers.isolates(worker)
    })
}

fn cheapest(
    authors: &[WorkerRef],
    different_provider: bool,
    sources: Sources<'_>,
    failed: &[WorkerRef],
    fits: impl Fn(&WorkerRef) -> bool,
) -> Result<WorkerRef, NoApprover> {
    let family_of = |worker: &WorkerRef| family(worker, sources);
    let mut companies: Vec<String> = Vec::new();
    for author in authors {
        let Some(family) = family_of(author) else {
            return Err(NoApprover::UnknownAuthor(author.clone()));
        };
        let company = company_of(&family);
        if !companies.contains(&company) {
            companies.push(company);
        }
    }
    let mut best: Option<(f64, WorkerRef)> = None;
    for (price, candidate) in candidates(sources) {
        let Some(family) = family_of(&candidate) else {
            continue;
        };
        if failed
            .iter()
            .any(|failed| failed.vendor() == candidate.vendor())
            || !fits(&candidate)
        {
            continue;
        }
        if companies.contains(&company_of(&family))
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
        .ok_or(NoApprover::NoneElsewhere { companies })
}

/// Every model the pick may choose, with its price per million tokens in and out together.
/// A model without a known price costs infinity, so it's picked only when nothing priced fits.
fn candidates(sources: Sources<'_>) -> Vec<(f64, WorkerRef)> {
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
            .filter(|entry| entry.id.worker == available.worker)
            .collect();
        if !live.is_empty() {
            found.extend(
                live.into_iter()
                    .map(|entry| (entry_price(entry), entry.id.clone())),
            );
            continue;
        }
        let candidate = match worker.cheapest_model() {
            Some(model) => WorkerRef {
                worker: available.worker.clone(),
                model: Some(model),
            },
            None => available,
        };
        found.push((price(&candidate, sources), candidate));
    }
    found
}

fn price(worker: &WorkerRef, sources: Sources<'_>) -> f64 {
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

/// The worker's family as its agent reports it, else as the model lists know it. `None` when
/// neither knows.
/// The company behind `worker`'s model family, when its family is known.
pub fn company(worker: &WorkerRef, sources: Sources<'_>) -> Option<String> {
    family(worker, sources).map(|family| company_of(&family))
}

fn family(worker: &WorkerRef, sources: Sources<'_>) -> Option<String> {
    let known = |family: String| {
        let family = family.trim().to_lowercase();
        (!family.is_empty() && family != "unknown" && family != "from-model").then_some(family)
    };
    sources
        .workers
        .get(worker)
        .ok()
        .and_then(|agent| known(agent.family()))
        .or_else(|| {
            sources
                .models
                .and_then(|models| known(models.family(worker)))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::registry::{CostModel, ModelAccess, RegistryKind};
    use argonauts_testkit::services::StaticRegistry;
    use argonauts_testkit::{FakeDirectory, FakeWorker};

    fn r(text: &str) -> WorkerRef {
        text.parse().unwrap()
    }

    fn crew() -> FakeDirectory {
        let workers = FakeDirectory::new();
        // Haiku 4.5, priced above the other two; haiku itself now runs the cheaper Haiku 5.5.
        workers.add(
            FakeWorker::harness("claude-code", "claude").with_cheapest_model("claude-haiku-4-5"),
        );
        workers.add(FakeWorker::harness("codex", "gpt").with_cheapest_model("gpt-5.6-luna"));
        workers.add(
            FakeWorker::harness("antigravity-cli", "gemini")
                .with_cheapest_model("gemini-3.6-flash-low"),
        );
        workers
    }

    fn picked(
        authors: &[&str],
        workers: &FakeDirectory,
        models: Option<&StaticRegistry>,
    ) -> Result<WorkerRef, NoApprover> {
        let authors: Vec<WorkerRef> = authors.iter().map(|a| r(a)).collect();
        let sources = Sources {
            workers,
            models: models.map(|m| m as &dyn ModelRegistry),
        };
        pick(&authors, true, sources)
    }

    fn live(id: &str, family: &str, input: f64, output: f64) -> ModelEntry {
        ModelEntry {
            id: r(id),
            family: family.to_owned(),
            kind: RegistryKind::Harness,
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
            Ok(r("codex/gpt-5.6-luna"))
        );
        assert_eq!(
            picked(&["codex/gpt-6-sol"], &workers, None),
            Ok(r("antigravity-cli/gemini-3.6-flash-low"))
        );
        assert_eq!(
            picked(&["codex/gpt-6-sol", "antigravity-cli"], &workers, None),
            Ok(r("claude-code/claude-haiku-4-5"))
        );
    }

    #[test]
    fn the_live_model_lists_win_over_each_agents_cheapest() {
        let workers = crew();
        let models = StaticRegistry::default();
        models.add(live("antigravity-cli/gemini-tiny", "gemini", 0.01, 0.02));
        models.add(live("antigravity-cli/gemini-big", "gemini", 5.0, 20.0));
        let mut retired = live("codex/gpt-old", "gpt", 0.001, 0.001);
        retired.retired = true;
        models.add(retired);
        assert_eq!(
            picked(&["claude-code/opus"], &workers, Some(&models)),
            Ok(r("antigravity-cli/gemini-tiny"))
        );
    }

    #[test]
    fn one_company_is_one_company_across_its_families() {
        let workers = FakeDirectory::new();
        workers.add(FakeWorker::harness("codex", "gpt"));
        workers.add(FakeWorker::endpoint("openrouter/gpt-oss-120b", "gpt-oss"));
        assert_eq!(
            picked(&["codex"], &workers, None),
            Err(NoApprover::NoneElsewhere {
                companies: vec!["openai".to_owned()]
            })
        );
    }

    #[test]
    fn nothing_from_elsewhere_or_an_unknown_author_names_why() {
        let workers = FakeDirectory::new();
        workers.add(FakeWorker::harness("claude-code", "claude"));
        let none = picked(&["claude-code/opus"], &workers, None).unwrap_err();
        assert!(none.reason().contains("another provider than Anthropic"));
        assert!(none.warning(false).starts_with("No model"));
        assert!(
            none.warning(false)
                .contains("pick an approver in /approvals")
        );
        assert!(!none.warning(false).contains("approvals.approver"));
        // Full auto never asks you, so its warning only says what gives it an approver.
        let autopilot = none.warning(true);
        assert!(!autopilot.contains("asks you"), "{autopilot}");
        assert!(
            autopilot.contains(". Pick an approver in /approvals"),
            "{autopilot}"
        );
        let unknown = picked(&["mystery"], &workers, None).unwrap_err();
        assert_eq!(unknown, NoApprover::UnknownAuthor(r("mystery")));
        // A worker the app has no name for keeps its spelling.
        let unknown = NoApprover::UnknownAuthor(r("my-cli/fast"));
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
        assert_eq!(picked(&[], &crew(), None), Ok(r("codex/gpt-5.6-luna")));
    }
}
