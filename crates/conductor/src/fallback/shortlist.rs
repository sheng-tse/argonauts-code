//! Which fallbacks a stopped task may go to: the most specific list that applies, every
//! candidate filtered by the task's rules, health, plan usage, cost, and whether its CLI would
//! start, then ordered for the strategy (`first`, `spread`, or `race`).

use std::cmp::Ordering;
use std::collections::HashMap;

use argonauts_core::events::task::TaskSpec;
use argonauts_core::limits::{
    Candidate, CandidateQuery, CostEstimate, HealthBoard, HealthState, ModelState, PlanUsageBoard,
    PlanUsageReading, PlanUsageStatus, Shortlist, TrackRecord, TrackRecords,
};
use argonauts_core::names::{AgentId, ModelRef, RoleName};
use argonauts_core::registry::{ModelAccess, ModelRegistry};
use argonauts_core::session_runner::{RunnerDirectory, model_run};
use argonauts_core::settings::Settings;
use argonauts_core::settings::fallback::{AppliedFallback, FallbackEntry, FallbackStrategy, Pick};
use globset::{Glob, GlobSetBuilder};
use time::OffsetDateTime;

use super::{FreeSlots, applied, entry_rules, task_type};
use crate::health::{DisableScope, disable_scope};

/// What the planner reads to judge candidates.
pub struct Sources<'a> {
    pub settings: &'a Settings,
    pub registry: &'a dyn ModelRegistry,
    pub plan_usage: &'a dyn PlanUsageBoard,
    pub health: &'a dyn HealthBoard,
    pub records: &'a dyn TrackRecords,
    pub slots: Option<&'a dyn FreeSlots>,
    /// Says which CLIs would refuse to start. Without it, every one is taken to start.
    pub workers: Option<&'a dyn RunnerDirectory>,
}

impl Sources<'_> {
    /// The model a session of `model` runs, as its runner names it where there are runners.
    pub fn runs(&self, model: &ModelRef) -> ModelRef {
        model_run(self.workers, self.settings, model)
    }
}

/// Why a candidate was left out, for the fallback question's "relax the rule" option.
#[derive(Debug, Clone, PartialEq)]
pub enum Dropped {
    Down,
    /// The model itself is out, while its CLI's other models may take work.
    ModelOut,
    /// Back from an outage and under test. Its one test task is new work, never a stopped task
    /// falling back, so a batch of stopped tasks can't all go to it at once.
    Testing,
    /// Disabled for new tasks, such as after a broken CLI update.
    Disabled(String),
    /// Its CLI would refuse to start, such as OpenCode with no model set.
    WontStart(String),
    OutOfPlanUsage,
    /// Its plan window is as full as your `limits.plan_usage` lets it get.
    AtPlanLimit,
    Retired,
    /// Such as a reviewer of the author's provider.
    BreaksRule(String),
    /// A paid API without `allow_api_billing`: offered to you, never taken by auto.
    PaidApi,
    OverLimitCap,
}

/// The token counts a cost estimate assumes for the rest of a task, until track records carry
/// each kind's typical spend.
const ESTIMATE_INPUT_TOKENS: u64 = 400_000;
const ESTIMATE_OUTPUT_TOKENS: u64 = 40_000;

/// Below this share of plan usage left, a candidate may run out before the task is done.
const LOW_PLAN_USAGE: f64 = 0.15;

