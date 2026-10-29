//! A direct turn's work outside your folder, through the real controller loop with a fake CLI
//! and a temp home: on autopilot it reads and writes anywhere but the never-list, in supervised
//! and auto preview a write there lands once you say yes and not when you say no, the
//! never-list holds even after a yes, and each write there leaves a dim line.

use std::path::{Path, PathBuf};
use std::time::Duration;

use argonauts_conductor::sandbox::profile::{ProfileInput, can_read, can_write, session_profile};
use argonauts_core::app::{
    AnswerTarget, AppChoice, AppEvent, ChoiceKind, ConversationEvent, UiCommand,
};
use argonauts_core::asking::Ask;
use argonauts_core::event::AgentEvent;
use argonauts_core::session_runner::{
    Access, RunnerEvent, SandboxMode, SandboxProfile, SessionSpec,
};
use argonauts_core::{Asks, NoteLevel};
use argonauts_testkit::{FakeRunner, FakeSession, Hold, TempRepo};
use serde_json::json;

use super::direct_tests::{ask, choose, first_spec};
use crate::controller::test_support::{CliOptions, TestDriver};

const TASK: &str = "Look at the screenshot in Downloads and write what it shows to notes.";

fn options(config: &'static str) -> CliOptions {
    CliOptions {
        prepare: Some(Box::new(move |repo: &TempRepo| {
            repo.write("README.md", "notes\n").unwrap();
            repo.commit_all("start").unwrap();
            std::fs::write(
                repo.global_config(),
                format!("{config}\n[captain_or_crew]\nforce = \"captain\"\n"),
            )
            .unwrap();
            let home = repo.user_home();
            std::fs::create_dir_all(home.join("Downloads")).unwrap();
            std::fs::create_dir_all(home.join(".ssh")).unwrap();
            std::fs::write(home.join("Downloads/x.png"), "picture").unwrap();
            std::fs::write(home.join(".ssh/id_rsa"), "not a real key").unwrap();
        })),
        ..CliOptions::default()
    }
}

/// The modes that work outside only once asked, as `[approvals]` sets them.
const ASKING_MODES: [&str; 2] = [
    "[approvals.understand]\nmode = \"supervised\"\n",
    "[approvals.understand]\nmode = \"auto_preview\"\n",
];

/// A CLI that writes `~/Downloads/y.md` with its Write tool and answers.
fn writing_outside(home: PathBuf) -> impl Fn(&SessionSpec) -> FakeSession + Send + Sync {
    move |_: &SessionSpec| {
        let notes = home.join("Downloads/y.md");
        std::fs::write(&notes, "a screenshot of a chart\n").unwrap();
        wrote(&notes)
    }
}

/// What a CLI reports once its Write tool wrote `notes`.
fn wrote(notes: &Path) -> FakeSession {
    FakeSession::completed("Wrote the notes.")
        .with_event(RunnerEvent::Agent(AgentEvent::ToolStarted {
            id: "w1".into(),
            name: "Write".into(),
            input: json!({ "file_path": notes }),
            parent: None,
        }))
        .with_event(RunnerEvent::Agent(AgentEvent::ToolFinished {
            id: "w1".into(),
            ok: true,
            output: None,
            parent: None,
        }))
        .with_event(RunnerEvent::Agent(AgentEvent::FileChanged {
            path: notes.display().to_string(),
            kind: "add".into(),
        }))
}

fn write_to(path: &Path) -> Ask {
    Ask::Permission {
        tool_name: "Write".into(),
        input: json!({ "file_path": path, "content": "a screenshot of a chart\n" }),
        tool_use_id: Some("w1".into()),
    }
}

/// Asks about `question` as the turn's CLI does, answers it on screen with `answer`, and says
/// whether the CLI was allowed.
async fn asked(driver: &mut TestDriver, spec: &SessionSpec, question: Ask, answer: &str) -> bool {
    let answered = tokio::spawn({
        let spec = spec.clone();
        async move { ask(&spec, question).await }
    });
    let shown = driver
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::Permission))
        .await;
    let AppEvent::Choice { choice } = shown else {
        unreachable!()
    };
    let command = match answer {
        "no" => UiCommand::SetAside {
            to: AnswerTarget::Choice { id: choice.id },
        },
        _ => choose(choice.id, answer),
    };
    driver.send(command).await;
    answered.await.unwrap().text.contains("\"allow\"")
}

/// What the turn's CLI does once it may write `path`: writes it inside the turn's OS sandbox,
/// as its file tool would. True when the file was written.
fn cli_writes(spec: &SessionSpec, home: &Path, argonauts: &Path, path: &Path) -> bool {
    let profile = SandboxProfile {
        mode: SandboxMode::Os,
        ..os_profile(spec, home, argonauts)
    };
    #[cfg(target_os = "macos")]
    {
        use argonauts_conductor::sandbox::SandboxWrap;
        use argonauts_conductor::sandbox::seatbelt::Seatbelt;
        let command = argonauts_core::CommandSpec::new("/bin/sh", &spec.workdir).args([
            "-c".to_owned(),
            r#"printf 'notes\n' > "$1""#.to_owned(),
            "sh".to_owned(),
            path.display().to_string(),
        ]);
        let wrapped = Seatbelt::new().wrap(command, &profile).unwrap();
        let wrote = std::process::Command::new(&wrapped.program)
            .args(&wrapped.args)
            .current_dir(&wrapped.cwd)
            .status()
            .is_ok_and(|status| status.success());
        wrote && path.exists()
    }
    #[cfg(not(target_os = "macos"))]
    {
        can_write(&profile, path) && std::fs::write(path, "notes\n").is_ok()
    }
}

