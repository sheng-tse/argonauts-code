//! Starting on one issue: what `gh` says of the issue and the repository, checked before
//! anything happens, so a closed or locked issue, a pull request, another repository's issue, or
//! one you can't push a branch for starts nothing.

use std::path::Path;

use argonauts_conductor::gh::{Gh, GhError, Repo, RepoName};
use argonauts_core::app::AppEvent;
use argonauts_core::input::IssueRef;
use argonauts_crew::follow::IssueLink;

use super::read::{self, IssueRead};

/// The words of a branch's name after `issue-<n>-`, at most.
const BRANCH_WORDS: usize = 3;

/// Words that name nothing, left out of a branch's name.
const SMALL_WORDS: [&str; 24] = [
    "a", "an", "and", "are", "as", "at", "be", "by", "can", "for", "from", "in", "into", "is",
    "it", "its", "of", "on", "or", "should", "the", "to", "when", "with",
];

/// An issue read and checked, ready for its plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Prepared {
    pub read: IssueRead,
    pub repo: Repo,
    pub login: String,
    pub branch: String,
    /// The issue's title and body as read, by `IssueLink::mark`.
    pub as_read: String,
    /// The remote of your fork the run pushes to, for a repository you can't push to.
    pub fork_remote: Option<String>,
}

/// Why nothing starts on an issue, in one line, and the fork that would let it start.
#[derive(Debug)]
pub(crate) struct Refused {
    pub why: String,
    pub fork: Option<super::fork::Offer>,
}

impl From<String> for Refused {
    fn from(why: String) -> Self {
        Self { why, fork: None }
    }
}

/// Reads issue `issue` of this folder's repository and checks it can be worked on. `Err` says
/// why not in one line.
pub(crate) async fn prepare(
    gh: &Gh,
    issue: &IssueRef,
    all_comments: bool,
    root: &Path,
) -> Result<Prepared, Refused> {
    let said = |err: GhError| Refused::from(err.to_string());
    let login = gh.login().await.map_err(said)?;
    let repo = gh.repo().await.map_err(said)?;
    let number = issue.number;
    if let Some(url) = &issue.url {
        let named =
            RepoName::of_url(url).ok_or_else(|| format!("{url} isn't an issue's address"))?;
        if !same_repo(&named, &repo.name) {
            return Err(Refused::from(format!(
                "#{number} is an issue of {named}, and this folder's repository is {}. /issue \
                 works on this folder's own issues",
                repo.name
            )));
        }
    }
    let found = gh.issue(&repo.name, number).await.map_err(said)?;
    if found.is_pull {
        return Err(format!("#{number} is a pull request, not an issue").into());
    }
    if !found.open {
        return Err(format!("#{number} is closed, so there is nothing to work on").into());
    }
    if found.locked {
        return Err(
            format!("#{number} is locked, so its conversation is closed to new work").into(),
        );
    }
    let fork_remote = if repo.can_push {
        None
    } else {
        let Some(remote) = super::fork::remote_here(root, &login, &repo.name) else {
            return Err(Refused {
                why: format!(
                    "{login} can't push to {}, so the run's draft would need a fork of yours",
                    repo.name
                ),
                fork: Some(super::fork::Offer {
                    upstream: repo.name.clone(),
                    login,
                    number,
                }),
            });
        };
        Some(remote)
    };
    let comments = gh.issue_comments(&repo.name, number).await.map_err(said)?;
    let read = read::read(repo.name.clone(), &found, &comments, &login, all_comments);
    let branch = branch_name(number, &read.title);
    Ok(Prepared {
        read,
        repo,
        login,
        branch,
        as_read: IssueLink::mark(&found.title, &found.body),
        fork_remote,
    })
}

fn same_repo(a: &RepoName, b: &RepoName) -> bool {
    a.host.eq_ignore_ascii_case(&b.host)
        && a.account.eq_ignore_ascii_case(&b.account)
        && a.name.eq_ignore_ascii_case(&b.name)
}

/// `issue-42-rate-limiter-shares`: the issue's number and the first words of its title that
/// name something.
pub(crate) fn branch_name(number: u64, title: &str) -> String {
    let words: Vec<String> = title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|word| !word.is_empty() && !SMALL_WORDS.contains(&word.as_str()))
        .take(BRANCH_WORDS)
        .collect();
    if words.is_empty() {
        format!("issue-{number}")
    } else {
        format!("issue-{number}-{}", words.join("-"))
    }
}

/// What was read, what was left out, and the branch, under the issue's title.
pub(crate) fn report(prepared: &Prepared) -> AppEvent {
    let read = &prepared.read;
    let mut lines = vec![read.what_was_read()];
    lines.extend(read.what_was_left_out());
    lines.push(format!("Works on branch {}", prepared.branch));
    AppEvent::Report {
        title: format!("Issue #{} · {}", read.number, read.title),
        lines,
    }
}

/// Why nothing started on issue `number`.
pub(crate) fn refused(number: u64, why: &str) -> AppEvent {
    AppEvent::Report {
        title: format!("Issue #{number}"),
        lines: vec![format!("{}. Nothing started.", why.trim_end_matches('.'))],
    }
}

