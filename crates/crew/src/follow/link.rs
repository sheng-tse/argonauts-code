//! A run that works on a GitHub issue. `/issue` keeps one beside the repository's runs before
//! the run starts, and the run finds it by its request, so its end opens a draft pull request
//! that names the issue rather than merging.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use argonauts_conductor::gh::RepoName;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueLink {
    pub repo: RepoName,
    pub number: u64,
    pub title: String,
    pub url: String,
    /// The branch the draft opens from on the remote: `issue-<n>-<words>`.
    pub branch: String,
    /// The repository's default branch, where `Fixes #n` closes the issue on merge.
    pub default_branch: String,
    /// Who `gh` posts as.
    pub login: String,
    /// The run's request, word for word, which finds this link once the run has started.
    pub request: String,
    /// "Allow GitHub writes for the rest of this run" was chosen, so later writes go
    /// without asking.
    #[serde(default)]
    pub writes_allowed: bool,
    /// The issue's title and body as `/issue` read them, by [`IssueLink::mark`], so the draft
    /// can say the issue was edited since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_read: Option<String>,
    /// The remote the branch is pushed to, when it isn't `origin`: a fork of yours, for a
    /// repository you can't push to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_remote: Option<String>,
    /// The fork's account, which the draft names its branch by: `you:issue-42-…`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_account: Option<String>,
}

impl IssueLink {
    /// What tells an issue's title and body from any edit of them.
    pub fn mark(title: &str, body: &str) -> String {
        argonauts_core::hash::sha256_hex(format!("{title}\n{body}").as_bytes())
    }

    /// `<state>/issues/`, where every link of a repository is kept.
    pub fn dir(state_dir: &Path) -> PathBuf {
        state_dir.join("issues")
    }

    fn path(&self, state_dir: &Path) -> PathBuf {
        Self::dir(state_dir).join(format!("{}.json", self.number))
    }

    /// Keeps it, in place of an earlier link to the same issue.
    pub fn save(&self, state_dir: &Path) -> io::Result<()> {
        fs::create_dir_all(Self::dir(state_dir))?;
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        let path = self.path(state_dir);
        let partial = path.with_extension("json.partial");
        fs::write(&partial, text)?;
        fs::rename(partial, path)
    }

    /// The link of the run whose request is `request`, if `/issue` started it.
    pub fn for_request(state_dir: &Path, request: &str) -> Option<Self> {
        let entries = fs::read_dir(Self::dir(state_dir)).ok()?;
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|entry| fs::read_to_string(entry.path()).ok())
            .filter_map(|text| serde_json::from_str::<Self>(&text).ok())
            .find(|link| link.request == request)
    }

    /// `Fixes #42`, which closes the issue when the draft merges into the default branch, or
    /// `Refs #42` against any other base, where GitHub ignores the keyword.
    pub fn closing_line(&self, base: &str) -> String {
        if base == self.default_branch {
            format!("Fixes #{}", self.number)
        } else {
            format!("Refs #{}", self.number)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(number: u64, request: &str) -> IssueLink {
        IssueLink {
            repo: RepoName::parse("test-user/limiter").unwrap(),
            number,
            title: "Rate limiter shares one bucket across API keys".to_owned(),
            url: format!("https://github.com/test-user/limiter/issues/{number}"),
            branch: format!("issue-{number}-rate-limiter-shares"),
            default_branch: "main".to_owned(),
            login: "test-user".to_owned(),
            request: request.to_owned(),
            writes_allowed: false,
            as_read: None,
            push_remote: None,
            fork_account: None,
        }
    }

    #[test]
    fn a_run_finds_its_link_by_its_request() {
        let state = tempfile::tempdir().unwrap();
        link(42, "work on 42").save(state.path()).unwrap();
        link(45, "work on 45").save(state.path()).unwrap();
        let found = IssueLink::for_request(state.path(), "work on 45").unwrap();
        assert_eq!(found.number, 45);
        assert!(IssueLink::for_request(state.path(), "something else").is_none());
        // A second start on the same issue takes the first one's place.
        link(42, "work on 42 again").save(state.path()).unwrap();
        assert!(IssueLink::for_request(state.path(), "work on 42").is_none());
    }

    #[test]
    fn the_closing_line_names_the_issue_as_github_reads_it() {
        let link = link(42, "");
        assert_eq!(link.closing_line("main"), "Fixes #42");
        assert_eq!(link.closing_line("release"), "Refs #42");
    }
}
