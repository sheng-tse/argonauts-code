//! The folders the agents work in and what changed there, whatever CLI they are: `/cd` opens
//! the app in another repo, `/add-dir` lets the agents write one more folder, `/diff` shows
//! the working tree's changes, and the reviews read the changes from here.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, bail};
use argonauts_core::NoteLevel;
use argonauts_core::TaskId;
use argonauts_core::app::{AppEvent, ViewData};
use argonauts_core::base::log::DiffStat;
use argonauts_core::runs::api::DiffView;
use argonauts_core::settings::SettingScope;

use crate::controller::{Ctx, Effect};
use crate::git;

const EXTRA_WRITABLE: &str = "sandbox.extra_writable";
/// A review prompt carries at most this much of the diff; the reviewer reads the rest itself.
const REVIEW_DIFF_CHARS: usize = 60_000;

/// `~/` is your home, and anything else not absolute is from the repo root.
pub(crate) fn resolve_path(ctx: &Ctx, path: &Path) -> PathBuf {
    if let Ok(rest) = path.strip_prefix("~")
        && let Some(home) = &ctx.env.user_home
    {
        return normalize(&home.join(rest));
    }
    if path.is_absolute() {
        normalize(path)
    } else {
        normalize(&ctx.env.repo_root.join(path))
    }
}

/// Without `.` and `..`, as far as the path itself says.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// `/cd`: no path says where the agents work. A path in another repo closes this screen and
/// opens the app there; runs here keep going.
pub(crate) fn cd(
    ctx: &mut Ctx,
    path: Option<PathBuf>,
    conversation_settled: bool,
) -> anyhow::Result<Vec<Effect>> {
    let Some(path) = path else {
        ctx.notice(
            NoteLevel::Info,
            format!(
                "Working in {}. /cd <path> opens another repo.",
                ctx.env.display_repo()
            ),
        );
        return Ok(Vec::new());
    };
    let dir = resolve_path(ctx, &path);
    if !dir.is_dir() {
        bail!("{} isn't a folder.", ctx.env.display(&dir));
    }
    let root = git::project_root(&dir);
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    if same(&root, &ctx.env.repo_root) {
        ctx.notice(
            NoteLevel::Info,
            format!("Already working in {}.", ctx.env.display_repo()),
        );
        return Ok(Vec::new());
    }
    if !conversation_settled {
        ctx.notice(
            NoteLevel::Info,
            "A turn is still running. Esc stops it, then /cd opens the other repo.",
        );
        return Ok(Vec::new());
    }
    ctx.notice(
        NoteLevel::Info,
        format!(
            "Opening {}. Runs in {} keep going.",
            ctx.env.display(&root),
            ctx.env.display_repo()
        ),
    );
    ctx.reopen_in(root);
    Ok(vec![Effect::Exit])
}

/// `/add-dir <path>`: one more folder the agents may write. It lasts for this session, as
/// Claude Code's does, unless `asked` names a file to keep it in. Conversation turns stay
/// read-only.
pub(crate) fn add_dir(
    ctx: &mut Ctx,
    path: &Path,
    asked: Option<SettingScope>,
) -> anyhow::Result<Vec<Effect>> {
    let dir = resolve_path(ctx, path);
    let shown = ctx.env.display(&dir);
    if !dir.is_dir() {
        bail!("{shown} isn't a folder.");
    }
    let dir = dir.canonicalize().unwrap_or(dir);
    let inside = |outer: &Path| {
        let outer = outer.canonicalize().unwrap_or_else(|_| outer.to_owned());
        dir.starts_with(&outer)
    };
    if inside(&ctx.env.repo_root) {
        ctx.notice(
            NoteLevel::Info,
            format!("{shown} is in this repo, where the agents already work."),
        );
        return Ok(Vec::new());
    }
    let state = ctx.env.paths.home();
    if inside(state) || state.starts_with(&dir) {
        bail!(
            "{shown} holds Argonauts Code's own files, which agents never write. Pick a folder outside {}.",
            ctx.env.display(state)
        );
    }
    let entry = ctx.env.display(&dir);
    let mut dirs = ctx.settings().sandbox.extra_writable.clone();
    if dirs
        .iter()
        .any(|known| resolve_path(ctx, Path::new(known)) == dir)
    {
        ctx.notice(
            NoteLevel::Info,
            format!("The agents may already write {entry}."),
        );
        return Ok(Vec::new());
    }
    dirs.push(entry);
    Ok(vec![Effect::SaveSettings {
        pairs: vec![(EXTRA_WRITABLE.to_owned(), toml_list(&dirs))],
        scope: asked.unwrap_or(SettingScope::Session),
    }])
}