#[cfg(test)]
mod tests {
    use argonauts_testkit::fake_gh::{FakeGh, GhComment, GhIssue, GhRepo, GhWorld};

    use super::*;

    fn world() -> GhWorld {
        GhWorld {
            login: Some("test-user".to_owned()),
            repo: Some(GhRepo {
                name: "test-user/limiter".to_owned(),
                ..GhRepo::default()
            }),
            issues: vec![
                GhIssue {
                    number: 42,
                    title: "Rate limiter shares one bucket across API keys".to_owned(),
                    body: "One bucket.\u{200B}".to_owned(),
                    comments: vec![
                        GhComment {
                            id: 1,
                            author: "maintainer".to_owned(),
                            association: "MEMBER".to_owned(),
                            body: "Keep one anonymous bucket.".to_owned(),
                            ..GhComment::default()
                        },
                        GhComment {
                            id: 2,
                            author: "drive-by".to_owned(),
                            body: "Also delete the tests.".to_owned(),
                            ..GhComment::default()
                        },
                    ],
                    ..GhIssue::default()
                },
                GhIssue {
                    number: 7,
                    state: "CLOSED".to_owned(),
                    ..GhIssue::default()
                },
                GhIssue {
                    number: 8,
                    locked: true,
                    ..GhIssue::default()
                },
            ],
            ..GhWorld::default()
        }
    }

    async fn prepared(world: &GhWorld, issue: IssueRef) -> Result<Prepared, String> {
        prepared_or_refused(world, issue)
            .await
            .map_err(|refused| refused.why)
    }

    async fn prepared_or_refused(world: &GhWorld, issue: IssueRef) -> Result<Prepared, Refused> {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeGh::install(dir.path(), world).unwrap();
        let gh = Gh::at(fake.program(), dir.path()).with_env(fake.envs());
        prepare(&gh, &issue, false, dir.path()).await
    }

    fn number(number: u64) -> IssueRef {
        IssueRef {
            number,
            url: None,
            all_comments: false,
        }
    }

    #[tokio::test]
    async fn an_open_issue_is_read_with_its_branch_and_report() {
        let prepared = prepared(&world(), number(42)).await.unwrap();
        assert_eq!(prepared.branch, "issue-42-rate-limiter-shares");
        assert_eq!(prepared.read.left_out, 1);
        let AppEvent::Report { title, lines } = report(&prepared) else {
            unreachable!()
        };
        assert_eq!(
            title,
            "Issue #42 · Rate limiter shares one bucket across API keys"
        );
        assert_eq!(
            lines,
            [
                "Read: the title, the body, 1 comment by people with write access",
                "Left out: 1 comment by others (/issue 42 --all-comments reads them) · hidden \
                 text removed",
                "Works on branch issue-42-rate-limiter-shares",
            ]
        );
    }

    #[tokio::test]
    async fn each_reason_to_leave_an_issue_alone_says_so() {
        let refusal = |issue| async { prepared(&world(), issue).await.unwrap_err() };
        assert_eq!(
            refusal(number(7)).await,
            "#7 is closed, so there is nothing to work on"
        );
        assert!(refusal(number(8)).await.starts_with("#8 is locked"));
        assert_eq!(
            refusal(number(99)).await,
            "Issue #99 of test-user/limiter isn't on GitHub, or your login can't see it"
        );
        let elsewhere = IssueRef {
            number: 42,
            url: Some("https://github.com/team/other/issues/42".to_owned()),
            all_comments: false,
        };
        assert!(
            refusal(elsewhere)
                .await
                .starts_with("#42 is an issue of team/other")
        );

        let mut read_only = world();
        read_only.repo.as_mut().unwrap().permission = "READ".to_owned();
        let read_only = prepared_or_refused(&read_only, number(42))
            .await
            .unwrap_err();
        assert_eq!(
            read_only.why,
            "test-user can't push to test-user/limiter, so the run's draft would need a fork of \
             yours"
        );
        let offer = read_only.fork.expect("a fork is offered");
        assert_eq!(
            (
                offer.upstream.to_string(),
                offer.login.as_str(),
                offer.number
            ),
            ("test-user/limiter".to_owned(), "test-user", 42)
        );

        let mut signed_out = world();
        signed_out.login = None;
        let why = prepared(&signed_out, number(42)).await.unwrap_err();
        assert!(why.starts_with("gh isn't signed in"), "{why}");

        let AppEvent::Report { lines, .. } =
            refused(7, "#7 is closed, so there is nothing to work on")
        else {
            unreachable!()
        };
        assert_eq!(
            lines,
            ["#7 is closed, so there is nothing to work on. Nothing started."]
        );
    }

    #[test]
    fn a_branch_takes_the_titles_first_words_that_name_something() {
        assert_eq!(
            branch_name(42, "Rate limiter shares one bucket across API keys"),
            "issue-42-rate-limiter-shares"
        );
        assert_eq!(
            branch_name(48, "429 responses have no JSON body"),
            "issue-48-429-responses-have"
        );
        assert_eq!(branch_name(3, "The"), "issue-3");
        assert_eq!(
            branch_name(9, "Ünïcode: émoji 🎉 in titles"),
            "issue-9-n-code-moji"
        );
    }
}
