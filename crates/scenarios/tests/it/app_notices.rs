//! Things the app tells you once, or warns about: an edited workflow file, the old config file,
//! a committed local file, and the conversation message when a task needs you.

use crate::app_support;
use std::thread;
use std::time::Duration;

use anyhow::Context as _;
use app_support::*;
use argonauts_core::app::{AppEvent, ChoiceKind};
use argonauts_scenarios::{Scenario, as_set_up};
use argonauts_testkit::Pty;

/// Starts the app on a fresh trace and gives it a moment for its start-up notices.
fn restart(scenario: &Scenario, name: &str) -> anyhow::Result<Pty> {
    rotate_trace(scenario, name)?;
    let pty = start(scenario)?;
    thread::sleep(Duration::from_secs(1));
    Ok(pty)
}

/// The line a workflow file records its built-in's hash on, and the one remembering you were told
/// an upgrade kept your edits.
const HASH_LINE: &str = "# built-in sha256: ";
const TOLD_LINE: &str = "# kept your edits over built-in sha256: ";

fn notices_about(scenario: &Scenario, needle: &str) -> anyhow::Result<Vec<String>> {
    Ok(notices(scenario)?
        .into_iter()
        .filter(|text| text.contains(needle))
        .collect())
}

#[test]
fn app_workflow_edited_file_told_once() -> anyhow::Result<()> {
    // `workflow` sits above the config's first table.
    let scenario = scenario_with(&format!("workflow = \"council\"\n{GLOBAL_CONFIG}"))?;
    let copy = scenario.repo.home().join("workflows").join("council.toml");

    let mut pty = start(&scenario)?;
    submit(&mut pty, "/workflow copy council")?;
    wait_until("the workflow file", || Ok(copy.exists().then_some(())))?;
    quit(&mut pty)?;
    // As if an older version wrote it: the file records a built-in that has changed since, so
    // this start's upgrade finds your edit and keeps it.
    let written = read(&copy)?;
    let (header, rest) = written
        .split_once(HASH_LINE)
        .context("the copy records no built-in")?;
    let (_, body) = rest
        .split_once('\n')
        .context("nothing below the hash line")?;
    let edited = format!(
        "{header}{HASH_LINE}{}\n{body}# my own change\n",
        "0".repeat(64)
    );
    std::fs::write(&copy, &edited)?;

    let mut pty = restart(&scenario, "copied")?;
    let told = wait_until("the notice about the edited workflow", || {
        let told = notices_about(&scenario, "council")?;
        Ok((!told.is_empty()).then_some(told))
    })?;
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].contains("/workflow diff council"), "{told:?}");
    scenario.shot(&pty, "app_workflow_edited_file_told_once")?;
    quit(&mut pty)?;

    let pty = restart(&scenario, "told")?;
    assert!(
        notices_about(&scenario, "council")?.is_empty(),
        "the edited workflow was mentioned again"
    );
    // The header may remember that you were told; everything else is as you left it.
    let kept: String = read(&copy)?
        .lines()
        .filter(|line| !line.starts_with(TOLD_LINE))
        .map(|line| format!("{line}\n"))
        .collect();
    assert_eq!(kept, edited, "the app changed your workflow file");
    drop(pty);
    Ok(())
}

#[test]
fn app_old_config_file_told_once() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let old = scenario
        .repo
        .config_home()
        .join("argonauts")
        .join("config.toml");
    std::fs::create_dir_all(old.parent().expect("a parent"))?;
    std::fs::write(&old, "crew.main = \"codex\"\n")?;

    let mut pty = start(&scenario)?;
    let told = wait_until("the notice about the old file", || {
        let told = notices_about(&scenario, "config.toml")?;
        Ok((!told.is_empty()).then_some(told))
    })?;
    assert_eq!(told.len(), 1, "{told:?}");
    let global = scenario.repo.global_config();
    assert!(
        told[0].contains(&global.display().to_string()),
        "the notice doesn't say where the keys go now: {told:?}"
    );
    let status_agent = events(&scenario)?
        .into_iter()
        .find_map(|event| match event {
            AppEvent::Status { status } => Some(status.main_agent.to_string()),
            _ => None,
        });
    assert!(
        status_agent.is_none_or(|agent| agent.starts_with("claude-code")),
        "the old file's main agent was used"
    );
    scenario.shot(&pty, "app_old_config_file_told_once")?;
    quit(&mut pty)?;

    let pty = restart(&scenario, "told")?;
    assert!(
        notices_about(&scenario, "config.toml")?.is_empty(),
        "the old file was mentioned again"
    );
    drop(pty);
    Ok(())
}

