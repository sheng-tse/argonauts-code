//! What a fallback is told (`fallback.context`): the task only, the note, the note and the old
//! session's transcript, or the note with a summary in it. Each test drives the engine's `Host`
//! with fake workers on a real temp repo. Most write the transcript files themselves, in place of
//! the transcript writer. The whole-path tests at the end use argonauts-conductor's own planner,
//! which writes them from the run's log on disk, as the app does.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, ensure};
use serde_json::json;
use time::macros::datetime;
use tokio::sync::mpsc;

use argonauts_conductor::fallback::Planner;
use argonauts_core::agents::session_runner::{Access, RunnerEvent};
use argonauts_core::base::layout::run_files;
use argonauts_core::base::names::{AgentId, ModelRef, SessionKey, WorkflowId};
use argonauts_core::models::limits::{
    Candidate, CandidateQuery, FallbackNote, FallbackPlanner, PlanUsageBoard, PlanUsageReading,
    PlanUsageSource, PlanUsageStatus, RecoveryAction, TranscriptPaths, WorkSoFar,
};
use argonauts_core::runs::engine::{EngineCommand, EventSink, RepoContext};
use argonauts_core::runs::events::agent_status::FallbackEvent;
use argonauts_core::runs::events::goal::{GoalEvent, GoalFile, GoalSource};
use argonauts_core::runs::events::run::{Party, RunLifecycle, RunMode, RunStarted};
use argonauts_core::runs::events::session::{EndReason, InputPart, InputPartKind, SessionEvent};
use argonauts_core::runs::events::task::{
    TaskEvent, TaskKind, TaskLimit, TaskSpec, TaskState, TryCause,
};
use argonauts_core::runs::events::workflow::{StepStarted, WorkflowEvent, WorkflowStarted};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::runs::journal::{JournalError, read_all};
use argonauts_core::settings::Settings;
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::settings::fallback::{AgentFallbacks, FallbackContext, FallbackTable, Pick};
use argonauts_core::workflows::steps::StepName;
use argonauts_core::workflows::workflow::{StandardPhase, StepKind, StepRef, Workflow};
use argonauts_core::{AgentEvent, DiffStat, NoteLevel, RepoId, RunId, TaskId};
use argonauts_crew::engine::host::Host;
use argonauts_crew::fallback::{self, rules};
use argonauts_crew::state::RunState;
use argonauts_crew::task::{pipeline, prompt};
use argonauts_testkit::engine::runner::{FakeRunner, FakeSession};
use argonauts_testkit::engine::services::{FakeServices, MemorySink, TEST_RUN, engine_context_in};
use argonauts_testkit::home::repo::TempRepo;
use argonauts_testkit::pty::deadline;

use crate::support::shared_in;

const OPUS: &str = "claude-code/opus";
const CODEX: &str = "codex/gpt-5.6";
/// Codex's cheapest model, which summarizes for it when no summarizer is set.
const LUNA: &str = "codex/gpt-6-luna";
const FLASH: &str = "agy/gemini-3.6-flash";
const TRANSCRIPT: &str = "# task-001 · claude-code/opus · usage limit\n\nRead src/store.rs\n";
const SUMMARY: &str = "It tried a Redis pool, kept the store trait, and left the TTL tests.";

fn worker(name: &str) -> ModelRef {
    name.parse().expect("a valid model")
}

fn task() -> TaskId {
    TaskId::numbered(1)
}

/// Who writes the transcript a fallback names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Writer {
    /// The test, before anything runs, with a planner that names it in its place.
    Test,
    /// Argonauts Code's planner, from the run's log on disk.
    Conductor,
}

/// `task-001`, planned for claude-code/opus in its own worktree, with its transcript in both
/// places the transcript writer puts it: written by the test, or by Argonauts Code's planner.
struct Bench {
    services: FakeServices,
    host: Host,
    sink: Arc<MemorySink>,
    /// The task's worktree.
    tree: PathBuf,
    paths: TranscriptPaths,
    repo: TempRepo,
    _commands: mpsc::Sender<EngineCommand>,
}

impl Bench {
    async fn new(settings: Settings) -> anyhow::Result<Self> {
        Self::in_state(settings, TaskState::NeedsDecision).await
    }

    async fn in_state(settings: Settings, state: TaskState) -> anyhow::Result<Self> {
        Self::with(settings, state, Writer::Test).await
    }

    /// Ready to build, with Argonauts Code's planner and the run's log on disk, as the app wires
    /// them. The transcript appears once a fallback's note is built.
    async fn whole(settings: Settings) -> anyhow::Result<Self> {
        Self::with(settings, TaskState::Ready, Writer::Conductor).await
    }

