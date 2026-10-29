//! A seat's own session as its view shows it, from the run as it stands: each prompt the seat
//! read, each answer it gave, and its latest tool call, under a head that names the seat, its
//! council and how to carry the session on in its CLI.

use std::collections::HashMap;

use argonauts_core::RunId;
use argonauts_core::cli_sessions::{CliSessionRef, ResumeCommand, shell_words};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::session::InputPartKind;
use argonauts_core::names::{AgentId, SeatId, SessionKey};

use crate::bridge::tools::start_call;
use crate::cell::ToolTitle;
use crate::council::conversation::Private;
use crate::council::session_view::{PRIVATELY, SeatSession, Turn, session_keys};
use crate::council::{Conversation, Faces};
use crate::council::{Face, Piece, Tone};
use crate::format::tokens;
use crate::state::{RunState, SessionState};

/// The answer's first rows show; the rest fold under a count.
const ANSWER_ROWS: usize = 1;

/// Where a seat sits, for its session's head.
pub struct Sitting<'a> {
    pub seat: &'a SeatId,
    pub conversation: &'a Conversation,
    /// The council's seats in seat order.
    pub seats: &'a [SeatId],
    pub names: &'a DisplayNames,
    /// How each session carries on in its CLI's own screen, as its adapter said.
    pub carried: &'a HashMap<SessionKey, ResumeCommand>,
}

/// The seat's newest session in `run`, as its view draws it, with where a private message to
/// it goes. `None` while the seat has no session there.
pub fn seat_session(
    run: &RunState,
    own: impl Fn(&SessionKey) -> bool,
    at: &Sitting<'_>,
) -> Option<(SeatSession, (SeatId, RunId, SessionKey))> {
    let session = run
        .sessions_in_order()
        .filter(|session| own(&session.key))
        .last()?;
    let faces: &Faces = &at.conversation.faces;
    let face = faces.face(at.seat);
    let cli = at.names.cli(&face.cli);
    let about = match at.seats.iter().position(|seat| seat == at.seat) {
        _ if at.conversation.notes.as_ref() == Some(at.seat) => {
            " · keeps the notes · in the council ".to_owned()
        }
        Some(index) => format!(
            " · seat {} of {} · in the council ",
            index + 1,
            at.seats.len()
        ),
        None => " · in the council ".to_owned(),
    };
    let carried = at.carried.get(&session.key);
    let (resume, command) = match (&session.cli_session, carry_on(carried)) {
        (Some(id), Some(command)) => (
            format!(
                "{cli} session {} · carry it on in {cli} with ",
                short_id(id)
            ),
            command,
        ),
        _ => (String::new(), String::new()),
    };
    let target = (at.seat.clone(), run.id.clone(), session.key.clone());
    let turns = turns(session, at.conversation.private_to(at.seat));
    let share = turns
        .iter()
        .any(|turn| matches!(turn, Turn::Read { yours: Some(_), .. }));
    let keys = session_keys((!command.is_empty()).then_some(cli.as_str()), share);
    let view = SeatSession {
        badge: Piece::Badge(at.seat.clone()),
        name: face.label(),
        cli: face.cli.clone(),
        about,
        council: run.title.clone(),
        stats: stats(session),
        resume,
        command,
        only_you: Vec::new(),
        turns,
        placeholder: format!("Only {} sees what you write here", face.label()),
        keys,
        picked: None,
        carried: session.cli_session.clone().map(|id| CliSessionRef {
            agent_id: AgentId(session.agent_id.clone()),
            id,
        }),
    };
    Some((view, target))
}

/// What a check's session view names besides the session: whose fresh session checks, the
/// task it checks, and the seat that made the change, with whether the checker is its very
/// model.
pub struct Checking<'a> {
    pub checker: &'a Face,
    pub task: &'a str,
    pub maker: Option<(SeatId, bool)>,
    pub names: &'a DisplayNames,
    /// How the session carries on in its CLI's own screen, as its adapter said.
    pub carried: Option<&'a ResumeCommand>,
}

