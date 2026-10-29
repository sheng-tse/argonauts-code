//! The main agent alone, kept to a goal. With no workflow there is no run to keep it, so the
//! conversation does: after each turn the main agent works in your checkout, Argonauts Code runs
//! the goal's check there and, while a line doesn't hold, sends back what still fails as the
//! same session's next turn. There's no fixed number of tries; stuck, it changes approach
//! once, then stops and says why.
//!
//! On Codex the goal is Codex's own where the live check allows it (`crew::goal::cli`): the
//! turn is told to set the lines as its goal, a turn that ends with the goal still active is
//! carried on, and Argonauts Code still runs the check. On Claude Code and agy
//! Argonauts Code keeps it. A notice says which of the two it is.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use argonauts_core::events::goal::{CriterionState, GoalChecked};
use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::events::task::CheckOutcome;
use argonauts_core::goal::keep::{self, Limits, Next};
use argonauts_core::goal::{GoalSpec, KeptBy, RoleGoal};
use argonauts_core::limits::{CheckRequest, CheckRunner};
use argonauts_core::names::{ConversationId, ModelRef, RoleName};
use argonauts_core::paths::create_private_dir_all;
use argonauts_core::session_runner::{Access, SessionRunner, SessionSpec};
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::CheckCommand;
use argonauts_core::steps::pursue::{CheckGoal, CheckGoalInput, GoalCheck};
use argonauts_core::steps::{StepName, StepRequest, StepRunner, output_schema};
use argonauts_core::{NoteLevel, RunId, TaskId};
use argonauts_crew::error::{EngineError, EngineResult};
use argonauts_crew::goal::check::{self, Means, Place};
use argonauts_crew::goal::cli::{self, Says};
use argonauts_crew::prompts::goal::CHECK;

use super::super::{ConversationState, Turn, direct, side};
use crate::controller::turn::{self, DirectWords, Launch, TurnStep};
use crate::controller::{Ctx, Effect, Internal};
use crate::telemetry::{self, Use};

/// What the judge reads of the changes in your checkout.
const DIFF_BYTES: usize = 60_000;
/// Used when `checks.timeout` doesn't parse or says `none`.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The main agent's work toward a goal, turn by turn.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::controller::conversation) struct Keeping {
    spec: GoalSpec,
    by: KeptBy,
    /// Your request, which every further try is filed under for the record and `/rewind`.
    request: String,
    /// One check for each try so far.
    checks: Vec<GoalChecked>,
    approach_changed: bool,
    /// Turns in a row the CLI ended with its own goal still active.
    continues: u32,
    /// The session was told of the goal.
    told: bool,
    checking: bool,
}

/// A check that came back, for the conversation it was run for.
#[derive(Debug)]
pub(crate) struct Checked {
    conversation: ConversationId,
    result: Result<GoalChecked, String>,
}

/// The goal a turn in your checkout works toward: this request's own, from its words or from
/// the main session's tool, else the conversation's. None when `[roles.main] goal` is off.
fn applies(
    ctx: &Ctx,
    conversation: &ConversationState,
    words: Option<&DirectWords>,
) -> Option<GoalSpec> {
    let settings = ctx.settings();
    if main_role_goal(settings) == RoleGoal::Off {
        return None;
    }
    let own = words
        .and_then(|words| words.goal.as_deref())
        .or(conversation.goal.for_request.as_deref());
    match own {
        Some(condition) => super::spec(settings, condition).ok(),
        None => super::of(settings),
    }
}

/// `[roles.main] goal`. Unset, the CLI's own goal is used where it has one.
fn main_role_goal(settings: &Settings) -> RoleGoal {
    settings
        .roles
        .get(&RoleName::Main)
        .and_then(|role| role.goal)
        .unwrap_or(RoleGoal::Cli)
}