    async fn with(settings: Settings, state: TaskState, writer: Writer) -> anyhow::Result<Self> {
        let services = FakeServices::new();
        let repo = TempRepo::new()?;
        let base = repo.head()?;
        let state_dir = repo.scratch().join("argonauts");
        let layout = RepoContext {
            id: RepoId::from_canonical_path(repo.root()),
            root: repo.root().to_path_buf(),
            run_dir: state_dir.join("state/runs").join(TEST_RUN),
            worktrees: state_dir.join("worktrees").join(TEST_RUN),
            base_worktree: state_dir.join("worktrees").join(TEST_RUN).join("base"),
        };
        let tree = layout.worktrees.join(task().as_str());
        std::fs::create_dir_all(&layout.worktrees)?;
        let branch = format!("argonauts/{TEST_RUN}/{}", task());
        let tree_arg = tree.to_string_lossy().into_owned();
        repo.git(&[
            "worktree", "add", "--quiet", "-b", &branch, &tree_arg, &base,
        ])?;

        let paths = TranscriptPaths {
            worktree: tree.join(".argonauts/fallback/1/transcript.md"),
            run: layout
                .run_dir
                .join("tasks")
                .join(task().as_str())
                .join("fallback-1/transcript.md"),
        };
        if writer == Writer::Test {
            for copy in [&paths.worktree, &paths.run] {
                std::fs::create_dir_all(copy.parent().context("a transcript dir")?)?;
                std::fs::write(copy, TRANSCRIPT)?;
            }
        }

        let history: Vec<RunEvent> = [
            (None, started(&repo, &base)),
            (None, understood(&repo)),
            (None, workflow_started()?),
            (None, build_step()),
            (
                Some(task()),
                RunEventKind::Task(TaskEvent::Created(Box::new(spec()))),
            ),
            (
                Some(task()),
                RunEventKind::Task(TaskEvent::Worktree {
                    path: tree.clone(),
                    branch,
                    base: base.clone(),
                }),
            ),
            (
                Some(task()),
                RunEventKind::Task(TaskEvent::State {
                    from: None,
                    to: state,
                    reason: "set up by the test".to_owned(),
                }),
            ),
        ]
        .into_iter()
        .zip(0..)
        .map(|((task, kind), seq)| event(seq, task, kind))
        .collect();

        let run_dir = layout.run_dir.clone();
        let (mut context, sink, commands) =
            engine_context_in(&services, settings, layout, history.clone());
        match writer {
            Writer::Test => {
                context.services.fallback = Arc::new(WithTranscript(paths.clone()));
            }
            Writer::Conductor => {
                let planner = Planner::new(
                    context.settings.clone(),
                    services.registry.clone(),
                    services.plan_usage.clone(),
                    services.health.clone(),
                    services.track_records.clone(),
                )
                .with_run_dir(run_dir.clone());
                context.services.fallback = Arc::new(planner);
                context.sink = Arc::new(OnDisk::new(sink.clone(), &run_dir, &history)?);
            }
        }
        let shared = shared_in(&repo, &context, RunState::fold(&history))?;
        Ok(Self {
            services,
            host: Host::new(Arc::new(shared)),
            sink,
            tree,
            paths,
            repo,
            _commands: commands,
        })
    }

    fn add(&self, fake: FakeRunner) -> Arc<FakeRunner> {
        self.services.workers.add(fake)
    }

    /// Codex takes the task, as a fallback's rules give it.
    async fn fall_back_to_codex(&self) -> anyhow::Result<()> {
        let action = RecoveryAction::Fallback {
            to: worker(CODEX),
            note: Box::new(note(None)),
            checkpoint: None,
        };
        fallback::apply(&self.host, &task(), action).await?;
        Ok(())
    }

    /// What a fresh session of `to` is told after `cause`.
    async fn told(&self, to: &str, cause: TryCause) -> anyhow::Result<Option<InputPart>> {
        Ok(prompt::note_part(&self.host, &task(), &worker(to), cause, "", None).await?)
    }

    fn fell_back_context(&self) -> Option<FallbackContext> {
        self.events().into_iter().find_map(|kind| match kind {
            RunEventKind::Fallback(FallbackEvent::FellBack { context, .. }) => context,
            _ => None,
        })
    }

    fn notes(&self) -> usize {
        self.events()
            .into_iter()
            .filter(|kind| matches!(kind, RunEventKind::Fallback(FallbackEvent::Note { .. })))
            .count()
    }

    fn warnings(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .filter_map(|kind| match kind {
                RunEventKind::Note(note) if note.level == NoteLevel::Warn => Some(note.text),
                _ => None,
            })
            .collect()
    }

    fn events(&self) -> Vec<RunEventKind> {
        self.sink
            .events()
            .into_iter()
            .map(|event| event.kind)
            .collect()
    }

