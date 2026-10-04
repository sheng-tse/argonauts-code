//! The solo pipeline runs one card with one agent in one worktree, then commits the work,
//! runs the gates, and shows a merge card.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, anyhow};
use argonauts_core::{
    Adapter, BudgetAction, BudgetLimit, BudgetMeter, BudgetStatus, BudgetUnit, Card, Checkpoint,
    CheckpointKind, CommandSpec, CommitInfo, Config, DecidedBy, DecisionRecord, DiffStat, EventLog,
    GateResult, MergeCard, MergeDecision, MergeMode, Note, NoteLevel, Paths, Phase, PhaseEvent,
    PhaseStatus, RepoId, Report, RunEvent, RunEventKind, RunFinished, RunId, RunOutcome,
    RunStarted, SessionRequest, SessionRole, WRAP_UP_MESSAGE, create_private_dir_all,
    render_solo_prompt, report_json_schema, sha256_hex, write_private_file,
};

use crate::control::{ConductorSettings, StopHandle};
use crate::decision::{MergeDecider, MergeRequest};
use crate::gates::run_gate;
use crate::git::Git;
use crate::recorder::{CONDUCTOR_SOURCE, EventSink, Recorder};
use crate::supervisor::{SessionOutcome, Supervisor, WrapUpDelivery};

/// Largest diff handed to the merge card, in bytes. The full diff is always saved to a file.
pub const MAX_DIFF_BYTES: usize = 1024 * 1024;

/// Paths agents must not change, since a later session in the same worktree could load them.
use crate::sandbox::profile::PROTECTED_PATHS;

/// Files that configure the agent CLIs. A session isn't resumed after the agent changed them.
const AGENT_CONFIG_PATHS: &[&str] = &[
    ".claude",
    ".mcp.json",
    ".agents",
    ".codex",
    ".opencode",
    "opencode.json",
    "opencode.jsonc",
];

/// A CLI's native spend cap sits this much above the meter's limit, so the meter acts first.
const NATIVE_CAP_MARGIN: f64 = 1.1;

/// A dollar budget needs the CLI to report dollars, or a price for the model from
/// `agents.<id>.price` or the built-in table.
pub fn budget_problem(config: &Config, adapter: &dyn Adapter) -> Option<String> {
    if config.budget.unit != BudgetUnit::Usd {
        return None;
    }
    let capabilities = adapter.capabilities();
    let agent = config.agent(adapter.id());
    if capabilities.reports_cost || agent.effective_price().is_some() {
        return None;
    }
    let name = adapter.display_name();
    let id = adapter.id();
    let cheapest = adapter.cheapest_model().0;
    let fix = format!(
        "Pick a model Argonauts has a price for, such as --model {cheapest}, set agents.{id}.price, or use --set budget.unit=tokens."
    );
    match agent.model {
        Some(model) => Some(format!(
            "{name} reports only tokens and Argonauts has no price for {model}, so it can't enforce a dollar budget. {fix}"
        )),
        // The model named at the start of the session decides.
        None if capabilities.reports_model => None,
        None => Some(format!(
            "{name} reports only tokens and doesn't say which model it runs, so Argonauts can't enforce a dollar budget. {fix}"
        )),
    }
}

/// CLI flags such as `--model` or `--gate` are best applied as `--set` config overrides before
/// building this, so the run's recorded config and hash reflect them.
pub struct SoloRun {
    task: String,
    repo: PathBuf,
    adapter: Arc<dyn Adapter>,
    config: Config,
    paths: Paths,
    settings: ConductorSettings,
    events: Option<EventSink>,
    stop: StopHandle,
    run_id: Option<RunId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SoloOutcome {
    pub run_id: RunId,
    pub outcome: RunOutcome,
    pub detail: String,
    pub branch: Option<String>,
    pub commit: Option<String>,
    /// Kept only when its changes could not be committed.
    pub worktree: Option<PathBuf>,
    pub event_log: PathBuf,
}

impl SoloRun {
    pub fn new(
        task: impl Into<String>,
        repo: impl Into<PathBuf>,
        adapter: Arc<dyn Adapter>,
        config: Config,
        paths: Paths,
    ) -> Self {
        Self {
            task: task.into(),
            repo: repo.into(),
            adapter,
            config,
            paths,
            settings: ConductorSettings::default(),
            events: None,
            stop: StopHandle::new(),
            run_id: None,
        }
    }

    pub fn on_event(mut self, sink: impl FnMut(RunEvent) + Send + 'static) -> Self {
        self.events = Some(Box::new(sink));
        self
    }

    pub fn stop_handle(mut self, stop: StopHandle) -> Self {
        self.stop = stop;
        self
    }

    pub fn settings(mut self, settings: ConductorSettings) -> Self {
        self.settings = settings;
        self
    }

    pub fn run_id(mut self, run_id: RunId) -> Self {
        self.run_id = Some(run_id);
        self
    }

