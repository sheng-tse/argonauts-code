//! `/issue`: a GitHub issue to work on, named by its number in this repository or by its
//! address.

use serde::{Deserialize, Serialize};

use super::{InputError, SlashCommand, bad};

/// An issue `/issue` names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueRef {
    pub number: u64,
    /// The issue's address when you gave one, without its query or fragment. `gh` reads it as
    /// it is, so it may name another repository than this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// `--all-comments`: everyone's comments are read, not only those of the people who may
    /// write to the repository and yours.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all_comments: bool,
}

const USAGE: &str =
    "use /issue, /issue 42, or /issue and the issue's address, with --all-comments after it";
const ALL_COMMENTS: &str = "--all-comments";

/// `/issue` lists the open issues, `/issue 42` or `/issue #42` starts one of this repository's,
/// and `/issue https://github.com/team/app/issues/42` one by its address. `--all-comments`
/// after the issue reads everyone's comments.
pub(super) fn parse_issue(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let words: Vec<&str> = args.split_whitespace().collect();
    let (named, all_comments) = match words.as_slice() {
        [] => return Ok(SlashCommand::Issue { issue: None }),
        [named] => (*named, false),
        [named, ALL_COMMENTS] => (*named, true),
        _ => return Err(bad(name, USAGE)),
    };
    let mut issue = number(named.strip_prefix('#').unwrap_or(named))
        .map(|number| IssueRef {
            number,
            url: None,
            all_comments: false,
        })
        .or_else(|| from_address(named))
        .ok_or_else(|| bad(name, USAGE))?;
    issue.all_comments = all_comments;
    Ok(SlashCommand::Issue { issue: Some(issue) })
}

fn number(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().filter(|number| *number > 0)
}

/// `https://<host>/<account>/<repo>/issues/<number>`, on any host, since GitHub Enterprise
/// has its own.
fn from_address(text: &str) -> Option<IssueRef> {
    let (scheme, rest) = ["https://", "http://"]
        .into_iter()
        .find_map(|scheme| text.strip_prefix(scheme).map(|rest| (scheme, rest)))?;
    let path = rest.split(['?', '#']).next().unwrap_or(rest);
    let parts: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    let [host, account, repo, "issues", number_text] = parts.as_slice() else {
        return None;
    };
    if [host, account, repo].iter().any(|part| part.is_empty()) {
        return None;
    }
    let number = number(number_text)?;
    Some(IssueRef {
        number,
        url: Some(format!("{scheme}{host}/{account}/{repo}/issues/{number}")),
        all_comments: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(args: &str) -> Result<Option<IssueRef>, InputError> {
        parse_issue("issue", args).map(|command| match command {
            SlashCommand::Issue { issue } => issue,
            other => panic!("{other:?}"),
        })
    }

    #[test]
    fn no_issue_lists_them_and_a_number_or_address_names_one() {
        assert_eq!(issue("").unwrap(), None);
        let by_number = IssueRef {
            number: 42,
            url: None,
            all_comments: false,
        };
        assert_eq!(issue("42").unwrap(), Some(by_number.clone()));
        assert_eq!(issue(" #42 ").unwrap(), Some(by_number));
        let address = "https://github.com/test-user/rate-limiter-app/issues/42";
        assert_eq!(
            issue(&format!("{address}#issuecomment-7")).unwrap(),
            Some(IssueRef {
                number: 42,
                url: Some(address.to_owned()),
                all_comments: false,
            })
        );
        let enterprise = issue("https://git.example.com/team/app/issues/7/").unwrap();
        assert_eq!(
            enterprise.and_then(|issue| issue.url).as_deref(),
            Some("https://git.example.com/team/app/issues/7")
        );
    }

    #[test]
    fn all_comments_after_the_issue_reads_everyones() {
        let issue = issue("42 --all-comments").unwrap().unwrap();
        assert_eq!((issue.number, issue.all_comments), (42, true));
        let address = "https://github.com/team/app/issues/7";
        let issue = issue_at(&format!("{address} --all-comments"));
        assert_eq!(issue.url.as_deref(), Some(address));
        assert!(issue.all_comments);
        assert!(!issue_at("42").all_comments);
    }

    fn issue_at(args: &str) -> IssueRef {
        issue(args).unwrap().expect("an issue")
    }

    #[test]
    fn anything_else_says_how_to_name_an_issue() {
        for args in [
            "0",
            "-3",
            "42 43",
            "--all-comments",
            "--all-comments 42",
            "42 --all-comments --all-comments",
            "forty-two",
            "#",
            "https://github.com/team/app/pull/42",
            "https://github.com/team/issues/42",
            "github.com/team/app/issues/42",
        ] {
            assert!(
                matches!(issue(args), Err(InputError::BadArguments { .. })),
                "{args}"
            );
        }
    }
}
