//! Which replacements a stopped card may go to: the most specific list that applies, every
//! candidate filtered by the card's rules, health, quota, and cost, then ordered for the
//! strategy (`first`, `spread`, or `race`).

use std::cmp::Ordering;
use std::collections::HashMap;

use argonauts_core::events::card::CardSpec;
use argonauts_core::limits::{
    Candidate, CandidateQuery, CostEstimate, HealthBoard, HealthState, QuotaBoard, QuotaReading,
    QuotaStatus, Shortlist, TrackRecord, TrackRecords,
};
use argonauts_core::names::{RoleName, VendorId, WorkerRef};
use argonauts_core::registry::{ModelAccess, ModelRegistry};
use argonauts_core::settings::Settings;
use argonauts_core::settings::handoff::{AppliedHandoff, FallbackEntry, HandoffStrategy};
use globset::{Glob, GlobSetBuilder};
use time::OffsetDateTime;

use super::{FreeSlots, applied, entry_rules, task_type};
use crate::health::{DisableScope, disable_scope};

/// What the planner reads to judge candidates.
pub struct Sources<'a> {
    pub settings: &'a Settings,
    pub registry: &'a dyn ModelRegistry,
    pub quota: &'a dyn QuotaBoard,
    pub health: &'a dyn HealthBoard,
    pub records: &'a dyn TrackRecords,
    pub slots: Option<&'a dyn FreeSlots>,
}

/// Why a candidate was left out, for the handoff card's "relax the rule" option.
#[derive(Debug, Clone, PartialEq)]
pub enum Dropped {
    Down,
    /// Back from an outage and under test. Its one test card is new work, never a stopped card
    /// handed over, so a batch of stopped cards can't all land on it at once.
    Testing,
    /// Disabled for new cards, such as after a broken CLI update.
    Disabled(String),
    OutOfQuota,
    Retired,
    /// Such as a reviewer of the author's family.
    BreaksRule(String),
    /// A paid API without `allow_api_billing`: offered to you, never taken by auto.
    PaidApi,
    OverBudgetCap,
}

/// The token counts a cost estimate assumes for the rest of a card, until track records carry
/// each kind's typical spend.
const ESTIMATE_INPUT_TOKENS: u64 = 400_000;
const ESTIMATE_OUTPUT_TOKENS: u64 = 40_000;

/// Below this share of quota left, a candidate may run out before the card is done.
const LOW_QUOTA: f64 = 0.15;

pub fn shortlist(query: &CandidateQuery, sources: &Sources<'_>) -> Shortlist {
    let rules = applied(sources.settings, query.role, &query.stopped.vendor());
    let mut candidates: Vec<Candidate> = entries(query, &rules, sources)
        .iter()
        .map(|entry| candidate(entry, query, &rules, sources))
        .filter(|candidate| check(candidate, query, sources).is_ok())
        .collect();
    if rules.strategy == HandoffStrategy::Spread {
        spread(&mut candidates, sources);
    }
    let race_count = match rules.strategy {
        HandoffStrategy::Race => rules.race_count.max(1),
        HandoffStrategy::First | HandoffStrategy::Spread => 1,
    };
    Shortlist {
        strategy: rules.strategy,
        race_count,
        candidates,
    }
}

