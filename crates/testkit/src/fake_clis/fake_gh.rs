//! A fake `gh` for tests: the `argonauts-fake-cli` binary installed as `gh`, answering from a
//! world file of one repository's issues and pull requests (`ARGONAUTS_FAKE_GH`) and logging
//! every call beside it. Writes change the world, so a test reads back the draft it opened or
//! the comment it posted. Nothing reaches GitHub.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::home::bins::fake_cli_bin;

/// The world file the fake answers from.
pub const WORLD_ENV: &str = "ARGONAUTS_FAKE_GH";

/// What `gh` says when no one is signed in, as gh 2.x words it, with its exit code.
pub const SIGNED_OUT: &str = "To get started with GitHub CLI, please run:  gh auth login\n\
     Alternatively, populate the GH_TOKEN environment variable with a GitHub API authentication token.";
const SIGNED_OUT_EXIT: i32 = 4;

/// One repository as `gh` sees it from the folder it runs in.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GhWorld {
    /// Who `gh` is signed in as. `None` is signed out.
    pub login: Option<String>,
    /// The repository the folder's remote names. `None` is a folder with no GitHub remote.
    pub repo: Option<GhRepo>,
    pub issues: Vec<GhIssue>,
    pub pulls: Vec<GhPull>,
    /// What `gh run view --log-failed` prints, by job id.
    pub logs: BTreeMap<u64, String>,
    /// How each check last ended on a pull request's base branch, by its name: `success` or
    /// `failure`.
    pub base_checks: BTreeMap<String, String>,
    /// Every write, in order.
    pub posted: Vec<GhPost>,
    /// The number the next pull request gets.
    pub next_number: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GhRepo {
    /// `account/name`.
    pub name: String,
    pub host: String,
    pub default_branch: String,
    /// GitHub's `viewerPermission`, such as `ADMIN`, `WRITE` or `READ`.
    pub permission: String,
}

impl Default for GhRepo {
    fn default() -> Self {
        Self {
            name: "team/app".to_owned(),
            host: "github.com".to_owned(),
            default_branch: "main".to_owned(),
            permission: "ADMIN".to_owned(),
        }
    }
}

