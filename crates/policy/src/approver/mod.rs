//! Your approver model: independent of every author and owner, in a fresh no-tools session,
//! reading only the evidence packet with every document fenced as data. It decides only below
//! the hard limits, and anything that goes wrong is an escalation.

pub mod independence;
pub mod pick;
pub mod render;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use argonauts_core::events::session::{EndReason, InputPartKind, loaded_words};
use argonauts_core::future::BoxFuture;
use argonauts_core::layout::{repo_state, run_files};
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::policy::{DecisionPoint, EvidencePacket, Verdict};
use argonauts_core::registry::ModelRegistry;
use argonauts_core::settings::Settings;
use argonauts_core::steps::judgment::{ApproverDecision, ApproverVerdict, ModelApprover};
use argonauts_core::steps::{StepError, StepName, StepRequest, StepRunner, decode, output_schema};
use argonauts_core::worker::WorkerDirectory;

use crate::parts::{Approve, ModelVerdict};
use crate::session::{
    self, NoToolsSession, TimedOut, claim_session, clean_text, no_tools_spec, part, run_step,
};

/// The prompt template the evidence is rendered into.
pub const PROMPT: &str = include_str!("../prompts/approver.md");

/// An escalation's reason is cut to this many characters.
const REASON_MAX: usize = 300;

pub struct StepApprover {
    workers: Arc<dyn WorkerDirectory>,
    /// The live model lists an unset approver is picked from, beside each agent's cheapest.
    models: Option<Arc<dyn ModelRegistry>>,
    steps: Arc<dyn StepRunner>,
    /// `~/.argonauts/state/<repo>`: approver sessions go in the run's `sessions/` folder, and
    /// big evidence documents sit beside the packet in `runs/<run>/evidence/`.
    state_dir: PathBuf,
    /// How long one approver session may take before it's stopped and you're asked instead.
    deadline: Duration,
    /// Set once the fuller warning about having no approver to pick was given.
    warned: AtomicBool,
}

