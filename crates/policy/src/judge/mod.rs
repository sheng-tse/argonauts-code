//! Your judge: independent of every author and worker, in a fresh no-tools session,
//! reading only the evidence packet with every document fenced as data. It decides only below
//! the hard limits, and anything that goes wrong comes to you.

pub mod independence;
pub mod pick;
pub mod render;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use argonauts_core::agents::session_runner::RunnerDirectory;
use argonauts_core::base::future::BoxFuture;
use argonauts_core::base::layout::{repo_state, run_files};
use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::models::registry::ModelRegistry;
use argonauts_core::runs::events::session::{EndReason, InputPartKind, loaded_words};
use argonauts_core::safety::policy::{DecisionPoint, EvidencePacket, Verdict};
use argonauts_core::settings::Settings;
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::workflows::steps::judgment::{
    ModelJudge, ModelJudgeDecision, ModelJudgeVerdict,
};
use argonauts_core::workflows::steps::{
    StepError, StepName, StepRequest, StepRunner, decode, output_schema,
};

use crate::approvals::parts::{Approve, ModelVerdict};
use crate::sessions::session::{
    self, NoToolsSession, TimedOut, claim_session, clean_text, no_tools_spec, part, run_step,
};

/// The prompt template the evidence is rendered into.
pub const PROMPT: &str = argonauts_core::template!("prompts/approvals/judge.md");

/// An ask_you verdict's reason is cut to this many characters.
const REASON_MAX: usize = 300;

/// What fixes a judge that may not judge the work.
const PICK_ANOTHER: &str = "Pick another judge in /approvals";

/// The judges you named, in your order of preference: `roles.judge.models`, else the crew's
/// judge, else the reviewers a judge you left out follows. They are who
/// `approvals.decides = "judge"` means, and the policy picks one itself only when none of them
/// may check the work. A crew with everyone in every role, as the built-in one and a single
/// `crew = [...]` list have, names no judge of its own.
pub fn judges(settings: &Settings) -> Vec<ModelRef> {
    let fine = settings
        .roles
        .get(&RoleName::Judge)
        .map(|role| role.models.clone())
        .unwrap_or_default();
    if !fine.is_empty() {
        return fine;
    }
    let crew = &settings.crew;
    if !crew.judge.is_empty() {
        let everyone = crew.judge == crew.worker && crew.judge == crew.reviewer;
        return if everyone {
            Vec::new()
        } else {
            crew.judge.clone()
        };
    }
    match crew.followed(CrewRole::Judge) {
        Some(CrewRole::Reviewer) if crew.reviewer != crew.worker => crew.reviewer.clone(),
        _ => Vec::new(),
    }
}

pub struct StepJudge {
    workers: Arc<dyn RunnerDirectory>,
    /// The live model lists an unset judge is picked from, beside each agent's cheapest.
    models: Option<Arc<dyn ModelRegistry>>,
    steps: Arc<dyn StepRunner>,
    /// `~/.argonauts/state/<repo>`: judge sessions go in the run's `sessions/` folder, and
    /// big evidence documents sit beside the packet in `runs/<run>/evidence/`.
    state_dir: PathBuf,
    /// How long one judge session may take before it's stopped and you're asked instead.
    deadline: Duration,
    /// Set once the fuller warning about having no judge to pick was given.
    warned: AtomicBool,
}

impl StepJudge {
    pub fn new(
        workers: Arc<dyn RunnerDirectory>,
        steps: Arc<dyn StepRunner>,
        state_dir: PathBuf,
    ) -> Self {
        Self {
            workers,
            models: None,
            steps,
            state_dir,
            deadline: session::DEADLINE,
            warned: AtomicBool::new(false),
        }
    }

    /// Picks an unset judge from these model lists too, not only each agent's cheapest.
    pub fn with_models(mut self, models: Arc<dyn ModelRegistry>) -> Self {
        self.models = Some(models);
        self
    }

    /// The judge you set, else the first judge you named that may check this work, else the
    /// cheapest model from another provider than the authors, with the authors it must be
    /// independent of. In autopilot, when none comes from outside every author, as with a crew of
    /// every provider you run, it's the cheapest from another provider than the main agent, whose
    /// answers it checks. The first time none can be picked, the reason is the fuller warning.
    fn first_judge(
        &self,
        point: &DecisionPoint,
        settings: &Settings,
    ) -> Result<(ModelRef, Vec<ModelRef>), String> {
        let judge_settings = &settings.approvals.judge;
        if let Some(model) = settings.approvals.judge_model() {
            return Ok((model.clone(), point.authors.clone()));
        }
        if let Some(judge) = self.named_judge(point, settings) {
            return Ok((judge, point.authors.clone()));
        }
        let sources = pick::Sources {
            workers: &*self.workers,
            models: self.models.as_deref(),
        };
        let none = match pick::pick(&point.authors, judge_settings.different_provider, sources) {
            Ok(first) => return Ok((first, point.authors.clone())),
            Err(none) => none,
        };
        if crate::approvals::in_autopilot(point)
            && let Some(main_agent) = &point.main_agent
        {
            let main_agent = vec![main_agent.clone()];
            if let Ok(first) = pick::pick(&main_agent, judge_settings.different_provider, sources) {
                return Ok((first, main_agent));
            }
        }
        Err(if self.warned.swap(true, Ordering::SeqCst) {
            none.reason()
        } else {
            none.warning(crate::approvals::in_autopilot(point))
        })
    }

    /// The first judge you named that you can run and that may check this work: none of its
    /// authors, and from another provider than each of them where your settings ask for that.
    fn named_judge(&self, point: &DecisionPoint, settings: &Settings) -> Option<ModelRef> {
        let different_provider = settings.approvals.judge.different_provider;
        judges(settings)
            .iter()
            .find(|judge| {
                self.workers.get(judge).is_ok()
                    && independence::check(
                        judge,
                        &point.authors,
                        different_provider,
                        &*self.workers,
                    )
                    .is_ok()
            })
            .cloned()
    }

