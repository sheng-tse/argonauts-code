//! The `RunStarted` a new run's supervisor writes from `start.json`, with the commit the run
//! works from, and the base worktree at that commit.

use std::collections::BTreeMap;
use std::io::BufRead as _;
use std::path::Path;

use anyhow::{Context as _, bail};
use argonauts_core::events::RunEventKind;
use argonauts_core::events::run::{OverrideRecord, RunLifecycle, RunStarted};
use argonauts_core::paths::create_private_dir_all;
use argonauts_core::settings::delivery::HistorySettings;
use argonauts_core::{RunId, unborn};
use argonauts_registry::resolve;

use crate::app_settings::sources;
use crate::env::{AppEnv, FolderGit};
use crate::git::{git, has_commits, head, uncommitted_paths};
use crate::hidden_git::{self, Survey};
use crate::run_files::{SettingsRecord, StartRecord};
use crate::trust::TrustStore;

/// Longest title, in characters, before it's cut.
const TITLE_CHARS: usize = 60;

/// Where a new run starts from.
struct Base {
    commit: String,
    /// Your uncommitted files when the run started, for the record.
    uncommitted: Vec<String>,
    /// Whether the base holds them.
    included: bool,
    /// What you're told about how the base was taken.
    notes: Vec<String>,
}

/// The most files a note names before it says how many more.
const NAMED: usize = 5;

/// A new run's first event, with what to tell you about where it starts from.
pub(crate) fn run_started(
    env: &AppEnv,
    run: &RunId,
    run_dir: &Path,
    start: &StartRecord,
    recorded: &SettingsRecord,
) -> anyhow::Result<(RunStarted, Vec<String>)> {
    let root = &env.repo_root;
    let history = &recorded.settings.history;
    let base = match env.git() {
        FolderGit::Repository if has_commits(root)? => {
            repository_base(root, run_dir, start.request.include_uncommitted)?
        }
        FolderGit::Repository => {
            let first_commit = unborn::makes_first_commit(&recorded.settings);
            unborn_base(env, run, run_dir, history, first_commit)?
        }
        FolderGit::Hidden(_) | FolderGit::Off => folder_base(env, history)?,
    };
    let Base {
        commit: base_commit,
        uncommitted,
        included,
        notes,
    } = base;
    let started = started(env, start, recorded, base_commit, uncommitted, included)?;
    Ok((started, notes))
}

/// In a repository the base is `HEAD`, or a snapshot commit of `HEAD` plus your uncommitted
/// changes when the request included them.
fn repository_base(root: &Path, run_dir: &Path, include_uncommitted: bool) -> anyhow::Result<Base> {
    let uncommitted = uncommitted_paths(root)?;
    let included = include_uncommitted && !uncommitted.is_empty();
    let commit = if included {
        snapshot_commit(root, run_dir)?
    } else {
        head(root).context("reading the base commit")?
    };
    Ok(Base {
        commit,
        uncommitted,
        included,
        notes: Vec::new(),
    })
}

/// A repository with no commits has no `HEAD` to branch from, so the base is your files as
/// they are: what your index holds and what your ignore rules allow, by the sizes a folder
/// without git is recorded with. It's a commit with no parent, at a ref of the run's own and
/// built in a private index, so your branch, your staging area and your files stay as they
/// are. The run can't move a branch that has nothing on it, so its work comes back as files,
/// or as the branch's `first_commit` when your settings ask for that.
fn unborn_base(
    env: &AppEnv,
    run: &RunId,
    run_dir: &Path,
    history: &HistorySettings,
    first_commit: bool,
) -> anyhow::Result<Base> {
    let root = &env.repo_root;
    refuse_too_much(env)?;
    create_private_dir_all(run_dir)?;
    let index = run_dir.join("base.index");
    let snapshot = (|| {
        copy_your_index(root, &index)?;
        let new = git_with_index(
            root,
            &index,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )?;
        let survey = Survey::of(root, &new, history);
        survey.fits(history)?;
        survey.leaves_room(root, history)?;
        let add = survey.add_all();
        let add: Vec<&str> = add.iter().map(String::as_str).collect();
        git_with_index(root, &index, &add)?;
        let tree = git_with_index(root, &index, &["write-tree"])?;
        let message = "Argonauts run base: your files before your first commit";
        let commit = git_with_index(root, &index, &["commit-tree", &tree, "-m", message])?;
        git(root, &["update-ref", &unborn::base_ref(run), &commit])?;
        Ok::<_, anyhow::Error>((commit, survey.left_out))
    })();
    remove_index(&index);
    let (commit, left_out) = snapshot?;
    let lands = if first_commit {
        "Its finished work becomes your branch's first commit, together with those files, as \
         merge.unborn asks."
    } else {
        "Its finished work is written into your files for you to commit, and nothing is \
         committed or staged for you."
    };
    let mut notes = vec![format!(
        "This repository has no commits yet, so this workflow starts from your files as they \
         are. {lands}"
    )];
    notes.extend(left_out_note(&left_out));
    Ok(Base {
        commit,
        uncommitted: Vec::new(),
        included: false,
        notes,
    })
}

