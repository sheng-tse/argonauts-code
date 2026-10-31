//! What a run on an issue writes on the issue itself, when `[issue]` asks for it: a comment as
//! work starts or once its draft opens, a label while it's worked on, and you as its assignee.
//! Everything posts as you, so by default nothing is written, and each write is shown first
//! unless you allowed GitHub writes for the run.

use argonauts_conductor::gh::{Gh, GhError, RepoName};
use argonauts_core::NoteLevel;
use argonauts_core::app::{AppChoice, AppEvent, ChoiceKind};
use argonauts_core::base::names::ChoiceId;
use argonauts_core::runs::events::approval::{AnswerAction, AnswerOption, FreeText};
use argonauts_core::settings::issue::IssueSettings;
use argonauts_crew::follow::{Followed, IssueLink};

use super::gh_here;
use crate::controller::{Ctx, Outbox};

/// The answers of the question, which also tell the screen whose question it is.
pub(crate) const POST: &str = "issue:write-post";
pub(crate) const ALLOW: &str = "issue:write-allow";
pub(crate) const SKIP: &str = "issue:write-skip";

/// The label an issue carries while a run works on it.
pub(crate) const LABEL: &str = "in progress";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Write {
    Comment { body: String },
    Label,
    Unlabel,
    AssignMe,
}

/// Writes on one issue, as who.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OnIssue {
    pub repo: RepoName,
    pub number: u64,
    pub login: String,
    pub writes: Vec<Write>,
}

impl OnIssue {
    fn line(&self, write: &Write) -> String {
        let number = self.number;
        match write {
            Write::Comment { body } => format!("Comment on #{number}: {body}"),
            Write::Label => format!("Label #{number} \"{LABEL}\""),
            Write::Unlabel => format!("Take \"{LABEL}\" off #{number}"),
            Write::AssignMe => format!("Assign #{number} to {}", self.login),
        }
    }
}

/// What goes on the issue as the run on it starts.
pub(crate) fn at_start(settings: &IssueSettings, link: &IssueLink) -> OnIssue {
    let mut writes = Vec::new();
    if settings.comment.at_start() {
        writes.push(Write::Comment {
            body: format!(
                "Work on this has started in Argonauts Code, on branch {}. A draft pull request \
                 will name this issue.",
                link.branch
            ),
        });
    }
    if settings.labels {
        writes.push(Write::Label);
    }
    if settings.assign_me {
        writes.push(Write::AssignMe);
    }
    OnIssue {
        repo: link.repo.clone(),
        number: link.number,
        login: link.login.clone(),
        writes,
    }
}

/// What goes on the issue once its draft opened: a comment with the draft's address, and the
/// label comes off, since the run's work on it is done.
pub(crate) fn at_draft(settings: &IssueSettings, followed: &Followed, login: &str) -> OnIssue {
    let mut writes = Vec::new();
    if settings.comment.at_draft() {
        writes.push(Write::Comment {
            body: format!("A draft pull request for this is open: {}", followed.url),
        });
    }
    if settings.labels {
        writes.push(Write::Unlabel);
    }
    OnIssue {
        repo: followed.repo.clone(),
        number: followed.issue.unwrap_or_default(),
        login: login.to_owned(),
        writes,
    }
}

/// The question the writes wait on.
pub(crate) fn question(id: ChoiceId, on: &OnIssue) -> AppChoice {
    let title = if on.writes.len() == 1 {
        format!("Write this on #{} on GitHub?", on.number)
    } else {
        format!("Write these on #{} on GitHub?", on.number)
    };
    let mut body: Vec<String> = on.writes.iter().map(|write| on.line(write)).collect();
    body.push(String::new());
    body.push(format!("It posts as you (gh: {}).", on.login));
    let option = |key: char, label: &str, value: &str, detail: &str| AnswerOption {
        key,
        label: label.to_owned(),
        action: AnswerAction::Choose {
            value: value.to_owned(),
        },
        recommended: false,
        detail: Some(detail.to_owned()),
    };
    AppChoice {
        id,
        kind: ChoiceKind::GoAhead,
        title,
        body,
        options: vec![
            option('p', "Write them", POST, "on GitHub now"),
            option(
                'a',
                "Allow GitHub writes for the rest of this run",
                ALLOW,
                "these and any later comment or reply in the run go without asking",
            ),
            option('n', "Don't write them", SKIP, "nothing goes to GitHub"),
        ],
        free_text: FreeText::None,
        bell: true,
    }
}

/// Writes them beside the controller, saying which didn't go.
pub(crate) fn post(ctx: &Ctx, on: OnIssue) {
    let gh = gh_here::gh_for(&ctx.env.repo_root);
    let out = ctx.out.clone();
    tokio::spawn(async move {
        match gh {
            Ok(gh) => post_with(&gh, &on, &out).await,
            Err(err) => say(
                &out,
                format!("Nothing was written on #{}: {err}", on.number),
            ),
        }
    });
}

async fn post_with(gh: &Gh, on: &OnIssue, out: &Outbox) {
    for write in &on.writes {
        if let Err(err) = write_one(gh, on, write).await {
            say(
                out,
                format!("Couldn't {}: {err}", lowercase_first(&on.line(write))),
            );
        }
    }
}

async fn write_one(gh: &Gh, on: &OnIssue, write: &Write) -> Result<(), GhError> {
    match write {
        Write::Comment { body } => gh.comment_on_issue(&on.repo, on.number, body).await,
        Write::Label => gh.label(&on.repo, on.number, LABEL).await,
        Write::Unlabel => gh.unlabel(&on.repo, on.number, LABEL).await,
        Write::AssignMe => gh.assign_me(&on.repo, on.number).await,
    }
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn say(out: &Outbox, text: String) {
    out.send(AppEvent::Notice {
        level: NoteLevel::Error,
        text,
    });
}
