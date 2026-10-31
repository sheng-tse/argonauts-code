//! Following the pull requests Argonauts Code opened, while the app is open and again at the
//! next start. What GitHub says of each is read beside the controller. A round that's due goes
//! to the engine of the run that opened it, and what only you can say is asked. Replies post as
//! you, so they're shown first, unless you allowed GitHub writes for the run.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use tokio::time::Instant;

use argonauts_conductor::gh::{Gh, GhError};
use argonauts_core::NoteLevel;
use argonauts_core::RunId;
use argonauts_core::app::{
    AppChoice, AppEvent, ChoiceKind, FollowedPullRequest, PullRequestStands,
};
use argonauts_core::base::names::ChoiceId;
use argonauts_core::runs::events::approval::{AnswerAction, AnswerOption, FreeText};
use argonauts_core::settings::pr::PrSettings;
use argonauts_crew::follow::watch::{self, AskWhy, Next};
use argonauts_crew::follow::{Due, Followed, Reply, Work};

use super::gh_here;
use crate::controller::{Ctx, Internal, Outbox};

/// How often GitHub is asked again about each pull request being followed.
const LOOK_EVERY: Duration = Duration::from_secs(120);

/// The answers of the follow-up's questions, which also tell the screen whose question it is.
pub(crate) const GO: &str = "issue:pr-go";
pub(crate) const LATER: &str = "issue:pr-later";
pub(crate) const STOP: &str = "issue:pr-stop";
pub(crate) const POST: &str = "issue:pr-post";
pub(crate) const ALLOW: &str = "issue:pr-allow";
pub(crate) const DROP: &str = "issue:pr-drop";

#[derive(Debug, Default)]
pub(crate) struct Following {
    looking: bool,
    next_look: Option<Instant>,
    asked: Vec<Asked>,
    /// The runs asked to work a round since the app opened, by the round's number.
    resumed: BTreeSet<(RunId, u32)>,
    /// How each pull request's run row reads on screen, by its number, as last told.
    told: BTreeMap<u64, FollowedPullRequest>,
}

#[derive(Debug)]
struct Asked {
    choice: ChoiceId,
    number: u64,
    about: About,
}

#[derive(Debug)]
enum About {
    /// Only you can say what happens next, for `why`. `said` is how its row reads, and `work`
    /// is what goes back to the builder if you say so.
    You {
        why: AskWhy,
        said: String,
        work: Option<Work>,
    },
    /// A round couldn't go on, for the reason it kept.
    Held { why: String },
    /// Replies a round wrote, waiting to be posted.
    Replies,
    /// What `[issue]` writes on the issue once its draft opened.
    OnIssue(super::writes::OnIssue),
}

/// What one look at GitHub found.
#[derive(Debug)]
pub(crate) struct Looked {
    found: Vec<Found>,
    max_rounds: u32,
    /// Who `gh` posts as.
    login: String,
}

#[derive(Debug)]
struct Found {
    followed: Followed,
    /// What comes next for it, or why GitHub couldn't say.
    said: Result<Said, String>,
}

#[derive(Debug)]
struct Said {
    next: Next,
    work: Option<Work>,
    /// Comments left out for who wrote them.
    left_out: usize,
    /// The commit the pull request is at.
    head: Option<String>,
}

/// Looks at GitHub once it's time: at the start, then every [`LOOK_EVERY`]. The answer comes
/// back as [`Internal::Followed`].
pub(crate) fn look(ctx: &Ctx, following: &mut Following) {
    let now = Instant::now();
    if following.looking || following.next_look.is_some_and(|at| now < at) {
        return;
    }
    following.next_look = Some(now + LOOK_EVERY);
    let open = Followed::open(&ctx.env.state_dir);
    if open.is_empty() {
        return;
    }
    following.looking = true;
    let gh = gh_here::gh_for(&ctx.env.repo_root);
    let pr = ctx.settings().pr.clone();
    let internal = ctx.internal.clone();
    tokio::spawn(async move {
        let looked = look_at(gh, open, &pr).await;
        // After Exit nobody listens, and the next start looks again.
        let _ = internal.send(Internal::Followed(Box::new(looked)));
    });
}