/// A private index that starts as a copy of yours, so the snapshot holds what you staged,
/// a file your ignore rules would hide included.
fn copy_your_index(root: &Path, index: &Path) -> anyhow::Result<()> {
    let yours = root.join(git(root, &["rev-parse", "--git-path", "index"])?);
    match std::fs::copy(&yours, index) {
        // Nothing staged yet.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("reading {}", yours.display())),
        Ok(_) => Ok(()),
    }
}

/// Your home folder and a file system's top hold far more than a project.
fn refuse_too_much(env: &AppEnv) -> anyhow::Result<()> {
    if let Some(what) = hidden_git::refused(&env.repo_root, env.user_home.as_deref()) {
        bail!(
            "this is {what}, which is too much to record for a workflow. Start Argonauts in \
             the project's own folder"
        );
    }
    Ok(())
}

fn left_out_note(left_out: &[String]) -> Option<String> {
    let left_out = named(left_out)?;
    Some(format!(
        "Left out of what the agents see, and left alone in your folder: {left_out}."
    ))
}

/// A folder without git has nothing committed, so the base is the folder as it is, recorded
/// in the history Argonauts keeps for it. A folder that kept none until now starts here, and
/// is told so: agents working side by side need branches, and chat and solo never did.
fn folder_base(env: &AppEnv, history: &HistorySettings) -> anyhow::Result<Base> {
    if !history.enabled {
        bail!(
            "this folder has no git, and history.enabled is off, so a workflow has nowhere to \
             keep its branches. Turn the setting on, or make the folder a repository with /git"
        );
    }
    refuse_too_much(env)?;
    let mut notes = Vec::new();
    if !env.git().has_history() {
        notes.push(format!(
            "This folder has no git, so this workflow keeps its history under {}. Your folder \
             gets no .git, and the finished work is written back into your files. /git changes \
             that.",
            env.display(env.paths.home())
        ));
    }
    hidden_git::turn_on(&env.state_dir)?;
    let snapshot = hidden_git::record(&env.repo_root, history, "Your folder before a run")?;
    notes.extend(left_out_note(&snapshot.left_out));
    Ok(Base {
        commit: snapshot.commit,
        uncommitted: Vec::new(),
        included: false,
        notes,
    })
}

/// The first few of `paths`, then how many more.
fn named(paths: &[String]) -> Option<String> {
    let shown = paths.len().min(NAMED);
    let mut text = paths.get(..shown)?.join(", ");
    if text.is_empty() {
        return None;
    }
    if paths.len() > shown {
        text.push_str(&format!(" and {} more", paths.len() - shown));
    }
    Some(text)
}