/// Whether `candidate` stays in, and if not, why. A paid API stays, marked `needs_confirm`;
/// [`auto_check`] is what keeps auto mode off it.
pub fn check(
    candidate: &Candidate,
    query: &CandidateQuery,
    sources: &Sources<'_>,
) -> Result<(), Dropped> {
    let worker = &candidate.worker;
    if sources
        .settings
        .agents
        .get(&worker.worker)
        .is_some_and(|agent| !agent.enabled)
    {
        return Err(Dropped::Disabled(format!(
            "agents.{}.enabled is false",
            worker.worker
        )));
    }
    let vendor = worker.vendor();
    let health = sources.health.state(&vendor);
    match health.state {
        HealthState::Down => return Err(Dropped::Down),
        HealthState::Testing => return Err(Dropped::Testing),
        HealthState::Healthy | HealthState::Degraded => {}
    }
    if let Some(why) = health.disabled {
        let covered = match disable_scope(&why) {
            DisableScope::Every => true,
            DisableScope::ApiKey => sources
                .registry
                .get(worker)
                .is_some_and(|entry| entry.access == ModelAccess::ApiKey),
        };
        if covered {
            return Err(Dropped::Disabled(why));
        }
    }
    if sources
        .quota
        .get(&vendor)
        .is_some_and(|reading| out_of_quota(&reading, OffsetDateTime::now_utc()))
    {
        return Err(Dropped::OutOfQuota);
    }
    if sources
        .registry
        .get(worker)
        .is_some_and(|entry| entry.retired)
    {
        return Err(Dropped::Retired);
    }
    let family = sources.registry.family(worker);
    if query.exclude_families.contains(&family) {
        return Err(Dropped::BreaksRule(format!(
            "{worker} is of the {family} family, which this card excludes"
        )));
    }
    if query.role == RoleName::Reviewer {
        if let Some(owner) = &query.card.owner
            && sources.registry.family(owner) == family
        {
            return Err(Dropped::BreaksRule(format!(
                "{worker} is of the author's family ({family}), so it can't review"
            )));
        }
        if query.card.reviewers.contains(worker) {
            return Err(Dropped::BreaksRule(format!(
                "{worker} already reviews this card"
            )));
        }
    }
    if query.role == RoleName::Owner
        && let Some(path) = hidden_owned_path(sources.settings, worker, &query.card)
    {
        return Err(Dropped::BreaksRule(format!(
            "{worker} can't see {path}, which this card owns"
        )));
    }
    let rules = applied(sources.settings, query.role, &query.stopped.vendor());
    let cap = entry_rules(&query.fallback, worker)
        .or_else(|| entry_rules(&rules.candidates, worker))
        .and_then(|rules| rules.max_budget_usd);
    if let (Some(cap), CostEstimate::Usd(cost)) = (cap, candidate.cost)
        && cost > cap
    {
        return Err(Dropped::OverBudgetCap);
    }
    Ok(())
}

/// An owned path that `agents.<worker>.exclude_paths` hides whole, where a card without owned
/// paths owns the whole repo. An exclude that hides only part of it still lets the agent own
/// the card.
fn hidden_owned_path(settings: &Settings, worker: &WorkerRef, card: &CardSpec) -> Option<String> {
    let excluded = &settings.agents.get(&worker.worker)?.exclude_paths;
    let mut set = GlobSetBuilder::new();
    for pattern in excluded {
        // A pattern that is no glob hides nothing.
        if let Ok(glob) = Glob::new(pattern) {
            set.add(glob);
        }
    }
    let set = set.build().ok()?;
    let whole_repo = [String::from("**")];
    let owned = if card.owned_paths.is_empty() {
        &whole_repo[..]
    } else {
        &card.owned_paths[..]
    };
    owned
        .iter()
        .find(|path| set.is_match(path.as_str()))
        .cloned()
}

/// Whether auto mode may take `candidate` without asking you.
pub fn auto_check(candidate: &Candidate) -> Result<(), Dropped> {
    if candidate.needs_confirm {
        return Err(Dropped::PaidApi);
    }
    Ok(())
}

/// A rejected reading counts until its reset has passed.
pub fn out_of_quota(reading: &QuotaReading, now: OffsetDateTime) -> bool {
    reading.status == QuotaStatus::Rejected && reading.resets_at.is_none_or(|at| at > now)
}

/// The list to judge: your pick, else the most specific settings list, else the crew. With
/// `all_valid`, every other worker the crew and the registry know follows. The stopped worker
/// itself is never a candidate.
fn entries(
    query: &CandidateQuery,
    rules: &AppliedHandoff,
    sources: &Sources<'_>,
) -> Vec<FallbackEntry> {
    let crew = || {
        sources
            .settings
            .crew
            .iter()
            .cloned()
            .map(FallbackEntry::Worker)
    };
    let mut list: Vec<FallbackEntry> = if !query.fallback.is_empty() {
        query.fallback.clone()
    } else if !rules.candidates.is_empty() {
        rules.candidates.clone()
    } else {
        crew().collect()
    };
    if query.all_valid {
        list.extend(rules.candidates.iter().cloned());
        list.extend(crew());
        list.extend(
            sources
                .registry
                .list()
                .into_iter()
                .filter(|entry| !entry.retired)
                .map(|entry| FallbackEntry::Worker(entry.id)),
        );
    }
    let mut seen: Vec<WorkerRef> = vec![query.stopped.clone()];
    list.retain(|entry| {
        let fresh = !seen.contains(entry.worker());
        if fresh {
            seen.push(entry.worker().clone());
        }
        fresh
    });
    list
}

