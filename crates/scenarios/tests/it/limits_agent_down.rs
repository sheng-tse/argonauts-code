//! Agent outages: the breaker marks the agent down, its tasks pause with their work
//! committed, and the role's fallback mode decides what happens next.

use crate::limits_support;
use argonauts_core::events::approval::{AnswerAction, ApprovalEvent, ApprovalKind, FreeText};
use argonauts_core::events::run::Party;
use argonauts_core::events::session::{EndReason, SessionEvent};
use argonauts_core::events::shared::{BlackboardEvent, BlackboardKind, Recipient};
use argonauts_core::events::{AgentStatusEvent, FallbackEvent, RunEvent, RunEventKind};
use argonauts_core::limits::{HealthState, ModelState};
use argonauts_core::settings::fallback::FallbackMode;
use argonauts_scenarios::WAIT;
use argonauts_testkit::Key;
use limits_support::*;

/// One network error ending one session marks an agent down, so a single task shows the outage.
const DOWN_AT_ONCE: &str = "[health]\ndown_after = 1\ndown_sessions = 1\n";

fn ask_with_codex() -> String {
    format!("{DOWN_AT_ONCE}\n[fallback]\nmode = \"ask\"\norder = [\"codex\"]\n")
}

fn went_down(events: &[RunEvent], agent_id: &str) -> bool {
    !find(events, |kind| match kind {
        RunEventKind::AgentStatus(AgentStatusEvent::Health(health))
            if health.agent_id.0 == agent_id && health.state == HealthState::Down =>
        {
            Some(())
        }
        _ => None,
    })
    .is_empty()
}

fn model_out(events: &[RunEvent], agent_id: &str) -> bool {
    !find(events, |kind| match kind {
        RunEventKind::AgentStatus(AgentStatusEvent::Model(health))
            if health.model.agent == agent_id && health.state == ModelState::Out =>
        {
            Some(())
        }
        _ => None,
    })
    .is_empty()
}

#[test]
fn limits_agent_down_pauses_tasks() -> anyhow::Result<()> {
    let limits = setup("agent-down-pauses-tasks", &ask_with_codex())?;
    let pty = limits.start()?;
    let question = limits.wait_for_question(&pty, ApprovalKind::AgentDown)?;
    let events = limits.events();

    assert!(went_down(&events, "claude-code"));
    let ends = ends_of(&events, "claude-code");
    assert!(
        matches!(
            ends.first().map(|end| &end.reason),
            Some(EndReason::CliError { .. })
        ),
        "{ends:?}"
    );
    let packet = agent_down_packet(&question)?;
    assert_eq!(packet.agent_id.0, "claude-code");
    let paused: Vec<_> = packet
        .paused
        .iter()
        .map(|paused| paused.task.clone())
        .collect();
    assert_eq!(paused, first_task(&events).into_iter().collect::<Vec<_>>());
    assert!(before(
        seq_of(&events, committed),
        opened_at(&events, ApprovalKind::AgentDown)
    ));
    // Waiting comes first: a task that waits loses nothing.
    assert_eq!(option_keys(&question), ['w', 'a', 'e']);
    assert!(limits.calls_with("codex", "exec")?.is_empty());
    pty.wait_for("Claude down", WAIT)?;
    limits.shot(&pty, "limits_agent_down_pauses_tasks")
}

/// With `same_cli` a task stays on the CLI that went down. It waits there, and one question
/// asks about the CLI and no one task: it names the setting that keeps the task, and waiting
/// is your own answer already.
#[test]
fn limits_agent_down_asks_about_the_cli_its_tasks_are_kept_on() -> anyhow::Result<()> {
    let settings = format!(
        "{DOWN_AT_ONCE}\n[fallback]\nmode = \"auto\"\nsame_cli = true\norder = [\"codex\"]\n"
    );
    let limits = setup("agent-down-pauses-tasks", &settings)?;
    let pty = limits.start()?;
    let question = limits.wait_for_question(&pty, ApprovalKind::AgentDown)?;
    assert_eq!(question.task, None, "it asks about the CLI");
    let packet = agent_down_packet(&question)?;
    assert_eq!(packet.agent_id.0, "claude-code");
    assert_eq!(packet.kept_by.as_deref(), Some("same_cli"));
    assert_eq!(packet.paused.len(), 1);
    assert!(limits.calls_with("codex", "exec")?.is_empty());
    pty.wait_for("same_cli keeps it on Claude Code.", WAIT)?;
    pty.wait_for("your setting", WAIT)?;
    // Codex names no model here, so it is named once, by its CLI.
    pty.wait_for("Codex can take it", WAIT)?;
    pty.wait_for("Enter to choose · Tab to add a note", WAIT)?;
    limits.shot(&pty, "limits_agent_down_same_cli")
}