/// A check's own session, as its view draws it: the fresh session that checked a council's
/// task, every prompt it read and what it answered, and a line only you see of who made the
/// change, which the check was never told.
pub fn check_session(session: &SessionState, at: &Checking<'_>) -> SeatSession {
    let cli = at.names.cli(&at.checker.cli);
    let (resume, command) = match (&session.cli_session, carry_on(at.carried)) {
        (Some(id), Some(command)) => (
            format!(
                "{cli} session {} · never a seat's session or a fork of one · ",
                short_id(id)
            ),
            command,
        ),
        _ => (String::new(), String::new()),
    };
    let mut only_you = Vec::new();
    if let Some((seat, same)) = &at.maker {
        only_you.push(Piece::tone("Only you see this: ", Tone::Label));
        only_you.push(Piece::dim("the change came from "));
        only_you.push(Piece::Called(seat.clone(), Tone::Dim));
        let model = if *same { ", the same model" } else { "" };
        only_you.push(Piece::dim(format!("{model}; the check was never told")));
    }
    SeatSession {
        badge: Piece::Logo(at.checker.logo.clone()),
        name: "Review".to_owned(),
        cli: at.checker.cli.clone(),
        about: format!(" · a fresh {} session · task ", at.checker.name),
        council: at.task.to_owned(),
        stats: stats(session),
        resume,
        keys: session_keys((!command.is_empty()).then_some(cli.as_str()), false),
        command,
        only_you,
        turns: check_turns(session),
        placeholder: "Only this session sees what you write here · don't tell it who made \
                      the change"
            .to_owned(),
        picked: None,
        carried: session.cli_session.clone().map(|id| CliSessionRef {
            agent_id: AgentId(session.agent_id.clone()),
            id,
        }),
    }
}

/// "19 turns · 7m 58s · 305k tokens".
fn stats(session: &SessionState) -> String {
    let turns = session.inputs.len().max(1);
    let plural = if turns == 1 { "" } else { "s" };
    let span = session.ended.unwrap_or(session.last_event) - session.started;
    let seconds = span.whole_seconds().max(0);
    let time = if seconds >= 60 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    };
    format!(
        "{turns} turn{plural} · {time} · {} tokens",
        tokens(session.tokens_in + session.tokens_out)
    )
}

/// The command that opens the session in its own CLI, as its adapter built it, with the
/// session's whole id.
fn carry_on(carried: Option<&ResumeCommand>) -> Option<String> {
    carried.map(|command| shell_words(&command.words))
}

/// A session id as its head names it in short: its first eight characters.
fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Each prompt the seat read and the answer that followed it, then its latest tool call. Your
/// private messages to the seat reach it in the order you wrote them, so each private prompt
/// is the next of `private`.
fn turns<'a>(session: &SessionState, mut private: impl Iterator<Item = &'a Private>) -> Vec<Turn> {
    let mut turns = Vec::new();
    for (index, input) in session.inputs.iter().enumerate() {
        if index > 0 {
            turns.push(Turn::Gap);
        }
        let read = input
            .parts
            .iter()
            .filter(|part| part.kind != InputPartKind::Prompt && !part.text.trim().is_empty());
        for part in read {
            turns.push(read_turn(&part.text, &mut private));
        }
        if let Some(answer) = session.messages.get(index) {
            turns.extend(answer_turns(answer));
        }
    }
    if let Some(tool) = session.tools.last()
        && let ToolTitle::Call { name, args } = start_call("", &tool.name, &tool.input).title
    {
        turns.push(Turn::Tool {
            name,
            argument: args,
            result: tool.output.clone().unwrap_or_else(|| "Running…".to_owned()),
        });
    }
    turns
}