    async fn decide(
        &self,
        point: &DecisionPoint,
        evidence: &EvidencePacket,
        settings: &Settings,
    ) -> ModelVerdict {
        let judge_settings = &settings.approvals.judge;
        let (first, authors) = match self.first_judge(point, settings) {
            Ok(picked) => picked,
            Err(why) => return ask_you(None, &why),
        };
        if let Err(why) = independence::check(
            &first,
            &authors,
            judge_settings.different_provider,
            &*self.workers,
        ) {
            return ask_you(None, &format!("{why}. {PICK_ANOTHER}"));
        }
        // Judges whose sessions failed for this approval, left out of every fallback.
        let mut failed = Vec::new();
        let Some(second) = judge_settings.second_model.clone() else {
            return match self
                .ask_or_fall_back(&first, &authors, &mut failed, point, evidence, settings)
                .await
            {
                Ok((model, answer)) => answer.verdict(model),
                Err(why) => unchecked(&why),
            };
        };
        // The second must be independent of the authors and, always, of the first's provider.
        let independent = independence::check(
            &second,
            &authors,
            judge_settings.different_provider,
            &*self.workers,
        )
        .and_then(|()| {
            independence::check(&second, std::slice::from_ref(&first), true, &*self.workers)
        });
        if let Err(why) = independent {
            return ask_you(None, &format!("second judge: {why}. {PICK_ANOTHER}"));
        }

        // Asked one after the other, so a first answer other than approve spends nothing more.
        let (first, first_answer) = match self
            .ask_or_fall_back(&first, &authors, &mut failed, point, evidence, settings)
            .await
        {
            Ok(answered) => answered,
            Err(why) => return unchecked(&why),
        };
        if first_answer.decision != Verdict::Approve {
            return first_answer.labeled(&first).verdict(first);
        }
        // Whoever answered first, a fallback included, the second comes from another provider
        // than it and the authors, so both approvals never come from one provider.
        let mut others = authors;
        others.push(first.clone());
        let second = match self.second_besides(second, &first, &others, &failed, settings) {
            Ok(second) => second,
            Err(why) => return unchecked(&why),
        };
        // A second that couldn't answer leaves the work unchecked: autopilot lets your rules
        // decide alone, and elsewhere it's yours.
        let (second, second_answer) = match self
            .ask_or_fall_back(&second, &others, &mut failed, point, evidence, settings)
            .await
        {
            Ok(answered) => answered,
            Err(why) => return unchecked(&why),
        };
        let verdict = second_answer.decision;
        let mut reasons = first_answer.labeled(&first).reasons;
        reasons.extend(second_answer.labeled(&second).reasons);
        ModelVerdict {
            verdict,
            judge: Some(first),
            reasons,
        }
    }

    /// The second judge you set, when it comes from another provider than `first`, the agent
    /// that answered first; else the cheapest from another provider than all of `others`, but
    /// none whose sessions already `failed` here.
    fn second_besides(
        &self,
        second: ModelRef,
        first: &ModelRef,
        others: &[ModelRef],
        failed: &[ModelRef],
        settings: &Settings,
    ) -> Result<ModelRef, String> {
        let sources = pick::Sources {
            workers: &*self.workers,
            models: self.models.as_deref(),
        };
        let provider = |worker: &ModelRef| pick::provider(worker, sources);
        let elsewhere = provider(&second).is_some() && provider(&second) != provider(first);
        if elsewhere
            && independence::check(&second, std::slice::from_ref(first), true, &*self.workers)
                .is_ok()
        {
            return Ok(second);
        }
        let different_provider = settings.approvals.judge.different_provider;
        pick::pick_besides(others, different_provider, sources, failed).map_err(|_| {
            format!(
                "{} checked it first, and no second judge from another provider could check \
                 it too",
                DisplayNames::default().worker(first)
            )
        })
    }

    /// Asks `first`, and when its session fails, asks it once more; when that fails too, the
    /// next judge from another provider than the authors is its fallback, the same way, leaving
    /// out each in `failed`, to which it adds its own. A fallback's CLI must keep
    /// your plugins out of the judge's isolated session, and a CLI that loaded them anyway
    /// is never asked again. `Err` says why none could answer, naming the last one asked, so no
    /// model checked the work.
    async fn ask_or_fall_back(
        &self,
        first: &ModelRef,
        authors: &[ModelRef],
        failed: &mut Vec<ModelRef>,
        point: &DecisionPoint,
        evidence: &EvidencePacket,
        settings: &Settings,
    ) -> Result<(ModelRef, Answer), String> {
        let judge_settings = &settings.approvals.judge;
        let sources = pick::Sources {
            workers: &*self.workers,
            models: self.models.as_deref(),
        };
        let different_provider = judge_settings.different_provider;
        let mut model = first.clone();
        loop {
            let no_verdict = match self.ask_twice(&model, point, evidence, settings).await {
                Ok(answer) => return Ok((model, answer)),
                Err(no_verdict) => no_verdict,
            };
            failed.push(model.clone());
            match pick::pick_fallback(authors, different_provider, sources, failed) {
                Ok(next) => model = next,
                Err(_) => {
                    let again = if no_verdict.leaked {
                        ""
                    } else {
                        " even when asked again"
                    };
                    // Named when the only agents left can't keep your plugins out.
                    let fits = pick::pick_besides(authors, different_provider, sources, failed)
                        .map_or("", |_| " that keeps your plugins out");
                    return Err(format!(
                        "{} couldn't check it, as {}{again}, and no agent from another \
                         provider{fits} could instead",
                        DisplayNames::default().worker(&model),
                        no_verdict.why,
                    ));
                }
            }
        }
    }

    /// One fresh session with one model, and one more when it fails, unless its CLI loaded your
    /// plugins, which it would do again. `Err` says why the last one failed.
    async fn ask_twice(
        &self,
        model: &ModelRef,
        point: &DecisionPoint,
        evidence: &EvidencePacket,
        settings: &Settings,
    ) -> Result<Answer, NoVerdict> {
        let run_dir = self
            .state_dir
            .join(repo_state::RUNS)
            .join(point.run.as_str());
        match self
            .session(model, point, evidence, settings, &run_dir)
            .await
        {
            Ok(answer) => Ok(answer),
            Err(no_verdict) if no_verdict.leaked => Err(no_verdict),
            Err(_) => {
                self.session(model, point, evidence, settings, &run_dir)
                    .await
            }
        }
    }