/// What GitHub says of each pull request in `open`, through `gh`, with `pr`'s rounds and the
/// review bots it names.
pub(crate) async fn look_at(
    gh: Result<Gh, GhError>,
    open: Vec<Followed>,
    pr: &PrSettings,
) -> Looked {
    let max_rounds = pr.max_rounds;
    let looked = |found, login| Looked {
        found,
        max_rounds,
        login,
    };
    let you = match &gh {
        Ok(gh) => gh.login().await,
        Err(err) => Err(err.clone()),
    };
    let (gh, you) = match (gh, you) {
        (Ok(gh), Ok(you)) => (gh, you),
        (_, Err(err)) | (Err(err), _) => {
            tracing::warn!("following pull requests: {err}");
            return looked(Vec::new(), String::new());
        }
    };
    let mut found = Vec::new();
    for followed in open {
        // A round already due is the engine's, so GitHub has nothing new to say of it yet.
        if followed.due.is_some() {
            found.push(Found {
                followed,
                said: Err(String::new()),
            });
            continue;
        }
        let who = watch::Commenters::new(&you, &pr.review_bots);
        let said = watch::look(&gh, &followed)
            .await
            .map(|seen| Said {
                next: watch::next(&seen, &followed, max_rounds, &who),
                work: watch::work_of(&seen, &followed, &who),
                left_out: watch::left_out(&seen, &followed, &who),
                head: seen.pull.head_commit.clone(),
            })
            .map_err(|err| err.to_string());
        found.push(Found { followed, said });
    }
    looked(found, you)
}

/// Acts on what a look found: a round goes to its run, an end is kept, and what needs you is
/// asked, one question at a time for each pull request.
pub(crate) fn heard(ctx: &mut Ctx, following: &mut Following, looked: Looked) {
    following.looking = false;
    for Found { mut followed, said } in looked.found {
        if following
            .asked
            .iter()
            .any(|asked| asked.number == followed.number)
        {
            continue;
        }
        if followed.due.is_some() {
            resume_once(ctx, following, &followed);
            continue;
        }
        if followed.issue.is_some() && !followed.issue_told {
            let on = super::writes::at_draft(&ctx.settings().issue, &followed, &looked.login);
            if !on.writes.is_empty() && !followed.writes_allowed {
                ask(ctx, following, &followed, About::OnIssue(on));
                continue;
            }
            if !on.writes.is_empty() {
                super::writes::post(ctx, on);
            }
            followed.issue_told = true;
            keep(ctx, &followed);
        }
        if !followed.replies.is_empty() {
            if followed.writes_allowed {
                post_replies(ctx, followed);
            } else {
                ask(ctx, following, &followed, About::Replies);
            }
            continue;
        }
        if let Some(why) = followed.held.clone() {
            if followed.set_aside.as_deref() != Some(why.as_str()) {
                ask(ctx, following, &followed, About::Held { why });
            }
            continue;
        }
        let said = match said {
            Ok(said) => said,
            Err(why) => {
                tracing::warn!("following #{}: {why}", followed.number);
                continue;
            }
        };
        act(ctx, following, followed, said, looked.max_rounds);
    }
}

fn act(ctx: &mut Ctx, following: &mut Following, mut followed: Followed, said: Said, max: u32) {
    let words = watch::said(&followed, &said.next, max);
    let heading = watch::heading(&followed, &said.next, max, said.left_out);
    tell_row(ctx, following, &followed, &words, stands(&said.next));
    if followed.said.as_deref() != Some(words.as_str())
        || followed.heading.as_ref() != Some(&heading)
    {
        followed.said = Some(words);
        followed.heading = Some(heading);
        keep(ctx, &followed);
    }
    match said.next {
        Next::Ended { merged } => {
            followed.ended = true;
            keep(ctx, &followed);
            let how = if merged { "merged" } else { "was closed" };
            ctx.notice(
                NoteLevel::Info,
                format!(
                    "#{} {how} on GitHub, so following it ends.",
                    followed.number
                ),
            );
        }
        // What you set aside is asked again once GitHub says something else.
        Next::Wait { .. } => {
            if followed.set_aside.take().is_some() {
                keep(ctx, &followed);
            }
        }
        Next::FixCheck { check, log } => {
            start_round(
                ctx,
                following,
                followed,
                Work::Check { check, log },
                said.head,
            );
        }
        Next::Answer { comments } => {
            start_round(
                ctx,
                following,
                followed,
                Work::Comments { comments },
                said.head,
            );
        }
        Next::AskYou { ref why } => {
            let words = watch::said(&followed, &said.next, max);
            if followed.set_aside.as_deref() == Some(words.as_str()) {
                return;
            }
            let about = About::You {
                why: why.clone(),
                said: words,
                work: said.work,
            };
            ask(ctx, following, &followed, about);
        }
    }
}

