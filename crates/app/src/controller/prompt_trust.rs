//! Roles and prompts files that came with a repository are instructions for your agents that
//! someone else wrote. They wait until you trust them, and you are asked once.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, ChoiceKind};
use argonauts_core::events::checkpoint::AnswerAction;
use argonauts_core::names::ChoiceId;
use argonauts_crew::prompts::trust;

use super::{Ctx, Effect};
use crate::choices;

type Handled = anyhow::Result<Vec<Effect>>;

#[derive(Default)]
pub(crate) struct PromptTrustState {
    /// The open question's files.
    pending: HashMap<ChoiceId, Vec<PathBuf>>,
}

pub(crate) fn owns_choice(state: &PromptTrustState, id: &ChoiceId) -> bool {
    state.pending.contains_key(id)
}

/// At start: one question for the files that wait and that you weren't asked about yet.
pub(crate) fn ask_at_start(ctx: &mut Ctx, state: &mut PromptTrustState) {
    let files = trust::waiting(&ctx.env.repo_root, &ctx.env.state_dir).new;
    if files.is_empty() {
        return;
    }
    let mut body = vec![
        "This repository came with instructions, workflows or guide pages for your agents that \
         you haven't read:"
            .to_owned(),
    ];
    body.extend(
        files
            .iter()
            .map(|file| format!("  {}", ctx.env.shown(file))),
    );
    body.push(
        "Until you trust them they aren't used, and anyone who can change the repository could \
         have written them. /prompts trust says yes later."
            .to_owned(),
    );
    let id = ctx.next_choice_id();
    ctx.send(AppEvent::Choice {
        choice: choices::app_choice(
            id.clone(),
            ChoiceKind::TrustProject,
            "Use these instructions?",
            body,
            choices::options(ChoiceKind::TrustProject),
        ),
    });
    state.pending.insert(id, files);
}

pub(crate) fn answer_choice(
    ctx: &mut Ctx,
    state: &mut PromptTrustState,
    id: ChoiceId,
    action: AnswerAction,
) -> Handled {
    let Some(files) = state.pending.remove(&id) else {
        ctx.notice(NoteLevel::Info, "That question is already closed.");
        return Ok(Vec::new());
    };
    let outcome = match action {
        AnswerAction::Approve => {
            trust::trust(&ctx.env.state_dir, &files).context("trusting the files")?;
            format!(
                "Trusted {}. Agents that start from now on read them.",
                counted(&files)
            )
        }
        // "Show the file": print each, and leave the question open.
        AnswerAction::ShowDiff => {
            for file in &files {
                show_file(ctx, file);
            }
            state.pending.insert(id, files);
            return Ok(Vec::new());
        }
        _ => leave_unused(ctx, &files)?,
    };
    close(ctx, id, outcome);
    Ok(Vec::new())
}

/// A file a trust question is about, printed above the question as its path and its lines, so
/// you can read what your agents would. A long one stops after [`SHOWN_LINES`].
pub(crate) fn show_file(ctx: &Ctx, file: &Path) {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(err) => format!("It can't be read: {err}"),
    };
    let all: Vec<&str> = text.lines().collect();
    let mut lines: Vec<String> = all
        .iter()
        .take(SHOWN_LINES)
        .map(|line| (*line).to_owned())
        .collect();
    if all.len() > SHOWN_LINES {
        lines.push(format!(
            "… {} more lines in the file",
            all.len() - SHOWN_LINES
        ));
    }
    ctx.send(AppEvent::Report {
        title: ctx.env.shown(file),
        lines,
    });
}

/// How much of a file "Show the file" prints. The rest is the file's to show.
const SHOWN_LINES: usize = 200;

/// Esc: not now. The files stay unused, and you aren't asked again.
pub(crate) fn set_aside(ctx: &mut Ctx, state: &mut PromptTrustState, id: ChoiceId) -> Handled {
    if let Some(files) = state.pending.remove(&id) {
        let outcome = leave_unused(ctx, &files)?;
        close(ctx, id, outcome);
    }
    Ok(Vec::new())
}