fn toml_list(items: &[String]) -> String {
    let quoted: Vec<String> = items
        .iter()
        .map(|item| toml::Value::String(item.clone()).to_string())
        .collect();
    format!("[{}]", quoted.join(", "))
}

/// `/diff`: everything that is different from the last commit, new files included, in the diff
/// view. Your index is left alone.
pub(crate) fn diff(ctx: &mut Ctx) -> anyhow::Result<Vec<Effect>> {
    let root = &ctx.env.repo_root;
    let text = working_tree_diff(root)?;
    if text.trim().is_empty() {
        ctx.notice(NoteLevel::Info, "No changes in the working tree.");
        return Ok(Vec::new());
    }
    let task = TaskId::new("working-tree").context("naming the working tree's diff")?;
    ctx.send(AppEvent::View {
        data: Box::new(ViewData::Diff {
            diff: DiffView {
                task,
                stat: stat_of(&text),
                text,
                path: root.clone(),
            },
        }),
    });
    Ok(Vec::new())
}

/// The working tree against `HEAD`, or against nothing before the first commit. A scratch
/// index takes the new files, so `git add` never touches yours.
pub(crate) fn working_tree_diff(root: &Path) -> anyhow::Result<String> {
    let index = git::git(root, &["rev-parse", "--git-path", "index"])?;
    let index = root.join(index);
    let scratch =
        ScratchIndex(index.with_file_name(format!("argonauts-diff-index-{}", std::process::id())));
    if index.exists() {
        git::copy_index(&index, &scratch.0).context("copying the index")?;
    }
    let with_scratch = |args: &[&str]| -> anyhow::Result<String> {
        let output = crate::git::command(root)
            .args(args)
            .env("GIT_INDEX_FILE", &scratch.0)
            .output()
            .with_context(|| format!("running git {}", args.join(" ")))?;
        if !output.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    };
    with_scratch(&["add", "--all", "--", "."])?;
    with_scratch(&["diff", "--cached", "--no-color", "--no-ext-diff"])
}

/// Removed when the diff is done, whatever happened.
struct ScratchIndex(PathBuf);

impl Drop for ScratchIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn stat_of(diff: &str) -> DiffStat {
    let mut stat = DiffStat {
        files_changed: 0,
        insertions: 0,
        deletions: 0,
    };
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            stat.files_changed += 1;
        } else if line.starts_with('+') && !line.starts_with("+++") {
            stat.insertions += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            stat.deletions += 1;
        }
    }
    stat
}

/// What a review looks at, and the changes it reads.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReviewChanges {
    /// Such as "the branch feature-x".
    pub target: String,
    pub text: String,
}

/// `None` when there's nothing to review. No target is the working tree; a pull request, as a
/// number, `#n`, or a URL, is left for the reviewer to read.
pub(crate) fn review_changes(
    root: &Path,
    target: Option<&str>,
) -> anyhow::Result<Option<ReviewChanges>> {
    let Some(target) = target.map(str::trim).filter(|target| !target.is_empty()) else {
        let text = working_tree_diff(root)?;
        return Ok((!text.trim().is_empty()).then(|| ReviewChanges {
            target: "the uncommitted changes".to_owned(),
            text: diff_part(&text),
        }));
    };
    if let Some(number) = pull_request(target) {
        return Ok(Some(ReviewChanges {
            target: format!("pull request {number}"),
            text: format!(
                "Read the pull request with `gh pr view {number}` and `gh pr diff {number}`, or \
                 from {target} if gh isn't there."
            ),
        }));
    }
    let (target, range) = if target.contains("..") {
        (format!("the commits {target}"), target.to_owned())
    } else {
        let commit = format!("{target}^{{commit}}");
        if !git::git_ok(root, &["rev-parse", "--verify", "--quiet", &commit])? {
            bail!("{target} isn't a branch, a commit, a range, or a pull request git knows.");
        }
        // Behind HEAD, it's the base and HEAD holds the work; otherwise it holds the work.
        if git::git_ok(root, &["merge-base", "--is-ancestor", target, "HEAD"])? {
            (
                format!("this branch since {target}"),
                format!("{target}..HEAD"),
            )
        } else {
            (format!("the branch {target}"), format!("HEAD...{target}"))
        }
    };
    let text = git::git(root, &["diff", "--no-color", "--no-ext-diff", &range])?;
    Ok((!text.trim().is_empty()).then(|| ReviewChanges {
        target,
        text: diff_part(&text),
    }))
}