impl StepApprover {
    pub fn new(
        workers: Arc<dyn WorkerDirectory>,
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

    /// Picks an unset approver from these model lists too, not only each agent's cheapest.
    pub fn with_models(mut self, models: Arc<dyn ModelRegistry>) -> Self {
        self.models = Some(models);
        self
    }

    /// The approver you set, else the cheapest model from another company than the authors,
    /// with the authors it must be independent of. In full auto, when none comes from outside
    /// every author, as with a crew of every company you run, it's the cheapest from another
    /// company than the lead, whose answers it checks. The first time none can be picked, the
    /// reason is the fuller warning.
    fn first_approver(
        &self,
        point: &DecisionPoint,
        settings: &Settings,
    ) -> Result<(WorkerRef, Vec<WorkerRef>), String> {
        let approver = &settings.approvals.approver;
        if let Some(model) = &approver.model {
            return Ok((model.clone(), point.authors.clone()));
        }
        let sources = pick::Sources {
            workers: &*self.workers,
            models: self.models.as_deref(),
        };
        let none = match pick::pick(&point.authors, approver.different_family, sources) {
            Ok(first) => return Ok((first, point.authors.clone())),
            Err(none) => none,
        };
        if crate::approvals::in_full_auto(point)
            && let Some(lead) = &point.lead
        {
            let lead = vec![lead.clone()];
            if let Ok(first) = pick::pick(&lead, approver.different_family, sources) {
                return Ok((first, lead));
            }
        }
        Err(if self.warned.swap(true, Ordering::SeqCst) {
            none.reason()
        } else {
            none.warning()
        })
    }

    async fn decide(
        &self,
        point: &DecisionPoint,
        evidence: &EvidencePacket,
        settings: &Settings,
    ) -> ModelVerdict {
        let approver = &settings.approvals.approver;
        let (first, authors) = match self.first_approver(point, settings) {
            Ok(picked) => picked,
            Err(why) => return escalate(None, &why),
        };
        if let Err(why) =
            independence::check(&first, &authors, approver.different_family, &*self.workers)
        {
            return escalate(None, &why);
        }
        let Some(second) = approver.second_model.clone() else {
            return self
                .ask(&first, point, evidence, settings)
                .await
                .verdict(first);
        };
        // The second must be independent of the authors and, always, of the first's family.
        let independent =
            independence::check(&second, &authors, approver.different_family, &*self.workers)
                .and_then(|()| {
                    independence::check(&second, std::slice::from_ref(&first), true, &*self.workers)
                });
        if let Err(why) = independent {
            return escalate(None, &format!("second approver: {why}"));
        }

        // Asked one after the other, so a first answer other than approve spends nothing more.
        let first_answer = self.ask(&first, point, evidence, settings).await;
        if first_answer.decision != Verdict::Approve {
            return first_answer.labeled(&first).verdict(first);
        }
        let second_answer = self.ask(&second, point, evidence, settings).await;
        let verdict = second_answer.decision;
        let mut reasons = first_answer.labeled(&first).reasons;
        reasons.extend(second_answer.labeled(&second).reasons);
        ModelVerdict {
            verdict,
            approver: Some(first),
            reasons,
        }
    }

    /// One fresh session with one model. Every failure is an escalation with its reason.
    async fn ask(
        &self,
        model: &WorkerRef,
        point: &DecisionPoint,
        evidence: &EvidencePacket,
        settings: &Settings,
    ) -> Answer {
        let run_dir = self
            .state_dir
            .join(repo_state::RUNS)
            .join(point.run.as_str());
        match self
            .session(model, point, evidence, settings, &run_dir)
            .await
        {
            Ok(answer) => answer,
            Err(why) => Answer::escalate(why),
        }
    }

    async fn session(
        &self,
        model: &WorkerRef,
        point: &DecisionPoint,
        evidence: &EvidencePacket,
        settings: &Settings,
        run_dir: &Path,
    ) -> Result<Answer, String> {
        let worker = self
            .workers
            .get(model)
            .map_err(|err| format!("the approver {model} isn't available: {err}"))?;
        let input = render::approver_input(
            point,
            evidence,
            settings,
            &run_dir.join(run_files::EVIDENCE),
        )
        .map_err(|err| match err {
            render::RenderError::NoRules => format!(
                "the evidence has no approval rules file ({})",
                render::rules_label(&settings.approvals.approver.model_rules)
            ),
            render::RenderError::Document(why) => why,
        })?;
        let prefix = format!("approver-{}", point.checkpoint);
        let (key, session_dir) = claim_session(run_dir, &prefix)
            .map_err(|err| format!("couldn't make the approver's session folder: {err}"))?;
        let prompt = vec![
            part(InputPartKind::Prompt, "approver", PROMPT.to_owned()),
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
            role: RoleName::Approver,
            step: StepName::ModelApprover,
            run: Some(point.run.clone()),
            key,
            session_dir,
            prompt,
            schema: output_schema::<ModelApprover>(),
            settings,
            budget: None,
        })
        .map_err(|err| format!("couldn't set up the approver's session: {err}"))?;
        spec.worker = model.clone();

        let mut request = StepRequest::new(worker, spec);
        // A verdict that fails the schema is an escalation, not something to argue about.
        request.retries_on_mismatch = 0;
        let outcome = run_step(&*self.steps, request, Box::new(|_| {}), self.deadline)
            .await
            .map_err(|TimedOut { .. }| {
                "the approver didn't answer in time, so it was stopped".to_owned()
            })?
            .map_err(|err| session_failure(&err))?;
        let verdict = decode::<ModelApprover>(&outcome)
            .map_err(|_| "the approver's verdict didn't match the schema".to_owned())?;
        Ok(Answer::from_verdict(verdict))
    }
}

impl Approve for StepApprover {
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
    fn escalate(why: String) -> Self {
        Self {
            decision: Verdict::Escalate,
            reasons: vec![reason_line(&why)],
        }
    }

    fn from_verdict(verdict: ApproverVerdict) -> Self {
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
            ApproverDecision::Approve => Verdict::Approve,
            ApproverDecision::Reject => Verdict::Reject,
            ApproverDecision::Escalate => Verdict::Escalate,
        };
        // An approval has to say why and on which of your rules; one that doesn't is odd.
        if decision == Verdict::Approve && (reasons.is_empty() || rules.is_empty()) {
            return Self::escalate(
                "the approver approved without giving reasons and the rules they rely on"
                    .to_owned(),
            );
        }
        let mut reasons = reasons;
        if !rules.is_empty() {
            reasons.push(format!("rules cited: {}", rules.join("; ")));
        }
        Self { decision, reasons }
    }

    fn labeled(self, model: &WorkerRef) -> Self {
        Self {
            decision: self.decision,
            reasons: self
                .reasons
                .into_iter()
                .map(|reason| format!("{model}: {reason}"))
                .collect(),
        }
    }

    fn verdict(self, approver: WorkerRef) -> ModelVerdict {
        ModelVerdict {
            verdict: self.decision,
            approver: Some(approver),
            reasons: self.reasons,
        }
    }
}