/// What each CLI prints for a write to `path` that worked, as its adapter reads it.
pub(super) fn written_by(agent: &str, path: &Path) -> Vec<AgentEvent> {
    let at = |template: &str| template.replace("PATH", &path.display().to_string());
    let lines: Vec<String> = match agent {
        "claude-code" => [
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Write","input":{"file_path":"PATH","content":"hi\n"}}]},"parent_tool_use_id":null}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_1","type":"tool_result","content":"File created successfully at: PATH"}]},"parent_tool_use_id":null,"tool_use_result":{"type":"create","filePath":"PATH","content":"hi\n"}}"#,
        ]
        .map(at)
        .to_vec(),
        "codex" => [
            r#"{"type":"item.started","item":{"id":"item_1","type":"file_change","changes":[{"path":"PATH","kind":"add"}],"status":"in_progress"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_1","type":"file_change","changes":[{"path":"PATH","kind":"add"}],"status":"completed"}}"#,
        ]
        .map(at)
        .to_vec(),
        "agy write_to_file" | "agy sed_file" => {
            let tool = agent.trim_start_matches("agy ");
            let step = |state: &str| {
                at(&format!(
                    r#"{{"event":"step_update","step_update":{{"conversation_id":"c1","step_index":5,"state":"{state}","step_type":"tool","tool_name":"{tool}","tool_info":{{"name":"{tool}","parameters":{{"TargetFile":"PATH"}}}}}}}}"#
                ))
            };
            vec![step("ACTIVE"), step("DONE")]
        }
        "opencode" => [
            r#"{"type":"tool_use","part":{"type":"tool","tool":"edit","callID":"call_1","state":{"status":"completed","input":{"path":"PATH","oldString":"a","newString":"b"},"output":"Edit applied successfully."}}}"#,
        ]
        .map(at)
        .to_vec(),
        _ => unreachable!("{agent}"),
    };
    let id = agent.split(' ').next().unwrap_or(agent);
    let adapter = crate::adapters::builtin_adapters()
        .into_iter()
        .find(|adapter| adapter.id() == id.replace("agy", "antigravity-cli"))
        .unwrap();
    let mut parser = adapter.parser();
    lines
        .iter()
        .flat_map(|line| parser.parse_stdout_line(line))
        .collect()
}

/// The OS profile the runner builds from the turn's spec, as its CLI would run in it.
fn os_profile(spec: &SessionSpec, home: &Path, argonauts: &Path) -> SandboxProfile {
    let input = ProfileInput {
        access: spec.access,
        agent_id: "claude-code",
        workdir: &spec.workdir,
        session_dir: &spec.session_dir,
        argonauts_home: argonauts,
        user_home: home,
        base_worktree: None,
        status_file: None,
        extra_writable: &[],
        sees_other_worktrees: false,
        commits: false,
    };
    session_profile(&spec.sandbox, &input)
}

fn turn_ended(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::Conversation {
            event: ConversationEvent::TurnFinished { .. } | ConversationEvent::TurnFailed { .. }
        }
    )
}

fn notices(events: &[AppEvent]) -> Vec<(NoteLevel, String)> {
    events
        .iter()
        .filter_map(|event| match event {
            AppEvent::Notice { level, text, .. } => Some((*level, text.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_direct_turn_on_autopilot_works_outside_but_never_in_the_never_list() {
    let mut driver = TestDriver::start_with(options("[approvals]\nautopilot = true\n")).await;
    let home = driver.repo.user_home().to_owned();
    driver.agent.respond(writing_outside(home.clone()));
    driver.submit(TASK).await;
    driver
        .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.starts_with("wrote")))
        .await;
    let spec = driver.agent.specs().remove(0);
    let argonauts = driver.env.paths.home().to_owned();
    let events = driver.exit().await;

    assert_eq!(spec.access, Access::Direct);
    let outside = &spec.sandbox.outside;
    assert!(outside.read && outside.write);
    assert!(outside.never.refuses_read(&home.join(".ssh/id_rsa")));
    assert!(
        outside
            .never
            .refuses_read(&home.join("Downloads/../.ssh/id_rsa"))
    );
    let profile = os_profile(&spec, &home, &argonauts);
    assert!(can_read(&profile, &home.join("Downloads/x.png")));
    assert!(can_write(&profile, &home.join("Downloads/y.md")));
    assert!(!can_read(&profile, &home.join(".ssh/id_rsa")));
    assert!(!can_write(&profile, &home.join(".ssh/authorized_keys")));

    let said = notices(&events);
    let line = "wrote ~/Downloads/y.md, outside the project".to_owned();
    assert!(said.contains(&(NoteLevel::Quiet, line.clone())), "{said:?}");
    assert_eq!(said.iter().filter(|(_, text)| *text == line).count(), 1);
}

/// No CLI reports what a command changed, so a command's write outside your folder leaves its
/// line once the command ends, as a file tool's does once the CLI reports it.
#[tokio::test]
async fn a_command_that_wrote_outside_your_folder_leaves_its_line_once_it_ends() {
    let mut driver = TestDriver::start_with(options("[approvals]\nautopilot = true\n")).await;
    let home = driver.repo.user_home().to_owned();
    let notes = home.join("Downloads/y.md");
    // The session waits after the command starts, so the test writes the file between the
    // command's start and its end, as the command would.
    let running = Hold::new();
    let mut command = FakeSession::completed("Wrote the notes.").held_after(1, &running);
    command.events = vec![
        RunnerEvent::Agent(AgentEvent::ToolStarted {
            id: "c1".into(),
            name: "Bash".into(),
            input: json!({ "command": "printf 'a chart' > ~/Downloads/y.md" }),
            parent: None,
        }),
        RunnerEvent::Agent(AgentEvent::ToolFinished {
            id: "c1".into(),
            ok: true,
            output: Some(String::new()),
            parent: None,
        }),
    ];
    driver.agent.push(command);
    driver.submit(TASK).await;
    driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Conversation {
                    event: ConversationEvent::Agent {
                        event: AgentEvent::ToolStarted { .. },
                        ..
                    }
                }
            )
        })
        .await;
    std::fs::write(&notes, "a chart").unwrap();
    running.release();
    driver.until(turn_ended).await;

    let said = notices(&driver.exit().await);
    let line = "wrote ~/Downloads/y.md, outside the project".to_owned();
    assert_eq!(
        said.iter()
            .filter(|(level, text)| *level == NoteLevel::Quiet && *text == line)
            .count(),
        1,
        "{said:?}"
    );
}

