//! Replays: the app's `ReplayRunner` and the background worker that drains the candidate
//! queue. A replay is a supervised run in an isolated repo under the state folder, with the
//! candidate version's settings, the replay's workers, its limit, and `merge.mode = "leave"`,
//! so it never touches your repository. Its tasks keep the task's hidden tests out of reach, and
//! its asks before the phase the change touches take what the task's own run answered. One
//! replays at a time, while candidates wait and the month's `limits.evolution_usd_per_month`
//! lasts; then each candidate that passes reaches the promotion approval.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use time::OffsetDateTime;

use argonauts_core::Paths;
use argonauts_core::api::{ConductorApi as _, StartRun};
use argonauts_core::events::run::{RunLifecycle, RunMode};
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::future::BoxFuture;
use argonauts_core::limits::CheckRunner;
use argonauts_core::names::ModelRef;
use argonauts_core::policy::ApprovalPolicy;
use argonauts_core::settings::Settings;
use argonauts_core::settings::crew::CrewSettings;
use argonauts_crew::replay_terms::{self, ReplayTerms};
use argonauts_learning::evolution::{self, Action, Candidate, CandidateState, Evolution, Stage};
use argonauts_learning::learned_workflows::{self, LearnedWorkflowMode};
use argonauts_learning::replay::{self, ReplayRequest, ReplayRun, ReplayRunner, ReplaySet};
use argonauts_learning::{LearningError, Store};
use argonauts_registry::LearnedWorkflowOverlay;

use super::learned_workflow::ForRun;
use super::promotion::{self, Promotion};
use crate::client::{Client, Launcher};
use crate::env::AppEnv;

/// How often a replay's log is read while it runs.
const POLL: Duration = Duration::from_secs(2);
/// A replay that hasn't finished in this long is given up on and scored as failed.
const GIVE_UP: Duration = Duration::from_secs(6 * 60 * 60);

/// Starts each replay as a supervised run of its own, through the same start as yours.
pub(crate) struct SupervisedReplays {
    paths: Paths,
    /// Your repository's runs, whose logs hold what a prefix replay reuses.
    runs: PathBuf,
    launcher: Arc<dyn Launcher>,
    poll: Duration,
    give_up: Duration,
}

impl SupervisedReplays {
    pub(crate) fn new(env: &AppEnv, launcher: Arc<dyn Launcher>) -> Self {
        Self {
            paths: env.paths.clone(),
            runs: env.runs_dir(),
            launcher,
            poll: POLL,
            give_up: GIVE_UP,
        }
    }

    /// What the replay's tasks keep hidden, and with `from_phase`, what the run its task came
    /// from answered before that phase. A log that's gone or unreadable leaves the whole run
    /// to the replay's workers.
    fn terms(&self, request: &ReplayRequest) -> ReplayTerms {
        let reuse = request.from_phase.and_then(|from| {
            let log = self
                .runs
                .join(request.entry.run.as_str())
                .join(argonauts_core::layout::run_files::EVENTS);
            match argonauts_core::journal::read_all(&log) {
                Ok(back) => replay_terms::recorded(&back.events, from),
                Err(err) => {
                    tracing::debug!("{} runs whole: {err}", request.entry.id);
                    None
                }
            }
        });
        ReplayTerms {
            hidden_paths: request.hidden_paths.clone(),
            reuse,
        }
    }

    async fn replay(&self, request: &ReplayRequest) -> anyhow::Result<ReplayRun> {
        let env = Arc::new(AppEnv::for_repo(self.paths.clone(), &request.repo, false)?);
        let client = Client::new(env, Arc::clone(&self.launcher), false);
        let mut resolved = crate::client::resolve_settings(&client, &[], Some(overlay(request)?))?;
        fence(&mut resolved, &request.workers)?;
        let mode = match request.learned_workflow.learned_workflow.mode {
            Some(LearnedWorkflowMode::Captain) => RunMode::Captain,
            _ => RunMode::Crew,
        };
        let start = StartRun {
            request: request.entry.task.clone(),
            mode,
            forced: true,
            estimate: None,
            captain_or_crew: None,
            conversation_turn: None,
            continues: None,
            overrides: Vec::new(),
            include_uncommitted: false,
            main_agent: request.workers.first().cloned(),
            approval_mode: None,
            outside_agent: Some("evolution".to_owned()),
            instructions: Vec::new(),
            request_settings: Vec::new(),
            required_steps: Vec::new(),
        };
        let learned_workflow = ForRun {
            loaded: request.learned_workflow.clone(),
            canary: false,
            replay: Some(self.terms(request)),
        };
        let began = Instant::now();
        let handle =
            crate::client::start_resolved(&client, start, resolved, Some(&learned_workflow))
                .await
                .map_err(|err| anyhow::anyhow!("{err}"))?;
        let end = wait_for_end(&handle.events, self.poll, self.give_up).await;
        if end != End::Finished {
            // Nobody answers a replay, so whatever it waits on, its agents stop spending now.
            if let Err(err) = client.stop(&handle.run).await {
                tracing::debug!("stopping replay run {}: {err}", handle.run);
            }
        }
        let events = read_events(&handle.events);
        let head = match end {
            End::Finished => {
                let state = argonauts_crew::state::RunState::fold(&events);
                argonauts_crew::merge::merge_packet::landing_branch_of(&state).and_then(|branch| {
                    argonauts_learning::git::run(
                        &request.repo,
                        &["rev-parse", "--verify", &format!("{branch}^{{commit}}")],
                    )
                    .ok()
                    .map(|sha| sha.trim().to_owned())
                })
            }
            End::Asked | End::GaveUp => None,
        };
        Ok(ReplayRun {
            cost_usd: spent(&events),
            minutes: began.elapsed().as_secs_f64() / 60.0,
            head,
        })
    }
}

