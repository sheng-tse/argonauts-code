//! Every call Argonauts Code makes to GitHub, through the `gh` you signed in with. Only this code
//! runs `gh`, and always with fixed argument vectors: agents in their worktrees get no token and
//! can't read `gh`'s login, so nothing a model writes reaches GitHub except through here.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use time::OffsetDateTime;

use crate::boards::plan_usage::find_unsandboxed;

/// A hung network never holds a screen longer than this.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// So `gh` never prompts, pages, colours or offers an update in what Argonauts Code reads.
const QUIET: [(&str, &str); 5] = [
    ("GH_PROMPT_DISABLED", "1"),
    ("GH_NO_UPDATE_NOTIFIER", "1"),
    ("GH_PAGER", "cat"),
    ("NO_COLOR", "1"),
    ("GH_SPINNER_DISABLED", "1"),
];

/// gh 2.x exits 4 when no one is signed in.
const SIGNED_OUT_EXIT: i32 = 4;

/// `gh pr checks` exits 1 once a check failed and 8 while one is pending, with its list printed
/// either way.
const CHECKS_EXITS: [i32; 2] = [1, 8];

/// Why a call to GitHub didn't give its answer, each said in one line.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GhError {
    #[error(
        "gh isn't installed. Install GitHub's CLI from cli.github.com, then sign in with gh auth login"
    )]
    NotInstalled,
    #[error("gh isn't signed in. Sign in with gh auth login, then try again")]
    NotSignedIn,
    #[error("this folder has no GitHub remote, so there is no repository to work with")]
    NoRemote,
    #[error("{what} isn't on GitHub, or your login can't see it")]
    NotFound { what: String },
    #[error("gh {command} failed: {said}")]
    Failed { command: String, said: String },
    #[error("gh {command} gave an answer Argonauts Code can't read: {why}")]
    Unreadable { command: String, why: String },
}

/// A repository, as `account/name` on its host.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RepoName {
    pub host: String,
    /// The user or organization it belongs to.
    pub account: String,
    pub name: String,
}

impl RepoName {
    pub const GITHUB: &'static str = "github.com";

    /// `account/name` on github.com.
    pub fn parse(full: &str) -> Option<Self> {
        let (account, name) = full.trim().split_once('/')?;
        let plain = |part: &str| !part.is_empty() && !part.contains(['/', ' ']);
        (plain(account) && plain(name)).then(|| Self {
            host: Self::GITHUB.to_owned(),
            account: account.to_owned(),
            name: name.to_owned(),
        })
    }

    /// The repository of `https://<host>/<account>/<name>/issues/<n>`, or of a pull request's.
    pub fn of_url(url: &str) -> Option<Self> {
        let rest = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))?;
        let mut parts = rest.split('/');
        let (host, account, name) = (parts.next()?, parts.next()?, parts.next()?);
        let mut repo = Self::parse(&format!("{account}/{name}"))?;
        host.clone_into(&mut repo.host);
        Some(repo)
    }

    pub fn on_github(&self) -> bool {
        self.host == Self::GITHUB
    }

    pub fn url(&self) -> String {
        format!("https://{}/{}/{}", self.host, self.account, self.name)
    }

    /// What `--repo` takes: `account/name`, with the host first when it isn't github.com.
    fn flag(&self) -> String {
        if self.on_github() {
            self.to_string()
        } else {
            format!("{}/{self}", self.host)
        }
    }
}

impl fmt::Display for RepoName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.account, self.name)
    }
}

/// The repository the folder's remote names, as your login sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub name: RepoName,
    pub default_branch: String,
    /// Your login may push branches to it. Without that, a pull request needs a fork.
    pub can_push: bool,
}

/// One open issue, as `gh issue list` gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedIssue {
    pub number: u64,
    pub title: String,
    pub labels: Vec<String>,
    pub updated: OffsetDateTime,
    pub author: String,
    pub assignees: Vec<String>,
}

/// One issue in full, as GitHub's REST API gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub open: bool,
    pub locked: bool,
    pub author: String,
    pub association: Association,
    pub url: String,
    /// GitHub lists pull requests among issues; `/issue` works only on issues.
    pub is_pull: bool,
}

