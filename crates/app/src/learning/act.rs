//! What a Learning picker's keys and the shell's learning commands do. Each says what it did
//! in one line. Promotions and anything that writes an instruction file need you at the
//! terminal; what would change your branch arrives as a branch that waits for you.

use std::path::Path;
use std::process::{Command, Stdio};

use time::OffsetDateTime;

use argonauts_core::events::run::Party;
use argonauts_core::settings::Settings;
use argonauts_core::text::sentence_case;
use argonauts_core::views::LearningCommand;
use argonauts_crew::retro::{action_note, notes};
use argonauts_learning::Store;
use argonauts_learning::evolution::{Action, Evolution};
use argonauts_learning::git::{self, Entry};
use argonauts_learning::recipes;
use argonauts_learning::records::Book;
use argonauts_learning::replay::{self, ReplaySet};
use argonauts_learning::skills;
use argonauts_learning::sync::{self, SyncMode, SyncReport};

use crate::env::AppEnv;

/// Whoever asks: you at a terminal may promote and force; anything else may not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Asker {
    pub at_terminal: bool,
}

pub(crate) fn act(
    env: &AppEnv,
    settings: &Settings,
    command: &LearningCommand,
    asker: Asker,
) -> anyhow::Result<String> {
    let store = super::store(env);
    match command {
        LearningCommand::Rebuild => {
            let book = Book::rebuild(&store, &settings.routing, OffsetDateTime::now_utc())?;
            Ok(format!(
                "Rebuilt {} track records from the outcome log.",
                book.rows().len()
            ))
        }
        LearningCommand::Pin { worker, task_type } => {
            let harness = harness_of(worker);
            let book = Book::rebuild(&store, &settings.routing, OffsetDateTime::now_utc())?;
            let worker_ref = worker.parse()?;
            let rate = book.record(&worker_ref, task_type).pass.mean();
            let key = format!("routing.pins.{harness}.{task_type}");
            save(env, &key, &format!("{rate:.3}"))?;
            Ok(format!(
                "Pinned {harness}'s first-pass rate on {task_type} at {:.0}% in your local settings.",
                rate * 100.0
            ))
        }
        LearningCommand::Reset { worker } => {
            let harness = harness_of(worker);
            let today = OffsetDateTime::now_utc().date().to_string();
            save(
                env,
                &format!("routing.resets.{harness}"),
                &format!("\"{today}\""),
            )?;
            Ok(format!(
                "{harness}'s outcomes before {today} no longer count, in your local settings."
            ))
        }
        LearningCommand::Promote { candidate } | LearningCommand::Reject { candidate } => {
            if !asker.at_terminal {
                anyhow::bail!("promoting or rejecting a recipe needs you at a terminal");
            }
            let promote = matches!(command, LearningCommand::Promote { .. });
            let actions = Evolution::open(&store)?.decide(candidate, promote, &Party::User)?;
            Ok(told(
                &actions,
                if promote { "Promoted" } else { "Rejected" },
                candidate,
            ))
        }
        LearningCommand::Rollback { recipe } => {
            let actions = Evolution::open(&store)?.rollback(recipe, "rolled back by you")?;
            Ok(told(&actions, "Rolled back", recipe))
        }
        LearningCommand::Close { candidate } => close(&store, candidate),
        LearningCommand::Export { recipe } => export(&store, settings, recipe),
        LearningCommand::AcceptEntry { id } | LearningCommand::RejectEntry { id } => {
            if !asker.at_terminal {
                anyhow::bail!("what joins the replay set needs you at a terminal");
            }
            let set = ReplaySet::open(&store)?;
            if matches!(command, LearningCommand::RejectEntry { .. }) {
                set.reject(id)?;
                return Ok(format!("Turned {id} down for the replay set."));
            }
            // The conductor draws the side, never you or an agent, and the split records it.
            let entry = set.accept(id, replay::draw_seed())?;
            let side = if entry.holdout { "holdout" } else { "tuning" };
            Ok(format!("{id} joined the replay set's {side} tasks."))
        }
        LearningCommand::AcceptNote { id } => accept_note(&store, id),
        LearningCommand::DropNote { id } => {
            let path = flagged_path(&store, id)?;
            std::fs::remove_file(&path)?;
            Ok("Dropped the flagged note.".to_owned())
        }
        LearningCommand::Sync => {
            let report = sync::sync(&store, SyncMode::Write)?;
            Ok(sync_line(&report, &store))
        }
        LearningCommand::OpenPullRequest { branch } => open_pull_request(&store.repo_root, branch),
    }
}