    fn state(&self) -> Option<TaskState> {
        self.host
            .read(|state| state.tasks.get(&task()).map(|record| record.state))
    }

    fn summary_file(&self) -> PathBuf {
        self.paths.run.with_file_name("summary.md")
    }

    /// Argonauts Code's answer when claude-code stops: `to` takes the task.
    fn fallback_at_the_stop(&self, to: &str) {
        self.services.limits.set_next(RecoveryAction::Fallback {
            to: worker(to),
            note: Box::new(note(None)),
            checkpoint: None,
        });
    }

    /// Builds the task from its step until it's in `state`.
    async fn run_until(&self, state: TaskState) -> anyhow::Result<()> {
        let at = rules::step_now(&self.host)?;
        let c1 = task();
        let reached = async {
            while self.state() != Some(state) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let run = async {
            tokio::select! {
                ran = pipeline::run_task(&self.host, &c1, &at) => {
                    ran?;
                }
                () = reached => {}
            }
            anyhow::Ok(())
        };
        tokio::time::timeout(deadline::scaled(Duration::from_secs(60)), run)
            .await
            .with_context(|| format!("{c1} never got to {state:?}"))?
    }

    /// The task's events as the run's log on disk has them, which is what Argonauts Code reads.
    fn logged(&self) -> anyhow::Result<Vec<RunEvent>> {
        let file = self.host.repo().run_dir.join(run_files::EVENTS);
        Ok(read_all(&file)?
            .events
            .into_iter()
            .filter(|event| event.task_id == Some(task()))
            .collect())
    }

    /// Fails unless git ignores `path` in the worktree `tree`.
    fn ignored(&self, tree: &Path, path: &Path) -> anyhow::Result<()> {
        let tree = tree.to_string_lossy();
        let path = path.to_string_lossy();
        self.repo
            .git(&["-C", &tree, "check-ignore", "--quiet", &path])?;
        Ok(())
    }
}

/// The run's log on disk as well as in memory: the engine reads a task's events back from the
/// file for its fallback note.
struct OnDisk {
    memory: Arc<MemorySink>,
    file: PathBuf,
}

impl OnDisk {
    fn new(memory: Arc<MemorySink>, run_dir: &Path, history: &[RunEvent]) -> anyhow::Result<Self> {
        std::fs::create_dir_all(run_dir)?;
        let sink = Self {
            memory,
            file: run_dir.join(run_files::EVENTS),
        };
        for event in history {
            sink.append(event)?;
        }
        Ok(sink)
    }

    fn append(&self, event: &RunEvent) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file)?
            .write_all(&line)
    }
}

impl EventSink for OnDisk {
    fn record(
        &self,
        task: Option<&TaskId>,
        session: Option<&SessionKey>,
        source: &str,
        kind: RunEventKind,
    ) -> Result<RunEvent, JournalError> {
        let event = self.memory.record(task, session, source, kind)?;
        self.append(&event).map_err(|err| JournalError::Io {
            path: self.file.clone(),
            message: err.to_string(),
        })?;
        Ok(event)
    }
}

/// Builds the note with the transcript's paths filled in, as the transcript writer does.
struct WithTranscript(TranscriptPaths);

impl FallbackPlanner for WithTranscript {
    fn candidates(&self, _query: &CandidateQuery) -> Vec<Candidate> {
        Vec::new()
    }

    fn note(&self, _task: &TaskSpec, _events: &[RunEvent]) -> FallbackNote {
        note(Some(self.0.clone()))
    }
}

fn note(transcript: Option<TranscriptPaths>) -> FallbackNote {
    FallbackNote {
        task: spec(),
        goal_sha256: None,
        notes: Vec::new(),
        work: WorkSoFar {
            branch: format!("argonauts/{TEST_RUN}/{}", task()),
            commit: None,
            stat: DiffStat::default(),
        },
        happened: vec!["Edited src/store.rs (+14 −2)".to_owned()],
        last_todo: Vec::new(),
        checks: Vec::new(),
        why_stopped: "usage limit, resets at 3:45 PM".to_owned(),
        last_words: None,
        last_message: None,
        limit_left: None,
        transcript,
        summary: None,
    }
}

fn with_context(context: FallbackContext) -> Settings {
    let mut settings = Settings::default();
    settings.fallback.context = Some(context);
    settings
}

fn replace_context(context: FallbackContext) -> AgentFallbacks {
    AgentFallbacks {
        context: Some(context),
        ..AgentFallbacks::default()
    }
}

fn claude() -> AgentId {
    worker(OPUS).agent_id()
}

fn summary(text: &str) -> FakeSession {
    FakeSession::structured(json!({ "summary": text }))
}

