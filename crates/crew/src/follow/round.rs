//! One round of a followed pull request's follow-up, worked by the engine of the run that
//! opened it once the app has seen there is something to do: the task's builder is resumed in
//! its own session with what to fix or answer, the task's reviewer looks at the fix, and the
//! fix is pushed to the pull request's branch. The branch and its pull request are the run's,
//! so the push goes ahead on its own. Replies post as you, so the round only keeps them, for the
//! app to show you first.

use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use argonauts_core::agents::session_runner::Access;
use argonauts_core::base::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::run::RunOutcome;
use argonauts_core::runs::events::session::{EndReason, InputPartKind, SessionEnd};
use argonauts_core::workflows::steps::judgment::{Review, ReviewOutput, ReviewVerdict};
use argonauts_core::workflows::steps::last_json_object;
use argonauts_core::workflows::steps::output_schema;
use argonauts_core::{Note, NoteLevel, RunId, TaskId};

use super::followed::{Due, Followed, Reply, Work};
use super::watch::{self, ToAnswer};
use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::{Fallbacks, Host, Ran, SessionPlan};
use crate::review::diff_cut;
use crate::state::RunState;
use crate::task::support::part;
use crate::task::worktree;

/// The pull request of run `run` with a round to work, if there is one.
pub fn due(state_dir: &Path, run: &RunId) -> Option<Followed> {
    Followed::of_run(state_dir, run).filter(|followed| followed.due.is_some())
}

/// Works the round `followed` is due, then keeps where it stands. The run ends as it ended
/// before, since the round is part of its pull request's life, not of its own work.
pub async fn run(host: Host, mut followed: Followed) -> EngineResult<RunOutcome> {
    let outcome = host
        .read(|state| state.run.finished)
        .unwrap_or(RunOutcome::PullRequest);
    let Some(mut due) = followed.due.take() else {
        return Ok(outcome);
    };
    let worked = work(&host, &followed, &mut due).await;
    match worked {
        Ok(Ok(head)) => {
            if let Some(head) = head {
                followed.head = Some(head);
            }
            if let Work::Comments { comments } = &due.work {
                followed
                    .answered
                    .extend(comments.iter().map(|comment| comment.id.clone()));
            }
            followed.replies.append(&mut due.replies);
            followed.rounds += 1;
        }
        Ok(Err(why)) => {
            note(
                &host,
                NoteLevel::Warn,
                format!("#{}: {why}", followed.number),
            )?;
            followed.held = Some(why);
            followed.rounds += 1;
        }
        // A pause or the app closing cut the round short, so the next start carries it on
        // from where it stood.
        Err(EngineError::Interrupted) => {
            let stopped = host.read(|state| state.run.stop_requested);
            if stopped {
                followed.ended = true;
            } else {
                followed.due = Some(due);
            }
            keep(&host, &followed);
            return Err(EngineError::Interrupted);
        }
        Err(err) => {
            followed.held = Some(format!("the round failed: {err}"));
            keep(&host, &followed);
            return Err(err);
        }
    }
    keep(&host, &followed);
    Ok(outcome)
}