#[test]
fn app_committed_local_file_warned() -> anyhow::Result<()> {
    let scenario = scenario()?;
    scenario.repo.write(
        ".argonauts/config.local.toml",
        "# only mine, or so I thought\n",
    )?;
    scenario.repo.commit_all("track the local settings file")?;
    let local_before = read(&project_local_config(&scenario))?;

    let mut pty = start(&scenario)?;
    submit(&mut pty, "/effort high")?;
    let question = wait_shown_choice(&scenario, &pty, ChoiceKind::SettingScope)?;
    let said = format!(
        "{}\n{}\n{}",
        question.title,
        question.body.join("\n"),
        question
            .options
            .iter()
            .filter_map(|o| o.detail.clone())
            .collect::<Vec<_>>()
            .join("\n")
    )
    .to_lowercase();
    assert!(
        said.contains("team") || said.contains("tracked") || said.contains("commit"),
        "no warning that git tracks the local file:\n{said}"
    );
    scenario.shot(&pty, "app_committed_local_file_warned")?;

    // Esc cancels the change.
    pty.press(argonauts_testkit::Key::Esc)?;
    wait_choice_closed(&scenario, &question.id)?;
    assert_eq!(read(&project_local_config(&scenario))?, local_before);
    Ok(())
}

#[test]
fn app_needs_you_sends_conversation_notification() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let hermes = scenario.clis.bin_dir().join("hermes");
    // Merges land on their own by default; this one waits for you.
    let config = format!(
        "{}\n[approvals.merge]\nmode = \"supervised\"\n\
         \n[notifications.conversation]\ncommand = [\"{}\", \"send\", \"--to\", \"me\", \"{{text}}\"]\n",
        run_config(),
        hermes.display()
    );
    std::fs::write(scenario.repo.global_config(), as_set_up(&config))?;

    let mut pty = start(&scenario)?;
    // Focus out, as when you switch to another window.
    pty.send("\x1b[O")?;
    let (run_dir, _) = reach_merge_question(&scenario, &mut pty)?;
    let run = run_id(&run_dir)?;

    let sends = wait_until("the conversation message", || {
        let sends = scenario.clis.message_sends()?;
        Ok((!sends.is_empty()).then_some(sends))
    })?;
    assert_eq!(sends.len(), 1, "{sends:?}");
    let argv = &sends[0];
    assert_eq!(argv.len(), 5, "the text isn't one whole argument: {argv:?}");
    assert_eq!(argv[..4], ["hermes", "send", "--to", "me"], "{argv:?}");
    let text = &argv[4];
    assert!(
        text.contains(run.as_str()),
        "the message doesn't name the run: {text}"
    );
    for agent_words in [
        "Updated README.md",
        "the install command now uses the scoped name",
    ] {
        assert!(
            !text.contains(agent_words),
            "agent text reached the message: {text}"
        );
    }
    scenario.shot(&pty, "app_needs_you_sends_conversation_notification")?;
    // Once the run has merged it waits for nobody, so a second message for the question would
    // have come by now.
    merge(&mut pty, &run_dir)?;
    let waits = scenario
        .clis
        .message_sends()?
        .into_iter()
        .filter(|argv| {
            argv.last()
                .is_some_and(|text| text.contains("waits for you"))
        })
        .count();
    assert_eq!(waits, 1, "one question sent more than one message");
    Ok(())
}