fn transcript_line(bench: &Bench) -> String {
    format!(
        "The old session's whole transcript is at {}. Read it only if you need more detail \
         than this note gives.",
        bench.paths.worktree.display()
    )
}

fn note_text(part: Option<InputPart>) -> anyhow::Result<String> {
    let part = part.context("a fallback part")?;
    ensure!(part.kind == InputPartKind::Fallback, "{part:?}");
    Ok(part.text)
}

#[tokio::test]
async fn fresh_gives_the_task_only() -> anyhow::Result<()> {
    let bench = Bench::new(with_context(FallbackContext::Fresh)).await?;
    bench.add(FakeRunner::cli(CODEX, "openai"));
    bench.fall_back_to_codex().await?;
    let notes = bench.notes();

    let told = bench.told(CODEX, TryCause::AfterFailure).await?;
    ensure!(told.is_none(), "fresh still told {told:?}");
    ensure!(bench.notes() == notes, "fresh saved a note it never sent");
    ensure!(bench.fell_back_context() == Some(FallbackContext::Fresh));
    Ok(())
}

#[tokio::test]
async fn note_renders_the_note_alone() -> anyhow::Result<()> {
    let bench = Bench::new(with_context(FallbackContext::Note)).await?;
    bench.add(FakeRunner::cli(CODEX, "openai"));
    bench.fall_back_to_codex().await?;

    let text = note_text(bench.told(CODEX, TryCause::AfterFailure).await?)?;
    ensure!(text.starts_with("This task fell back to you from another session."));
    ensure!(text.contains("Edited src/store.rs (+14 −2)"), "{text}");
    ensure!(
        !text.contains("transcript"),
        "the note named the transcript: {text}"
    );
    ensure!(bench.fell_back_context() == Some(FallbackContext::Note));
    Ok(())
}

#[tokio::test]
async fn note_and_transcript_is_the_default() -> anyhow::Result<()> {
    let bench = Bench::new(Settings::default()).await?;
    bench.add(FakeRunner::cli(CODEX, "openai"));
    bench.fall_back_to_codex().await?;

    let text = note_text(bench.told(CODEX, TryCause::AfterFailure).await?)?;
    ensure!(text.ends_with(&transcript_line(&bench)), "{text}");
    // The note names the copy in the worktree, which every CLI can read.
    ensure!(!text.contains(&bench.paths.run.display().to_string()));
    ensure!(bench.fell_back_context() == Some(FallbackContext::NoteAndTranscript));
    Ok(())
}

#[tokio::test]
async fn note_and_summary_puts_the_cheapest_models_summary_in_the_note() -> anyhow::Result<()> {
    let bench = Bench::new(with_context(FallbackContext::NoteAndSummary)).await?;
    bench.add(FakeRunner::cli(CODEX, "openai").with_cheapest_model("gpt-6-luna"));
    let luna = bench.add(FakeRunner::cli(LUNA, "openai").then(summary(SUMMARY)));
    bench.fall_back_to_codex().await?;

    let text = note_text(bench.told(CODEX, TryCause::AfterFailure).await?)?;
    ensure!(text.contains(SUMMARY), "{text}");
    ensure!(!text.contains("transcript.md"), "{text}");

    let specs = luna.specs();
    ensure!(specs.len() == 1, "{} summary sessions", specs.len());
    let spec = &specs[0];
    ensure!(spec.access == Access::NoTools, "{:?}", spec.access);
    ensure!(spec.step == Some(StepName::FallbackSummary));
    ensure!(spec.task == Some(task()));
    ensure!(
        spec.prompt
            .iter()
            // Parts go out with a blank line between them.
            .any(|part| part.label == "transcript" && part.text.trim_end() == TRANSCRIPT.trim_end()),
        "the summarizer didn't get the transcript"
    );

    // Metered like any other step, and saved next to both copies of the transcript.
    let ended = bench
        .events()
        .into_iter()
        .any(|kind| matches!(kind, RunEventKind::Session(SessionEvent::Ended { .. })));
    ensure!(ended, "the summary's session wasn't recorded");
    ensure!(std::fs::read_to_string(bench.summary_file())?.trim() == SUMMARY);
    let beside = bench.paths.worktree.with_file_name("summary.md");
    ensure!(std::fs::read_to_string(beside)?.trim() == SUMMARY);

    // Another try of a race at the same fallback reads the saved summary.
    let again = note_text(bench.told(CODEX, TryCause::AfterFailure).await?)?;
    ensure!(again.contains(SUMMARY));
    ensure!(luna.specs().len() == 1, "the summary ran twice");
    ensure!(bench.warnings().is_empty(), "{:?}", bench.warnings());
    Ok(())
}

