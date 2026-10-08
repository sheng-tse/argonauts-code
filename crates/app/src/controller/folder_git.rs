//! `/git`: how this folder keeps history, changed on the spot. A folder with no git picks
//! between off, a hidden history under Argonauts' home, and becoming a repository, which
//! shows what its first commit would hold and asks before anything is written. A repository
//! picks what a merged run leaves on your branch. Nothing here touches your folder or your
//! `.git` before you've seen what it will do and said yes.

use std::collections::HashMap;

use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, ChoiceKind};
use argonauts_core::events::checkpoint::{AnswerAction, AnswerOption};
use argonauts_core::names::ChoiceId;
use argonauts_core::paths::exclude_from_git;
use argonauts_core::settings::delivery::{DeliveryCommits, DeliveryWrite};
use argonauts_core::settings::{SettingScope, Settings};
use argonauts_core::unborn;

use super::{Ctx, Effect};
use crate::choices::{self, NOT_CHANGED, choose, option};
use crate::env::FolderGit;
use crate::git;
use crate::hidden_git::{self, Survey};
use crate::live_registry::live_supervisors;

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

/// The most files the first commit's card names before it says how many more.
const NAMED: usize = 6;

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
    /// Whether to go ahead, once the card has shown what the first commit would hold.
    MakeRepository(Survey),
    /// What a merged run leaves on your branch, in a repository.
    Delivery,
}

pub(crate) fn owns_choice(state: &GitState, id: &ChoiceId) -> bool {
    state.pending.contains_key(id)
}

/// `/git`: the card for what this folder is.
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
            "solo and chat write your files directly",
            *now == FolderGit::Off,
        ),
        way(
            'h',
            "Hidden",
            HIDDEN,
            &format!("Argonauts keeps its own history under {home}. Your folder gets no .git"),
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

/// What an answer came to: the line the card closes with, and what follows.
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
                    "This is {what}, which is too much to keep a history of. Start Argonauts \
                     in the project's own folder."
                );
            }
            hidden_git::turn_on(&env.state_dir)?;
            Ok(Answered::said(format!(
                "Argonauts keeps this folder's history under {home} from now on. /rewind \
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

/// A run in this folder works on the history as it is, so the history can't change under it.
fn no_run_open(ctx: &Ctx, change: &str) -> anyhow::Result<()> {
    let open = live_supervisors(&ctx.env.paths)
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

/// What the first commit would hold, for the card that asks: how much, the first few files,
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
        exclude_from_git(&env.repo_root, argonauts_core::paths::LOCAL_CONFIG_EXCLUDE)?;
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

#[cfg(test)]
mod tests {
    use argonauts_core::app::{AnswerTarget, AppChoice, UiCommand};
    use argonauts_core::input::SlashCommand;

    use super::*;
    use crate::controller::test_support::{Harness, HarnessOptions};

    /// The app in a folder with a few files and no git.
    async fn without_git() -> Harness {
        Harness::start_with(HarnessOptions {
            prepare: Some(Box::new(|repo| {
                std::fs::remove_dir_all(repo.root().join(".git")).unwrap();
                repo.write("todo.txt", "buy milk\n").unwrap();
            })),
            ..HarnessOptions::default()
        })
        .await
    }

    async fn ask_git(harness: &mut Harness) -> AppChoice {
        harness
            .send(UiCommand::Slash {
                command: SlashCommand::Git,
            })
            .await;
        question(harness).await
    }

    async fn question(harness: &mut Harness) -> AppChoice {
        match harness
            .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::Git))
            .await
        {
            AppEvent::Choice { choice } => choice,
            _ => unreachable!(),
        }
    }

    async fn pick(harness: &mut Harness, choice: &AppChoice, label: &str) -> String {
        let action = choice
            .options
            .iter()
            .find(|option| option.label == label)
            .unwrap_or_else(|| panic!("no option {label} in {:?}", choice.options))
            .action
            .clone();
        harness
            .send(UiCommand::Answer {
                to: AnswerTarget::Choice {
                    id: choice.id.clone(),
                },
                action,
                text: None,
                evidence_sha256: None,
                cards: Vec::new(),
                remember: false,
            })
            .await;
        let closed = harness
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

    #[tokio::test]
    async fn a_folder_without_git_picks_off_hidden_or_a_repository() {
        let mut harness = without_git().await;
        let root = harness.env.repo_root.clone();
        let card = ask_git(&mut harness).await;
        assert!(card.title.starts_with("Git for "), "{}", card.title);
        let options = shown(&card);
        assert_eq!(
            options[0],
            ("Off", "solo and chat write your files directly · now")
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
        assert!(card.options.iter().all(|option| !option.recommended));

        let outcome = pick(&mut harness, &card, "Hidden").await;
        assert!(outcome.contains("/rewind works"), "{outcome}");
        assert!(matches!(harness.env.git(), FolderGit::Hidden(_)));
        assert!(git::has_git(&root), "git calls reach the hidden history");
        assert!(!root.join(".git").exists());

        let card = ask_git(&mut harness).await;
        assert!(shown(&card)[1].1.ends_with("· now"), "{:?}", shown(&card));
        let outcome = pick(&mut harness, &card, "Off").await;
        assert!(
            outcome.starts_with("Git is off for this folder."),
            "{outcome}"
        );
        assert_eq!(harness.env.git(), FolderGit::Off);
        assert!(!git::has_git(&root));
        // The history is kept, so turning it on again loses nothing.
        assert!(
            hidden_git::dir(&harness.env.state_dir)
                .join("HEAD")
                .is_file()
        );
    }

    #[tokio::test]
    async fn making_a_repository_shows_the_first_commit_and_waits_for_yes() {
        let mut harness = without_git().await;
        let root = harness.env.repo_root.clone();
        std::fs::create_dir_all(root.join("vendor/lib/.git")).unwrap();
        let card = ask_git(&mut harness).await;
        harness
            .send(UiCommand::Answer {
                to: AnswerTarget::Choice {
                    id: card.id.clone(),
                },
                action: card.options[2].action.clone(),
                text: None,
                evidence_sha256: None,
                cards: Vec::new(),
                remember: false,
            })
            .await;
        let confirm = question(&mut harness).await;
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
        harness
            .send(UiCommand::SetAside {
                to: AnswerTarget::Choice {
                    id: confirm.id.clone(),
                },
            })
            .await;
        harness
            .until(|event| matches!(event, AppEvent::ChoiceClosed { id, .. } if *id == confirm.id))
            .await;
        assert!(!root.join(".git").exists());

        let card = ask_git(&mut harness).await;
        let action = card.options[2].action.clone();
        harness
            .send(UiCommand::Answer {
                to: AnswerTarget::Choice { id: card.id },
                action,
                text: None,
                evidence_sha256: None,
                cards: Vec::new(),
                remember: false,
            })
            .await;
        let confirm = question(&mut harness).await;
        let outcome = pick(
            &mut harness,
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
        assert_eq!(harness.env.git(), FolderGit::Repository);
        assert!(git::head(&root).is_err(), "no commit was made");

        // In a repository the same command offers what a merged run leaves on your branch,
        // and says what holds until there's a commit to merge onto.
        let delivery = ask_git(&mut harness).await;
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
        let delivery = ask_git(&mut harness).await;
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
    fn the_first_commit_is_made_under_your_name_with_what_the_card_showed() {
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
    fn sizes_read_in_the_unit_that_fits() {
        assert_eq!(size(9), "9 B");
        assert_eq!(size(2048), "2 KB");
        assert_eq!(size(5 * 1024 * 1024 / 2), "2.5 MB");
    }
}
