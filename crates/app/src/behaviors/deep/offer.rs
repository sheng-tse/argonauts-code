//! Summaries' questions as the app fills them: how many actions each answer covers,
//! across how many projects and CLIs, how long it takes and the tokens a hosted model would use,
//! and who may summarize.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::time::Duration;

use argonauts_core::deep_map::{
    CliCount, DeepOffer, DeepScope, EmbedderNote, EmbedderOffer, OfferPlace, ScopeOffer,
    SummarizerOffer,
};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::registry::ModelRegistry;
use time::OffsetDateTime;

use super::action::DeepAction;
use super::choose::{Candidate, Place};
use super::embed::{KeyedEndpoint, NOMIC, NOMIC_MEGABYTES, OPENAI_EMBEDDING};
use super::prompt::{PER_REQUEST, PROMPT};

/// The days `The last … days` covers.
pub(crate) const RECENT_DAYS: u32 = 7;

/// A local model writes about four summaries a second: 562 actions took Qwen3 Coder 30B on a
/// laptop 110 seconds, 480 of them distinct.
const LOCAL_SECONDS_PER_TEXT: f64 = 0.23;
/// A CLI starts a session for each request, which takes longer than the summaries.
const HOSTED_SECONDS_PER_REQUEST: f64 = 12.0;
/// What a summary line takes to write, in tokens.
const SUMMARY_TOKENS: u64 = 12;

/// Whether `action` is one `scope` covers.
pub(crate) fn covers(scope: &DeepScope, action: &DeepAction, now: OffsetDateTime) -> bool {
    match scope {
        DeepScope::Everything => true,
        DeepScope::Project { name } => &action.project == name,
        DeepScope::Days { days } => action.at >= now - time::Duration::days(i64::from(*days)),
        DeepScope::Since { at } => action.at >= *at,
    }
}

/// How long and how many tokens summarizing `actions` takes, counting each distinct text once.
pub(crate) fn estimate(actions: &[&DeepAction], local: bool) -> (Duration, u64) {
    let mut texts: HashSet<String> = HashSet::new();
    let mut tokens = 0u64;
    for action in actions {
        let text = action.text();
        if texts.insert(super::action::text_key(&text)) {
            tokens += text.chars().count() as u64 / 4 + SUMMARY_TOKENS;
        }
    }
    let distinct = texts.len();
    let requests = distinct.div_ceil(PER_REQUEST);
    tokens += requests as u64 * (PROMPT.chars().count() as u64 / 4);
    let seconds = if local {
        distinct as f64 * LOCAL_SECONDS_PER_TEXT
    } else {
        requests as f64 * HOSTED_SECONDS_PER_REQUEST
    };
    (Duration::from_secs_f64(seconds), tokens)
}

/// One answer to what to summarize: the actions `covered`, their projects and CLIs, how long
/// auto's summarizer takes (`local` or not) and the tokens a hosted one would use.
pub(crate) fn scope_offer(
    scope: DeepScope,
    covered: &[&DeepAction],
    local: bool,
    names: &DisplayNames,
) -> ScopeOffer {
    let projects: BTreeSet<&str> = covered.iter().map(|a| a.project.as_str()).collect();
    let mut by_cli: BTreeMap<&str, usize> = BTreeMap::new();
    for action in covered {
        *by_cli.entry(action.cli.as_str()).or_insert(0) += 1;
    }
    let (took, _) = estimate(covered, local);
    let (_, tokens) = estimate(covered, false);
    ScopeOffer {
        scope,
        actions: covered.len(),
        projects: projects.len(),
        by_cli: by_cli
            .into_iter()
            .map(|(cli, actions)| CliCount {
                cli: cli.to_owned(),
                name: names.cli(cli),
                actions,
            })
            .collect(),
        seconds: took.as_secs(),
        tokens,
    }
}

/// The offer for `recorded`, with you in `project` (when you're in one) and `candidates` from
/// the choice, auto's first.
pub(crate) fn offer(
    recorded: &[DeepAction],
    project: Option<&str>,
    candidates: &[Candidate],
    registry: &dyn ModelRegistry,
    names: &DisplayNames,
    embedder: EmbedderNote,
    now: OffsetDateTime,
) -> DeepOffer {
    let auto_local = candidates.first().is_some_and(Candidate::local);
    let mut scopes = vec![DeepScope::Everything];
    if let Some(project) = project {
        scopes.push(DeepScope::Project {
            name: project.to_owned(),
        });
    }
    scopes.push(DeepScope::Days { days: RECENT_DAYS });
    let scopes = scopes
        .into_iter()
        .map(|scope| {
            let covered: Vec<&DeepAction> =
                recorded.iter().filter(|a| covers(&scope, a, now)).collect();
            scope_offer(scope, &covered, auto_local, names)
        })
        .collect();
    let summarizers = candidates
        .iter()
        .take(super::choose::LISTED)
        .map(|candidate| SummarizerOffer {
            model: candidate.model.clone(),
            name: candidate.name.clone(),
            short: candidate.short.clone(),
            place: match &candidate.place {
                Place::ThisMachine(local) => OfferPlace::ThisMachine {
                    bytes: local.bytes,
                    fit: local.fit,
                },
                Place::Hosted(hosted) => {
                    let (plan, provider) =
                        super::gather::plan_and_provider(registry, &hosted.worker);
                    OfferPlace::Plan {
                        cli: hosted.worker.agent.clone(),
                        plan,
                        provider,
                    }
                }
            },
        })
        .collect();
    DeepOffer {
        scopes,
        summarizers,
        embedder,
        mac: cfg!(target_os = "macos"),
        made_by: None,
        named: None,
        keyed_embedder: None,
    }
}

