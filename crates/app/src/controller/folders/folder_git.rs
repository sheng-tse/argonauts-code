//! `/git`: how this folder keeps history, changed on the spot. A folder with no git picks
//! between off, a hidden history under Argonauts Code's home, and becoming a repository, which
//! shows what its first commit would hold and asks before anything is written. A repository
//! picks what a merged run leaves on your branch. Nothing here touches your folder or your
//! `.git` before you've seen what it will do and said yes. A folder with no git that you never
//! turned off gets its hidden history the first time Captain may write there, so `/rewind`
//! always has something to go back to.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use anyhow::Context as _;
use argonauts_core::app::{AppEvent, ChoiceKind};
use argonauts_core::base::names::ChoiceId;
use argonauts_core::base::paths::exclude_from_git;
use argonauts_core::runs::api::{RunState, RunSummary};
use argonauts_core::runs::events::approval::{AnswerAction, AnswerOption};
use argonauts_core::settings::delivery::{DeliveryCommits, DeliveryWrite};
use argonauts_core::settings::{SettingScope, Settings};
use argonauts_core::{NoteLevel, RunId, runs::unborn};
use argonauts_crew::git::Git;
use argonauts_crew::merge::copies;

use crate::controller::{Ctx, Effect};
use crate::git;
use crate::git::hidden_git::{self, Survey};
use crate::host::live_registry::live_run_hosts;
use crate::setup::choices::{self, NOT_CHANGED, choose, option};
use crate::startup::env::FolderGit;

type Handled = anyhow::Result<Vec<Effect>>;

const OFF: &str = "off";
const HIDDEN: &str = "hidden";
const REPOSITORY: &str = "repository";
/// `git init` alone, leaving the first commit to you.
const INIT_ONLY: &str = "init";

/// The ways a merged run can reach your branch: what you pick, what it's called, what it
/// does, and the settings that say so.
const DELIVERIES: [(&str, &str, &str, DeliveryCommits, DeliveryWrite); 4] = [
    (
        "squash",
        "One commit",
        "a run merges as one commit on your branch",
        DeliveryCommits::Squash,
        DeliveryWrite::Commit,
    ),
    (
        "per_task",
        "A commit for each task",
        "each task of a run merges as a commit of its own",
        DeliveryCommits::PerTask,
        DeliveryWrite::Commit,
    ),
    (
        "as_run",
        "The run's own history",
        "every agent's commits, and the merges between them",
        DeliveryCommits::AsRun,
        DeliveryWrite::Commit,
    ),
    (
        "files",
        "Only write files",
        "a run changes your files and leaves the commit to you",
        DeliveryCommits::Squash,
        DeliveryWrite::Files,
    ),
];

/// The most files the first commit's question names before it says how many more.
const NAMED: usize = 6;

/// Folders whose history Captain found past the limits this session, so each turn after doesn't
/// count their files again.
static TOO_BIG: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// Before a Captain turn that may write in `root`: in a folder with no git, the hidden history
/// turns on, so `/rewind` can put back what the turn changes. `Some` is what to tell you, the
/// one time it turns on. It stays off where you turned it off with `/git`, where
/// `history.enabled` is off, in your home folder and at the top of the file system, and for a
/// folder past the history's limits, as a workflow's first record would be.
pub(crate) fn keep_history_for_captain(ctx: &Ctx, root: &Path) -> Option<String> {
    let env = &ctx.env;
    let history = &ctx.settings().history;
    let turned_off = hidden_git::dir(&env.state_dir)
        .join(argonauts_core::runs::hidden_git::OFF)
        .exists();
    if root != env.repo_root || env.git() != FolderGit::Off || !history.enabled || turned_off {
        return None;
    }
    if hidden_git::refused(root, env.user_home.as_deref()).is_some() {
        return None;
    }
    let mut too_big = TOO_BIG.lock().unwrap_or_else(PoisonError::into_inner);
    if too_big.contains(root) {
        return None;
    }
    let fits = hidden_git::first_commit_within(root, &env.state_dir, history);
    if let Err(err) = fits {
        tracing::info!("Captain keeps no history of {}: {err:#}", root.display());
        too_big.insert(root.to_path_buf());
        return None;
    }
    drop(too_big);
    if let Err(err) = hidden_git::turn_on(&env.state_dir) {
        tracing::warn!("turning the hidden history on for Captain: {err:#}");
        return None;
    }
    Some(format!(
        "This folder has no git, so Captain keeps its history under {}, and /rewind can put \
         back what it changes. Your folder gets no .git. /git changes that.",
        env.display(env.paths.home())
    ))
}

/// How a run lands in a repository with no commits, whichever of the choices is in use.
fn before_a_first_commit(settings: &Settings) -> &'static str {
    if unborn::makes_first_commit(settings) {
        "Until your first commit, a run makes it: your files as it finds them, with its work, \
         as merge.unborn asks."
    } else {
        "Until your first commit, a run's work is written into your files for you to commit."
    }
}