fn candidate(
    entry: &FallbackEntry,
    query: &CandidateQuery,
    rules: &AppliedHandoff,
    sources: &Sources<'_>,
) -> Candidate {
    let worker = entry.worker().clone();
    let access = sources.registry.get(&worker).map(|entry| entry.access);
    let reading = sources.quota.get(&worker.vendor());
    let quota_left = match access {
        Some(ModelAccess::ApiKey | ModelAccess::Local) => None,
        Some(ModelAccess::Subscription) | None => reading
            .as_ref()
            .and_then(|reading| reading.used)
            .map(|used| (1.0 - used).clamp(0.0, 1.0)),
    };
    let cost = sources
        .registry
        .estimate(&worker, ESTIMATE_INPUT_TOKENS, ESTIMATE_OUTPUT_TOKENS);
    let entry_allows = match entry {
        FallbackEntry::WithRules(rules) => rules.allow_api_billing,
        FallbackEntry::Worker(_) => false,
    };
    let needs_confirm =
        access == Some(ModelAccess::ApiKey) && !(rules.allow_api_billing || entry_allows);
    let warned = reading
        .as_ref()
        .is_some_and(|reading| reading.status == QuotaStatus::Warning);
    let needed = match cost {
        CostEstimate::Quota(share) => share,
        CostEstimate::Usd(_) | CostEstimate::Free | CostEstimate::Unknown => LOW_QUOTA,
    };
    let may_run_out = warned || quota_left.is_some_and(|left| left < needed);
    Candidate {
        quota_text: quota_text(access, reading.as_ref(), quota_left),
        track_record: sources.records.get(&worker, task_type(&query.card)),
        worker,
        quota_left,
        cost,
        needs_confirm,
        may_run_out,
    }
}

/// Such as `62%`, `API key`, or `local`.
fn quota_text(
    access: Option<ModelAccess>,
    reading: Option<&QuotaReading>,
    left: Option<f64>,
) -> String {
    match access {
        Some(ModelAccess::ApiKey) => return "API key".to_owned(),
        Some(ModelAccess::Local) => return "local".to_owned(),
        Some(ModelAccess::Subscription) | None => {}
    }
    if let Some(left) = left {
        return format!("{:.0}%", left * 100.0);
    }
    match reading.map(|reading| reading.status) {
        Some(QuotaStatus::Allowed) => "ok".to_owned(),
        Some(QuotaStatus::Warning) => "low".to_owned(),
        Some(QuotaStatus::Rejected) => "out".to_owned(),
        Some(QuotaStatus::Unknown) | None => "?".to_owned(),
    }
}

/// Most quota left first, then the most free slots, then the best record. Unknowns go last,
/// and ties keep the list's order. Each vendor's slots are counted once.
fn spread(candidates: &mut Vec<Candidate>, sources: &Sources<'_>) {
    let mut free: HashMap<VendorId, Option<u32>> = HashMap::new();
    let mut ranked: Vec<(Option<u32>, Candidate)> = candidates
        .drain(..)
        .map(|candidate| {
            let slots = *free
                .entry(candidate.worker.vendor())
                .or_insert_with_key(|vendor| sources.slots.and_then(|slots| slots.free(vendor)));
            (slots, candidate)
        })
        .collect();
    ranked.sort_by(|(a_slots, a), (b_slots, b)| {
        descending(a.quota_left, b.quota_left)
            .then_with(|| b_slots.cmp(a_slots))
            .then_with(|| descending(pass_rate(a.track_record), pass_rate(b.track_record)))
    });
    candidates.extend(ranked.into_iter().map(|(_, candidate)| candidate));
}

/// Spreads one vendor's stopped cards: candidates that won't run out come first, then the ones
/// holding the fewest of the run's handed-off cards. Ties keep the shortlist's order.
pub fn balance(candidates: &mut [Candidate], handed_to: impl Fn(&WorkerRef) -> u32) {
    candidates
        .sort_by_cached_key(|candidate| (candidate.may_run_out, handed_to(&candidate.worker)));
}