/// The embedder auto would use: Ollama's nomic-embed-text, downloaded once you say so, else
/// an OpenAI endpoint with your key, else word vectors.
pub(crate) fn embedder_note(
    ollama: Option<&(String, bool)>,
    keyed: Option<&KeyedEndpoint>,
    names: &DisplayNames,
) -> EmbedderNote {
    let name = format!("{NOMIC} on {}", names.cli("ollama"));
    match (ollama, keyed) {
        (Some((_, true)), _) => EmbedderNote::Ready { name },
        (Some((_, false)), _) => EmbedderNote::Download {
            name,
            megabytes: NOMIC_MEGABYTES,
        },
        (None, keyed) => match keyed_embedder(keyed) {
            Some(keyed) => EmbedderNote::Ready { name: keyed.name },
            None => EmbedderNote::Words,
        },
    }
}

/// The embedding model of the endpoint with your key, as the embedder's picker offers it.
pub(crate) fn keyed_embedder(keyed: Option<&KeyedEndpoint>) -> Option<EmbedderOffer> {
    let keyed = keyed?;
    Some(EmbedderOffer {
        model: argonauts_core::names::ModelRef::new(
            keyed.id.clone(),
            Some(OPENAI_EMBEDDING.to_owned()),
        ),
        name: format!(
            "{OPENAI_EMBEDDING} on {}",
            argonauts_core::registry::provider::provider_name(&keyed.id)
        ),
    })
}

#[cfg(test)]
mod tests {
    use argonauts_testkit::services::StaticRegistry;

    use super::super::action::sample::shell;
    use super::super::choose::{candidates, sample::page_models};
    use super::*;

    fn recorded(now: OffsetDateTime) -> Vec<DeepAction> {
        let mut all = Vec::new();
        for at in 0..30 {
            let mut action = shell("run-1", &format!("c{at}"), &format!("step {}", at % 20));
            action.at = now - time::Duration::days(if at < 12 { 2 } else { 30 });
            action.project = if at < 8 { "council" } else { "ledger" }.to_owned();
            action.cli = if at < 25 { "claude-code" } else { "codex" }.to_owned();
            all.push(action);
        }
        all
    }

    #[test]
    fn each_scope_counts_its_actions_projects_and_clis() {
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::days(400);
        let (locals, hosted) = page_models();
        let names = DisplayNames::default();
        let offered = candidates(&locals, &hosted, &names);
        let registry = StaticRegistry::default();
        let offer = offer(
            &recorded(now),
            Some("council"),
            &offered,
            &registry,
            &names,
            EmbedderNote::Words,
            now,
        );
        let counts: Vec<(usize, usize)> = offer
            .scopes
            .iter()
            .map(|scope| (scope.actions, scope.projects))
            .collect();
        assert_eq!(counts, [(30, 2), (8, 1), (12, 2)]);
        let everything = &offer.scopes[0];
        assert_eq!(everything.by_cli.len(), 2);
        assert_eq!(everything.by_cli[1].actions, 5);
        assert_eq!(offer.summarizers.len(), 4);
        assert!(offer.summarizers[0].local());
    }

    #[test]
    fn an_estimate_counts_each_distinct_text_once() {
        let now = OffsetDateTime::UNIX_EPOCH;
        let all = recorded(now);
        let refs: Vec<&DeepAction> = all.iter().collect();
        let (local, tokens) = estimate(&refs, true);
        assert!((local.as_secs_f64() - 20.0 * LOCAL_SECONDS_PER_TEXT).abs() < 1e-6);
        let (hosted, same) = estimate(&refs, false);
        assert_eq!(hosted.as_secs_f64(), 2.0 * HOSTED_SECONDS_PER_REQUEST);
        assert_eq!(tokens, same);
        assert!(tokens > 20 * SUMMARY_TOKENS);
    }

    #[test]
    fn the_embedder_is_ollamas_once_its_there_and_word_vectors_without_ollama() {
        let names = DisplayNames::default();
        let base = "http://127.0.0.1:11434/v1".to_owned();
        assert!(matches!(
            embedder_note(Some(&(base.clone(), true)), None, &names),
            EmbedderNote::Ready { .. }
        ));
        assert_eq!(
            embedder_note(Some(&(base, false)), None, &names),
            EmbedderNote::Download {
                name: "nomic-embed-text on Ollama".to_owned(),
                megabytes: 274
            }
        );
        assert_eq!(embedder_note(None, None, &names), EmbedderNote::Words);
        let keyed = KeyedEndpoint {
            id: "openai".to_owned(),
            base_url: "https://api.openai.com/v1".to_owned(),
            key_env: "OPENAI_API_KEY".to_owned(),
        };
        assert_eq!(
            embedder_note(None, Some(&keyed), &names),
            EmbedderNote::Ready {
                name: "text-embedding-3-small on OpenAI".to_owned()
            }
        );
        let offered = keyed_embedder(Some(&keyed)).unwrap();
        assert_eq!(offered.model.to_string(), "openai/text-embedding-3-small");
        assert_eq!(offered.name, "text-embedding-3-small on OpenAI");
        assert_eq!(keyed_embedder(None), None);
    }
}
