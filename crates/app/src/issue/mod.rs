//! `/issue`: work on a GitHub issue, from the list of open ones in the input's place to a draft
//! pull request that names it. `/issue 42` reads the issue, checks it can be worked on, has the
//! main agent read it back as a plan, and starts an ordinary crew run on that plan once you approve
//! it. Argonauts Code makes every GitHub call itself, so no agent holds your login.

pub(crate) mod follow;
mod fork;
mod gh_here;
mod list;
mod plan;
mod read;
mod start;
mod writes;

use argonauts_core::NoteLevel;
use argonauts_core::agents::cli_commands::{CliCommand, CommandKind, CommandSource};
use argonauts_core::app::AppEvent;
use argonauts_core::base::names::{ChoiceId, TurnId};
use argonauts_core::input::IssueRef;
use argonauts_core::runs::events::approval::AnswerAction;
use argonauts_core::workflows::steps::{StepError, StepOutcome};
use argonauts_crew::follow::IssueLink;

use crate::controller::conversation::{self, ConversationState, TurnUpdate};
use crate::controller::{Ctx, Effect};
use start::Prepared;

/// A run on an issue ends as the draft pull request that names it, shown to you before it's
/// opened, never as a merge into your branch. You start the run, so the safety key is yours.
const ENDS_AS_A_DRAFT: &str = "merge.mode=\"pull_request\"";

/// `/issue`'s state between reading an issue and starting its run.
#[derive(Debug, Default)]
pub(crate) struct IssueWork {
    pending: Option<Pending>,
    /// The pull requests runs on issues opened, as they're followed.
    follow: follow::Following,
    /// Writes on an issue a run started on, each waiting on its question.
    writes: Vec<AskedWrites>,
    /// A fork offered for a repository you can't push to.
    fork: Option<(ChoiceId, fork::Offer)>,
}

#[derive(Debug)]
struct AskedWrites {
    choice: ChoiceId,
    on: writes::OnIssue,
    /// The run's link, which "allow GitHub writes" changes for the rest of the run.
    link: IssueLink,
}

#[derive(Debug)]
struct Pending {
    prepared: Prepared,
    /// What follows `/issue` in the conversation, such as `42`.
    args: String,
    stage: Stage,
}

#[derive(Debug)]
enum Stage {
    /// The main agent reads the issue back, in the turn `turn` once its first event names it.
    ReadingBack { turn: Option<TurnId> },
    /// The plan waits on your answer to `choice`.
    Asked { choice: ChoiceId, plan: String },
}

/// `/issue`, with the issue it names, or none for the list.
pub(crate) async fn start(
    ctx: &mut Ctx,
    work: &mut IssueWork,
    conversation: &mut ConversationState,
    issue: Option<IssueRef>,
) -> anyhow::Result<Vec<Effect>> {
    let Some(issue) = issue else {
        return list(ctx);
    };
    let all_comments = issue.all_comments;
    start_on(ctx, work, conversation, issue, all_comments).await
}

/// Starts on one issue. `all_comments` reads everyone's comments, not only those of people
/// who may write to the repository and yours.
pub(crate) async fn start_on(
    ctx: &mut Ctx,
    work: &mut IssueWork,
    conversation: &mut ConversationState,
    issue: IssueRef,
    all_comments: bool,
) -> anyhow::Result<Vec<Effect>> {
    if let Some(pending) = &work.pending {
        ctx.notice(
            NoteLevel::Info,
            format!(
                "#{} still waits on its plan. Answer that first, or say No to it.",
                pending.prepared.read.number
            ),
        );
        return Ok(Vec::new());
    }
    if !conversation::settled(conversation) {
        ctx.notice(
            NoteLevel::Info,
            "The main agent is still answering. Start on the issue once it's done.",
        );
        return Ok(Vec::new());
    }
    let prepared = match gh_here::gh_for(&ctx.env.repo_root) {
        Ok(gh) => start::prepare(&gh, &issue, all_comments, &ctx.env.repo_root).await,
        Err(err) => Err(err.to_string().into()),
    };
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(refused) => {
            ctx.send(start::refused(issue.number, &refused.why));
            if let Some(offer) = refused.fork {
                let choice = ctx.next_choice_id();
                ctx.send(AppEvent::Choice {
                    choice: fork::question(choice.clone(), &offer),
                });
                work.fork = Some((choice, offer));
            }
            return Ok(Vec::new());
        }
    };
    let args = issue
        .url
        .clone()
        .unwrap_or_else(|| issue.number.to_string());
    let workflow = plan::workflow_named(ctx.settings());
    let prompt = plan::reading_back_prompt(&prepared.read, &workflow);
    let effects = conversation::reading_cli_command(ctx, conversation, &command(), &args, prompt)?;
    // After the command's own row, so the report sits between it and the main agent's answer.
    ctx.send(start::report(&prepared));
    work.pending = Some(Pending {
        prepared,
        args,
        stage: Stage::ReadingBack { turn: None },
    });
    Ok(effects)
}