fn keeper(settings: &Settings, worker: &ModelRef) -> KeptBy {
    if main_role_goal(settings) == RoleGoal::Cli && cli::has_own_goal(worker) {
        KeptBy::Cli
    } else {
        KeptBy::Argonauts
    }
}

/// `kept by Codex's own goal · checked by Argonauts Code`, or `kept by Argonauts Code`.
fn kept_words(by: KeptBy, worker: &ModelRef) -> String {
    match by {
        KeptBy::Cli => format!(
            "kept by {}'s own goal · checked by Argonauts Code",
            cli::name(worker)
        ),
        KeptBy::Argonauts => "kept by Argonauts Code".to_owned(),
    }
}

/// The keeping for `spec`, begun now unless this goal is being kept already.
fn keeping<'a>(
    ctx: &Ctx,
    conversation: &'a mut ConversationState,
    spec: GoalSpec,
    by: KeptBy,
    request: &str,
) -> &'a mut Keeping {
    let same = conversation
        .goal
        .keeping
        .as_ref()
        .is_some_and(|kept| kept.spec == spec && kept.by == by);
    if !same {
        telemetry::used(&ctx.env.paths, Use::Goal);
        conversation.goal.keeping = Some(Keeping {
            spec,
            by,
            request: request.to_owned(),
            checks: Vec::new(),
            approach_changed: false,
            continues: 0,
            told: false,
            checking: false,
        });
    }
    conversation
        .goal
        .keeping
        .as_mut()
        .expect("the keeping was just set")
}

/// What a turn in your checkout is told about the goal as it starts, once: its lines, and who
/// keeps it. Argonauts Code adds nothing else of its own to such a turn.
pub(in crate::controller::conversation) fn brief(
    ctx: &Ctx,
    conversation: &mut ConversationState,
    step: &TurnStep,
    worker: &ModelRef,
    request: &str,
) -> Option<InputPart> {
    let TurnStep::Direct { words, .. } = step else {
        return None;
    };
    let spec = applies(ctx, conversation, Some(words))?;
    let by = keeper(ctx.settings(), worker);
    let kept = keeping(ctx, conversation, spec, by, request);
    if kept.told {
        return None;
    }
    kept.told = true;
    let text = match kept.by {
        KeptBy::Cli => cli::brief(&kept.spec.lines, Says::InAnswer),
        KeptBy::Argonauts => {
            let mut text = String::from(
                "This request is worked as a goal. It is done when all of these hold, and not \
                 before:\n",
            );
            for line in &kept.spec.lines {
                text.push_str(&format!(
                    "- {} (checked by {})\n",
                    line.text,
                    kept.spec.line_checked_by(line)
                ));
            }
            text.push_str(
                "\nArgonauts Code runs the check itself after you finish, and sends back what still \
                 fails.",
            );
            text
        }
    };
    ctx.notice(
        NoteLevel::Info,
        format!("Goal · {}", kept_words(kept.by, worker)),
    );
    Some(turn::part(InputPartKind::Goal, "goal", text))
}

/// After the answer of a turn that worked in your checkout: the goal's check starts, or, where
/// the CLI keeps the goal and says it's still at it, the same session carries on.
pub(in crate::controller::conversation) fn after_turn(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    turn: &Turn,
    answer: &str,
) -> Vec<Effect> {
    let TurnStep::Direct { words, .. } = &turn.step else {
        return Vec::new();
    };
    // A run this turn started does the work, and carries the goal itself.
    if turn.run_began {
        return Vec::new();
    }
    let Some(spec) = applies(ctx, conversation, Some(words)) else {
        conversation.goal.keeping = None;
        return Vec::new();
    };
    let by = keeper(ctx.settings(), &turn.worker);
    let kept = keeping(ctx, conversation, spec, by, &turn.text);
    // A goal the session's tool set mid-turn is in what the tool answered.
    kept.told = true;
    if kept.by == KeptBy::Cli
        && let Some(said) = cli::said(&turn.worker, answer)
    {
        let name = cli::name(&turn.worker);
        let still_going = cli::still_going(&said) && kept.continues < cli::MAX_CONTINUES;
        let note = match said.note.as_str() {
            "" => String::new(),
            note => format!(" · \"{note}\""),
        };
        ctx.notice(
            NoteLevel::Info,
            format!("{name} says · goal {}{note}", said.status),
        );
        if still_going {
            kept.continues += 1;
            let request = kept.request.clone();
            return go_on(ctx, conversation, request, &cli::carry_on(Says::InAnswer));
        }
    }
    kept.continues = 0;
    start_check(ctx, conversation, &turn.worker);
    Vec::new()
}