    /// Errors before the event log exists, such as not being in a git repository, are returned.
    /// After that, failures end the run as [`RunOutcome::Failed`] in the log and the outcome.
    pub async fn run<D: MergeDecider>(mut self, decider: &mut D) -> anyhow::Result<SoloOutcome> {
        let task = self.task.trim();
        anyhow::ensure!(!task.is_empty(), "the task is empty");
        if let Some(problem) = budget_problem(&self.config, self.adapter.as_ref()) {
            anyhow::bail!(problem);
        }
        let git = Git::with_env(self.settings.git_env.clone());
        let start = RepoStart::inspect(&git, &self.repo).await?;

        let run_id = self.run_id.clone().unwrap_or_else(RunId::generate);
        let card = Card::solo(task, self.config.budget.per_card_limit());
        let repo_id = RepoId::from_canonical_path(&start.root);
        self.paths
            .prepare()
            .with_context(|| format!("preparing {}", self.paths.home().display()))?;
        let log_path = self.paths.event_log_path(&repo_id, &run_id);
        let log = EventLog::open(&log_path).context("opening the event log")?;

        let mut pipeline = Pipeline {
            adapter: self.adapter.as_ref(),
            config: &self.config,
            settings: &self.settings,
            stop: &self.stop,
            recorder: Recorder::new(log, run_id.clone(), self.events.take()),
            branch: format!("argonauts/{run_id}/{}", card.id),
            worktree: self.paths.worktree_dir(&repo_id, &run_id, &card.id),
            card_dir: self.paths.card_dir(&repo_id, &run_id, &card.id),
            goal_hash: sha256_hex(card.task.as_bytes()),
            git,
            worktree_git: None,
            dot_git: None,
            card,
            run_id,
            start,
            worktree_created: false,
            work_saved: false,
            tip: None,
            merge_block: None,
            target_at_card: None,
        };
        pipeline.intake()?;
        let ending = pipeline.execute(decider).await;
        pipeline.finish(ending, log_path).await
    }
}

struct RepoStart {
    root: PathBuf,
    base_commit: String,
    base_branch: Option<String>,
    uncommitted: usize,
}

impl RepoStart {
    async fn inspect(git: &Git, dir: &Path) -> anyhow::Result<Self> {
        let root = git
            .toplevel(dir)
            .await
            .with_context(|| format!("{} is not inside a git repository", dir.display()))?;
        let root = root
            .canonicalize()
            .with_context(|| format!("resolving {}", root.display()))?;
        let base_commit = git.head_commit(&root).await?.ok_or_else(|| {
            anyhow!("the repository has no commits yet, so there is nothing to branch from")
        })?;
        Ok(Self {
            base_branch: git.current_branch(&root).await?,
            uncommitted: git.tracked_changes(&root).await?.len(),
            root,
            base_commit,
        })
    }
}

struct Ending {
    outcome: RunOutcome,
    detail: String,
}

impl Ending {
    fn new(outcome: RunOutcome, detail: impl Into<String>) -> Self {
        Self {
            outcome,
            detail: detail.into(),
        }
    }
}

struct AgentRun {
    session: SessionOutcome,
    report: Option<Report>,
    budget: BudgetStatus,
}

struct Committed {
    tip: String,
    stat: DiffStat,
}

struct Pipeline<'a> {
    adapter: &'a dyn Adapter,
    config: &'a Config,
    settings: &'a ConductorSettings,
    stop: &'a StopHandle,
    recorder: Recorder,
    git: Git,
    worktree_git: Option<Git>,
    /// The worktree's `.git` file as git wrote it.
    dot_git: Option<Vec<u8>>,
    card: Card,
    /// SHA-256 of the task, which is the goal in solo mode.
    goal_hash: String,
    run_id: RunId,
    start: RepoStart,
    branch: String,
    worktree: PathBuf,
    card_dir: PathBuf,
    worktree_created: bool,
    /// The worktree's changes are committed, so removing it loses nothing.
    work_saved: bool,
    tip: Option<String>,
    /// Why approving can't merge this card, if something moved that shouldn't have.
    merge_block: Option<String>,
    target_at_card: Option<String>,
}