/// What a check read, from Argonauts Code and never from the council, then every call it made
/// and what it answered.
fn check_turns(session: &SessionState) -> Vec<Turn> {
    let mut turns = Vec::new();
    let mut first = true;
    for (index, input) in session.inputs.iter().enumerate() {
        if index > 0 {
            turns.push(Turn::Gap);
        }
        let read = input
            .parts
            .iter()
            .filter(|part| part.kind != InputPartKind::Prompt && !part.text.trim().is_empty());
        for part in read {
            let from = if std::mem::take(&mut first) {
                vec![Piece::dim("From Argonauts Code: ")]
            } else {
                Vec::new()
            };
            turns.push(Turn::Read {
                from,
                words: part.text.trim().to_owned(),
                time: None,
                yours: None,
            });
        }
    }
    for tool in &session.tools {
        if let ToolTitle::Call { name, args } = start_call("", &tool.name, &tool.input).title {
            turns.push(Turn::Tool {
                name,
                argument: args,
                result: tool.output.clone().unwrap_or_else(|| "Running…".to_owned()),
            });
        }
    }
    for answer in &session.messages {
        turns.extend(answer_turns(answer));
    }
    turns
}

/// A prompt the seat read: the council's turns name who they are from before a colon, and a
/// private one of yours says so, yours to share until you have.
fn read_turn<'a>(text: &str, private: &mut impl Iterator<Item = &'a Private>) -> Turn {
    let text = text.trim();
    if let Some(words) = text.strip_prefix(PRIVATELY) {
        let message = private
            .next()
            .filter(|private| !private.shared)
            .map(|private| private.message.clone());
        return Turn::private(words, None, message);
    }
    if let Some(rest) = text.strip_prefix("From the council · ")
        && let Some((from, words)) = rest.split_once(": ")
    {
        return Turn::council(&format!("{from}: "), words);
    }
    Turn::council("", text)
}

