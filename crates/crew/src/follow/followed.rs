//! A pull request Argonauts Code opened, kept beside the repository's runs so it's followed
//! while the app is open and picked up again at the next start, until it merges or closes.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use argonauts_conductor::gh::RepoName;
use argonauts_core::RunId;
use serde::{Deserialize, Serialize};

use super::IssueLink;
use super::watch::ToAnswer;
use crate::engine::host::Host;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Followed {
    pub run: RunId,
    pub repo: RepoName,
    pub number: u64,
    pub url: String,
    /// The branch on the remote the pull request opened from, where fixes are pushed.
    pub branch: String,
    /// That remote, when it isn't `origin`, as for a fork.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    /// The issue it names, when `/issue` started its run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<u64>,
    /// Rounds of fixes so far, which `pr.max_rounds` bounds.
    #[serde(default)]
    pub rounds: u32,
    /// Rounds you allowed beyond `pr.max_rounds`.
    #[serde(default)]
    pub more_rounds: u32,
    /// Comments and replies go without asking for the rest of the run.
    #[serde(default)]
    pub writes_allowed: bool,
    /// Merged or closed on GitHub, or you stopped following it. Nothing more is done.
    #[serde(default)]
    pub ended: bool,
    /// The commit Argonauts Code last pushed to the branch, where the next fix starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// The commit the pull request was at when the newest round began, so a check that failed
    /// there isn't fixed twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worked_at: Option<String>,
    /// The comments a round answered, with a commit or a reply.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answered: Vec<String>,
    /// The round the run's engine works next, once the app has seen it's due.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<Due>,
    /// Why the follow-up waits for you, when a round couldn't go on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<String>,
    /// Replies a round wrote, shown to you before any goes to GitHub.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replies: Vec<Reply>,
    /// What you said "not now" to, by its words in the bottom list, so it isn't asked again
    /// until GitHub says something else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_aside: Option<String>,
    /// The issue was told of the draft, when `issue.comment` asks for that.
    #[serde(default)]
    pub issue_told: bool,
    /// How its run's row reads, from the newest look at GitHub: `#57 · CI running`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub said: Option<String>,
    /// The line under its run's header in the workflow view, from the same look.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading: Option<Heading>,
}

/// The line under a followed run's header: what GitHub says of its pull request, drawn in the
/// line's colour, then what Argonauts Code does about it, dim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heading {
    /// `#57 · test (ubuntu) failed`.
    pub said: String,
    /// `back to its builder, round 1 of 3 · the fix is pushed once reviewed`, or nothing where
    /// what GitHub says is all there is.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub then: String,
}

/// A round of fixes the run's engine works: the task's builder, resumed, then its reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Due {
    pub work: Work,
    /// The commit the builder's fix ended at, once it was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed: Option<String>,
    /// The fix's reviewer approved it.
    #[serde(default)]
    pub reviewed: bool,
    /// The replies the builder wrote, until the round ends and they wait for you.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replies: Vec<Reply>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Work {
    /// A check failed, with the failing part of its log.
    Check { check: String, log: String },
    /// Comments to answer, each with a commit or a reply.
    Comments { comments: Vec<ToAnswer> },
}

impl Work {
    /// What its agents' labels name: `fix:ci-round-1`.
    pub fn what(&self) -> &'static str {
        match self {
            Work::Check { .. } => "ci",
            Work::Comments { .. } => "comments",
        }
    }
}

/// A reply a round's builder wrote to a comment it didn't change the code for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    /// Who wrote the comment.
    pub to: String,
    /// The line comment it goes under, or `None` for the pull request's conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<u64>,
    pub body: String,
}

impl Followed {
    /// A pull request Argonauts Code opened for run `run`, with nothing done on it yet.
    pub fn new(run: RunId, repo: RepoName, number: u64, url: String, branch: String) -> Self {
        Self {
            run,
            repo,
            number,
            url,
            branch,
            remote: None,
            issue: None,
            rounds: 0,
            more_rounds: 0,
            writes_allowed: false,
            ended: false,
            head: None,
            worked_at: None,
            answered: Vec::new(),
            due: None,
            held: None,
            replies: Vec::new(),
            set_aside: None,
            issue_told: false,
            said: None,
            heading: None,
        }
    }