impl GhRepo {
    pub fn url(&self) -> String {
        format!("https://{}/{}", self.host, self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GhIssue {
    pub number: u64,
    pub title: String,
    pub body: String,
    /// `OPEN` or `CLOSED`.
    pub state: String,
    pub locked: bool,
    pub author: String,
    pub assignees: Vec<String>,
    pub labels: Vec<String>,
    /// RFC 3339.
    pub updated: String,
    pub comments: Vec<GhComment>,
}

impl Default for GhIssue {
    fn default() -> Self {
        Self {
            number: 1,
            title: String::new(),
            body: String::new(),
            state: "OPEN".to_owned(),
            locked: false,
            author: String::new(),
            assignees: Vec::new(),
            labels: Vec::new(),
            updated: "2026-10-20T10:00:00Z".to_owned(),
            comments: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GhComment {
    pub id: u64,
    pub author: String,
    /// GitHub's `author_association`, such as `MEMBER`, `COLLABORATOR`, `CONTRIBUTOR` or `NONE`.
    pub association: String,
    pub body: String,
    pub created: String,
}

impl Default for GhComment {
    fn default() -> Self {
        Self {
            id: 1,
            author: String::new(),
            association: "NONE".to_owned(),
            body: String::new(),
            created: "2026-10-20T10:00:00Z".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GhPull {
    pub number: u64,
    pub head: String,
    /// The commit `head` is at, as GitHub would say.
    pub head_oid: String,
    pub base: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
    /// `OPEN`, `CLOSED` or `MERGED`.
    pub state: String,
    pub checks: Vec<GhCheck>,
    pub reviews: Vec<GhReview>,
    pub comments: Vec<GhComment>,
    pub review_comments: Vec<GhReviewComment>,
}

impl Default for GhPull {
    fn default() -> Self {
        Self {
            number: 1,
            head: String::new(),
            head_oid: String::new(),
            base: "main".to_owned(),
            title: String::new(),
            body: String::new(),
            draft: true,
            state: "OPEN".to_owned(),
            checks: Vec::new(),
            reviews: Vec::new(),
            comments: Vec::new(),
            review_comments: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GhCheck {
    pub name: String,
    pub workflow: String,
    /// `gh pr checks`' bucket: `pass`, `fail`, `pending`, `skipping` or `cancel`.
    pub bucket: String,
    pub link: String,
    pub description: String,
}

impl Default for GhCheck {
    fn default() -> Self {
        Self {
            name: "test".to_owned(),
            workflow: "CI".to_owned(),
            bucket: "pass".to_owned(),
            link: String::new(),
            description: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GhReview {
    pub id: String,
    pub author: String,
    pub association: String,
    /// `APPROVED`, `CHANGES_REQUESTED`, `COMMENTED` or `DISMISSED`.
    pub state: String,
    pub body: String,
}

impl Default for GhReview {
    fn default() -> Self {
        Self {
            id: "R_1".to_owned(),
            author: String::new(),
            association: "NONE".to_owned(),
            state: "COMMENTED".to_owned(),
            body: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GhReviewComment {
    pub id: u64,
    pub author: String,
    pub association: String,
    pub body: String,
    pub path: String,
    pub line: Option<u64>,
    pub in_reply_to: Option<u64>,
}

impl Default for GhReviewComment {
    fn default() -> Self {
        Self {
            id: 1,
            author: String::new(),
            association: "NONE".to_owned(),
            body: String::new(),
            path: String::new(),
            line: None,
            in_reply_to: None,
        }
    }
}

/// A write the fake took.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GhPost {
    Pull {
        number: u64,
        head: String,
        base: String,
        title: String,
        body: String,
        draft: bool,
    },
    /// On an issue or a pull request, by its number.
    Comment {
        on: u64,
        body: String,
    },
    Reply {
        pull: u64,
        to: u64,
        body: String,
    },
    Label {
        issue: u64,
        label: String,
    },
    Unlabel {
        issue: u64,
        label: String,
    },
    Assign {
        issue: u64,
        who: String,
    },
    /// `gh repo fork`, of `account/name`.
    Fork {
        repo: String,
    },
}

/// The fake installed under `dir/bin/gh`, playing `dir/gh-world.json`.
pub struct FakeGh {
    bin_dir: PathBuf,
    world: PathBuf,
}

impl FakeGh {
    pub fn install(dir: &Path, world: &GhWorld) -> anyhow::Result<Self> {
        Self::install_with(&fake_cli_bin()?, dir, world)
    }

    /// Like `install`, with the fake binary given, such as a test's
    /// `CARGO_BIN_EXE_argonauts-fake-cli`.
    pub fn install_with(binary: &Path, dir: &Path, world: &GhWorld) -> anyhow::Result<Self> {
        let bin_dir = dir.join("bin");
        fs::create_dir_all(&bin_dir).with_context(|| format!("creating {}", bin_dir.display()))?;
        let link = bin_dir.join("gh");
        if link.exists() || link.is_symlink() {
            fs::remove_file(&link).with_context(|| format!("replacing {}", link.display()))?;
        }
        std::os::unix::fs::symlink(binary, &link)
            .with_context(|| format!("linking {}", link.display()))?;
        let fake = Self {
            bin_dir,
            world: dir.join("gh-world.json"),
        };
        fake.set_world(world)?;
        Ok(fake)
    }

    /// The `gh` to run.
    pub fn program(&self) -> PathBuf {
        self.bin_dir.join("gh")
    }

    pub fn bin_dir(&self) -> &Path {
        &self.bin_dir
    }

    /// What a `gh` process needs to find its world.
    pub fn envs(&self) -> Vec<(String, String)> {
        vec![(
            WORLD_ENV.to_owned(),
            self.world.to_string_lossy().into_owned(),
        )]
    }

    /// `PATH` with the fake first, and the world, for a process that looks `gh` up itself.
    pub fn path_envs(&self) -> Vec<(String, OsString)> {
        let mut path = OsString::from(self.bin_dir.as_os_str());
        if let Some(rest) = std::env::var_os("PATH") {
            path.push(":");
            path.push(rest);
        }
        vec![
            ("PATH".to_owned(), path),
            (WORLD_ENV.to_owned(), self.world.clone().into_os_string()),
        ]
    }

    pub fn world(&self) -> anyhow::Result<GhWorld> {
        load(&self.world)
    }

    pub fn set_world(&self, world: &GhWorld) -> anyhow::Result<()> {
        save(&self.world, world)
    }

    /// Every call's arguments, in order.
    pub fn calls(&self) -> anyhow::Result<Vec<Vec<String>>> {
        let log = calls_log(&self.world);
        let Ok(text) = fs::read_to_string(&log) else {
            return Ok(Vec::new());
        };
        text.lines()
            .map(|line| serde_json::from_str(line).context("reading the fake gh's log"))
            .collect()
    }

    /// The calls that changed something on GitHub, as the world recorded them.
    pub fn posted(&self) -> anyhow::Result<Vec<GhPost>> {
        Ok(self.world()?.posted)
    }
}

fn calls_log(world: &Path) -> PathBuf {
    world.with_extension("calls.jsonl")
}

fn load(path: &Path) -> anyhow::Result<GhWorld> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Written whole under a name of its own and moved into place, so a test or another `gh` call
/// reading the world while this one writes it never reads half a file.
fn save(path: &Path, world: &GhWorld) -> anyhow::Result<()> {
    let text = serde_json::to_string_pretty(world)?;
    let part = path.with_extension(format!("{}.part", std::process::id()));
    fs::write(&part, text).with_context(|| format!("writing {}", part.display()))?;
    fs::rename(&part, path).with_context(|| format!("writing {}", path.display()))
}

/// What one call printed, and its exit code.
struct Answer {
    out: String,
    err: String,
    code: i32,
}

impl Answer {
    fn ok(out: impl Into<String>) -> Self {
        Self {
            out: out.into(),
            err: String::new(),
            code: 0,
        }
    }

    fn json(value: &Value) -> Self {
        Self::ok(format!("{value}\n"))
    }

    fn fails(err: impl Into<String>, code: i32) -> Self {
        Self {
            out: String::new(),
            err: err.into(),
            code,
        }
    }

    fn not_found() -> Self {
        Self::fails("gh: Not Found (HTTP 404)", 1)
    }
}

/// Plays one `gh` call: the fake binary's entry when it runs as `gh`.
pub fn serve(args: &[String]) -> i32 {
    let Some(path) = std::env::var_os(WORLD_ENV).map(PathBuf::from) else {
        eprintln!("argonauts-fake-cli: {WORLD_ENV} is not set");
        return 2;
    };
    if let Ok(line) = serde_json::to_string(args)
        && let Ok(mut log) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(calls_log(&path))
    {
        let _ = writeln!(log, "{line}");
    }
    let mut world = match load(&path) {
        Ok(world) => world,
        Err(err) => {
            eprintln!("argonauts-fake-cli: {err:#}");
            return 2;
        }
    };
    let before = world.clone();
    let answer = answer(&mut world, &Call::parse(args));
    if world != before
        && let Err(err) = save(&path, &world)
    {
        eprintln!("argonauts-fake-cli: {err:#}");
        return 2;
    }
    print!("{}", answer.out);
    if !answer.err.is_empty() {
        eprintln!("{}", answer.err);
    }
    answer.code
}

/// A call's words and its `--flag value` options.
struct Call {
    words: Vec<String>,
    options: BTreeMap<String, String>,
    switches: Vec<String>,
}

/// The flags of the calls Argonauts Code makes that take a value.
const VALUED: [&str; 20] = [
    "--json",
    "--author",
    "--assignee",
    "--jq",
    "--repo",
    "-R",
    "--state",
    "--limit",
    "--head",
    "--base",
    "--title",
    "--body",
    "--hostname",
    "--method",
    "-X",
    "--job",
    "-f",
    "--add-label",
    "--remove-label",
    "--add-assignee",
];

impl Call {
    fn parse(args: &[String]) -> Self {
        let mut call = Self {
            words: Vec::new(),
            options: BTreeMap::new(),
            switches: Vec::new(),
        };
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            if VALUED.contains(&arg.as_str()) {
                let value = args.next().cloned().unwrap_or_default();
                call.options.insert(arg.clone(), value);
            } else if arg.starts_with('-') {
                call.switches.push(arg.clone());
            } else {
                call.words.push(arg.clone());
            }
        }
        call
    }

    fn word(&self, at: usize) -> &str {
        self.words.get(at).map_or("", String::as_str)
    }

    fn option(&self, name: &str) -> Option<&str> {
        self.options.get(name).map(String::as_str)
    }

    fn has(&self, switch: &str) -> bool {
        self.switches.iter().any(|given| given == switch)
    }

    fn number(&self, at: usize) -> Option<u64> {
        self.word(at).trim_start_matches('#').parse().ok()
    }
}

fn answer(world: &mut GhWorld, call: &Call) -> Answer {
    let Some(login) = world.login.clone() else {
        if call.word(0) == "auth" {
            return Answer::fails(
                "You are not logged into any GitHub hosts. To log in, run: gh auth login",
                1,
            );
        }
        return Answer::fails(SIGNED_OUT, SIGNED_OUT_EXIT);
    };
    match (call.word(0), call.word(1)) {
        ("auth", "status") => Answer::ok(format!(
            "github.com\n  ✓ Logged in to github.com account {login} (keyring)\n"
        )),
        ("api", _) => api(world, call, &login),
        ("repo", "view") => match &world.repo {
            Some(repo) => Answer::json(&json!({
                "url": repo.url(),
                "defaultBranchRef": {"name": repo.default_branch},
                "viewerPermission": repo.permission,
            })),
            None => Answer::fails("no git remotes found", 1),
        },
        _ if world.repo.is_none() => Answer::fails("no git remotes found", 1),
        ("issue", "list") => {
            let limit = call
                .option("--limit")
                .and_then(|limit| limit.parse().ok())
                .unwrap_or(30);
            // `@me` is who gh is signed in as.
            let me = |who: &str| {
                if who == "@me" {
                    login.clone()
                } else {
                    who.to_owned()
                }
            };
            let author = call.option("--author").map(me);
            let assignee = call.option("--assignee").map(me);
            let listed: Vec<Value> = world
                .issues
                .iter()
                .filter(|issue| issue.state == "OPEN")
                .filter(|issue| author.as_ref().is_none_or(|who| issue.author == *who))
                .filter(|issue| {
                    assignee
                        .as_ref()
                        .is_none_or(|who| issue.assignees.contains(who))
                })
                .take(limit)
                .map(listed_issue)
                .collect();
            Answer::json(&Value::Array(listed))
        }
        ("repo", "fork") => {
            let repo = call.word(2).to_owned();
            let name = repo.rsplit('/').next().unwrap_or_default().to_owned();
            world.posted.push(GhPost::Fork { repo });
            Answer::ok(format!("https://github.com/{login}/{name}\n"))
        }
        ("issue", "comment") => post_comment(world, call, false),
        ("issue", "edit") => edit_issue(world, call),
        ("pr", "create") => create_pull(world, call),
        ("pr", "checks") => match pull(world, call) {
            Some(pull) => checks(pull),
            None => Answer::fails("no pull requests found", 1),
        },
        ("pr", "view") => match pull(world, call) {
            Some(pull) => Answer::json(&viewed_pull(world, pull)),
            None => Answer::fails("no pull requests found", 1),
        },
        ("pr", "comment") => post_comment(world, call, true),
        ("run", "view") => failed_log(world, call),
        _ => Answer::fails(format!("unknown command {:?}", call.words), 1),
    }
}

fn listed_issue(issue: &GhIssue) -> Value {
    json!({
        "number": issue.number,
        "title": issue.title,
        "labels": issue.labels.iter().map(|name| json!({"name": name})).collect::<Vec<_>>(),
        "updatedAt": issue.updated,
        "author": {"login": issue.author},
        "assignees": issue.assignees.iter().map(|login| json!({"login": login})).collect::<Vec<_>>(),
    })
}

fn api(world: &mut GhWorld, call: &Call, login: &str) -> Answer {
    let path = call.word(1);
    if path == "user" {
        return Answer::ok(format!("{login}\n"));
    }
    // Only the open-issue count is searched for.
    if path == "search/issues" {
        let open = world
            .issues
            .iter()
            .filter(|issue| issue.state == "OPEN")
            .count();
        return Answer::ok(format!("{open}\n"));
    }
    let Some(repo) = world.repo.clone() else {
        return Answer::not_found();
    };
    let Some(rest) = path
        .strip_prefix("repos/")
        .and_then(|rest| rest.strip_prefix(repo.name.as_str()))
    else {
        return Answer::not_found();
    };
    let parts: Vec<&str> = rest.trim_matches('/').split('/').collect();
    let posting = call
        .option("--method")
        .or(call.option("-X"))
        .is_some_and(|method| method.eq_ignore_ascii_case("POST"));
    match parts.as_slice() {
        // The check's name is the last `-f`, as the client sends it.
        ["commits", _, "check-runs"] => {
            let named = call
                .option("-f")
                .and_then(|field| field.strip_prefix("check_name="))
                .unwrap_or_default();
            Answer::ok(format!(
                "{}\n",
                world.base_checks.get(named).map_or("", String::as_str)
            ))
        }
        ["issues", number] => match find_issue(world, number) {
            Some(issue) => Answer::json(&rest_issue(&repo, issue)),
            None => Answer::not_found(),
        },
        ["issues", number, "comments"] => match find_issue(world, number) {
            Some(issue) => Answer::json(&Value::Array(
                issue.comments.iter().map(rest_comment).collect(),
            )),
            None => Answer::not_found(),
        },
        ["pulls", number, "comments"] => match find_pull(world, number) {
            Some(pull) => Answer::json(&Value::Array(
                pull.review_comments
                    .iter()
                    .map(rest_review_comment)
                    .collect(),
            )),
            None => Answer::not_found(),
        },
        ["pulls", number, "comments", to, "replies"] if posting => {
            let (Ok(pull), Ok(to)) = (number.parse::<u64>(), to.parse::<u64>()) else {
                return Answer::not_found();
            };
            let body = call
                .option("-f")
                .and_then(|field| field.strip_prefix("body="))
                .unwrap_or_default()
                .to_owned();
            world.posted.push(GhPost::Reply { pull, to, body });
            Answer::json(&json!({"id": 9000 + world.posted.len()}))
        }
        _ => Answer::not_found(),
    }
}

fn find_issue<'a>(world: &'a GhWorld, number: &str) -> Option<&'a GhIssue> {
    let number: u64 = number.parse().ok()?;
    world.issues.iter().find(|issue| issue.number == number)
}

fn find_pull<'a>(world: &'a GhWorld, number: &str) -> Option<&'a GhPull> {
    let number: u64 = number.parse().ok()?;
    world.pulls.iter().find(|pull| pull.number == number)
}

fn rest_issue(repo: &GhRepo, issue: &GhIssue) -> Value {
    json!({
        "number": issue.number,
        "title": issue.title,
        "body": issue.body,
        "state": issue.state.to_lowercase(),
        "locked": issue.locked,
        "user": {"login": issue.author},
        "author_association": "NONE",
        "html_url": format!("{}/issues/{}", repo.url(), issue.number),
    })
}

fn rest_comment(comment: &GhComment) -> Value {
    json!({
        "id": comment.id,
        "user": {"login": comment.author},
        "author_association": comment.association,
        "body": comment.body,
        "created_at": comment.created,
    })
}

fn rest_review_comment(comment: &GhReviewComment) -> Value {
    json!({
        "id": comment.id,
        "user": {"login": comment.author},
        "author_association": comment.association,
        "body": comment.body,
        "path": comment.path,
        "line": comment.line,
        "in_reply_to_id": comment.in_reply_to,
    })
}

fn pull<'a>(world: &'a GhWorld, call: &Call) -> Option<&'a GhPull> {
    let number = call.number(2)?;
    world.pulls.iter().find(|pull| pull.number == number)
}

/// As gh 2.x, `gh pr checks` exits 8 while a check is pending and 1 once one failed, even
/// with `--json`.
fn checks(pull: &GhPull) -> Answer {
    let listed: Vec<Value> = pull
        .checks
        .iter()
        .map(|check| {
            json!({
                "name": check.name,
                "workflow": check.workflow,
                "bucket": check.bucket,
                "state": check.bucket.to_uppercase(),
                "link": check.link,
                "description": check.description,
            })
        })
        .collect();
    let mut answer = Answer::json(&Value::Array(listed));
    if pull.checks.iter().any(|check| check.bucket == "fail") {
        answer.code = 1;
    } else if pull.checks.iter().any(|check| check.bucket == "pending") {
        answer.code = 8;
    }
    answer
}

fn viewed_pull(world: &GhWorld, pull: &GhPull) -> Value {
    let url = world
        .repo
        .as_ref()
        .map(|repo| format!("{}/pull/{}", repo.url(), pull.number))
        .unwrap_or_default();
    json!({
        "number": pull.number,
        "url": url,
        "state": pull.state,
        "isDraft": pull.draft,
        "headRefName": pull.head,
        "headRefOid": pull.head_oid,
        "baseRefName": pull.base,
        "reviews": pull.reviews.iter().map(|review| json!({
            "id": review.id,
            "author": {"login": review.author},
            "authorAssociation": review.association,
            "state": review.state,
            "body": review.body,
        })).collect::<Vec<_>>(),
        "comments": pull.comments.iter().map(|comment| json!({
            "id": format!("IC_{}", comment.id),
            "author": {"login": comment.author},
            "authorAssociation": comment.association,
            "body": comment.body,
            "createdAt": comment.created,
        })).collect::<Vec<_>>(),
    })
}

fn create_pull(world: &mut GhWorld, call: &Call) -> Answer {
    let Some(head) = call.option("--head").map(str::to_owned) else {
        return Answer::fails("--head is required when not running interactively", 1);
    };
    if world
        .pulls
        .iter()
        .any(|pull| pull.head == head && pull.state == "OPEN")
    {
        return Answer::fails(
            format!("a pull request for branch \"{head}\" already exists"),
            1,
        );
    }
    let repo = world.repo.clone().unwrap_or_default();
    let number = world.next_number.max(1);
    world.next_number = number + 1;
    let pull = GhPull {
        number,
        head: head.clone(),
        base: call
            .option("--base")
            .map_or(repo.default_branch.clone(), str::to_owned),
        title: call.option("--title").unwrap_or_default().to_owned(),
        body: call.option("--body").unwrap_or_default().to_owned(),
        draft: call.has("--draft"),
        ..GhPull::default()
    };
    world.posted.push(GhPost::Pull {
        number,
        head,
        base: pull.base.clone(),
        title: pull.title.clone(),
        body: pull.body.clone(),
        draft: pull.draft,
    });
    world.pulls.push(pull);
    Answer::ok(format!("{}/pull/{number}\n", repo.url()))
}

fn post_comment(world: &mut GhWorld, call: &Call, on_pull: bool) -> Answer {
    let Some(on) = call.number(2) else {
        return Answer::fails("a number is required", 1);
    };
    let known = if on_pull {
        world.pulls.iter().any(|pull| pull.number == on)
    } else {
        world.issues.iter().any(|issue| issue.number == on)
    };
    if !known {
        return Answer::not_found();
    }
    let body = call.option("--body").unwrap_or_default().to_owned();
    world.posted.push(GhPost::Comment { on, body });
    let repo = world.repo.clone().unwrap_or_default();
    let kind = if on_pull { "pull" } else { "issues" };
    Answer::ok(format!(
        "{}/{kind}/{on}#issuecomment-{}\n",
        repo.url(),
        world.posted.len()
    ))
}

fn edit_issue(world: &mut GhWorld, call: &Call) -> Answer {
    let Some(issue) = call.number(2) else {
        return Answer::fails("a number is required", 1);
    };
    if let Some(label) = call.option("--add-label") {
        world.posted.push(GhPost::Label {
            issue,
            label: label.to_owned(),
        });
    }
    if let Some(label) = call.option("--remove-label") {
        world.posted.push(GhPost::Unlabel {
            issue,
            label: label.to_owned(),
        });
    }
    if let Some(who) = call.option("--add-assignee") {
        world.posted.push(GhPost::Assign {
            issue,
            who: who.to_owned(),
        });
    }
    Answer::ok(String::new())
}

fn failed_log(world: &GhWorld, call: &Call) -> Answer {
    let job = call
        .option("--job")
        .and_then(|job| job.parse::<u64>().ok())
        .or_else(|| call.number(2));
    match job.and_then(|job| world.logs.get(&job)) {
        Some(log) if call.has("--log-failed") => Answer::ok(log.clone()),
        _ => Answer::fails("failed to get run log: log not found", 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(args: &[&str]) -> Call {
        Call::parse(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
    }

    fn world() -> GhWorld {
        GhWorld {
            login: Some("test-user".to_owned()),
            repo: Some(GhRepo::default()),
            issues: vec![GhIssue {
                number: 42,
                title: "Limits".to_owned(),
                ..GhIssue::default()
            }],
            next_number: 57,
            ..GhWorld::default()
        }
    }

    #[test]
    fn signed_out_it_answers_as_gh_does() {
        let mut world = GhWorld::default();
        let answer = answer(&mut world, &call(&["api", "user", "--jq", ".login"]));
        assert_eq!(answer.code, SIGNED_OUT_EXIT);
        assert!(answer.err.contains("gh auth login"));
    }

    #[test]
    fn a_draft_is_recorded_and_numbered() {
        let mut world = world();
        let created = answer(
            &mut world,
            &call(&[
                "pr",
                "create",
                "--repo",
                "team/app",
                "--draft",
                "--head",
                "issue-42-x",
                "--base",
                "main",
                "--title",
                "T",
                "--body",
                "Fixes #42",
            ]),
        );
        assert_eq!(created.out, "https://github.com/team/app/pull/57\n");
        assert_eq!(
            world.posted,
            [GhPost::Pull {
                number: 57,
                head: "issue-42-x".to_owned(),
                base: "main".to_owned(),
                title: "T".to_owned(),
                body: "Fixes #42".to_owned(),
                draft: true,
            }]
        );
        let again = answer(
            &mut world,
            &call(&["pr", "create", "--head", "issue-42-x", "--title", "T"]),
        );
        assert_eq!(again.code, 1);
    }

    #[test]
    fn a_failed_check_exits_one_and_a_pending_one_eight() {
        let mut pull = GhPull {
            number: 57,
            checks: vec![GhCheck {
                bucket: "pending".to_owned(),
                ..GhCheck::default()
            }],
            ..GhPull::default()
        };
        assert_eq!(checks(&pull).code, 8);
        pull.checks[0].bucket = "fail".to_owned();
        assert_eq!(checks(&pull).code, 1);
    }

    #[test]
    fn a_reply_takes_its_body_from_the_field() {
        let mut world = world();
        world.pulls.push(GhPull {
            number: 57,
            ..GhPull::default()
        });
        answer(
            &mut world,
            &call(&[
                "api",
                "--method",
                "POST",
                "repos/team/app/pulls/57/comments/7/replies",
                "-f",
                "body=Done in abc123",
            ]),
        );
        assert_eq!(
            world.posted,
            [GhPost::Reply {
                pull: 57,
                to: 7,
                body: "Done in abc123".to_owned(),
            }]
        );
    }
}