fn escalate(approver: Option<WorkerRef>, why: &str) -> ModelVerdict {
    ModelVerdict {
        verdict: Verdict::Escalate,
        approver,
        reasons: vec![reason_line(why)],
    }
}

/// Reasons are shown on the card and logged, so an error becomes one clean, short line.
fn reason_line(text: &str) -> String {
    render::cut(&render::one_line(&clean_text(text)), REASON_MAX)
}

/// A session that ended early is named by its kind only. Its last words and the vendor's
/// message can quote the evidence or a secret, and they stay in the raw log. What an isolated
/// session loaded comes from the CLI's own list, so it's named.
fn session_failure(err: &StepError) -> String {
    match err {
        StepError::Session(end) => match &end.reason {
            EndReason::IsolationBroken { plugins, skills } => format!(
                "the approver's session ended early: it loaded {}, which a blind role mustn't see",
                loaded_words(plugins, skills)
            ),
            other => format!("the approver's session ended early: {}", end_kind(other)),
        },
        StepError::SchemaMismatch { .. } => {
            "the approver's verdict didn't match the schema".to_owned()
        }
        StepError::Worker(err) => format!("the approver's session failed: {err}"),
    }
}

/// Why a session ended, in words, such as `vendor error`, without its details.
pub(crate) fn end_kind(reason: &EndReason) -> String {
    let tag = serde_json::to_value(reason).ok().and_then(|value| {
        value
            .get("why")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    });
    tag.map_or_else(|| "unknown".to_owned(), |tag| tag.replace('_', " "))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Mutex;

    use argonauts_core::events::checkpoint::CheckpointKind;
    use argonauts_core::names::CheckpointId;
    use argonauts_core::policy::{EvidenceDocument, EvidenceFacts};
    use argonauts_core::steps::SchemaStepRunner;
    use argonauts_core::worker::{Access, SandboxMode, SessionSpec};
    use argonauts_core::{ErrorCategory, RunId, sha256_hex};
    use argonauts_testkit::{FakeDirectory, FakeSession, FakeWorker};
    use serde_json::{Value, json};

    use super::*;

    const RULES: &str = "R1: approve a merge when every gate and review passed.";

    fn point(authors: &[&str]) -> DecisionPoint {
        DecisionPoint {
            run: RunId::new("run-01").unwrap(),
            checkpoint: CheckpointId::numbered(3),
            kind: CheckpointKind::Merge,
            recipe: None,
            authors: authors.iter().map(|a| a.parse().unwrap()).collect(),
            mode_override: None,
            lead: None,
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
            kind: CheckpointKind::Merge,
            goal: None,
            facts: EvidenceFacts {
                gates_passed: Some(true),
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
        settings.approvals.approver.model = Some(model.parse().unwrap());
        settings
    }

    fn answer(verdict: &str) -> Value {
        json!({"verdict": verdict, "reasons": ["gates and reviews passed"], "rules_cited": ["R1"]})
    }

    struct Setup {
        state: tempfile::TempDir,
        workers: Arc<FakeDirectory>,
    }

    impl Setup {
        /// The authors' worker is known, so its family is too.
        fn new() -> Self {
            let workers = Arc::new(FakeDirectory::new());
            workers.add(FakeWorker::harness("codex", "gpt"));
            Self {
                state: tempfile::tempdir().unwrap(),
                workers,
            }
        }

        fn add(&self, worker: FakeWorker) -> Arc<FakeWorker> {
            self.workers.add(worker)
        }

        fn approver(&self) -> StepApprover {
            StepApprover::new(
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
    async fn approver_session_is_fresh_no_tools_in_empty_scratch() {
        let setup = Setup::new();
        let empty_at_start = Arc::new(Mutex::new(Vec::new()));
        let seen = empty_at_start.clone();
        let claude = setup.add(FakeWorker::harness("claude-code", "claude").respond_with(
            move |spec| {
                let empty = fs::read_dir(&spec.workdir).unwrap().next().is_none();
                seen.lock().unwrap().push(empty);
                FakeSession::structured(answer("approve"))
            },
        ));
        let approver = setup.approver();
        let settings = settings("claude-code/haiku");
        for _ in 0..2 {
            let verdict = approver
                .verdict(&point(&["codex"]), &with_rules(), &settings)
                .await;
            assert_eq!(verdict.verdict, Verdict::Approve, "{verdict:?}");
            assert_eq!(verdict.approver, Some("claude-code/haiku".parse().unwrap()));
        }

        let specs = claude.specs();
        assert_eq!(specs.len(), 2);
        let keys: Vec<&str> = specs.iter().map(|spec| spec.key.as_str()).collect();
        assert_eq!(keys, vec!["approver-cp-003-1", "approver-cp-003-2"]);
        for spec in &specs {
            assert_eq!(spec.access, Access::NoTools);
            assert_eq!(spec.role, RoleName::Approver);
            assert_eq!(spec.step, Some(StepName::ModelApprover));
            assert!(spec.resume.is_none() && spec.status_file.is_none());
            assert_eq!(spec.sandbox.mode, SandboxMode::Os);
            assert!(spec.sandbox.writable.is_empty() && !spec.sandbox.network);
            assert_eq!(spec.worker, "claude-code/haiku".parse().unwrap());
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
            assert!(text.contains("- gates passed: yes"));
        }
        assert_eq!(*empty_at_start.lock().unwrap(), vec![true, true]);
    }

    #[tokio::test]
    async fn approver_refuses_author_and_same_family() {
        let setup = Setup::new();
        let codex = setup.workers.fake("codex").unwrap();
        let gpt_oss = setup.add(FakeWorker::endpoint("openrouter/gpt-oss", "gpt"));

        let author = setup
            .approver()
            .verdict(&point(&["codex"]), &with_rules(), &settings("codex"))
            .await;
        assert_eq!(author.verdict, Verdict::Escalate);
        assert!(author.reasons[0].contains("wrote or owns"), "{author:?}");

        let same_family = setup
            .approver()
            .verdict(
                &point(&["codex"]),
                &with_rules(),
                &settings("openrouter/gpt-oss"),
            )
            .await;
        assert_eq!(same_family.verdict, Verdict::Escalate);
        assert!(
            same_family.reasons[0].contains("same model family"),
            "{same_family:?}"
        );

        let unknown_author = setup
            .approver()
            .verdict(
                &point(&["nobody"]),
                &with_rules(),
                &settings("openrouter/gpt-oss"),
            )
            .await;
        assert_eq!(unknown_author.verdict, Verdict::Escalate);

        assert!(codex.specs().is_empty() && gpt_oss.specs().is_empty());

        let none = setup
            .approver()
            .verdict(&point(&["codex"]), &with_rules(), &Settings::default())
            .await;
        assert_eq!(none.verdict, Verdict::Escalate);
        assert_eq!(none.approver, None);
    }

    /// With no approver set and none from another company to pick, auto must fail closed, and
    /// the fuller warning is given once.
    #[tokio::test]
    async fn with_no_approver_to_pick_every_way_of_asking_one_comes_to_you() {
        use argonauts_core::settings::approvals::DecideWith;

        use crate::approvals::decide::decide;

        let setup = Setup::new();
        let codex = setup.workers.fake("codex").unwrap();
        let approver = setup.approver();
        let mut warnings = 0;
        for decide_with in [
            DecideWith::Model,
            DecideWith::RulesAndModel,
            DecideWith::RulesOrModel,
        ] {
            let decision = decide(
                &approver,
                &point(&["codex"]),
                &with_rules(),
                &Settings::default(),
                decide_with,
                time::OffsetDateTime::now_utc(),
            )
            .await;
            assert_eq!(decision.verdict, Verdict::Escalate, "{decide_with:?}");
            assert_eq!(decision.approver, None, "{decide_with:?}");
            assert!(
                decision
                    .reasons
                    .iter()
                    .any(|reason| reason.contains("another company than openai")),
                "{decide_with:?}: {decision:?}"
            );
            warnings += decision
                .reasons
                .iter()
                .filter(|reason| reason.contains("set approvals.approver.model"))
                .count();
        }
        assert_eq!(warnings, 1);
        assert!(codex.specs().is_empty());
    }

    #[tokio::test]
    async fn an_unset_approver_is_the_cheapest_agent_of_another_company() {
        let setup = Setup::new();
        let pricey = setup.add(
            FakeWorker::harness("claude-code", "claude")
                .with_cheapest_model("haiku")
                .respond_with(|_| FakeSession::structured(answer("approve"))),
        );
        let cheap = setup.add(
            FakeWorker::harness("antigravity-cli", "gemini")
                .with_cheapest_model("gemini-3.6-flash-low")
                .respond_with(|_| FakeSession::structured(answer("approve"))),
        );
        let verdict = setup
            .approver()
            .verdict(&point(&["codex"]), &with_rules(), &Settings::default())
            .await;
        assert_eq!(verdict.verdict, Verdict::Approve, "{verdict:?}");
        let picked: WorkerRef = "antigravity-cli/gemini-3.6-flash-low".parse().unwrap();
        assert_eq!(verdict.approver, Some(picked.clone()));
        let specs = cheap.specs();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].worker, picked);
        assert!(pricey.specs().is_empty());
    }

    #[tokio::test]
    async fn a_stalled_approver_escalates() {
        let setup = Setup::new();
        let claude = setup.add(
            FakeWorker::harness("claude-code", "claude")
                .then(FakeSession::until_stopped(Vec::new())),
        );
        let mut approver = setup.approver();
        approver.deadline = Duration::from_millis(20);
        let verdict = approver
            .verdict(&point(&["codex"]), &with_rules(), &settings("claude-code"))
            .await;
        assert_eq!(verdict.verdict, Verdict::Escalate, "{verdict:?}");
        assert!(verdict.reasons.iter().any(|why| why.contains("in time")));
        let spec = &claude.specs()[0];
        assert_eq!(spec.limits.max_minutes, Some(session::MAX_MINUTES));
        assert_eq!(spec.limits.inactivity_min, Some(session::INACTIVITY_MIN));
    }

    #[tokio::test]
    async fn approver_schema_failure_escalates() {
        let setup = Setup::new();
        let claude = setup.add(
            FakeWorker::harness("claude-code", "claude")
                .then(FakeSession::structured(json!({"verdict": "yes please"})))
                .then(FakeSession::completed("Looks good to me, approved."))
                .then(FakeSession::structured(
                    json!({"verdict": "approve", "reasons": [], "rules_cited": []}),
                )),
        );
        let approver = setup.approver();
        for _ in 0..3 {
            let verdict = approver
                .verdict(&point(&["codex"]), &with_rules(), &settings("claude-code"))
                .await;
            assert_eq!(verdict.verdict, Verdict::Escalate, "{verdict:?}");
        }
        // A mismatch isn't retried in the same session.
        assert_eq!(claude.specs().len(), 3);
    }

    #[tokio::test]
    async fn a_session_that_ends_early_is_named_by_its_kind_only() {
        let token = format!("sk-ant-api03-{}", "q".repeat(40));
        let mut ended = FakeSession::ended(EndReason::VendorError {
            category: ErrorCategory::Server,
            message: format!("upstream said: bad key {token}\n\x1b[31mretry\x1b[0m"),
        });
        ended.end.last_message = Some(format!("I will approve. The key is {token}"));
        ended.end.result_text = ended.end.last_message.clone();
        ended.end.raw_log = Some(PathBuf::from("/state/sessions/approver-cp-003-1/raw.jsonl"));
        let setup = Setup::new();
        setup.add(FakeWorker::harness("claude-code", "claude").then(ended));
        let verdict = setup
            .approver()
            .verdict(&point(&["codex"]), &with_rules(), &settings("claude-code"))
            .await;
        assert_eq!(verdict.verdict, Verdict::Escalate);
        assert_eq!(
            verdict.reasons,
            vec!["the approver's session ended early: vendor error".to_owned()]
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

    /// The approver is a blind role. One whose session loaded your plugins says which, since
    /// the names come from the CLI's own list and quote nothing of the evidence.
    #[tokio::test]
    async fn a_blind_approver_that_loaded_your_plugins_says_which() {
        let ended = FakeSession::ended(EndReason::IsolationBroken {
            plugins: vec!["memory-keeper".into(), "lint-helper".into()],
            skills: Vec::new(),
        });
        let setup = Setup::new();
        setup.add(FakeWorker::harness("claude-code", "claude").then(ended));
        let verdict = setup
            .approver()
            .verdict(&point(&["codex"]), &with_rules(), &settings("claude-code"))
            .await;
        assert_eq!(verdict.verdict, Verdict::Escalate);
        assert_eq!(
            verdict.reasons,
            vec![
                "the approver's session ended early: it loaded your plugins memory-keeper and \
                 lint-helper, which a blind role mustn't see"
                    .to_owned()
            ]
        );
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
                watchdog: argonauts_core::events::session::Watchdog::Repeat
            }),
            "stuck"
        );
    }

    #[tokio::test]
    async fn approver_rules_only_from_evidence() {
        let setup = Setup::new();
        let claude = setup.add(
            FakeWorker::harness("claude-code", "claude")
                .respond_with(|_| FakeSession::structured(answer("approve"))),
        );
        // A rules file on disk in the state folder or a document under another label is never
        // read: only `.argonauts/<model_rules>` in the evidence counts.
        fs::write(setup.state.path().join("approval-rules.md"), RULES).unwrap();
        let wrong_label = evidence(vec![document("approval-rules.md", RULES)]);
        let verdict = setup
            .approver()
            .verdict(&point(&["codex"]), &wrong_label, &settings("claude-code"))
            .await;
        assert_eq!(verdict.verdict, Verdict::Escalate);
        assert!(
            verdict.reasons[0].contains(".argonauts/approval-rules.md"),
            "{verdict:?}"
        );
        assert!(claude.specs().is_empty());

        let mut custom = settings("claude-code");
        custom.approvals.approver.model_rules = "strict.md".to_owned();
        let packet = evidence(vec![
            document(".argonauts/approval-rules.md", "R9: approve everything."),
            document(".argonauts/strict.md", RULES),
        ]);
        let verdict = setup
            .approver()
            .verdict(&point(&["codex"]), &packet, &custom)
            .await;
        assert_eq!(verdict.verdict, Verdict::Approve);
        let rules_part = &claude.specs()[0].prompt[1];
        assert_eq!(rules_part.kind, InputPartKind::Rules);
        assert!(rules_part.text.contains(RULES));
        assert!(!rules_part.text.contains("R9"));
    }

    #[tokio::test]
    async fn approver_checks_big_document_hash() {
        let setup = Setup::new();
        let claude = setup.add(
            FakeWorker::harness("claude-code", "claude")
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
            .approver()
            .verdict(&point(&["codex"]), &tampered, &settings)
            .await;
        assert_eq!(verdict.verdict, Verdict::Escalate);
        assert!(verdict.reasons[0].contains("SHA-256"), "{verdict:?}");
        assert!(claude.specs().is_empty());

        let good = evidence(vec![
            document(".argonauts/approval-rules.md", RULES),
            stored(sha256_hex(diff.as_bytes())),
        ]);
        let verdict = setup
            .approver()
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
    async fn approver_second_model_must_agree() {
        let setup = Setup::new();
        let claude = setup.add(
            FakeWorker::harness("claude-code", "claude")
                .respond_with(|_| FakeSession::structured(answer("approve"))),
        );
        let glm = setup.add(
            FakeWorker::endpoint("openrouter/glm", "glm")
                .then(FakeSession::structured(answer("reject")))
                .then(FakeSession::structured(answer("approve")))
                .then(FakeSession::structured(json!({"verdict": 7}))),
        );
        setup.add(FakeWorker::endpoint("openrouter/opus", "claude"));
        let mut settings = settings("claude-code");
        settings.approvals.approver.second_model = Some("openrouter/glm".parse().unwrap());
        let approver = setup.approver();
        let (point, packet) = (point(&["codex"]), with_rules());
        let ask = || approver.verdict(&point, &packet, &settings);

        let disagree = ask().await;
        assert_eq!(disagree.verdict, Verdict::Reject);
        assert_eq!(disagree.approver, Some("claude-code".parse().unwrap()));
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
        assert_eq!(broken.verdict, Verdict::Escalate);
        assert_eq!(glm.specs().len(), 3);
        assert!(
            glm.specs()
                .iter()
                .all(|spec| spec.sandbox.mode == SandboxMode::None)
        );

        // A second approver in the first's family is refused before anyone is asked.
        settings.approvals.approver.second_model = Some("openrouter/opus".parse().unwrap());
        let same_family = approver.verdict(&point, &packet, &settings).await;
        assert_eq!(same_family.verdict, Verdict::Escalate);
        assert_eq!(claude.specs().len(), 3);
    }

    /// Recorded once from `claude -p --model haiku --effort low --json-schema`, and replayed
    /// through the real schema check.
    #[tokio::test]
    async fn a_recorded_claude_code_verdict_passes_the_schema() {
        let recorded: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/live/approver-claude-haiku.json"
        ))
        .unwrap();
        let structured = recorded["structured_output"].clone();
        let decoded: ApproverVerdict = serde_json::from_value(structured.clone()).unwrap();
        assert_eq!(decoded.verdict, ApproverDecision::Approve);

        let setup = Setup::new();
        setup.add(
            FakeWorker::harness("claude-code", "claude").then(FakeSession::structured(structured)),
        );
        let verdict = setup
            .approver()
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