impl Pipeline<'_> {
    fn intake(&mut self) -> anyhow::Result<()> {
        let agent = self.config.agent(self.adapter.id());
        self.record_run(RunEventKind::RunStarted(RunStarted {
            task: self.card.task.clone(),
            mode: "solo".to_owned(),
            agent: self.adapter.id().to_owned(),
            model: agent.model,
            effort: agent.effort,
            repo_root: self.start.root.clone(),
            base_branch: self.start.base_branch.clone(),
            base_commit: self.start.base_commit.clone(),
            config_hash: self.config.resolved_hash(),
            config: self.config.to_json(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }))?;
        let on = match &self.start.base_branch {
            Some(branch) => format!("on {branch}"),
            None => "with a detached HEAD".to_owned(),
        };
        let detail = format!("starting from {} {on}", short(&self.start.base_commit));
        self.record_run(phase_kind(
            Phase::Intake,
            PhaseStatus::Finished,
            Some(detail),
        ))?;
        let goal = format!(
            "your prompt, recorded as the goal · sha256 {} · solo.restate_first is off",
            short(&self.goal_hash)
        );
        self.record_run(phase_kind(Phase::Goal, PhaseStatus::Finished, Some(goal)))?;
        if self.start.uncommitted > 0 {
            self.note(
                NoteLevel::Info,
                "Your uncommitted changes are not part of this run. The agent starts from the last commit.",
            )?;
        }
        Ok(())
    }

    async fn execute<D: MergeDecider>(&mut self, decider: &mut D) -> anyhow::Result<Ending> {
        // A request the adapter rejects fails here, before there is a worktree to clean up.
        self.adapter
            .command(&self.build_request())
            .context("the agent command could not be built")?;
        self.create_worktree().await?;
        if self.stop.is_stopped() {
            // The fresh worktree holds nothing, so cleanup may remove it and its branch.
            self.work_saved = true;
            self.tip = Some(self.start.base_commit.clone());
            return Ok(self.cancelled(false));
        }
        let agent = self.run_agent().await?;
        let committed = self.commit(&agent).await?;

        if self.stop.is_stopped() {
            return Ok(self.cancelled(committed.is_some()));
        }
        let Some(committed) = committed else {
            return self.no_changes(&agent);
        };
        let target_before_gates = self.target_tip().await;
        let gates = self.run_gates().await?;
        if self.stop.is_stopped() {
            return Ok(self.cancelled(true));
        }
        self.check_refs_after_gates(&committed, target_before_gates)
            .await?;

        let card = MergeCard {
            branch: self.branch.clone(),
            target_branch: self.start.base_branch.clone(),
            stat: committed.stat,
            gates,
            report: agent.report.clone(),
            budget: Some(agent.budget),
            stopped_by: agent.session.stopped_by,
        };
        let Some(decision) = self.decide(card.clone(), &committed, decider).await? else {
            return Ok(self.cancelled(true));
        };
        self.act(decision, &card, &committed).await
    }

    /// Blocks the merge if a gate, or anything else, moved the card branch or the target branch
    /// while the gates ran. Gates run code the agent wrote.
    async fn check_refs_after_gates(
        &mut self,
        committed: &Committed,
        target_before_gates: Option<String>,
    ) -> anyhow::Result<()> {
        let target_now = self.target_tip().await;
        let target = self.start.base_branch.clone().unwrap_or_default();
        if target_now != target_before_gates {
            self.block_merge(format!(
                "{target} moved while the gates ran, from {} to {}. A gate may have changed it, so this card won't be merged.",
                short_or_none(target_before_gates.as_deref()),
                short_or_none(target_now.as_deref())
            ))?;
        } else if let Some(now) = &target_now
            && *now != self.start.base_commit
        {
            // You may have committed meanwhile, but so could something the agent ran.
            self.note(
                NoteLevel::Warn,
                &format!(
                    "{target} moved since the run started and is now at {}. Check it before approving; approving merges the card into it as it is now.",
                    short(now)
                ),
            )?;
        }
        let branch_now = self.branch_tip().await;
        if branch_now.as_deref() != Some(committed.tip.as_str()) {
            self.block_merge(format!(
                "The card branch moved after the conductor's commit, to {}. A gate may have changed it, so this card won't be merged.",
                short_or_none(branch_now.as_deref())
            ))?;
        }
        self.target_at_card = target_now;
        Ok(())
    }

    fn block_merge(&mut self, reason: String) -> anyhow::Result<()> {
        self.note(NoteLevel::Warn, &reason)?;
        self.merge_block.get_or_insert(reason);
        Ok(())
    }

    fn target_tip(&self) -> impl Future<Output = Option<String>> + Send + 'static {
        self.branch_tip_of(self.start.base_branch.clone())
    }

    fn branch_tip(&self) -> impl Future<Output = Option<String>> + Send + 'static {
        self.branch_tip_of(Some(self.branch.clone()))
    }

    // Owns what it needs, so the pipeline isn't borrowed across the await.
    fn branch_tip_of(
        &self,
        branch: Option<String>,
    ) -> impl Future<Output = Option<String>> + Send + 'static {
        let git = self.git.clone();
        let root = self.start.root.clone();
        async move {
            let rev = format!("refs/heads/{}", branch?);
            git.resolve(&root, &rev).await.ok()
        }
    }

    fn build_request(&self) -> SessionRequest {
        let agent = self.config.agent(self.adapter.id());
        let schema = report_json_schema();
        SessionRequest {
            workdir: self.worktree.clone(),
            prompt: render_solo_prompt(&self.card, &schema),
            model: agent.model,
            effort: agent.effort,
            role: SessionRole::Build,
            auth: agent.auth,
            json_schema: Some(schema),
            resume_session: None,
            max_budget_usd: native_cap(self.card.budget, 0.0),
            extra_args: Vec::new(),
            scratch_dir: Some(self.card_dir.clone()),
            goal_hash: Some(self.goal_hash.clone()),
            ..SessionRequest::default()
        }
    }

    fn worktree_git(&self) -> anyhow::Result<Git> {
        self.worktree_git
            .clone()
            .ok_or_else(|| anyhow!("the worktree isn't set up"))
    }

    async fn create_worktree(&mut self) -> anyhow::Result<()> {
        self.phase(Phase::Worktree, PhaseStatus::Started, None)?;
        if let Err(err) = self.add_worktree().await {
            return Err(self.phase_failed(Phase::Worktree, err));
        }
        self.worktree_created = true;
        if let Err(err) = self.bind_worktree_git().await {
            // Nothing ran in it yet, so cleanup may remove it.
            self.work_saved = true;
            self.tip = Some(self.start.base_commit.clone());
            return Err(self.phase_failed(Phase::Worktree, err));
        }
        let detail = format!("{} at {}", self.branch, self.worktree.display());
        self.phase(Phase::Worktree, PhaseStatus::Finished, Some(detail))?;
        Ok(())
    }

    async fn add_worktree(&mut self) -> anyhow::Result<()> {
        if let Some(parent) = self.worktree.parent() {
            create_private_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        self.git
            .add_worktree(
                &self.start.root,
                &self.worktree,
                &self.branch,
                &self.start.base_commit,
            )
            .await?;
        Ok(())
    }

    /// Binds git to the worktree's real git directory and keeps a copy of its `.git` file. Done
    /// before the agent runs, so later git calls never trust what it may rewrite.
    async fn bind_worktree_git(&mut self) -> anyhow::Result<()> {
        let git_dir = self.git.absolute_git_dir(&self.worktree).await?;
        let dot_git_path = self.worktree.join(".git");
        let dot_git = std::fs::read(&dot_git_path)
            .with_context(|| format!("reading {}", dot_git_path.display()))?;
        self.worktree_git = Some(self.git.bound_to(git_dir, self.worktree.clone()));
        self.dot_git = Some(dot_git);
        Ok(())
    }

    async fn run_agent(&mut self) -> anyhow::Result<AgentRun> {
        let agent = self.config.agent(self.adapter.id());
        let model_name = agent.model.as_deref().unwrap_or("its default model");
        let detail = format!("{} with {model_name}", self.adapter.display_name());
        self.phase(Phase::Agent, PhaseStatus::Started, Some(detail))?;
        match self.agent_sessions().await {
            Ok(run) => Ok(run),
            Err(err) => Err(self.phase_failed(Phase::Agent, err)),
        }
    }

    async fn agent_sessions(&mut self) -> anyhow::Result<AgentRun> {
        let agent = self.config.agent(self.adapter.id());
        create_private_dir_all(&self.card_dir)
            .with_context(|| format!("creating {}", self.card_dir.display()))?;

        let mut meter = BudgetMeter::new(self.card.budget, self.config.budget.soft_ratio)
            .with_price(agent.effective_price())
            .with_work_limits(self.config.budget.work_limits());
        self.record(RunEventKind::Budget(meter.status()))?;

        let request = self.build_request();
        let mut session = self.run_session(&mut meter, &request).await?;
        let mut report = session.report();

        // A CLI that can't take input mid-run was interrupted to wrap up. Resume it once so it
        // can still send its report.
        let resumable = session.wrap_up == Some(WrapUpDelivery::Interrupt)
            && session.stopped_by.is_none()
            && report.is_none()
            && meter.level() != BudgetAction::Stop
            && !self.stop.is_stopped();
        if resumable
            && let Some(session_id) = session.session_id.clone()
            && self.safe_to_resume().await?
        {
            session = self
                .resume_for_report(&mut meter, request, session_id)
                .await?;
            report = session.report();
        }

        match &report {
            Some(report) => self.record(RunEventKind::Report(report.clone()))?,
            None => self.note(NoteLevel::Warn, "The agent did not send a valid report.")?,
        }
        match session.failure() {
            None => {
                let detail = report.as_ref().map(|r| first_line(&r.summary).to_owned());
                self.phase(Phase::Agent, PhaseStatus::Finished, detail)?;
            }
            Some(failure) => self.phase(Phase::Agent, PhaseStatus::Failed, Some(failure))?,
        }
        Ok(AgentRun {
            session,
            report,
            budget: meter.status(),
        })
    }

    /// Whether the worktree is still safe to start another session in. A resumed CLI would load
    /// agent config files the agent wrote, and could run hooks from them.
    async fn safe_to_resume(&mut self) -> anyhow::Result<bool> {
        let git = self.worktree_git()?;
        let changed = match git.status_of(&self.worktree, AGENT_CONFIG_PATHS).await {
            Ok(changed) => changed,
            Err(err) => vec![format!("could not check them: {err}")],
        };
        if changed.is_empty() {
            return Ok(true);
        }
        let list: Vec<&str> = changed
            .iter()
            .map(|line| line.get(3..).unwrap_or(line))
            .collect();
        self.note(
            NoteLevel::Warn,
            &format!(
                "Not resuming the session for its report, because the agent changed files that configure agent CLIs: {}.",
                list.join(", ")
            ),
        )?;
        Ok(false)
    }

    /// Resumes an interrupted session with the wrap-up message, under what is left of the budget.
    async fn resume_for_report(
        &mut self,
        meter: &mut BudgetMeter,
        mut request: SessionRequest,
        session_id: String,
    ) -> anyhow::Result<SessionOutcome> {
        self.note(
            NoteLevel::Info,
            "Resuming the session so the agent can send its report.",
        )?;
        let spent_before = if self.adapter.capabilities().resume_totals_include_earlier {
            meter.continue_conversation();
            0.0
        } else {
            meter.start_new_session();
            meter.spend().cost_usd
        };
        request.prompt = WRAP_UP_MESSAGE.to_owned();
        request.resume_session = Some(session_id);
        request.max_budget_usd = native_cap(self.card.budget, spent_before);
        self.run_session(meter, &request).await
    }

    async fn run_session(
        &mut self,
        meter: &mut BudgetMeter,
        request: &SessionRequest,
    ) -> anyhow::Result<SessionOutcome> {
        Supervisor::new(
            self.adapter,
            &self.card.id,
            &mut self.recorder,
            meter,
            self.stop,
            self.settings,
        )
        .run_session(request)
        .await
    }

    /// `None` means the branch has no changes.
    async fn commit(&mut self, agent: &AgentRun) -> anyhow::Result<Option<Committed>> {
        self.phase(Phase::Commit, PhaseStatus::Started, None)?;
        match self.try_commit(agent).await {
            Ok(committed) => Ok(committed),
            Err(err) => Err(self.phase_failed(Phase::Commit, err)),
        }
    }

    async fn try_commit(&mut self, agent: &AgentRun) -> anyhow::Result<Option<Committed>> {
        let git = self.intact_worktree_git().await?;
        let message = self.commit_message(agent.report.as_ref());
        let sha = git.commit_all(&self.worktree, &message).await?;
        self.work_saved = true;
        let tip = git.resolve(&self.worktree, "HEAD").await?;
        let changed = tip != self.start.base_commit;
        self.tip = Some(tip.clone());
        let stat = if changed {
            git.diff_stat(&self.worktree, &self.start.base_commit, &tip)
                .await?
        } else {
            DiffStat::default()
        };
        self.record(RunEventKind::Commit(CommitInfo {
            branch: self.branch.clone(),
            sha: sha.clone(),
            message,
            stat,
            by: None,
        }))?;

        let (status, detail) = match (&sha, changed) {
            (Some(sha), _) => (
                PhaseStatus::Finished,
                format!("{} · {}", short(sha), describe_stat(&stat)),
            ),
            (None, true) => {
                self.note(
                    NoteLevel::Warn,
                    "The agent made its own commits on the card branch. They are kept as they are.",
                )?;
                (PhaseStatus::Skipped, "nothing new to commit".to_owned())
            }
            (None, false) => (PhaseStatus::Skipped, "no changes".to_owned()),
        };
        self.phase(Phase::Commit, status, Some(detail))?;
        if !changed {
            return Ok(None);
        }
        self.warn_about_protected_paths(&tip).await?;
        Ok(Some(Committed { tip, stat }))
    }

    /// Git for the worktree, after checking the agent neither repointed its `.git` file nor
    /// moved it off the card branch.
    async fn intact_worktree_git(&mut self) -> anyhow::Result<Git> {
        let dot_git_path = self.worktree.join(".git");
        if std::fs::read(&dot_git_path).ok() != self.dot_git {
            anyhow::bail!(
                "the worktree's .git file was changed, so its changes were not committed; review {} before using it",
                self.worktree.display()
            );
        }
        let git = self.worktree_git()?;
        let on_branch = git.current_branch(&self.worktree).await?;
        if on_branch.as_deref() != Some(self.branch.as_str()) {
            anyhow::bail!(
                "the worktree is no longer on {}, so its changes were not committed",
                self.branch
            );
        }
        Ok(git)
    }

    async fn warn_about_protected_paths(&mut self, tip: &str) -> anyhow::Result<()> {
        let paths = self
            .worktree_git()?
            .changed_paths(&self.worktree, &self.start.base_commit, tip)
            .await?;
        let touched: Vec<String> = paths.into_iter().filter(|p| is_protected(p)).collect();
        if !touched.is_empty() {
            self.note(
                NoteLevel::Warn,
                &format!(
                    "The agent changed Argonauts or agent config files, which are off limits: {}. Review them before merging.",
                    touched.join(", ")
                ),
            )?;
        }
        Ok(())
    }

    fn commit_message(&self, report: Option<&Report>) -> String {
        let summary = report
            .map(|r| r.summary.trim())
            .filter(|s| !s.is_empty())
            .map(|s| format!("\n\n{s}"))
            .unwrap_or_default();
        let model = self
            .config
            .agent(self.adapter.id())
            .model
            .map(|m| format!("/{m}"))
            .unwrap_or_default();
        format!(
            "{title}{summary}\n\nArgonauts-Run: {run}\nArgonauts-Card: {card}\nArgonauts-Agent: {agent}{model}",
            title = self.card.title,
            run = self.run_id,
            card = self.card.id,
            agent = self.adapter.id(),
        )
    }

    async fn run_gates(&mut self) -> anyhow::Result<Vec<GateResult>> {
        let commands = &self.config.gates.commands;
        if commands.is_empty() {
            let detail = "no gate commands configured".to_owned();
            self.phase(Phase::Gates, PhaseStatus::Skipped, Some(detail))?;
            return Ok(Vec::new());
        }
        let count = commands.len();
        self.phase(
            Phase::Gates,
            PhaseStatus::Started,
            Some(plural(count, "command")),
        )?;
        let mut results = Vec::with_capacity(count);
        for command in commands {
            if self.stop.is_stopped() {
                break;
            }
            let result = run_gate(
                command,
                &self.worktree,
                self.settings.gate_timeout,
                self.stop,
            )
            .await;
            self.record(RunEventKind::GateResult(result.clone()))?;
            results.push(result);
        }
        let passed = results.iter().filter(|r| r.passed).count();
        let status = if passed == count {
            PhaseStatus::Finished
        } else {
            PhaseStatus::Failed
        };
        self.phase(
            Phase::Gates,
            status,
            Some(format!("{passed} of {count} passed")),
        )?;
        Ok(results)
    }

    /// `None` means the run was stopped instead.
    async fn decide<D: MergeDecider>(
        &mut self,
        card: MergeCard,
        committed: &Committed,
        decider: &mut D,
    ) -> anyhow::Result<Option<MergeDecision>> {
        let diff = self
            .worktree_git()?
            .diff(&self.worktree, &self.start.base_commit, &committed.tip)
            .await?;
        let diff_path = self.card_dir.join("diff.patch");
        write_private_file(&diff_path, diff.as_bytes())
            .with_context(|| format!("writing {}", diff_path.display()))?;
        let (diff, diff_truncated) = truncate(diff, MAX_DIFF_BYTES);

        self.record(RunEventKind::Checkpoint(Checkpoint::Merge(card.clone())))?;
        self.phase(Phase::Decision, PhaseStatus::Started, None)?;
        let (decision, by) = match self.config.merge.mode {
            MergeMode::Leave => (MergeDecision::Leave, DecidedBy::Config),
            MergeMode::Ask => {
                let request = MergeRequest {
                    run_id: self.run_id.clone(),
                    card_id: self.card.id.clone(),
                    title: self.card.title.clone(),
                    card,
                    diff,
                    diff_truncated,
                    diff_path,
                };
                let Some(answer) = self.ask(request, decider).await? else {
                    return Ok(None);
                };
                answer
            }
        };
        self.record(RunEventKind::Decision(DecisionRecord {
            checkpoint: CheckpointKind::Merge,
            decision: decision.clone(),
            by,
            commit: Some(committed.tip.clone()),
        }))?;
        self.phase(
            Phase::Decision,
            PhaseStatus::Finished,
            Some(decision_label(&decision).to_owned()),
        )?;
        Ok(Some(decision))
    }

    /// A stop gives `None`. A decider that fails leaves the branch.
    async fn ask<D: MergeDecider>(
        &mut self,
        request: MergeRequest,
        decider: &mut D,
    ) -> anyhow::Result<Option<(MergeDecision, DecidedBy)>> {
        let stop = self.stop;
        let answer = tokio::select! {
            answer = decider.decide(request) => answer,
            () = stop.stopped() => {
                self.phase(Phase::Decision, PhaseStatus::Skipped, Some("stopped".to_owned()))?;
                return Ok(None);
            }
        };
        match answer {
            Ok(decision) => Ok(Some((decision, DecidedBy::User))),
            Err(err) => {
                self.note(
                    NoteLevel::Warn,
                    &format!(
                        "No merge decision arrived ({err:#}), so the branch is left as it is."
                    ),
                )?;
                Ok(Some((MergeDecision::Leave, DecidedBy::Config)))
            }
        }
    }

    async fn act(
        &mut self,
        decision: MergeDecision,
        card: &MergeCard,
        committed: &Committed,
    ) -> anyhow::Result<Ending> {
        match decision {
            MergeDecision::Approve => self.merge(card, committed).await,
            MergeDecision::RequestChanges { note } => self.record_change_request(note.as_deref()),
            MergeDecision::OpenPr => self.leave_for_pull_request(card),
            MergeDecision::Leave => {
                self.phase(Phase::Merge, PhaseStatus::Skipped, Some("left".to_owned()))?;
                Ok(Ending::new(
                    RunOutcome::BranchLeft,
                    format!("Left {} as it is.", self.branch),
                ))
            }
        }
    }

    fn record_change_request(&mut self, note: Option<&str>) -> anyhow::Result<Ending> {
        let asked = note
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(|n| format!(" ({n})"))
            .unwrap_or_default();
        self.phase(
            Phase::Merge,
            PhaseStatus::Skipped,
            Some("changes requested".to_owned()),
        )?;
        let branch = &self.branch;
        Ok(Ending::new(
            RunOutcome::BranchLeft,
            format!(
                "Recorded your change request{asked}. Follow-up sessions aren't supported yet, so {branch} is left for you."
            ),
        ))
    }

    fn leave_for_pull_request(&mut self, card: &MergeCard) -> anyhow::Result<Ending> {
        self.phase(
            Phase::Merge,
            PhaseStatus::Skipped,
            Some("pull request".to_owned()),
        )?;
        let branch = self.branch.clone();
        let push = CommandSpec::new("git", ".").args(["push", "-u", "origin", &branch]);
        let mut pr = CommandSpec::new("gh", ".").args(["pr", "create", "--head", &branch]);
        if let Some(target) = &card.target_branch {
            pr = pr.args(["--base", target.as_str()]);
        }
        let pr = pr.arg("--fill");
        self.note(
            NoteLevel::Info,
            &format!(
                "To open a pull request, run:\n  {}\n  {}",
                push.display_line(),
                pr.display_line()
            ),
        )?;
        Ok(Ending::new(
            RunOutcome::BranchLeft,
            format!("Left {branch} for a pull request."),
        ))
    }

    /// Merges the reviewed commit into the branch checked out at the start, but only into a clean
    /// tree, and only while both branches are where they were when the card was shown.
    async fn merge(&mut self, card: &MergeCard, committed: &Committed) -> anyhow::Result<Ending> {
        let branch = self.branch.clone();
        let Some(target) = self.start.base_branch.clone() else {
            return self.merge_skipped(
                "No branch was checked out when the run started, so there is nothing to merge into.",
            );
        };
        self.phase(
            Phase::Merge,
            PhaseStatus::Started,
            Some(format!("into {target}")),
        )?;
        if let Some(reason) = self.merge_obstacle(committed, &target).await? {
            return self.merge_skipped(&reason);
        }
        if card.gates.iter().any(|g| !g.passed) {
            self.note(
                NoteLevel::Warn,
                "Merging although a gate failed, because you approved.",
            )?;
        }
        let message = format!("Merge {branch} ({})", short(&committed.tip));
        match self
            .git
            .merge(&self.start.root, &committed.tip, &message)
            .await
        {
            Ok(()) => {
                self.phase(
                    Phase::Merge,
                    PhaseStatus::Finished,
                    Some(format!("into {target}")),
                )?;
                Ok(Ending::new(
                    RunOutcome::Merged,
                    format!(
                        "Merged {branch} into {target} at {}.",
                        short(&committed.tip)
                    ),
                ))
            }
            Err(err) => {
                self.phase(Phase::Merge, PhaseStatus::Failed, Some(err.to_string()))?;
                Ok(Ending::new(
                    RunOutcome::BranchLeft,
                    format!("The merge failed and was undone, so {branch} is left for you: {err}"),
                ))
            }
        }
    }

    async fn merge_obstacle(
        &mut self,
        committed: &Committed,
        target: &str,
    ) -> anyhow::Result<Option<String>> {
        if let Some(reason) = &self.merge_block {
            return Ok(Some(reason.clone()));
        }
        if self.branch_tip().await.as_deref() != Some(committed.tip.as_str()) {
            let reason = "The card branch moved after you reviewed it, so nothing was merged.";
            return Ok(Some(reason.to_owned()));
        }
        if self.target_tip().await != self.target_at_card {
            return Ok(Some(format!(
                "{target} moved while the merge card was open, so nothing was merged."
            )));
        }
        let current = self.git.current_branch(&self.start.root).await?;
        if current.as_deref() != Some(target) {
            let now = current.unwrap_or_else(|| "a detached HEAD".to_owned());
            return Ok(Some(format!(
                "Your working tree is now on {now}, not {target}, so nothing was merged."
            )));
        }
        let changes = self.git.tracked_changes(&self.start.root).await?;
        if !changes.is_empty() {
            return Ok(Some(format!(
                "Your working tree has uncommitted changes to {}, so nothing was merged.",
                plural(changes.len(), "file")
            )));
        }
        Ok(None)
    }

    fn merge_skipped(&mut self, reason: &str) -> anyhow::Result<Ending> {
        self.phase(Phase::Merge, PhaseStatus::Skipped, Some(reason.to_owned()))?;
        let branch = &self.branch;
        Ok(Ending::new(
            RunOutcome::BranchLeft,
            format!("{reason} {branch} is left; merge it later with `git merge {branch}`."),
        ))
    }

    fn no_changes(&mut self, agent: &AgentRun) -> anyhow::Result<Ending> {
        self.phase(
            Phase::Gates,
            PhaseStatus::Skipped,
            Some("no changes".to_owned()),
        )?;
        let detail = match (agent.session.failure(), &agent.report) {
            (Some(failure), _) => format!("The agent made no changes: {failure}."),
            (None, Some(report)) => format!(
                "The agent made no changes. It reported: {}",
                first_line(&report.summary)
            ),
            (None, None) => "The agent made no changes.".to_owned(),
        };
        Ok(Ending::new(RunOutcome::Failed, detail))
    }

    fn cancelled(&self, has_changes: bool) -> Ending {
        let detail = if has_changes {
            format!("Stopped. The work so far is committed on {}.", self.branch)
        } else {
            "Stopped before the agent changed anything.".to_owned()
        };
        Ending::new(RunOutcome::Cancelled, detail)
    }

    async fn finish(
        mut self,
        ending: anyhow::Result<Ending>,
        event_log: PathBuf,
    ) -> anyhow::Result<SoloOutcome> {
        let Ending { outcome, detail } = match ending {
            Ok(ending) => ending,
            Err(err) => Ending::new(RunOutcome::Failed, format!("{err:#}")),
        };
        let (kept_worktree, branch_exists) = self.cleanup().await?;
        self.record_run(RunEventKind::RunFinished(RunFinished {
            outcome,
            detail: Some(detail.clone()),
        }))?;
        self.recorder.flush()?;
        let changed = self
            .tip
            .as_ref()
            .filter(|tip| **tip != self.start.base_commit);
        Ok(SoloOutcome {
            run_id: self.run_id.clone(),
            outcome,
            detail,
            branch: branch_exists.then(|| self.branch.clone()),
            commit: changed.cloned(),
            worktree: kept_worktree,
            event_log,
        })
    }

    /// Removes the worktree once its work is committed, and the branch if it holds nothing new.
    /// Returns the worktree if it was kept, and whether the branch still exists.
    async fn cleanup(&mut self) -> anyhow::Result<(Option<PathBuf>, bool)> {
        if !self.worktree_created {
            return Ok((None, false));
        }
        if !self.work_saved {
            let detail = format!("kept {}", self.worktree.display());
            self.phase(Phase::Cleanup, PhaseStatus::Skipped, Some(detail))?;
            self.note(
                NoteLevel::Warn,
                &format!(
                    "The worktree at {} was kept because its changes aren't committed.",
                    self.worktree.display()
                ),
            )?;
            return Ok((Some(self.worktree.clone()), true));
        }
        self.phase(Phase::Cleanup, PhaseStatus::Started, None)?;
        if let Err(err) = self
            .git
            .remove_worktree(&self.start.root, &self.worktree)
            .await
        {
            self.note(
                NoteLevel::Warn,
                &format!("Could not remove the worktree: {err}"),
            )?;
            self.phase(Phase::Cleanup, PhaseStatus::Failed, Some(err.to_string()))?;
            return Ok((Some(self.worktree.clone()), true));
        }
        let empty_branch = self.tip.as_deref() == Some(self.start.base_commit.as_str());
        let mut branch_exists = true;
        if empty_branch {
            match self.git.delete_branch(&self.start.root, &self.branch).await {
                Ok(()) => branch_exists = false,
                Err(err) => self.note(
                    NoteLevel::Warn,
                    &format!("Could not delete the empty card branch: {err}"),
                )?,
            }
        }
        let detail = if branch_exists {
            "removed the worktree"
        } else {
            "removed the worktree and the empty branch"
        };
        self.phase(
            Phase::Cleanup,
            PhaseStatus::Finished,
            Some(detail.to_owned()),
        )?;
        Ok((None, branch_exists))
    }

    fn phase_failed(&mut self, phase: Phase, err: anyhow::Error) -> anyhow::Error {
        if let Err(log_err) = self.phase(phase, PhaseStatus::Failed, Some(format!("{err:#}"))) {
            return log_err;
        }
        err
    }

    fn phase(
        &mut self,
        phase: Phase,
        status: PhaseStatus,
        detail: Option<String>,
    ) -> anyhow::Result<()> {
        self.record(phase_kind(phase, status, detail))
    }

    fn note(&mut self, level: NoteLevel, text: &str) -> anyhow::Result<()> {
        self.record(RunEventKind::Note(Note {
            level,
            text: text.to_owned(),
        }))
    }

    fn record(&mut self, kind: RunEventKind) -> anyhow::Result<()> {
        self.recorder
            .record(Some(&self.card.id), CONDUCTOR_SOURCE, kind)?;
        Ok(())
    }

    fn record_run(&mut self, kind: RunEventKind) -> anyhow::Result<()> {
        self.recorder.record(None, CONDUCTOR_SOURCE, kind)?;
        Ok(())
    }
}