/// A note typed with the answer is for whoever takes the paused task: Tab opens it on the
/// answer, and Codex, which takes the task, is given it with its first prompt.
#[test]
fn limits_agent_down_passes_your_note_to_whoever_takes_the_task() -> anyhow::Result<()> {
    const NOTE: &str = "keep the old flag working too";
    let limits = setup("agent-down-one-agent-for-all", &ask_with_codex())?;
    let mut pty = limits.start()?;
    let question = limits.wait_for_question(&pty, ApprovalKind::AgentDown)?;
    assert_eq!(question.free_text, FreeText::Guidance);
    pty.wait_for("Enter to choose · Tab to add a note", WAIT)?;

    argonauts_scenarios::past_the_question_guard();
    pty.press(Key::Up)?;
    pty.press(Key::Down)?;
    pty.wait_for(r"❯ 2\. One agent for every paused task", WAIT)?;
    pty.press(Key::Tab)?;
    pty.wait_for("Enter to choose with the note", WAIT)?;
    pty.send(NOTE)?;
    pty.press(Key::Enter)?;

    let events = limits.wait_for("Codex to finish the task", LONG_WAIT, completed_by("codex"))?;
    let decided = find(&events, |kind| match kind {
        RunEventKind::Approval(ApprovalEvent::Decided(decided)) if decided.id == question.id => {
            Some((&decided.action, decided.text.as_deref()))
        }
        _ => None,
    });
    assert_eq!(decided, [(&AnswerAction::OneForAll, Some(NOTE))]);

    let task = first_task(&events).expect("the paused task");
    let posted = find(&events, |kind| match kind {
        RunEventKind::Blackboard(BlackboardEvent::Posted(message)) if message.text == NOTE => {
            Some(message)
        }
        _ => None,
    });
    assert_eq!(posted.len(), 1, "{posted:?}");
    let note = posted[0];
    assert_eq!(
        (note.kind, &note.from, &note.to),
        (
            BlackboardKind::Mention,
            &Party::User,
            &Recipient::Task { task }
        )
    );
    let given_to = find(&events, |kind| match kind {
        RunEventKind::Blackboard(BlackboardEvent::Delivered { message, to })
            if *message == note.id =>
        {
            Some(to)
        }
        _ => None,
    });
    let codex_sessions: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started))
                if started.worker.agent == "codex" =>
            {
                event.session.as_ref()
            }
            _ => None,
        })
        .collect();
    assert_eq!(given_to.len(), 1, "it is given once: {given_to:?}");
    assert!(codex_sessions.contains(&given_to[0]), "{codex_sessions:?}");
    Ok(())
}

#[test]
fn limits_agent_down_one_agent_for_all() -> anyhow::Result<()> {
    // passes when: it's settled whether "one agent for every paused task" takes the role's
    // first valid candidate, as here, or asks which agent with a picker.
    let limits = setup("agent-down-one-agent-for-all", &ask_with_codex())?;
    let mut pty = limits.start()?;
    let question = limits.wait_for_question(&pty, ApprovalKind::AgentDown)?;
    limits.shot(&pty, "limits_agent_down_one_agent_for_all")?;

    answer(&mut pty, &question, 'a')?;
    let events = limits.wait_for("Codex to finish the task", LONG_WAIT, completed_by("codex"))?;
    let decided = find(&events, |kind| match kind {
        RunEventKind::Approval(ApprovalEvent::Decided(decided)) if decided.id == question.id => {
            Some(&decided.action)
        }
        _ => None,
    });
    assert_eq!(decided, [&AnswerAction::OneForAll]);
    let paused = &agent_down_packet(&question)?.paused;
    for paused in paused {
        assert!(
            fell_back(&events).iter().any(|event| matches!(
                event,
                FallbackEvent::FellBack { task, to, .. } if *task == paused.task && to.agent == "codex"
            )),
            "{} was not passed to Codex",
            paused.task
        );
    }
    Ok(())
}

#[test]
fn limits_agent_down_follows_fallback_mode() -> anyhow::Result<()> {
    let limits = setup(
        "agent-down-follows-fallback-mode",
        &format!(
            "crew.main = \"antigravity-cli\"\n\n{DOWN_AT_ONCE}\n\
             [fallback]\nmode = \"ask\"\n\n\
             [fallback.worker]\nmode = \"auto\"\norder = [\"codex\"]\n"
        ),
    )?;
    let mut pty = limits.run(&[])?;
    let events = limits.wait_for("Codex to finish the task", LONG_WAIT, completed_by("codex"))?;

    // No capacity is about the one model: it goes out, and its CLI stays up.
    assert!(model_out(&events, "antigravity-cli"));
    assert!(!went_down(&events, "antigravity-cli"));
    assert!(
        opened(&events, ApprovalKind::AgentDown).is_none(),
        "the worker role falls back on its own, so nothing should ask"
    );
    assert!(fell_back(&events).iter().any(|event| matches!(
        event,
        FallbackEvent::FellBack { to, mode: FallbackMode::Auto, .. } if to.agent == "codex"
    )));
    pty.wait_for_exit(LONG_WAIT)?;
    Ok(())
}