fn pass_rate(record: Option<TrackRecord>) -> Option<f64> {
    record
        .filter(|record| record.of > 0)
        .map(|record| f64::from(record.passed) / f64::from(record.of))
}

fn descending(a: Option<f64>, b: Option<f64>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => b.total_cmp(&a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use argonauts_core::settings::handoff::{FallbackRules, VendorReplacement};

    use argonauts_core::limits::HealthSignal;

    use super::*;
    use crate::handoff::testing::{Fixture, api_model, entry, model, quota, worker};
    use crate::health::billing_disable;

    fn workers(shortlist: &Shortlist) -> Vec<String> {
        shortlist
            .candidates
            .iter()
            .map(|candidate| candidate.worker.to_string())
            .collect()
    }

    fn list(names: &[&str]) -> Vec<FallbackEntry> {
        names.iter().map(|name| entry(name)).collect()
    }

    #[test]
    fn shortlist_takes_most_specific_list() {
        let mut fixture = Fixture::new();
        let stopped = "claude-code/opus";
        let vendor = VendorId("claude-code".to_owned());
        let settings = fixture.settings_mut();
        settings.handoff.fallback = list(&["w-top-fallback"]);
        settings.handoff.replace.insert(
            vendor.clone(),
            VendorReplacement {
                with: list(&["w-top-replace"]),
                ..VendorReplacement::default()
            },
        );
        let role = settings.roles.entry(RoleName::Owner).or_default();
        role.handoff.fallback = list(&["w-role-fallback"]);
        role.handoff.replace.insert(
            vendor.clone(),
            VendorReplacement {
                with: list(&["w-role-replace"]),
                ..VendorReplacement::default()
            },
        );
        let planner = fixture.planner();
        let mut query = fixture.query(stopped);
        let picked = |query: &CandidateQuery| workers(&shortlist(query, &planner.sources()));

        query.fallback = list(&["w-your-pick"]);
        assert_eq!(picked(&query), ["w-your-pick"]);
        query.fallback.clear();
        assert_eq!(picked(&query), ["w-role-replace"]);

        // Emptying each level in turn falls through to the next.
        type EmptyLevel = fn(&mut Settings, &VendorId);
        let steps: [(&str, EmptyLevel); 4] = [
            ("w-role-fallback", |s, v| {
                s.roles
                    .entry(RoleName::Owner)
                    .or_default()
                    .handoff
                    .replace
                    .remove(v);
            }),
            ("w-top-replace", |s, _| {
                s.roles
                    .entry(RoleName::Owner)
                    .or_default()
                    .handoff
                    .fallback
                    .clear();
            }),
            ("w-top-fallback", |s, v| {
                s.handoff.replace.remove(v);
            }),
            ("claude-code", |s, _| s.handoff.fallback.clear()),
        ];
        for (expected, empty_one) in steps {
            empty_one(fixture.settings_mut(), &vendor);
            let planner = fixture.planner();
            let first = workers(&shortlist(&query, &planner.sources()));
            if expected == "claude-code" {
                // Nothing set: every healthy crew member but the stopped worker.
                assert_eq!(first, ["claude-code", "codex", "antigravity-cli"]);
            } else {
                assert_eq!(first, [expected]);
            }
        }

        // Another vendor's replacement list never applies to this stop.
        let settings = fixture.settings_mut();
        settings.handoff.replace.insert(
            VendorId("codex".to_owned()),
            VendorReplacement {
                with: list(&["w-codex-only"]),
                ..VendorReplacement::default()
            },
        );
        let planner = fixture.planner();
        assert!(!picked_with(&planner, &query).contains(&"w-codex-only".to_owned()));
    }

    fn picked_with(planner: &crate::handoff::Planner, query: &CandidateQuery) -> Vec<String> {
        workers(&shortlist(query, &planner.sources()))
    }

    #[test]
    fn an_agent_you_turned_off_is_never_a_candidate() {
        let mut fixture = Fixture::new();
        let settings = fixture.settings_mut();
        settings.handoff.fallback = list(&["codex", "opencode", "antigravity-cli"]);
        settings
            .agents
            .entry("opencode".to_owned())
            .or_default()
            .enabled = false;
        let planner = fixture.planner();
        let query = fixture.query("claude-code/opus");
        assert_eq!(picked_with(&planner, &query), ["codex", "antigravity-cli"]);
    }

    #[test]
    fn candidates_are_filtered_by_health_quota_and_rules() {
        let mut fixture = Fixture::new();
        fixture.registry.add(model("codex", "gpt-5", "gpt"));
        fixture
            .registry
            .add(model("antigravity-cli", "gemini-3", "gemini"));
        fixture
            .registry
            .add(model("claude-code", "sonnet", "claude"));
        fixture.registry.add(model("claude-code", "opus", "claude"));
        let mut retired = model("opencode", "old", "glm");
        retired.retired = true;
        fixture.registry.add(retired);
        fixture.settings_mut().handoff.fallback = list(&[
            "codex/gpt-5",
            "antigravity-cli/gemini-3",
            "claude-code/sonnet",
            "opencode/old",
        ]);
        let query = fixture.query("claude-code/opus");
        let planner = fixture.planner();
        assert_eq!(
            picked_with(&planner, &query),
            [
                "codex/gpt-5",
                "antigravity-cli/gemini-3",
                "claude-code/sonnet"
            ]
        );

        fixture.health.set(
            &VendorId("codex".to_owned()),
            HealthState::Down,
            Some("overloaded"),
        );
        fixture
            .quota
            .observe(quota("antigravity-cli", QuotaStatus::Rejected, Some(1.0)));
        assert_eq!(picked_with(&planner, &query), ["claude-code/sonnet"]);

        // A reviewer never gets the author's family, nor a seat another reviewer holds.
        let mut review = fixture.query("claude-code/opus");
        review.role = RoleName::Reviewer;
        review.card.owner = Some(worker("claude-code/opus"));
        assert!(picked_with(&planner, &review).is_empty());
        let sources = planner.sources();
        let sonnet = candidate(
            &entry("claude-code/sonnet"),
            &review,
            &AppliedHandoff::default(),
            &sources,
        );
        assert!(matches!(
            check(&sonnet, &review, &sources),
            Err(Dropped::BreaksRule(_))
        ));

        // Excluded families go too.
        let mut excluded = fixture.query("claude-code/opus");
        excluded.exclude_families = vec!["claude".to_owned()];
        assert!(picked_with(&planner, &excluded).is_empty());
    }

    #[test]
    fn an_owner_must_see_what_the_card_owns() {
        let mut fixture = Fixture::new();
        fixture.registry.add(model("codex", "gpt-5", "gpt"));
        fixture.settings_mut().handoff.fallback = list(&["codex/gpt-5"]);
        let mut query = fixture.query("claude-code/opus");
        let exclude = |fixture: &mut Fixture, paths: &[&str]| {
            let codex = fixture.settings_mut().agents.entry("codex".to_owned());
            codex.or_default().exclude_paths = paths.iter().map(|p| (*p).to_owned()).collect();
        };

        // Hiding part of the repo still lets it own a card that owns the whole repo.
        exclude(&mut fixture, &["secrets/**"]);
        assert_eq!(picked_with(&fixture.planner(), &query), ["codex/gpt-5"]);

        exclude(&mut fixture, &["**"]);
        assert!(picked_with(&fixture.planner(), &query).is_empty());

        // An owned path it can't see rules it out; a reviewer reads, so it stays.
        exclude(&mut fixture, &["src/**"]);
        query.card.owned_paths = vec!["src/api/**".to_owned()];
        assert!(picked_with(&fixture.planner(), &query).is_empty());
        query.card.owned_paths = vec!["docs/**".to_owned()];
        assert_eq!(picked_with(&fixture.planner(), &query), ["codex/gpt-5"]);
        query.card.owned_paths = vec!["src/api/**".to_owned()];
        query.role = RoleName::Reviewer;
        assert_eq!(picked_with(&fixture.planner(), &query), ["codex/gpt-5"]);
    }

    #[test]
    fn testing_and_billing_capped_vendors_are_dropped() {
        let mut fixture = Fixture::new();
        fixture.registry.add(model("codex", "gpt-5", "gpt"));
        fixture
            .registry
            .add(api_model("openrouter", "kimi", "kimi", 1.0));
        fixture.registry.add(model("openrouter", "glm", "glm"));
        fixture.settings_mut().handoff.fallback =
            list(&["codex/gpt-5", "openrouter/kimi", "openrouter/glm"]);
        let query = fixture.query("claude-code/opus");
        let planner = fixture.planner();
        assert_eq!(
            picked_with(&planner, &query),
            ["codex/gpt-5", "openrouter/kimi", "openrouter/glm"]
        );

        // A vendor under test takes no stopped card, and a billing cap keeps only the vendor's
        // API-key models away.
        fixture.health.set(
            &VendorId("codex".to_owned()),
            HealthState::Testing,
            Some("the cool-down is over"),
        );
        fixture.health.observe(HealthSignal::AdapterBroken {
            vendor: VendorId("openrouter".to_owned()),
            detail: billing_disable(),
        });
        assert_eq!(picked_with(&planner, &query), ["openrouter/glm"]);
        let sources = planner.sources();
        let judged = |name: &str| {
            let candidate = candidate(&entry(name), &query, &AppliedHandoff::default(), &sources);
            check(&candidate, &query, &sources)
        };
        assert_eq!(judged("codex/gpt-5"), Err(Dropped::Testing));
        assert!(matches!(
            judged("openrouter/kimi"),
            Err(Dropped::Disabled(_))
        ));
    }

    #[test]
    fn fallback_entries_with_rules() {
        let mut fixture = Fixture::new();
        fixture
            .registry
            .add(api_model("openrouter", "glm-5", "glm", 10.0));
        fixture
            .registry
            .add(api_model("openrouter", "kimi", "kimi", 1.0));
        fixture.registry.add(model("codex", "gpt-5", "gpt"));
        let capped = |worker_name: &str, cap: f64, allow: bool| {
            FallbackEntry::WithRules(FallbackRules {
                worker: worker(worker_name),
                max_budget_usd: Some(cap),
                instructions: Some("Keep to the card's paths.".to_owned()),
                read_only: false,
                allow_api_billing: allow,
            })
        };
        // glm-5 costs about $4.40 for the estimate, over its $2 cap; kimi about $0.44.
        fixture.settings_mut().handoff.fallback = vec![
            capped("openrouter/glm-5", 2.0, true),
            capped("openrouter/kimi", 2.0, true),
            entry("codex/gpt-5"),
        ];
        let planner = fixture.planner();
        let query = fixture.query("claude-code/opus");
        let shortlist = shortlist(&query, &planner.sources());
        assert_eq!(workers(&shortlist), ["openrouter/kimi", "codex/gpt-5"]);
        let kimi = &shortlist.candidates[0];
        assert!(!kimi.needs_confirm, "the entry itself allows API billing");
        assert_eq!(kimi.quota_text, "API key");
        assert!(matches!(kimi.cost, CostEstimate::Usd(usd) if usd < 2.0));
        let glm = candidate(
            &capped("openrouter/glm-5", 2.0, true),
            &query,
            &applied(
                &fixture.settings,
                RoleName::Owner,
                &VendorId("claude-code".to_owned()),
            ),
            &planner.sources(),
        );
        assert_eq!(
            check(&glm, &query, &planner.sources()),
            Err(Dropped::OverBudgetCap)
        );
    }

    #[test]
    fn api_billing_needs_confirm() {
        let mut fixture = Fixture::new();
        fixture
            .registry
            .add(api_model("openrouter", "kimi", "kimi", 1.0));
        fixture.registry.add(model("codex", "gpt-5", "gpt"));
        fixture.settings_mut().handoff.fallback = list(&["openrouter/kimi", "codex/gpt-5"]);
        let planner = fixture.planner();
        let query = fixture.query("claude-code/opus");
        let shortlist = shortlist(&query, &planner.sources());
        let kimi = &shortlist.candidates[0];
        assert!(kimi.needs_confirm, "moving to a paid API needs your OK");
        assert_eq!(auto_check(kimi), Err(Dropped::PaidApi));
        assert!(auto_check(&shortlist.candidates[1]).is_ok());

        fixture.settings_mut().handoff.allow_api_billing = true;
        let planner = fixture.planner();
        let allowed = super::shortlist(&query, &planner.sources());
        assert!(!allowed.candidates[0].needs_confirm);
        assert!(auto_check(&allowed.candidates[0]).is_ok());
    }

    #[test]
    fn spread_balances_by_quota_and_record() {
        let mut fixture = Fixture::new();
        for (vendor, model_id, family) in [
            ("codex", "gpt-5", "gpt"),
            ("antigravity-cli", "gemini-3", "gemini"),
            ("opencode", "glm", "glm"),
            ("qwen", "coder", "qwen"),
        ] {
            fixture.registry.add(model(vendor, model_id, family));
        }
        let settings = fixture.settings_mut();
        settings.handoff.strategy = Some(HandoffStrategy::Spread);
        settings.handoff.fallback = list(&[
            "codex/gpt-5",
            "antigravity-cli/gemini-3",
            "opencode/glm",
            "qwen/coder",
        ]);
        fixture
            .quota
            .observe(quota("codex", QuotaStatus::Allowed, Some(0.9)));
        fixture
            .quota
            .observe(quota("antigravity-cli", QuotaStatus::Allowed, Some(0.3)));
        fixture
            .quota
            .observe(quota("opencode", QuotaStatus::Allowed, Some(0.3)));
        let task = "build";
        fixture.records.set(
            worker("antigravity-cli/gemini-3"),
            task,
            TrackRecord { passed: 2, of: 6 },
        );
        fixture.records.set(
            worker("opencode/glm"),
            task,
            TrackRecord { passed: 5, of: 6 },
        );
        let planner = fixture.planner();
        let query = fixture.query("claude-code/opus");
        let shortlist = shortlist(&query, &planner.sources());
        assert_eq!(shortlist.strategy, HandoffStrategy::Spread);
        assert_eq!(shortlist.race_count, 1);
        // 70% left beats 10%; at 70% each, the better record wins; unknown quota goes last.
        assert_eq!(
            workers(&shortlist),
            [
                "opencode/glm",
                "antigravity-cli/gemini-3",
                "codex/gpt-5",
                "qwen/coder"
            ]
        );
        let first = &shortlist.candidates[0];
        assert_eq!(first.quota_text, "70%");
        assert_eq!(first.track_record, Some(TrackRecord { passed: 5, of: 6 }));
        let codex = &shortlist.candidates[2];
        assert!(codex.may_run_out, "10% left is low");
        assert!(!first.may_run_out);

        // Free slots break a tie in quota before records do.
        struct Slots;
        impl FreeSlots for Slots {
            fn free(&self, vendor: &VendorId) -> Option<u32> {
                Some(if vendor.0 == "antigravity-cli" { 3 } else { 0 })
            }
        }
        let planner = fixture.planner().with_slots(Arc::new(Slots));
        assert_eq!(
            workers(&super::shortlist(&query, &planner.sources()))[..2],
            ["antigravity-cli/gemini-3", "opencode/glm"]
        );
    }

    #[test]
    fn race_returns_race_count() {
        let mut fixture = Fixture::new();
        let settings = fixture.settings_mut();
        settings.handoff.strategy = Some(HandoffStrategy::Race);
        settings.handoff.race_count = Some(3);
        let planner = fixture.planner();
        let shortlist = shortlist(&fixture.query("claude-code/opus"), &planner.sources());
        assert_eq!(shortlist.strategy, HandoffStrategy::Race);
        assert_eq!(shortlist.race_count, 3);
        assert_eq!(
            workers(&shortlist),
            ["claude-code", "codex", "antigravity-cli"]
        );
    }

    #[test]
    fn all_valid_lists_every_valid_worker() {
        let mut fixture = Fixture::new();
        fixture.registry.add(model("opencode", "glm", "glm"));
        fixture.settings_mut().handoff.fallback = list(&["codex"]);
        let planner = fixture.planner();
        let mut query = fixture.query("claude-code/opus");
        assert_eq!(picked_with(&planner, &query), ["codex"]);
        query.all_valid = true;
        assert_eq!(
            picked_with(&planner, &query),
            ["codex", "claude-code", "antigravity-cli", "opencode/glm"]
        );
    }
}