#[tokio::test]
async fn the_summarizer_setting_names_who_summarizes() -> anyhow::Result<()> {
    let mut settings = with_context(FallbackContext::NoteAndSummary);
    settings.fallback.summarizer = Some(worker(FLASH));
    let bench = Bench::new(settings).await?;
    bench.add(FakeRunner::cli(CODEX, "openai").with_cheapest_model("gpt-6-luna"));
    let flash = bench.add(FakeRunner::cli(FLASH, "google").then(summary(SUMMARY)));
    bench.fall_back_to_codex().await?;

    let text = note_text(bench.told(CODEX, TryCause::AfterFailure).await?)?;
    ensure!(text.contains(SUMMARY), "{text}");
    ensure!(flash.specs().len() == 1);
    Ok(())
}

#[tokio::test]
async fn a_failed_summary_goes_on_as_note_and_transcript() -> anyhow::Result<()> {
    let bench = Bench::new(with_context(FallbackContext::NoteAndSummary)).await?;
    bench.add(FakeRunner::cli(CODEX, "openai").with_cheapest_model("gpt-6-luna"));
    bench.add(
        FakeRunner::cli(LUNA, "openai").then(FakeSession::ended(EndReason::Crashed {
            message: "segfault".to_owned(),
        })),
    );
    bench.fall_back_to_codex().await?;

    let text = note_text(bench.told(CODEX, TryCause::AfterFailure).await?)?;
    ensure!(text.ends_with(&transcript_line(&bench)), "{text}");
    ensure!(!text.contains(SUMMARY));
    let warnings = bench.warnings();
    ensure!(
        warnings.iter().any(|warning| warning.starts_with(
            "task-001's fallback goes on as note + transcript: the summary by \
             codex/gpt-6-luna failed"
        )),
        "{warnings:?}"
    );
    ensure!(!bench.summary_file().exists(), "a failed summary was saved");
    Ok(())
}

#[tokio::test]
async fn a_summarizer_out_of_plan_usage_is_never_started() -> anyhow::Result<()> {
    let mut settings = with_context(FallbackContext::NoteAndSummary);
    settings.fallback.summarizer = Some(worker(FLASH));
    let bench = Bench::new(settings).await?;
    bench.add(FakeRunner::cli(CODEX, "openai"));
    let flash = bench.add(FakeRunner::cli(FLASH, "google").then(summary(SUMMARY)));
    bench.services.plan_usage.observe(PlanUsageReading {
        agent_id: worker(FLASH).agent_id(),
        status: PlanUsageStatus::Rejected,
        used: Some(1.0),
        resets_at: None,
        window: Some("five_hour".to_owned()),
        source: PlanUsageSource::RateLimitEvent,
        observed_at: bench.host.now(),
        detail: None,
        wrap_up: false,
    });
    bench.fall_back_to_codex().await?;

    let text = note_text(bench.told(CODEX, TryCause::AfterFailure).await?)?;
    ensure!(text.ends_with(&transcript_line(&bench)), "{text}");
    ensure!(flash.specs().is_empty(), "the summarizer started anyway");
    let expected = "task-001's fallback goes on as note + transcript: agy/gemini-3.6-flash is out \
                    of plan usage";
    ensure!(
        bench.warnings().iter().any(|warning| warning == expected),
        "{:?}",
        bench.warnings()
    );
    Ok(())
}

#[tokio::test]
async fn the_stopped_agents_context_wins_over_the_global_one() -> anyhow::Result<()> {
    let mut settings = with_context(FallbackContext::Note);
    let fresh = replace_context(FallbackContext::Fresh);
    settings.fallback.replace.insert(claude(), fresh);
    let bench = Bench::new(settings).await?;
    bench.add(FakeRunner::cli(CODEX, "openai"));

    // A race try starts while the task is still claude-code's, whose table applies.
    ensure!(bench.told(CODEX, TryCause::Fallback).await?.is_none());

    bench.fall_back_to_codex().await?;
    ensure!(bench.fell_back_context() == Some(FallbackContext::Fresh));
    ensure!(bench.told(CODEX, TryCause::AfterFailure).await?.is_none());
    // Codex's own session rotating has no table of its own, so the global one applies.
    let rotated = note_text(bench.told(CODEX, TryCause::Rotation).await?)?;
    ensure!(!rotated.contains("transcript"), "{rotated}");
    Ok(())
}

#[tokio::test]
async fn the_roles_context_wins_over_the_agents() -> anyhow::Result<()> {
    let mut settings = Settings::default();
    let fresh = replace_context(FallbackContext::Fresh);
    settings.fallback.replace.insert(claude(), fresh);
    let role = FallbackTable {
        context: Some(FallbackContext::Note),
        ..FallbackTable::default()
    };
    settings.fallback.roles.insert(CrewRole::Worker, role);
    let bench = Bench::new(settings).await?;
    bench.add(FakeRunner::cli(CODEX, "openai"));
    bench.fall_back_to_codex().await?;

    let text = note_text(bench.told(CODEX, TryCause::AfterFailure).await?)?;
    ensure!(!text.contains("transcript"), "{text}");
    ensure!(bench.fell_back_context() == Some(FallbackContext::Note));
    Ok(())
}

