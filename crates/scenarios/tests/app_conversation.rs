//! The conversation with the main agent: answers, requests and how each is sent to Captain or crew,
//! `/ask`, `/captain` and `/crew`, resuming the CLI session, the settings hash, Esc, and
//! uncommitted files.

mod app_support;

use std::thread;
use std::time::{Duration, Instant};

use app_support::*;
use argonauts_core::app::{AppEvent, AppMode, ChoiceKind, ConversationEvent, TurnReply, UiCommand};
use argonauts_core::events::run::{RunLifecycle, RunMode};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::input::SlashCommand;
use argonauts_scenarios::{Scenario, WAIT};
use argonauts_testkit::{Key, Pty};
use regex::Regex;

/// A Captain change whose worker sits in a tool call until stopped, so a pause stays pending.
const TASK_IN_A_TOOL_CALL: &str = "rename the config loader while the build runs";

/// The turn end, whichever done word the app picked, such as `✻ Sailed for 3s`.
const TURN_END: &str = r"✻ \w+ for \d";

/// Waits for the `n`th finished turn and returns its reply.
fn nth_reply(scenario: &Scenario, n: usize) -> anyhow::Result<TurnReply> {
    let finished = wait_events(scenario, "a finished turn", n, is_turn_finished)?;
    match &finished[n - 1] {
        AppEvent::Conversation {
            event: ConversationEvent::TurnFinished { reply, .. },
        } => Ok(reply.clone()),
        other => anyhow::bail!("not a finished turn: {other:?}"),
    }
}

fn is_start_work(event: &AppEvent) -> bool {
    matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::StartWork)
}

/// Where each text first shows on the screen, so a test can check their order.
fn positions(text: &str, needles: &[&str]) -> Vec<usize> {
    needles
        .iter()
        .map(|needle| {
            text.find(needle)
                .unwrap_or_else(|| panic!("{needle:?} isn't on the screen:\n{text}"))
        })
        .collect()
}

/// The turn end as `text` shows it, or the pattern itself, which no screen shows, when there's
/// none.
fn turn_end(text: &str) -> &str {
    Regex::new(TURN_END)
        .expect("a valid pattern")
        .find(text)
        .map_or(TURN_END, |found| found.as_str())
}

fn assert_in_order(text: &str, needles: &[&str]) {
    let found = positions(text, needles);
    assert!(
        found.windows(2).all(|pair| pair[0] < pair[1]),
        "expected {needles:?} in this order:\n{text}"
    );
}

#[test]
fn app_example_hi() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    pty.wait_for(r"Argonauts Code v?\d+\.\d+\.\d+", WAIT)?;

    submit(&mut pty, requests::HI)?;
    wait_turns_finished(&scenario, 1)?;
    let screen = pty.wait_for(TURN_END, WAIT)?;
    let text = screen.text();
    assert_in_order(
        &text,
        &["Hi. Ask me anything about this repo", turn_end(&text)],
    );
    check_bottom_row(&pty)?;

    assert!(run_dirs(&scenario).is_empty(), "hi started a run");
    let sessions = all_sessions(&scenario)?;
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    assert!(
        has_flag(&sessions[0].args, "--permission-mode", "dontAsk"),
        "the conversation session isn't read-only: {:?}",
        sessions[0].args
    );
    let turns = turns_started(&scenario)?;
    assert_eq!(turns.len(), 1);
    assert!(turns[0].read_only);
    scenario.shot(&pty, "app_example_hi")?;
    Ok(())
}