/// `123`, `#123`, or a pull request's URL, as its number or the URL.
fn pull_request(target: &str) -> Option<String> {
    let number = target.strip_prefix('#').unwrap_or(target);
    if !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) {
        return Some(number.to_owned());
    }
    let url = target.starts_with("https://") && target.contains("/pull");
    url.then(|| target.to_owned())
}

fn diff_part(diff: &str) -> String {
    let mut text = String::from("The diff:\n\n```diff\n");
    if diff.len() > REVIEW_DIFF_CHARS {
        let mut end = REVIEW_DIFF_CHARS;
        while !diff.is_char_boundary(end) {
            end -= 1;
        }
        text.push_str(&diff[..end]);
        text.push_str("\n```\n\nThe diff is cut here; read the rest with git.");
    } else {
        text.push_str(diff.trim_end());
        text.push_str("\n```");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::base::test_support::{CliOptions, TestCtx};
    use argonauts_testkit::TempRepo;

    #[test]
    fn the_working_tree_diff_has_new_files_and_leaves_the_index_alone() {
        let repo = TempRepo::new().unwrap();
        let root = repo.root();
        repo.write("new.txt", "hello\n").unwrap();
        let status_before = git::git(root, &["status", "--porcelain"]).unwrap();
        let diff = working_tree_diff(root).unwrap();
        assert!(
            diff.contains("new.txt") && diff.contains("+hello"),
            "{diff}"
        );
        assert_eq!(
            git::git(root, &["status", "--porcelain"]).unwrap(),
            status_before
        );
        let stat = stat_of(&diff);
        assert_eq!((stat.files_changed, stat.insertions), (1, 1));
    }

    #[test]
    fn a_review_reads_a_branch_a_range_or_leaves_a_pull_request_to_the_reviewer() {
        let repo = TempRepo::new().unwrap();
        let root = repo.root();
        assert_eq!(review_changes(root, None).unwrap(), None);
        git::git(root, &["checkout", "-q", "-b", "feature"]).unwrap();
        repo.write("f.txt", "feature\n").unwrap();
        repo.commit_all("feature").unwrap();
        let base = git::git(root, &["rev-parse", "HEAD~1"]).unwrap();
        let since = review_changes(root, Some(&base)).unwrap().unwrap();
        assert!(since.target.starts_with("this branch since"), "{since:?}");
        assert!(since.text.contains("+feature"));
        git::git(root, &["checkout", "-q", &base]).unwrap();
        let branch = review_changes(root, Some("feature")).unwrap().unwrap();
        assert_eq!(branch.target, "the branch feature");
        assert!(branch.text.contains("+feature"));
        let pr = review_changes(root, Some("#42")).unwrap().unwrap();
        assert_eq!(pr.target, "pull request 42");
        assert!(pr.text.contains("gh pr diff 42"));
        assert!(review_changes(root, Some("no-such-branch")).is_err());
    }

    #[test]
    fn cd_opens_the_app_in_another_repo_once_the_conversation_is_settled() {
        let (mut parts, _) = TestCtx::new(CliOptions::default());
        let ctx = &mut parts.ctx;
        assert!(cd(ctx, None, true).unwrap().is_empty());
        assert!(cd(ctx, Some(PathBuf::from(".")), true).unwrap().is_empty());
        let other = TempRepo::new().unwrap();
        assert!(
            cd(ctx, Some(other.root().to_owned()), false)
                .unwrap()
                .is_empty()
        );
        assert!(ctx.reopen().lock().unwrap().is_none());
        let effects = cd(ctx, Some(other.root().to_owned()), true).unwrap();
        assert!(matches!(effects.as_slice(), [Effect::Exit]), "{effects:?}");
        assert_eq!(ctx.reopen().lock().unwrap().as_deref(), Some(other.root()));
        let plain = other.scratch().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        // A folder outside any repository opens like any other.
        let effects = cd(ctx, Some(plain.clone()), true).unwrap();
        assert!(matches!(effects.as_slice(), [Effect::Exit]), "{effects:?}");
        assert_eq!(
            ctx.reopen().lock().unwrap().as_deref(),
            Some(plain.as_path())
        );
        let mut said = Vec::new();
        while let Ok(event) = parts.events.try_recv() {
            said.push(event);
        }
        let said = format!("{said:?}");
        assert!(
            said.contains("Working in") && said.contains("Already working in"),
            "{said}"
        );
    }

    #[test]
    fn paths_resolve_from_the_repo_without_dots() {
        assert_eq!(
            normalize(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
        assert_eq!(
            toml_list(&["~/shared".to_owned(), "/x \"y\"".to_owned()]),
            r#"["~/shared", '/x "y"']"#
        );
    }
}