/// The answer's first rows, with the rest folded.
fn answer_turns(answer: &str) -> Vec<Turn> {
    let rows: Vec<&str> = answer
        .lines()
        .map(str::trim)
        .filter(|row| !row.is_empty())
        .collect();
    let mut turns: Vec<Turn> = rows
        .iter()
        .take(ANSWER_ROWS)
        .map(|row| Turn::Said((*row).to_owned()))
        .collect();
    let folded = rows.len().saturating_sub(ANSWER_ROWS);
    if folded > 0 {
        turns.push(Turn::Fold(u32::try_from(folded).unwrap_or(u32::MAX)));
    }
    turns
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use argonauts_core::cli_sessions::ResumeCommand;
    use argonauts_core::events::council::{CouncilEventKind, Seat, SeatRole, Seating};
    use argonauts_core::events::session::{InputPart, LaunchRecord, PromptVia, SessionInput};
    use argonauts_core::names::{MessageId, ModelRef, RoleName};
    use time::macros::datetime;

    use super::*;
    use crate::council::Face;
    use crate::state::SessionStatus;

    /// A command as an adapter builds it, typed in the session's folder.
    fn typed(words: &[&str]) -> ResumeCommand {
        ResumeCommand {
            words: words.iter().map(|word| (*word).to_owned()).collect(),
            local_model: false,
            from_any_folder: false,
            also: None,
        }
    }

    fn part(kind: InputPartKind, text: &str) -> InputPart {
        InputPart {
            kind,
            label: String::new(),
            text: text.to_owned(),
            approx_tokens: 0,
            source: None,
        }
    }

    fn input(parts: Vec<InputPart>) -> SessionInput {
        SessionInput {
            launch: LaunchRecord {
                program: "codex".to_owned(),
                args: Vec::new(),
                cwd: PathBuf::from("/Users/you/shop-api"),
                env_set: Vec::new(),
                env_removed: Vec::new(),
                prompt_via: PromptVia::Argument,
                sandbox: None,
            },
            parts,
            queued: Vec::new(),
        }
    }

    /// The head names the seat, its place and its council, and how to carry the session on in
    /// its CLI; the turns are what it read and what it answered.
    #[test]
    fn a_seats_session_reads_as_its_cli_showed_it() {
        let luna = ModelRef::new("codex", Some("gpt-5.6-luna".to_owned()));
        let at = datetime!(2026-10-25 10:41 UTC);
        let mut session = SessionState::new(
            SessionKey::numbered(7),
            luna.clone(),
            "codex",
            RoleName::Worker,
            at,
        );
        session.status = SessionStatus::Working;
        session.cli_session = Some("0a1b2c3d-0000-4000".to_owned());
        session.last_event = at + time::Duration::seconds(478);
        session.tokens_out = 305_000;
        session.inputs = vec![
            input(vec![
                part(InputPartKind::Prompt, "You sit on a council."),
                part(
                    InputPartKind::Task,
                    "From the council · The user: Add rate limiting.",
                ),
            ]),
            input(vec![part(
                InputPartKind::Message,
                "From the user, privately: Does a 401 break the mobile app?",
            )]),
        ];
        session.messages = vec![
            "A sliding window counter per key.\nMore rows here.".to_owned(),
            "No, the app sends its key on every call.".to_owned(),
        ];
        let mut run = RunState::new(RunId::new("run-rate-limit").expect("a run id"));
        run.title = "Rate limit the API".to_owned();
        run.session_order.push(session.key.clone());
        run.sessions.insert(session.key.clone(), session);

        let seat = |n: u32, model: &ModelRef| Seat {
            id: SeatId::numbered(n),
            model: model.clone(),
            role: SeatRole::Proposer,
            number: None,
        };
        let opus = ModelRef::new("claude-code", Some("claude-opus-5-5".to_owned()));
        let seating = Seating {
            seats: vec![seat(1, &opus), seat(2, &luna)],
            notes: Some(SeatId::numbered(1)),
        };
        let logo = argonauts_core::logos::logo_for("codex", Some("gpt-5.6-luna"));
        let faces = Faces::new([(SeatId::numbered(2), Face::new("5.6 Luna", "codex", logo))]);
        let mut conversation = Conversation::new(&seating, faces);
        let private = CouncilEventKind::Private {
            seat: SeatId::numbered(2),
            message: MessageId::numbered(3),
        };
        conversation.on(&private, None);
        let names = DisplayNames::default();
        let order = [SeatId::numbered(1), SeatId::numbered(2)];
        let carried = HashMap::from([(
            SessionKey::numbered(7),
            typed(&["codex", "resume", "0a1b2c3d-0000-4000"]),
        )]);
        let sitting = Sitting {
            seat: &SeatId::numbered(2),
            conversation: &conversation,
            seats: &order,
            names: &names,
            carried: &carried,
        };
        let (view, (to, _, key)) =
            seat_session(&run, |key| *key == SessionKey::numbered(7), &sitting).expect("a view");
        assert_eq!((to, key), (SeatId::numbered(2), SessionKey::numbered(7)));
        assert_eq!(view.name, "5.6 Luna");
        assert_eq!(view.about, " · seat 2 of 2 · in the council ");
        assert_eq!(view.council, "Rate limit the API");
        assert_eq!(view.stats, "2 turns · 7m 58s · 305k tokens");
        assert_eq!(view.command, "codex resume 0a1b2c3d-0000-4000");
        assert!(
            view.resume
                .ends_with("0a1b2c3d · carry it on in Codex with "),
            "{}",
            view.resume
        );
        assert_eq!(view.placeholder, "Only 5.6 Luna sees what you write here");
        assert_eq!(
            view.turns,
            [
                Turn::council("The user: ", "Add rate limiting."),
                Turn::Said("A sliding window counter per key.".to_owned()),
                Turn::Fold(1),
                Turn::Gap,
                Turn::private(
                    "Does a 401 break the mobile app?",
                    None,
                    Some(MessageId::numbered(3))
                ),
                Turn::Said("No, the app sends its key on every call.".to_owned()),
            ]
        );
        assert_eq!(
            view.keys,
            "esc to go back to the talk · o to carry it on in Codex · ↑  to pick a message, s to share it with the council"
        );

        // Once shared, it is the council's already: nothing is left to share.
        conversation.on(
            &CouncilEventKind::Shared {
                message: MessageId::numbered(3),
            },
            None,
        );
        let sitting = Sitting {
            seat: &SeatId::numbered(2),
            conversation: &conversation,
            seats: &order,
            names: &names,
            carried: &carried,
        };
        let (view, _) =
            seat_session(&run, |key| *key == SessionKey::numbered(7), &sitting).expect("a view");
        assert!(view.shareable_newest_first().is_empty());
        assert_eq!(
            view.keys,
            "esc to go back to the talk · o to carry it on in Codex"
        );
    }

    #[test]
    fn a_council_turn_names_who_it_is_from() {
        assert_eq!(
            read_turn(
                "From the council · Seat T, replying to you: A bucket bursts less.",
                &mut std::iter::empty()
            ),
            Turn::council("Seat T, replying to you: ", "A bucket bursts less.")
        );
        let mut none = std::iter::empty();
        assert!(matches!(
            read_turn("From the user, privately: Does a 401 break the app?", &mut none),
            Turn::Read { words, yours: None, .. } if words == "Does a 401 break the app?"
        ));
        assert_eq!(
            read_turn("Plain words", &mut none),
            Turn::council("", "Plain words")
        );
    }

    #[test]
    fn an_answer_shows_its_first_row_and_folds_the_rest() {
        assert_eq!(
            answer_turns("A window counter.\n\nIt costs one call.\nAnd one more."),
            [Turn::Said("A window counter.".to_owned()), Turn::Fold(2)]
        );
    }

    /// A check's own session reads as a seat's does, its head naming the fresh session and the
    /// task it checks, and a line only you see of who made the change, which the check was
    /// never told.
    #[test]
    fn a_checks_session_tells_only_you_who_made_the_change() {
        use argonauts_core::logos::logo_for;

        use crate::state::ToolRecord;

        let opus = ModelRef::new("claude-code", Some("claude-opus-5-5".to_owned()));
        let at = datetime!(2026-10-25 10:52 UTC);
        let mut session = SessionState::new(
            SessionKey::numbered(12),
            opus,
            "claude-code",
            RoleName::Reviewer,
            at,
        );
        session.status = SessionStatus::Done;
        session.ended = Some(at + time::Duration::seconds(200));
        session.cli_session = Some("3a4b5c6d-0000-4000-8000-000000000001".to_owned());
        session.tokens_in = 230_000;
        session.tokens_out = 8_000;
        let check = "You check one task of a plan that a council agreed. You get the plan, the \
                     task, the change and what the checks printed. You aren't told who made the \
                     change or how the council talked. It may come from the same model as you, \
                     or from another. Neither says anything about its quality. Read the change, \
                     run what you need, and accept it or list what must change.";
        let task = "Task 1 of 3, Redis bucket: A token bucket per API key in Redis; one Lua \
                    script checks and takes a token. The burst is the key's plan limit. Change: \
                    task-001, 6 files. Checks printed: 14 passed.";
        session.inputs = vec![
            input(vec![
                part(InputPartKind::Prompt, "Check the task."),
                part(InputPartKind::Instructions, check),
            ]),
            input(vec![part(InputPartKind::Task, task)]),
        ];
        session.messages = vec![
            "Accepted. The script checks and takes a token in one call, so two nodes can't both \
             take the last one, and the burst comes from the key's plan. One note: Retry-After \
             rounds down; round it up."
                .to_owned(),
        ];
        let call = |name: &str, input: serde_json::Value, output: &str| ToolRecord {
            id: format!("toolu_{name}"),
            name: name.to_owned(),
            input,
            parent: None,
            ok: Some(true),
            output: Some(output.to_owned()),
        };
        session.tools = vec![
            call(
                "Bash",
                serde_json::json!({ "command": "git diff --stat run/base...task-001" }),
                "6 files changed, 212 insertions(+), 4 deletions(-)",
            ),
            call(
                "Read",
                serde_json::json!({ "file_path": "src/rate/bucket.lua" }),
                "Read 41 lines",
            ),
            call(
                "Bash",
                serde_json::json!({ "command": "npm test -- rate" }),
                "14 passed",
            ),
        ];
        let face = Face::new(
            "Opus 5.5",
            "claude-code",
            logo_for("claude-code", Some("claude-opus-5-5")),
        );
        let names = DisplayNames::default();
        let view = check_session(
            &session,
            &Checking {
                checker: &face,
                task: "Redis bucket",
                maker: Some((crate::council::story::opus_one(), true)),
                names: &names,
                carried: Some(&typed(&[
                    "claude",
                    "--resume",
                    "3a4b5c6d-0000-4000-8000-000000000001",
                ])),
            },
        );
        let theme = crate::kit::logo::golden::pictured(&crate::theme::Theme::dark());
        let faces = crate::council::story::faces_one_model_twice();
        let rows: Vec<String> = view
            .lines(120, 38, &faces, &theme)
            .iter()
            .map(|line| {
                crate::kit::logo::golden::council_text(line)
                    .trim_end()
                    .to_owned()
            })
            .collect();
        crate::council::golden_tests::assert_rows_from("the_checks_own_session", &rows, 0);
    }

    /// A seat or a check on agy says how to carry its conversation on there, as Claude Code's
    /// and Codex's do. Its seats carry the adapter's id.
    #[test]
    fn an_agy_session_says_how_to_carry_it_on_in_agy() {
        let flash = ModelRef::new("antigravity-cli", Some("gemini-3.8-flash".to_owned()));
        let at = datetime!(2026-10-25 10:41 UTC);
        let mut session = SessionState::new(
            SessionKey::numbered(4),
            flash,
            "antigravity-cli",
            RoleName::Worker,
            at,
        );
        session.cli_session = Some("5e6f7a8b-0000-4000".to_owned());
        let face = Face::new(
            "Gemini 3.8 Flash",
            "antigravity-cli",
            argonauts_core::logos::logo_for("antigravity-cli", Some("gemini-3.8-flash")),
        );
        let names = DisplayNames::default();
        let view = check_session(
            &session,
            &Checking {
                checker: &face,
                task: "Redis bucket",
                maker: None,
                names: &names,
                carried: Some(&typed(&["agy", "--conversation", "5e6f7a8b-0000-4000"])),
            },
        );
        assert_eq!(view.command, "agy --conversation 5e6f7a8b-0000-4000");
        assert!(
            view.resume.starts_with("agy session 5e6f7a8b · "),
            "{}",
            view.resume
        );
        assert!(
            view.keys.contains("o to carry it on in agy"),
            "{}",
            view.keys
        );
    }

    /// Each CLI's line is the command its adapter sent with the session, whole id and all; a
    /// session whose adapter sent none says nothing about carrying it on.
    #[test]
    fn each_clis_line_is_the_command_its_adapter_sent() {
        let id = "019a2b3c-4d5e-4f60-8a7b-9c0d1e2f3a4b";
        let sent = [
            (
                "claude-code",
                "claude-opus-5-5",
                vec!["claude", "--resume", id],
            ),
            ("codex", "gpt-5.6-luna", vec!["codex", "resume", id]),
            (
                "antigravity-cli",
                "gemini-3.8-flash",
                vec!["agy", "--conversation", id],
            ),
            (
                "opencode",
                "ollama/qwen3-coder:30b",
                vec!["opencode", "-s", id],
            ),
        ];
        let names = DisplayNames::default();
        for (cli, model, words) in sent {
            let worker = ModelRef::new(cli, Some(model.to_owned()));
            let mut session = SessionState::new(
                SessionKey::numbered(4),
                worker,
                cli,
                RoleName::Reviewer,
                datetime!(2026-10-25 10:41 UTC),
            );
            session.cli_session = Some(id.to_owned());
            let face = Face::new(
                model,
                cli,
                argonauts_core::logos::logo_for(cli, Some(model)),
            );
            let mut at = Checking {
                checker: &face,
                task: "Redis bucket",
                maker: None,
                names: &names,
                carried: None,
            };
            assert_eq!(check_session(&session, &at).command, "", "{cli}");
            let command = typed(&words);
            at.carried = Some(&command);
            assert_eq!(
                check_session(&session, &at).command,
                words.join(" "),
                "{cli}"
            );
        }
    }
}