/// Where a pull request stands after a look found `next`.
fn stands(next: &Next) -> PullRequestStands {
    match next {
        Next::Ended { .. } => PullRequestStands::Ended,
        Next::Wait { .. } => PullRequestStands::Waits,
        Next::FixCheck { .. } | Next::Answer { .. } => PullRequestStands::Works,
        Next::AskYou { .. } => PullRequestStands::NeedsYou,
    }
}

/// Tells the screen how the run's row reads now, `#57 · fixing CI, round 1 of 3`, once each
/// time that changes.
fn tell_row(
    ctx: &Ctx,
    following: &mut Following,
    followed: &Followed,
    said: &str,
    stands: PullRequestStands,
) {
    let row = FollowedPullRequest {
        said: said.to_owned(),
        stands,
    };
    if following.told.get(&followed.number) == Some(&row) {
        return;
    }
    following.told.insert(followed.number, row.clone());
    ctx.send(AppEvent::PullRequestFollowed {
        run: followed.run.clone(),
        followed: row,
    });
}

/// Keeps the round as due, then has the run's engine work it.
fn start_round(
    ctx: &Ctx,
    following: &mut Following,
    mut followed: Followed,
    work: Work,
    head: Option<String>,
) {
    followed.due = Some(Due {
        work,
        fixed: None,
        reviewed: false,
        replies: Vec::new(),
    });
    followed.worked_at = head.or_else(|| followed.head.clone());
    followed.set_aside = None;
    if keep(ctx, &followed) {
        resume_once(ctx, following, &followed);
    }
}

/// Wakes the run that opened the pull request, so its engine works the round that's due. A
/// run asked once since the app opened isn't asked again for the same round.
fn resume_once(ctx: &Ctx, following: &mut Following, followed: &Followed) {
    if !following
        .resumed
        .insert((followed.run.clone(), followed.rounds + 1))
    {
        return;
    }
    let conductor = ctx.services.conductor.clone();
    let out = ctx.out.clone();
    let run = followed.run.clone();
    let number = followed.number;
    tokio::spawn(async move {
        if let Err(err) = conductor.resume(&run).await {
            notice(
                &out,
                NoteLevel::Error,
                format!("Couldn't carry on {run} for #{number}'s follow-up: {err}"),
            );
        }
    });
}

fn ask(ctx: &mut Ctx, following: &mut Following, followed: &Followed, about: About) {
    let choice = ctx.next_choice_id();
    ctx.send(AppEvent::Choice {
        choice: question(choice.clone(), followed, &about),
    });
    following.asked.push(Asked {
        choice,
        number: followed.number,
        about,
    });
}