/// A replay's own terms over whatever your settings say. Only the replay's workers fill the
/// crew and every role, with no fallback to anyone else, no narrator, and no run review proposal,
/// and the goal and plan approvals follow your rules, since nobody answers a replay. A
/// worker that would still come from elsewhere refuses the replay.
pub(crate) fn fence(
    resolved: &mut argonauts_registry::ResolvedSettings,
    workers: &[ModelRef],
) -> anyhow::Result<()> {
    let Some(first) = workers.first() else {
        anyhow::bail!("a replay needs at least one worker");
    };
    let settings = &mut resolved.settings;
    settings.crew = CrewSettings::of(workers.to_vec());
    settings.crew.main = first.clone();
    let no_fallbacks = |fallback: &mut argonauts_core::settings::fallback::FallbackTable| {
        fallback.order.clear();
        fallback.replace.clear();
        fallback.summarizer = None;
    };
    no_fallbacks(&mut settings.fallback);
    settings.fallback.roles.values_mut().for_each(no_fallbacks);
    for role in settings.roles.values_mut() {
        role.models = workers.to_vec();
    }
    settings.ui.narrator = argonauts_core::settings::ui::NarratorMode::Off;
    settings.narrator.model = None;
    // A replay is an evaluation: its run review proposes nothing and shares nothing.
    settings.evolution.propose_every = 0;
    settings.outcomes.share = false;
    replay_approvals(settings);
    let judge_settings = &settings.approvals.judge;
    let named = argonauts_crew::workflow::runner::allowed_workers(settings)
        .into_iter()
        .chain(
            judge_decides(settings)
                .then(|| {
                    let model = settings.approvals.judge_model();
                    model
                        .into_iter()
                        .chain(&judge_settings.second_model)
                        .cloned()
                })
                .into_iter()
                .flatten(),
        );
    for worker in named {
        if !workers.contains(&worker) {
            anyhow::bail!(
                "{worker} would work in the replay, but only {} may, so it can't run at the \
                 same terms",
                workers
                    .iter()
                    .map(ModelRef::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    let json = serde_json::to_value(&*settings)?;
    resolved.sha256 = argonauts_core::sha256_hex(argonauts_core::canonical_json(&json).as_bytes());
    Ok(())
}

/// The goal and plan approvals of a replay follow your rules: nobody answers a replay, and
/// it never merges. One the rules can't settle still waits, which ends the try.
pub(crate) fn replay_approvals(settings: &mut Settings) {
    use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
    let approvals = &mut settings.approvals;
    approvals.understand.mode = ApprovalMode::Auto;
    approvals.understand.decide_with = DecideWith::Rules;
    approvals.plan.mode = ApprovalMode::Auto;
    approvals.plan.decide_with = DecideWith::Rules;
}

/// Whether an approval a replay can open, left to auto mode, asks your judge. A
/// replay leaves its branch before the merge approval, so that one never opens.
fn judge_decides(settings: &Settings) -> bool {
    use argonauts_core::settings::approvals::DecideWith;
    let a = &settings.approvals;
    [
        (a.stuck.mode, a.stuck.decide_with),
        (a.limit.mode, a.limit.decide_with),
        (a.change_request.mode, a.change_request.decide_with),
    ]
    .into_iter()
    .any(|(mode, with)| mode.goes_ahead() && with != DecideWith::Rules)
}

impl ReplayRunner for SupervisedReplays {
    fn run<'a>(
        &'a self,
        request: &'a ReplayRequest,
    ) -> BoxFuture<'a, argonauts_learning::Result<ReplayRun>> {
        Box::pin(async move {
            self.replay(request)
                .await
                .map_err(|err| LearningError::Refused(format!("{err:#}")))
        })
    }
}

/// The version's settings with the replay's workers, as a replay's workflow layer: it never
/// merges and spends at most the replay's limit.
pub(crate) fn overlay(request: &ReplayRequest) -> anyhow::Result<LearnedWorkflowOverlay> {
    let mut table = learned_workflows::settings_overlay(&request.learned_workflow)?;
    if !request.workers.is_empty() {
        let crew = request
            .workers
            .iter()
            .map(|worker| toml::Value::String(worker.to_string()))
            .collect();
        table.insert("crew".to_owned(), toml::Value::Array(crew));
    }
    Ok(LearnedWorkflowOverlay {
        label: format!("replay of {}", request.learned_workflow.reference()),
        settings: table.to_string(),
        canary: false,
        replay_limit_usd: Some(request.limit_usd.max(0.0).ceil() as u64),
    })
}

/// How a replay's run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    Finished,
    /// An approval waits for you, or the run stopped to wait: a replay has nobody to answer it.
    Asked,
    /// Still going at `give_up`.
    GaveUp,
}

/// Reads the run's log as it grows, until it finishes, waits for you, or runs past `give_up`.
async fn wait_for_end(events: &Path, poll: Duration, give_up: Duration) -> End {
    let began = Instant::now();
    let mut read = 0;
    loop {
        let text = std::fs::read(events).unwrap_or_default();
        // Only whole lines: the run host may be halfway through writing the last one.
        let upto = text
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |at| at + 1);
        for line in text
            .get(read..upto)
            .unwrap_or_default()
            .split(|byte| *byte == b'\n')
        {
            let Ok(event) = serde_json::from_slice::<RunEvent>(line) else {
                continue;
            };
            if let Some(end) = ends(&event) {
                return end;
            }
        }
        read = upto.max(read);
        if began.elapsed() >= give_up {
            return End::GaveUp;
        }
        tokio::time::sleep(poll).await;
    }
}

/// Whether `event` ends a replay's try.
fn ends(event: &RunEvent) -> Option<End> {
    use argonauts_core::events::approval::ApprovalEvent;
    use argonauts_core::policy::Verdict;
    match &event.kind {
        RunEventKind::Run(RunLifecycle::Finished { .. }) => Some(End::Finished),
        RunEventKind::Run(RunLifecycle::Waiting { .. }) => Some(End::Asked),
        RunEventKind::Approval(ApprovalEvent::Opened(opened)) if opened.needs_you => {
            Some(End::Asked)
        }
        RunEventKind::Approval(ApprovalEvent::Checked(check))
            if !check.auto_preview
                && !matches!(check.verdict, Some(Verdict::Approve | Verdict::Reject)) =>
        {
            Some(End::Asked)
        }
        _ => None,
    }
}

fn read_events(path: &Path) -> Vec<RunEvent> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// What the run's sessions cost, as they reported it.
fn spent(events: &[RunEvent]) -> f64 {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Ended(end)) => Some(end.spend.cost_usd),
            _ => None,
        })
        .sum()
}

/// What one turn of the worker did.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Turn {
    /// Nothing queued, the stage doesn't replay, or the month's limit is spent.
    Idle(String),
    /// A candidate was replayed; the loop's actions, and the promotion approval's result when
    /// it reached one.
    Replayed {
        candidate: String,
        actions: Vec<Action>,
        promotion: Option<Promotion>,
    },
}

