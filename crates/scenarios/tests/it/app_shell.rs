//! `!` shell mode in the real app: the command runs in the repo, its output shows in the
//! transcript, and the main agent gets both with your next message.

use crate::app_support;
use app_support::*;
use argonauts_core::app::{AppEvent, ShellEvent, UiCommand};
use argonauts_scenarios::WAIT;
use argonauts_testkit::Key;

fn finished(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::Shell {
            event: ShellEvent::Finished { .. }
        }
    )
}

#[test]
fn app_shell_mode_runs_echo_hi() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    pty.wait_for(r"Argonauts Code v?\d+\.\d+\.\d+", WAIT)?;

    check_bottom_row(&pty)?;
    pty.send("!")?;
    pty.wait_for("! for shell mode", WAIT)?;
    pty.send("echo hi")?;
    pty.wait_for("! echo hi", WAIT)?;
    check_no_history_search(&pty)?;
    pty.press(Key::Enter)?;

    let end = wait_event(&scenario, "the command's end", finished)?;
    assert_eq!(
        end,
        AppEvent::Shell {
            event: ShellEvent::Finished {
                id: 1,
                exit_code: Some(0),
                stopped: false
            }
        }
    );
    let screen = pty.wait_for(r"⎿  hi", WAIT)?;
    let text = screen.text();
    assert!(text.contains("! echo hi"), "{text}");
    assert!(
        commands(&scenario)?.contains(&UiCommand::Shell {
            command: "echo hi".into()
        }),
        "the command went to the app as typed"
    );
    assert!(
        all_sessions(&scenario)?.is_empty(),
        "running a command started no agent"
    );
    scenario.shot(&pty, "app_shell_mode_runs_echo_hi")?;

    // A command that asks the terminal, as sudo does for a password, has none, so it fails at
    // once rather than stopping in the background.
    pty.wait_for(r"shift\+tab to cycle", WAIT)?;
    pty.send("!")?;
    pty.wait_for("! for shell mode", WAIT)?;
    pty.send("cat /dev/tty")?;
    pty.wait_for("! cat /dev/tty", WAIT)?;
    pty.press(Key::Enter)?;
    let ends = wait_events(&scenario, "the second command's end", 2, finished)?;
    assert!(
        matches!(
            &ends[1],
            AppEvent::Shell {
                event: ShellEvent::Finished {
                    id: 2,
                    exit_code: Some(code),
                    stopped: false
                }
            } if *code != 0
        ),
        "{ends:?}"
    );

    // Back in message mode, the main agent gets the command and its output with the next message.
    pty.wait_for(r"shift\+tab to cycle", WAIT)?;
    submit(&mut pty, requests::HI)?;
    wait_turns_finished(&scenario, 1)?;
    let sessions = all_sessions(&scenario)?;
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    let session = &sessions[0];
    // Claude Code takes the prompt as a JSON line on stdin, with its newlines escaped.
    let prompt = session
        .stdin
        .as_deref()
        .unwrap_or_default()
        .replace("\\n", "\n");
    assert!(prompt.contains("$ echo hi\nhi\n"), "{prompt}");
    let turns = turns_started(&scenario)?;
    assert_eq!(
        turns[0].text,
        requests::HI,
        "your message shows as you typed it"
    );
    Ok(())
}
