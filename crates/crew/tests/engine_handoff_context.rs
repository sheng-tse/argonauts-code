//! What a replacement is told (`handoff.context`): the card only, the note, the note
//! and the old session's transcript, or the note with a summary in it. Each test drives the
//! engine's `Host` with fake workers on a real temp repo. Most write the transcript files
//! themselves, standing in for the transcript writer. The whole-path tests at the end use the
//! conductor's own planner, which writes them from the run's log on disk, as the app does.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, ensure};
use serde_json::json;
use time::macros::datetime;
use tokio::sync::mpsc;

use argonauts_conductor::handoff::Planner;
use argonauts_core::engine::{EngineCommand, EventSink, RepoContext};
use argonauts_core::events::card::{
    AttemptCause, CardBudget, CardEvent, CardKind, CardSpec, CardState,
};
use argonauts_core::events::goal::{GoalEvent, GoalFile, GoalSource};
use argonauts_core::events::run::{Party, RunLifecycle, RunMode, RunStarted};
use argonauts_core::events::session::{EndReason, InputPart, InputPartKind, SessionEvent};
use argonauts_core::events::vendor::HandoffEvent;
use argonauts_core::events::workflow::{StepStarted, WorkflowEvent, WorkflowStarted};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::journal::{JournalError, read_all};
use argonauts_core::layout::run_files;
use argonauts_core::limits::{
    Candidate, CandidateQuery, HandoffPacket, HandoffPlanner, QuotaBoard, QuotaReading,
    QuotaSource, QuotaStatus, RecoveryAction, TranscriptPaths, WorkSoFar,
};
use argonauts_core::names::{RoleName, SessionKey, VendorId, WorkerRef, WorkflowId};
use argonauts_core::settings::Settings;
use argonauts_core::settings::handoff::{HandoffContext, HandoffSettings, VendorReplacement};
use argonauts_core::settings::roles::RoleSettings;
use argonauts_core::steps::StepName;
use argonauts_core::worker::{Access, WorkerEvent};
use argonauts_core::workflow::{StandardPhase, StepKind, StepRef, Workflow};
use argonauts_core::{AgentEvent, CardId, DiffStat, NoteLevel, RepoId, RunId};
use argonauts_crew::card::{pipeline, prompt};
use argonauts_crew::engine::host::Host;
use argonauts_crew::engine::shared::Shared;
use argonauts_crew::handoff::{self, rules};
use argonauts_crew::state::RunState;
use argonauts_testkit::repo::TempRepo;
use argonauts_testkit::services::{FakeServices, MemorySink, TEST_RUN, engine_context_in};
use argonauts_testkit::worker::{FakeSession, FakeWorker};

const OPUS: &str = "claude-code/opus";
const CODEX: &str = "codex/gpt-5.6";
/// Codex's cheapest model, which summarizes for it when no summarizer is set.
const LUNA: &str = "codex/gpt-5.6-luna";
const FLASH: &str = "agy/gemini-3.6-flash";
const TRANSCRIPT: &str = "# card-001 · claude-code/opus · usage limit\n\nRead src/store.rs\n";
const SUMMARY: &str = "It tried a Redis pool, kept the store trait, and left the TTL tests.";

fn worker(name: &str) -> WorkerRef {
    name.parse().expect("a valid worker")
}

fn card() -> CardId {
    CardId::numbered(1)
}

/// Who writes the transcript a handoff names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Writer {
    /// The test, before anything runs, with a planner that names it standing in.
    StandIn,
    /// The conductor's planner, from the run's log on disk.
    Conductor,
}

/// `card-001`, planned for claude-code/opus in its own worktree, with its transcript in both
/// places the transcript writer puts it: written by the test, or by the conductor's planner.
struct Bench {
    services: FakeServices,
    host: Host,
    sink: Arc<MemorySink>,
    /// The card's worktree.
    tree: PathBuf,
    paths: TranscriptPaths,
    repo: TempRepo,
    _commands: mpsc::Sender<EngineCommand>,
}