/// The same session's next turn, reading only `words`. Your request stays the turn's request.
fn go_on(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    request: String,
    words: &str,
) -> Vec<Effect> {
    match direct::go_ahead(
        ctx,
        conversation,
        request,
        words,
        None,
        DirectWords::default(),
    ) {
        Ok(effects) => effects,
        Err(err) => {
            ctx.error(&err);
            Vec::new()
        }
    }
}

/// Runs the check off the loop, and sends what it found back as [`Checked`].
fn start_check(ctx: &mut Ctx, conversation: &mut ConversationState, worker: &ModelRef) {
    let Some(kept) = conversation.goal.keeping.as_mut() else {
        return;
    };
    kept.checking = true;
    let spec = kept.spec.clone();
    let try_number = u32::try_from(kept.checks.len()).unwrap_or(u32::MAX) + 1;
    let folder = conversation.folder(ctx).to_path_buf();
    let judge = spec
        .asks_the_judge()
        .then(|| judge(ctx, conversation, &folder))
        .flatten();
    let means = InConversation {
        settings: ctx.settings.shared(),
        checks: argonauts_conductor::check_runner(ctx.settings.shared()),
        steps: Arc::clone(&ctx.services.steps),
        temp: ctx
            .env
            .conversation_dir()
            .join(conversation.id.as_str())
            .join("goal-tmp"),
        folder: folder.clone(),
        judge,
    };
    let mut place = Place::new(folder, try_number);
    place.authors = vec![worker.clone()];
    ctx.notice(
        NoteLevel::Info,
        format!("Checking the goal · {}", spec.checked_by()),
    );
    let done = ctx.internal.clone();
    let conversation = conversation.id.clone();
    tokio::spawn(async move {
        let result = check::with(&means, &spec, &place)
            .await
            .map_err(|err| err.to_string());
        // After Exit nobody listens, and the check has nowhere to go.
        let _ = done.send(Internal::Goal(Checked {
            conversation,
            result,
        }));
    });
}

