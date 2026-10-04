//! The `RunStarted` a new run's supervisor writes from `start.json`, with the commit the run
//! works from, and the base worktree at that commit.

use std::collections::BTreeMap;
use std::io::BufRead as _;
use std::path::Path;
use std::process::Command;

use anyhow::{Context as _, bail};
use argonauts_core::events::RunEventKind;
use argonauts_core::events::run::{OverrideRecord, RunLifecycle, RunStarted};
use argonauts_core::paths::create_private_dir_all;
use argonauts_registry::resolve;

use crate::app_settings::sources;
use crate::env::AppEnv;
use crate::git::{git, head, uncommitted_paths};
use crate::run_files::{SettingsRecord, StartRecord};
use crate::trust::TrustStore;

/// Longest title, in characters, before it's cut.
const TITLE_CHARS: usize = 60;

/// Takes the base at `HEAD`, or at a snapshot commit of `HEAD` plus your uncommitted changes
/// when the request included them.
pub(crate) fn run_started(
    env: &AppEnv,
    run_dir: &Path,
    start: &StartRecord,
    recorded: &SettingsRecord,
) -> anyhow::Result<RunStarted> {
    let root = &env.repo_root;
    let uncommitted = uncommitted_paths(root)?;
    let included = start.request.include_uncommitted && !uncommitted.is_empty();
    let base_commit = if included {
        snapshot_commit(root, run_dir)?
    } else {
        head(root).context("reading the base commit")?
    };
    let request = &start.request;
    Ok(RunStarted {
        request: request.request.clone(),
        title: title(&request.request),
        mode: request.mode,
        forced: request.forced,
        preset: recorded.settings.preset.clone(),
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
    if let Err(err) = std::fs::remove_file(&index)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!("removing {}: {err}", index.display());
    }
    result
}

fn git_with_index(root: &Path, index: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
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
}