/// `/issue` as the conversation shows it, typed by you.
fn command() -> CliCommand {
    CliCommand {
        name: "issue".to_owned(),
        aliases: Vec::new(),
        args: String::new(),
        help: String::new(),
        kind: CommandKind::Cli,
        source: CommandSource::BuiltIn,
    }
}

/// The open issues, answered as `Issues` once `gh` has listed them. The reading goes on beside
/// the controller, so a slow network holds nothing else up.
pub(crate) fn list(ctx: &Ctx) -> anyhow::Result<Vec<Effect>> {
    let out = ctx.out.clone();
    let gh = gh_here::gh_for(&ctx.env.repo_root);
    tokio::spawn(async move {
        let data = list::issues_view(gh).await;
        out.send(AppEvent::View {
            data: Box::new(data),
        });
    });
    Ok(Vec::new())
}

/// What the main agent's turn that reads the issue back came to, once it ended: its plan, or
/// why there is none.
pub(crate) type ReadBack = Result<String, String>;

/// Watches the conversation's updates for the main agent's turn that reads the issue back, and
/// gives what it
/// came to once it ends.
pub(crate) fn heard(work: &mut IssueWork, update: &TurnUpdate) -> Option<ReadBack> {
    let pending = work.pending.as_mut()?;
    let Stage::ReadingBack { turn } = &mut pending.stage else {
        return None;
    };
    match update {
        // Nothing else could start a turn while this one reads the issue back, so the first one
        // heard is it.
        TurnUpdate::Event { turn: heard, .. } => {
            turn.get_or_insert_with(|| heard.clone());
            None
        }
        TurnUpdate::Done {
            turn: done,
            outcome,
        } if turn.as_ref().is_none_or(|turn| turn == done) => Some(plan_of(outcome)),
        _ => None,
    }
}

fn plan_of(outcome: &Result<StepOutcome, StepError>) -> ReadBack {
    let outcome = outcome
        .as_ref()
        .map_err(|err| format!("the main agent gave no plan: {err}"))?;
    // A command's reply is the CLI's own text, so there is no value to read.
    [&outcome.end.result_text, &outcome.end.last_message]
        .into_iter()
        .flatten()
        .map(|text| text.trim())
        .find(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "the main agent gave no plan".to_owned())
}

/// The main agent's turn ended: its plan waits on your answer, or nothing starts.
pub(crate) fn read_back(ctx: &mut Ctx, work: &mut IssueWork, read_back: ReadBack) {
    let Some(pending) = work.pending.as_mut() else {
        return;
    };
    match read_back {
        Ok(plan) => {
            let choice = ctx.next_choice_id();
            ctx.send(AppEvent::Choice {
                choice: plan::question(choice.clone()),
            });
            pending.stage = Stage::Asked { choice, plan };
        }
        Err(why) => {
            ctx.send(start::refused(pending.prepared.read.number, &why));
            work.pending = None;
        }
    }
}

pub(crate) fn owns_choice(work: &IssueWork, id: &ChoiceId) -> bool {
    follow::owns_choice(&work.follow, id)
        || work.writes.iter().any(|asked| &asked.choice == id)
        || work.fork.as_ref().is_some_and(|(choice, _)| choice == id)
        || asks_the_plan(work, id)
}

fn asks_the_plan(work: &IssueWork, id: &ChoiceId) -> bool {
    matches!(
        work.pending.as_ref().map(|pending| &pending.stage),
        Some(Stage::Asked { choice, .. }) if choice == id
    )
}

/// Looks at the pull requests being followed, once it's time.
pub(crate) fn follow_up(ctx: &Ctx, work: &mut IssueWork) {
    follow::look(ctx, &mut work.follow);
}

/// What GitHub said of them came back.
pub(crate) fn followed(ctx: &mut Ctx, work: &mut IssueWork, looked: follow::Looked) {
    follow::heard(ctx, &mut work.follow, looked);
}