/// `/prompts trust`: trusts every file that waits, asked about before or not.
pub(crate) fn trust_all(ctx: &Ctx) -> anyhow::Result<()> {
    let files: Vec<PathBuf> = trust::waiting(&ctx.env.repo_root, &ctx.env.state_dir)
        .all()
        .collect();
    if files.is_empty() {
        ctx.notice(
            NoteLevel::Info,
            "No roles or prompts file in this repository waits for your trust.",
        );
        return Ok(());
    }
    trust::trust(&ctx.env.state_dir, &files).context("trusting the files")?;
    let names: Vec<String> = files.iter().map(|file| ctx.env.shown(file)).collect();
    ctx.notice(
        NoteLevel::Info,
        format!(
            "Trusted {}. Agents that start from now on read them.",
            names.join(", ")
        ),
    );
    Ok(())
}

fn leave_unused(ctx: &Ctx, files: &[PathBuf]) -> anyhow::Result<String> {
    trust::decline(&ctx.env.state_dir, files).context("remembering your answer")?;
    Ok(format!(
        "{} unused. /prompts trust uses them.",
        match files.len() {
            1 => "The file stays".to_owned(),
            n => format!("The {n} files stay"),
        }
    ))
}

fn counted(files: &[PathBuf]) -> String {
    match files.len() {
        1 => "1 file".to_owned(),
        n => format!("{n} files"),
    }
}

fn close(ctx: &Ctx, id: ChoiceId, outcome: impl Into<String>) {
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: outcome.into(),
    });
}

#[cfg(test)]
mod tests {
    use argonauts_core::app::{AnswerTarget, UiCommand};
    use argonauts_core::input::{PromptsAction, SlashCommand};
    use argonauts_core::names::RoleName;
    use argonauts_testkit::TempRepo;

    use super::super::test_support::{Harness, HarnessOptions};
    use super::*;
    use crate::prompts::catalog;

    const ROLE: &str = ".argonauts/roles/reviewers.md";

    /// A repository as a clone has it: the role file is committed, and nothing here wrote it.
    fn cloned(repo: &TempRepo) {
        repo.write(ROLE, "Go easy on this code.\n").unwrap();
        repo.commit_all("add a reviewers file").unwrap();
    }

    async fn start(prepare: fn(&TempRepo)) -> Harness {
        Harness::start_with(HarnessOptions {
            prepare: Some(Box::new(prepare)),
            ..HarnessOptions::default()
        })
        .await
    }

