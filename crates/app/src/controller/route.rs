//! What plain text and mentions mean in each app mode. Slash commands run at once in any mode.

use argonauts_core::RunId;
use argonauts_core::app::{AppMode, UiCommand};
use argonauts_core::input::{MentionTarget, ParsedInput, SlashCommand, parse_input_with};
use argonauts_core::vendor_commands::VendorCommandSet;

/// Said when a mention arrives with no run to deliver it to.
pub(crate) const NO_RUN_FOR_MENTION: &str = "No run is going, so the mention wasn't sent.";

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Route {
    /// `Idle`: record it and run the triage estimate.
    NewRequest {
        text: String,
    },
    /// `Answering`: queued as the next chat turn.
    NextTurn {
        text: String,
    },
    /// `Working`, `Pausing`, or `Parked`: queued for every working agent's next turn.
    Guidance {
        run: RunId,
        text: String,
    },
    /// `Pausing` and an empty Enter.
    CancelPause,
    /// `Parked` and an empty Enter.
    Resume {
        run: RunId,
    },
    Mention {
        run: RunId,
        target: MentionTarget,
        text: String,
    },
    /// Such as a mention with no run going: nothing is sent.
    Notice {
        text: String,
    },
    Slash {
        command: SlashCommand,
    },
    Nothing,
}

/// For `Submit` and `Mention`. Anything else routes to `Nothing`. `commands` are the main
/// agent's own, which `/` also takes. Text that `leads_with_file`, such as `@README.md what
/// is this?`, is about that file, so it isn't a mention.
pub(crate) fn route(
    mode: &AppMode,
    command: UiCommand,
    commands: Option<&VendorCommandSet>,
    leads_with_file: impl Fn(&str) -> bool,
) -> Route {
    match command {
        UiCommand::Submit { text } if leads_with_file(&text) => plain(mode, text.trim().to_owned()),
        UiCommand::Submit { text } => submitted(mode, &text, commands),
        UiCommand::Mention { target, text } => mention(mode, target, text),
        _ => Route::Nothing,
    }
}

/// The screen parses what you type, but a front end may send it raw, so `/` and `@` still
/// count here.
fn submitted(mode: &AppMode, text: &str, commands: Option<&VendorCommandSet>) -> Route {
    match parse_input_with(text, commands) {
        Ok(ParsedInput::Slash { command }) => Route::Slash { command },
        Ok(ParsedInput::Mention { target, text }) => mention(mode, target, text),
        Ok(ParsedInput::Plain { text }) => plain(mode, text),
        Ok(ParsedInput::Empty) => empty(mode),
        Err(err) => Route::Notice {
            text: err.to_string(),
        },
    }
}

fn plain(mode: &AppMode, text: String) -> Route {
    match mode {
        AppMode::Idle => Route::NewRequest { text },
        AppMode::Answering { .. } => Route::NextTurn { text },
        AppMode::Working { run } | AppMode::Pausing { run } | AppMode::Parked { run, .. } => {
            Route::Guidance {
                run: run.clone(),
                text,
            }
        }
    }
}

fn empty(mode: &AppMode) -> Route {
    match mode {
        AppMode::Pausing { .. } => Route::CancelPause,
        AppMode::Parked { run, .. } => Route::Resume { run: run.clone() },
        AppMode::Idle | AppMode::Answering { .. } | AppMode::Working { .. } => Route::Nothing,
    }
}