/// Turns a promoted candidate down: a reversal against the run whose retro proposed it, then
/// the version before runs again. Only the version that runs now can be closed, so a rollback
/// never undoes a newer one.
fn close(store: &Store, id: &str) -> anyhow::Result<String> {
    use argonauts_learning::evolution::CandidateState;
    use argonauts_learning::outcomes::{self, LogLine, Reversal};
    let evolution = Evolution::open(store)?;
    let candidate = evolution
        .candidate(id)?
        .ok_or_else(|| anyhow::anyhow!("there's no candidate {id}"))?;
    if !matches!(
        candidate.state,
        CandidateState::Canary { .. } | CandidateState::Current
    ) {
        anyhow::bail!("{id} isn't promoted, so there's nothing to close");
    }
    let recipe = &candidate.parent.name;
    if evolution.running_version(recipe)? != Some(candidate.version) {
        anyhow::bail!(
            "{recipe} v{} isn't the version that runs now, so closing it would roll back another",
            candidate.version
        );
    }
    if let Some(run) = &candidate.from_run {
        let reversal = Reversal {
            run: run.clone(),
            card: None,
            what: format!(
                "you closed the promotion of {recipe} v{}",
                candidate.version
            ),
            at: OffsetDateTime::now_utc(),
        };
        outcomes::append(store, run, &[LogLine::Reversal(reversal)])?;
    }
    let actions = evolution.rollback(recipe, "closed by you")?;
    Ok(told(&actions, "Closed", id))
}

/// Track records key pins and resets by harness, a plain settings name.
fn harness_of(worker: &str) -> &str {
    worker.split('/').next().unwrap_or(worker)
}

/// Saves `key` in `.argonauts/config.local.toml`, just for you.
fn save(env: &AppEnv, key: &str, value: &str) -> anyhow::Result<()> {
    argonauts_registry::write_setting(&env.project_local_config(), key, value)
        .map_err(|err| anyhow::anyhow!("{err}"))
}

fn told(actions: &[Action], done: &str, what: &str) -> String {
    if actions.is_empty() {
        return format!("{done} {what}.");
    }
    actions
        .iter()
        .map(|action| action_note(action).text)
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn export(store: &Store, settings: &Settings, recipe: &str) -> anyhow::Result<String> {
    let sets = recipes::load_all(store)?;
    let loaded = sets
        .iter()
        .find(|set| set.name == recipe)
        .and_then(|set| set.current())
        .ok_or_else(|| anyhow::anyhow!("no recipe is named {recipe}"))?;
    let proposal = skills::propose_export(store, loaded, settings.skills.claude_link)?;
    Ok(format!(
        "Proposed the {recipe} skill on {}. It waits for you; {} opens it as a pull request. \
         For Hermes: hermes skills trust {}. OpenClaw sees it only when this repo is its agent's \
         workspace.",
        proposal.branch,
        proposal.pr_command.join(" "),
        store.repo_root.display()
    ))
}

fn flagged_path(store: &Store, id: &str) -> anyhow::Result<std::path::PathBuf> {
    let plain = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !plain {
        anyhow::bail!("{id:?} isn't a flagged note");
    }
    let path = store
        .state_dir
        .join(notes::FLAGGED_DIR)
        .join(format!("{id}.json"));
    if !path.is_file() {
        anyhow::bail!("no flagged note {id} waits for you");
    }
    Ok(path)
}

/// Commits a flagged note on its own branch for you to merge, then lets it go.
fn accept_note(store: &Store, id: &str) -> anyhow::Result<String> {
    let path = flagged_path(store, id)?;
    let flagged: notes::Flagged = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    let relative = format!(
        "{}/{}.md",
        notes::NOTES_DIR,
        notes::slug(&flagged.note.topic)
    );
    // The topic file as main has it, with the note added, built aside in the state folder.
    let scratch = store
        .state_dir
        .join(notes::FLAGGED_DIR)
        .join(format!("{id}.d"));
    let _ = std::fs::remove_dir_all(&scratch);
    let current =
        git::run(&store.repo_root, &["show", &format!("HEAD:{relative}")]).unwrap_or_default();
    if !current.is_empty() {
        let file = scratch.join(&relative);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file, format!("{current}\n"))?;
    }
    notes::write_note(&scratch, &flagged.note, &flagged.source)?;
    let text = std::fs::read_to_string(scratch.join(&relative));
    let _ = std::fs::remove_dir_all(&scratch);
    let text = text?;
    let branch = format!("argonauts/notes/{id}");
    let message = format!(
        "Note from {}: {}\n\nFlagged by the scan and accepted by you.\n\nArgonauts-Conductor: {}",
        flagged.source.card,
        flagged.note.topic.trim(),
        flagged.source.run
    );
    git::commit_on_branch(
        &store.repo_root,
        "HEAD",
        &branch,
        &[(relative, Entry::File(text))],
        &message,
    )?;
    std::fs::remove_file(&path)?;
    Ok(format!(
        "Committed the note on {branch}. Merge it to keep the note for later runs."
    ))
}