/// A check came back: the goal holds, or what still fails goes back as the next try, or it
/// stops and says why.
pub(crate) fn on_checked(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    checked: Checked,
) -> Vec<Effect> {
    if conversation.id != checked.conversation {
        return Vec::new();
    }
    // Paused or cleared while the check ran: nothing is sent round.
    if applies(ctx, conversation, None).is_none() {
        conversation.goal.keeping = None;
        return Vec::new();
    }
    let worker = ctx.settings().crew.main.clone();
    let stuck_after = ctx.settings().goal.stuck_after;
    let max_rounds = ctx.settings().goal.max_rounds;
    let Some(kept) = conversation.goal.keeping.as_mut() else {
        return Vec::new();
    };
    kept.checking = false;
    let found = match checked.result {
        Ok(found) => found,
        Err(why) => {
            ctx.notice(
                NoteLevel::Warn,
                format!("The goal's check couldn't run: {why}. /goal shows the goal."),
            );
            return Vec::new();
        }
    };
    ctx.notice(NoteLevel::Info, lines_words(&kept.spec, &found));
    kept.checks.push(found);
    let refs: Vec<&GoalChecked> = kept.checks.iter().collect();
    let limits = Limits {
        stuck_after,
        max_rounds: kept.spec.max_rounds.or(max_rounds),
    };
    let tries = kept.checks.len();
    let kept_by = kept_words(kept.by, &worker);
    let change = match keep::next(&refs, limits, kept.approach_changed) {
        Next::Met => {
            let tries = match tries {
                1 => "1 try".to_owned(),
                n => format!("{n} tries"),
            };
            ctx.notice(NoteLevel::Info, format!("Goal met · {tries} · {kept_by}"));
            end(conversation);
            return Vec::new();
        }
        Next::Stop { why } => {
            let (met, total) = met_of(refs[refs.len() - 1]);
            ctx.notice(
                NoteLevel::Warn,
                format!(
                    "The goal stopped: {why}. {met} of {total} {} hold. /goal shows it, and \
                     your next message takes it up again.",
                    if total == 1 { "line" } else { "lines" }
                ),
            );
            end(conversation);
            return Vec::new();
        }
        Next::Again => None,
        Next::ChangeApproach { why } => {
            kept.approach_changed = true;
            Some(why)
        }
    };
    // Your own message comes first: its turn's end checks the goal again.
    if conversation.turn.is_some() || !conversation.queued.is_empty() {
        return Vec::new();
    }
    let last = &kept.checks[kept.checks.len() - 1];
    let mut words = keep::send_back(&kept.spec.lines, last, change.as_deref());
    // A conversation turn ends with its answer, where a task ends with a report.
    words = words.replace(
        "then end with your report again.",
        "then say what you changed.",
    );
    if kept.by == KeptBy::Cli {
        words.push_str("\n\n");
        words.push_str(&cli::not_achieved(Says::InAnswer));
    }
    let request = kept.request.clone();
    ctx.notice(
        NoteLevel::Info,
        format!("Goal · try {} · sent back what still fails", tries + 1),
    );
    go_on(ctx, conversation, request, &words)
}

/// The keeping is over, and so is a goal that covered this request alone.
fn end(conversation: &mut ConversationState) {
    conversation.goal.keeping = None;
    conversation.goal.for_request = None;
}

fn met_of(checked: &GoalChecked) -> (usize, usize) {
    let met = checked
        .verdicts
        .iter()
        .filter(|verdict| verdict.verdict == CriterionState::Met)
        .count();
    (met, checked.verdicts.len())
}

/// `✓ the CSV has a header row   ✗ the export tests pass · 3 still fail`.
fn lines_words(spec: &GoalSpec, checked: &GoalChecked) -> String {
    let lines: Vec<String> = spec
        .lines
        .iter()
        .map(|line| {
            let verdict = checked
                .verdicts
                .iter()
                .find(|verdict| verdict.id == line.id);
            match verdict {
                Some(verdict) if verdict.verdict == CriterionState::Met => {
                    format!("✓ {}", line.text)
                }
                Some(verdict) if !verdict.missing.trim().is_empty() => {
                    format!("✗ {} · {}", line.text, verdict.missing.trim())
                }
                _ => format!("✗ {}", line.text),
            }
        })
        .collect();
    lines.join("   ")
}