    /// The draft just opened for `link`'s issue, at `url`, which ends in its number, from the
    /// commit `head`.
    pub fn opened(host: &Host, link: &IssueLink, url: &str, head: Option<String>) -> Option<Self> {
        let number = number_of(url)?;
        Some(Self {
            issue: Some(link.number),
            writes_allowed: link.writes_allowed,
            remote: link.push_remote.clone(),
            head,
            ..Self::new(
                host.run_id().clone(),
                link.repo.clone(),
                number,
                url.to_owned(),
                link.branch.clone(),
            )
        })
    }

    /// The rounds it may have: `pr.max_rounds`, and those you allowed beyond it.
    pub fn rounds_allowed(&self, max_rounds: u32) -> u32 {
        max_rounds.saturating_add(self.more_rounds)
    }

    /// `<state>/pull_requests/`.
    pub fn dir(state_dir: &Path) -> PathBuf {
        state_dir.join("pull_requests")
    }

    fn path(&self, state_dir: &Path) -> PathBuf {
        Self::dir(state_dir).join(format!("{}.json", self.number))
    }

    pub fn save(&self, state_dir: &Path) -> io::Result<()> {
        fs::create_dir_all(Self::dir(state_dir))?;
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        let path = self.path(state_dir);
        let partial = path.with_extension("json.partial");
        fs::write(&partial, text)?;
        fs::rename(partial, path)
    }

    /// Every pull request still followed, by number.
    pub fn open(state_dir: &Path) -> Vec<Self> {
        let Ok(entries) = fs::read_dir(Self::dir(state_dir)) else {
            return Vec::new();
        };
        let mut all: Vec<Self> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|entry| fs::read_to_string(entry.path()).ok())
            .filter_map(|text| serde_json::from_str::<Self>(&text).ok())
            .filter(|followed| !followed.ended)
            .collect();
        all.sort_by_key(|followed| followed.number);
        all
    }

    /// The pull request of run `run`, if one is followed.
    pub fn of_run(state_dir: &Path, run: &RunId) -> Option<Self> {
        Self::open(state_dir)
            .into_iter()
            .find(|followed| &followed.run == run)
    }
}

/// The number a pull request's address ends in: `https://github.com/team/app/pull/57`.
fn number_of(url: &str) -> Option<u64> {
    let (_, number) = url.trim_end_matches('/').rsplit_once("/pull/")?;
    number.split(['/', '#', '?']).next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn followed(number: u64, run: u32, ended: bool) -> Followed {
        Followed {
            issue: Some(42),
            ended,
            ..Followed::new(
                RunId::new(format!("run-0{run}")).unwrap(),
                RepoName::parse("test-user/limiter").unwrap(),
                number,
                format!("https://github.com/test-user/limiter/pull/{number}"),
                "issue-42-per-key-limiter".to_owned(),
            )
        }
    }

    #[test]
    fn a_pull_requests_number_is_the_end_of_its_address() {
        assert_eq!(number_of("https://github.com/team/app/pull/57"), Some(57));
        assert_eq!(
            number_of("https://github.com/team/app/pull/57/files"),
            Some(57)
        );
        assert_eq!(number_of("https://github.com/team/app/issues/57"), None);
    }

    #[test]
    fn only_those_still_open_are_followed() {
        let state = tempfile::tempdir().unwrap();
        followed(57, 1, false).save(state.path()).unwrap();
        followed(58, 2, true).save(state.path()).unwrap();
        followed(51, 3, false).save(state.path()).unwrap();
        let numbers: Vec<u64> = Followed::open(state.path())
            .iter()
            .map(|followed| followed.number)
            .collect();
        assert_eq!(numbers, [51, 57]);
        assert_eq!(
            Followed::of_run(state.path(), &RunId::new("run-01").unwrap()).map(|f| f.number),
            Some(57)
        );
        assert!(Followed::of_run(state.path(), &RunId::new("run-02").unwrap()).is_none());
    }
}