    async fn session(
        &self,
        model: &ModelRef,
        point: &DecisionPoint,
        evidence: &EvidencePacket,
        settings: &Settings,
        run_dir: &Path,
    ) -> Result<Answer, NoVerdict> {
        let worker = self
            .workers
            .get(model)
            .map_err(|err| format!("it isn't available ({err})"))?;
        let input = render::judge_input(
            point,
            evidence,
            settings,
            &run_dir.join(run_files::EVIDENCE),
        )
        .map_err(|err| match err {
            render::RenderError::NoRules => format!(
                "the evidence has no approval rules file ({})",
                render::rules_label(&settings.approvals.judge.model_rules)
            ),
            render::RenderError::Document(why) => why,
        })?;
        let prefix = format!("judge-{}", point.approval);
        let (key, session_dir) = claim_session(run_dir, &prefix)
            .map_err(|err| format!("its session folder couldn't be made ({err})"))?;
        let prompt = vec![
            part(
                InputPartKind::Prompt,
                "judge",
                crate::sessions::templates::text(&crate::sessions::templates::JUDGE),
            ),
            part(
                InputPartKind::Rules,
                "your rules",
                format!("\n## Your rules\n\n{}\n", input.rules.trim_end()),
            ),
            part(
                InputPartKind::Evidence,
                "evidence",
                render::evidence_text(&input),
            ),
        ];
        let mut spec = no_tools_spec(NoToolsSession {
            worker: &*worker,
            role: RoleName::Judge,
            step: StepName::ModelJudge,
            run: Some(point.run.clone()),
            key,
            session_dir,
            prompt,
            schema: output_schema::<ModelJudge>(),
            settings,
            limit: None,
        })
        .map_err(|err| format!("its session couldn't be set up ({err})"))?;
        spec.model = model.clone();

        let mut request = StepRequest::new(worker, spec);
        // A verdict that fails the schema comes to you, not something to argue about.
        request.retries_on_mismatch = 0;
        let outcome = run_step(&*self.steps, request, Box::new(|_| {}), self.deadline)
            .await
            .map_err(|TimedOut { .. }| "it didn't answer in time".to_owned())?
            .map_err(|err| session_failure(&err))?;
        let verdict = decode::<ModelJudge>(&outcome)
            .map_err(|_| "its verdict didn't match the schema".to_owned())?;
        Ok(Answer::from_verdict(verdict))
    }
}

impl Approve for StepJudge {
    fn verdict<'a>(
        &'a self,
        point: &'a DecisionPoint,
        evidence: &'a EvidencePacket,
        settings: &'a Settings,
    ) -> BoxFuture<'a, ModelVerdict> {
        Box::pin(self.decide(point, evidence, settings))
    }
}

/// One model's answer, cleaned for showing.
struct Answer {
    decision: Verdict,
    reasons: Vec<String>,
}

impl Answer {
    fn ask_you(why: String) -> Self {
        Self {
            decision: Verdict::AskYou,
            reasons: vec![reason_line(&why)],
        }
    }

    fn from_verdict(verdict: ModelJudgeVerdict) -> Self {
        let reasons: Vec<String> = verdict
            .reasons
            .iter()
            .map(|reason| clean_text(reason).trim().to_owned())
            .filter(|reason| !reason.is_empty())
            .collect();
        let rules: Vec<String> = verdict
            .rules_cited
            .iter()
            .map(|rule| clean_text(rule).trim().to_owned())
            .filter(|rule| !rule.is_empty())
            .collect();
        let decision = match verdict.verdict {
            ModelJudgeDecision::Approve => Verdict::Approve,
            ModelJudgeDecision::Reject => Verdict::Reject,
            ModelJudgeDecision::AskYou => Verdict::AskYou,
        };
        // An approval has to say why and on which of your rules; one that doesn't is odd.
        if decision == Verdict::Approve && (reasons.is_empty() || rules.is_empty()) {
            return Self::ask_you(
                "the judge approved without giving reasons and the rules they rely on".to_owned(),
            );
        }
        let mut reasons = reasons;
        if !rules.is_empty() {
            reasons.push(format!("rules cited: {}", rules.join("; ")));
        }
        Self { decision, reasons }
    }

    fn labeled(self, model: &ModelRef) -> Self {
        Self {
            decision: self.decision,
            reasons: self
                .reasons
                .into_iter()
                .map(|reason| format!("{model}: {reason}"))
                .collect(),
        }
    }

    fn verdict(self, judge: ModelRef) -> ModelVerdict {
        ModelVerdict {
            verdict: self.decision,
            judge: Some(judge),
            reasons: self.reasons,
        }
    }
}

/// No model could check it. Autopilot lets your rules decide alone and says why; other modes
/// ask you.
fn unchecked(why: &str) -> ModelVerdict {
    ask_you(None, why)
}

fn ask_you(judge: Option<ModelRef>, why: &str) -> ModelVerdict {
    ModelVerdict {
        verdict: Verdict::AskYou,
        judge,
        reasons: vec![reason_line(why)],
    }
}

/// Reasons are shown on the approval and logged, so an error becomes one clean, short line.
fn reason_line(text: &str) -> String {
    render::cut(&render::one_line(&clean_text(text)), REASON_MAX)
}

/// Why a judge's session gave no verdict, as a clause about the judge. A session that
/// ended early is named by its kind only: its last words and the agent's message can quote the
/// evidence or a secret, and they stay in the raw log. What an isolated session loaded comes
/// from the CLI's own list, so it's named.
fn session_failure(err: &StepError) -> NoVerdict {
    match err {
        StepError::Session(end) => match &end.reason {
            EndReason::IsolationBroken { plugins, skills } => NoVerdict {
                why: format!(
                    "its session loaded {}, which a blind role mustn't see",
                    loaded_words(plugins, skills)
                ),
                leaked: true,
            },
            other => format!("its session ended early ({})", end_kind(other)).into(),
        },
        StepError::SchemaMismatch { .. } => "its verdict didn't match the schema".to_owned().into(),
        StepError::Runner(err) => format!("its session failed ({err})").into(),
    }
}

/// Why one judge's session gave no verdict.
struct NoVerdict {
    /// A clause about the judge, such as `its session ended early (it crashed)`.
    why: String,
    /// Its CLI loaded your plugins into the session, as it would on every try, so neither it
    /// nor another model of its CLI is asked again.
    leaked: bool,
}

impl From<String> for NoVerdict {
    fn from(why: String) -> Self {
        Self { why, leaked: false }
    }
}

