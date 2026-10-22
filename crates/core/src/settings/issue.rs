//! Working on a GitHub issue with `/issue` (`[issue]`). Everything Argonauts Code writes on an
//! issue posts as you, so by default it writes nothing there: the draft pull request names the
//! issue, and the issue links back to it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct IssueSettings {
    /// When to comment on the issue: `none`, `start` when work on it starts, `draft` when its
    /// draft pull request opens, or `both`. Each comment is shown to you first
    pub comment: IssueComment,
    /// Label the issue while a run works on it
    pub labels: bool,
    /// Assign the issue to you when a run starts on it
    pub assign_me: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IssueComment {
    #[default]
    None,
    Start,
    Draft,
    Both,
}

impl IssueComment {
    /// Whether a comment goes on the issue when work on it starts.
    pub fn at_start(self) -> bool {
        matches!(self, IssueComment::Start | IssueComment::Both)
    }

    /// Whether a comment goes on the issue when its draft pull request opens.
    pub fn at_draft(self) -> bool {
        matches!(self, IssueComment::Draft | IssueComment::Both)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_written_on_the_issue_until_you_ask() {
        let issue = IssueSettings::default();
        assert_eq!(issue.comment, IssueComment::None);
        assert!(!issue.comment.at_start() && !issue.comment.at_draft());
        assert!(!issue.labels);
        assert!(!issue.assign_me);
        let both: IssueSettings = toml::from_str("comment = \"both\"\nassign_me = true\n").unwrap();
        assert!(both.comment.at_start() && both.comment.at_draft());
        assert!(both.assign_me);
        assert!(!both.labels);
        let draft: IssueSettings = toml::from_str("comment = \"draft\"").unwrap();
        assert!(!draft.comment.at_start() && draft.comment.at_draft());
    }
}