/// In supervised and auto preview Claude Code asks about a write outside your folder, as it
/// does on its own: a yes lets it land, our sandbox being open there, and it leaves its dim
/// line. A key in the never-list stays closed to the same yes, and its refusal leaves the line
/// that says so.
#[tokio::test]
async fn an_asking_turn_writes_outside_once_you_say_yes_but_never_in_the_never_list() {
    for mode in ASKING_MODES {
        let mut driver = TestDriver::start_with(options(mode)).await;
        let home = driver.repo.user_home().to_owned();
        let notes = home.join("Downloads/y.md");
        let key = home.join(".ssh/authorized_keys");
        let refused = format!("EPERM: operation not permitted, open '{}'", key.display());
        driver.agent.push(
            wrote(&notes)
                .with_event(RunnerEvent::Agent(AgentEvent::ToolStarted {
                    id: "w2".into(),
                    name: "Write".into(),
                    input: json!({ "file_path": key }),
                    parent: None,
                }))
                .with_event(RunnerEvent::Agent(AgentEvent::ToolFinished {
                    id: "w2".into(),
                    ok: false,
                    output: Some(refused),
                    parent: None,
                }))
                .with_delay(Duration::from_secs(1)),
        );
        driver.submit(TASK).await;
        let spec = first_spec(&driver).await;
        let argonauts = driver.env.paths.home().to_owned();

        let outside = &spec.sandbox.outside;
        assert!(
            outside.after_asking && !outside.read && !outside.write,
            "{mode}"
        );
        assert_eq!(
            spec.asking.as_ref().map(|asking| asking.asks),
            Some(Asks::LikeCli),
            "{mode}"
        );
        assert!(outside.never.refuses_read(&home.join(".ssh/id_rsa")));
        let profile = os_profile(&spec, &home, &argonauts);
        assert!(can_read(&profile, &home.join("Downloads/x.png")), "{mode}");
        assert!(!can_read(&profile, &home.join(".ssh/id_rsa")), "{mode}");

        assert!(
            asked(&mut driver, &spec, write_to(&notes), "yes").await,
            "{mode}"
        );
        assert!(cli_writes(&spec, &home, &argonauts, &notes), "{mode}");
        assert!(
            asked(&mut driver, &spec, write_to(&key), "yes").await,
            "{mode}"
        );
        assert!(!cli_writes(&spec, &home, &argonauts, &key), "{mode}");
        assert!(!cli_writes(
            &spec,
            &home,
            &argonauts,
            &home.join("Downloads/../.ssh/x")
        ));
        assert!(!key.exists());

        driver
            .until(|event| {
                matches!(event, AppEvent::Notice { text, .. } if text.starts_with("kept out"))
            })
            .await;
        let said = notices(&driver.exit().await);
        for line in [
            "wrote ~/Downloads/y.md, outside the project",
            "kept out: ~/.ssh/authorized_keys is on the never-list",
        ] {
            assert_eq!(
                said.iter()
                    .filter(|(level, text)| *level == NoteLevel::Quiet && text == line)
                    .count(),
                1,
                "{mode}: {said:?}"
            );
        }
    }
}