/// Why a session ended, in plain words, such as `its agent had an error`, without its
/// details, never a tag such as `auth`.
pub(crate) fn end_kind(reason: &EndReason) -> String {
    match reason {
        EndReason::Completed => "it finished",
        EndReason::LimitHit { .. } => "it reached its limit",
        EndReason::Paused => "the run paused",
        EndReason::Stopped => "it was stopped",
        EndReason::Stuck { .. } => "it got stuck",
        EndReason::UsageLimit { .. } => "it hit its plan's usage limit",
        EndReason::RateLimited => "it was rate limited",
        EndReason::CliError { .. } => "its agent had an error",
        EndReason::Auth { .. } => "its agent refused the sign-in",
        EndReason::Billing { .. } => "its agent refused the billing",
        EndReason::ContextFull => "its context filled up",
        EndReason::ModelUnavailable { .. } => "its model isn't available",
        EndReason::AdapterBroken { .. } => "its adapter broke",
        EndReason::IsolationBroken { .. } => "it loaded your plugins where it mustn't",
        EndReason::StartFailed { .. } => "it couldn't start",
        EndReason::Crashed { .. } => "it crashed",
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Mutex;

    use argonauts_core::agents::session_runner::{Access, SandboxMode, SessionSpec};
    use argonauts_core::base::names::ApprovalId;
    use argonauts_core::runs::events::approval::ApprovalKind;
    use argonauts_core::safety::policy::{EvidenceDocument, EvidenceFacts};
    use argonauts_core::settings::approvals::Decides;
    use argonauts_core::workflows::steps::SchemaStepRunner;
    use argonauts_core::{ErrorCategory, RunId, sha256_hex};
    use argonauts_testkit::{FakeDirectory, FakeRunner, FakeSession};
    use serde_json::{Value, json};

    use super::*;

    const RULES: &str = "R1: approve a merge when every check and review passed.";

    fn point(authors: &[&str]) -> DecisionPoint {
        DecisionPoint {
            run: RunId::new("run-01").unwrap(),
            approval: ApprovalId::numbered(3),
            kind: ApprovalKind::Merge,
            learned_workflow: None,
            authors: authors.iter().map(|a| a.parse().unwrap()).collect(),
            mode_override: None,
            main_agent: None,
        }
    }

    fn document(label: &str, text: &str) -> EvidenceDocument {
        EvidenceDocument {
            label: label.to_owned(),
            text: text.to_owned(),
            path: None,
            sha256: None,
        }
    }

    fn evidence(documents: Vec<EvidenceDocument>) -> EvidencePacket {
        EvidencePacket {
            kind: ApprovalKind::Merge,
            goal: None,
            facts: EvidenceFacts {
                checks_passed: Some(true),
                reviews_approved: Some(true),
                ..EvidenceFacts::default()
            },
            agent_outputs: vec!["reviewer B: approve".to_owned()],
            documents,
        }
    }

    fn with_rules() -> EvidencePacket {
        evidence(vec![document(".argonauts/approval-rules.md", RULES)])
    }

    fn settings(model: &str) -> Settings {
        let mut settings = Settings::default();
        settings.approvals.decides = Decides::Model(model.parse().unwrap());
        settings
    }

    fn answer(verdict: &str) -> Value {
        json!({"verdict": verdict, "reasons": ["checks and reviews passed"], "rules_cited": ["R1"]})
    }

    struct Setup {
        state: tempfile::TempDir,
        workers: Arc<FakeDirectory>,
    }

    impl Setup {
        /// The authors' worker is known, so its provider is too.
        fn new() -> Self {
            let workers = Arc::new(FakeDirectory::new());
            workers.add(FakeRunner::cli("codex", "openai"));
            Self {
                state: tempfile::tempdir().unwrap(),
                workers,
            }
        }

        fn add(&self, worker: FakeRunner) -> Arc<FakeRunner> {
            self.workers.add(worker)
        }

        fn judge(&self) -> StepJudge {
            StepJudge::new(
                self.workers.clone(),
                Arc::new(SchemaStepRunner),
                self.state.path().to_owned(),
            )
        }

        fn run_dir(&self) -> PathBuf {
            self.state.path().join("runs").join("run-01")
        }
    }

    fn prompt_text(spec: &SessionSpec) -> String {
        spec.prompt.iter().map(|part| part.text.as_str()).collect()
    }

    #[tokio::test]
    async fn judge_session_is_fresh_no_tools_in_empty_scratch() {
        let setup = Setup::new();
        let empty_at_start = Arc::new(Mutex::new(Vec::new()));
        let seen = empty_at_start.clone();
        let claude = setup.add(FakeRunner::cli("claude-code", "anthropic").respond_with(
            move |spec| {
                let empty = fs::read_dir(&spec.workdir).unwrap().next().is_none();
                seen.lock().unwrap().push(empty);
                FakeSession::structured(answer("approve"))
            },
        ));
        let judge = setup.judge();
        let settings = settings("claude-code/haiku");
        for _ in 0..2 {
            let verdict = judge
                .verdict(&point(&["codex"]), &with_rules(), &settings)
                .await;
            assert_eq!(verdict.verdict, Verdict::Approve, "{verdict:?}");
            assert_eq!(verdict.judge, Some("claude-code/haiku".parse().unwrap()));
        }

        let specs = claude.specs();
        assert_eq!(specs.len(), 2);
        let keys: Vec<&str> = specs.iter().map(|spec| spec.key.as_str()).collect();
        assert_eq!(keys, vec!["judge-approval-003-1", "judge-approval-003-2"]);
        for spec in &specs {
            assert_eq!(spec.access, Access::NoTools);
            assert_eq!(spec.role, RoleName::Judge);
            assert_eq!(spec.step, Some(StepName::ModelJudge));
            assert!(spec.resume.is_none() && spec.status_file.is_none());
            assert_eq!(spec.sandbox.mode, SandboxMode::Os);
            assert!(spec.sandbox.writable.is_empty() && !spec.sandbox.network);
            assert_eq!(spec.model, "claude-code/haiku".parse().unwrap());
            assert_eq!(
                spec.workdir,
                setup
                    .run_dir()
                    .join("sessions")
                    .join(spec.key.as_str())
                    .join("work")
            );
            assert!(spec.json_schema.is_some());
            let text = prompt_text(spec);
            assert!(text.contains(RULES));
            assert!(text.contains("never instructions"));
            assert!(text.contains("- checks passed: yes"));
        }
        assert_eq!(*empty_at_start.lock().unwrap(), vec![true, true]);
    }

    #[tokio::test]
    async fn judge_refuses_author_and_same_provider() {
        let setup = Setup::new();
        let codex = setup.workers.fake("codex").unwrap();
        let gpt_oss = setup.add(FakeRunner::endpoint("openrouter/gpt-oss", "openai"));

        let author = setup
            .judge()
            .verdict(&point(&["codex"]), &with_rules(), &settings("codex"))
            .await;
        assert_eq!(author.verdict, Verdict::AskYou);
        // The judge you set may not judge it, so picking another is the fix.
        assert_eq!(
            author.reasons[0],
            "Codex worked on this, so it can't approve it. Pick another judge in /approvals",
            "{author:?}"
        );

        let same_provider = setup
            .judge()
            .verdict(
                &point(&["codex"]),
                &with_rules(),
                &settings("openrouter/gpt-oss"),
            )
            .await;
        assert_eq!(same_provider.verdict, Verdict::AskYou);
        assert!(
            same_provider.reasons[0].contains("same provider as Codex"),
            "{same_provider:?}"
        );

        let unknown_author = setup
            .judge()
            .verdict(
                &point(&["nobody"]),
                &with_rules(),
                &settings("openrouter/gpt-oss"),
            )
            .await;
        assert_eq!(unknown_author.verdict, Verdict::AskYou);

        assert!(codex.specs().is_empty() && gpt_oss.specs().is_empty());

        let none = setup
            .judge()
            .verdict(&point(&["codex"]), &with_rules(), &Settings::default())
            .await;
        assert_eq!(none.verdict, Verdict::AskYou);
        assert_eq!(none.judge, None);
    }

    /// With no judge set and none from another provider to pick, auto must fail closed, and
    /// the fuller warning is given once.
    #[tokio::test]
    async fn with_no_judge_to_pick_every_way_of_asking_one_comes_to_you() {
        use argonauts_core::settings::approvals::DecideWith;

        use crate::approvals::decide::decide;

        let setup = Setup::new();
        let codex = setup.workers.fake("codex").unwrap();
        let judge = setup.judge();
        let mut warnings = 0;
        for decide_with in [
            DecideWith::Model,
            DecideWith::RulesAndModel,
            DecideWith::RulesOrModel,
        ] {
            let decision = decide(
                &judge,
                &point(&["codex"]),
                &with_rules(),
                &Settings::default(),
                decide_with,
                time::OffsetDateTime::now_utc(),
            )
            .await;
            assert_eq!(decision.verdict, Verdict::AskYou, "{decide_with:?}");
            assert_eq!(decision.judge, None, "{decide_with:?}");
            assert!(
                decision
                    .reasons
                    .iter()
                    .any(|reason| reason.contains("another provider than OpenAI")),
                "{decide_with:?}: {decision:?}"
            );
            warnings += decision
                .reasons
                .iter()
                .filter(|reason| reason.contains("pick a judge in /approvals"))
                .count();
        }
        assert_eq!(warnings, 1);
        assert!(codex.specs().is_empty());
    }

    #[tokio::test]
    async fn an_unset_judge_is_the_cheapest_agent_of_another_provider() {
        let setup = Setup::new();
        let pricey = setup.add(
            FakeRunner::cli("claude-code", "anthropic")
                .with_cheapest_model("claude-haiku-4-5")
                .respond_with(|_| FakeSession::structured(answer("approve"))),
        );
        let cheap = setup.add(
            FakeRunner::cli("antigravity-cli", "google")
                .with_cheapest_model("gemini-3.8-flash-low")
                .respond_with(|_| FakeSession::structured(answer("approve"))),
        );
        let verdict = setup
            .judge()
            .verdict(&point(&["codex"]), &with_rules(), &Settings::default())
            .await;
        assert_eq!(verdict.verdict, Verdict::Approve, "{verdict:?}");
        let picked: ModelRef = "antigravity-cli/gemini-3.8-flash-low".parse().unwrap();
        assert_eq!(verdict.judge, Some(picked.clone()));
        let specs = cheap.specs();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].model, picked);
        assert!(pricey.specs().is_empty());
    }

    /// `decides = "judge"` is the judge you named: the first on your list that may check the
    /// work, whatever it costs. One from the authors' provider is passed over, and so is one
    /// you can't run.
    #[tokio::test]
    async fn a_judge_you_named_decides_before_the_cheapest_is_picked() {
        let setup = Setup::new();
        let approves = || FakeSession::structured(answer("approve"));
        let named = setup.add(
            FakeRunner::cli("claude-code", "anthropic")
                .with_cheapest_model("claude-haiku-4-5")
                .respond_with(move |_| approves()),
        );
        let cheapest = setup.add(
            FakeRunner::cli("antigravity-cli", "google")
                .with_cheapest_model("gemini-3.8-flash-low")
                .respond_with(move |_| approves()),
        );
        let mut settings = Settings::default();
        settings.roles.entry(RoleName::Judge).or_default().models = vec![
            // Not installed, then the authors' own provider: neither may judge this.
            "opencode/qwen".parse().unwrap(),
            "codex/gpt-5.6".parse().unwrap(),
            "claude-code/opus".parse().unwrap(),
            "antigravity-cli".parse().unwrap(),
        ];
        let verdict = setup
            .judge()
            .verdict(&point(&["codex"]), &with_rules(), &settings)
            .await;
        assert_eq!(verdict.verdict, Verdict::Approve, "{verdict:?}");
        let judge: ModelRef = "claude-code/opus".parse().unwrap();
        assert_eq!(verdict.judge, Some(judge.clone()));
        assert_eq!(named.specs()[0].model, judge);
        assert!(cheapest.specs().is_empty());

        // With no judge on the list that may check it, the pick is the cheapest, as before.
        settings.roles.entry(RoleName::Judge).or_default().models =
            vec!["codex/gpt-5.6".parse().unwrap()];
        let verdict = setup
            .judge()
            .verdict(&point(&["codex"]), &with_rules(), &settings)
            .await;
        let picked: ModelRef = "antigravity-cli/gemini-3.8-flash-low".parse().unwrap();
        assert_eq!(verdict.judge, Some(picked));
    }

    /// The judge `[crew]` names decides too, and a judge you left out follows the reviewers
    /// you named. The built-in crew, everyone in every role, names no judge.
    #[test]
    fn the_crew_s_judge_is_the_judge_you_named() {
        let mut settings = Settings::default();
        assert!(
            judges(&settings).is_empty(),
            "the built-in crew names no one"
        );
        let codex: ModelRef = "codex/gpt-5.6".parse().unwrap();
        settings.crew.judge = vec![codex.clone()];
        assert_eq!(judges(&settings), [codex]);
        let gemini: ModelRef = "antigravity-cli/gemini-3.6-pro".parse().unwrap();
        settings.crew.judge = Vec::new();
        settings.crew.reviewer = vec![gemini.clone()];
        assert_eq!(judges(&settings), [gemini]);
        // A fine role's list still comes first.
        let opus: ModelRef = "claude-code/opus".parse().unwrap();
        settings.roles.entry(RoleName::Judge).or_default().models = vec![opus.clone()];
        assert_eq!(judges(&settings), [opus]);
    }

    #[tokio::test]
    async fn a_stalled_judge_asks_you() {
        let setup = Setup::new();
        let claude = setup.add(
            FakeRunner::cli("claude-code", "anthropic")
                .then(FakeSession::until_stopped(Vec::new()))
                .then(FakeSession::until_stopped(Vec::new())),
        );
        let mut judge = setup.judge();
        judge.deadline = Duration::from_millis(20);
        let verdict = judge
            .verdict(&point(&["codex"]), &with_rules(), &settings("claude-code"))
            .await;
        assert_eq!(verdict.verdict, Verdict::AskYou, "{verdict:?}");
        assert!(verdict.reasons.iter().any(|why| why.contains("in time")));
        let spec = &claude.specs()[0];
        assert_eq!(spec.limits.max_minutes, Some(session::MAX_MINUTES));
        assert_eq!(spec.limits.inactivity_min, Some(session::INACTIVITY_MIN));
    }

    #[tokio::test]
    async fn a_failed_judge_is_asked_again_then_falls_back_to_another_provider() {
        let auth = || {
            FakeSession::ended(EndReason::Auth {
                message: "workspace routing discovery unauthorized (401)".to_owned(),
            })
        };
        let setup = Setup::new();
        let claude = setup.add(
            FakeRunner::cli("claude-code", "anthropic")
                .then(auth())
                .then(FakeSession::structured(answer("approve"))),
        );
        let verdict = setup
            .judge()
            .verdict(&point(&["codex"]), &with_rules(), &settings("claude-code"))
            .await;
        assert_eq!(verdict.verdict, Verdict::Approve, "{verdict:?}");
        assert_eq!(claude.specs().len(), 2, "asked once more after the 401");

        let setup = Setup::new();
        let claude = setup.add(
            FakeRunner::cli("claude-code", "anthropic")
                .then(auth())
                .then(auth()),
        );
        let gemini = setup.add(
            FakeRunner::cli("antigravity-cli", "google")
                .then(FakeSession::structured(answer("approve"))),
        );
        // A fallback's CLI keeps your plugins out of the judge's isolated session.
        setup.workers.isolating("antigravity-cli");
        let verdict = setup
            .judge()
            .verdict(&point(&["codex"]), &with_rules(), &settings("claude-code"))
            .await;
        assert_eq!(verdict.verdict, Verdict::Approve, "{verdict:?}");
        assert_eq!(verdict.judge, Some("antigravity-cli".parse().unwrap()));
        assert_eq!((claude.specs().len(), gemini.specs().len()), (2, 1));
    }

    /// Nobody chose a fallback to see your plugins, so one whose CLI can't keep them out of the
    /// judge's session is never picked, and the reason says so.
    #[tokio::test]
    async fn a_fallback_judge_must_keep_your_plugins_out() {
        let setup = Setup::new();
        let auth = || {
            FakeSession::ended(EndReason::Auth {
                message: "unauthorized (401)".to_owned(),
            })
        };
        setup.add(
            FakeRunner::cli("claude-code", "anthropic")
                .then(auth())
                .then(auth()),
        );
        let gemini = setup.add(
            FakeRunner::cli("antigravity-cli", "google")
                .then(FakeSession::structured(answer("approve"))),
        );
        let verdict = setup
            .judge()
            .verdict(&point(&["codex"]), &with_rules(), &settings("claude-code"))
            .await;
        assert_eq!(verdict.verdict, Verdict::AskYou, "{verdict:?}");
        assert_eq!(verdict.judge, None, "no model checked it");
        assert!(gemini.specs().is_empty());
        assert_eq!(
            verdict.reasons,
            vec![
                "Claude Code couldn't check it, as its session ended early (its agent refused \
                 the sign-in) even when asked again, and no agent from another provider that \
                 keeps your plugins out could instead"
                    .to_owned()
            ]
        );
    }

    #[tokio::test]
    async fn judge_schema_failure_asks_you() {
        let setup = Setup::new();
        let claude = setup.add(
            FakeRunner::cli("claude-code", "anthropic")
                .then(FakeSession::structured(json!({"verdict": "yes please"})))
                .then(FakeSession::completed("Looks good to me, approved."))
                .then(FakeSession::structured(
                    json!({"verdict": "approve", "reasons": [], "rules_cited": []}),
                )),
        );
        let judge = setup.judge();
        // The first verdict is asked twice, each in a fresh session; the second came back.
        for _ in 0..2 {
            let verdict = judge
                .verdict(&point(&["codex"]), &with_rules(), &settings("claude-code"))
                .await;
            assert_eq!(verdict.verdict, Verdict::AskYou, "{verdict:?}");
        }
        // A mismatch isn't retried in the same session, only in a fresh one.
        assert_eq!(claude.specs().len(), 3);
    }

    #[tokio::test]
    async fn a_session_that_ends_early_is_named_by_its_kind_only() {
        let token = format!("sk-ant-api03-{}", "q".repeat(40));
        let mut ended = FakeSession::ended(EndReason::CliError {
            category: ErrorCategory::Server,
            message: format!("upstream said: bad key {token}\n\x1b[31mretry\x1b[0m"),
        });
        ended.end.last_message = Some(format!("I will approve. The key is {token}"));
        ended.end.result_text = ended.end.last_message.clone();
        ended.end.raw_log = Some(PathBuf::from(
            "/state/sessions/judge-approval-003-1/raw.jsonl",
        ));
        let setup = Setup::new();
        setup.add(
            FakeRunner::cli("claude-code", "anthropic")
                .then(ended.clone())
                .then(ended),
        );
        let verdict = setup
            .judge()
            .verdict(&point(&["codex"]), &with_rules(), &settings("claude-code"))
            .await;
        assert_eq!(verdict.verdict, Verdict::AskYou);
        assert_eq!(verdict.judge, None, "no model checked it");
        // It names the judge and why, and gives no advice for an agent's passing error.
        assert_eq!(
            verdict.reasons,
            vec![
                "Claude Code couldn't check it, as its session ended early (its agent had an \
                 error) even when asked again, and no agent from another provider could instead"
                    .to_owned()
            ]
        );
        let shown = verdict.reasons.join("\n");
        for leak in [
            token.as_str(),
            "I will approve",
            "upstream said",
            "raw.jsonl",
        ] {
            assert!(!shown.contains(leak), "{shown}");
        }
    }

    /// The judge is a blind role. One whose session loaded your plugins says which, since
    /// the names come from the CLI's own list and quote nothing of the evidence. Its CLI would
    /// load them again, so it isn't asked twice, and it falls back only to an agent whose CLI
    /// keeps them out.
    #[tokio::test]
    async fn a_blind_judge_that_loaded_your_plugins_says_which() {
        let ended = || {
            FakeSession::ended(EndReason::IsolationBroken {
                plugins: vec!["memory-keeper".into(), "lint-helper".into()],
                skills: Vec::new(),
            })
        };
        let setup = Setup::new();
        let claude = setup.add(
            FakeRunner::cli("claude-code", "anthropic")
                .then(ended())
                .then(ended()),
        );
        let verdict = setup
            .judge()
            .verdict(&point(&["codex"]), &with_rules(), &settings("claude-code"))
            .await;
        assert_eq!(verdict.verdict, Verdict::AskYou);
        assert_eq!(
            verdict.reasons,
            vec![
                "Claude Code couldn't check it, as its session loaded your plugins memory-keeper \
                 and lint-helper, which a blind role mustn't see, and no agent from another \
                 provider could instead"
                    .to_owned()
            ]
        );
        assert_eq!(claude.specs().len(), 1, "not asked again");

        let setup = Setup::new();
        let claude = setup.add(
            FakeRunner::cli("claude-code", "anthropic")
                .then(ended())
                .then(ended()),
        );
        let gemini = setup.add(
            FakeRunner::cli("antigravity-cli", "google")
                .then(FakeSession::structured(answer("approve"))),
        );
        setup.workers.isolating("antigravity-cli");
        let verdict = setup
            .judge()
            .verdict(&point(&["codex"]), &with_rules(), &settings("claude-code"))
            .await;
        assert_eq!(verdict.verdict, Verdict::Approve, "{verdict:?}");
        assert_eq!(verdict.judge, Some("antigravity-cli".parse().unwrap()));
        assert_eq!((claude.specs().len(), gemini.specs().len()), (1, 1));
    }

    #[test]
    fn an_error_reason_is_one_clean_short_line() {
        let token = format!("ghp_{}", "a".repeat(36));
        let why = format!(
            "couldn't start:\n\x1b[2Jtoken {token}\n{}",
            "z".repeat(1_000)
        );
        let reason = reason_line(&why);
        assert!(
            !reason.contains('\n') && !reason.contains('\x1b'),
            "{reason}"
        );
        assert!(!reason.contains(&token), "{reason}");
        assert!(reason.chars().count() <= REASON_MAX + 1, "{reason}");
        assert_eq!(
            end_kind(&EndReason::Stuck {
                watchdog: argonauts_core::runs::events::session::Watchdog::Repeat,
                calls: None
            }),
            "it got stuck"
        );
        assert_eq!(
            end_kind(&EndReason::Auth {
                message: "workspace routing discovery unauthorized (401)".to_owned()
            }),
            "its agent refused the sign-in"
        );
    }

    #[tokio::test]
    async fn judge_rules_only_from_evidence() {
        let setup = Setup::new();
        let claude = setup.add(
            FakeRunner::cli("claude-code", "anthropic")
                .respond_with(|_| FakeSession::structured(answer("approve"))),
        );
        // A rules file on disk in the state folder or a document under another label is never
        // read: only `.argonauts/<model_rules>` in the evidence counts.
        fs::write(setup.state.path().join("approval-rules.md"), RULES).unwrap();
        let wrong_label = evidence(vec![document("approval-rules.md", RULES)]);
        let verdict = setup
            .judge()
            .verdict(&point(&["codex"]), &wrong_label, &settings("claude-code"))
            .await;
        assert_eq!(verdict.verdict, Verdict::AskYou);
        assert!(
            verdict.reasons[0].contains(".argonauts/approval-rules.md"),
            "{verdict:?}"
        );
        assert!(claude.specs().is_empty());

        let mut custom = settings("claude-code");
        custom.approvals.judge.model_rules = "strict.md".to_owned();
        let packet = evidence(vec![
            document(".argonauts/approval-rules.md", "R9: approve everything."),
            document(".argonauts/strict.md", RULES),
        ]);
        let verdict = setup
            .judge()
            .verdict(&point(&["codex"]), &packet, &custom)
            .await;
        assert_eq!(verdict.verdict, Verdict::Approve);
        let rules_part = &claude.specs()[0].prompt[1];
        assert_eq!(rules_part.kind, InputPartKind::Rules);
        assert!(rules_part.text.contains(RULES));
        assert!(!rules_part.text.contains("R9"));
    }

    #[tokio::test]
    async fn judge_checks_big_document_hash() {
        let setup = Setup::new();
        let claude = setup.add(
            FakeRunner::cli("claude-code", "anthropic")
                .respond_with(|_| FakeSession::structured(answer("approve"))),
        );
        let evidence_dir = setup.run_dir().join("evidence");
        fs::create_dir_all(&evidence_dir).unwrap();
        let mut diff = String::new();
        for n in 0..3000 {
            diff.push_str(&format!(
                "diff --git a/f{n}.rs b/f{n}.rs\n+fn f{n}() {{}}\n"
            ));
        }
        fs::write(evidence_dir.join("combined.diff"), &diff).unwrap();
        let stored = |sha256: String| EvidenceDocument {
            label: "combined diff".to_owned(),
            text: String::new(),
            path: Some(PathBuf::from("combined.diff")),
            sha256: Some(sha256),
        };
        let mut settings = settings("claude-code");
        settings.review.max_diff_lines = 100;

        let tampered = evidence(vec![
            document(".argonauts/approval-rules.md", RULES),
            stored(sha256_hex(b"what was approved")),
        ]);
        let verdict = setup
            .judge()
            .verdict(&point(&["codex"]), &tampered, &settings)
            .await;
        assert_eq!(verdict.verdict, Verdict::AskYou);
        assert!(verdict.reasons[0].contains("SHA-256"), "{verdict:?}");
        assert!(claude.specs().is_empty());

        let good = evidence(vec![
            document(".argonauts/approval-rules.md", RULES),
            stored(sha256_hex(diff.as_bytes())),
        ]);
        let verdict = setup
            .judge()
            .verdict(&point(&["codex"]), &good, &settings)
            .await;
        assert_eq!(verdict.verdict, Verdict::Approve);
        let text = prompt_text(&claude.specs()[0]);
        assert!(text.contains("diff --git a/f0.rs b/f0.rs"));
        assert!(!text.contains("f2999.rs"));
        assert!(
            text.contains("100 of 6000 diff lines shown"),
            "the note says what was cut"
        );
    }

    #[tokio::test]
    async fn judge_second_model_must_agree() {
        let setup = Setup::new();
        let claude = setup.add(
            FakeRunner::cli("claude-code", "anthropic")
                .respond_with(|_| FakeSession::structured(answer("approve"))),
        );
        let glm = setup.add(
            FakeRunner::endpoint("openrouter/glm", "zhipu")
                .then(FakeSession::structured(answer("reject")))
                .then(FakeSession::structured(answer("approve")))
                .then(FakeSession::structured(json!({"verdict": 7}))),
        );
        setup.add(FakeRunner::endpoint("openrouter/opus", "anthropic"));
        let mut settings = settings("claude-code");
        settings.approvals.judge.second_model = Some("openrouter/glm".parse().unwrap());
        let judge = setup.judge();
        let (point, packet) = (point(&["codex"]), with_rules());
        let ask = || judge.verdict(&point, &packet, &settings);

        let disagree = ask().await;
        assert_eq!(disagree.verdict, Verdict::Reject);
        assert_eq!(disagree.judge, Some("claude-code".parse().unwrap()));
        assert!(
            disagree
                .reasons
                .iter()
                .any(|r| r.starts_with("openrouter/glm: "))
        );

        let agree = ask().await;
        assert_eq!(agree.verdict, Verdict::Approve);
        assert!(agree.reasons.iter().any(|r| r.starts_with("claude-code: ")));

        let broken = ask().await;
        assert_eq!(broken.verdict, Verdict::AskYou);
        // The broken verdict was asked for once more, in a fresh session.
        assert_eq!(glm.specs().len(), 4);
        assert!(
            glm.specs()
                .iter()
                .all(|spec| spec.sandbox.mode == SandboxMode::None)
        );

        // A second judge in the first's provider is refused before anyone is asked.
        settings.approvals.judge.second_model = Some("openrouter/opus".parse().unwrap());
        let same_provider = judge.verdict(&point, &packet, &settings).await;
        assert_eq!(same_provider.verdict, Verdict::AskYou);
        assert_eq!(claude.specs().len(), 3);
    }

    /// A second judge whose sessions fail gets a fallback from another provider than the
    /// authors and the first, the way the first does.
    #[tokio::test]
    async fn a_failing_second_judge_gets_a_fallback_from_another_provider() {
        let failed = || {
            FakeSession::ended(EndReason::Auth {
                message: "workspace routing discovery unauthorized (401)".to_owned(),
            })
        };
        let setup = Setup::new();
        setup.add(
            FakeRunner::cli("claude-code", "anthropic")
                .respond_with(|_| FakeSession::structured(answer("approve"))),
        );
        let glm = setup.add(
            FakeRunner::endpoint("openrouter/glm", "zhipu")
                .then(failed())
                .then(failed()),
        );
        // Of the first's provider, so it may never take the second's place.
        let opus = setup.add(FakeRunner::endpoint("openrouter/opus", "anthropic"));
        let gemini = setup.add(
            FakeRunner::cli("antigravity-cli", "google")
                .then(FakeSession::structured(answer("approve"))),
        );
        // A fallback's CLI keeps your plugins out of the judge's isolated session.
        setup.workers.isolating("antigravity-cli");
        let mut settings = settings("claude-code");
        settings.approvals.judge.second_model = Some("openrouter/glm".parse().unwrap());
        let verdict = setup
            .judge()
            .verdict(&point(&["codex"]), &with_rules(), &settings)
            .await;
        assert_eq!(verdict.verdict, Verdict::Approve, "{verdict:?}");
        assert_eq!(glm.specs().len(), 2, "asked once more");
        assert!(opus.specs().is_empty());
        assert_eq!(gemini.specs().len(), 1);
        assert!(
            verdict
                .reasons
                .iter()
                .any(|reason| reason.starts_with("antigravity-cli: ")),
            "{verdict:?}"
        );
    }

    /// When no second judge can answer, the work is left unchecked, with no judge named,
    /// so autopilot lets your rules decide alone rather than leave the approval to you.
    #[tokio::test]
    async fn a_second_judge_that_cant_answer_leaves_it_unchecked() {
        let setup = Setup::new();
        setup.add(
            FakeRunner::cli("claude-code", "anthropic")
                .respond_with(|_| FakeSession::structured(answer("approve"))),
        );
        setup.add(
            FakeRunner::endpoint("openrouter/glm", "zhipu").respond_with(|_| {
                FakeSession::ended(EndReason::Auth {
                    message: "unauthorized (401)".to_owned(),
                })
            }),
        );
        let mut settings = settings("claude-code");
        settings.approvals.judge.second_model = Some("openrouter/glm".parse().unwrap());
        let verdict = setup
            .judge()
            .verdict(&point(&["codex"]), &with_rules(), &settings)
            .await;
        assert_eq!(verdict.verdict, Verdict::AskYou);
        assert_eq!(verdict.judge, None, "{verdict:?}");
        assert_eq!(
            verdict.reasons,
            vec![
                "GLM on OpenRouter couldn't check it, as its session ended early (its agent \
                 refused the sign-in) even when asked again, and no agent from another provider \
                 could instead"
                    .to_owned()
            ]
        );
    }

    /// A first judge that was a fallback is checked against the second again: a second of the
    /// fallback's provider is replaced by one from yet another provider.
    #[tokio::test]
    async fn a_fallback_first_never_shares_a_provider_with_the_second() {
        let failed = || {
            FakeSession::ended(EndReason::Auth {
                message: "unauthorized (401)".to_owned(),
            })
        };
        let setup = Setup::new();
        setup.add(
            FakeRunner::cli("claude-code", "anthropic")
                .then(failed())
                .then(failed()),
        );
        let gemini = setup.add(
            FakeRunner::cli("antigravity-cli", "google")
                .respond_with(|_| FakeSession::structured(answer("approve"))),
        );
        // A fallback's CLI keeps your plugins out of the judge's isolated session.
        setup.workers.isolating("antigravity-cli");
        let gemma = setup.add(FakeRunner::endpoint("openrouter/gemma", "google"));
        let glm = setup.add(
            FakeRunner::endpoint("openrouter/glm", "zhipu")
                .then(FakeSession::structured(answer("approve"))),
        );
        let mut settings = settings("claude-code");
        // Google's, as the fallback first turns out to be.
        settings.approvals.judge.second_model = Some("openrouter/gemma".parse().unwrap());
        let verdict = setup
            .judge()
            .verdict(&point(&["codex"]), &with_rules(), &settings)
            .await;
        assert_eq!(verdict.verdict, Verdict::Approve, "{verdict:?}");
        assert_eq!(verdict.judge, Some("antigravity-cli".parse().unwrap()));
        assert_eq!(gemini.specs().len(), 1);
        assert!(gemma.specs().is_empty(), "never two approvals from Google");
        assert_eq!(glm.specs().len(), 1);
    }

    /// Recorded once from `claude -p --model haiku --effort low --json-schema`, and replayed
    /// through the real schema check.
    #[tokio::test]
    async fn a_recorded_claude_code_verdict_passes_the_schema() {
        let recorded: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/live/judge-claude-haiku.json"
        ))
        .unwrap();
        let structured = recorded["structured_output"].clone();
        let decoded: ModelJudgeVerdict = serde_json::from_value(structured.clone()).unwrap();
        assert_eq!(decoded.verdict, ModelJudgeDecision::Approve);

        let setup = Setup::new();
        setup.add(
            FakeRunner::cli("claude-code", "anthropic").then(FakeSession::structured(structured)),
        );
        let verdict = setup
            .judge()
            .verdict(
                &point(&["codex"]),
                &with_rules(),
                &settings("claude-code/haiku"),
            )
            .await;
        assert_eq!(verdict.verdict, Verdict::Approve, "{verdict:?}");
        assert!(
            verdict
                .reasons
                .iter()
                .any(|r| r.starts_with("rules cited: "))
        );
    }
}