#[test]
fn app_example_question_could_be_task() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, requests::QUESTION_COULD_BE_TASK)?;

    let question = wait_open_choice(&scenario, ChoiceKind::StartWork)?;
    let keys: Vec<char> = question.options.iter().map(|option| option.key).collect();
    assert_eq!(keys, ['s', 'c', 'n'], "{question:?}");
    let question_text = format!("{} {}", question.title, question.body.join(" "));
    assert!(
        question_text.contains("Argonauts Code would pick crew"),
        "{question_text}"
    );
    let screen = pty.wait_for(r"Start work on it\? Argonauts Code would pick crew\.", WAIT)?;
    let text = screen.text();
    assert_in_order(
        &text,
        &[
            "Read",
            turn_end(&text),
            "Start work on it? Argonauts Code would pick crew.",
        ],
    );

    answer_question(&mut pty, number_of(&question.options, 'n'))?;
    wait_choice_closed(&scenario, &question.id)?;
    pty.wait_for("no, just asking", WAIT)?;
    check_bottom_row(&pty)?;
    assert!(
        run_dirs(&scenario).is_empty(),
        "no, just asking started a run"
    );
    scenario.shot(&pty, "app_example_question_could_be_task")?;
    Ok(())
}

#[test]
fn app_plain_text_is_a_request() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, requests::SMALL_FIX)?;

    wait_until("the text to go up as a request", || {
        Ok(commands(&scenario)?
            .into_iter()
            .find(|c| matches!(c, UiCommand::Submit { text, .. } if text == requests::SMALL_FIX)))
    })?;
    assert!(matches!(nth_reply(&scenario, 1)?, TurnReply::Change { .. }));
    let turns = turns_started(&scenario)?;
    assert_eq!(turns[0].text, requests::SMALL_FIX);

    let started = run_started(&wait_run_dir(&scenario)?)?;
    assert_eq!(started.request, requests::SMALL_FIX);
    assert!(
        started.conversation_turn.is_some(),
        "the run doesn't name its conversation turn"
    );
    assert!(
        !started.forced,
        "plain text went around the Captain-or-crew rules"
    );
    scenario.shot(&pty, "app_plain_text_is_a_request")?;
    Ok(())
}

/// `/tmp` is no command, yet it is a folder on this computer, so a prompt that starts with it
/// goes to the main agent as text.
#[test]
fn app_a_prompt_led_by_a_path_is_a_request() -> anyhow::Result<()> {
    const PROMPT: &str = "/tmp is full, clean it";
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    let before = turns_started(&scenario)?.len();
    submit(&mut pty, PROMPT)?;

    let turns = wait_until("the prompt to start a turn", || {
        let turns = turns_started(&scenario)?;
        Ok((turns.len() > before).then_some(turns))
    })?;
    assert_eq!(turns.len(), before + 1, "{turns:?}");
    assert_eq!(turns[before].text, PROMPT);
    wait_until("the main agent to be started for it", || {
        Ok(Some(sessions_of(&scenario, "claude")?).filter(|sessions| !sessions.is_empty()))
    })?;
    let notices = notices(&scenario)?;
    assert!(
        !notices
            .iter()
            .any(|notice| notice.contains("unknown command")),
        "the path was read as a command: {notices:?}"
    );
    scenario.shot(&pty, "app_a_prompt_led_by_a_path_is_a_request")?;
    Ok(())
}

#[test]
fn app_routes_input_by_mode() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;

    // Idle: plain text is a new request.
    submit(&mut pty, requests::HI)?;
    wait_turns_finished(&scenario, 1)?;

    // Working: plain text is guidance for the run, not another turn.
    submit(&mut pty, &format!("/captain {TASK_IN_A_TOOL_CALL}"))?;
    wait_mode(&scenario, "the run to work", |m| {
        matches!(m, AppMode::Working { .. })
    })?;
    wait_worker_started(&scenario)?;
    let run_dir = wait_run_dir(&scenario)?;
    let guidance = "keep the old name as an alias";
    submit(&mut pty, guidance)?;
    wait_journal(&run_dir, "the guidance in the journal", |event| {
        serde_json::to_string(event).is_ok_and(|line| line.contains(guidance))
    })?;
    assert_eq!(
        turns_started(&scenario)?.len(),
        1,
        "guidance became a conversation turn"
    );

    // Pausing: Enter on an empty prompt goes up as it is, and the app cancels the pause.
    argonauts_scenarios::pause_run(&mut pty)?;
    wait_mode(&scenario, "the run to start pausing", |m| {
        matches!(m, AppMode::Pausing { .. })
    })?;
    check_bottom_row(&pty)?;
    pty.press(Key::Enter)?;
    wait_until("the empty prompt to go up", || {
        Ok(commands(&scenario)?
            .into_iter()
            .find(|c| matches!(c, UiCommand::Submit { text, .. } if text.is_empty())))
    })?;
    wait_journal(&run_dir, "PauseCancelled", |event| {
        matches!(event.kind, RunEventKind::Run(RunLifecycle::PauseCancelled))
    })?;
    scenario.shot(&pty, "app_routes_input_by_mode")?;
    Ok(())
}