/// The round itself: the commit pushed, if the fix changed anything, or why it waits for you.
async fn work(
    host: &Host,
    followed: &Followed,
    due: &mut Due,
) -> EngineResult<Result<Option<String>, String>> {
    let round = followed.rounds + 1;
    let what = due.work.what();
    let Some(task) = host.read(|state| task_of(state, &due.work)) else {
        return Ok(Err(
            "the run has no task whose builder could take it".to_owned()
        ));
    };
    let Some(builder) = host.read(|state| builder_of(state, &task)) else {
        return Ok(Err("its builder's session can't be carried on".to_owned()));
    };
    let Some(head) = head_of(host, followed).await else {
        return Ok(Err(
            "the commit the pull request was opened from isn't here".to_owned(),
        ));
    };
    let tree = worktree::ensure(host, &task).await?;
    let git = worktree::tree_git(host, &tree.path).await?;
    let fixed = match due.fixed.clone() {
        Some(fixed) => fixed,
        None => {
            note(host, NoteLevel::Info, starting(followed, &due.work, round))?;
            if git.head_commit(&tree.path).await?.as_deref() != Some(head.as_str()) {
                git.reset_hard(&tree.path, &head).await?;
            }
            let plan = SessionPlan {
                without_status: false,
                at: None,
                role: RoleName::Worker,
                worker: builder.1.clone(),
                task: Some(task.clone()),
                access: Access::Build,
                // No step of the workflow's: the session carries on the builder's conversation
                // under its own name, as a row of the Pull request phase.
                step: None,
                workdir: tree.path.clone(),
                parts: vec![part(
                    InputPartKind::Instructions,
                    "the pull request",
                    prompt(followed.number, &due.work),
                    None,
                )],
                json_schema: matches!(due.work, Work::Comments { .. }).then(answers_schema),
                resume: Some(builder.0.clone()),
                slot: 0,
                label: Some(format!("fix:{what}-round-{round}")),
                reviews: false,
                fresh: false,
                independent_of: Vec::new(),
                fork_of: None,
                in_place_of: None,
            };
            let plan = SessionPlan {
                slot: host.slot_for(&plan),
                ..plan
            };
            let Ran { key, worker, end } = host.run_session(plan, Fallbacks::None).await?;
            if end.reason != EndReason::Completed {
                return Ok(Err(format!(
                    "the builder's session ended before it was done ({})",
                    ended_why(&end)
                )));
            }
            worktree::commit_in(host, &task, &tree, Some(&key), Some(worker)).await?;
            if let Work::Comments { comments } = &due.work {
                due.replies = replies_of(&end, comments);
            }
            let fixed = git.head_commit(&tree.path).await?.unwrap_or(head.clone());
            due.fixed = Some(fixed.clone());
            keep_progress(host, followed, due);
            fixed
        }
    };
    if fixed == head {
        return Ok(match &due.work {
            Work::Check { check, .. } => Err(format!(
                "its builder found nothing to change for {check}, so it's yours to look at"
            )),
            // Every comment was answered with a reply, which waits for you.
            Work::Comments { .. } => Ok(None),
        });
    }
    if !due.reviewed {
        let diff = git.diff(&tree.path, &head, &fixed).await?;
        let reviewer = host
            .read(|state| reviewer_of(state, &task))
            .or_else(|| another_provider(host, &builder.1));
        let Some(reviewer) = reviewer else {
            return Ok(Err(
                "no agent from another provider than its builder can look at the fix".to_owned(),
            ));
        };
        let (chunks, cut) = diff_cut::cut(&diff, host.settings().review.max_diff_lines);
        let mut shown = chunks.concat();
        if let Some(cut) = cut {
            shown.push_str(&format!("\n{cut}\n"));
        }
        let plan = SessionPlan {
            without_status: false,
            at: None,
            role: RoleName::Reviewer,
            worker: reviewer.worker.clone(),
            task: Some(task.clone()),
            access: reviewer.access,
            step: None,
            workdir: tree.path.clone(),
            parts: vec![part(
                InputPartKind::Instructions,
                "the fix",
                watch::review_prompt(followed.number, &due.work, &shown),
                None,
            )],
            json_schema: Some(output_schema::<Review>()),
            resume: reviewer.session,
            slot: 0,
            label: Some(format!("review:{what}-round-{round}")),
            reviews: true,
            fresh: false,
            independent_of: vec![builder.1.clone()],
            fork_of: None,
            in_place_of: None,
        };
        let plan = SessionPlan {
            slot: host.slot_for(&plan),
            ..plan
        };
        let Ran { end, worker, .. } = host.run_session(plan, Fallbacks::None).await?;
        let review = structured::<ReviewOutput>(&end);
        match review {
            Some(review) if review.verdict == ReviewVerdict::Approve => {}
            Some(review) => {
                return Ok(Err(format!(
                    "{} asked for changes to the fix: {}",
                    worker,
                    review.summary.trim()
                )));
            }
            None => {
                return Ok(Err(format!(
                    "the fix's reviewer gave no verdict ({})",
                    ended_why(&end)
                )));
            }
        }
        due.reviewed = true;
        keep_progress(host, followed, due);
    }
    // Comments come from other people, and a push runs on GitHub with the repository's
    // secrets, so a fix that touches a protected path, such as `.github/**`, is yours to push.
    let changed = git.changed_paths(&tree.path, &head, &fixed).await?;
    let protected =
        crate::merge::merge_packet::protected(&host.settings().limits.protected_paths, &changed)?;
    if !protected.is_empty() {
        return Ok(Err(format!(
            "the fix changes {}, which only you may push",
            argonauts_core::base::text::and_list(&protected)
        )));
    }
    let root = host.repo().root.clone();
    let remotes = host.git().remotes(&root).await?;
    let Some(remote) = super::remote_of(&remotes, followed.remote.as_deref()) else {
        return Ok(Err(
            "this repository has no remote to push the fix to".to_owned()
        ));
    };
    if let Err(err) = host
        .git()
        .push_commit(&root, remote, &fixed, &followed.branch)
        .await
    {
        return Ok(Err(format!(
            "pushing the fix to {} failed, so it waits here: {err}",
            followed.branch
        )));
    }
    note(
        host,
        NoteLevel::Info,
        format!(
            "Pushed the reviewed fix to {}, so #{} updates.",
            followed.branch, followed.number
        ),
    )?;
    Ok(Ok(Some(fixed)))
}