#[derive(Default)]
pub(crate) struct GitState {
    /// Open `/git` questions.
    pending: HashMap<ChoiceId, Pending>,
}

enum Pending {
    /// Off, hidden, or a repository, in a folder with no git.
    Folder,
    /// Whether to go ahead, once the question has shown what the first commit would hold.
    MakeRepository(Survey),
    /// What a merged run leaves on your branch, in a repository.
    Delivery,
}

pub(crate) fn owns_choice(state: &GitState, id: &ChoiceId) -> bool {
    state.pending.contains_key(id)
}

/// `/git`: the question for what this folder is.
pub(crate) fn command(ctx: &mut Ctx, state: &mut GitState) -> Handled {
    let mut body = Vec::new();
    let (pending, options) = match ctx.env.git() {
        FolderGit::Repository => {
            // The choices below take hold with the first commit. Unreadable counts as having
            // one, so a git failure adds no wrong line here.
            if !git::has_commits(&ctx.env.repo_root).unwrap_or(true) {
                body.push(format!(
                    "This repository has no commits yet. {}",
                    before_a_first_commit(ctx.settings())
                ));
            }
            (Pending::Delivery, delivery_options(ctx))
        }
        now => (Pending::Folder, folder_options(ctx, &now)),
    };
    let title = format!("Git for {}", ctx.env.display_repo());
    ask(ctx, state, pending, title, body, options);
    Ok(Vec::new())
}

fn ask(
    ctx: &mut Ctx,
    state: &mut GitState,
    pending: Pending,
    title: String,
    body: Vec<String>,
    options: Vec<AnswerOption>,
) {
    let id = ctx.next_choice_id();
    ctx.send(AppEvent::Choice {
        choice: choices::app_choice(id.clone(), ChoiceKind::Git, title, body, options),
    });
    state.pending.insert(id, pending);
}

/// One option, saying what it does, with the one in use marked. None is recommended: which
/// way a folder keeps history is yours to say.
fn way(key: char, label: &str, value: &str, does: &str, now: bool) -> AnswerOption {
    let mut option = option(key, label, choose(value), false);
    option.detail = Some(if now {
        format!("{does} · now")
    } else {
        does.to_owned()
    });
    option
}

fn folder_options(ctx: &Ctx, now: &FolderGit) -> Vec<AnswerOption> {
    let home = ctx.env.display(ctx.env.paths.home());
    vec![
        way(
            'o',
            "Off",
            OFF,
            "Captain and the conversation write your files directly",
            *now == FolderGit::Off,
        ),
        way(
            'h',
            "Hidden",
            HIDDEN,
            &format!("Argonauts Code keeps its own history under {home}. Your folder gets no .git"),
            matches!(now, FolderGit::Hidden(_)),
        ),
        way(
            'r',
            "Make this a repository",
            REPOSITORY,
            "runs git init here and commits what's there as the first commit",
            false,
        ),
    ]
}

fn delivery_options(ctx: &Ctx) -> Vec<AnswerOption> {
    let delivery = &ctx.settings().delivery;
    let files_only = delivery.write == DeliveryWrite::Files;
    DELIVERIES
        .iter()
        .zip('1'..)
        .map(|((value, label, does, commits, write), key)| {
            let now = if files_only {
                *write == DeliveryWrite::Files
            } else {
                *write == DeliveryWrite::Commit && *commits == delivery.commits
            };
            way(key, label, value, does, now)
        })
        .collect()
}

pub(crate) fn answer_choice(
    ctx: &mut Ctx,
    state: &mut GitState,
    id: ChoiceId,
    action: AnswerAction,
) -> Handled {
    let Some(pending) = state.pending.remove(&id) else {
        ctx.notice(NoteLevel::Info, "That question is already closed.");
        return Ok(Vec::new());
    };
    let answered = match (&pending, &action) {
        (Pending::Folder, AnswerAction::Choose { value }) => folder(ctx, state, value),
        (Pending::MakeRepository(survey), AnswerAction::Approve) => {
            make_repository(ctx, survey, true)
        }
        (Pending::MakeRepository(survey), AnswerAction::Choose { value }) if value == INIT_ONLY => {
            make_repository(ctx, survey, false)
        }
        (Pending::Delivery, AnswerAction::Choose { value }) => delivery(value),
        (_, AnswerAction::Cancel) => Ok(Answered::said(NOT_CHANGED)),
        (_, other) => {
            state.pending.insert(id, pending);
            anyhow::bail!("{other:?} doesn't answer the /git question");
        }
    };
    match answered {
        Ok(answered) => {
            close(ctx, id, answered.outcome);
            Ok(answered.effects)
        }
        Err(err) => {
            close(ctx, id, NOT_CHANGED);
            Err(err)
        }
    }
}