/// Replays the candidate whose replay is under way, else the oldest queued one, if the stage
/// and the month's limit allow, and takes a passing one to the promotion approval. A replay
/// the limit stops books what it spent and waits for next month.
pub(crate) async fn replay_next(
    store: &Store,
    settings: &Settings,
    runner: Arc<dyn ReplayRunner>,
    checks: Arc<dyn CheckRunner>,
    policy: &dyn ApprovalPolicy,
) -> anyhow::Result<Turn> {
    if !matches!(evolution::stage(settings), Stage::Evolve | Stage::Auto) {
        return Ok(Turn::Idle("evolution.stage doesn't replay".to_owned()));
    }
    let evolution = Evolution::open(store)?;
    let spent = super::or_empty(evolution.spent_this_month(OffsetDateTime::now_utc()))?;
    // Without a monthly limit, each replay run still keeps to its own limit.
    let left = settings
        .limits
        .evolution_usd_per_month
        .map_or(f64::INFINITY, |limit| limit - spent);
    if left <= 0.0 {
        return Ok(Turn::Idle(
            "the month's evolution limit is spent; candidates wait for next month".to_owned(),
        ));
    }
    let Some(candidate) = next_candidate(&evolution, settings)? else {
        return Ok(Turn::Idle("no candidate waits for a replay".to_owned()));
    };
    let parent = evolution
        .load_version(&candidate.parent.name, candidate.parent.version)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{} is in neither .argonauts/learned/ nor the loop's candidates, so {} can't \
                 replay",
                candidate.parent,
                candidate.id
            )
        })?;
    // The candidate's own files, as the loop wrote them, never rebuilt from its change.
    let child = evolution.candidate_learned_workflow(&candidate.id)?;
    let set = ReplaySet::open(store)?;
    let versions = replay::Versions {
        parent: &parent,
        child: &child,
        thresholds: &candidate.thresholds,
    };
    let report = replay::replay(&set, versions, settings, left, runner, checks).await?;
    if report.at_limit {
        let told = evolution.replay_paused(&candidate.id, report.spent_usd)?;
        let text = told
            .iter()
            .map(|action| argonauts_crew::run_review::action_note(action).text)
            .collect::<Vec<_>>()
            .join(" ");
        return Ok(Turn::Idle(text));
    }
    let actions =
        evolution.after_replay(settings, &candidate.id, &report.results, report.spent_usd)?;
    let mut promotion = None;
    for action in &actions {
        if let Action::AskPromotion {
            candidate: id,
            packet,
        } = action
            && let Some(asked) = evolution.candidate(id)?
        {
            promotion = Some(promotion::approval(policy, &evolution, &asked, packet).await?);
        }
    }
    Ok(Turn::Replayed {
        candidate: candidate.id.clone(),
        actions,
        promotion,
    })
}

/// The candidate replaying already, which a run review may have handed out, else the oldest queued
/// one, handed out the same way, so two never replay at once.
fn next_candidate(evolution: &Evolution, settings: &Settings) -> anyhow::Result<Option<Candidate>> {
    let replaying = |candidates: Vec<Candidate>| {
        candidates
            .into_iter()
            .find(|candidate| matches!(candidate.state, CandidateState::Replaying { .. }))
    };
    if let Some(found) = replaying(super::or_empty(evolution.candidates())?) {
        return Ok(Some(found));
    }
    super::or_empty(evolution.next_replay(settings))?;
    Ok(replaying(super::or_empty(evolution.candidates())?))
}

/// The runs the replay set's checks have seen, beside the set.
const CONSIDERED: &str = "considered.json";

/// Checks each merged run the outcome log names, once, for the replay set: under
/// `accept_entries = "auto"` one that passes joins at once, and under "ask" it waits for you in
/// /learned and `argonauts approvals`. Returns the tasks proposed.
pub(crate) async fn consider_merged(
    store: &Store,
    settings: &Settings,
    checks: Arc<dyn CheckRunner>,
) -> anyhow::Result<Vec<String>> {
    use argonauts_core::events::run::RunOutcome;
    use argonauts_learning::outcomes::{self, LogLine};
    let path = store.replay_dir().join(CONSIDERED);
    let mut considered: std::collections::BTreeSet<String> = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Default::default(),
        Err(err) => return Err(err.into()),
    };
    let merged: Vec<argonauts_core::RunId> = super::or_empty(outcomes::read_all(store))?
        .into_iter()
        .filter_map(|line| match line {
            LogLine::Run(run) if run.outcome == RunOutcome::Merged => Some(run.run),
            _ => None,
        })
        .collect();
    let set = ReplaySet::open(store)?;
    let mut proposed = Vec::new();
    for run in merged {
        if !considered.insert(run.as_str().to_owned()) {
            continue;
        }
        match set.consider(&run, settings, Arc::clone(&checks)).await {
            Ok(Some(proposal)) => proposed.push(proposal.entry.id),
            Ok(None) => {}
            Err(err) => tracing::warn!("run {run} wasn't checked for the replay set: {err}"),
        }
        // After each one, so a crash checks no run twice.
        std::fs::create_dir_all(store.replay_dir())?;
        std::fs::write(&path, serde_json::to_vec(&considered)?)?;
    }
    Ok(proposed)
}

static DRAINING: AtomicBool = AtomicBool::new(false);

/// Starts draining the queue in the background, unless a drain already runs or the settings
/// leave nothing to do. Candidates replay one at a time; a failure stops the drain with a
/// warning in the log, and the next kick tries again.
pub(crate) fn kick(env: Arc<AppEnv>, settings: Arc<Settings>, policy: Arc<dyn ApprovalPolicy>) {
    if !matches!(evolution::stage(&settings), Stage::Evolve | Stage::Auto)
        || settings
            .limits
            .evolution_usd_per_month
            .is_some_and(|limit| limit <= 0.0)
    {
        return;
    }
    if DRAINING.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::spawn(async move {
        let result = drain(&env, &settings, policy.as_ref()).await;
        DRAINING.store(false, Ordering::SeqCst);
        if let Err(err) = result {
            tracing::warn!("the replay worker stopped: {err:#}");
        }
    });
}