fn started(
    env: &AppEnv,
    start: &StartRecord,
    recorded: &SettingsRecord,
    base_commit: String,
    uncommitted: Vec<String>,
    included: bool,
) -> anyhow::Result<RunStarted> {
    let root = &env.repo_root;
    let request = &start.request;
    Ok(RunStarted {
        request: request.request.clone(),
        title: title(&request.request),
        mode: request.mode,
        forced: request.forced,
        workflow: recorded.settings.workflow.clone(),
        estimate: request.estimate.clone(),
        triage: request.triage.clone(),
        chat_turn: request.chat_turn.clone(),
        continues: request.continues.clone(),
        repo_root: root.clone(),
        base_branch: git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok(),
        base_commit,
        uncommitted,
        uncommitted_included: included,
        settings_sha256: recorded.sha256.clone(),
        settings: serde_json::to_value(&recorded.settings)?,
        overrides: override_records(env, start, recorded),
        harness_versions: BTreeMap::new(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        front_end: request.front_end.clone(),
        instructions: request.instructions.clone(),
        required_steps: request.required_steps.clone(),
    })
}

/// The first line of the request, cut to a length a header can show.
fn title(request: &str) -> String {
    let line = request
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if line.chars().count() <= TITLE_CHARS {
        return line.to_owned();
    }
    let cut: String = line.chars().take(TITLE_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

/// Each layer's changes when resolving again gives the recorded settings. When the files changed
/// since `start`, only the flags can be vouched for.
fn override_records(
    env: &AppEnv,
    start: &StartRecord,
    recorded: &SettingsRecord,
) -> Vec<OverrideRecord> {
    let trust = TrustStore::load(&env.state_dir);
    let mut sources = sources(env, &trust, start.overrides.clone(), start.interactive);
    sources.request = start.request.request_settings.clone();
    match resolve(&sources) {
        Ok(resolved) if resolved.sha256 == recorded.sha256 => resolved.overrides,
        _ => start
            .overrides
            .iter()
            .filter_map(|flag| flag.split_once('='))
            .map(|(key, value)| OverrideRecord {
                key: key.trim().to_owned(),
                value: value.trim().to_owned(),
                from: "flag".to_owned(),
            })
            .collect(),
    }
}

/// A commit of `HEAD` plus every uncommitted and untracked file git doesn't ignore, built in a
/// private index so your staging area stays as it is.
fn snapshot_commit(root: &Path, run_dir: &Path) -> anyhow::Result<String> {
    create_private_dir_all(run_dir)?;
    let index = run_dir.join("base.index");
    let result = (|| {
        git_with_index(root, &index, &["read-tree", "HEAD"])?;
        git_with_index(root, &index, &["add", "--all"])?;
        let tree = git_with_index(root, &index, &["write-tree"])?;
        git_with_index(
            root,
            &index,
            &[
                "commit-tree",
                &tree,
                "-p",
                "HEAD",
                "-m",
                "Argonauts run base: HEAD with your uncommitted changes",
            ],
        )
    })();
    remove_index(&index);
    result
}

fn remove_index(index: &Path) {
    if let Err(err) = std::fs::remove_file(index)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!("removing {}: {err}", index.display());
    }
}

fn git_with_index(root: &Path, index: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = crate::git::command(root)
        .args(args)
        .env("GIT_INDEX_FILE", index)
        .env("GIT_AUTHOR_NAME", "Argonauts")
        .env("GIT_AUTHOR_EMAIL", "argonauts@localhost")
        .env("GIT_COMMITTER_NAME", "Argonauts")
        .env("GIT_COMMITTER_EMAIL", "argonauts@localhost")
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The run's first event, which is always its `RunStarted`, without reading the rest.
pub(crate) fn first_started(journal: &Path) -> anyhow::Result<RunStarted> {
    let file =
        std::fs::File::open(journal).with_context(|| format!("opening {}", journal.display()))?;
    let mut first = String::new();
    std::io::BufReader::new(file)
        .read_line(&mut first)
        .with_context(|| format!("reading {}", journal.display()))?;
    let event: argonauts_core::events::RunEvent = serde_json::from_str(&first)
        .with_context(|| format!("reading the first event of {}", journal.display()))?;
    match event.kind {
        RunEventKind::Run(RunLifecycle::Started(started)) => Ok(*started),
        _ => bail!("{} doesn't start with RunStarted", journal.display()),
    }
}

/// `<worktrees>/base`, detached at `commit`, where read-only sessions work. A restarted
/// supervisor finds it in place.
pub(crate) fn base_worktree(root: &Path, base: &Path, commit: &str) -> anyhow::Result<()> {
    if base.join(".git").exists() {
        return Ok(());
    }
    if let Some(parent) = base.parent() {
        create_private_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let path = base.to_string_lossy();
    // --force takes back a path git still lists after its folder was deleted.
    git(
        root,
        &[
            "worktree", "add", "--force", "--detach", "--quiet", &path, commit,
        ],
    )
    .context("creating the run's base worktree")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use argonauts_core::Paths;
    use argonauts_testkit::TempRepo;

    use super::*;

    #[test]
    fn titles_take_the_first_line_and_cut_long_ones() {
        assert_eq!(title("\n  fix the login bug  \nmore"), "fix the login bug");
        let long = "a".repeat(80);
        let cut = title(&long);
        assert_eq!(cut.chars().count(), TITLE_CHARS);
        // The screens' own cut mark, never three dots in the middle of a header.
        assert!(cut.ends_with('…'));
        assert!(!cut.contains("..."));
    }

    #[test]
    fn an_included_change_lands_in_a_snapshot_commit() {
        let repo = TempRepo::new().unwrap();
        repo.write("README.md", "# Changed\n").unwrap();
        repo.write("new.txt", "untracked\n").unwrap();
        let run_dir = repo.scratch().join("run");
        let commit = snapshot_commit(repo.root(), &run_dir).unwrap();
        assert_ne!(commit, repo.head().unwrap());
        let listed = repo
            .git(&["show", "--name-only", "--format=", &commit])
            .unwrap();
        assert!(
            listed.contains("README.md") && listed.contains("new.txt"),
            "{listed}"
        );
        assert!(
            repo.git(&["status", "--porcelain"])
                .unwrap()
                .contains("?? new.txt"),
            "your index is untouched"
        );
        assert!(!run_dir.join("base.index").exists());

        let env = AppEnv::for_repo(Paths::new(repo.home()), repo.root(), false).unwrap();
        let base = repo.scratch().join("worktrees").join("base");
        base_worktree(&env.repo_root, &base, &commit).unwrap();
        assert_eq!(
            std::fs::read_to_string(base.join("new.txt")).unwrap(),
            "untracked\n"
        );
        base_worktree(&env.repo_root, &base, &commit).unwrap();
    }

    #[test]
    fn a_run_in_a_folder_without_git_starts_from_the_folder_as_it_is() {
        let folder = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("todo.txt"), "buy milk\n").unwrap();
        let env = AppEnv::for_repo(Paths::new(home.path()), folder.path(), false).unwrap();
        let settings = argonauts_core::settings::Settings::default();

        let base = folder_base(&env, &settings.history).unwrap();
        assert!(!env.repo_root.join(".git").exists());
        assert!(
            env.git().has_history(),
            "the workflow turned the hidden history on"
        );
        assert_eq!(base.commit, head(&env.repo_root).unwrap());
        assert_eq!(
            git(
                &env.repo_root,
                &["show", &format!("{}:todo.txt", base.commit)]
            )
            .unwrap(),
            "buy milk"
        );
        assert!(base.uncommitted.is_empty() && !base.included);
        assert!(
            matches!(base.notes.as_slice(), [note] if note.starts_with("This folder has no git, so")),
            "{:?}",
            base.notes
        );

        // The base worktree is made from that history, and the next run says nothing new.
        let tree = home.path().join("worktrees/base");
        base_worktree(&env.repo_root, &tree, &base.commit).unwrap();
        assert_eq!(
            std::fs::read_to_string(tree.join("todo.txt")).unwrap(),
            "buy milk\n"
        );
        std::fs::write(folder.path().join("todo.txt"), "buy oat milk\n").unwrap();
        let next = folder_base(&env, &settings.history).unwrap();
        assert_ne!(next.commit, base.commit);
        assert!(next.notes.is_empty(), "{:?}", next.notes);
    }

    #[test]
    fn a_workflow_is_refused_where_the_history_is_turned_off() {
        let folder = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let env = AppEnv::for_repo(Paths::new(home.path()), folder.path(), false).unwrap();
        let off = HistorySettings {
            enabled: false,
            ..HistorySettings::default()
        };
        let err = folder_base(&env, &off).err().unwrap();
        assert!(err.to_string().contains("history.enabled"), "{err}");
        assert!(!env.git().has_history());
    }

    /// A repository with no commits, some files, and its environment.
    fn unborn() -> (tempfile::TempDir, tempfile::TempDir, AppEnv) {
        let folder = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        git(folder.path(), &["init", "--quiet"]).unwrap();
        std::fs::write(folder.path().join("todo.txt"), "buy milk\n").unwrap();
        std::fs::write(folder.path().join(".gitignore"), "*.log\n").unwrap();
        std::fs::write(folder.path().join("debug.log"), "noise\n").unwrap();
        let env = AppEnv::for_repo(Paths::new(home.path()), folder.path(), false).unwrap();
        (folder, home, env)
    }

    #[test]
    fn a_run_in_a_repository_with_no_commits_starts_from_a_snapshot_of_your_files() {
        let (_folder, home, env) = unborn();
        let root = &env.repo_root;
        // Staged past the ignore rules, so it's yours to keep in the project.
        std::fs::write(root.join("kept.log"), "wanted\n").unwrap();
        git(root, &["add", "--force", "kept.log"]).unwrap();
        let status = git(root, &["status", "--porcelain"]).unwrap();
        let run = RunId::new("run-01").unwrap();
        let settings = argonauts_core::settings::Settings::default();

        let run_dir = home.path().join("run");
        let base = unborn_base(&env, &run, &run_dir, &settings.history, false).unwrap();

        let held = git(root, &["ls-tree", "-r", "--name-only", &base.commit]).unwrap();
        assert_eq!(held, ".gitignore\nkept.log\ntodo.txt");
        assert!(
            git(root, &["rev-list", "--parents", "-1", &base.commit])
                .unwrap()
                .split(' ')
                .nth(1)
                .is_none(),
            "the snapshot has no parent"
        );
        assert_eq!(
            git(root, &["rev-parse", "refs/argonauts/run-01/base"]).unwrap(),
            base.commit
        );
        assert!(
            matches!(base.notes.as_slice(), [note] if note.starts_with("This repository has no commits yet")
                && note.ends_with("nothing is committed or staged for you.")),
            "{:?}",
            base.notes
        );
        // Asked to make the first commit, the run says so from the start.
        let again = RunId::new("run-02").unwrap();
        let asked = unborn_base(&env, &again, &run_dir, &settings.history, true).unwrap();
        assert!(
            matches!(asked.notes.as_slice(), [note] if note.ends_with("becomes your branch's first commit, together with those files, as merge.unborn asks.")),
            "{:?}",
            asked.notes
        );

        // Your branch still has nothing on it, and your staging area is as you left it.
        assert!(!has_commits(root).unwrap());
        assert_eq!(git(root, &["status", "--porcelain"]).unwrap(), status);
        assert!(!run_dir.join("base.index").exists());

        let tree = home.path().join("worktrees/base");
        base_worktree(root, &tree, &base.commit).unwrap();
        assert_eq!(
            std::fs::read_to_string(tree.join("todo.txt")).unwrap(),
            "buy milk\n"
        );
    }

    #[test]
    fn a_snapshot_leaves_out_what_is_too_big_and_says_so() {
        let (_folder, home, env) = unborn();
        std::fs::write(env.repo_root.join("data.bin"), vec![0_u8; 2 * 1024 * 1024]).unwrap();
        let limits = HistorySettings {
            max_file_mb: 1,
            ..HistorySettings::default()
        };
        let run = RunId::new("run-01").unwrap();

        let base = unborn_base(&env, &run, &home.path().join("run"), &limits, false).unwrap();

        let held = git(
            &env.repo_root,
            &["ls-tree", "-r", "--name-only", &base.commit],
        )
        .unwrap();
        assert_eq!(held, ".gitignore\ntodo.txt");
        assert!(
            base.notes
                .iter()
                .any(|note| note.contains("data.bin (over 1 MB)")),
            "{:?}",
            base.notes
        );
    }
}
