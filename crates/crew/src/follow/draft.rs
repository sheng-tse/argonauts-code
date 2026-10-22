//! The end of a run on a GitHub issue: a draft pull request in place of a merge. The push and
//! the draft post as you, so the draft is shown first and waits for your yes, unless you allowed
//! GitHub writes for this run or autopilot is on, which say so in the conversation. Argonauts
//! Code never closes, merges or marks the draft ready: `Fixes #n` closes the issue when you merge.

use std::path::Path;

use argonauts_conductor::gh::{Draft, Gh, GhError, Issue};
use argonauts_core::display_names::builtin_cli_name;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::approval::{
    AnswerAction, AnswerOption, ApprovalKind, ApprovalOpened, ApprovalPacket, FreeText,
    QuestionPacket,
};
use argonauts_core::events::run::RunOutcome;
use argonauts_core::events::task::{MergeEvent, ReviewEvent, TaskState};
use argonauts_core::hidden_text::sanitize;
use argonauts_core::names::ModelRef;
use argonauts_core::policy::EvidenceFacts;
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::{DiffStat, Note, NoteLevel, TaskId};

use super::IssueLink;
use super::followed::Followed;
use crate::approvals::{self, ApprovalRequest};
use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::merge::land::Landing;
use crate::merge::{final_check, kept, merge_packet};
use crate::workflow::StepAt;

/// The draft question's answers. The screen draws the question by them.
pub const PUSH: &str = "issue:push";
pub const WHOLE_BODY: &str = "issue:body";
pub const KEEP: &str = "issue:keep";
pub const ALLOW: &str = "issue:allow";

pub const TITLE: &str = "Open a draft pull request on GitHub?";

/// Whether `opened` is the draft's question, whose yes pushes and posts on GitHub as you.
pub fn asks(opened: &ApprovalOpened) -> bool {
    opened
        .options
        .iter()
        .any(|option| matches!(&option.action, AnswerAction::Choose { value } if value == PUSH))
}

/// Why nothing went out when you kept the branch.
const KEPT: &str = "you kept the branch here, and nothing was posted on GitHub";

/// Body rows the question shows before it folds the rest.
const SHOWN_BODY: usize = 2;

/// What the draft says, and what the question shows of the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftShown {
    pub branch: String,
    pub stat: DiffStat,
    pub checks: (usize, usize),
    /// `Codex approved, agy signed off`.
    pub reviewed: Option<String>,
    pub login: String,
    pub title: String,
    /// The summary, then the checks, who reviewed and how it was made, then the closing line.
    pub body: String,
    /// What happened to the issue on GitHub since the run read it, when anything did.
    pub since: Option<String>,
}

impl DraftShown {
    /// The question's words: two lines on the branch and who it posts as, then the title and the
    /// body, its middle folded unless `whole`.
    pub fn text(&self, whole: bool) -> String {
        let mut lines = vec![self.head_line(), self.posts_as()];
        lines.extend(self.since.clone());
        lines.push(String::new());
        lines.push(format!("title   {}", self.title));
        let body: Vec<&str> = self.body.lines().collect();
        let (closing, rest) = body.split_last().unwrap_or((&"", &[]));
        let folded = !whole && rest.len() > SHOWN_BODY;
        let shown = if folded { &rest[..SHOWN_BODY] } else { rest };
        for (at, line) in shown.iter().chain([closing]).enumerate() {
            if folded && at == SHOWN_BODY {
                lines.push(format!(
                    "        … {} more lines: the checks, who reviewed, and how it was made",
                    rest.len() - SHOWN_BODY
                ));
            }
            let key = if at == 0 { "body    " } else { "        " };
            lines.push(format!("{key}{line}").trim_end().to_owned());
        }
        lines.join("\n")
    }

    /// `branch issue-42-… · 2 files +30 −4 · checks 5/5 · Codex approved, agy signed off`.
    fn head_line(&self) -> String {
        let files = match self.stat.files_changed {
            1 => "1 file".to_owned(),
            n => format!("{n} files"),
        };
        let mut parts = vec![
            format!("branch {}", self.branch),
            format!(
                "{files} +{} \u{2212}{}",
                self.stat.insertions, self.stat.deletions
            ),
        ];
        let (passed, total) = self.checks;
        if total > 0 {
            parts.push(format!("checks {passed}/{total}"));
        }
        parts.extend(self.reviewed.clone());
        parts.join(" · ")
    }