fn phase_kind(phase: Phase, status: PhaseStatus, detail: Option<String>) -> RunEventKind {
    RunEventKind::Phase(PhaseEvent {
        phase,
        status,
        detail,
    })
}

/// The CLI's own spend cap, set to what is left of a dollar budget after `spent` plus a margin
/// so the meter acts first. `None` when there is no dollar budget or nothing is left.
fn native_cap(limit: BudgetLimit, spent: f64) -> Option<f64> {
    match limit {
        BudgetLimit::Usd(amount) => {
            Some((amount - spent) * NATIVE_CAP_MARGIN).filter(|cap| *cap > 0.0)
        }
        _ => None,
    }
}

fn is_protected(path: &str) -> bool {
    PROTECTED_PATHS
        .iter()
        .any(|protected| match protected.strip_suffix('/') {
            Some(dir) => path.starts_with(protected) || path == dir,
            None => path == *protected,
        })
}

fn short_or_none(sha: Option<&str>) -> &str {
    sha.map_or("nothing", short)
}

fn decision_label(decision: &MergeDecision) -> &'static str {
    match decision {
        MergeDecision::Approve => "approved",
        MergeDecision::RequestChanges { .. } => "changes requested",
        MergeDecision::OpenPr => "open a pull request",
        MergeDecision::Leave => "leave the branch",
    }
}

fn describe_stat(stat: &DiffStat) -> String {
    format!(
        "{} +{} −{}",
        plural(stat.files_changed as usize, "file"),
        stat.insertions,
        stat.deletions
    )
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// Cuts `text` to at most `max` bytes on a character boundary. Returns whether it was cut.
fn truncate(mut text: String, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut cut = max;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    (text, true)
}

#[cfg(test)]
mod tests;