#[tokio::test]
async fn the_roles_agent_context_wins_over_the_role() -> anyhow::Result<()> {
    let mut settings = Settings::default();
    let mut role = FallbackTable {
        context: Some(FallbackContext::Fresh),
        ..FallbackTable::default()
    };
    let transcript = replace_context(FallbackContext::NoteAndTranscript);
    role.replace.insert(claude(), transcript);
    settings.fallback.roles.insert(CrewRole::Worker, role);
    let bench = Bench::new(settings).await?;
    bench.add(FakeRunner::cli(CODEX, "openai"));
    bench.fall_back_to_codex().await?;

    let text = note_text(bench.told(CODEX, TryCause::AfterFailure).await?)?;
    ensure!(text.ends_with(&transcript_line(&bench)), "{text}");
    Ok(())
}

#[tokio::test]
async fn a_give_back_follows_the_fallbacks_agent() -> anyhow::Result<()> {
    let mut settings = Settings::default();
    let note = replace_context(FallbackContext::Note);
    settings
        .fallback
        .replace
        .insert(worker(CODEX).agent_id(), note);
    let bench = Bench::new(settings).await?;
    bench.add(FakeRunner::cli(CODEX, "openai"));
    bench.fall_back_to_codex().await?;
    // Claude Code is back and takes its task again, as `on_return = "give_back"` records it.
    let back = TaskEvent::Reassigned {
        from: Some(worker(CODEX)),
        to: worker(OPUS),
        by: Party::Rules,
        rule_override: None,
    };
    bench
        .host
        .record(Some(&task()), None, RunEventKind::Task(back))?;

    let text = note_text(bench.told(OPUS, TryCause::Reassigned).await?)?;
    ensure!(!text.contains("transcript"), "{text}");
    Ok(())
}