    fn posts_as(&self) -> String {
        format!(
            "It posts as you (gh: {}) · nothing is posted on the issue itself",
            self.login
        )
    }
}

/// The draft's body: what the run did, its checks, who reviewed it and how it was made, then
/// the line that names the issue.
pub fn body(summary: &str, shown: &DraftShown, link: &IssueLink, closing: &str) -> String {
    let (passed, total) = shown.checks;
    let mut lines = vec![summary.trim().to_owned(), String::new()];
    if total > 0 {
        lines.push(format!("Checks: {passed} of {total} passed."));
    }
    if let Some(reviewed) = &shown.reviewed {
        lines.push(format!("Reviewed: {reviewed}."));
    }
    lines.push(format!(
        "Made by a crew of agents in Argonauts Code, on the plan approved for #{}.",
        link.number
    ));
    lines.push(String::new());
    lines.push(closing.to_owned());
    lines.join("\n")
}

/// Pushes the run's branch as the issue's and opens the draft, once you said yes.
pub async fn open(host: &Host, mut link: IssueLink) -> EngineResult<Landing> {
    let (local, _) = merge_packet::landing_branch(host).await?;
    let root = host.repo().root.clone();
    let base = match final_check::lands_on(host).await {
        Ok(Some(base)) => base,
        _ => link.default_branch.clone(),
    };
    let closing = link.closing_line(&base);
    let mut shown = shown_of(host, &root, &local, &link).await;
    let now = match host.git().installed("gh") {
        Some(program) => {
            let gh = Gh::at(program, &root).with_env(host.git().env().to_vec());
            gh.issue(&link.repo, link.number).await.ok()
        }
        None => None,
    };
    let closed = now.as_ref().is_some_and(|issue| !issue.open);
    if let Some(issue) = &now {
        shown.since = since(&link, issue);
        // Anyone may edit the title, and the draft posts it as you, so it's read as you'd see it.
        let title = one_line(&sanitize(&issue.title).text);
        if issue.open && !title.is_empty() {
            shown.title = title;
        }
    }
    shown.body = body(&summary_of(host), &shown, &link, &closing);
    let mut whole = false;
    let autopilot = approvals::autopilot::applies_now(host, ApprovalKind::Question);
    // An issue closed or edited since the run read it is no longer the one you approved the plan
    // for. Autopilot answers no question of yours, so it gets no draft in your place: the run
    // keeps its branch.
    let changed = shown.since.is_some();
    if autopilot && changed {
        let what = if closed { "closed" } else { "edited" };
        return not_opened(format!(
            "#{} was {what} on GitHub since the run read it, so autopilot opened no draft for \
             it. {} is kept here",
            link.number, local
        ));
    }
    loop {
        // An issue closed or edited since the run read it is yours to decide on, whatever writes
        // allow.
        if (link.writes_allowed || autopilot) && !changed {
            let why = if link.writes_allowed {
                "you allowed GitHub writes for this run"
            } else {
                "autopilot is on"
            };
            note(
                host,
                NoteLevel::Info,
                format!(
                    "Pushing {} and opening a draft pull request for #{}, since {why}.",
                    link.branch, link.number
                ),
            )?;
            return push_and_open(host, &root, &local, &base, &link, &shown).await;
        }
        let decided = ask(host, &shown, whole).await?;
        let value = match &decided {
            Ok(AnswerAction::Choose { value }) => value.as_str(),
            Ok(_) => PUSH,
            // Withdrawn, as when the run stops: nothing went out.
            Err(why) => return Ok(Landing::Again(why.clone())),
        };
        match value {
            WHOLE_BODY => whole = true,
            KEEP => return Ok(Landing::Again(KEPT.to_owned())),
            ALLOW => {
                link.writes_allowed = true;
                if let Err(err) = link.save(host.repo().state_dir()) {
                    tracing::warn!("keeping that GitHub writes are allowed: {err}");
                }
            }
            _ => return push_and_open(host, &root, &local, &base, &link, &shown).await,
        }
    }
}