/// What a comment's writer is to the repository, as GitHub's `author_association` says. The
/// repository's own account is known by its login, [`RepoName::account`], so it isn't listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Association {
    Member,
    Collaborator,
    Contributor,
    FirstTimeContributor,
    FirstTimer,
    Mannequin,
    #[serde(other)]
    None,
}

impl Association {
    /// Someone who may write to the repository, beside its own account.
    pub fn writes(self) -> bool {
        matches!(self, Self::Member | Self::Collaborator)
    }
}

/// Whose open issues to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Whose {
    Anyone,
    OpenedByYou,
    AssignedToYou,
}

/// A comment on an issue or a pull request's conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub association: Association,
    pub body: String,
}

/// A pull request and what has been said on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
    pub state: PullState,
    pub draft: bool,
    pub head: String,
    /// The commit its branch is at, as GitHub last saw it.
    pub head_commit: Option<String>,
    /// The branch it would merge into.
    pub base: String,
    pub reviews: Vec<Review>,
    pub comments: Vec<Comment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PullState {
    Open,
    Closed,
    Merged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub id: String,
    pub author: String,
    pub association: Association,
    pub state: ReviewState,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    #[serde(other)]
    Pending,
}

/// A comment on a line of a pull request's change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewComment {
    pub id: u64,
    pub author: String,
    pub association: Association,
    pub body: String,
    pub path: String,
    pub line: Option<u64>,
    /// The comment this one answers, in a thread.
    pub reply_to: Option<u64>,
}

/// One check on a pull request, as `gh pr checks` sorts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub workflow: String,
    pub bucket: CheckBucket,
    pub link: String,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckBucket {
    Pass,
    Fail,
    Pending,
    Skipping,
    Cancel,
}

impl Check {
    /// The Actions job a check ran as, from its link
    /// `https://github.com/<account>/<name>/actions/runs/<run>/job/<job>`.
    pub fn job(&self) -> Option<u64> {
        let (_, job) = self.link.split_once("/job/")?;
        job.split(['/', '?', '#']).next()?.parse().ok()
    }

    /// The Actions run a check belongs to, from its link.
    pub fn run(&self) -> Option<u64> {
        let (_, run) = self.link.split_once("/actions/runs/")?;
        run.split(['/', '?', '#']).next()?.parse().ok()
    }
}

/// A draft pull request to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub repo: RepoName,
    /// The branch on the remote that holds the change.
    pub head: String,
    pub base: String,
    pub title: String,
    pub body: String,
}

impl Draft {
    /// The `gh` arguments that open it, as the record keeps them.
    pub fn command(&self) -> Vec<String> {
        [
            "gh",
            "pr",
            "create",
            "--repo",
            &self.repo.flag(),
            "--draft",
            "--head",
            &self.head,
            "--base",
            &self.base,
            "--title",
            &self.title,
            "--body",
            &self.body,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }
}

/// `gh` at a path, run in one folder.
#[derive(Debug, Clone)]
pub struct Gh {
    program: PathBuf,
    dir: PathBuf,
    env: Vec<(String, String)>,
}

impl Gh {
    /// The `gh` on `PATH`, never one inside `dir` or Argonauts Code's own home, where a
    /// repository or an agent could have put one.
    pub fn find(dir: &Path) -> Result<Self, GhError> {
        let program = find_unsandboxed("gh", Some(dir)).map_err(|_| GhError::NotInstalled)?;
        Ok(Self::at(program, dir))
    }

    pub fn at(program: impl Into<PathBuf>, dir: &Path) -> Self {
        Self {
            program: program.into(),
            dir: dir.to_owned(),
            env: Vec::new(),
        }
    }