#[tokio::test]
async fn a_full_context_rotates_with_the_transcript() -> anyhow::Result<()> {
    let bench = Bench::in_state(Settings::default(), TaskState::Ready).await?;
    let crashed = || {
        FakeSession::ended(EndReason::Crashed {
            message: "segfault".to_owned(),
        })
    };
    let opus = bench.add(
        FakeRunner::cli(OPUS, "anthropic")
            .then(FakeSession::ended(EndReason::ContextFull))
            .then(crashed())
            .then(crashed()),
    );
    bench.services.limits.set_next(RecoveryAction::Rotate {
        note: Box::new(note(Some(bench.paths.clone()))),
    });

    let at = rules::step_now(&bench.host)?;
    // The second session crashes, goes again once on its own session, crashes
    // again, and the task needs you, where it waits.
    let needs_you = async {
        while bench.state() != Some(TaskState::NeedsYou) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    let c1 = task();
    tokio::select! {
        ran = pipeline::run_task(&bench.host, &c1, &at) => {
            ran?;
        }
        () = needs_you => {}
    }

    let specs = opus.specs();
    ensure!(specs.len() == 3, "{} sessions", specs.len());
    ensure!(
        specs[1].key == specs[2].key,
        "the crash went again on its own session"
    );
    let first = &specs[0].prompt;
    ensure!(
        first
            .iter()
            .all(|part| part.kind != InputPartKind::Fallback)
    );
    let fallback = specs[1]
        .prompt
        .iter()
        .find(|part| part.kind == InputPartKind::Fallback)
        .context("the rotated session got no note")?;
    ensure!(
        fallback.text.contains("Edited src/store.rs"),
        "{}",
        fallback.text
    );
    ensure!(
        fallback.text.ends_with(&transcript_line(&bench)),
        "{}",
        fallback.text
    );
    Ok(())
}

#[tokio::test]
async fn the_note_names_the_transcript_the_conductor_wrote() -> anyhow::Result<()> {
    let bench = Bench::whole(Settings::default()).await?;
    bench.add(FakeRunner::cli(OPUS, "anthropic").then(stopped_on_a_usage_limit()));
    let codex = bench.add(FakeRunner::cli(CODEX, "openai").then(crashed()));
    bench.fallback_at_the_stop(CODEX);
    bench.run_until(TaskState::NeedsYou).await?;

    let note = first_note(&codex)?;
    ensure!(note.ends_with(&transcript_line(&bench)), "{note}");
    // Written from the old session's events in the run's log.
    let written = std::fs::read_to_string(&bench.paths.worktree)?;
    let title = "# task-001 Redis store · claude-code/opus · why it stopped: usage limit\n";
    ensure!(written.starts_with(title), "{written}");
    for line in ["Read src/store.rs", STORE, "The store has no pool yet."] {
        ensure!(written.contains(line), "no {line:?} in\n{written}");
    }
    ensure!(std::fs::read_to_string(&bench.paths.run)? == written);
    // Never committed, though the task's work is.
    bench.ignored(&bench.tree, &bench.paths.worktree)?;
    let committed = bench.repo.git(&[
        "-C",
        &bench.tree.to_string_lossy(),
        "ls-tree",
        "-r",
        "--name-only",
        "HEAD",
    ])?;
    ensure!(!committed.contains(".argonauts/fallback"), "{committed}");
    Ok(())
}

#[tokio::test]
async fn the_summary_reads_the_transcript_the_conductor_wrote() -> anyhow::Result<()> {
    let bench = Bench::whole(with_context(FallbackContext::NoteAndSummary)).await?;
    bench.add(FakeRunner::cli(OPUS, "anthropic").then(stopped_on_a_usage_limit()));
    let codex = bench.add(
        FakeRunner::cli(CODEX, "openai")
            .with_cheapest_model("gpt-6-luna")
            .then(crashed()),
    );
    let luna = bench.add(FakeRunner::cli(LUNA, "openai").then(summary(SUMMARY)));
    bench.fallback_at_the_stop(CODEX);
    bench.run_until(TaskState::NeedsYou).await?;

    let written = std::fs::read_to_string(&bench.paths.run)?;
    ensure!(written.contains("Read src/store.rs"), "{written}");
    let asked = luna.specs();
    ensure!(asked.len() == 1, "{} summary sessions", asked.len());
    ensure!(
        asked[0]
            .prompt
            .iter()
            .any(|part| part.label == "transcript" && part.text.trim_end() == written.trim_end()),
        "the summarizer didn't read the transcript Argonauts Code wrote"
    );
    let note = first_note(&codex)?;
    ensure!(note.contains(SUMMARY), "{note}");
    ensure!(!note.contains("transcript.md"), "{note}");
    for copy in [&bench.paths.run, &bench.paths.worktree] {
        let saved = std::fs::read_to_string(copy.with_file_name("summary.md"))?;
        ensure!(saved.trim() == SUMMARY, "{saved}");
    }
    // Nothing warns of the summary: the one warning is Codex's crash, said with its retry.
    ensure!(
        bench.warnings() == ["GPT 5.6 on Codex crashed · trying again in 5s"],
        "{:?}",
        bench.warnings()
    );
    Ok(())
}

/// A race's tries start at once. Each is told of the session that stopped, never of a
/// try that started before it, and reads the transcript's copy in its own tree, since a
/// session reads no worktree but its own.
#[tokio::test]
async fn every_race_try_reads_the_stopped_sessions_transcript() -> anyhow::Result<()> {
    let bench = Bench::whole(Settings::default()).await?;
    bench.add(FakeRunner::cli(OPUS, "anthropic").then(stopped_on_a_usage_limit()));
    let racers = [
        bench.add(FakeRunner::cli(CODEX, "openai").then(crashed())),
        bench.add(FakeRunner::cli(FLASH, "google").then(crashed())),
    ];
    bench.services.limits.set_next(RecoveryAction::Wait {
        agent_id: claude(),
        until: None,
        kept_off: None,
    });
    bench.run_until(TaskState::NeedsDecision).await?;

    // Argonauts Code builds the race's note when claude-code stops.
    let note = bench
        .host
        .services()
        .fallback
        .note(&spec(), &bench.logged()?);
    let race = RecoveryAction::Race {
        to: vec![worker(CODEX), worker(FLASH)],
        pick: Pick::FirstPassing,
        note: Box::new(note),
        checkpoint: None,
    };
    fallback::apply(&bench.host, &task(), race).await?;

    let written = std::fs::read_to_string(&bench.paths.run)?;
    ensure!(written.contains("Read src/store.rs"), "{written}");
    for racer in &racers {
        let specs = racer.specs();
        let spec = specs.first().context("a try never started")?;
        let named = named_transcript(&first_note(racer)?)?;
        ensure!(
            named.starts_with(&spec.workdir),
            "{} was told to read {} outside its own tree",
            spec.model,
            named.display()
        );
        ensure!(
            std::fs::read_to_string(&named)? == written,
            "{} was told of another session",
            spec.model
        );
        bench.ignored(&spec.workdir, &named)?;
    }
    Ok(())
}

/// What `read src/store.rs` shows.
const STORE: &str = "pub struct Store;";

/// Claude Code reads the store, says what it found, and runs into its usage limit.
fn stopped_on_a_usage_limit() -> FakeSession {
    let read = AgentEvent::ToolStarted {
        id: "read-1".to_owned(),
        name: "Read".to_owned(),
        input: json!({ "file_path": "src/store.rs" }),
        parent: None,
    };
    let shown = AgentEvent::ToolFinished {
        id: "read-1".to_owned(),
        ok: true,
        output: Some(STORE.to_owned()),
        parent: None,
    };
    let found = AgentEvent::Message {
        text: "The store has no pool yet.".to_owned(),
        parent: None,
    };
    [read, shown, found].into_iter().fold(
        FakeSession::ended(EndReason::UsageLimit { resets_at: None }),
        |session, event| session.with_event(RunnerEvent::Agent(event)),
    )
}

fn crashed() -> FakeSession {
    FakeSession::ended(EndReason::Crashed {
        message: "segfault".to_owned(),
    })
}

/// The note `fake`'s first session started with.
fn first_note(fake: &FakeRunner) -> anyhow::Result<String> {
    let specs = fake.specs();
    let spec = specs.first().context("no session started")?;
    let note = spec
        .prompt
        .iter()
        .find(|part| part.kind == InputPartKind::Fallback)
        .context("its first session got no note")?;
    Ok(note.text.clone())
}

/// The transcript a note names.
fn named_transcript(note: &str) -> anyhow::Result<PathBuf> {
    let (_, rest) = note
        .rsplit_once("The old session's whole transcript is at ")
        .with_context(|| format!("the note names no transcript: {note}"))?;
    let (path, _) = rest
        .split_once(". Read it only if")
        .with_context(|| format!("the transcript's line is cut: {note}"))?;
    Ok(PathBuf::from(path))
}

fn event(seq: u64, task: Option<TaskId>, kind: RunEventKind) -> RunEvent {
    RunEvent {
        seq,
        ts: datetime!(2026-10-01 10:00 UTC),
        run_id: RunId::new(TEST_RUN).expect("the test run id"),
        task_id: task,
        session: None,
        source: "argonauts-code".to_owned(),
        kind,
    }
}

fn started(repo: &TempRepo, base: &str) -> RunEventKind {
    RunEventKind::Run(RunLifecycle::Started(Box::new(RunStarted {
        request: "Add a Redis store".to_owned(),
        title: "Redis store".to_owned(),
        // Captain, so build sessions need only the approved goal, not a plan as well.
        mode: RunMode::Captain,
        forced: true,
        workflow: "default".to_owned(),
        estimate: None,
        captain_or_crew: None,
        conversation_turn: None,
        continues: None,
        repo_root: repo.root().to_path_buf(),
        base_branch: Some("main".to_owned()),
        base_commit: base.to_owned(),
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
        starts_from: None,
    })))
}

fn understood(repo: &TempRepo) -> RunEventKind {
    RunEventKind::Goal(GoalEvent::Approved {
        file: GoalFile {
            run: RunId::new(TEST_RUN).expect("the test run id"),
            version: 1,
            summary: "A Redis store".to_owned(),
            criteria: Vec::new(),
            in_scope: Vec::new(),
            out_of_scope: Vec::new(),
            assumptions: Vec::new(),
            answers: Vec::new(),
            source: GoalSource::Prompt,
            path: repo.root().join(".argonauts/goal.md"),
            sha256: "0".repeat(64),
        },
        by: Party::User,
    })
}

fn workflow_started() -> anyhow::Result<RunEventKind> {
    let workflow: Workflow = serde_json::from_value(json!({
        "name": "build-only",
        "phases": [{"name": "Build", "standard": "do", "steps": [{"kind": "build"}]}],
    }))?;
    Ok(RunEventKind::Workflow(WorkflowEvent::Started(Box::new(
        WorkflowStarted {
            id: WorkflowId::numbered(1),
            from: None,
            version: "1".to_owned(),
            parent: None,
            workflow,
        },
    ))))
}

fn build_step() -> RunEventKind {
    RunEventKind::Workflow(WorkflowEvent::StepStarted(StepStarted {
        step: StepRef {
            workflow: WorkflowId::numbered(1),
            phase: 0,
            path: vec![0],
        },
        kind: StepKind::Build,
        standard: StandardPhase::Do,
        round: 1,
    }))
}

fn spec() -> TaskSpec {
    TaskSpec {
        id: task(),
        title: "Redis store".to_owned(),
        task: "Build the Redis store".to_owned(),
        kind: TaskKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: Vec::new(),
        worker: Some(worker(OPUS)),
        reviewers: Vec::new(),
        limit: TaskLimit::default(),
        predictions: None,
        item: None,
        way: None,
    }
}