/// What happened to the issue since the run read it: closed, or edited, with its new title
/// when that changed.
fn since(link: &IssueLink, now: &Issue) -> Option<String> {
    let number = link.number;
    if !now.open {
        return Some(format!(
            "#{number} was closed on GitHub since the run read it"
        ));
    }
    let edited = link
        .as_read
        .as_ref()
        .is_some_and(|read| *read != IssueLink::mark(&now.title, &now.body));
    if !edited {
        return None;
    }
    let title = one_line(&sanitize(&now.title).text);
    Some(if title == link.title {
        format!("#{number} was edited on GitHub since the run read it")
    } else {
        format!(
            "#{number} was edited on GitHub since the run read it: its title is now \"{title}\""
        )
    })
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Opens the question and waits for its answer, or why it closed without one.
async fn ask(
    host: &Host,
    shown: &DraftShown,
    whole: bool,
) -> EngineResult<Result<AnswerAction, String>> {
    let Some(task) = merged_task(host) else {
        return Ok(Err("the run has no task to open a draft for".to_owned()));
    };
    let request = ApprovalRequest {
        // Its step's place lets a run picked up again find the answer already given.
        at: running_step(host),
        kind: ApprovalKind::Question,
        title: TITLE.to_owned(),
        task: None,
        packet: ApprovalPacket::Question(QuestionPacket {
            from: host.settings().crew.main.clone(),
            task,
            text: shown.text(whole),
            options: Vec::new(),
        }),
        facts: EvidenceFacts::default(),
        agent_outputs: Vec::new(),
        documents: Vec::new(),
        options: Some(options()),
        free_text: FreeText::Guidance,
        authors: Vec::new(),
    };
    let because = "pushing and opening a pull request post on GitHub as you".to_owned();
    let decided = approvals::open_yours(host, request, because)?
        .decided_or_withdrawn(host)
        .await?;
    Ok(decided.map(|decided| decided.action))
}

/// The four answers, each with what it does beside it.
pub fn options() -> Vec<AnswerOption> {
    let option = |key: char, label: &str, value: &str, detail: &str| AnswerOption {
        key,
        label: label.to_owned(),
        action: AnswerAction::Choose {
            value: value.to_owned(),
        },
        recommended: false,
        detail: Some(detail.to_owned()),
    };
    vec![
        option(
            'p',
            "Push and open the draft",
            PUSH,
            "pushes the branch to origin, then opens it as a draft",
        ),
        option(
            'b',
            "Show the whole body",
            WHOLE_BODY,
            "the body in full, then these choices again",
        ),
        option(
            'k',
            "Keep the branch here, post nothing",
            KEEP,
            "the run ends with the branch kept",
        ),
        option(
            'a',
            "Allow GitHub writes for the rest of this run",
            ALLOW,
            "this and any later comment or reply in the run go without asking",
        ),
    ]
}

async fn push_and_open(
    host: &Host,
    root: &Path,
    local: &str,
    base: &str,
    link: &IssueLink,
    shown: &DraftShown,
) -> EngineResult<Landing> {
    let commit = kept::without_yours(host).await?;
    let git = host.git();
    let remotes = git.remotes(root).await?;
    let Some(remote) = super::remote_of(&remotes, link.push_remote.as_deref()) else {
        return not_opened(format!(
            "this repository has no remote to push {} to. {local} holds the run's work",
            link.branch
        ));
    };
    let pushing = commit.as_deref().unwrap_or(local);
    let pushed = git.push_commit(root, remote, pushing, &link.branch).await;
    if let Err(err) = pushed {
        return not_opened(format!("pushing {} to {remote} failed: {err}", link.branch));
    }
    let gh = match git.installed("gh") {
        Some(program) => Gh::at(program, root).with_env(git.env().to_vec()),
        None => return not_opened(GhError::NotInstalled.to_string()),
    };
    let draft = Draft {
        repo: link.repo.clone(),
        head: match &link.fork_account {
            Some(account) => format!("{account}:{}", link.branch),
            None => link.branch.clone(),
        },
        base: base.to_owned(),
        title: shown.title.clone(),
        body: shown.body.clone(),
    };
    let url = match gh.open_draft(&draft).await {
        Ok(url) => url,
        Err(err) => {
            return not_opened(format!("{err}. {} is pushed to {remote}", link.branch));
        }
    };
    let head = git.resolve(root, pushing).await.ok();
    if let Some(followed) = Followed::opened(host, link, &url, head)
        && let Err(err) = followed.save(host.repo().state_dir())
    {
        tracing::warn!("keeping the draft for its follow-up: {err}");
    }
    host.record(
        None,
        None,
        RunEventKind::Merge(MergeEvent::PullRequest {
            branch: link.branch.clone(),
            url: Some(url),
            command: Some(draft.command()),
        }),
    )?;
    Ok(Landing::Done(RunOutcome::PullRequest))
}

/// Nothing went out, so the run keeps its branch, and the merge step says why.
fn not_opened(why: String) -> EngineResult<Landing> {
    Ok(Landing::Again(why))
}

fn note(host: &Host, level: NoteLevel, text: String) -> EngineResult<()> {
    host.record(None, None, RunEventKind::Note(Note { level, text }))?;
    Ok(())
}

/// The branch, what the run changed against its base, its checks and who reviewed it.
async fn shown_of(host: &Host, root: &Path, local: &str, link: &IssueLink) -> DraftShown {
    let base = host.read(|state| {
        state
            .run
            .started
            .as_ref()
            .map(|started| started.base_commit.clone())
    });
    let stat = match base {
        Some(base) => host
            .git()
            .diff_stat(root, &base, local)
            .await
            .unwrap_or_default(),
        None => DiffStat::default(),
    };
    let (checks, reviewers) = host.read(|state| {
        let merged = state
            .tasks
            .by_id
            .values()
            .filter(|task| task.state == TaskState::Merged);
        let mut passed = 0;
        let mut total = 0;
        let mut reviewers: Vec<(ModelRef, &'static str)> = Vec::new();
        for task in merged {
            total += task.checks.len();
            passed += task.checks.iter().filter(|check| check.passed).count();
            for review in &task.reviews {
                let said = match review {
                    ReviewEvent::Reviewed {
                        reviewer, review, ..
                    } => {
                        let verdict = match review.verdict {
                            ReviewVerdict::Approve => "approved",
                            ReviewVerdict::RequestChanges => "asked for changes",
                            ReviewVerdict::Reject => "rejected it",
                        };
                        Some((reviewer.clone(), verdict))
                    }
                    ReviewEvent::Settled { judge, .. } => Some((judge.clone(), "signed off")),
                    ReviewEvent::TestsWritten { .. } => None,
                };
                if let Some(said) = said {
                    reviewers.retain(|(who, _)| who.agent != said.0.agent);
                    reviewers.push(said);
                }
            }
        }
        ((passed, total), reviewers)
    });
    let reviewed = (!reviewers.is_empty()).then(|| {
        reviewers
            .iter()
            .map(|(who, verdict)| {
                let cli = builtin_cli_name(&who.agent).unwrap_or(&who.agent);
                format!("{cli} {verdict}")
            })
            .collect::<Vec<_>>()
            .join(", ")
    });
    DraftShown {
        branch: link.branch.clone(),
        stat,
        checks,
        reviewed,
        login: link.login.clone(),
        title: link.title.clone(),
        body: String::new(),
        since: None,
    }
}

/// What the run did, in its builders' words: each merged task's last report.
fn summary_of(host: &Host) -> String {
    host.read(|state| {
        state
            .tasks
            .by_id
            .values()
            .filter(|task| task.state == TaskState::Merged)
            .filter_map(|task| task.reports.last())
            .map(|report| report.summary.trim().to_owned())
            .filter(|summary| !summary.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// Where the step that lands the run is, in its round.
fn running_step(host: &Host) -> Option<StepAt> {
    host.read(|state| {
        state
            .workflows
            .running()
            .last()
            .map(|(_, progress)| StepAt {
                step: progress.step.clone(),
                round: progress.round,
            })
    })
}

/// A task of the run, which the question names as where it comes from.
fn merged_task(host: &Host) -> Option<TaskId> {
    host.read(|state| {
        state
            .tasks
            .by_id
            .values()
            .find(|task| task.state == TaskState::Merged)
            .or_else(|| state.tasks.by_id.values().next())
            .map(|task| task.spec.id.clone())
    })
}

#[cfg(test)]
mod tests {
    use argonauts_conductor::gh::RepoName;

    use super::*;

    fn link() -> IssueLink {
        IssueLink {
            repo: RepoName::parse("test-user/limiter").unwrap(),
            number: 42,
            title: "Give each API key its own rate limit bucket".to_owned(),
            url: "https://github.com/test-user/limiter/issues/42".to_owned(),
            branch: "issue-42-per-key-limiter".to_owned(),
            default_branch: "main".to_owned(),
            login: "test-user".to_owned(),
            request: String::new(),
            writes_allowed: false,
            as_read: None,
            push_remote: None,
            fork_account: None,
        }
    }

    #[test]
    fn what_happened_to_the_issue_since_is_said_in_one_line() {
        let link = IssueLink {
            as_read: Some(IssueLink::mark(
                "Give each API key its own rate limit bucket",
                "All keys share one.",
            )),
            ..link()
        };
        let issue = |body: &str, open: bool| Issue {
            number: 42,
            title: link.title.clone(),
            body: body.to_owned(),
            open,
            locked: false,
            author: "test-user".to_owned(),
            association: argonauts_conductor::gh::Association::Member,
            url: link.url.clone(),
            is_pull: false,
        };
        assert_eq!(since(&link, &issue("All keys share one.", true)), None);
        assert_eq!(
            since(&link, &issue("All keys share one. Edited.", true)).as_deref(),
            Some("#42 was edited on GitHub since the run read it")
        );
        assert_eq!(
            since(&link, &issue("All keys share one.", false)).as_deref(),
            Some("#42 was closed on GitHub since the run read it")
        );
        // A link kept before the mark was says nothing of edits.
        let unmarked = IssueLink {
            as_read: None,
            ..link.clone()
        };
        assert_eq!(since(&unmarked, &issue("Anything.", true)), None);
    }

    fn shown() -> DraftShown {
        let mut shown = DraftShown {
            branch: "issue-42-per-key-limiter".to_owned(),
            stat: DiffStat {
                files_changed: 2,
                insertions: 30,
                deletions: 4,
            },
            checks: (5, 5),
            reviewed: Some("Codex approved, agy signed off".to_owned()),
            login: "test-user".to_owned(),
            title: "Give each API key its own rate limit bucket".to_owned(),
            body: String::new(),
            since: None,
        };
        shown.body = body(
            "Gives each X-Api-Key its own token bucket, so one busy key no longer slows the others.\n\
             Requests with no key share one anonymous bucket.",
            &shown,
            &link(),
            "Fixes #42",
        );
        shown
    }

    #[test]
    fn the_question_shows_the_branch_who_it_posts_as_and_the_folded_body() {
        assert_eq!(
            shown().text(false),
            "branch issue-42-per-key-limiter · 2 files +30 \u{2212}4 · checks 5/5 · Codex approved, agy signed off\n\
             It posts as you (gh: test-user) · nothing is posted on the issue itself\n\
             \n\
             title   Give each API key its own rate limit bucket\n\
             body    Gives each X-Api-Key its own token bucket, so one busy key no longer slows the others.\n\
             \x20       Requests with no key share one anonymous bucket.\n\
             \x20       … 5 more lines: the checks, who reviewed, and how it was made\n\
             \x20       Fixes #42"
        );
    }

    #[test]
    fn the_whole_body_shows_every_line() {
        let text = shown().text(true);
        assert!(text.contains("        Checks: 5 of 5 passed."), "{text}");
        assert!(text.contains("        Reviewed: Codex approved, agy signed off."));
        assert!(!text.contains("more lines"));
        assert!(text.ends_with("        Fixes #42"));
    }

    #[test]
    fn the_body_ends_with_the_line_that_names_the_issue() {
        let body = shown().body;
        assert!(body.starts_with("Gives each X-Api-Key"));
        assert!(body.ends_with("\n\nFixes #42"));
        assert!(body.contains("on the plan approved for #42."));
    }

    #[test]
    fn every_answer_is_the_issues_and_says_what_it_does() {
        let options = options();
        assert_eq!(options.len(), 4);
        assert!(options.iter().all(|option| {
            option.detail.is_some()
                && matches!(&option.action, AnswerAction::Choose { value } if value.starts_with("issue:"))
        }));
    }
}