async fn drain(
    env: &AppEnv,
    settings: &Arc<Settings>,
    policy: &dyn ApprovalPolicy,
) -> anyhow::Result<()> {
    let store = super::store(env);
    let launcher: Arc<dyn Launcher> = Arc::new(crate::client::ProcessLauncher::current()?);
    let runner: Arc<dyn ReplayRunner> = Arc::new(SupervisedReplays::new(env, launcher));
    let checks = argonauts_conductor::check_runner(Arc::clone(settings));
    if let Err(err) = consider_merged(&store, settings, Arc::clone(&checks)).await {
        tracing::warn!("merged runs weren't checked for the replay set: {err:#}");
    }
    loop {
        match replay_next(
            &store,
            settings,
            Arc::clone(&runner),
            Arc::clone(&checks),
            policy,
        )
        .await
        {
            Ok(Turn::Idle(_)) => return Ok(()),
            Ok(Turn::Replayed { .. }) => continue,
            Err(err) => match err.downcast_ref::<LearningError>() {
                Some(LearningError::NotYet(_)) => return Ok(()),
                _ => return Err(err),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use argonauts_core::RunId;
    use argonauts_core::events::run::RunOutcome;
    use argonauts_core::names::ModelRef;
    use argonauts_core::settings::agents::EndpointSettings;
    use argonauts_core::steps::task::ProcessChange;
    use argonauts_learning::learned_workflows::{
        ChangeClass, LearnedWorkflow, LoadedLearnedWorkflow, Match,
    };
    use argonauts_learning::replay::{Check, Entry, Proposal, holdout_draw};
    use argonauts_learning::{LearnedWorkflowRef, RunRecord};
    use argonauts_testkit::repo::TempRepo;

    use super::*;

    fn parent() -> LoadedLearnedWorkflow {
        let mut settings = std::collections::BTreeMap::new();
        settings.insert(
            "critique".to_owned(),
            serde_json::json!({"rounds": 1, "checklist": ["criteria"]}),
        );
        LoadedLearnedWorkflow {
            learned_workflow: LearnedWorkflow {
                name: "api-feature".to_owned(),
                version: 4,
                parent: Some(3),
                description: "HTTP endpoints".to_owned(),
                workflow: None,
                mode: Some(LearnedWorkflowMode::Crew),
                phases: Some("council".to_owned()),
                matches: Match::default(),
                prompts: Default::default(),
                settings,
            },
            path: PathBuf::from(".argonauts/learned/api-feature/v4.yaml"),
            text: String::new(),
            sha256: String::new(),
        }
    }

    /// A runner that records each request and leaves no commit, so every run fails.
    #[derive(Default)]
    struct Fake {
        asked: Mutex<Vec<ReplayRequest>>,
    }

    impl Fake {
        fn asked(&self) -> Vec<ReplayRequest> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl ReplayRunner for Fake {
        fn run<'a>(
            &'a self,
            request: &'a ReplayRequest,
        ) -> BoxFuture<'a, argonauts_learning::Result<ReplayRun>> {
            if let Ok(mut asked) = self.asked.lock() {
                asked.push(request.clone());
            }
            Box::pin(async {
                Ok(ReplayRun {
                    cost_usd: 1.0,
                    minutes: 2.0,
                    head: None,
                })
            })
        }
    }

    const V1: &str = "name: api-feature\nversion: 1\ndescription: HTTP endpoints\nmode: crew\n\
                      match: { task_types: [backend] }\nprompts: { drafting: drafting-v1.md }\n\
                      critique: { rounds: 1, checklist: [criteria, testability] }\n";

    /// A repo with `api-feature` v1 in its checkout and `tasks` tuning tasks in the replay set,
    /// each with a $1 limit, under settings whose one worker is an endpoint you let bill.
    struct World {
        repo: TempRepo,
        store: Store,
        settings: Settings,
    }

    impl World {
        fn new(tasks: usize) -> Self {
            let repo = TempRepo::new().unwrap();
            let store = Store::new(repo.scratch().join("state"), repo.root());
            repo.write(".argonauts/learned/api-feature/v1.yaml", V1)
                .unwrap();
            repo.write(
                ".argonauts/learned/api-feature/drafting-v1.md",
                "Draft the routes first.\n",
            )
            .unwrap();
            let base = repo.commit_all("a learned workflow").unwrap();
            let set = ReplaySet::open(&store).unwrap();
            let tuning = (0u64..).find(|&seed| !holdout_draw(seed, 0.5)).unwrap();
            for n in 1..=tasks {
                let id = format!("run-{n:03}");
                let proposal = Proposal {
                    entry: Entry {
                        id: id.clone(),
                        run: RunId::new(&id).unwrap(),
                        task: format!("Add endpoint {n}"),
                        task_type: "backend".to_owned(),
                        base: base.clone(),
                        merged: base.clone(),
                        tests: Vec::new(),
                        test_command: vec!["true".to_owned()],
                        limit_usd: 1.0,
                        holdout: false,
                        accepted: OffsetDateTime::UNIX_EPOCH,
                    },
                    checks: vec![Check {
                        name: "fails_at_base".to_owned(),
                        passed: true,
                        detail: String::new(),
                    }],
                };
                let path = store
                    .replay_dir()
                    .join("proposals")
                    .join(format!("{id}.json"));
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, serde_json::to_string(&proposal).unwrap()).unwrap();
                let entry = set.accept(&id, tuning).unwrap();
                assert!(!entry.holdout);
            }
            let mut settings = Settings::default();
            let worker = ModelRef::new("openrouter", Some("open-model".to_owned()));
            settings.endpoints.insert(
                "openrouter".to_owned(),
                EndpointSettings {
                    allow_api_billing: true,
                    ..EndpointSettings::default()
                },
            );
            settings.crew = CrewSettings::of(vec![worker.clone()]);
            settings.crew.main = worker;
            Self {
                repo,
                store,
                settings,
            }
        }

        fn evolution(&self) -> Evolution {
            Evolution::open(&self.store).unwrap()
        }

        /// What a run of v1 whose run review proposed `setting = value` leaves the loop with.
        fn propose(&self, setting: &str, value: &str) -> Candidate {
            let count = self.evolution().candidates().unwrap().len();
            let run = RunRecord {
                run: RunId::new(format!("run-1{count:02}")).unwrap(),
                at: OffsetDateTime::now_utc(),
                task_type: "backend".to_owned(),
                learned_workflow: Some(LearnedWorkflowRef {
                    name: "api-feature".to_owned(),
                    version: 1,
                }),
                workflow: None,
                outcome: RunOutcome::Merged,
                stuck_tasks: 0,
                final_checks_passed: true,
                cost_usd: 1.0,
                minutes: 5.0,
                canary: false,
            };
            let proposal = ProcessChange {
                setting: setting.to_owned(),
                value: value.to_owned(),
                why: "the last run missed it".to_owned(),
            };
            self.evolution()
                .after_run(&self.settings, &run, Some(&proposal))
                .unwrap();
            self.evolution().candidates().unwrap().pop().unwrap()
        }

        fn state(&self, id: &str) -> CandidateState {
            self.evolution().candidate(id).unwrap().unwrap().state
        }

        async fn turn(&self, fake: &Arc<Fake>) -> Turn {
            let runner: Arc<dyn ReplayRunner> = fake.clone();
            let checks = argonauts_conductor::check_runner(Arc::new(self.settings.clone()));
            let policy = crate::commands::policy_for_state(
                &self.store.state_dir,
                Arc::new(self.settings.clone()),
            );
            replay_next(&self.store, &self.settings, runner, checks, policy.as_ref())
                .await
                .unwrap()
        }
    }

    fn requests_for(fake: &Fake, version: u32) -> Vec<ReplayRequest> {
        fake.asked()
            .into_iter()
            .filter(|request| request.learned_workflow.learned_workflow.version == version)
            .collect()
    }

    fn overlay_of(request: &ReplayRequest) -> toml::Table {
        overlay(request).unwrap().settings.parse().unwrap()
    }

    /// A checklist item added is the whole list with one more item, as the candidate's own
    /// file has it, never the one item alone. The run review already marked it replaying
    /// when it passed it on, and the worker takes it from there.
    #[tokio::test]
    async fn a_checklist_item_candidate_replays_as_written() {
        let world = World::new(2);
        let candidate = world.propose(
            "critique.checklist",
            "[\"criteria\", \"testability\", \"names the risk\"]",
        );
        assert_eq!(candidate.change.class, ChangeClass::ChecklistItem);
        assert_eq!(
            candidate.state,
            CandidateState::Replaying { spent_usd: 0.0 }
        );
        let fake = Arc::new(Fake::default());
        let turn = world.turn(&fake).await;
        assert!(
            matches!(&turn, Turn::Replayed { candidate: id, .. } if *id == candidate.id),
            "{turn:?}"
        );
        let asked = requests_for(&fake, candidate.version);
        assert!(!asked.is_empty());
        for request in &asked {
            assert_eq!(
                request.learned_workflow.learned_workflow.settings["critique"]["checklist"],
                serde_json::json!(["criteria", "testability", "names the risk"])
            );
            assert_eq!(
                request.learned_workflow.learned_workflow.settings["critique"]["rounds"],
                1
            );
            assert_eq!(
                overlay_of(request)["critique"]["checklist"]
                    .as_array()
                    .map(Vec::len),
                Some(3)
            );
        }
        // Its parent ran from your checkout, as it was.
        let parent = requests_for(&fake, 1);
        assert_eq!(parent.len(), asked.len());
        assert_eq!(
            parent[0].learned_workflow.learned_workflow.settings["critique"]["checklist"],
            serde_json::json!(["criteria", "testability"])
        );
    }

    /// A drafting prompt candidate replays with its own template, read from the candidate's
    /// folder in Argonauts Code's state by its full path, since neither your checkout nor the
    /// isolated repo has it.
    #[tokio::test]
    async fn a_drafting_prompt_candidate_replays_with_its_own_template() {
        let world = World::new(2);
        let candidate = world.propose("prompts.drafting", "\"Draft the risks first.\"");
        assert_eq!(candidate.change.class, ChangeClass::DraftingPrompt);
        let fake = Arc::new(Fake::default());
        world.turn(&fake).await;
        let asked = requests_for(&fake, candidate.version);
        assert!(!asked.is_empty());
        let folder = world.evolution().candidate_dir(&candidate.id);
        let checkout = world.repo.root().join(".argonauts/learned/api-feature");
        for request in &asked {
            let file = PathBuf::from(overlay_of(request)["prompts"]["drafting"].as_str().unwrap());
            assert!(file.is_absolute(), "{}", file.display());
            assert_eq!(file.parent(), Some(folder.as_path()));
            assert_eq!(
                std::fs::read_to_string(&file).unwrap(),
                "Draft the risks first."
            );
            let name = file.file_name().unwrap();
            assert!(!checkout.join(name).exists());
        }
        let parent = requests_for(&fake, 1);
        let file = PathBuf::from(
            overlay_of(&parent[0])["prompts"]["drafting"]
                .as_str()
                .unwrap(),
        );
        assert_eq!(file.parent(), Some(checkout.as_path()));
        assert_eq!(
            std::fs::read_to_string(file).unwrap(),
            "Draft the routes first.\n"
        );
    }

    /// The month's limit stops a replay part way: it books what it spent, keeps replaying,
    /// never reaches the comparison with part of its results, and carries on where it stopped
    /// when there's limit again, booking only the rest.
    #[tokio::test]
    async fn a_replay_the_limit_stops_waits_without_a_verdict() {
        let mut world = World::new(3);
        world.settings.limits.evolution_usd_per_month = Some(3.0);
        let candidate = world.propose("critique.rounds", "2");
        let queued = world.propose("divide.max_tasks", "6");
        assert_eq!(queued.state, CandidateState::Queued);
        let fake = Arc::new(Fake::default());
        let now = OffsetDateTime::now_utc();

        let turn = world.turn(&fake).await;
        assert!(
            matches!(&turn, Turn::Idle(why) if why.contains("limit")),
            "{turn:?}"
        );
        assert_eq!(fake.asked().len(), 2);
        assert_eq!(world.evolution().spent_this_month(now).unwrap(), 2.0);
        assert_eq!(
            world.state(&candidate.id),
            CandidateState::Replaying { spent_usd: 2.0 }
        );
        assert_eq!(world.state(&queued.id), CandidateState::Queued);

        // A dollar left isn't a pair's worth: nothing more runs, and nothing is booked twice.
        let turn = world.turn(&fake).await;
        assert!(matches!(&turn, Turn::Idle(_)), "{turn:?}");
        assert_eq!(fake.asked().len(), 2);
        assert_eq!(world.evolution().spent_this_month(now).unwrap(), 2.0);

        world.settings.limits.evolution_usd_per_month = Some(100.0);
        let turn = world.turn(&fake).await;
        assert!(
            matches!(&turn, Turn::Replayed { candidate: id, .. } if *id == candidate.id),
            "{turn:?}"
        );
        // Each task once per version: the first pair didn't run again.
        assert_eq!(fake.asked().len(), 6);
        assert_eq!(world.evolution().spent_this_month(now).unwrap(), 6.0);
        assert!(matches!(
            world.state(&candidate.id),
            CandidateState::Rejected { .. }
        ));
        // The next candidate's turn comes after this one's verdict, never beside it.
        assert_eq!(
            world.state(&queued.id),
            CandidateState::Replaying { spent_usd: 0.0 }
        );
    }

    #[tokio::test]
    async fn the_worker_waits_when_the_stage_or_the_limit_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("state"), dir.path().join("repo"));
        let runner: Arc<dyn ReplayRunner> = Arc::new(Fake::default());
        let checks = argonauts_conductor::check_runner(Arc::new(Settings::default()));
        let policy = crate::commands::policy_for_state(
            &dir.path().join("state"),
            Arc::new(Settings::default()),
        );
        let mut settings = Settings::default();
        settings.evolution.stage = argonauts_core::settings::evolution::EvolutionStage::Suggest;
        let turn = replay_next(
            &store,
            &settings,
            Arc::clone(&runner),
            Arc::clone(&checks),
            policy.as_ref(),
        )
        .await
        .unwrap();
        assert!(matches!(turn, Turn::Idle(why) if why.contains("stage")));
        let mut settings = Settings::default();
        settings.limits.evolution_usd_per_month = Some(0.0);
        let turn = replay_next(
            &store,
            &settings,
            Arc::clone(&runner),
            Arc::clone(&checks),
            policy.as_ref(),
        )
        .await
        .unwrap();
        assert!(matches!(turn, Turn::Idle(why) if why.contains("limit")));
        let turn = replay_next(
            &store,
            &Settings::default(),
            runner,
            checks,
            policy.as_ref(),
        )
        .await
        .unwrap();
        assert!(matches!(turn, Turn::Idle(why) if why.contains("no candidate")));
    }

    fn request() -> ReplayRequest {
        ReplayRequest {
            entry: Entry {
                id: "task-001".to_owned(),
                run: RunId::new("run-01").unwrap(),
                task: "Add rate limits".to_owned(),
                task_type: "backend".to_owned(),
                base: "a".repeat(40),
                merged: "b".repeat(40),
                tests: Vec::new(),
                test_command: Vec::new(),
                limit_usd: 7.5,
                holdout: false,
                accepted: OffsetDateTime::UNIX_EPOCH,
            },
            learned_workflow: parent(),
            repo: PathBuf::from("/nowhere"),
            limit_usd: 7.5,
            from_phase: None,
            workers: vec![ModelRef::new("codex", None)],
            hidden_paths: vec!["tests/limits.rs".to_owned(), "/state/replay".to_owned()],
        }
    }

    /// The log of a run whose Understand phase asked one reader, which answered.
    fn answered_run(env: &AppEnv, run: &RunId) {
        use argonauts_core::events::session::{EndReason, SessionEnd, SessionStarted};
        use argonauts_core::events::workflow::{StepStarted, WorkflowEvent, WorkflowStarted};
        use argonauts_core::names::{AgentId, RoleName, SessionKey, WorkflowId};
        use argonauts_core::session_runner::{Access, RunnerKind};
        use argonauts_core::settings::sessions::SessionLifetime;
        use argonauts_core::steps::StepName;
        use argonauts_core::workflow::{StandardPhase, StepKind, StepRef};
        let id = WorkflowId::numbered(1);
        let step = StepRef {
            workflow: id.clone(),
            phase: 0,
            path: vec![0],
        };
        let key = SessionKey::numbered(1);
        let kinds = [
            RunEventKind::Workflow(WorkflowEvent::Started(Box::new(WorkflowStarted {
                id,
                from: None,
                version: "v1".to_owned(),
                parent: None,
                workflow: argonauts_crew::workflow::builtin::load("captain").unwrap(),
            }))),
            RunEventKind::Workflow(WorkflowEvent::StepStarted(StepStarted {
                step: step.clone(),
                kind: StepKind::Read,
                standard: StandardPhase::Understand,
                round: 1,
            })),
            RunEventKind::Session(SessionEvent::Started(Box::new(SessionStarted {
                worker: ModelRef::new("claude-code", None),
                agent_id: AgentId("claude-code".to_owned()),
                provider: "anthropic".to_owned(),
                kind: RunnerKind::Cli,
                role: RoleName::Main,
                step: Some(StepName::Read),
                access: Access::ReadOnly,
                lifetime: SessionLifetime::Task,
                resumed_from: None,
                follow_key: None,
                workflow_step: Some(step),
                label: None,
                asked: None,
            }))),
            RunEventKind::Session(SessionEvent::Ended(Box::new(SessionEnd {
                reason: EndReason::Completed,
                structured: Some(serde_json::json!({"summary": "Add rate limits"})),
                ..argonauts_crew::engine::host::crashed_end("")
            }))),
        ];
        let dir = env.run_dir(run);
        std::fs::create_dir_all(&dir).unwrap();
        let mut log = String::new();
        for (seq, kind) in kinds.into_iter().enumerate() {
            let session = matches!(kind, RunEventKind::Session(_)).then(|| key.clone());
            let event = RunEvent {
                seq: seq as u64,
                ts: OffsetDateTime::UNIX_EPOCH,
                run_id: run.clone(),
                task_id: None,
                session,
                source: "argonauts-code".to_owned(),
                kind,
            };
            log.push_str(&serde_json::to_string(&event).unwrap());
            log.push('\n');
        }
        std::fs::write(dir.join(argonauts_core::layout::run_files::EVENTS), log).unwrap();
    }

    /// A replay's tasks always keep the request's hidden paths. With a phase to start from, it
    /// reuses what the task's own run answered before that phase, from your repository's runs;
    /// a run whose log is gone, or one asked to replay whole, reuses nothing.
    #[test]
    fn a_replay_hides_its_tests_and_reuses_its_tasks_answers() {
        use argonauts_core::workflow::StandardPhase;
        let repo = TempRepo::new().unwrap();
        let env = AppEnv::for_repo(Paths::new(repo.home()), repo.root(), false).unwrap();
        let launcher: Arc<dyn Launcher> =
            Arc::new(crate::client::ProcessLauncher::current().unwrap());
        let replays = SupervisedReplays::new(&env, launcher);
        let mut request = request();

        let whole = replays.terms(&request);
        assert_eq!(whole.hidden_paths, request.hidden_paths);
        assert_eq!(whole.reuse, None);
        request.from_phase = Some(StandardPhase::Do);
        assert_eq!(replays.terms(&request).reuse, None, "no log to reuse yet");

        answered_run(&env, &request.entry.run);
        let terms = replays.terms(&request);
        assert_eq!(terms.hidden_paths, request.hidden_paths);
        let reuse = terms.reuse.expect("the Understand phase's answer");
        assert_eq!(reuse.run, request.entry.run);
        assert_eq!(reuse.from, StandardPhase::Do);
        assert_eq!(reuse.answers.len(), 1);
        assert_eq!(
            reuse.answers[0].structured,
            serde_json::json!({"summary": "Add rate limits"})
        );
        request.from_phase = None;
        assert_eq!(replays.terms(&request).reuse, None);
    }

    #[test]
    fn a_replay_never_merges_and_keeps_to_its_limit() {
        let request = request();
        let overlay = overlay(&request).unwrap();
        assert_eq!(overlay.replay_limit_usd, Some(8));
        assert!(
            overlay.settings.contains("crew = [\"codex\"]"),
            "{}",
            overlay.settings
        );
        assert!(!overlay.canary);
    }

    fn resolved(settings: Settings) -> argonauts_registry::ResolvedSettings {
        argonauts_registry::ResolvedSettings {
            settings,
            sha256: "00".repeat(32),
            entries: Vec::new(),
            overrides: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Only the replay's workers work in it, whatever your roles, fallbacks, or narrator name,
    /// and its goal and plan approvals follow your rules. The settings' hash is the fenced
    /// settings' own.
    #[test]
    fn a_replay_runs_only_its_workers_and_answers_to_rules() {
        use argonauts_core::names::RoleName;
        use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
        use argonauts_core::settings::crew::CrewRole;
        use argonauts_core::settings::fallback::FallbackEntry;
        let open = ModelRef::new("openrouter", Some("open-model".to_owned()));
        let subscription = ModelRef::new("claude-code", None);
        let mut settings = Settings {
            crew: CrewSettings {
                main: subscription.clone(),
                ..CrewSettings::of(vec![subscription.clone()])
            },
            ..Settings::default()
        };
        let reviewer = settings.roles.entry(RoleName::Reviewer).or_default();
        reviewer.models = vec![subscription.clone()];
        let reviewers = settings
            .fallback
            .roles
            .entry(CrewRole::Reviewer)
            .or_default();
        reviewers.order = vec![FallbackEntry::Model(subscription.clone())];
        settings.fallback.order = vec![FallbackEntry::Model(subscription.clone())];
        settings.fallback.summarizer = Some(subscription.clone());
        settings.narrator.model = Some(subscription.clone());
        let mut fenced = resolved(settings);
        fence(&mut fenced, std::slice::from_ref(&open)).unwrap();
        let settings = &fenced.settings;
        assert_eq!(settings.crew.members(), std::slice::from_ref(&open));
        assert_eq!(settings.crew.main, open);
        assert_eq!(
            settings.roles[&RoleName::Reviewer].models,
            std::slice::from_ref(&open)
        );
        assert!(
            settings.fallback.roles[&CrewRole::Reviewer]
                .order
                .is_empty()
        );
        assert!(settings.fallback.order.is_empty());
        assert_eq!(settings.fallback.summarizer, None);
        assert_eq!(settings.narrator.model, None);
        assert_eq!(settings.evolution.propose_every, 0);
        for (mode, with) in [
            (
                settings.approvals.understand.mode,
                settings.approvals.understand.decide_with,
            ),
            (
                settings.approvals.plan.mode,
                settings.approvals.plan.decide_with,
            ),
        ] {
            assert_eq!((mode, with), (ApprovalMode::Auto, DecideWith::Rules));
        }
        let json = serde_json::to_value(settings).unwrap();
        assert_eq!(
            fenced.sha256,
            argonauts_core::sha256_hex(argonauts_core::canonical_json(&json).as_bytes())
        );

        // A judge your auto approvals ask would work in it too, so it's refused.
        let mut asks_judge = Settings::default();
        asks_judge.approvals.stuck.mode = ApprovalMode::Auto;
        asks_judge.approvals.stuck.decide_with = DecideWith::Model;
        asks_judge.approvals.decides = subscription.clone().into();
        let refused = fence(&mut resolved(asks_judge), std::slice::from_ref(&open));
        assert!(
            refused
                .as_ref()
                .is_err_and(|err| err.to_string().contains("claude-code")),
            "{refused:?}"
        );
        assert!(fence(&mut resolved(Settings::default()), &[]).is_err());
    }

    fn line(kind: RunEventKind) -> String {
        let event = RunEvent {
            seq: 1,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new("run-01").unwrap(),
            task_id: None,
            session: None,
            source: "argonauts-code".to_owned(),
            kind,
        };
        format!("{}\n", serde_json::to_string(&event).unwrap())
    }

    /// A finished run ends the wait; so does the first approval that waits for you, opened
    /// that way or left to you, since nobody answers a replay. A half-written last line waits
    /// for the rest of it.
    #[tokio::test]
    async fn a_replay_try_ends_at_the_first_approval_that_asks() {
        use argonauts_core::events::approval::ApprovalEvent;
        use argonauts_core::events::run::RunOutcome;
        use argonauts_core::names::ApprovalId;
        use argonauts_core::policy::Verdict;
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        let poll = Duration::from_millis(5);
        let check = |verdict: Verdict, auto_preview: bool| {
            line(RunEventKind::Approval(ApprovalEvent::Checked(
                argonauts_core::events::approval::PolicyCheck {
                    id: ApprovalId::numbered(1),
                    violations: Vec::new(),
                    verdict: Some(verdict),
                    by: None,
                    judge: None,
                    reasons: Vec::new(),
                    auto_preview,
                },
            )))
        };
        let finished = line(RunEventKind::Run(RunLifecycle::Finished {
            outcome: RunOutcome::BranchLeft,
            detail: None,
        }));

        std::fs::write(
            &log,
            format!("{}{finished}", check(Verdict::Approve, false)),
        )
        .unwrap();
        assert_eq!(
            wait_for_end(&log, poll, Duration::from_secs(5)).await,
            End::Finished
        );

        std::fs::write(&log, format!("{}{finished}", check(Verdict::AskYou, true))).unwrap();
        assert_eq!(
            wait_for_end(&log, poll, Duration::from_secs(5)).await,
            End::Finished
        );

        std::fs::write(&log, format!("{}{finished}", check(Verdict::AskYou, false))).unwrap();
        assert_eq!(
            wait_for_end(&log, poll, Duration::from_secs(5)).await,
            End::Asked
        );

        let half = &finished[..finished.len() / 2];
        std::fs::write(&log, half).unwrap();
        assert_eq!(
            wait_for_end(&log, poll, Duration::from_millis(30)).await,
            End::GaveUp
        );
        std::fs::write(&log, &finished).unwrap();
        assert_eq!(
            wait_for_end(&log, poll, Duration::from_secs(5)).await,
            End::Finished
        );
    }

    /// Runs each check's command in its folder, outside any sandbox.
    struct ShellChecks;

    impl CheckRunner for ShellChecks {
        fn run(
            &self,
            request: argonauts_core::limits::CheckRequest,
        ) -> BoxFuture<'_, argonauts_core::events::CheckOutcome> {
            let output = request.command.split_first().map(|(program, args)| {
                std::process::Command::new(program)
                    .args(args)
                    .current_dir(&request.workdir)
                    .output()
            });
            let passed = matches!(output, Some(Ok(ref output)) if output.status.success());
            let outcome = argonauts_core::events::CheckOutcome {
                name: request.name.clone(),
                kind: argonauts_core::events::task::CheckKind::Command,
                passed,
                command: Some(request.command.clone()),
                exit_code: Some(i32::from(!passed)),
                duration_ms: 1,
                summary: String::new(),
                output_tail: String::new(),
                violations: Vec::new(),
                sandboxed: false,
                skipped: false,
            };
            Box::pin(async move { outcome })
        }
    }

    /// Commits a feature and its test as one merged change, and leaves what the run that merged
    /// it leaves: its journal and its run line.
    fn merged_run(repo: &TempRepo, store: &Store, n: u32, name: &str) -> RunId {
        use argonauts_core::events::agent_status::RunSpend;
        use argonauts_core::events::run::RunStarted;
        use argonauts_core::events::task::CheckKind;
        use argonauts_core::events::{CheckOutcome, MergeEvent, ReviewEvent, SpendEvent};
        use argonauts_core::meter::Spend;
        use argonauts_core::steps::intake::ChangeEstimate;
        let request = format!("Add the {name} feature in src/{name}.txt");
        let test = format!("tests/{name}.sh");
        let base = repo.head().unwrap();
        repo.write(&format!("src/{name}.txt"), "ready\n").unwrap();
        repo.write(&test, &format!("test -f src/{name}.txt\n"))
            .unwrap();
        let merged = repo.commit_all(&request).unwrap();
        let run = RunId::new(format!("run-{n:03}")).unwrap();
        let started = RunStarted {
            request: request.clone(),
            title: request.clone(),
            mode: RunMode::Crew,
            forced: false,
            workflow: "council".to_owned(),
            estimate: Some(ChangeEstimate {
                task_type: "feature".to_owned(),
                summary: request.clone(),
                files_likely: Vec::new(),
                file_count: 1,
                risk_labels: Vec::new(),
                tests_exist: true,
                unknowns: Vec::new(),
            }),
            captain_or_crew: None,
            conversation_turn: None,
            continues: None,
            repo_root: repo.root().to_path_buf(),
            base_branch: Some("main".to_owned()),
            base_commit: base,
            uncommitted: Vec::new(),
            uncommitted_included: false,
            settings_sha256: String::new(),
            settings: serde_json::Value::Null,
            overrides: Vec::new(),
            cli_versions: Default::default(),
            version: "0.0.1".to_owned(),
            outside_agent: None,
            instructions: Vec::new(),
            required_steps: Vec::new(),
        };
        let kinds = vec![
            RunEventKind::Run(RunLifecycle::Started(Box::new(started))),
            RunEventKind::Review(ReviewEvent::TestsWritten {
                reviewer: ModelRef::new("codex", None),
                for_task: argonauts_core::TaskId::numbered(1),
                files: vec![test.clone()],
            }),
            RunEventKind::Check(CheckOutcome {
                name: "suite".to_owned(),
                kind: CheckKind::FullSuite,
                passed: true,
                command: Some(vec!["sh".to_owned(), test]),
                exit_code: Some(0),
                duration_ms: 5,
                summary: "1 passed".to_owned(),
                output_tail: String::new(),
                violations: Vec::new(),
                sandboxed: true,
                skipped: false,
            }),
            RunEventKind::Spend(SpendEvent::Run(RunSpend {
                spent: Spend {
                    cost_usd: 1.0,
                    ..Spend::default()
                },
                limit: None,
                lines: Vec::new(),
                reserve_left: None,
            })),
            RunEventKind::Merge(MergeEvent::RunMerged {
                target: "main".to_owned(),
                sha: merged,
                fast_forward: true,
            }),
        ];
        let mut journal = String::new();
        for (seq, kind) in kinds.into_iter().enumerate() {
            let event = RunEvent {
                seq: seq as u64,
                ts: OffsetDateTime::now_utc(),
                run_id: run.clone(),
                task_id: None,
                session: None,
                source: "argonauts-code".to_owned(),
                kind,
            };
            journal.push_str(&serde_json::to_string(&event).unwrap());
            journal.push('\n');
        }
        let dir = store.state_dir.join("runs").join(run.as_str());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("events.jsonl"), journal).unwrap();
        let line = RunRecord {
            run: run.clone(),
            at: OffsetDateTime::now_utc(),
            task_type: "feature".to_owned(),
            learned_workflow: None,
            workflow: None,
            outcome: RunOutcome::Merged,
            stuck_tasks: 0,
            final_checks_passed: true,
            cost_usd: 1.0,
            minutes: 5.0,
            canary: false,
        };
        argonauts_learning::outcomes::append(
            store,
            &run,
            &[argonauts_learning::outcomes::LogLine::Run(line)],
        )
        .unwrap();
        run
    }

    /// Each merged run is checked for the replay set once, however often the worker wakes.
    /// Under `accept_entries = "ask"` it waits for you: accepting it at the terminal puts it on
    /// the side Argonauts Code's seed draws, recorded with the split, and rejecting drops it.
    #[tokio::test]
    async fn merged_runs_are_checked_once_and_wait_for_you() {
        use super::super::act::{Asker, act};
        use argonauts_core::settings::evolution::EntryAcceptance;
        use argonauts_core::views::LearningCommand;
        let repo = TempRepo::new().unwrap();
        let env = AppEnv::for_repo(Paths::new(repo.home()), repo.root(), false).unwrap();
        let store = super::super::store(&env);
        let alpha = merged_run(&repo, &store, 1, "alpha");
        let beta = merged_run(&repo, &store, 2, "beta");
        let mut settings = Settings::default();
        settings.evolution.replay.accept_entries = EntryAcceptance::Ask;
        let checks: Arc<dyn CheckRunner> = Arc::new(ShellChecks);

        let proposed = consider_merged(&store, &settings, Arc::clone(&checks))
            .await
            .unwrap();
        assert_eq!(proposed, [alpha.as_str(), beta.as_str()]);
        let again = consider_merged(&store, &settings, checks).await.unwrap();
        assert!(again.is_empty());
        let set = ReplaySet::open(&store).unwrap();
        assert_eq!(set.proposals().unwrap().len(), 2);
        assert!(set.entries().unwrap().is_empty());

        let accept = LearningCommand::AcceptEntry {
            id: alpha.as_str().to_owned(),
        };
        let away = Asker { at_terminal: false };
        assert!(act(&env, &settings, &accept, away).is_err());
        let here = Asker { at_terminal: true };
        let said = act(&env, &settings, &accept, here).unwrap();
        assert!(said.contains("joined the replay set"), "{said}");
        let entries = set.entries().unwrap();
        assert_eq!(entries.len(), 1);
        let split = set.splits().unwrap();
        assert_eq!(split.len(), 1);
        assert_eq!(split[0].entry, alpha.as_str());
        assert!(split[0].seed.is_some());
        assert_eq!(split[0].holdout, entries[0].holdout);

        let reject = LearningCommand::RejectEntry {
            id: beta.as_str().to_owned(),
        };
        act(&env, &settings, &reject, here).unwrap();
        assert!(set.proposals().unwrap().is_empty());
        assert_eq!(set.entries().unwrap().len(), 1);
    }
}