/// The question for `about`, under the pull request it's on.
fn question(id: ChoiceId, followed: &Followed, about: &About) -> AppChoice {
    let number = followed.number;
    let (title, body, options) = match about {
        About::You { why, said, work } => {
            let (_, what) = said.split_once(" · ").unwrap_or(("", said.as_str()));
            let mut options = Vec::new();
            if work.is_some() {
                options.push(option(
                    'y',
                    "Send it to the task's builder",
                    GO,
                    "a round as any other: the builder, its reviewer, then the push",
                ));
            }
            options.push(option(
                'l',
                "Leave it to me",
                LATER,
                "asked again once GitHub says something else",
            ));
            options.push(stop(number));
            let body = vec![why_words(why), followed.url.clone()];
            (format!("#{number}: {what}"), body, options)
        }
        About::Held { why } => (
            format!("#{number}'s follow-up waits for you"),
            vec![why.clone()],
            vec![
                option(
                    'y',
                    "Look again",
                    GO,
                    "GitHub is read again, and a round starts if one is due",
                ),
                option(
                    'l',
                    "Leave it to me",
                    LATER,
                    "nothing more is done for this",
                ),
                stop(number),
            ],
        ),
        About::OnIssue(on) => {
            let asked = super::writes::question(id, on);
            return AppChoice {
                title: format!("#{number} is open as a draft. {}", asked.title),
                ..asked
            };
        }
        About::Replies => {
            let count = followed.replies.len();
            let title = if count == 1 {
                format!("Post a reply on #{number}?")
            } else {
                format!("Post {count} replies on #{number}?")
            };
            let mut body: Vec<String> = followed.replies.iter().map(reply_line).collect();
            body.push(String::new());
            body.push("They post as you on GitHub.".to_owned());
            let options = vec![
                option('p', "Post them", POST, "posted now, each under its comment"),
                option(
                    'a',
                    "Allow GitHub writes for the rest of this run",
                    ALLOW,
                    "these and any later replies on it go without asking",
                ),
                option('n', "Don't post", DROP, "nothing goes to GitHub"),
            ];
            (title, body, options)
        }
    };
    AppChoice {
        id,
        kind: ChoiceKind::GoAhead,
        title,
        body,
        options,
        free_text: FreeText::None,
        bell: true,
    }
}

/// Why only you can say, in a sentence under the question.
fn why_words(why: &AskWhy) -> String {
    match why {
        AskWhy::Disagree => {
            "One reviewer approved what another asked to change, so the crew doesn't pick a side."
                .to_owned()
        }
        AskWhy::OutsideTheChange { check } => {
            format!("{check}'s log points at the CI machine or the network, not at the change.")
        }
        AskWhy::FailsOnBase { check, base } => format!(
            "{check} failed the last time it ran on {base} too, so the change didn't break it: \
             a test that fails now and then, or one broken before."
        ),
        AskWhy::Question { from } => format!("{from} asks something only you can answer."),
        AskWhy::RoundsSpent { max } => format!(
            "It has had {max} rounds of fixes, as many as pr.max_rounds allows. Another goes \
             back to the task's builder as before."
        ),
    }
}

/// `To coderabbitai[bot]: It is named per key already.`
fn reply_line(reply: &Reply) -> String {
    format!("To {}: {}", reply.to, reply.body)
}

fn option(key: char, label: &str, value: &str, detail: &str) -> AnswerOption {
    AnswerOption {
        key,
        label: label.to_owned(),
        action: AnswerAction::Choose {
            value: value.to_owned(),
        },
        recommended: false,
        detail: Some(detail.to_owned()),
    }
}

fn stop(number: u64) -> AnswerOption {
    option(
        's',
        &format!("Stop following #{number}"),
        STOP,
        "it stays open on GitHub, as it is",
    )
}

pub(crate) fn owns_choice(following: &Following, id: &ChoiceId) -> bool {
    following.asked.iter().any(|asked| &asked.choice == id)
}