/// No refuses the write and stops the turn, as in Claude Code, so nothing lands and no line
/// is left.
#[tokio::test]
async fn an_asking_turn_writes_nothing_outside_when_you_say_no() {
    for mode in ASKING_MODES {
        let mut driver = TestDriver::start_with(options(mode)).await;
        let home = driver.repo.user_home().to_owned();
        let notes = home.join("Downloads/y.md");
        driver.agent.push(FakeSession::until_stopped(Vec::new()));
        driver.submit(TASK).await;
        let spec = first_spec(&driver).await;
        let argonauts = driver.env.paths.home().to_owned();

        assert!(
            !asked(&mut driver, &spec, write_to(&notes), "no").await,
            "{mode}"
        );
        driver
            .until(|event| {
                matches!(
                    event,
                    AppEvent::Conversation {
                        event: ConversationEvent::TurnCancelled { .. }
                    }
                )
            })
            .await;
        let said = notices(&driver.exit().await);
        assert!(!notes.exists(), "{mode}");
        assert!(
            !said
                .iter()
                .any(|(_, text)| text.contains("outside the project")),
            "{mode}: {said:?}"
        );
        // The sandbox was open there: what kept the file out was your No.
        let profile = os_profile(&spec, &home, &argonauts);
        assert!(can_write(&profile, &notes), "{mode}");
    }
}

/// Edits you allowed for the rest of the conversation keep to your folder, as Claude Code's
/// own do: a write outside it still asks.
#[tokio::test]
async fn edits_allowed_for_the_conversation_still_ask_outside_your_folder() {
    let mut driver = TestDriver::start_with(options(ASKING_MODES[0])).await;
    let home = driver.repo.user_home().to_owned();
    driver.agent.push(FakeSession::until_stopped(Vec::new()));
    driver.submit(TASK).await;
    let spec = first_spec(&driver).await;
    let readme = spec.workdir.join("README.md");
    assert!(asked(&mut driver, &spec, write_to(&readme), "always").await);
    let next = ask(&spec, write_to(&spec.workdir.join("notes.md"))).await;
    assert!(next.text.contains("\"allow\""), "{next:?}");
    let notes = home.join("Downloads/y.md");
    assert!(asked(&mut driver, &spec, write_to(&notes), "yes").await);
    driver.exit().await;
}

/// What a CLI reports when its write to `path` outside was turned away: Codex's patch fails
/// without a word under our sandbox, OpenCode's write with its own rule's, and agy 1.4.6's
/// `write_file` with its own policy's.
fn turned_away(agent: &str, path: &Path) -> FakeSession {
    let (name, input, output) = match agent {
        "codex" => (
            "file_change",
            json!({ "changes": [{ "path": path, "kind": "add" }] }),
            None,
        ),
        "opencode" => (
            "write",
            json!({ "filePath": path, "content": "notes\n" }),
            Some(
                "The user has specified a rule which prevents you from using this tool.".to_owned(),
            ),
        ),
        _ => (
            "write_file",
            json!({ "Path": path }),
            Some(format!(
                "permission check failed for write_file \"{0}\": user denied permission for \
                 write_file({0})",
                path.display()
            )),
        ),
    };
    FakeSession::completed("The sandbox turned the write away.")
        .with_event(RunnerEvent::Agent(AgentEvent::ToolStarted {
            id: "p1".into(),
            name: name.into(),
            input,
            parent: None,
        }))
        .with_event(RunnerEvent::Agent(AgentEvent::ToolFinished {
            id: "p1".into(),
            ok: false,
            output,
            parent: None,
        }))
        .with_cli_session("session-1")
}