/// `Following #57: CI failed on test (ubuntu), so its builder fixes it, round 1 of 3.`
fn starting(followed: &Followed, work: &Work, round: u32) -> String {
    let number = followed.number;
    match work {
        Work::Check { check, .. } => format!(
            "Following #{number}: {check} failed, so the task's builder fixes it (round {round})."
        ),
        Work::Comments { comments } if comments.len() == 1 => format!(
            "Following #{number}: a comment goes back to the task's builder (round {round})."
        ),
        Work::Comments { comments } => format!(
            "Following #{number}: {} comments go back to the task's builder (round {round}).",
            comments.len()
        ),
    }
}

fn prompt(number: u64, work: &Work) -> String {
    match work {
        Work::Check { check, log } => watch::fix_prompt(number, check, log),
        Work::Comments { comments } => watch::answer_prompt(number, comments),
    }
}

/// The task a round's work goes to: the one that changed the file a comment is on, else the
/// first the run merged.
fn task_of(state: &RunState, work: &Work) -> Option<TaskId> {
    use argonauts_core::runs::events::task::TaskState;
    let merged: Vec<_> = state
        .tasks
        .by_id
        .values()
        .filter(|task| task.state == TaskState::Merged)
        .collect();
    let on_a_file = match work {
        Work::Comments { comments } => comments
            .iter()
            .filter_map(|comment| comment.at.as_deref())
            .map(|at| at.split(':').next().unwrap_or(at))
            .find_map(|path| {
                merged.iter().find(|task| {
                    task.reports
                        .iter()
                        .any(|report| report.files_changed.iter().any(|file| file == path))
                })
            }),
        Work::Check { .. } => None,
    };
    on_a_file
        .or_else(|| merged.first())
        .map(|task| task.spec.id.clone())
}

/// The newest building session of `task` its CLI can carry on, with its worker.
fn builder_of(state: &RunState, task: &TaskId) -> Option<(SessionKey, ModelRef)> {
    state
        .sessions
        .by_key
        .values()
        .filter(|session| {
            session.task.as_ref() == Some(task)
                && session.started.role == RoleName::Worker
                && session.started.access == Access::Build
                && session.cli_session.is_some()
        })
        .max_by_key(|session| session.since)
        .map(|session| (session.key.clone(), session.started.worker.clone()))
}

/// Who reviewed `task` last, and the session the fix's review carries on, where its CLI can.
struct Reviewer {
    worker: ModelRef,
    access: Access,
    session: Option<SessionKey>,
}

fn reviewer_of(state: &RunState, task: &TaskId) -> Option<Reviewer> {
    let newest = state
        .sessions
        .by_key
        .values()
        .filter(|session| {
            session.task.as_ref() == Some(task) && session.started.role == RoleName::Reviewer
        })
        .max_by_key(|session| session.since)?;
    Some(Reviewer {
        worker: newest.started.worker.clone(),
        access: newest.started.access,
        session: newest.cli_session.is_some().then(|| newest.key.clone()),
    })
}