    async fn question(harness: &mut Harness) -> argonauts_core::app::AppChoice {
        let asked = harness
            .until(|event| {
                matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::TrustProject)
            })
            .await;
        match asked {
            AppEvent::Choice { choice } => choice,
            _ => unreachable!(),
        }
    }

    async fn answer(harness: &Harness, id: &ChoiceId, action: AnswerAction) {
        harness
            .send(UiCommand::Answer {
                to: AnswerTarget::Choice { id: id.clone() },
                action,
                text: None,
                evidence_sha256: None,
                cards: Vec::new(),
                remember: false,
            })
            .await;
    }

    fn reviewers_file_is_used(harness: &Harness) -> bool {
        catalog::dirs(&harness.env)
            .role(RoleName::Reviewer)
            .is_some()
    }

    #[tokio::test]
    async fn a_cloned_role_file_asks_once_and_is_used_after_a_yes() {
        let mut harness = start(cloned).await;
        let asked = question(&mut harness).await;
        assert_eq!(asked.title, "Use these instructions?");
        assert!(
            asked
                .body
                .iter()
                .any(|line| line.contains("roles/reviewers.md"))
        );
        assert!(!reviewers_file_is_used(&harness));

        // "Show the file" prints what it says above the question, which stays open.
        answer(&harness, &asked.id, AnswerAction::ShowDiff).await;
        let shown = harness
            .until(|event| matches!(event, AppEvent::Report { .. }))
            .await;
        let AppEvent::Report { title, lines } = shown else {
            unreachable!()
        };
        assert!(title.ends_with("roles/reviewers.md"), "{title}");
        assert_eq!(lines, ["Go easy on this code."]);

        answer(&harness, &asked.id, AnswerAction::Approve).await;
        let closed = harness
            .until(|event| matches!(event, AppEvent::ChoiceClosed { .. }))
            .await;
        let AppEvent::ChoiceClosed { outcome, .. } = closed else {
            unreachable!()
        };
        assert!(outcome.starts_with("Trusted 1 file"), "{outcome}");
        assert!(reviewers_file_is_used(&harness));
    }

    #[tokio::test]
    async fn a_no_leaves_it_unused_until_prompts_trust() {
        let mut harness = start(cloned).await;
        let asked = question(&mut harness).await;
        answer(&harness, &asked.id, AnswerAction::Cancel).await;
        harness
            .until(|event| matches!(event, AppEvent::ChoiceClosed { .. }))
            .await;
        assert!(!reviewers_file_is_used(&harness));
        // Asked once: the file now waits as one you said no to.
        let waiting = trust::waiting(&harness.env.repo_root, &harness.env.state_dir);
        assert!(waiting.new.is_empty());
        assert_eq!(waiting.declined.len(), 1);

        harness
            .send(UiCommand::Slash {
                command: SlashCommand::Prompts {
                    action: PromptsAction::Trust,
                },
            })
            .await;
        let trusted = harness
            .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.starts_with("Trusted")))
            .await;
        assert!(matches!(trusted, AppEvent::Notice { .. }));
        assert!(reviewers_file_is_used(&harness));
    }

    /// The first start runs the setup, which asks about the folder's settings and not about
    /// its prompt files. They are asked about once the setup ends.
    #[tokio::test]
    async fn the_question_comes_after_the_setup_on_a_first_start() {
        use crate::controller::Opening;

        // Nothing real runs for the setup's checks: every CLI points nowhere.
        let ids = [
            "claude-code",
            "codex",
            "antigravity-cli",
            "opencode",
            "opencode-acp",
        ];
        let mut harness = Harness::start_with(HarnessOptions {
            flags: ids
                .iter()
                .map(|id| format!(r#"agents.{id}.binary="/nowhere/{id}""#))
                .collect(),
            opening: Opening {
                guided_setup: true,
                ..Opening::default()
            },
            prepare: Some(Box::new(cloned)),
            ..HarnessOptions::default()
        })
        .await;
        let welcome = harness
            .until(|event| matches!(event, AppEvent::Choice { .. }))
            .await;
        let AppEvent::Choice { choice: welcome } = welcome else {
            unreachable!()
        };
        assert_ne!(welcome.kind, ChoiceKind::TrustProject, "{welcome:?}");
        // Esc takes the setup's recommended answers.
        harness
            .send(UiCommand::SetAside {
                to: AnswerTarget::Choice { id: welcome.id },
            })
            .await;
        let asked = question(&mut harness).await;
        assert_eq!(asked.title, "Use these instructions?");
        assert!(!reviewers_file_is_used(&harness));
        // The setup left every template where you can edit it, and none in the repository.
        let yours = harness.env.paths.home().join("prompts");
        assert!(yours.join("README.md").is_file());
        assert!(yours.join("frames/judge.md").is_file());
        assert!(!harness.env.repo_root.join(".argonauts/prompts").exists());
    }

    #[tokio::test]
    async fn a_file_you_wrote_here_is_never_asked_about() {
        let harness = start(|repo| {
            repo.write(ROLE, "Check error handling.\n").unwrap();
        })
        .await;
        assert!(trust::waiting(&harness.env.repo_root, &harness.env.state_dir).is_empty());
        assert!(reviewers_file_is_used(&harness));
    }
}