/// Codex, OpenCode and agy can't ask before a write in a headless run, so in supervised and
/// auto preview their turn reads outside and writes only its project. A write outside it was
/// turned from waits for you: Yes resumes its session for one turn that writes there, which
/// lands with its dim line once, the never-list still closed; No leaves nothing written.
#[tokio::test]
async fn a_cli_that_cant_ask_writes_outside_only_once_you_allow_it() {
    for (agent, provider, cli) in [
        ("codex", "openai", "Codex"),
        ("opencode", "zai", "OpenCode"),
        ("antigravity-cli", "google", "agy"),
    ] {
        for (mode, answer) in ASKING_MODES.into_iter().zip(["continue", "no"]) {
            let config = format!("crew.main = \"{agent}\"\n{mode}");
            let config: &'static str = Box::leak(config.into_boxed_str());
            let mut driver = TestDriver::start_with(options(config)).await;
            let home = driver.repo.user_home().to_owned();
            let argonauts = driver.env.paths.home().to_owned();
            let notes = home.join("Downloads/y.md");
            let runner = driver
                .directory
                .add(FakeRunner::cli(agent, provider).respond_with({
                    let (home, argonauts, notes, agent) = (
                        home.clone(),
                        argonauts.clone(),
                        notes.clone(),
                        agent.to_owned(),
                    );
                    // The turn you allow writes the file inside its own sandbox, as its CLI would;
                    // the one before is turned away.
                    move |spec: &SessionSpec| {
                        if spec.sandbox.outside.write && cli_writes(spec, &home, &argonauts, &notes)
                        {
                            wrote(&notes)
                        } else {
                            turned_away(&agent, &notes)
                        }
                    }
                }));
            driver.submit(TASK).await;
            let offer = driver
                .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::RefusedCommands))
                .await;
            let AppEvent::Choice { choice } = offer else {
                unreachable!()
            };
            let case = format!("{agent} {mode} {answer}");
            assert_eq!(
                choice.title,
                format!("Let {cli} write outside the project?"),
                "{case}"
            );
            assert_eq!(choice.body, ["~/Downloads/y.md"], "{case}");
            // agy's own policy can't be set for one run, so the turn you allow skips it.
            let allows = if agent == "antigravity-cli" {
                "one more turn in which agy runs commands without asking and may write outside \
                 the project, in our sandbox"
                    .to_owned()
            } else {
                format!(
                    "one more turn in which {cli} may write outside the project, in our sandbox. \
                     The never-list still holds"
                )
            };
            assert_eq!(
                choice.options[0].detail.as_deref(),
                Some(allows.as_str()),
                "{case}"
            );
            driver.send(choose(choice.id, answer)).await;
            if answer == "no" {
                driver
                    .until(|event| matches!(event, AppEvent::ChoiceClosed { outcome, .. } if outcome == "No"))
                    .await;
            } else {
                driver
                    .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.starts_with("wrote")))
                    .await;
                driver.until(turn_ended).await;
            }
            let specs = runner.specs();
            // The temp home goes with the driver, so the writes are tried first.
            let asking = &specs[0].sandbox.outside;
            assert!(
                asking.read && !asking.write && !asking.after_asking,
                "{case}"
            );
            if answer == "no" {
                assert!(!notes.exists(), "{case}");
            }
            let probe = home.join("Downloads/probe.md");
            assert!(!cli_writes(&specs[0], &home, &argonauts, &probe), "{case}");
            let allowed = specs.get(1);
            if let Some(allowed) = allowed {
                assert!(allowed.sandbox.outside.write, "{case}");
                let asks = allowed.asking.as_ref().map(|asking| asking.asks);
                if agent == "antigravity-cli" {
                    assert!(asks.is_none_or(|asks| asks == Asks::Never), "{case}");
                } else {
                    assert!(
                        asks.is_none_or(|asks| asks == Asks::LikeCli),
                        "{case}: {asks:?}"
                    );
                }
                assert_eq!(allowed.resume.as_deref(), Some("session-1"), "{case}");
                assert_eq!(
                    allowed.prompt[0].text,
                    "You may now write the files outside the project you were refused.",
                    "{case}"
                );
                assert!(cli_writes(allowed, &home, &argonauts, &notes), "{case}");
                let profile = os_profile(allowed, &home, &argonauts);
                assert!(
                    !can_write(&profile, &home.join(".ssh/authorized_keys")),
                    "{case}"
                );
            }
            let events = driver.exit().await;
            let lines = notices(&events)
                .into_iter()
                .filter(|(_, text)| text == "wrote ~/Downloads/y.md, outside the project")
                .count();
            if answer == "no" {
                assert!(allowed.is_none(), "{case}");
                assert_eq!(lines, 0, "{case}");
            } else {
                assert!(allowed.is_some(), "{case}");
                assert_eq!(lines, 1, "{case}");
            }
        }
    }
}

/// agy's commands its own policy refused and a write outside our sandbox turned away are one
/// question: the turn you allow runs commands without asking and may write outside.
#[tokio::test]
async fn agy_s_refused_commands_and_writes_outside_are_one_question() {
    let config = "crew.main = \"antigravity-cli\"\n[approvals.understand]\nmode = \"supervised\"\n";
    let mut driver = TestDriver::start_with(options(config)).await;
    let home = driver.repo.user_home().to_owned();
    let notes = home.join("Downloads/y.md");
    let refused = turned_away("antigravity-cli", &notes)
        .with_event(RunnerEvent::Agent(AgentEvent::ToolStarted {
            id: "s1".into(),
            name: "run_command".into(),
            input: json!({ "CommandLine": "npm test" }),
            parent: None,
        }))
        .with_event(RunnerEvent::Agent(AgentEvent::ToolFinished {
            id: "s1".into(),
            ok: false,
            output: Some("permission check failed for unsandboxed \"npm\"".into()),
            parent: None,
        }));
    let argonauts_home = driver.env.paths.home().to_owned();
    let agy = driver
        .directory
        .add(FakeRunner::cli("antigravity-cli", "google").respond_with({
            let (home, notes) = (home.clone(), notes.clone());
            move |spec: &SessionSpec| {
                if spec.sandbox.outside.write && cli_writes(spec, &home, &argonauts_home, &notes) {
                    wrote(&notes)
                } else {
                    refused.clone()
                }
            }
        }));
    driver.submit(TASK).await;
    let offer = driver
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::RefusedCommands))
        .await;
    let AppEvent::Choice { choice } = offer else {
        unreachable!()
    };
    assert_eq!(choice.title, "Let agy run what it was refused?");
    assert_eq!(choice.body, ["npm test", "write ~/Downloads/y.md"]);
    assert_eq!(
        choice.options[0].detail.as_deref(),
        Some(
            "one more turn in which agy runs commands without asking and may write outside the \
             project, in our sandbox"
        )
    );
    driver.send(choose(choice.id, "continue")).await;
    driver
        .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.starts_with("wrote")))
        .await;
    let argonauts = driver.env.paths.home().to_owned();
    let specs = agy.specs();
    driver.exit().await;

    let asking = &specs[0];
    assert_eq!(asking.asking.as_ref().map(|a| a.asks), Some(Asks::LikeCli));
    assert!(asking.sandbox.outside.read && !asking.sandbox.outside.write);
    let allowed = &specs[1];
    assert!(
        allowed
            .asking
            .as_ref()
            .is_none_or(|asking| asking.asks == Asks::Never)
    );
    assert!(allowed.sandbox.outside.write);
    assert_eq!(
        allowed.prompt[0].text,
        "You may run the commands you were refused. You may now write the files outside the \
         project you were refused."
    );
    let profile = os_profile(allowed, &home, &argonauts);
    assert!(can_write(&profile, &notes));
    assert!(!can_write(&profile, &home.join(".ssh/authorized_keys")));
}