pub fn shortlist(query: &CandidateQuery, sources: &Sources<'_>) -> Shortlist {
    let rules = applied(sources.settings, query.role, &query.stopped.agent_id());
    let mut candidates: Vec<Candidate> = entries(query, &rules, sources)
        .iter()
        .map(|entry| candidate(entry, query, &rules, sources))
        .filter(|candidate| check(candidate, query, sources).is_ok())
        .collect();
    if rules.strategy == FallbackStrategy::Spread {
        spread(&mut candidates, sources);
    }
    Shortlist {
        strategy: rules.strategy,
        race_count: rules.together,
        pick: rules.pick.unwrap_or(Pick::default_for(query.role)),
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
    let model = &candidate.worker;
    if sources
        .settings
        .agents
        .get(&model.agent)
        .is_some_and(|agent| !agent.enabled)
    {
        return Err(Dropped::Disabled(format!(
            "agents.{}.enabled is false",
            model.agent
        )));
    }
    let runner = sources.workers.and_then(|workers| workers.get(model).ok());
    if let Some(why) = runner.as_ref().and_then(|runner| runner.start_refusal()) {
        return Err(Dropped::WontStart(why));
    }
    // A worker takes over the task's build, which a model on a server, with no tools, can't do.
    if query.role == RoleName::Worker
        && runner.is_some_and(|runner| !runner.capabilities().can_build)
    {
        return Err(Dropped::WontStart(format!(
            "{model} has no tools, so it can't take over a build"
        )));
    }
    let agent_id = model.agent_id();
    let health = sources.health.state(&agent_id);
    match health.state {
        HealthState::Down => return Err(Dropped::Down),
        HealthState::Testing => return Err(Dropped::Testing),
        HealthState::Healthy | HealthState::Degraded => {}
    }
    match sources.health.model(model).state {
        ModelState::Out => return Err(Dropped::ModelOut),
        ModelState::Testing => return Err(Dropped::Testing),
        ModelState::Healthy | ModelState::Degraded => {}
    }
    if let Some(why) = health.disabled {
        let covered = match disable_scope(&why) {
            DisableScope::Every => true,
            DisableScope::ApiKey => sources
                .registry
                .get(model)
                .is_some_and(|entry| entry.access == ModelAccess::ApiKey),
        };
        if covered {
            return Err(Dropped::Disabled(why));
        }
    }
    if sources
        .plan_usage
        .get(&agent_id)
        .is_some_and(|reading| out_of_plan_usage(&reading, OffsetDateTime::now_utc()))
    {
        return Err(Dropped::OutOfPlanUsage);
    }
    if sources
        .plan_usage
        .get(&agent_id)
        .is_some_and(|reading| at_plan_limit(&reading, sources.settings, OffsetDateTime::now_utc()))
    {
        return Err(Dropped::AtPlanLimit);
    }
    if sources
        .registry
        .get(model)
        .is_some_and(|entry| entry.retired)
    {
        return Err(Dropped::Retired);
    }
    // The provider of the model it runs, so a bare CLI counts as the model it runs.
    let provider_of = |model: &ModelRef| sources.registry.provider(&sources.runs(model));
    let provider = provider_of(model);
    if query.exclude_providers.contains(&provider) {
        return Err(Dropped::BreaksRule(format!(
            "{model} is of the {provider} provider, which this task excludes"
        )));
    }
    if query.role == RoleName::Reviewer {
        if let Some(worker) = &query.task.worker
            && provider_of(worker) == provider
        {
            return Err(Dropped::BreaksRule(format!(
                "{model} is of the author's provider ({provider}), so it can't review"
            )));
        }
        if query.task.reviewers.contains(model) {
            return Err(Dropped::BreaksRule(format!(
                "{model} already reviews this task"
            )));
        }
    }
    if query.role == RoleName::Worker
        && let Some(path) = hidden_owned_path(sources.settings, model, &query.task)
    {
        return Err(Dropped::BreaksRule(format!(
            "{model} can't see {path}, which this task owns"
        )));
    }
    let rules = applied(sources.settings, query.role, &query.stopped.agent_id());
    let cap = entry_rules(&query.fallback, model)
        .or_else(|| entry_rules(&rules.candidates, model))
        .and_then(|rules| rules.max_spend_usd);
    if let (Some(cap), CostEstimate::Usd(cost)) = (cap, candidate.cost)
        && cost > cap
    {
        return Err(Dropped::OverLimitCap);
    }
    Ok(())
}

/// An owned path that `agents.<id>.exclude_paths` hides whole, where a task without owned
/// paths owns the whole repo. An exclude that hides only part of it still lets the agent own
/// the task.
fn hidden_owned_path(settings: &Settings, worker: &ModelRef, task: &TaskSpec) -> Option<String> {
    let excluded = &settings.agents.get(&worker.agent)?.exclude_paths;
    let mut set = GlobSetBuilder::new();
    for pattern in excluded {
        // A pattern that is no glob hides nothing.
        if let Ok(glob) = Glob::new(pattern) {
            set.add(glob);
        }
    }
    let set = set.build().ok()?;
    let whole_repo = [String::from("**")];
    let owned = if task.owned_paths.is_empty() {
        &whole_repo[..]
    } else {
        &task.owned_paths[..]
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
pub fn out_of_plan_usage(reading: &PlanUsageReading, now: OffsetDateTime) -> bool {
    reading.status == PlanUsageStatus::Rejected && reading.resets_at.is_none_or(|at| at > now)
}

/// Whether the reading's plan window is as full as your `limits.plan_usage` lets it get. It
/// counts until the window resets, as a refused one does. Never, when you set no limit.
pub fn at_plan_limit(reading: &PlanUsageReading, settings: &Settings, now: OffsetDateTime) -> bool {
    let Some(limit) = settings.limits.spend.plan_usage else {
        return false;
    };
    reading.used.is_some_and(|used| used >= limit) && reading.resets_at.is_none_or(|at| at > now)
}

/// The list to judge: your pick, else the most specific settings list, else the crew. With
/// `all_valid`, every other model the crew and the registry know follows. With `same_cli`, the
/// stopped CLI's other models follow whatever was listed, so a list of other CLIs alone still
/// leaves the work somewhere to go. The stopped one itself is never a candidate.
fn entries(
    query: &CandidateQuery,
    rules: &AppliedFallback,
    sources: &Sources<'_>,
) -> Vec<FallbackEntry> {
    let crew = || {
        sources
            .settings
            .crew
            .members()
            .into_iter()
            .map(FallbackEntry::Model)
    };
    let mut list: Vec<FallbackEntry> = if !query.fallback.is_empty() {
        query.fallback.clone()
    } else if !rules.candidates.is_empty() {
        rules.candidates.clone()
    } else {
        crew().collect()
    };
    if rules.same_cli && !query.all_valid {
        list.extend(
            sources
                .registry
                .list()
                .into_iter()
                .filter(|entry| !entry.retired && entry.id.agent == query.stopped.agent)
                .map(|entry| FallbackEntry::Model(entry.id)),
        );
    }
    if query.all_valid {
        list.extend(rules.candidates.iter().cloned());
        list.extend(crew());
        list.extend(
            sources
                .registry
                .list()
                .into_iter()
                .filter(|entry| !entry.retired)
                .map(|entry| FallbackEntry::Model(entry.id)),
        );
    }
    // Refs that run the same model are one candidate, and one that runs the stopped model is
    // none: it would only run that model again.
    let runs = |model: &ModelRef| sources.runs(model);
    let mut seen: Vec<ModelRef> = vec![runs(&query.stopped)];
    list.retain(|entry| {
        let model = runs(entry.model());
        let fresh = !seen.contains(&model);
        if fresh {
            seen.push(model);
        }
        fresh
    });
    list
}

fn candidate(
    entry: &FallbackEntry,
    query: &CandidateQuery,
    rules: &AppliedFallback,
    sources: &Sources<'_>,
) -> Candidate {
    let worker = entry.model().clone();
    let access = sources.registry.get(&worker).map(|entry| entry.access);
    let reading = sources.plan_usage.get(&worker.agent_id());
    let plan_usage_left = match access {
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
        FallbackEntry::Model(_) => false,
    };
    let needs_confirm =
        access == Some(ModelAccess::ApiKey) && !(rules.allow_api_billing || entry_allows);
    let warned = reading
        .as_ref()
        .is_some_and(|reading| reading.status == PlanUsageStatus::Warning);
    let needed = match cost {
        CostEstimate::PlanUsage(share) => share,
        CostEstimate::Usd(_) | CostEstimate::Free | CostEstimate::Unknown => LOW_PLAN_USAGE,
    };
    let may_run_out = warned || plan_usage_left.is_some_and(|left| left < needed);
    Candidate {
        plan_usage_text: plan_usage_text(access, reading.as_ref(), plan_usage_left),
        track_record: sources.records.get(&worker, task_type(&query.task)),
        worker,
        plan_usage_left,
        cost,
        needs_confirm,
        may_run_out,
    }
}

/// Such as `62%`, `API key`, or `local`.
fn plan_usage_text(
    access: Option<ModelAccess>,
    reading: Option<&PlanUsageReading>,
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
        Some(PlanUsageStatus::Allowed) => "ok".to_owned(),
        Some(PlanUsageStatus::Warning) => "low".to_owned(),
        Some(PlanUsageStatus::Rejected) => "out".to_owned(),
        Some(PlanUsageStatus::Unknown) | None => "?".to_owned(),
    }
}

/// Most plan usage left first, then the most free slots, then the best record. Unknowns go last,
/// and ties keep the list's order. Each agent's slots are counted once.
fn spread(candidates: &mut Vec<Candidate>, sources: &Sources<'_>) {
    let mut free: HashMap<AgentId, Option<u32>> = HashMap::new();
    let mut ranked: Vec<(Option<u32>, Candidate)> = candidates
        .drain(..)
        .map(|candidate| {
            let slots = *free
                .entry(candidate.worker.agent_id())
                .or_insert_with_key(|agent_id| {
                    sources.slots.and_then(|slots| slots.free(agent_id))
                });
            (slots, candidate)
        })
        .collect();
    ranked.sort_by(|(a_slots, a), (b_slots, b)| {
        descending(a.plan_usage_left, b.plan_usage_left)
            .then_with(|| b_slots.cmp(a_slots))
            .then_with(|| descending(pass_rate(a.track_record), pass_rate(b.track_record)))
    });
    candidates.extend(ranked.into_iter().map(|(_, candidate)| candidate));
}

/// Spreads one agent's stopped tasks: candidates that won't run out come first, then the ones
/// holding the fewest of the run's tasks that fell back. Ties keep the shortlist's order.
pub fn balance(candidates: &mut [Candidate], fell_back_to: impl Fn(&ModelRef) -> u32) {
    candidates
        .sort_by_cached_key(|candidate| (candidate.may_run_out, fell_back_to(&candidate.worker)));
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

    use argonauts_core::settings::crew::CrewRole;
    use argonauts_core::settings::fallback::{AgentFallbacks, FallbackRules, OrderStrategy};

    use argonauts_core::limits::HealthSignal;

    use super::*;
    use argonauts_testkit::FakeRunner;

    use crate::fallback::testing::{Fixture, api_model, entry, model, plan_usage, worker};
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
        let agent_id = AgentId("claude-code".to_owned());
        let settings = fixture.settings_mut();
        settings.fallback.order = list(&["w-top-fallback"]);
        settings.fallback.replace.insert(
            agent_id.clone(),
            AgentFallbacks {
                with: list(&["w-top-replace"]),
                ..AgentFallbacks::default()
            },
        );
        let role = settings.fallback.roles.entry(CrewRole::Worker).or_default();
        role.order = list(&["w-role-fallback"]);
        role.replace.insert(
            agent_id.clone(),
            AgentFallbacks {
                with: list(&["w-role-replace"]),
                ..AgentFallbacks::default()
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
        type EmptyLevel = fn(&mut Settings, &AgentId);
        let steps: [(&str, EmptyLevel); 4] = [
            ("w-role-fallback", |s, v| {
                s.fallback
                    .roles
                    .entry(CrewRole::Worker)
                    .or_default()
                    .replace
                    .remove(v);
            }),
            ("w-top-replace", |s, _| {
                s.fallback
                    .roles
                    .entry(CrewRole::Worker)
                    .or_default()
                    .order
                    .clear();
            }),
            ("w-top-fallback", |s, v| {
                s.fallback.replace.remove(v);
            }),
            ("claude-code", |s, _| s.fallback.order.clear()),
        ];
        for (expected, empty_one) in steps {
            empty_one(fixture.settings_mut(), &agent_id);
            let planner = fixture.planner();
            let first = workers(&shortlist(&query, &planner.sources()));
            if expected == "claude-code" {
                // Nothing set: every healthy crew member but the stopped one.
                assert_eq!(first, ["claude-code", "codex", "antigravity-cli"]);
            } else {
                assert_eq!(first, [expected]);
            }
        }

        // Another agent's fallback list never applies to this stop.
        let settings = fixture.settings_mut();
        settings.fallback.replace.insert(
            AgentId("codex".to_owned()),
            AgentFallbacks {
                with: list(&["w-codex-only"]),
                ..AgentFallbacks::default()
            },
        );
        let planner = fixture.planner();
        assert!(!picked_with(&planner, &query).contains(&"w-codex-only".to_owned()));
    }

    fn picked_with(planner: &crate::fallback::Planner, query: &CandidateQuery) -> Vec<String> {
        workers(&shortlist(query, &planner.sources()))
    }

    #[test]
    fn an_agent_you_turned_off_is_never_a_candidate() {
        let mut fixture = Fixture::new();
        let settings = fixture.settings_mut();
        settings.fallback.order = list(&["codex", "opencode", "antigravity-cli"]);
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
    fn a_cli_that_would_refuse_to_start_is_never_a_candidate() {
        let mut fixture = Fixture::new();
        fixture.settings_mut().fallback.order =
            list(&["opencode", "opencode-acp", "antigravity-cli"]);
        let workers = Arc::new(argonauts_testkit::FakeDirectory::new());
        let no_model = "OpenCode has no model set, and its own default runs on a hosted service";
        for refusing in ["opencode", "opencode-acp"] {
            workers.add(FakeRunner::cli(refusing, "opencode").refusing_to_start(no_model));
        }
        workers.add(FakeRunner::cli("antigravity-cli", "google"));
        let query = fixture.query("claude-code/opus");
        let planner = fixture.planner();
        assert_eq!(
            picked_with(&planner, &query),
            ["opencode", "opencode-acp", "antigravity-cli"]
        );
        let planner = fixture.planner().with_workers(workers);
        assert_eq!(picked_with(&planner, &query), ["antigravity-cli"]);
        let sources = planner.sources();
        let opencode = candidate(
            &entry("opencode"),
            &query,
            &AppliedFallback::default(),
            &sources,
        );
        assert_eq!(
            check(&opencode, &query, &sources),
            Err(Dropped::WontStart(no_model.to_owned()))
        );
    }

    /// A model on a server answers with no tools, so it never takes over a build, though it
    /// may still review.
    #[test]
    fn a_model_with_no_tools_never_takes_over_a_build() {
        let mut fixture = Fixture::new();
        fixture.settings_mut().fallback.order = list(&["ollama/gemma3:12b-32k", "antigravity-cli"]);
        let workers = Arc::new(argonauts_testkit::FakeDirectory::new());
        workers.add(FakeRunner::endpoint("ollama/gemma3:12b-32k", "ollama"));
        workers.add(FakeRunner::cli("antigravity-cli", "google"));
        let planner = fixture.planner().with_workers(workers);
        let query = fixture.query("opencode/ollama/qwen3-coder:30b-64k");
        assert_eq!(picked_with(&planner, &query), ["antigravity-cli"]);
        let review = CandidateQuery {
            role: RoleName::Reviewer,
            ..query
        };
        assert!(
            picked_with(&planner, &review)
                .iter()
                .any(|picked| picked == "ollama/gemma3:12b-32k")
        );
    }

    #[test]
    fn candidates_are_filtered_by_health_plan_usage_and_rules() {
        let mut fixture = Fixture::new();
        fixture.registry.add(model("codex", "gpt-5", "openai"));
        fixture
            .registry
            .add(model("antigravity-cli", "gemini-3", "google"));
        fixture
            .registry
            .add(model("claude-code", "sonnet", "anthropic"));
        fixture
            .registry
            .add(model("claude-code", "opus", "anthropic"));
        let mut retired = model("opencode", "old", "zhipu");
        retired.retired = true;
        fixture.registry.add(retired);
        fixture.settings_mut().fallback.order = list(&[
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
            &AgentId("codex".to_owned()),
            HealthState::Down,
            Some("overloaded"),
        );
        fixture.plan_usage.observe(plan_usage(
            "antigravity-cli",
            PlanUsageStatus::Rejected,
            Some(1.0),
        ));
        assert_eq!(picked_with(&planner, &query), ["claude-code/sonnet"]);

        // A reviewer never gets the author's provider, nor a model another reviewer has taken.
        let mut review = fixture.query("claude-code/opus");
        review.role = RoleName::Reviewer;
        review.task.worker = Some(worker("claude-code/opus"));
        assert!(picked_with(&planner, &review).is_empty());
        let sources = planner.sources();
        let sonnet = candidate(
            &entry("claude-code/sonnet"),
            &review,
            &AppliedFallback::default(),
            &sources,
        );
        assert!(matches!(
            check(&sonnet, &review, &sources),
            Err(Dropped::BreaksRule(_))
        ));

        // Excluded providers go too.
        let mut excluded = fixture.query("claude-code/opus");
        excluded.exclude_providers = vec!["anthropic".to_owned()];
        assert!(picked_with(&planner, &excluded).is_empty());
    }

    #[test]
    fn a_fallback_at_your_plan_limit_is_passed_over() {
        let mut fixture = Fixture::new();
        fixture.registry.add(model("codex", "gpt-5", "openai"));
        fixture
            .registry
            .add(model("antigravity-cli", "gemini-3", "google"));
        fixture
            .registry
            .add(model("claude-code", "opus", "anthropic"));
        fixture.settings_mut().fallback.order = list(&["codex/gpt-5", "antigravity-cli/gemini-3"]);
        fixture
            .plan_usage
            .observe(plan_usage("codex", PlanUsageStatus::Allowed, Some(0.85)));
        let query = fixture.query("claude-code/opus");

        // No limit of yours: a plan is used until its provider says it's out.
        assert_eq!(
            picked_with(&fixture.planner(), &query),
            ["codex/gpt-5", "antigravity-cli/gemini-3"]
        );

        fixture.settings_mut().limits.spend.plan_usage = Some(0.8);
        let planner = fixture.planner();
        assert_eq!(picked_with(&planner, &query), ["antigravity-cli/gemini-3"]);
        let sources = planner.sources();
        let codex = candidate(
            &entry("codex/gpt-5"),
            &query,
            &AppliedFallback::default(),
            &sources,
        );
        assert_eq!(check(&codex, &query, &sources), Err(Dropped::AtPlanLimit));
    }

    #[test]
    fn your_plan_limit_counts_until_the_window_resets() {
        let now = OffsetDateTime::now_utc();
        let mut settings = Settings::default();
        let mut reading = plan_usage("codex", PlanUsageStatus::Allowed, Some(0.8));
        assert!(!at_plan_limit(&reading, &settings, now), "no limit is set");
        settings.limits.spend.plan_usage = Some(0.8);
        assert!(at_plan_limit(&reading, &settings, now));
        reading.resets_at = Some(now - time::Duration::minutes(1));
        assert!(!at_plan_limit(&reading, &settings, now), "the window reset");
        reading.resets_at = Some(now + time::Duration::minutes(1));
        assert!(at_plan_limit(&reading, &settings, now));
        reading.used = Some(0.79);
        assert!(!at_plan_limit(&reading, &settings, now));
        reading.used = None;
        assert!(!at_plan_limit(&reading, &settings, now));
    }

    #[test]
    fn a_worker_must_see_what_the_task_owns() {
        let mut fixture = Fixture::new();
        fixture.registry.add(model("codex", "gpt-5", "openai"));
        fixture.settings_mut().fallback.order = list(&["codex/gpt-5"]);
        let mut query = fixture.query("claude-code/opus");
        let exclude = |fixture: &mut Fixture, paths: &[&str]| {
            let codex = fixture.settings_mut().agents.entry("codex".to_owned());
            codex.or_default().exclude_paths = paths.iter().map(|p| (*p).to_owned()).collect();
        };

        // Hiding part of the repo still lets it own a task that owns the whole repo.
        exclude(&mut fixture, &["secrets/**"]);
        assert_eq!(picked_with(&fixture.planner(), &query), ["codex/gpt-5"]);

        exclude(&mut fixture, &["**"]);
        assert!(picked_with(&fixture.planner(), &query).is_empty());

        // An owned path it can't see rules it out; a reviewer reads, so it stays.
        exclude(&mut fixture, &["src/**"]);
        query.task.owned_paths = vec!["src/api/**".to_owned()];
        assert!(picked_with(&fixture.planner(), &query).is_empty());
        query.task.owned_paths = vec!["docs/**".to_owned()];
        assert_eq!(picked_with(&fixture.planner(), &query), ["codex/gpt-5"]);
        query.task.owned_paths = vec!["src/api/**".to_owned()];
        query.role = RoleName::Reviewer;
        assert_eq!(picked_with(&fixture.planner(), &query), ["codex/gpt-5"]);
    }

    #[test]
    fn testing_and_billing_capped_agents_are_dropped() {
        let mut fixture = Fixture::new();
        fixture.registry.add(model("codex", "gpt-5", "openai"));
        fixture
            .registry
            .add(api_model("openrouter", "kimi", "moonshot", 1.0));
        fixture.registry.add(model("openrouter", "glm", "zhipu"));
        fixture.settings_mut().fallback.order =
            list(&["codex/gpt-5", "openrouter/kimi", "openrouter/glm"]);
        let query = fixture.query("claude-code/opus");
        let planner = fixture.planner();
        assert_eq!(
            picked_with(&planner, &query),
            ["codex/gpt-5", "openrouter/kimi", "openrouter/glm"]
        );

        // An agent under test takes no stopped task, and a billing cap keeps only the agent's
        // API-key models away.
        fixture.health.set(
            &AgentId("codex".to_owned()),
            HealthState::Testing,
            Some("the cool-down is over"),
        );
        fixture.health.observe(HealthSignal::AdapterBroken {
            agent_id: AgentId("openrouter".to_owned()),
            detail: billing_disable(),
        });
        assert_eq!(picked_with(&planner, &query), ["openrouter/glm"]);
        let sources = planner.sources();
        let judged = |name: &str| {
            let candidate = candidate(&entry(name), &query, &AppliedFallback::default(), &sources);
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
            .add(api_model("openrouter", "glm-5", "zhipu", 10.0));
        fixture
            .registry
            .add(api_model("openrouter", "kimi", "moonshot", 1.0));
        fixture.registry.add(model("codex", "gpt-5", "openai"));
        let capped = |worker_name: &str, cap: f64, allow: bool| {
            FallbackEntry::WithRules(FallbackRules {
                model: worker(worker_name),
                max_spend_usd: Some(cap),
                instructions: Some("Keep to the task's paths.".to_owned()),
                read_only: false,
                allow_api_billing: allow,
            })
        };
        // glm-5 costs about $4.40 for the estimate, over its $2 cap; kimi about $0.44.
        fixture.settings_mut().fallback.order = vec![
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
        assert_eq!(kimi.plan_usage_text, "API key");
        assert!(matches!(kimi.cost, CostEstimate::Usd(usd) if usd < 2.0));
        let glm = candidate(
            &capped("openrouter/glm-5", 2.0, true),
            &query,
            &applied(
                &fixture.settings,
                RoleName::Worker,
                &AgentId("claude-code".to_owned()),
            ),
            &planner.sources(),
        );
        assert_eq!(
            check(&glm, &query, &planner.sources()),
            Err(Dropped::OverLimitCap)
        );
    }

    #[test]
    fn api_billing_needs_confirm() {
        let mut fixture = Fixture::new();
        fixture
            .registry
            .add(api_model("openrouter", "kimi", "moonshot", 1.0));
        fixture.registry.add(model("codex", "gpt-5", "openai"));
        fixture.settings_mut().fallback.order = list(&["openrouter/kimi", "codex/gpt-5"]);
        let planner = fixture.planner();
        let query = fixture.query("claude-code/opus");
        let shortlist = shortlist(&query, &planner.sources());
        let kimi = &shortlist.candidates[0];
        assert!(kimi.needs_confirm, "moving to a paid API needs your OK");
        assert_eq!(auto_check(kimi), Err(Dropped::PaidApi));
        assert!(auto_check(&shortlist.candidates[1]).is_ok());

        fixture.settings_mut().fallback.allow_api_billing = true;
        let planner = fixture.planner();
        let allowed = super::shortlist(&query, &planner.sources());
        assert!(!allowed.candidates[0].needs_confirm);
        assert!(auto_check(&allowed.candidates[0]).is_ok());
    }

    #[test]
    fn spread_balances_by_plan_usage_and_record() {
        let mut fixture = Fixture::new();
        for (agent_id, model_id, provider) in [
            ("codex", "gpt-5", "openai"),
            ("antigravity-cli", "gemini-3", "google"),
            ("opencode", "glm", "zhipu"),
            ("qwen", "coder", "alibaba"),
        ] {
            fixture.registry.add(model(agent_id, model_id, provider));
        }
        let settings = fixture.settings_mut();
        settings.fallback.strategy = Some(OrderStrategy::Spread);
        settings.fallback.order = list(&[
            "codex/gpt-5",
            "antigravity-cli/gemini-3",
            "opencode/glm",
            "qwen/coder",
        ]);
        fixture
            .plan_usage
            .observe(plan_usage("codex", PlanUsageStatus::Allowed, Some(0.9)));
        fixture.plan_usage.observe(plan_usage(
            "antigravity-cli",
            PlanUsageStatus::Allowed,
            Some(0.3),
        ));
        fixture
            .plan_usage
            .observe(plan_usage("opencode", PlanUsageStatus::Allowed, Some(0.3)));
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
        assert_eq!(shortlist.strategy, FallbackStrategy::Spread);
        assert_eq!(shortlist.race_count, 1);
        // 70% left beats 10%; at 70% each, the better record wins; unknown plan usage goes last.
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
        assert_eq!(first.plan_usage_text, "70%");
        assert_eq!(first.track_record, Some(TrackRecord { passed: 5, of: 6 }));
        let codex = &shortlist.candidates[2];
        assert!(codex.may_run_out, "10% left is low");
        assert!(!first.may_run_out);

        // Free slots break a tie in plan usage before records do.
        struct Slots;
        impl FreeSlots for Slots {
            fn free(&self, agent_id: &AgentId) -> Option<u32> {
                Some(if agent_id.0 == "antigravity-cli" {
                    3
                } else {
                    0
                })
            }
        }
        let planner = fixture.planner().with_slots(Arc::new(Slots));
        assert_eq!(
            workers(&super::shortlist(&query, &planner.sources()))[..2],
            ["antigravity-cli/gemini-3", "opencode/glm"]
        );
    }

    #[test]
    fn together_says_how_many_fallbacks_work_and_pick_how_they_combine() {
        let mut fixture = Fixture::new();
        let planner = fixture.planner();
        let one = shortlist(&fixture.query("claude-code/opus"), &planner.sources());
        assert_eq!((one.strategy, one.race_count), (FallbackStrategy::First, 1));
        assert_eq!(one.pick, Pick::FirstPassing, "what a worker builds");

        let settings = fixture.settings_mut();
        settings.fallback.together = Some(3);
        let planner = fixture.planner();
        let mut query = fixture.query("claude-code/opus");
        let shortlist = shortlist(&query, &planner.sources());
        assert_eq!(shortlist.strategy, FallbackStrategy::Race);
        assert_eq!(shortlist.race_count, 3);
        assert_eq!(shortlist.pick, Pick::FirstPassing);
        // A review's findings are all kept, and a table that says how wins.
        query.role = RoleName::Reviewer;
        assert_eq!(
            super::shortlist(&query, &planner.sources()).pick,
            Pick::Merge
        );
        fixture.settings_mut().fallback.pick = Some(Pick::Judge);
        let planner = fixture.planner();
        assert_eq!(
            super::shortlist(&query, &planner.sources()).pick,
            Pick::Judge
        );
        assert_eq!(
            workers(&shortlist),
            ["claude-code", "codex", "antigravity-cli"]
        );
    }

    #[test]
    fn all_valid_lists_every_valid_worker() {
        let mut fixture = Fixture::new();
        fixture.registry.add(model("opencode", "glm", "zhipu"));
        fixture.settings_mut().fallback.order = list(&["codex"]);
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