pub(crate) fn sync_line(report: &SyncReport, store: &Store) -> String {
    let names = |paths: &[std::path::PathBuf]| {
        paths
            .iter()
            .map(|path| {
                store
                    .repo_relative(path)
                    .unwrap_or(path)
                    .display()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut parts = Vec::new();
    if !report.written.is_empty() {
        parts.push(format!("wrote {}", names(&report.written)));
    }
    if !report.unchanged.is_empty() {
        parts.push(format!("{} already in sync", names(&report.unchanged)));
    }
    if !report.edited.is_empty() {
        parts.push(format!(
            "left {} alone, since you edited it; --force overwrites",
            names(&report.edited)
        ));
    }
    for drift in &report.skill_drift {
        parts.push(format!("the {} skill was edited by hand", drift.name));
    }
    parts.extend(report.warnings.iter().cloned());
    if parts.is_empty() {
        "Nothing to sync.".to_owned()
    } else {
        let mut line = parts.join("; ");
        line.push('.');
        sentence_case(&line)
    }
}

/// Opens `branch` as a pull request with `gh` where a remote exists; otherwise says how.
fn open_pull_request(repo: &Path, branch: &str) -> anyhow::Result<String> {
    let command = ["gh", "pr", "create", "--head", branch, "--fill"];
    let has_remote = git::run(repo, &["remote"]).is_ok_and(|remotes| !remotes.trim().is_empty());
    let has_gh = Command::new("gh")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !(has_remote && has_gh) {
        return Ok(format!(
            "{branch} waits for you. With a remote and gh, `{}` opens it as a pull request.",
            command.join(" ")
        ));
    }
    let out = Command::new(command[0])
        .args(&command[1..])
        .current_dir(repo)
        .stdin(Stdio::null())
        .output()?;
    if out.status.success() {
        let url = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        Ok(format!("Opened {branch} as a pull request: {url}"))
    } else {
        let why = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!(
            "gh pr create failed: {}",
            why.trim().lines().last().unwrap_or("no output")
        )
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::RunId;
    use argonauts_core::steps::card::MemoryProposal;
    use argonauts_learning::memory::Source;

    use super::*;

    #[test]
    fn a_flagged_note_you_accept_lands_on_its_own_branch() {
        let repo = argonauts_testkit::repo::TempRepo::new().unwrap();
        let state = tempfile::tempdir().unwrap();
        let store = Store::new(state.path(), repo.root());
        let flagged = notes::Flagged {
            note: MemoryProposal {
                topic: "review".to_owned(),
                text: "Reviewers check the limiter's headers.".to_owned(),
            },
            source: Source {
                run: RunId::new("run-04").unwrap(),
                card: argonauts_core::CardId::numbered(2),
                worker: "codex".parse().unwrap(),
                date: OffsetDateTime::UNIX_EPOCH,
                evidence: Vec::new(),
            },
            findings: Vec::new(),
        };
        let path = notes::flag(state.path(), &flagged).unwrap();
        let id = path.file_stem().unwrap().to_string_lossy().into_owned();
        let main_before = repo.head().unwrap();

        let said = accept_note(&store, &id).unwrap();
        assert!(said.contains(&format!("argonauts/notes/{id}")), "{said}");
        assert!(!path.exists());
        assert_eq!(repo.head().unwrap(), main_before, "your branch never moves");
        let text = repo
            .git(&[
                "show",
                &format!("argonauts/notes/{id}:.argonauts/notes/review.md"),
            ])
            .unwrap();
        assert!(text.contains("Reviewers check the limiter's headers. (card-002, codex, run-04"));
        assert!(accept_note(&store, &id).is_err());
        assert!(flagged_path(&store, "../escape").is_err());
    }
}