/// What agy 1.4.6 reports for a write to `path` its own policy refused in headless mode: the
/// call done and the file changed, by `kind`.
fn called_done(path: &Path, kind: &str) -> FakeSession {
    FakeSession::completed("Done.")
        .with_event(RunnerEvent::Agent(AgentEvent::ToolStarted {
            id: "step-2".into(),
            name: "write_file".into(),
            input: json!({ "Path": path }),
            parent: None,
        }))
        .with_event(RunnerEvent::Agent(AgentEvent::ToolFinished {
            id: "step-2".into(),
            ok: true,
            output: None,
            parent: None,
        }))
        .with_event(RunnerEvent::Agent(AgentEvent::FileChanged {
            path: path.display().to_string(),
            kind: kind.into(),
        }))
}

/// What a supervised agy turn that reports `called_done` for `~/Downloads/y.md` leaves: the
/// offer that follows it, the turn's notices, and the file's text once the offer came.
struct CalledDone {
    offer: AppChoice,
    said: Vec<(NoteLevel, String)>,
    text: Option<String>,
}

async fn after_agy_called_done(mut driver: TestDriver, kind: &str) -> CalledDone {
    let notes = driver.repo.user_home().join("Downloads/y.md");
    driver
        .directory
        .add(FakeRunner::cli("antigravity-cli", "google").then(called_done(&notes, kind)));
    driver.submit(TASK).await;
    let offer = driver
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::RefusedCommands))
        .await;
    let AppEvent::Choice { choice: offer } = offer else {
        unreachable!()
    };
    let text = std::fs::read_to_string(&notes).ok();
    let said = notices(&driver.exit().await);
    CalledDone { offer, said, text }
}

const AGY_SUPERVISED: &str =
    "crew.main = \"antigravity-cli\"\n[approvals.understand]\nmode = \"supervised\"\n";

/// agy 1.4.6 reports a write its own policy refused in headless mode as done, file changed and
/// all. The disk says otherwise: no dim line claims it was written, and it waits for you as
/// any write outside turned away does.
#[tokio::test]
async fn a_write_agy_called_done_that_isnt_on_the_disk_waits_for_you() {
    let driver = TestDriver::start_with(options(AGY_SUPERVISED)).await;
    let done = after_agy_called_done(driver, "add").await;
    assert_eq!(done.offer.title, "Let agy write outside the project?");
    assert_eq!(done.offer.body, ["~/Downloads/y.md"]);
    assert_eq!(done.text, None);
    let said = done.said;
    assert!(
        !said.iter().any(|(_, text)| text.starts_with("wrote ")),
        "{said:?}"
    );
}

/// The same for an edit to a file you already had: it is there, but its size and time are as
/// they were before the turn, so no dim line claims it was written, and it waits for you.
#[tokio::test]
async fn an_edit_agy_called_done_to_a_file_already_there_that_didnt_change_waits_for_you() {
    let driver = TestDriver::start_with(options(AGY_SUPERVISED)).await;
    let notes = driver.repo.user_home().join("Downloads/y.md");
    std::fs::write(&notes, "last week's notes\n").unwrap();
    let hour_ago = std::time::SystemTime::now() - Duration::from_secs(3600);
    let file = std::fs::File::options().write(true).open(&notes).unwrap();
    file.set_modified(hour_ago).unwrap();
    let done = after_agy_called_done(driver, "update").await;
    assert_eq!(done.offer.title, "Let agy write outside the project?");
    assert_eq!(done.offer.body, ["~/Downloads/y.md"]);
    assert_eq!(done.text.as_deref(), Some("last week's notes\n"));
    let said = done.said;
    assert!(
        !said.iter().any(|(_, text)| text.starts_with("wrote ")),
        "{said:?}"
    );
}

/// A go-ahead turn, such as "Go ahead with this plan?", is held as any turn is: in supervised
/// with `captain.asks = "never"` it asks nothing, so it keeps to your folder. Only the turn you
/// allow by answering what a CLI was refused writes outside.
#[tokio::test]
async fn a_go_ahead_turn_that_asks_nothing_keeps_to_its_folder() {
    use argonauts_core::input::SlashCommand;

    let config = "[approvals.understand]\nmode = \"supervised\"\n[captain]\nasks = \"never\"\n";
    let mut driver = TestDriver::start_with(options(config)).await;
    let home = driver.repo.user_home().to_owned();
    driver.agent.respond(|spec: &SessionSpec| {
        if spec.access == Access::Direct {
            FakeSession::completed("Done.")
        } else {
            FakeSession::completed("1. Write the notes.").with_cli_session("plan-1")
        }
    });
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Plan {
                task: Some(TASK.into()),
            },
        })
        .await;
    let question = driver
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::GoAhead))
        .await;
    let AppEvent::Choice { choice } = question else {
        unreachable!()
    };
    driver.send(choose(choice.id, "yes")).await;
    driver.until(turn_ended).await;
    let argonauts = driver.env.paths.home().to_owned();
    let specs = driver.agent.specs();
    driver.exit().await;

    let went_ahead = &specs[1];
    assert_eq!(went_ahead.access, Access::Direct);
    assert_eq!(went_ahead.prompt[0].text, "Go ahead with this plan.");
    assert!(went_ahead.sandbox.outside.is_closed());
    let profile = os_profile(went_ahead, &home, &argonauts);
    assert!(!can_write(&profile, &home.join("Downloads/y.md")));
}