#[test]
fn app_change_request_picks_captain_or_crew() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, requests::SMALL_FIX)?;

    match nth_reply(&scenario, 1)? {
        TurnReply::Change {
            captain_or_crew,
            estimate,
            ..
        } => {
            assert_eq!(
                captain_or_crew.mode,
                RunMode::Captain,
                "{captain_or_crew:?}"
            );
            assert_eq!(estimate.task_type, "docs");
        }
        other => panic!("expected a change, got {other:?}"),
    }
    let started = run_started(&wait_run_dir(&scenario)?)?;
    assert_eq!(started.mode, RunMode::Captain);
    assert_eq!(
        started.estimate.as_ref().map(|e| e.file_count),
        Some(1),
        "{started:?}"
    );
    assert_eq!(
        started.captain_or_crew.as_ref().map(|t| t.mode),
        Some(RunMode::Captain)
    );
    // A run's start prints no row of its own: its agent's row marks it.
    pty.wait_for(r"(?m)^[●⏺] build:", WAIT)?;
    scenario.shot(&pty, "app_change_request_picks_captain_or_crew")?;
    Ok(())
}

#[test]
fn app_unclear_offers_captain_crew_no() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, requests::UNCLEAR_SMALL)?;

    assert!(matches!(
        nth_reply(&scenario, 1)?,
        TurnReply::Unclear { .. }
    ));
    let question = wait_open_choice(&scenario, ChoiceKind::StartWork)?;
    let options: Vec<(char, &str)> = question
        .options
        .iter()
        .map(|o| (o.key, o.label.as_str()))
        .collect();
    assert_eq!(
        options,
        [
            ('s', "Captain"),
            ('c', "Crew run"),
            ('n', "No, just asking")
        ],
        "{question:?}"
    );
    let question_text = format!("{} {}", question.title, question.body.join(" "));
    assert!(
        question_text.contains("Argonauts Code would pick captain"),
        "{question_text}"
    );

    answer_question(&mut pty, number_of(&question.options, 's'))?;
    wait_choice_closed(&scenario, &question.id)?;
    let started = run_started(&wait_run_dir(&scenario)?)?;
    assert_eq!(started.mode, RunMode::Captain);
    assert_eq!(started.request, requests::UNCLEAR_SMALL);
    scenario.shot(&pty, "app_unclear_offers_captain_crew_no")?;
    Ok(())
}

#[test]
fn app_ask_never_starts_a_run() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, &format!("/ask {}", requests::ASK))?;

    wait_until("the /ask command", || {
        Ok(commands(&scenario)?.into_iter().find(|c| {
            matches!(
                c,
                UiCommand::Slash {
                    command: SlashCommand::Ask { .. }
                }
            )
        }))
    })?;
    assert!(
        matches!(nth_reply(&scenario, 1)?, TurnReply::Answer { .. }),
        "/ask got something other than an answer"
    );
    pty.wait_for("Split it once the cache lands", WAIT)?;
    check_bottom_row(&pty)?;
    assert!(
        !events(&scenario)?.iter().any(is_start_work),
        "/ask offered work"
    );
    assert!(run_dirs(&scenario).is_empty(), "/ask started a run");
    scenario.shot(&pty, "app_ask_never_starts_a_run")?;
    Ok(())
}