    /// Variables every call runs with over the process's own.
    pub fn with_env(mut self, env: Vec<(String, String)>) -> Self {
        self.env = env;
        self
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Who `gh` is signed in as.
    pub async fn login(&self) -> Result<String, GhError> {
        let login = self.call(&["api", "user", "--jq", ".login"], &[]).await?;
        let login = login.trim();
        if login.is_empty() {
            return Err(GhError::NotSignedIn);
        }
        Ok(login.to_owned())
    }

    /// The repository the folder's remote names.
    pub async fn repo(&self) -> Result<Repo, GhError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Viewed {
            url: String,
            default_branch_ref: Option<Named>,
            #[serde(default)]
            viewer_permission: String,
        }
        let args = [
            "repo",
            "view",
            "--json",
            "url,defaultBranchRef,viewerPermission",
        ];
        let viewed: Viewed = self.json(&args, &[]).await?;
        let name = RepoName::of_url(&viewed.url)
            .ok_or_else(|| unreadable(&args, "no repository in its address"))?;
        Ok(Repo {
            name,
            default_branch: viewed
                .default_branch_ref
                .map_or_else(|| "main".to_owned(), |branch| branch.name),
            can_push: matches!(
                viewed.viewer_permission.as_str(),
                "ADMIN" | "MAINTAIN" | "WRITE"
            ),
        })
    }

    /// Up to `limit` open issues of `whose`, most recently updated first, as GitHub lists them.
    pub async fn open_issues(
        &self,
        repo: &RepoName,
        limit: usize,
        whose: Whose,
    ) -> Result<Vec<ListedIssue>, GhError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Listed {
            number: u64,
            title: String,
            #[serde(default)]
            labels: Vec<Named>,
            #[serde(with = "time::serde::rfc3339")]
            updated_at: OffsetDateTime,
            author: Option<Login>,
            #[serde(default)]
            assignees: Vec<Login>,
        }
        let limit = limit.to_string();
        let flag = repo.flag();
        let mut args = vec![
            "issue",
            "list",
            "--repo",
            &flag,
            "--state",
            "open",
            "--limit",
            &limit,
            "--json",
            "number,title,labels,updatedAt,author,assignees",
        ];
        match whose {
            Whose::Anyone => {}
            Whose::OpenedByYou => args.extend(["--author", "@me"]),
            Whose::AssignedToYou => args.extend(["--assignee", "@me"]),
        }
        let listed: Vec<Listed> = self.json(&args, &[]).await?;
        Ok(listed
            .into_iter()
            .map(|issue| ListedIssue {
                number: issue.number,
                title: issue.title,
                labels: issue.labels.into_iter().map(|label| label.name).collect(),
                updated: issue.updated_at,
                author: issue.author.map(|author| author.login).unwrap_or_default(),
                assignees: issue.assignees.into_iter().map(|who| who.login).collect(),
            })
            .collect())
    }

    /// How many issues are open on `repo`, pull requests left out.
    pub async fn open_issue_count(&self, repo: &RepoName) -> Result<usize, GhError> {
        let query = format!("q=repo:{repo} is:issue is:open");
        let args = [
            "api",
            "-X",
            "GET",
            "search/issues",
            "-f",
            "per_page=1",
            "-f",
            &query,
            "--jq",
            ".total_count",
        ];
        let out = self.call(&args, &[]).await?;
        out.trim()
            .parse()
            .map_err(|_| unreadable(&args, "the count isn't a number"))
    }

    /// One issue, with its state and whether it's locked.
    pub async fn issue(&self, repo: &RepoName, number: u64) -> Result<Issue, GhError> {
        #[derive(Deserialize)]
        struct Rest {
            number: u64,
            title: String,
            body: Option<String>,
            state: String,
            #[serde(default)]
            locked: bool,
            user: Option<Login>,
            author_association: Option<Association>,
            html_url: String,
            pull_request: Option<serde_json::Value>,
        }
        let path = format!("repos/{repo}/issues/{number}");
        let rest: Rest = self
            .api(repo, &path, false)
            .await
            .map_err(|err| named(err, format!("Issue #{number} of {repo}")))?;
        Ok(Issue {
            number: rest.number,
            title: rest.title,
            body: rest.body.unwrap_or_default(),
            open: rest.state.eq_ignore_ascii_case("open"),
            locked: rest.locked,
            author: rest.user.map(|user| user.login).unwrap_or_default(),
            association: rest.author_association.unwrap_or(Association::None),
            url: rest.html_url,
            is_pull: rest.pull_request.is_some(),
        })
    }