fn mention(mode: &AppMode, target: MentionTarget, text: String) -> Route {
    match mode {
        AppMode::Working { run } | AppMode::Pausing { run } | AppMode::Parked { run, .. } => {
            Route::Mention {
                run: run.clone(),
                target,
                text,
            }
        }
        AppMode::Idle | AppMode::Answering { .. } => Route::Notice {
            text: NO_RUN_FOR_MENTION.to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::events::run::ParkReason;
    use argonauts_core::input::parse_input;
    use argonauts_core::names::TurnId;

    fn run() -> RunId {
        RunId::new("run-001").unwrap()
    }

    fn route(mode: &AppMode, command: UiCommand, commands: Option<&VendorCommandSet>) -> Route {
        super::route(mode, command, commands, |_| false)
    }

    fn submit(text: &str) -> UiCommand {
        UiCommand::Submit {
            text: text.to_owned(),
        }
    }

    fn at_codex(text: &str) -> UiCommand {
        UiCommand::Mention {
            target: MentionTarget::Worker("codex".parse().unwrap()),
            text: text.to_owned(),
        }
    }

    fn modes() -> Vec<AppMode> {
        vec![
            AppMode::Idle,
            AppMode::Answering {
                turn: TurnId::numbered(1),
            },
            AppMode::Working { run: run() },
            AppMode::Pausing { run: run() },
            AppMode::Parked {
                run: run(),
                reason: ParkReason::Checkpoint,
            },
        ]
    }

    #[test]
    fn route_table() {
        let text = || "add rate limiting".to_owned();
        let guidance = || Route::Guidance {
            run: run(),
            text: text(),
        };
        let mentioned = || Route::Mention {
            run: run(),
            target: MentionTarget::Worker("codex".parse().unwrap()),
            text: "look at the tests".to_owned(),
        };
        let no_run = || Route::Notice {
            text: NO_RUN_FOR_MENTION.to_owned(),
        };
        // Per mode: plain text, empty Enter, a mention, and a mention typed as text.
        let expected = [
            (Route::NewRequest { text: text() }, Route::Nothing, no_run()),
            (Route::NextTurn { text: text() }, Route::Nothing, no_run()),
            (guidance(), Route::Nothing, mentioned()),
            (guidance(), Route::CancelPause, mentioned()),
            (guidance(), Route::Resume { run: run() }, mentioned()),
        ];
        for (mode, (plain, empty, mention)) in modes().iter().zip(expected) {
            assert_eq!(
                route(mode, submit("  add rate limiting "), None),
                plain,
                "{mode:?}"
            );
            assert_eq!(route(mode, submit("   "), None), empty, "{mode:?}");
            assert_eq!(
                route(mode, at_codex("look at the tests"), None),
                mention,
                "{mode:?}"
            );
            assert_eq!(
                route(mode, submit("@codex look at the tests"), None),
                mention,
                "{mode:?}"
            );
            assert_eq!(
                route(mode, submit("/status"), None),
                Route::Slash {
                    command: SlashCommand::Status
                },
                "{mode:?}"
            );
            assert!(
                matches!(
                    route(mode, submit("/no-such-command"), None),
                    Route::Notice { .. }
                ),
                "{mode:?}"
            );
            assert_eq!(
                route(mode, UiCommand::CancelTurn, None),
                Route::Nothing,
                "{mode:?}"
            );
        }
    }

    #[test]
    fn an_address_that_is_no_target_stays_plain_text() {
        assert_eq!(
            route(&AppMode::Idle, submit("@ nothing"), None),
            Route::Notice {
                text: parse_input("@ nothing").unwrap_err().to_string()
            }
        );
        assert_eq!(
            route(&AppMode::Idle, submit("mail me@example.com"), None),
            Route::NewRequest {
                text: "mail me@example.com".to_owned()
            }
        );
    }

    #[test]
    fn text_led_by_a_file_is_a_message_about_it_not_a_mention() {
        let names_readme = |text: &str| text.trim_start().starts_with("@README.md");
        let text = " @README.md what is this? ";
        assert_eq!(
            super::route(&AppMode::Idle, submit(text), None, names_readme),
            Route::NewRequest {
                text: "@README.md what is this?".to_owned()
            }
        );
        let working = AppMode::Working { run: run() };
        assert_eq!(
            super::route(&working, submit(text), None, names_readme),
            Route::Guidance {
                run: run(),
                text: "@README.md what is this?".to_owned()
            }
        );
        assert!(matches!(
            super::route(&working, submit("@codex look"), None, names_readme),
            Route::Mention { .. }
        ));
    }

    #[test]
    fn a_front_end_may_send_the_main_agents_own_commands_raw() {
        use argonauts_core::names::VendorId;
        let set = VendorCommandSet::new(
            VendorId("claude-code".into()),
            "Claude Code",
            argonauts_adapter_claude_code::BUILTIN_COMMANDS,
            Vec::new(),
        );
        let idle = &AppMode::Idle;
        assert_eq!(
            route(idle, submit("/simplify  the parser"), Some(&set)),
            Route::Slash {
                command: SlashCommand::Vendor {
                    name: "simplify".into(),
                    args: "the parser".into()
                }
            }
        );
        assert_eq!(
            route(idle, submit("/reset"), Some(&set)),
            Route::Slash {
                command: SlashCommand::Clear
            }
        );
        assert_eq!(
            route(idle, submit("/voice"), Some(&set)),
            Route::Notice {
                text: "/voice works only in Claude Code itself".into()
            }
        );
    }
}