#[test]
fn app_captain_and_crew_skip_estimate() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;

    submit(&mut pty, &format!("/captain {}", requests::SMALL_FIX))?;
    let captain_dir = wait_run_dir(&scenario)?;
    let captain = run_started(&captain_dir)?;
    assert_eq!(captain.mode, RunMode::Captain);
    assert!(captain.forced, "/captain wasn't recorded as forced");
    assert!(
        captain.estimate.is_none() && captain.captain_or_crew.is_none(),
        "{captain:?}"
    );

    submit(&mut pty, &format!("/crew {}", requests::CREW_TASK))?;
    // Run ids sort by the second they started, so two runs in one second can sort either way.
    let crew_dir = wait_until("a second run", || {
        Ok(run_dirs(&scenario)
            .into_iter()
            .find(|dir| *dir != captain_dir))
    })?;
    let crew = run_started(&crew_dir)?;
    assert_eq!(crew.mode, RunMode::Crew);
    assert!(crew.forced && crew.estimate.is_none(), "{crew:?}");

    // The estimate is the only session whose schema asks for `answer_markdown`.
    let estimates: Vec<_> = sessions_of(&scenario, "claude")?
        .into_iter()
        .filter(|i| i.args.iter().any(|arg| arg.contains("answer_markdown")))
        .collect();
    assert!(estimates.is_empty(), "an estimate ran: {estimates:?}");
    assert!(
        turns_started(&scenario)?.is_empty(),
        "a conversation turn ran"
    );
    scenario.shot(&pty, "app_captain_and_crew_skip_estimate")?;
    Ok(())
}

/// Whether a turn's arguments resume the conversation's CLI session.
type ResumeCheck = fn(&[String]) -> bool;

#[test]
fn app_conversation_resumes_cli_session() -> anyhow::Result<()> {
    let agents: [(&str, &str, ResumeCheck); 3] = [
        ("claude-code", "claude", |args| {
            has_flag(args, "--resume", "sess-conversation-claude")
        }),
        ("codex", "codex", |args| {
            args.first().is_some_and(|a| a == "exec")
                && has_words(args, &["resume", "thread-conversation-codex"])
        }),
        ("antigravity-cli", "agy", |args| {
            has_flag(args, "--conversation", "conv-conversation-agy")
        }),
    ];
    for (agent, program, resumes) in agents {
        // Codex never reports which model ran, so its adapter launches only with one chosen.
        let config = format!(
            "{}\n[agents.codex]\nmodel = \"gpt-5.6-luna\"\n",
            config_with_main_agent(agent)
        );
        let scenario = scenario_with(&config)?;
        let mut pty = start(&scenario)?;
        submit(&mut pty, requests::HI)?;
        wait_turns_finished(&scenario, 1)?;
        submit(&mut pty, "tell me more")?;
        wait_turns_finished(&scenario, 2)?;

        let sessions = sessions_of(&scenario, program)?;
        assert_eq!(sessions.len(), 2, "{agent}: {sessions:?}");
        assert!(!resumes(&sessions[0].args), "{agent}'s first turn resumed");
        assert!(
            resumes(&sessions[1].args),
            "{agent}'s second turn didn't resume: {:?}",
            sessions[1].args
        );
        scenario.shot(
            &pty,
            &format!("app_conversation_resumes_cli_session_{program}"),
        )?;
        quit(&mut pty)?;
    }
    Ok(())
}

#[test]
fn app_conversation_turn_records_settings_hash() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, requests::HI)?;
    wait_turns_finished(&scenario, 1)?;
    submit(&mut pty, "tell me more")?;
    wait_turns_finished(&scenario, 2)?;

    let turns = turns_started(&scenario)?;
    let first = turns[0].settings_sha256.clone().expect("no settings hash");
    assert_eq!(first.len(), 64, "{first}");
    assert!(first.chars().all(|c| c.is_ascii_hexdigit()), "{first}");
    assert_eq!(turns[1].settings_sha256.as_deref(), Some(first.as_str()));

    submit(&mut pty, "/effort high")?;
    answer_scope(&scenario, &mut pty, 's', None)?;
    wait_setting_changed(&scenario, "roles.main.effort", 1)?;
    submit(&mut pty, "and once more")?;
    wait_turns_finished(&scenario, 3)?;
    let third = turns_started(&scenario)?[2].settings_sha256.clone();
    assert!(
        third.is_some() && third.as_deref() != Some(first.as_str()),
        "the hash didn't follow the change: {third:?}"
    );

    // The conversation's own journal keeps the hash too.
    let conversation_dir = std::fs::read_dir(scenario.repo.home().join("state"))?
        .filter_map(Result::ok)
        .map(|repo| repo.path().join("conversation"))
        .find(|dir| dir.is_dir())
        .expect("no conversation journal");
    let recorded = std::fs::read_dir(&conversation_dir)?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|e| e == "jsonl"))
        .any(|entry| read(&entry.path()).is_ok_and(|text| text.contains(&first)));
    assert!(
        recorded,
        "no conversation journal in {} holds the hash",
        conversation_dir.display()
    );
    scenario.shot(&pty, "app_conversation_turn_records_settings_hash")?;
    Ok(())
}