/// Your answer to the plan: the run starts, the main agent writes the plan again with your
/// change, or nothing starts.
pub(crate) fn answer_choice(
    ctx: &mut Ctx,
    work: &mut IssueWork,
    conversation: &mut ConversationState,
    id: ChoiceId,
    action: &AnswerAction,
    text: Option<String>,
) -> anyhow::Result<Vec<Effect>> {
    if follow::owns_choice(&work.follow, &id) {
        follow::answer_choice(ctx, &mut work.follow, &id, action);
        return Ok(Vec::new());
    }
    if let Some((_, offer)) = work.fork.take_if(|(choice, _)| *choice == id) {
        fork::answer(ctx, id, offer, action);
        return Ok(Vec::new());
    }
    if let Some(at) = work.writes.iter().position(|asked| asked.choice == id) {
        let asked = work.writes.remove(at);
        answer_writes(ctx, asked, action);
        return Ok(Vec::new());
    }
    let Some(pending) = work.pending.take() else {
        return Ok(Vec::new());
    };
    let Stage::Asked { plan, .. } = &pending.stage else {
        work.pending = Some(pending);
        return Ok(Vec::new());
    };
    let number = pending.prepared.read.number;
    let value = match action {
        AnswerAction::Choose { value } => value.as_str(),
        _ => plan::STOP,
    };
    let closed = |ctx: &Ctx, outcome: String| {
        ctx.send(AppEvent::ChoiceClosed {
            id: id.clone(),
            outcome,
        });
    };
    match value {
        plan::START => {
            let request = plan::request(plan, &pending.prepared.read);
            let link = link(&pending.prepared, &request);
            if let Err(err) = link.save(&ctx.env.state_dir) {
                closed(ctx, format!("Nothing started on #{number}."));
                ctx.notice(
                    NoteLevel::Error,
                    format!("Couldn't keep what the run on #{number} works on: {err}"),
                );
                return Ok(Vec::new());
            }
            closed(ctx, format!("Starting the crew on #{number}."));
            let on = writes::at_start(&ctx.settings().issue, &link);
            if !on.writes.is_empty() {
                let choice = ctx.next_choice_id();
                ctx.send(AppEvent::Choice {
                    choice: writes::question(choice.clone(), &on),
                });
                work.writes.push(AskedWrites { choice, on, link });
            }
            let mut overrides = ctx.settings.run_overrides();
            overrides.push(ENDS_AS_A_DRAFT.to_owned());
            let run = plan::run(ctx.settings(), overrides, request);
            Ok(vec![Effect::StartRun(Box::new(run))])
        }
        plan::CHANGE => match text.filter(|change| !change.trim().is_empty()) {
            Some(change) => {
                closed(ctx, "The main agent writes the plan again.".to_owned());
                let prompt = plan::change_prompt(&change);
                let effects = conversation::cli_command(
                    ctx,
                    conversation,
                    &command(),
                    &pending.args,
                    prompt,
                )?;
                work.pending = Some(Pending {
                    stage: Stage::ReadingBack { turn: None },
                    ..pending
                });
                Ok(effects)
            }
            // The change is the note on that answer, so the question asks again.
            None => {
                closed(ctx, String::new());
                ctx.hint("Press Tab on Change the plan to say what to change.");
                let choice = ctx.next_choice_id();
                ctx.send(AppEvent::Choice {
                    choice: plan::question(choice.clone()),
                });
                let plan = plan.clone();
                work.pending = Some(Pending {
                    stage: Stage::Asked { choice, plan },
                    ..pending
                });
                Ok(Vec::new())
            }
        },
        _ => {
            closed(
                ctx,
                format!("Nothing started, and nothing was posted on #{number}."),
            );
            Ok(Vec::new())
        }
    }
}

/// Writes on the issue as you said: all of them, all and the rest of the run's, or none.
fn answer_writes(ctx: &mut Ctx, asked: AskedWrites, action: &AnswerAction) {
    let AskedWrites {
        choice,
        on,
        mut link,
    } = asked;
    let value = match action {
        AnswerAction::Choose { value } => value.as_str(),
        _ => writes::SKIP,
    };
    let number = on.number;
    let outcome = match value {
        writes::POST | writes::ALLOW => {
            if value == writes::ALLOW {
                link.writes_allowed = true;
                if let Err(err) = link.save(&ctx.env.state_dir) {
                    ctx.notice(
                        NoteLevel::Error,
                        format!(
                            "Couldn't keep that GitHub writes are allowed for #{number}: {err}"
                        ),
                    );
                }
            }
            writes::post(ctx, on);
            format!("Writing on #{number}.")
        }
        _ => format!("Nothing was written on #{number}."),
    };
    ctx.send(AppEvent::ChoiceClosed {
        id: choice,
        outcome,
    });
}

/// Esc leaves the plan, or a pull request's question, waiting, as a question that needs you
/// does.
pub(crate) fn set_aside(work: &IssueWork, id: &ChoiceId) -> bool {
    owns_choice(work, id)
}

fn link(prepared: &Prepared, request: &str) -> IssueLink {
    IssueLink {
        repo: prepared.repo.name.clone(),
        number: prepared.read.number,
        title: prepared.read.title.clone(),
        url: prepared.read.url.clone(),
        branch: prepared.branch.clone(),
        default_branch: prepared.repo.default_branch.clone(),
        login: prepared.login.clone(),
        request: request.to_owned(),
        writes_allowed: false,
        as_read: Some(prepared.as_read.clone()),
        push_remote: prepared.fork_remote.clone(),
        fork_account: prepared
            .fork_remote
            .as_ref()
            .map(|_| prepared.login.clone()),
    }
}

#[cfg(test)]
mod tests;