    /// Every comment on an issue's conversation, oldest first.
    pub async fn issue_comments(
        &self,
        repo: &RepoName,
        number: u64,
    ) -> Result<Vec<Comment>, GhError> {
        let path = format!("repos/{repo}/issues/{number}/comments");
        let pages: Vec<RestComment> = self.pages(repo, &path).await?;
        Ok(pages.into_iter().map(RestComment::into_comment).collect())
    }

    /// Opens a draft pull request, and gives its address.
    pub async fn open_draft(&self, draft: &Draft) -> Result<String, GhError> {
        let command = draft.command();
        let args: Vec<&str> = command.iter().skip(1).map(String::as_str).collect();
        let out = self.call(&args, &[]).await?;
        out.lines()
            .rev()
            .map(str::trim)
            .find(|line| line.starts_with("http"))
            .map(str::to_owned)
            .ok_or_else(|| unreadable(&args, "no address in what it printed"))
    }

    /// A pull request's checks. A pull request with none yet has an empty list.
    pub async fn checks(&self, repo: &RepoName, number: u64) -> Result<Vec<Check>, GhError> {
        #[derive(Deserialize)]
        struct Listed {
            name: String,
            #[serde(default)]
            workflow: String,
            bucket: CheckBucket,
            #[serde(default)]
            link: String,
            #[serde(default)]
            description: String,
        }
        let number = number.to_string();
        let flag = repo.flag();
        let args = [
            "pr",
            "checks",
            &number,
            "--repo",
            &flag,
            "--json",
            "name,state,bucket,link,workflow,description",
        ];
        let out = match self.call(&args, &CHECKS_EXITS).await {
            Err(GhError::Failed { said, .. }) if said.contains("no checks reported") => {
                return Ok(Vec::new());
            }
            out => out?,
        };
        // With no checks yet it may print nothing at all.
        if out.trim().is_empty() {
            return Ok(Vec::new());
        }
        let listed: Vec<Listed> =
            serde_json::from_str(&out).map_err(|err| unreadable(&args, &err.to_string()))?;
        Ok(listed
            .into_iter()
            .map(|check| Check {
                name: check.name,
                workflow: check.workflow,
                bucket: check.bucket,
                link: check.link,
                description: check.description,
            })
            .collect())
    }

    /// Whether the check named `check` failed the last time it ran on `branch`, as a pull
    /// request's base: `None` where it never ran there or hasn't ended.
    pub async fn failed_on(
        &self,
        repo: &RepoName,
        branch: &str,
        check: &str,
    ) -> Result<Option<bool>, GhError> {
        let path = format!("repos/{repo}/commits/{branch}/check-runs");
        let name = format!("check_name={check}");
        let args = [
            "api",
            "-X",
            "GET",
            &path,
            "-f",
            "per_page=1",
            "-f",
            &name,
            "--jq",
            ".check_runs[0].conclusion // \"\"",
        ];
        let out = self.call(&args, &[]).await?;
        Ok(match out.trim() {
            "failure" | "timed_out" | "startup_failure" => Some(true),
            "success" | "neutral" | "skipped" => Some(false),
            _ => None,
        })
    }