/// Your answer to one of the follow-up's questions.
pub(crate) fn answer_choice(
    ctx: &mut Ctx,
    following: &mut Following,
    id: &ChoiceId,
    action: &AnswerAction,
) {
    let Some(at) = following.asked.iter().position(|asked| &asked.choice == id) else {
        return;
    };
    let asked = following.asked.remove(at);
    let value = match action {
        AnswerAction::Choose { value } => value.as_str(),
        _ => LATER,
    };
    let Some(mut followed) = Followed::open(&ctx.env.state_dir)
        .into_iter()
        .find(|followed| followed.number == asked.number)
    else {
        closed(
            ctx,
            id,
            format!("#{} isn't followed any more.", asked.number),
        );
        return;
    };
    let number = followed.number;
    match (asked.about, value) {
        (About::OnIssue(on), super::writes::POST | super::writes::ALLOW) => {
            followed.issue_told = true;
            followed.writes_allowed |= value == super::writes::ALLOW;
            keep(ctx, &followed);
            closed(ctx, id, format!("Writing on #{}.", on.number));
            super::writes::post(ctx, on);
        }
        (About::OnIssue(on), _) => {
            followed.issue_told = true;
            keep(ctx, &followed);
            closed(ctx, id, format!("Nothing was written on #{}.", on.number));
        }
        (_, STOP) => {
            followed.ended = true;
            keep(ctx, &followed);
            closed(ctx, id, format!("Stopped following #{number}."));
        }
        (About::Replies, POST | ALLOW) => {
            followed.writes_allowed |= value == ALLOW;
            closed(ctx, id, format!("Posting on #{number}."));
            post_replies(ctx, followed);
        }
        (About::Replies, _) => {
            followed.replies.clear();
            keep(ctx, &followed);
            closed(ctx, id, format!("Nothing was posted on #{number}."));
        }
        (
            About::You {
                work: Some(work), ..
            },
            GO,
        ) => {
            // Past the round limit, your yes allows one more.
            if followed.rounds >= followed.rounds_allowed(ctx.settings().pr.max_rounds) {
                followed.more_rounds += 1;
            }
            closed(
                ctx,
                id,
                format!("#{number} goes back to the task's builder."),
            );
            let head = followed.worked_at.clone();
            start_round(ctx, following, followed, work, head);
        }
        (About::Held { .. }, GO) => {
            followed.held = None;
            keep(ctx, &followed);
            closed(ctx, id, format!("Looking at #{number} again."));
            following.next_look = None;
            look(ctx, following);
        }
        (About::You { said, .. }, _) => {
            followed.set_aside = Some(said);
            keep(ctx, &followed);
            closed(ctx, id, format!("Left to you: #{number}."));
        }
        (About::Held { why }, _) => {
            followed.set_aside = Some(why);
            keep(ctx, &followed);
            closed(ctx, id, format!("Left to you: #{number}."));
        }
    }
}

/// Posts the replies a round wrote, each under the comment it answers, then keeps the rest.
fn post_replies(ctx: &Ctx, mut followed: Followed) {
    let replies = std::mem::take(&mut followed.replies);
    // Kept without them first, so a second look doesn't post them again.
    if !keep(ctx, &followed) {
        return;
    }
    let gh = gh_here::gh_for(&ctx.env.repo_root);
    let state_dir = ctx.env.state_dir.clone();
    let out = ctx.out.clone();
    tokio::spawn(async move {
        let unposted = match gh {
            Ok(gh) => post(&gh, &followed, replies).await,
            Err(err) => {
                notice(&out, NoteLevel::Error, format!("Nothing was posted: {err}"));
                replies
            }
        };
        if unposted.is_empty() {
            return;
        }
        followed.replies = unposted;
        if let Err(err) = followed.save(&state_dir) {
            tracing::warn!("keeping #{}'s replies: {err}", followed.number);
        }
    });
}

/// Posts each reply, and gives back those that didn't go.
async fn post(gh: &Gh, followed: &Followed, replies: Vec<Reply>) -> Vec<Reply> {
    let mut unposted = Vec::new();
    for reply in replies {
        let posted = match reply.thread {
            Some(thread) => {
                gh.reply(&followed.repo, followed.number, thread, &reply.body)
                    .await
            }
            None => {
                let body = format!("@{} {}", reply.to, reply.body);
                gh.comment_on_pull(&followed.repo, followed.number, &body)
                    .await
            }
        };
        if let Err(err) = posted {
            tracing::warn!("posting a reply on #{}: {err}", followed.number);
            unposted.push(reply);
        }
    }
    unposted
}

fn closed(ctx: &Ctx, id: &ChoiceId, outcome: String) {
    ctx.send(AppEvent::ChoiceClosed {
        id: id.clone(),
        outcome,
    });
}

fn notice(out: &Outbox, level: NoteLevel, text: String) {
    out.send(AppEvent::Notice { level, text });
}

/// Saves `followed`, saying so when it can't.
fn keep(ctx: &Ctx, followed: &Followed) -> bool {
    match followed.save(&ctx.env.state_dir) {
        Ok(()) => true,
        Err(err) => {
            ctx.notice(
                NoteLevel::Error,
                format!("Couldn't keep where #{} stands: {err}", followed.number),
            );
            false
        }
    }
}

#[cfg(test)]
mod tests;