/// Esc: nothing changes.
pub(crate) fn set_aside(ctx: &mut Ctx, state: &mut GitState, id: ChoiceId) -> Handled {
    if state.pending.remove(&id).is_some() {
        close(ctx, id, NOT_CHANGED);
    }
    Ok(Vec::new())
}

fn close(ctx: &Ctx, id: ChoiceId, outcome: impl Into<String>) {
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: outcome.into(),
    });
}

/// What an answer came to: the line the question closes with, and what follows.
struct Answered {
    outcome: String,
    effects: Vec<Effect>,
}

impl Answered {
    fn said(outcome: impl Into<String>) -> Self {
        Self {
            outcome: outcome.into(),
            effects: Vec::new(),
        }
    }
}

fn folder(ctx: &mut Ctx, state: &mut GitState, value: &str) -> anyhow::Result<Answered> {
    let env = ctx.env.clone();
    let now = env.git();
    let home = env.display(env.paths.home());
    match value {
        OFF if now == FolderGit::Off => Ok(Answered::said(NOT_CHANGED)),
        OFF => {
            no_run_open(ctx, "turn its history off")?;
            hidden_git::turn_off(&env.state_dir)?;
            Ok(Answered::said(format!(
                "Git is off for this folder. Its history stays under {home}, and a workflow \
                 turns it back on."
            )))
        }
        HIDDEN if matches!(now, FolderGit::Hidden(_)) => Ok(Answered::said(NOT_CHANGED)),
        HIDDEN => {
            if let Some(what) = hidden_git::refused(&env.repo_root, env.user_home.as_deref()) {
                anyhow::bail!(
                    "This is {what}, which is too much to keep a history of. Start Argonauts Code \
                     in the project's own folder."
                );
            }
            hidden_git::turn_on(&env.state_dir)?;
            Ok(Answered::said(format!(
                "Argonauts Code keeps this folder's history under {home} from now on. /rewind \
                 works, and your folder gets no .git."
            )))
        }
        REPOSITORY => {
            no_run_open(ctx, "make it a repository")?;
            let survey =
                hidden_git::first_commit(&env.repo_root, &env.state_dir, &ctx.settings().history)?;
            let title = format!("Make {} a git repository?", env.display_repo());
            let body = first_commit_lines(&survey);
            let options = vec![
                option(
                    'y',
                    "Yes, with that first commit",
                    AnswerAction::Approve,
                    false,
                ),
                option(
                    'i',
                    "Only git init, and leave the commits to me",
                    choose(INIT_ONLY),
                    false,
                ),
                option('n', "No", AnswerAction::Cancel, false),
            ];
            ask(
                ctx,
                state,
                Pending::MakeRepository(survey),
                title,
                body,
                options,
            );
            Ok(Answered::said(format!(
                "Making {} a repository: nothing has run yet.",
                env.display_repo()
            )))
        }
        other => anyhow::bail!("{other} isn't one of the /git choices"),
    }
}

/// `/git clean`: removes the copies and branches that ended runs left behind, and in a folder
/// without git packs the hidden history, which is where the room goes. It's never run for
/// you. A run that waits, or that you stopped and may resume, keeps what it has. A run left on
/// its branch or sent up as a pull request keeps that branch, and so does a failed one, whose
/// work is nowhere else.
pub(crate) async fn clean(ctx: &mut Ctx) -> Handled {
    let now = ctx.env.git();
    if now == FolderGit::Off {
        anyhow::bail!(
            "This folder keeps no history, so there is nothing to clean. /git turns one on."
        );
    }
    no_run_open(ctx, "clean")?;
    let runs = ctx
        .services
        .conductor
        .list()
        .await
        .context("listing this folder's runs")?;
    let root = ctx.env.repo_root.clone();
    let mut git = Git::new();
    if let FolderGit::Hidden(hidden) = &now {
        git = git.with_hidden(root.clone(), hidden.clone());
    }
    let before = hidden_git::size(&ctx.env.state_dir);
    let worktrees = ctx
        .env
        .paths
        .home()
        .join("worktrees")
        .join(ctx.env.repo_id.as_str());
    let mut cleaned = 0;
    for run in left_behind(&worktrees, &runs) {
        let copies = worktrees.join(run.id.as_str());
        let removed = copies::remove_copies(&git, &root, &copies, &copies.join("merge")).await;
        let branches = if run.keeps_branches {
            0
        } else {
            copies::remove_branches(&git, &root, &run.id).await
        };
        if removed.count() + branches > 0 {
            cleaned += 1;
        }
    }
    let sizes = match &now {
        FolderGit::Hidden(hidden) => {
            git.pack(hidden)
                .await
                .context("packing the hidden history")?;
            Some(before.zip(hidden_git::size(&ctx.env.state_dir)))
        }
        _ => None,
    };
    let (title, line) = cleaned_report(cleaned, sizes);
    ctx.send(AppEvent::Report {
        title,
        lines: vec![line],
    });
    Ok(vec![Effect::Status])
}