/// A supervised turn that asks nothing, as `captain.asks = "never"` says, has nothing to hold
/// it outside, so it keeps to your folder.
#[tokio::test]
async fn a_supervised_turn_that_asks_nothing_keeps_to_its_folder() {
    let config = "[approvals.understand]\nmode = \"supervised\"\n[captain]\nasks = \"never\"\n";
    let mut driver = TestDriver::start_with(options(config)).await;
    let home = driver.repo.user_home().to_owned();
    driver.agent.push(FakeSession::completed("Looked."));
    driver.submit(TASK).await;
    driver.until(turn_ended).await;
    let spec = driver.agent.specs().remove(0);
    let argonauts = driver.env.paths.home().to_owned();
    driver.exit().await;
    assert!(
        spec.asking
            .as_ref()
            .is_none_or(|asking| asking.asks == Asks::Never)
    );
    assert!(spec.sandbox.outside.is_closed());
    let profile = os_profile(&spec, &home, &argonauts);
    assert!(!can_write(&profile, &home.join("Downloads/y.md")));
}

/// With `ask` emptied, supervised keeps to your folder as it did before. The never-list holds
/// only outside it, so a read of `.env` that failed there was refused by something else, and
/// leaves no line blaming the list.
#[tokio::test]
async fn an_empty_ask_keeps_a_supervised_turn_to_its_folder() {
    let config = "[approvals.understand]\nmode = \"supervised\"\n[outside]\nask = []\n";
    let mut driver = TestDriver::start_with(options(config)).await;
    let home = driver.repo.user_home().to_owned();
    let env = driver.repo.root().join(".env");
    driver.agent.push(
        FakeSession::completed("Looked.")
            .with_event(RunnerEvent::Agent(AgentEvent::ToolStarted {
                id: "r1".into(),
                name: "Read".into(),
                input: json!({ "file_path": env }),
                parent: None,
            }))
            .with_event(RunnerEvent::Agent(AgentEvent::ToolFinished {
                id: "r1".into(),
                ok: false,
                output: Some("File does not exist.".into()),
                parent: None,
            })),
    );
    driver.submit(TASK).await;
    driver.until(turn_ended).await;
    let spec = driver.agent.specs().remove(0);
    let argonauts = driver.env.paths.home().to_owned();
    let said = notices(&driver.exit().await);
    assert!(spec.sandbox.outside.is_closed());
    assert_eq!(
        spec.asking.as_ref().map(|asking| asking.asks),
        Some(Asks::LikeCli)
    );
    let profile = os_profile(&spec, &home, &argonauts);
    assert!(!can_write(&profile, &home.join("Downloads/y.md")));
    assert!(
        !said.iter().any(|(_, text)| text.starts_with("kept out")),
        "{said:?}"
    );
}

#[tokio::test]
async fn each_cli_s_write_outside_your_folder_leaves_its_line() {
    let writers = [
        "claude-code",
        "codex",
        "agy write_to_file",
        "agy sed_file",
        "opencode",
    ];
    let mut driver = TestDriver::start_with(options("[approvals]\nautopilot = true\n")).await;
    let home = driver.repo.user_home().to_owned();
    let events: Vec<AgentEvent> = writers
        .iter()
        .flat_map(|writer| {
            let name = writer.replace(' ', "-");
            written_by(writer, &home.join(format!("Downloads/{name}.md")))
        })
        .collect();
    for writer in writers {
        let name = writer.replace(' ', "-");
        let path = home
            .join(format!("Downloads/{name}.md"))
            .display()
            .to_string();
        let reported = events.iter().any(|event| {
            matches!(event, AgentEvent::FileChanged { path: changed, .. } if *changed == path)
        });
        assert!(reported, "{writer}: {events:?}");
    }
    let written: Vec<PathBuf> = writers
        .iter()
        .map(|writer| home.join(format!("Downloads/{}.md", writer.replace(' ', "-"))))
        .collect();
    driver.agent.respond(move |_: &SessionSpec| {
        // Each CLI's write is on the disk before it says so, as the line reads the disk.
        for path in &written {
            std::fs::write(path, "notes\n").unwrap();
        }
        events
            .iter()
            .cloned()
            .fold(FakeSession::completed("Wrote them."), |session, event| {
                session.with_event(RunnerEvent::Agent(event))
            })
    });
    driver.submit(TASK).await;
    driver.until(turn_ended).await;
    let said = notices(&driver.exit().await);

    for writer in writers {
        let name = writer.replace(' ', "-");
        let line = format!("wrote ~/Downloads/{name}.md, outside the project");
        let times = said.iter().filter(|(_, text)| *text == line).count();
        assert_eq!(times, 1, "{writer}: {said:?}");
    }
}

