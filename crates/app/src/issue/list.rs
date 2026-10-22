//! `/issue`'s list: the repository's open issues, yours first. Yours are asked for on their
//! own, so a repository with more open issues than the list holds still lists every one of
//! yours, and the list says when it holds only the newest of the others.

use argonauts_conductor::gh::{Gh, GhError, ListedIssue, Whose};
use argonauts_core::app::{IssueRow, ViewData};

/// As many as `gh issue list` gives by default, which the picker scrolls through.
const LISTED: usize = 30;

/// The list as the screen draws it, or why there is none.
pub(crate) async fn issues_view(gh: Result<Gh, GhError>) -> ViewData {
    match listed(gh).await {
        Ok(Listed { repo, issues, open }) => ViewData::Issues {
            repo,
            issues,
            problem: None,
            open,
        },
        Err(err) => ViewData::Issues {
            repo: String::new(),
            issues: Vec::new(),
            problem: Some(format!("{err}.")),
            open: None,
        },
    }
}

struct Listed {
    repo: String,
    issues: Vec<IssueRow>,
    /// How many are open, when that's more than the list holds.
    open: Option<usize>,
}

async fn listed(gh: Result<Gh, GhError>) -> Result<Listed, GhError> {
    let gh = gh?;
    let you = gh.login().await?;
    let repo = gh.repo().await?;
    let mut issues = gh
        .open_issues(&repo.name, LISTED, Whose::OpenedByYou)
        .await?;
    issues.extend(
        gh.open_issues(&repo.name, LISTED, Whose::AssignedToYou)
            .await?,
    );
    let newest = gh.open_issues(&repo.name, LISTED, Whose::Anyone).await?;
    let cut = newest.len() >= LISTED;
    issues.extend(newest);
    let rows = rows(issues, &you);
    // A count that can't be had leaves the list as it is.
    let open = match cut {
        true => gh.open_issue_count(&repo.name).await.ok(),
        false => None,
    }
    .filter(|open| *open > rows.len());
    Ok(Listed {
        repo: repo.name.to_string(),
        issues: rows,
        open,
    })
}

/// Yours first, then the others, each group most recently updated first, each issue once. An
/// issue is yours when you opened it or it's assigned to you.
pub(crate) fn rows(open: Vec<ListedIssue>, you: &str) -> Vec<IssueRow> {
    let is_you = |login: &str| login.eq_ignore_ascii_case(you);
    let mut seen = std::collections::BTreeSet::new();
    let mut open: Vec<ListedIssue> = open
        .into_iter()
        .filter(|issue| seen.insert(issue.number))
        .collect();
    // Stable, so issues updated together keep GitHub's order.
    open.sort_by_key(|issue| std::cmp::Reverse(issue.updated));
    let (yours, others): (Vec<IssueRow>, Vec<IssueRow>) = open
        .into_iter()
        .map(|issue| IssueRow {
            yours: is_you(&issue.author) || issue.assignees.iter().any(|who| is_you(who)),
            number: issue.number,
            title: issue.title,
            labels: issue.labels,
            updated: issue.updated,
        })
        .partition(|row| row.yours);
    yours.into_iter().chain(others).collect()
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    fn listed(number: u64, author: &str, assignees: &[&str]) -> ListedIssue {
        ListedIssue {
            number,
            title: format!("Issue {number}"),
            labels: Vec::new(),
            updated: datetime!(2026-10-20 10:00 UTC),
            author: author.to_owned(),
            assignees: assignees.iter().map(|who| (*who).to_owned()).collect(),
        }
    }

    #[test]
    fn yours_come_first_in_githubs_order() {
        let open = vec![
            listed(51, "someone", &[]),
            listed(42, "Test-User", &[]),
            listed(48, "someone", &[]),
            listed(45, "someone", &["test-user"]),
        ];
        let rows = rows(open, "test-user");
        let order: Vec<(u64, bool)> = rows.iter().map(|row| (row.number, row.yours)).collect();
        assert_eq!(order, [(42, true), (45, true), (51, false), (48, false)]);
    }

    /// A repository with more open issues than the list holds: yours come from their own ask,
    /// older ones too, each once, and the list says how many are open.
    #[tokio::test]
    async fn yours_are_listed_past_the_newest_and_the_list_says_its_cut() {
        use argonauts_testkit::fake_gh::{FakeGh, GhIssue, GhRepo, GhWorld};

        let mut issues: Vec<GhIssue> = (1..=40)
            .rev()
            .map(|number| GhIssue {
                number,
                title: format!("Issue {number}"),
                author: "someone".to_owned(),
                updated: format!("2026-10-{:02}T10:00:00Z", 1 + (number % 28)),
                ..GhIssue::default()
            })
            .collect();
        // The oldest is yours, the second oldest is assigned to you.
        issues[39].author = "test-user".to_owned();
        issues[38].assignees = vec!["test-user".to_owned()];
        let world = GhWorld {
            login: Some("test-user".to_owned()),
            repo: Some(GhRepo {
                name: "test-user/limiter".to_owned(),
                ..GhRepo::default()
            }),
            issues,
            ..GhWorld::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeGh::install(dir.path(), &world).unwrap();
        let gh = Gh::at(fake.program(), dir.path()).with_env(fake.envs());
        let ViewData::Issues { issues, open, .. } = issues_view(Ok(gh)).await else {
            unreachable!()
        };
        let yours: Vec<u64> = issues
            .iter()
            .filter(|row| row.yours)
            .map(|row| row.number)
            .collect();
        assert_eq!(yours.len(), 2);
        assert!(yours.contains(&1) && yours.contains(&2), "{yours:?}");
        assert_eq!(issues.len(), 32);
        assert_eq!(open, Some(40));
    }

    #[tokio::test]
    async fn without_gh_the_list_says_why() {
        let ViewData::Issues {
            problem, issues, ..
        } = issues_view(Err(GhError::NotSignedIn)).await
        else {
            unreachable!()
        };
        assert!(issues.is_empty());
        assert_eq!(
            problem.as_deref(),
            Some("gh isn't signed in. Sign in with gh auth login, then try again.")
        );
    }
}