/// A run with something `/git clean` may remove.
struct LeftBehind {
    id: RunId,
    /// It was left on its branch or sent up as a pull request, so the branch is its result,
    /// or it failed, and its work is only on its branches.
    keeps_branches: bool,
}

/// The runs whose copies and branches can go: the ended ones, and the ones whose folder of
/// copies outlived their record, such as an archived run's.
fn left_behind(worktrees: &Path, runs: &[RunSummary]) -> Vec<LeftBehind> {
    let mut found: Vec<LeftBehind> = runs
        .iter()
        .filter(|run| !run.live && over_for_good(run.state))
        .map(|run| LeftBehind {
            id: run.run.clone(),
            keeps_branches: matches!(
                run.state,
                RunState::BranchLeft | RunState::PullRequest | RunState::Failed
            ),
        })
        .collect();
    let listed = |name: &str| runs.iter().any(|run| run.run.as_str() == name);
    let orphans = std::fs::read_dir(worktrees)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !listed(name))
        .filter_map(|name| RunId::new(&name).ok());
    found.extend(orphans.map(|id| LeftBehind {
        id,
        keeps_branches: false,
    }));
    found
}

/// Whether nothing of a run in this state runs again. A stopped run opens again with
/// `/resume`, and so does one whose run host died.
fn over_for_good(state: RunState) -> bool {
    matches!(
        state,
        RunState::Merged
            | RunState::PullRequest
            | RunState::BranchLeft
            | RunState::Abandoned
            | RunState::Failed
            | RunState::NeverStarted
            | RunState::FromAnotherBuild
    )
}

/// `18 MB`, in whole megabytes, and at least 1 for anything there at all.
pub(crate) fn megabytes(bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    match bytes {
        0 => "0 MB".to_owned(),
        bytes if bytes >= 1024 * MB => format!("{:.1} GB", bytes as f64 / (1024 * MB) as f64),
        bytes => format!("{} MB", ((bytes + MB / 2) / MB).max(1)),
    }
}

/// A run in this folder works on the history as it is, so the history can't change under it.
fn no_run_open(ctx: &Ctx, change: &str) -> anyhow::Result<()> {
    let open = live_run_hosts(&ctx.env.paths)
        .iter()
        .any(|live| live.repo_root == ctx.env.repo_root);
    if open {
        anyhow::bail!(
            "A run is still going in this folder. /git can {change} once it has finished or \
             you've stopped it."
        );
    }
    Ok(())
}

/// What the first commit would hold, for the question that asks: how much, the first few files,
/// and what stays out with why.
fn first_commit_lines(survey: &Survey) -> Vec<String> {
    let count = survey.take.len();
    let mut lines = vec![match count {
        0 => "git init runs here. There are no files to commit yet.".to_owned(),
        1 => format!(
            "git init runs here, then a first commit of 1 file ({}):",
            size(survey.bytes)
        ),
        n => format!(
            "git init runs here, then a first commit of {n} files ({}):",
            size(survey.bytes)
        ),
    }];
    lines.extend(
        survey
            .take
            .iter()
            .take(NAMED)
            .map(|path| format!("  {path}")),
    );
    if count > NAMED {
        lines.push(format!("  … and {} more", count - NAMED));
    }
    if !survey.left_out.is_empty() {
        let shown = survey.left_out.len().min(NAMED);
        let mut left = survey.left_out[..shown].join(", ");
        if survey.left_out.len() > shown {
            left.push_str(&format!(" and {} more", survey.left_out.len() - shown));
        }
        lines.push(format!("Left out: {left}"));
    }
    lines
}

fn size(bytes: u64) -> String {
    const KB: u64 = 1024;
    match bytes {
        b if b < KB => format!("{b} B"),
        b if b < KB * KB => format!("{} KB", b.div_ceil(KB)),
        b => format!("{:.1} MB", b as f64 / (KB * KB) as f64),
    }
}