impl Bench {
    async fn new(settings: Settings) -> anyhow::Result<Self> {
        Self::in_state(settings, CardState::NeedsDecision).await
    }

    async fn in_state(settings: Settings, state: CardState) -> anyhow::Result<Self> {
        Self::with(settings, state, Writer::StandIn).await
    }

    /// Ready to build, with the conductor's planner and the run's log on disk, as the app wires
    /// them. The transcript appears once a handoff's packet is built.
    async fn whole(settings: Settings) -> anyhow::Result<Self> {
        Self::with(settings, CardState::Ready, Writer::Conductor).await
    }

    async fn with(settings: Settings, state: CardState, writer: Writer) -> anyhow::Result<Self> {
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
        let tree = layout.worktrees.join(card().as_str());
        std::fs::create_dir_all(&layout.worktrees)?;
        let branch = format!("argonauts/{TEST_RUN}/{}", card());
        let tree_arg = tree.to_string_lossy().into_owned();
        repo.git(&[
            "worktree", "add", "--quiet", "-b", &branch, &tree_arg, &base,
        ])?;

        let paths = TranscriptPaths {
            worktree: tree.join(".argonauts/handoff/1/transcript.md"),
            run: layout
                .run_dir
                .join("cards")
                .join(card().as_str())
                .join("handoff-1/transcript.md"),
        };
        if writer == Writer::StandIn {
            for copy in [&paths.worktree, &paths.run] {
                std::fs::create_dir_all(copy.parent().context("a transcript dir")?)?;
                std::fs::write(copy, TRANSCRIPT)?;
            }
        }

        let history: Vec<RunEvent> = [
            (None, started(&repo, &base)),
            (None, goal_approved(&repo)),
            (None, workflow_started()?),
            (None, build_step()),
            (
                Some(card()),
                RunEventKind::Card(CardEvent::Created(Box::new(spec()))),
            ),
            (
                Some(card()),
                RunEventKind::Card(CardEvent::Worktree {
                    path: tree.clone(),
                    branch,
                    base: base.clone(),
                }),
            ),
            (
                Some(card()),
                RunEventKind::Card(CardEvent::State {
                    from: None,
                    to: state,
                    reason: "set up by the test".to_owned(),
                }),
            ),
        ]
        .into_iter()
        .zip(0..)
        .map(|((card, kind), seq)| event(seq, card, kind))
        .collect();

        let run_dir = layout.run_dir.clone();
        let (mut context, sink, commands) =
            engine_context_in(&services, settings, layout, history.clone());
        match writer {
            Writer::StandIn => {
                context.services.handoff = Arc::new(WithTranscript(paths.clone()));
            }
            Writer::Conductor => {
                let planner = Planner::new(
                    context.settings.clone(),
                    services.registry.clone(),
                    services.quota.clone(),
                    services.health.clone(),
                    services.track_records.clone(),
                )
                .with_run_dir(run_dir.clone());
                context.services.handoff = Arc::new(planner);
                context.sink = Arc::new(OnDisk::new(sink.clone(), &run_dir, &history)?);
            }
        }
        let shared = Shared::new(&context, RunState::fold(&history))?;
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

    fn add(&self, fake: FakeWorker) -> Arc<FakeWorker> {
        self.services.workers.add(fake)
    }

    /// Codex takes the card, as a handoff's rules give it.
    async fn hand_to_codex(&self) -> anyhow::Result<()> {
        let action = RecoveryAction::HandOff {
            to: worker(CODEX),
            packet: Box::new(packet(None)),
            checkpoint: None,
        };
        handoff::apply(&self.host, &card(), action).await?;
        Ok(())
    }

    /// What a fresh session of `to` is told after `cause`.
    async fn told(&self, to: &str, cause: AttemptCause) -> anyhow::Result<Option<InputPart>> {
        Ok(prompt::packet_part(&self.host, &card(), &worker(to), cause, "", None).await?)
    }

    fn handed_off_context(&self) -> Option<HandoffContext> {
        self.events().into_iter().find_map(|kind| match kind {
            RunEventKind::Handoff(HandoffEvent::HandedOff { context, .. }) => context,
            _ => None,
        })
    }

    fn packets(&self) -> usize {
        self.events()
            .into_iter()
            .filter(|kind| matches!(kind, RunEventKind::Handoff(HandoffEvent::Packet { .. })))
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

    fn state(&self) -> Option<CardState> {
        self.host
            .read(|state| state.cards.get(&card()).map(|record| record.state))
    }

    fn summary_file(&self) -> PathBuf {
        self.paths.run.with_file_name("summary.md")
    }

    /// The conductor's answer when claude-code stops: `to` takes the card.
    fn hand_off_at_the_stop(&self, to: &str) {
        self.services.limits.set_next(RecoveryAction::HandOff {
            to: worker(to),
            packet: Box::new(packet(None)),
            checkpoint: None,
        });
    }

    /// Builds the card from its step until it's in `state`.
    async fn run_until(&self, state: CardState) -> anyhow::Result<()> {
        let at = rules::step_now(&self.host)?;
        let c1 = card();
        let reached = async {
            while self.state() != Some(state) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let run = async {
            tokio::select! {
                ran = pipeline::run_card(&self.host, &c1, &at) => {
                    ran?;
                }
                () = reached => {}
            }
            anyhow::Ok(())
        };
        tokio::time::timeout(Duration::from_secs(60), run)
            .await
            .with_context(|| format!("{c1} never got to {state:?}"))?
    }

    /// The card's events as the run's log on disk has them, which is what the conductor reads.
    fn logged(&self) -> anyhow::Result<Vec<RunEvent>> {
        let file = self.host.repo().run_dir.join(run_files::EVENTS);
        Ok(read_all(&file)?
            .events
            .into_iter()
            .filter(|event| event.card_id == Some(card()))
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

/// The run's log on disk as well as in memory: the engine reads a card's events back from the
/// file for its handoff packet.
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
        card: Option<&CardId>,
        session: Option<&SessionKey>,
        source: &str,
        kind: RunEventKind,
    ) -> Result<RunEvent, JournalError> {
        let event = self.memory.record(card, session, source, kind)?;
        self.append(&event).map_err(|err| JournalError::Io {
            path: self.file.clone(),
            message: err.to_string(),
        })?;
        Ok(event)
    }
}

/// Builds the packet with the transcript's paths filled in, as the transcript writer does.
struct WithTranscript(TranscriptPaths);

impl HandoffPlanner for WithTranscript {
    fn candidates(&self, _query: &CandidateQuery) -> Vec<Candidate> {
        Vec::new()
    }

    fn packet(&self, _card: &CardSpec, _events: &[RunEvent]) -> HandoffPacket {
        packet(Some(self.0.clone()))
    }
}

fn packet(transcript: Option<TranscriptPaths>) -> HandoffPacket {
    HandoffPacket {
        card: spec(),
        goal_sha256: None,
        notes: Vec::new(),
        work: WorkSoFar {
            branch: format!("argonauts/{TEST_RUN}/{}", card()),
            commit: None,
            stat: DiffStat::default(),
        },
        happened: vec!["Edited src/store.rs (+14 −2)".to_owned()],
        last_todo: Vec::new(),
        gates: Vec::new(),
        why_stopped: "usage limit, resets at 3:45 PM".to_owned(),
        last_words: None,
        last_message: None,
        budget_left: None,
        transcript,
        summary: None,
    }
}

fn with_context(context: HandoffContext) -> Settings {
    let mut settings = Settings::default();
    settings.handoff.context = Some(context);
    settings
}

fn replace_context(context: HandoffContext) -> VendorReplacement {
    VendorReplacement {
        context: Some(context),
        ..VendorReplacement::default()
    }
}

fn claude() -> VendorId {
    worker(OPUS).vendor()
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
    let part = part.context("a handoff part")?;
    ensure!(part.kind == InputPartKind::Handoff, "{part:?}");
    Ok(part.text)
}

#[tokio::test]
async fn fresh_gives_the_card_only() -> anyhow::Result<()> {
    let bench = Bench::new(with_context(HandoffContext::Fresh)).await?;
    bench.add(FakeWorker::harness(CODEX, "gpt"));
    bench.hand_to_codex().await?;
    let packets = bench.packets();

    let told = bench.told(CODEX, AttemptCause::AfterFailure).await?;
    ensure!(told.is_none(), "fresh still told {told:?}");
    ensure!(
        bench.packets() == packets,
        "fresh saved a note it never sent"
    );
    ensure!(bench.handed_off_context() == Some(HandoffContext::Fresh));
    Ok(())
}

#[tokio::test]
async fn note_renders_the_packet_alone() -> anyhow::Result<()> {
    let bench = Bench::new(with_context(HandoffContext::Note)).await?;
    bench.add(FakeWorker::harness(CODEX, "gpt"));
    bench.hand_to_codex().await?;

    let text = note_text(bench.told(CODEX, AttemptCause::AfterFailure).await?)?;
    ensure!(text.starts_with("You're taking over this card from another session."));
    ensure!(text.contains("Edited src/store.rs (+14 −2)"), "{text}");
    ensure!(
        !text.contains("transcript"),
        "the note named the transcript: {text}"
    );
    ensure!(bench.handed_off_context() == Some(HandoffContext::Note));
    Ok(())
}

#[tokio::test]
async fn note_and_transcript_is_the_default() -> anyhow::Result<()> {
    let bench = Bench::new(Settings::default()).await?;
    bench.add(FakeWorker::harness(CODEX, "gpt"));
    bench.hand_to_codex().await?;

    let text = note_text(bench.told(CODEX, AttemptCause::AfterFailure).await?)?;
    ensure!(text.ends_with(&transcript_line(&bench)), "{text}");
    // The note names the copy in the worktree, which every harness can read.
    ensure!(!text.contains(&bench.paths.run.display().to_string()));
    ensure!(bench.handed_off_context() == Some(HandoffContext::NoteAndTranscript));
    Ok(())
}

#[tokio::test]
async fn note_and_summary_puts_the_cheapest_models_summary_in_the_note() -> anyhow::Result<()> {
    let bench = Bench::new(with_context(HandoffContext::NoteAndSummary)).await?;
    bench.add(FakeWorker::harness(CODEX, "gpt").with_cheapest_model("gpt-5.6-luna"));
    let luna = bench.add(FakeWorker::harness(LUNA, "gpt").then(summary(SUMMARY)));
    bench.hand_to_codex().await?;

    let text = note_text(bench.told(CODEX, AttemptCause::AfterFailure).await?)?;
    ensure!(text.contains(SUMMARY), "{text}");
    ensure!(!text.contains("transcript.md"), "{text}");

    let specs = luna.specs();
    ensure!(specs.len() == 1, "{} summary sessions", specs.len());
    let spec = &specs[0];
    ensure!(spec.access == Access::NoTools, "{:?}", spec.access);
    ensure!(spec.step == Some(StepName::HandoffSummary));
    ensure!(spec.card == Some(card()));
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

    // Another attempt of a race at the same handoff reads the saved summary.
    let again = note_text(bench.told(CODEX, AttemptCause::AfterFailure).await?)?;
    ensure!(again.contains(SUMMARY));
    ensure!(luna.specs().len() == 1, "the summary ran twice");
    ensure!(bench.warnings().is_empty(), "{:?}", bench.warnings());
    Ok(())
}

#[tokio::test]
async fn the_summarizer_setting_names_who_summarizes() -> anyhow::Result<()> {
    let mut settings = with_context(HandoffContext::NoteAndSummary);
    settings.handoff.summarizer = Some(worker(FLASH));
    let bench = Bench::new(settings).await?;
    bench.add(FakeWorker::harness(CODEX, "gpt").with_cheapest_model("gpt-5.6-luna"));
    let flash = bench.add(FakeWorker::harness(FLASH, "gemini").then(summary(SUMMARY)));
    bench.hand_to_codex().await?;

    let text = note_text(bench.told(CODEX, AttemptCause::AfterFailure).await?)?;
    ensure!(text.contains(SUMMARY), "{text}");
    ensure!(flash.specs().len() == 1);
    Ok(())
}

#[tokio::test]
async fn a_failed_summary_goes_on_as_note_and_transcript() -> anyhow::Result<()> {
    let bench = Bench::new(with_context(HandoffContext::NoteAndSummary)).await?;
    bench.add(FakeWorker::harness(CODEX, "gpt").with_cheapest_model("gpt-5.6-luna"));
    bench.add(
        FakeWorker::harness(LUNA, "gpt").then(FakeSession::ended(EndReason::Crashed {
            message: "segfault".to_owned(),
        })),
    );
    bench.hand_to_codex().await?;

    let text = note_text(bench.told(CODEX, AttemptCause::AfterFailure).await?)?;
    ensure!(text.ends_with(&transcript_line(&bench)), "{text}");
    ensure!(!text.contains(SUMMARY));
    let warnings = bench.warnings();
    ensure!(
        warnings.iter().any(|warning| warning.starts_with(
            "card-001's handoff goes on as note + transcript: the summary by \
             codex/gpt-5.6-luna failed"
        )),
        "{warnings:?}"
    );
    ensure!(!bench.summary_file().exists(), "a failed summary was saved");
    Ok(())
}

#[tokio::test]
async fn a_summarizer_out_of_quota_is_never_started() -> anyhow::Result<()> {
    let mut settings = with_context(HandoffContext::NoteAndSummary);
    settings.handoff.summarizer = Some(worker(FLASH));
    let bench = Bench::new(settings).await?;
    bench.add(FakeWorker::harness(CODEX, "gpt"));
    let flash = bench.add(FakeWorker::harness(FLASH, "gemini").then(summary(SUMMARY)));
    bench.services.quota.observe(QuotaReading {
        vendor: worker(FLASH).vendor(),
        status: QuotaStatus::Rejected,
        used: Some(1.0),
        resets_at: None,
        window: Some("five_hour".to_owned()),
        source: QuotaSource::RateLimitEvent,
        observed_at: bench.host.now(),
        detail: None,
        wrap_up: false,
    });
    bench.hand_to_codex().await?;

    let text = note_text(bench.told(CODEX, AttemptCause::AfterFailure).await?)?;
    ensure!(text.ends_with(&transcript_line(&bench)), "{text}");
    ensure!(flash.specs().is_empty(), "the summarizer started anyway");
    let expected = "card-001's handoff goes on as note + transcript: agy/gemini-3.6-flash is out \
                    of quota";
    ensure!(
        bench.warnings().iter().any(|warning| warning == expected),
        "{:?}",
        bench.warnings()
    );
    Ok(())
}

#[tokio::test]
async fn the_stopped_vendors_context_wins_over_the_global_one() -> anyhow::Result<()> {
    let mut settings = with_context(HandoffContext::Note);
    let fresh = replace_context(HandoffContext::Fresh);
    settings.handoff.replace.insert(claude(), fresh);
    let bench = Bench::new(settings).await?;
    bench.add(FakeWorker::harness(CODEX, "gpt"));

    // A race attempt starts while the card is still claude-code's, whose table applies.
    ensure!(bench.told(CODEX, AttemptCause::Handoff).await?.is_none());

    bench.hand_to_codex().await?;
    ensure!(bench.handed_off_context() == Some(HandoffContext::Fresh));
    ensure!(
        bench
            .told(CODEX, AttemptCause::AfterFailure)
            .await?
            .is_none()
    );
    // Codex's own session rotating has no table of its own, so the global one applies.
    let rotated = note_text(bench.told(CODEX, AttemptCause::Rotation).await?)?;
    ensure!(!rotated.contains("transcript"), "{rotated}");
    Ok(())
}

#[tokio::test]
async fn the_roles_context_wins_over_the_vendors() -> anyhow::Result<()> {
    let mut settings = Settings::default();
    let fresh = replace_context(HandoffContext::Fresh);
    settings.handoff.replace.insert(claude(), fresh);
    let role = HandoffSettings {
        context: Some(HandoffContext::Note),
        ..HandoffSettings::default()
    };
    settings.roles.insert(
        RoleName::Owner,
        RoleSettings {
            handoff: role,
            ..RoleSettings::default()
        },
    );
    let bench = Bench::new(settings).await?;
    bench.add(FakeWorker::harness(CODEX, "gpt"));
    bench.hand_to_codex().await?;

    let text = note_text(bench.told(CODEX, AttemptCause::AfterFailure).await?)?;
    ensure!(!text.contains("transcript"), "{text}");
    ensure!(bench.handed_off_context() == Some(HandoffContext::Note));
    Ok(())
}

#[tokio::test]
async fn the_roles_vendor_context_wins_over_the_role() -> anyhow::Result<()> {
    let mut settings = Settings::default();
    let mut role = HandoffSettings {
        context: Some(HandoffContext::Fresh),
        ..HandoffSettings::default()
    };
    let transcript = replace_context(HandoffContext::NoteAndTranscript);
    role.replace.insert(claude(), transcript);
    settings.roles.insert(
        RoleName::Owner,
        RoleSettings {
            handoff: role,
            ..RoleSettings::default()
        },
    );
    let bench = Bench::new(settings).await?;
    bench.add(FakeWorker::harness(CODEX, "gpt"));
    bench.hand_to_codex().await?;

    let text = note_text(bench.told(CODEX, AttemptCause::AfterFailure).await?)?;
    ensure!(text.ends_with(&transcript_line(&bench)), "{text}");
    Ok(())
}

#[tokio::test]
async fn a_hand_back_follows_the_replacements_vendor() -> anyhow::Result<()> {
    let mut settings = Settings::default();
    let note = replace_context(HandoffContext::Note);
    settings
        .handoff
        .replace
        .insert(worker(CODEX).vendor(), note);
    let bench = Bench::new(settings).await?;
    bench.add(FakeWorker::harness(CODEX, "gpt"));
    bench.hand_to_codex().await?;
    // Claude Code is back and takes its card again, as `on_return = "hand_back"` records it.
    let back = CardEvent::Reassigned {
        from: Some(worker(CODEX)),
        to: worker(OPUS),
        by: Party::Rules,
        rule_override: None,
    };
    bench
        .host
        .record(Some(&card()), None, RunEventKind::Card(back))?;

    let text = note_text(bench.told(OPUS, AttemptCause::Reassigned).await?)?;
    ensure!(!text.contains("transcript"), "{text}");
    Ok(())
}

#[tokio::test]
async fn a_full_context_rotates_with_the_transcript() -> anyhow::Result<()> {
    let bench = Bench::in_state(Settings::default(), CardState::Ready).await?;
    let opus = bench.add(
        FakeWorker::harness(OPUS, "claude")
            .then(FakeSession::ended(EndReason::ContextFull))
            .then(FakeSession::ended(EndReason::Crashed {
                message: "segfault".to_owned(),
            })),
    );
    bench.services.limits.set_next(RecoveryAction::Rotate {
        packet: Box::new(packet(Some(bench.paths.clone()))),
    });

    let at = rules::step_now(&bench.host)?;
    // The second session crashes and the card escalates, where it waits for you.
    let escalated = async {
        while bench.state() != Some(CardState::Escalated) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    let c1 = card();
    tokio::select! {
        ran = pipeline::run_card(&bench.host, &c1, &at) => {
            ran?;
        }
        () = escalated => {}
    }

    let specs = opus.specs();
    ensure!(specs.len() == 2, "{} sessions", specs.len());
    let first = &specs[0].prompt;
    ensure!(first.iter().all(|part| part.kind != InputPartKind::Handoff));
    let handoff = specs[1]
        .prompt
        .iter()
        .find(|part| part.kind == InputPartKind::Handoff)
        .context("the rotated session got no note")?;
    ensure!(
        handoff.text.contains("Edited src/store.rs"),
        "{}",
        handoff.text
    );
    ensure!(
        handoff.text.ends_with(&transcript_line(&bench)),
        "{}",
        handoff.text
    );
    Ok(())
}

#[tokio::test]
async fn the_note_names_the_transcript_the_conductor_wrote() -> anyhow::Result<()> {
    let bench = Bench::whole(Settings::default()).await?;
    bench.add(FakeWorker::harness(OPUS, "claude").then(stopped_on_a_usage_limit()));
    let codex = bench.add(FakeWorker::harness(CODEX, "gpt").then(crashed()));
    bench.hand_off_at_the_stop(CODEX);
    bench.run_until(CardState::Escalated).await?;

    let note = first_note(&codex)?;
    ensure!(note.ends_with(&transcript_line(&bench)), "{note}");
    // Written from the old session's events in the run's log.
    let written = std::fs::read_to_string(&bench.paths.worktree)?;
    let title = "# card-001 Redis store · claude-code/opus · why it stopped: usage limit\n";
    ensure!(written.starts_with(title), "{written}");
    for line in ["Read src/store.rs", STORE, "The store has no pool yet."] {
        ensure!(written.contains(line), "no {line:?} in\n{written}");
    }
    ensure!(std::fs::read_to_string(&bench.paths.run)? == written);
    // Never committed, though the card's work is.
    bench.ignored(&bench.tree, &bench.paths.worktree)?;
    let committed = bench.repo.git(&[
        "-C",
        &bench.tree.to_string_lossy(),
        "ls-tree",
        "-r",
        "--name-only",
        "HEAD",
    ])?;
    ensure!(!committed.contains(".argonauts/handoff"), "{committed}");
    Ok(())
}

#[tokio::test]
async fn the_summary_reads_the_transcript_the_conductor_wrote() -> anyhow::Result<()> {
    let bench = Bench::whole(with_context(HandoffContext::NoteAndSummary)).await?;
    bench.add(FakeWorker::harness(OPUS, "claude").then(stopped_on_a_usage_limit()));
    let codex = bench.add(
        FakeWorker::harness(CODEX, "gpt")
            .with_cheapest_model("gpt-5.6-luna")
            .then(crashed()),
    );
    let luna = bench.add(FakeWorker::harness(LUNA, "gpt").then(summary(SUMMARY)));
    bench.hand_off_at_the_stop(CODEX);
    bench.run_until(CardState::Escalated).await?;

    let written = std::fs::read_to_string(&bench.paths.run)?;
    ensure!(written.contains("Read src/store.rs"), "{written}");
    let asked = luna.specs();
    ensure!(asked.len() == 1, "{} summary sessions", asked.len());
    ensure!(
        asked[0]
            .prompt
            .iter()
            .any(|part| part.label == "transcript" && part.text.trim_end() == written.trim_end()),
        "the summarizer didn't read the transcript the conductor wrote"
    );
    let note = first_note(&codex)?;
    ensure!(note.contains(SUMMARY), "{note}");
    ensure!(!note.contains("transcript.md"), "{note}");
    for copy in [&bench.paths.run, &bench.paths.worktree] {
        let saved = std::fs::read_to_string(copy.with_file_name("summary.md"))?;
        ensure!(saved.trim() == SUMMARY, "{saved}");
    }
    ensure!(bench.warnings().is_empty(), "{:?}", bench.warnings());
    Ok(())
}

/// A race's attempts start at once. Each is told of the session that stopped, never of an
/// attempt that started before it, and reads its own copy of the transcript, since a session
/// reads no worktree but its own.
#[tokio::test]
async fn every_race_attempt_reads_the_stopped_sessions_transcript() -> anyhow::Result<()> {
    let bench = Bench::whole(Settings::default()).await?;
    bench.add(FakeWorker::harness(OPUS, "claude").then(stopped_on_a_usage_limit()));
    let racers = [
        bench.add(FakeWorker::harness(CODEX, "gpt").then(crashed())),
        bench.add(FakeWorker::harness(FLASH, "gemini").then(crashed())),
    ];
    bench.services.limits.set_next(RecoveryAction::Wait {
        vendor: claude(),
        until: None,
    });
    bench.run_until(CardState::NeedsDecision).await?;

    // The conductor builds the race's packet when claude-code stops.
    let packet = bench
        .host
        .services()
        .handoff
        .packet(&spec(), &bench.logged()?);
    let race = RecoveryAction::Race {
        to: vec![worker(CODEX), worker(FLASH)],
        packet: Box::new(packet),
        checkpoint: None,
    };
    handoff::apply(&bench.host, &card(), race).await?;

    let written = std::fs::read_to_string(&bench.paths.run)?;
    ensure!(written.contains("Read src/store.rs"), "{written}");
    for racer in &racers {
        let specs = racer.specs();
        let spec = specs.first().context("an attempt never started")?;
        let named = named_transcript(&first_note(racer)?)?;
        ensure!(
            named.starts_with(&spec.workdir),
            "{} was told to read {} outside its own tree",
            spec.worker,
            named.display()
        );
        ensure!(
            std::fs::read_to_string(&named)? == written,
            "{} was told of another session",
            spec.worker
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
        |session, event| session.with_event(WorkerEvent::Agent(event)),
    )
}

fn crashed() -> FakeSession {
    FakeSession::ended(EndReason::Crashed {
        message: "segfault".to_owned(),
    })
}

/// The note `fake`'s first session started with.
fn first_note(fake: &FakeWorker) -> anyhow::Result<String> {
    let specs = fake.specs();
    let spec = specs.first().context("no session started")?;
    let note = spec
        .prompt
        .iter()
        .find(|part| part.kind == InputPartKind::Handoff)
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

fn event(seq: u64, card: Option<CardId>, kind: RunEventKind) -> RunEvent {
    RunEvent {
        seq,
        ts: datetime!(2026-10-01 10:00 UTC),
        run_id: RunId::new(TEST_RUN).expect("the test run id"),
        card_id: card,
        session: None,
        source: "conductor".to_owned(),
        kind,
    }
}

fn started(repo: &TempRepo, base: &str) -> RunEventKind {
    RunEventKind::Run(RunLifecycle::Started(Box::new(RunStarted {
        request: "Add a Redis store".to_owned(),
        title: "Redis store".to_owned(),
        // Solo, so build sessions need only the approved goal, not a plan as well.
        mode: RunMode::Solo,
        forced: true,
        preset: "default".to_owned(),
        estimate: None,
        triage: None,
        chat_turn: None,
        continues: None,
        repo_root: repo.root().to_path_buf(),
        base_branch: Some("main".to_owned()),
        base_commit: base.to_owned(),
        uncommitted: Vec::new(),
        uncommitted_included: false,
        settings_sha256: String::new(),
        settings: serde_json::Value::Null,
        overrides: Vec::new(),
        harness_versions: Default::default(),
        version: "0.0.1".to_owned(),
        front_end: None,
        instructions: Vec::new(),
    })))
}

fn goal_approved(repo: &TempRepo) -> RunEventKind {
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

fn spec() -> CardSpec {
    CardSpec {
        id: card(),
        title: "Redis store".to_owned(),
        task: "Build the Redis store".to_owned(),
        kind: CardKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: Vec::new(),
        owner: Some(worker(OPUS)),
        reviewers: Vec::new(),
        budget: CardBudget::default(),
        predictions: None,
        item: None,
    }
}