/// A reviewer for a task none reviewed, as a Captain run's: the first of the crew's reviewers,
/// then of its members, from another provider than the builder.
fn another_provider(host: &Host, builder: &ModelRef) -> Option<Reviewer> {
    let workers = &host.services().workers;
    let provider = |model: &ModelRef| workers.get(model).ok().map(|worker| worker.provider());
    let theirs = provider(builder)?;
    let settings = host.settings();
    crate::roles::candidates(settings, RoleName::Reviewer)
        .into_iter()
        .chain(settings.crew.members())
        .find(|candidate| provider(candidate).is_some_and(|provider| provider != theirs))
        .map(|worker| Reviewer {
            access: if crate::review::is_endpoint(host, &worker) {
                Access::NoTools
            } else {
                Access::ReadOnly
            },
            worker,
            session: None,
        })
}

/// The commit the pull request's branch was last pushed at: as kept, else the run's own
/// branch, which the draft was opened from.
async fn head_of(host: &Host, followed: &Followed) -> Option<String> {
    if let Some(head) = &followed.head {
        return Some(head.clone());
    }
    let branch = host.read(crate::merge::merge_packet::landing_branch_of)?;
    host.git().resolve(&host.repo().root, &branch).await.ok()
}

/// What the builder answered each comment with, as the schema asks.
#[derive(Debug, Deserialize)]
struct Answers {
    answers: Vec<Answer>,
}

#[derive(Debug, Deserialize)]
struct Answer {
    /// Its number in the list, from 1.
    comment: usize,
    changed: bool,
    #[serde(default)]
    reply: String,
}

fn answers_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "answers": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "comment": {"type": "integer", "minimum": 1},
                        "changed": {"type": "boolean"},
                        "reply": {"type": "string"}
                    },
                    "required": ["comment", "changed", "reply"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["answers"],
        "additionalProperties": false
    })
}

/// The replies to the comments the builder answered with words rather than a change.
fn replies_of(end: &SessionEnd, comments: &[ToAnswer]) -> Vec<Reply> {
    let Some(answers) = structured::<Answers>(end) else {
        return Vec::new();
    };
    answers
        .answers
        .into_iter()
        .filter(|answer| !answer.changed && !answer.reply.trim().is_empty())
        .filter_map(|answer| {
            let comment = comments.get(answer.comment.checked_sub(1)?)?;
            Some(Reply {
                to: comment.author.clone(),
                thread: comment.thread,
                body: answer.reply.trim().to_owned(),
            })
        })
        .collect()
}

/// A completed session's answer in the shape `T`: its structured output, or else the last JSON
/// object in its text, for agents with no schema flag.
fn structured<T: for<'de> Deserialize<'de>>(end: &SessionEnd) -> Option<T> {
    if end.reason != EndReason::Completed {
        return None;
    }
    end.structured
        .clone()
        .filter(Value::is_object)
        .or_else(|| {
            [&end.result_text, &end.last_message]
                .into_iter()
                .flatten()
                .find_map(|text| last_json_object(text))
        })
        .and_then(|value| serde_json::from_value(value).ok())
}

fn ended_why(end: &SessionEnd) -> String {
    match &end.reason {
        EndReason::Completed => "it ended without an answer".to_owned(),
        other => crate::engine::job_fallback::in_words(other),
    }
}

fn note(host: &Host, level: NoteLevel, text: String) -> EngineResult<()> {
    host.record(None, None, RunEventKind::Note(Note { level, text }))?;
    Ok(())
}

/// Saves where the round stands, so a pause or a crash carries on from there.
fn keep_progress(host: &Host, followed: &Followed, due: &Due) {
    let standing = Followed {
        due: Some(due.clone()),
        ..followed.clone()
    };
    keep(host, &standing);
}

fn keep(host: &Host, followed: &Followed) {
    if let Err(err) = followed.save(host.repo().state_dir()) {
        tracing::warn!("keeping where #{} stands: {err}", followed.number);
    }
}