fn make_repository(ctx: &mut Ctx, survey: &Survey, commit: bool) -> anyhow::Result<Answered> {
    let env = ctx.env.clone();
    no_run_open(ctx, "make it a repository")?;
    // The settings that are just yours were kept with the folder's state. A repository
    // keeps them in the folder, hidden from git.
    let own_settings = env.project_local_config();
    let committed = hidden_git::make_repository(&env.repo_root, survey, commit)?;
    let kept = env.project_local_config();
    if own_settings.is_file() && own_settings != kept && !kept.exists() {
        if let Some(dir) = kept.parent() {
            std::fs::create_dir_all(dir)?;
        }
        exclude_from_git(
            &env.repo_root,
            argonauts_core::base::paths::LOCAL_CONFIG_EXCLUDE,
        )?;
        std::fs::rename(&own_settings, &kept)?;
    }
    let what = match (commit, committed, survey.take.len()) {
        (false, ..) => "Nothing is committed yet.".to_owned(),
        (true, true, 1) => "Its first commit holds 1 file.".to_owned(),
        (true, true, n) => format!("Its first commit holds {n} files."),
        (true, false, 0) => "There was nothing to commit yet.".to_owned(),
        (true, false, _) => "Git knows no name and email for you, so the files are staged and \
                             the first commit is yours to make."
            .to_owned(),
    };
    let then = if committed {
        "From here on a run merges as one commit.".to_owned()
    } else {
        format!(
            "{} Once it has a first commit, a run merges as one commit.",
            before_a_first_commit(ctx.settings())
        )
    };
    Ok(Answered::said(format!(
        "{} is a git repository now. {what} {then}",
        env.display_repo()
    )))
}

fn delivery(value: &str) -> anyhow::Result<Answered> {
    let Some((_, label, does, commits, write)) =
        DELIVERIES.iter().find(|(known, ..)| *known == value)
    else {
        anyhow::bail!("{value} isn't one of the /git choices");
    };
    let mut pairs = vec![("delivery.write".to_owned(), toml_text(write))];
    if *write == DeliveryWrite::Commit {
        pairs.push(("delivery.commits".to_owned(), toml_text(commits)));
    }
    Ok(Answered {
        outcome: format!("{label}: {does}."),
        effects: vec![Effect::SaveSettings {
            pairs,
            scope: SettingScope::ProjectLocal,
        }],
    })
}

/// A setting's value as the TOML text the settings writer takes, such as `"squash"`.
fn toml_text(value: impl serde::Serialize) -> String {
    toml::Value::try_from(value)
        .map(|value| value.to_string())
        .unwrap_or_default()
}