/// A shell startup file you have and a program a cargo install puts in `~/.cargo/bin`.
fn startup_file_and_program(home: &Path) -> [PathBuf; 2] {
    std::fs::create_dir_all(home.join(".cargo/bin")).unwrap();
    std::fs::write(home.join(".zshrc"), "# shell setup\n").unwrap();
    [home.join(".zshrc"), home.join(".cargo/bin/tool")]
}

/// What Codex reports when its patch to `paths` outside was turned away: it fails without a
/// word under our sandbox.
fn patch_turned_away(paths: &[PathBuf]) -> FakeSession {
    let changes: Vec<serde_json::Value> = paths
        .iter()
        .map(|path| json!({ "path": path, "kind": "update" }))
        .collect();
    FakeSession::completed("The sandbox turned the patch away.")
        .with_event(RunnerEvent::Agent(AgentEvent::ToolStarted {
            id: "p1".into(),
            name: "file_change".into(),
            input: json!({ "changes": changes }),
            parent: None,
        }))
        .with_event(RunnerEvent::Agent(AgentEvent::ToolFinished {
            id: "p1".into(),
            ok: false,
            output: None,
            parent: None,
        }))
        .with_cli_session("session-1")
}

/// A startup file and a program folder outside are asked about as any write there. In
/// supervised Claude Code asks and a yes lets the write land; Codex's is turned away and waits
/// for you, and the turn you allow writes it. With `ask` emptied it is refused and nothing asks.
#[tokio::test]
async fn startup_files_and_program_folders_are_asked_about_as_any_write_outside() {
    let mut driver = TestDriver::start_with(options(ASKING_MODES[0])).await;
    let home = driver.repo.user_home().to_owned();
    let argonauts = driver.env.paths.home().to_owned();
    let paths = startup_file_and_program(&home);
    driver.agent.push(FakeSession::until_stopped(Vec::new()));
    driver.submit(TASK).await;
    let spec = first_spec(&driver).await;
    for path in &paths {
        let shown = path.display();
        assert!(
            asked(&mut driver, &spec, write_to(path), "yes").await,
            "{shown}"
        );
        assert!(cli_writes(&spec, &home, &argonauts, path), "{shown}");
    }
    driver.exit().await;

    let config = "crew.main = \"codex\"\n[approvals.understand]\nmode = \"supervised\"\n";
    let mut driver = TestDriver::start_with(options(config)).await;
    let home = driver.repo.user_home().to_owned();
    let argonauts = driver.env.paths.home().to_owned();
    let paths = startup_file_and_program(&home);
    let runner = driver
        .directory
        .add(FakeRunner::cli("codex", "openai").respond_with({
            let (home, argonauts, paths) = (home.clone(), argonauts.clone(), paths.clone());
            move |spec: &SessionSpec| {
                let lands = spec.sandbox.outside.write
                    && paths
                        .iter()
                        .all(|path| cli_writes(spec, &home, &argonauts, path));
                if lands {
                    wrote(&paths[0])
                } else {
                    patch_turned_away(&paths)
                }
            }
        }));
    driver.submit(TASK).await;
    let offer = driver
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::RefusedCommands))
        .await;
    let AppEvent::Choice { choice } = offer else {
        unreachable!()
    };
    assert_eq!(choice.title, "Let Codex write outside the project?");
    assert_eq!(choice.body, ["~/.zshrc", "~/.cargo/bin/tool"]);
    driver.send(choose(choice.id, "continue")).await;
    driver
        .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.starts_with("wrote")))
        .await;
    driver.until(turn_ended).await;
    let specs = runner.specs();
    assert!(!specs[0].sandbox.outside.write);
    assert!(specs[1].sandbox.outside.write);
    assert_eq!(std::fs::read_to_string(&paths[0]).unwrap(), "notes\n");
    assert!(paths[1].is_file());
    driver.exit().await;

    let config = "crew.main = \"codex\"\n[approvals.understand]\nmode = \"supervised\"\n\
                  [outside]\nask = []\n";
    let mut driver = TestDriver::start_with(options(config)).await;
    let home = driver.repo.user_home().to_owned();
    let argonauts = driver.env.paths.home().to_owned();
    let paths = startup_file_and_program(&home);
    let runner = driver
        .directory
        .add(FakeRunner::cli("codex", "openai").respond_with({
            let paths = paths.clone();
            move |_: &SessionSpec| patch_turned_away(&paths)
        }));
    driver.submit(TASK).await;
    driver.until(turn_ended).await;
    let specs = runner.specs();
    assert_eq!(specs.len(), 1);
    assert!(specs[0].sandbox.outside.is_closed());
    for path in &paths {
        let shown = path.display();
        assert!(!cli_writes(&specs[0], &home, &argonauts, path), "{shown}");
    }
    assert_eq!(
        std::fs::read_to_string(&paths[0]).unwrap(),
        "# shell setup\n"
    );
    let events = driver.exit().await;
    let asked = events.iter().any(|event| {
        matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::RefusedCommands)
    });
    assert!(!asked, "{events:?}");
}