    /// A pull request's state, reviews and conversation.
    pub async fn pull(&self, repo: &RepoName, number: u64) -> Result<PullRequest, GhError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Viewed {
            number: u64,
            url: String,
            state: PullState,
            #[serde(default)]
            is_draft: bool,
            head_ref_name: String,
            #[serde(default)]
            head_ref_oid: String,
            #[serde(default)]
            base_ref_name: String,
            #[serde(default)]
            reviews: Vec<ViewedReview>,
            #[serde(default)]
            comments: Vec<ViewedComment>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ViewedReview {
            #[serde(default)]
            id: String,
            author: Option<Login>,
            author_association: Option<Association>,
            state: ReviewState,
            #[serde(default)]
            body: String,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ViewedComment {
            #[serde(default)]
            id: String,
            author: Option<Login>,
            author_association: Option<Association>,
            #[serde(default)]
            body: String,
        }
        let number_text = number.to_string();
        let flag = repo.flag();
        let args = [
            "pr",
            "view",
            &number_text,
            "--repo",
            &flag,
            "--json",
            "number,url,state,isDraft,headRefName,headRefOid,baseRefName,reviews,comments",
        ];
        let viewed: Viewed = self
            .json(&args, &[])
            .await
            .map_err(|err| named(err, format!("Pull request #{number} of {repo}")))?;
        Ok(PullRequest {
            number: viewed.number,
            url: viewed.url,
            state: viewed.state,
            draft: viewed.is_draft,
            head: viewed.head_ref_name,
            head_commit: Some(viewed.head_ref_oid).filter(|oid| !oid.is_empty()),
            base: viewed.base_ref_name,
            reviews: viewed
                .reviews
                .into_iter()
                .map(|review| Review {
                    id: review.id,
                    author: review.author.map(|who| who.login).unwrap_or_default(),
                    association: review.author_association.unwrap_or(Association::None),
                    state: review.state,
                    body: review.body,
                })
                .collect(),
            comments: viewed
                .comments
                .into_iter()
                .map(|comment| Comment {
                    id: comment.id,
                    author: comment.author.map(|who| who.login).unwrap_or_default(),
                    association: comment.author_association.unwrap_or(Association::None),
                    body: comment.body,
                })
                .collect(),
        })
    }

    /// The comments on lines of a pull request's change, oldest first.
    pub async fn review_comments(
        &self,
        repo: &RepoName,
        number: u64,
    ) -> Result<Vec<ReviewComment>, GhError> {
        #[derive(Deserialize)]
        struct Rest {
            id: u64,
            user: Option<Login>,
            author_association: Option<Association>,
            #[serde(default)]
            body: String,
            #[serde(default)]
            path: String,
            line: Option<u64>,
            in_reply_to_id: Option<u64>,
        }
        let path = format!("repos/{repo}/pulls/{number}/comments");
        let pages: Vec<Rest> = self.pages(repo, &path).await?;
        Ok(pages
            .into_iter()
            .map(|rest| ReviewComment {
                id: rest.id,
                author: rest.user.map(|user| user.login).unwrap_or_default(),
                association: rest.author_association.unwrap_or(Association::None),
                body: rest.body,
                path: rest.path,
                line: rest.line,
                reply_to: rest.in_reply_to_id,
            })
            .collect())
    }

    /// What the failed steps of a check's job printed, as `gh run view --log-failed` gives it.
    pub async fn failed_log(&self, repo: &RepoName, check: &Check) -> Result<String, GhError> {
        let flag = repo.flag();
        let job = check.job().map(|job| job.to_string());
        let run = check.run().map(|run| run.to_string());
        let mut args = vec!["run", "view"];
        match (&job, &run) {
            (Some(job), _) => args.extend(["--job", job.as_str()]),
            (None, Some(run)) => args.push(run.as_str()),
            (None, None) => {
                return Err(GhError::NotFound {
                    what: format!("The log of {}", check.name),
                });
            }
        }
        args.extend(["--repo", &flag, "--log-failed"]);
        self.call(&args, &[]).await
    }

    /// Posts a comment on a pull request's conversation.
    pub async fn comment_on_pull(
        &self,
        repo: &RepoName,
        number: u64,
        body: &str,
    ) -> Result<(), GhError> {
        let number = number.to_string();
        let flag = repo.flag();
        let args = ["pr", "comment", &number, "--repo", &flag, "--body", body];
        self.call(&args, &[]).await.map(drop)
    }

    /// Posts a comment on an issue.
    pub async fn comment_on_issue(
        &self,
        repo: &RepoName,
        number: u64,
        body: &str,
    ) -> Result<(), GhError> {
        let number = number.to_string();
        let flag = repo.flag();
        let args = ["issue", "comment", &number, "--repo", &flag, "--body", body];
        self.call(&args, &[]).await.map(drop)
    }

    /// Answers a comment on a line of a pull request's change, in its thread.
    pub async fn reply(
        &self,
        repo: &RepoName,
        number: u64,
        comment: u64,
        body: &str,
    ) -> Result<(), GhError> {
        let path = format!("repos/{repo}/pulls/{number}/comments/{comment}/replies");
        let field = format!("body={body}");
        let mut args = vec!["api", "--method", "POST", &path, "-f", &field];
        let host = (!repo.on_github()).then(|| repo.host.clone());
        if let Some(host) = &host {
            args.extend(["--hostname", host.as_str()]);
        }
        self.call(&args, &[]).await.map(drop)
    }

    /// Adds a label to an issue.
    pub async fn label(&self, repo: &RepoName, number: u64, label: &str) -> Result<(), GhError> {
        let number = number.to_string();
        let flag = repo.flag();
        let args = [
            "issue",
            "edit",
            &number,
            "--repo",
            &flag,
            "--add-label",
            label,
        ];
        self.call(&args, &[]).await.map(drop)
    }

    /// Forks `repo` to `login`'s account on GitHub, with no clone and no remote changed here.
    /// The fork is `login`'s repository of the same name.
    pub async fn fork(&self, repo: &RepoName, login: &str) -> Result<RepoName, GhError> {
        let flag = repo.flag();
        let args = ["repo", "fork", &flag, "--clone=false", "--remote=false"];
        self.call(&args, &[]).await?;
        Ok(RepoName {
            host: repo.host.clone(),
            account: login.to_owned(),
            name: repo.name.clone(),
        })
    }

    /// Takes a label off an issue.
    pub async fn unlabel(&self, repo: &RepoName, number: u64, label: &str) -> Result<(), GhError> {
        let number = number.to_string();
        let flag = repo.flag();
        let args = [
            "issue",
            "edit",
            &number,
            "--repo",
            &flag,
            "--remove-label",
            label,
        ];
        self.call(&args, &[]).await.map(drop)
    }

    /// Assigns an issue to whoever `gh` is signed in as.
    pub async fn assign_me(&self, repo: &RepoName, number: u64) -> Result<(), GhError> {
        let number = number.to_string();
        let flag = repo.flag();
        let args = [
            "issue",
            "edit",
            &number,
            "--repo",
            &flag,
            "--add-assignee",
            "@me",
        ];
        self.call(&args, &[]).await.map(drop)
    }

    async fn api<T: DeserializeOwned>(
        &self,
        repo: &RepoName,
        path: &str,
        paginate: bool,
    ) -> Result<T, GhError> {
        let mut args = vec!["api", path];
        if paginate {
            args.push("--paginate");
        }
        if !repo.on_github() {
            args.extend(["--hostname", repo.host.as_str()]);
        }
        self.json(&args, &[]).await
    }

    /// Every page of a list the REST API gives in pages. `--paginate` prints one JSON array per
    /// page, one after another.
    async fn pages<T: DeserializeOwned>(
        &self,
        repo: &RepoName,
        path: &str,
    ) -> Result<Vec<T>, GhError> {
        let mut args = vec!["api", path, "--paginate"];
        if !repo.on_github() {
            args.extend(["--hostname", repo.host.as_str()]);
        }
        let out = self.call(&args, &[]).await?;
        let mut all = Vec::new();
        for page in serde_json::Deserializer::from_str(&out).into_iter::<Vec<T>>() {
            all.extend(page.map_err(|err| unreadable(&args, &err.to_string()))?);
        }
        Ok(all)
    }

    async fn json<T: DeserializeOwned>(
        &self,
        args: &[&str],
        also_ok: &[i32],
    ) -> Result<T, GhError> {
        let out = self.call(args, also_ok).await?;
        serde_json::from_str(&out).map_err(|err| unreadable(args, &err.to_string()))
    }

    /// Runs one call and gives what it printed. `also_ok` names exit codes, beside 0, that
    /// still printed the answer.
    async fn call(&self, args: &[&str], also_ok: &[i32]) -> Result<String, GhError> {
        let mut command = tokio::process::Command::new(&self.program);
        command
            .args(args)
            .current_dir(&self.dir)
            .envs(QUIET)
            .envs(self.env.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let out = match tokio::time::timeout(CALL_TIMEOUT, command.output()).await {
            Err(_) => {
                return Err(GhError::Failed {
                    command: named_command(args),
                    said: "it gave no answer within a minute".to_owned(),
                });
            }
            Ok(Err(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(GhError::NotInstalled);
            }
            Ok(Err(err)) => {
                return Err(GhError::Failed {
                    command: named_command(args),
                    said: err.to_string(),
                });
            }
            Ok(Ok(out)) => out,
        };
        let code = out.status.code();
        if out.status.success() || code.is_some_and(|code| also_ok.contains(&code)) {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
        Err(problem(args, code, &String::from_utf8_lossy(&out.stderr)))
    }
}

#[derive(Deserialize)]
struct Named {
    name: String,
}

#[derive(Deserialize)]
struct Login {
    login: String,
}

#[derive(Deserialize)]
struct RestComment {
    id: u64,
    user: Option<Login>,
    author_association: Option<Association>,
    #[serde(default)]
    body: String,
}

impl RestComment {
    fn into_comment(self) -> Comment {
        Comment {
            id: self.id.to_string(),
            author: self.user.map(|user| user.login).unwrap_or_default(),
            association: self.author_association.unwrap_or(Association::None),
            body: self.body,
        }
    }
}

/// `issue list`, `pr create`, or `api` with its path: enough to say which call failed.
fn named_command(args: &[&str]) -> String {
    args.iter()
        .filter(|arg| !arg.starts_with('-'))
        .take(2)
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
}

fn unreadable(args: &[&str], why: &str) -> GhError {
    GhError::Unreadable {
        command: named_command(args),
        why: why.to_owned(),
    }
}

/// A missing thing named for what was asked, such as "Issue #42 of team/app".
fn named(err: GhError, what: String) -> GhError {
    match err {
        GhError::NotFound { .. } => GhError::NotFound { what },
        other => other,
    }
}

/// What a failed call's exit code and words mean.
fn problem(args: &[&str], code: Option<i32>, stderr: &str) -> GhError {
    let said = stderr.trim();
    let lower = said.to_lowercase();
    if code == Some(SIGNED_OUT_EXIT)
        || lower.contains("gh auth login")
        || lower.contains("not logged into")
    {
        return GhError::NotSignedIn;
    }
    if lower.contains("no git remotes")
        || lower.contains("none of the git remotes")
        || lower.contains("not a git repository")
    {
        return GhError::NoRemote;
    }
    if lower.contains("http 404")
        || lower.contains("not found")
        || lower.contains("could not resolve")
    {
        return GhError::NotFound {
            what: named_command(args),
        };
    }
    GhError::Failed {
        command: named_command(args),
        said: said
            .lines()
            .last()
            .unwrap_or("it gave no reason")
            .trim()
            .to_owned(),
    }
}

/// The part of a failed job's log that shows the failure, for the agent that fixes it: the
/// job and step columns and timestamps `gh` puts before each line go, and of a long log only
/// the lines around the first error are kept, at most `most` lines.
pub fn failure_part(log: &str, most: usize) -> String {
    let lines: Vec<&str> = log.lines().map(log_text).collect();
    if lines.len() <= most {
        return lines.join("\n");
    }
    let first_error = lines
        .iter()
        .position(|line| {
            let lower = line.to_lowercase();
            [
                "error",
                "failed",
                "panicked",
                "assert",
                "traceback",
                "exception",
            ]
            .iter()
            .any(|word| lower.contains(word))
        })
        .unwrap_or(lines.len() - most);
    // Some lines before the error say what was running when it came.
    let start = first_error.saturating_sub(most / 4).min(lines.len() - most);
    let mut part = Vec::with_capacity(most + 2);
    if start > 0 {
        part.push(format!("… {start} lines before"));
    }
    part.extend(
        lines[start..start + most]
            .iter()
            .map(|line| (*line).to_owned()),
    );
    let after = lines.len() - start - most;
    if after > 0 {
        part.push(format!("… {after} lines after"));
    }
    part.join("\n")
}

/// A `--log-failed` line without its `job<TAB>step<TAB>timestamp ` prefix.
fn log_text(line: &str) -> &str {
    let mut columns = line.splitn(3, '\t');
    let (Some(_job), Some(_step), Some(rest)) = (columns.next(), columns.next(), columns.next())
    else {
        return line;
    };
    match rest.split_once(' ') {
        Some((stamp, text)) if stamp.len() >= 20 && stamp.as_bytes()[4] == b'-' => text,
        _ => rest,
    }
}

#[cfg(test)]
mod tests;