/// What `/git clean` reports: its heading and the line under it. `hidden` is `Some` for a
/// folder without git, with the bytes its history took before and after where both were read.
fn cleaned_report(cleaned: usize, hidden: Option<Option<(u64, u64)>>) -> (String, String) {
    let kept = "a paused run keeps its copies for /resume";
    let runs = match cleaned {
        0 => "no ended run had left anything behind".to_owned(),
        1 => "removed what 1 ended run left behind".to_owned(),
        runs => format!("removed what {runs} ended runs left behind"),
    };
    match hidden {
        Some(sizes) => {
            let sizes = sizes.map_or_else(String::new, |(before, after)| {
                format!(" · {} →  {}", megabytes(before), megabytes(after))
            });
            (
                format!("Cleaned this folder's hidden history{sizes}"),
                format!("{runs} and packed the rest · {kept}"),
            )
        }
        None => (
            "Cleaned what ended runs left in this repository".to_owned(),
            format!("{runs} · your own branches and commits are untouched · {kept}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::app::{AnswerTarget, AppChoice, UiCommand};
    use argonauts_core::input::SlashCommand;

    use super::*;
    use crate::controller::base::test_support::{CliOptions, TestDriver};

    /// The report `/git clean` prints, with the numbers of its picture, reads as
    /// the picture's two rows do.
    #[test]
    fn the_clean_report_reads_as_its_picture() {
        const MB: u64 = 1024 * 1024;
        let picture = include_str!("../../../../tui/tests/golden/no_git_fixes.txt");
        let rows: Vec<&str> = picture
            .lines()
            .skip_while(|row| !row.starts_with("● Cleaned this folder's hidden history"))
            .take(2)
            .collect();
        let (title, line) = cleaned_report(3, Some(Some((18 * MB, 7 * MB))));
        assert_eq!(rows, [format!("● {title}"), format!("  ⎿  {line}")]);

        let (title, line) = cleaned_report(1, None);
        assert_eq!(title, "Cleaned what ended runs left in this repository");
        assert!(
            line.starts_with("removed what 1 ended run left behind · your own"),
            "{line}"
        );
    }

    /// The app in a folder with a few files and no git.
    async fn without_git() -> TestDriver {
        TestDriver::start_with(CliOptions {
            prepare: Some(Box::new(|repo| {
                std::fs::remove_dir_all(repo.root().join(".git")).unwrap();
                repo.write("todo.txt", "buy milk\n").unwrap();
            })),
            ..CliOptions::default()
        })
        .await
    }

    async fn ask_git(driver: &mut TestDriver) -> AppChoice {
        driver
            .send(UiCommand::Slash {
                command: SlashCommand::Git,
            })
            .await;
        question(driver).await
    }

    async fn question(driver: &mut TestDriver) -> AppChoice {
        match driver
            .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::Git))
            .await
        {
            AppEvent::Choice { choice } => choice,
            _ => unreachable!(),
        }
    }

    async fn pick(driver: &mut TestDriver, choice: &AppChoice, label: &str) -> String {
        let action = choice
            .options
            .iter()
            .find(|option| option.label == label)
            .unwrap_or_else(|| panic!("no option {label} in {:?}", choice.options))
            .action
            .clone();
        driver
            .send(UiCommand::Answer {
                to: AnswerTarget::Choice {
                    id: choice.id.clone(),
                },
                action,
                text: None,
                evidence_sha256: None,
                tasks: Vec::new(),
                remember: false,
            })
            .await;
        let closed = driver
            .until(|event| matches!(event, AppEvent::ChoiceClosed { id, .. } if *id == choice.id))
            .await;
        match closed {
            AppEvent::ChoiceClosed { outcome, .. } => outcome,
            _ => unreachable!(),
        }
    }

    fn shown(choice: &AppChoice) -> Vec<(&str, &str)> {
        choice
            .options
            .iter()
            .map(|option| {
                (
                    option.label.as_str(),
                    option.detail.as_deref().unwrap_or(""),
                )
            })
            .collect()
    }

    /// The app in a folder without git where every request goes to Captain, whose session
    /// changes `todo.txt`.
    async fn captain_without_git() -> TestDriver {
        let driver = TestDriver::start_with(CliOptions {
            prepare: Some(Box::new(|repo| {
                std::fs::remove_dir_all(repo.root().join(".git")).unwrap();
                repo.write("todo.txt", "buy milk\n").unwrap();
                std::fs::write(
                    repo.global_config(),
                    "[captain_or_crew]\nforce = \"captain\"\n",
                )
                .unwrap();
            })),
            ..CliOptions::default()
        })
        .await;
        driver.agent.respond(|spec| {
            std::fs::write(spec.workdir.join("todo.txt"), "buy bread\n").unwrap();
            argonauts_testkit::FakeSession::completed("Changed the list.")
        });
        driver
    }

    async fn captain_turn(driver: &mut TestDriver) {
        driver.submit("Put bread on the list").await;
        driver
            .until(|event| {
                matches!(
                    event,
                    AppEvent::Conversation {
                        event: argonauts_core::app::ConversationEvent::TurnFinished { .. }
                    }
                )
            })
            .await;
    }

    fn said_captain_keeps_history(events: &[AppEvent]) -> bool {
        events.iter().any(|event| {
            matches!(event, AppEvent::Notice { text, .. } if text.contains("Captain keeps its history"))
        })
    }

    #[tokio::test]
    async fn captains_first_write_turns_the_hidden_history_on_so_rewind_works() {
        let mut driver = captain_without_git().await;
        let root = driver.env.repo_root.clone();
        assert_eq!(driver.env.git(), FolderGit::Off);
        captain_turn(&mut driver).await;
        assert!(matches!(driver.env.git(), FolderGit::Hidden(_)));
        assert_eq!(
            std::fs::read_to_string(root.join("todo.txt")).unwrap(),
            "buy bread\n"
        );
        assert!(!root.join(".git").exists(), "your folder gets no .git");

        driver
            .send(UiCommand::Slash {
                command: SlashCommand::Rewind { task: None },
            })
            .await;
        let AppEvent::Choice { choice } = driver
            .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::RewindTurn))
            .await
        else {
            unreachable!()
        };
        let label = choice.options[0].label.clone();
        let outcome = pick(&mut driver, &choice, &label).await;
        assert_eq!(outcome, "Put back todo.txt");
        assert_eq!(
            std::fs::read_to_string(root.join("todo.txt")).unwrap(),
            "buy milk\n"
        );
        let events = driver.exit().await;
        let told: Vec<&AppEvent> = events
            .iter()
            .filter(|event| said_captain_keeps_history(std::slice::from_ref(*event)))
            .collect();
        assert_eq!(told.len(), 1, "said once");
        let AppEvent::Notice { text, .. } = told[0] else {
            unreachable!()
        };
        assert!(
            text.starts_with("This folder has no git, so Captain keeps its history under ")
                && text.ends_with(
                    ", and /rewind can put back what it changes. Your folder gets no .git. /git \
                     changes that."
                ),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_history_you_turned_off_stays_off_for_captain() {
        let mut driver = captain_without_git().await;
        hidden_git::turn_on(&driver.env.state_dir).unwrap();
        hidden_git::turn_off(&driver.env.state_dir).unwrap();
        captain_turn(&mut driver).await;
        assert_eq!(driver.env.git(), FolderGit::Off);
        let events = driver.exit().await;
        assert!(!said_captain_keeps_history(&events));
    }

    #[tokio::test]
    async fn a_folder_without_git_picks_off_hidden_or_a_repository() {
        let mut driver = without_git().await;
        let root = driver.env.repo_root.clone();
        let asked = ask_git(&mut driver).await;
        assert!(asked.title.starts_with("Git for "), "{}", asked.title);
        let options = shown(&asked);
        assert_eq!(
            options[0],
            (
                "Off",
                "Captain and the conversation write your files directly · now"
            )
        );
        assert_eq!(options[1].0, "Hidden");
        assert!(
            options[1].1.ends_with("Your folder gets no .git"),
            "{options:?}"
        );
        assert_eq!(
            options[2],
            (
                "Make this a repository",
                "runs git init here and commits what's there as the first commit"
            )
        );
        assert!(asked.options.iter().all(|option| !option.recommended));

        let outcome = pick(&mut driver, &asked, "Hidden").await;
        assert!(outcome.contains("/rewind works"), "{outcome}");
        assert!(matches!(driver.env.git(), FolderGit::Hidden(_)));
        assert!(git::has_git(&root), "git calls reach the hidden history");
        assert!(!root.join(".git").exists());

        let asked = ask_git(&mut driver).await;
        assert!(shown(&asked)[1].1.ends_with("· now"), "{:?}", shown(&asked));
        let outcome = pick(&mut driver, &asked, "Off").await;
        assert!(
            outcome.starts_with("Git is off for this folder."),
            "{outcome}"
        );
        assert_eq!(driver.env.git(), FolderGit::Off);
        assert!(!git::has_git(&root));
        // The history is kept, so turning it on again loses nothing.
        assert!(
            hidden_git::dir(&driver.env.state_dir)
                .join("HEAD")
                .is_file()
        );
    }

    #[tokio::test]
    async fn making_a_repository_shows_the_first_commit_and_waits_for_yes() {
        let mut driver = without_git().await;
        let root = driver.env.repo_root.clone();
        std::fs::create_dir_all(root.join("agent/lib/.git")).unwrap();
        let asked = ask_git(&mut driver).await;
        driver
            .send(UiCommand::Answer {
                to: AnswerTarget::Choice {
                    id: asked.id.clone(),
                },
                action: asked.options[2].action.clone(),
                text: None,
                evidence_sha256: None,
                tasks: Vec::new(),
                remember: false,
            })
            .await;
        let confirm = question(&mut driver).await;
        assert!(confirm.title.starts_with("Make "), "{}", confirm.title);
        assert!(
            confirm.title.ends_with(" a git repository?"),
            "{}",
            confirm.title
        );
        let body = confirm.body.join("\n");
        assert!(
            body.contains("then a first commit of") && body.contains("  todo.txt"),
            "{body}"
        );
        assert!(!root.join(".git").exists(), "nothing ran before the answer");

        // Esc: nothing changes.
        driver
            .send(UiCommand::SetAside {
                to: AnswerTarget::Choice {
                    id: confirm.id.clone(),
                },
            })
            .await;
        driver
            .until(|event| matches!(event, AppEvent::ChoiceClosed { id, .. } if *id == confirm.id))
            .await;
        assert!(!root.join(".git").exists());

        let asked = ask_git(&mut driver).await;
        let action = asked.options[2].action.clone();
        driver
            .send(UiCommand::Answer {
                to: AnswerTarget::Choice { id: asked.id },
                action,
                text: None,
                evidence_sha256: None,
                tasks: Vec::new(),
                remember: false,
            })
            .await;
        let confirm = question(&mut driver).await;
        let outcome = pick(
            &mut driver,
            &confirm,
            "Only git init, and leave the commits to me",
        )
        .await;
        assert!(outcome.contains("is a git repository now"), "{outcome}");
        assert!(outcome.contains("Nothing is committed yet"), "{outcome}");
        assert!(
            outcome.ends_with(
                "Until your first commit, a run's work is written into your files for you to \
                 commit. Once it has a first commit, a run merges as one commit."
            ),
            "{outcome}"
        );
        assert_eq!(driver.env.git(), FolderGit::Repository);
        assert!(git::head(&root).is_err(), "no commit was made");

        // In a repository the same command offers what a merged run leaves on your branch,
        // and says what holds until there's a commit to merge onto.
        let delivery = ask_git(&mut driver).await;
        assert!(
            matches!(delivery.body.as_slice(), [line] if line.starts_with("This repository has no commits yet.")),
            "{:?}",
            delivery.body
        );
        let labels: Vec<&str> = shown(&delivery).iter().map(|(label, _)| *label).collect();
        assert_eq!(
            labels,
            [
                "One commit",
                "A commit for each task",
                "The run's own history",
                "Only write files"
            ]
        );
        assert!(shown(&delivery)[0].1.ends_with("· now"));

        let identity = ["-c", "user.name=You", "-c", "user.email=you@example.com"];
        git::git(&root, &["add", "todo.txt"]).unwrap();
        git::git(
            &root,
            &[&identity[..], &["commit", "--quiet", "-m", "first"]].concat(),
        )
        .unwrap();
        let delivery = ask_git(&mut driver).await;
        assert!(delivery.body.is_empty(), "{:?}", delivery.body);
    }

    #[test]
    fn what_holds_before_a_first_commit_follows_your_settings() {
        use argonauts_core::settings::delivery::UnbornLanding;

        let mut settings = Settings::default();
        assert!(before_a_first_commit(&settings).ends_with("for you to commit."));
        settings.merge.unborn = UnbornLanding::FirstCommit;
        assert!(before_a_first_commit(&settings).contains("a run makes it"));
        // Only writing files wins: nothing is ever committed for you.
        settings.delivery.write = DeliveryWrite::Files;
        assert!(before_a_first_commit(&settings).ends_with("for you to commit."));
    }

    #[test]
    fn the_first_commit_is_made_under_your_name_with_what_the_question_showed() {
        let folder = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let root = folder.path();
        std::fs::write(root.join("todo.txt"), "buy milk\n").unwrap();
        std::fs::write(root.join("big.bin"), vec![0u8; 2 * 1024 * 1024]).unwrap();
        let limits = argonauts_core::settings::delivery::HistorySettings {
            max_file_mb: 1,
            ..Default::default()
        };
        let survey = hidden_git::first_commit(root, state.path(), &limits).unwrap();
        assert_eq!(survey.take, ["todo.txt"]);
        assert_eq!(
            first_commit_lines(&survey),
            [
                "git init runs here, then a first commit of 1 file (9 B):",
                "  todo.txt",
                "Left out: big.bin (over 1 MB)"
            ]
        );
        assert!(!root.join(".git").exists(), "looking leaves nothing behind");
        assert_eq!(std::fs::read_dir(state.path()).unwrap().count(), 0);

        hidden_git::make_repository(root, &survey, true).unwrap();
        assert!(root.join(".git").is_dir());
        // Whether the commit was made depends on git knowing who you are, so either the
        // commit holds the file or the file is staged for it.
        let committed = git::git(root, &["ls-tree", "-r", "--name-only", "HEAD"]).ok();
        let staged = git::git(root, &["diff", "--cached", "--name-only"]).unwrap();
        assert!(
            committed.as_deref() == Some("todo.txt") || staged == "todo.txt",
            "{committed:?} {staged}"
        );
        assert!(root.join("big.bin").is_file());
    }

    #[test]
    fn the_history_is_sized_in_whole_megabytes() {
        const MB: u64 = 1024 * 1024;
        assert_eq!(megabytes(0), "0 MB");
        assert_eq!(megabytes(4096), "1 MB");
        assert_eq!(megabytes(18 * MB + 100), "18 MB");
        assert_eq!(megabytes(7 * MB - 1), "7 MB");
        assert_eq!(megabytes(3 * 1024 * MB / 2), "1.5 GB");
    }

    #[test]
    fn only_runs_that_are_over_for_good_leave_something_to_clean() {
        let worktrees = tempfile::tempdir().unwrap();
        // An archived run's copies outlived its record.
        std::fs::create_dir_all(worktrees.path().join("run-01")).unwrap();
        let run = |id: &str, state: RunState, live: bool| RunSummary {
            run: RunId::new(id).unwrap(),
            title: id.to_owned(),
            name: None,
            mode: argonauts_core::runs::events::run::RunMode::Crew,
            workflow: String::new(),
            state,
            stopped_at: String::new(),
            updated: time::OffsetDateTime::UNIX_EPOCH,
            spent: argonauts_core::Spend::default(),
            sessions_to_resume: 0,
            waiting: Vec::new(),
            live,
            remind: false,
            shape_known: true,
        };
        let runs = [
            run("run-02", RunState::Merged, false),
            run("run-03", RunState::BranchLeft, false),
            run("run-04", RunState::Waiting, false),
            run("run-05", RunState::Stopped, false),
            run("run-06", RunState::Crashed, false),
            run("run-07", RunState::Failed, true),
            run("run-08", RunState::Failed, false),
        ];
        let found = left_behind(worktrees.path(), &runs);
        let ids: Vec<(&str, bool)> = found
            .iter()
            .map(|run| (run.id.as_str(), run.keeps_branches))
            .collect();
        assert_eq!(
            ids,
            [
                ("run-02", false),
                ("run-03", true),
                ("run-08", true),
                ("run-01", false)
            ],
            "a run that waits, was stopped or still goes keeps what it has, and a failed \
             one keeps the branches its work is on"
        );
    }

    #[test]
    fn sizes_read_in_the_unit_that_fits() {
        assert_eq!(size(9), "9 B");
        assert_eq!(size(2048), "2 KB");
        assert_eq!(size(5 * 1024 * 1024 / 2), "2.5 MB");
    }
}