#[test]
fn app_cancel_turn_kills_agent_process() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, requests::HANGING_TURN)?;

    let pid = wait_until("the conversation session to start", || {
        Ok(claude_sessions(&scenario, "dontAsk")?
            .first()
            .map(|i| i.pid))
    })?;
    pty.wait_for("Let me look through the repo first", WAIT)?;
    assert!(pid_alive(pid));

    pty.press(Key::Esc)?;
    wait_event(&scenario, "the turn to be cancelled", |event| {
        matches!(
            event,
            AppEvent::Conversation {
                event: ConversationEvent::TurnCancelled { .. }
            }
        )
    })?;
    assert!(commands(&scenario)?.contains(&UiCommand::CancelTurn));
    wait_pid_gone(pid)?;
    let screen = pty.wait_for(
        "Interrupted · What should Argonauts Code do instead\\?",
        WAIT,
    )?;
    assert_eq!(
        turn_end(&screen.text()),
        TURN_END,
        "a turn-end line followed the interruption"
    );
    check_bottom_row(&pty)?;
    scenario.shot(&pty, "app_cancel_turn_kills_agent_process")?;
    Ok(())
}

#[test]
fn app_uncommitted_left_out_and_said() -> anyhow::Result<()> {
    let scenario = scenario()?;
    scenario.repo.write("notes.txt", "scratch notes\n")?;
    scenario
        .repo
        .write("README.md", "# Test repo\n\nlocal edit\n")?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, requests::SMALL_FIX)?;

    wait_turns_finished(&scenario, 1)?;
    let mut left_out = turns_started(&scenario)?[0].uncommitted.clone();
    left_out.sort();
    assert_eq!(left_out, ["README.md", "notes.txt"]);
    let estimate = claude_sessions(&scenario, "dontAsk")?;
    assert!(
        estimate[0]
            .stdin
            .as_deref()
            .is_some_and(|prompt| prompt.contains("uncommitted")),
        "the estimate's prompt doesn't mention the uncommitted files"
    );

    let run_dir = wait_run_dir(&scenario)?;
    let started = run_started(&run_dir)?;
    assert!(!started.uncommitted_included);
    let mut recorded = started.uncommitted.clone();
    recorded.sort();
    assert_eq!(recorded, ["README.md", "notes.txt"]);
    pty.wait_for("2 uncommitted files left out", WAIT)?;

    wait_approval(
        &scenario,
        argonauts_core::events::approval::ApprovalKind::Merge,
    )?;
    let run = run_id(&run_dir)?;
    for branch in run_branches(&scenario, &run)? {
        let files = branch_files(&scenario, &branch)?;
        assert!(
            !files.contains(&"notes.txt".to_owned()),
            "{branch} took the uncommitted file"
        );
    }
    scenario.shot(&pty, "app_uncommitted_left_out_and_said")?;
    Ok(())
}