/// The judge's session, ready to start: an agent from another provider than the main agent's
/// where one is ready, as `/review` picks its reviewer, read-only in the folder.
fn judge(ctx: &Ctx, conversation: &mut ConversationState, folder: &Path) -> Option<Judge> {
    let (worker, handle) = side::reviewer(ctx, &[]).ok()?;
    conversation.sides += 1;
    let turn = side::side_turn_id(conversation.sides);
    let key_text = format!("{}-{turn}", conversation.id);
    let key = argonauts_core::names::SessionKey::parse(&key_text)?;
    let session_dir = ctx
        .env
        .conversation_dir()
        .join(conversation.id.as_str())
        .join(turn.as_str());
    create_private_dir_all(&session_dir).ok()?;
    // A review's session is what a judge needs: its own, read-only, in the folder.
    let step = TurnStep::Review {
        security: false,
        target: String::new(),
        changes: String::new(),
        other_provider: true,
    };
    let launch = Launch {
        conversation: &conversation.id,
        // A reviewer's session isn't the one named for the conversation.
        title: "",
        turn: &turn,
        step: &step,
        text: "",
        pictures: Vec::new(),
        worker: worker.clone(),
        handle: Arc::clone(&handle),
        resume: None,
        fork: false,
        excerpt: None,
        notes: Vec::new(),
        shell: None,
        fast: false,
        workdir: folder.to_path_buf(),
        outside: Default::default(),
    };
    let mut spec = turn::session_spec(ctx, &launch, key, session_dir);
    spec.role = RoleName::Reviewer;
    spec.step = Some(StepName::CheckGoal);
    spec.json_schema = Some(output_schema::<CheckGoal>());
    spec.goal_sha256 = None;
    Some(Judge {
        worker,
        handle,
        spec,
    })
}

struct Judge {
    worker: ModelRef,
    handle: Arc<dyn SessionRunner>,
    spec: SessionSpec,
}

/// A check in your checkout: the commands run in the sandbox a build would have there, and
/// the judge reads the folder.
struct InConversation {
    settings: Arc<Settings>,
    checks: Arc<dyn CheckRunner>,
    steps: Arc<dyn StepRunner>,
    folder: PathBuf,
    temp: PathBuf,
    judge: Option<Judge>,
}

impl Means for InConversation {
    fn project_commands(&self) -> Vec<CheckCommand> {
        self.settings.checks.run.clone()
    }

    async fn run(&self, command: &CheckCommand) -> EngineResult<CheckOutcome> {
        std::fs::create_dir_all(&self.temp)?;
        let refused = |what: String| EngineError::Refused(what);
        let request = CheckRequest {
            run: RunId::new("run-conversation").map_err(|err| refused(err.to_string()))?,
            task: TaskId::new("goal").map_err(|err| refused(err.to_string()))?,
            name: command.name.clone(),
            command: command.command.clone(),
            workdir: self.folder.clone(),
            sandbox: argonauts_crew::spec::sandbox(
                &self.settings,
                Access::Build,
                &self.folder,
                &self.temp,
            )?,
            timeout: self
                .settings
                .checks
                .timeout
                .parse()
                .ok()
                .flatten()
                .unwrap_or(DEFAULT_TIMEOUT),
            setup: false,
        };
        Ok(self.checks.run(request).await)
    }

    /// What your checkout holds that its last commit doesn't.
    async fn diff(&self) -> String {
        let output = tokio::process::Command::new("git")
            .arg("-C")
            .arg(&self.folder)
            .args(["diff", "HEAD"])
            .output()
            .await;
        let Ok(output) = output else {
            return String::new();
        };
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        if text.len() > DIFF_BYTES {
            let mut cut = DIFF_BYTES;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            text.push_str("\n[cut: the rest is in the folder]\n");
        }
        text
    }

    async fn judge(&self, input: CheckGoalInput) -> EngineResult<(GoalCheck, ModelRef)> {
        let Some(judge) = &self.judge else {
            return Err(EngineError::Refused(
                "no agent is ready to judge it".to_owned(),
            ));
        };
        let mut spec = judge.spec.clone();
        spec.prompt = vec![
            turn::part(InputPartKind::Prompt, "judge", CHECK.to_owned()),
            turn::part(
                InputPartKind::StepInput,
                "input",
                serde_json::to_string_pretty(&input)?,
            ),
        ];
        let request = StepRequest::new(Arc::clone(&judge.handle), spec);
        let outcome = self
            .steps
            .run(request, Box::new(|_| {}))
            .await
            .map_err(|err| EngineError::Refused(turn::describe(&err)))?;
        Ok((serde_json::from_value(outcome.value)?, judge.worker.clone()))
    }
}