#[test]
fn app_include_uncommitted_setting() -> anyhow::Result<()> {
    let scenario = scenario_with(&format!(
        "{GLOBAL_CONFIG}\n[intake]\ninclude_uncommitted = true\n"
    ))?;
    scenario.repo.write("notes.txt", "scratch notes\n")?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, requests::SMALL_FIX)?;

    wait_turns_finished(&scenario, 1)?;
    assert!(
        turns_started(&scenario)?[0].uncommitted.is_empty(),
        "files were left out although the setting includes them"
    );
    let run_dir = wait_run_dir(&scenario)?;
    let started = run_started(&run_dir)?;
    assert!(started.uncommitted_included, "{started:?}");

    wait_approval(
        &scenario,
        argonauts_core::events::approval::ApprovalKind::Merge,
    )?;
    let run = run_id(&run_dir)?;
    let branches = run_branches(&scenario, &run)?;
    assert!(
        branches
            .iter()
            .any(|b| branch_files(&scenario, b).is_ok_and(|f| f.contains(&"notes.txt".to_owned()))),
        "no branch of {run} holds the uncommitted file: {branches:?}"
    );
    scenario.shot(&pty, "app_include_uncommitted_setting")?;
    Ok(())
}

#[test]
fn app_rules_pick_crew_by_labels() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, requests::LABELED_TASK)?;

    match nth_reply(&scenario, 1)? {
        TurnReply::Change {
            captain_or_crew,
            estimate,
            ..
        } => {
            assert!(estimate.file_count <= 3, "the test wants a small change");
            assert_eq!(captain_or_crew.mode, RunMode::Crew, "{captain_or_crew:?}");
            let why = format!("{} {}", captain_or_crew.rule, captain_or_crew.reason);
            assert!(
                why.contains("migration"),
                "the rules didn't name the label: {why}"
            );
        }
        other => panic!("expected a change, got {other:?}"),
    }
    let started = run_started(&wait_run_dir(&scenario)?)?;
    assert_eq!(started.mode, RunMode::Crew);
    // The crew's first agent marks the start, with no row above it.
    pty.wait_for(r"(?m)^[●⏺] main:design", WAIT)?;
    scenario.shot(&pty, "app_rules_pick_crew_by_labels")?;
    Ok(())
}

#[test]
fn app_watching_costs_no_tokens() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let waiting = fabricate_waiting_run(&scenario, 1)?;
    let journal_before = run_events(&waiting.dir)?.len();
    let mut pty = start(&scenario)?;

    let views = [
        "/status",
        "/context",
        "/workflows",
        "/rewind",
        "/config",
        "/approvals",
        "/resume",
        "/help",
    ];
    for view in views {
        submit(&mut pty, view)?;
        settle(&scenario)?;
        // A view the app answers opens when its answer comes, so an Esc sent before then would
        // leave it open over the conversation once it does.
        wait_for_view(&pty);
        pty.press(Key::Esc)?;
        settle(&scenario)?;
    }
    for key in [
        Key::Char('?'),
        Key::Ctrl('o'),
        Key::Ctrl('t'),
        Key::Left,
        Key::Down,
    ] {
        check_bottom_row(&pty)?;
        pty.press(key)?;
        settle(&scenario)?;
        pty.press(Key::Esc)?;
        settle(&scenario)?;
    }

    let sessions = all_sessions(&scenario)?;
    assert!(
        sessions.is_empty(),
        "watching started sessions: {sessions:?}"
    );
    assert!(
        turns_started(&scenario)?.is_empty(),
        "watching started a conversation turn"
    );
    let journal: Vec<RunEvent> = run_events(&waiting.dir)?;
    assert_eq!(journal.len(), journal_before, "watching wrote to the run");
    scenario.shot(&pty, "app_watching_costs_no_tokens")?;
    Ok(())
}

/// Waits for the view a command opens, which hides the bottom line, or for the screen to hold
/// still a second after a command that opens none. Five seconds at most.
fn wait_for_view(pty: &Pty) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = String::new();
    let mut still_since = Instant::now();
    while Instant::now() < deadline {
        let text = pty.screen().visible_text();
        if !text.contains("shift+tab to cycle") {
            return;
        }
        if text != last {
            last = text;
            still_since = Instant::now();
        } else if still_since.elapsed() >= Duration::from_secs(1) {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Lets the app answer a key before the next one, since views answer with no event to wait on.
fn settle(scenario: &Scenario) -> anyhow::Result<()> {
    let before = scenario.trace.lines()?.len();
    for _ in 0..20 {
        thread::sleep(Duration::from_millis(50));
        if scenario.trace.lines()?.len() > before {
            break;
        }
    }
    Ok(())
}
