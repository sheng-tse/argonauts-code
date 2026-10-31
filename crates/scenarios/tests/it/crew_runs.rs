//! Crew runs through the real binary with fake agents: WORKFLOW 12's rate-limiting example end
//! to end, its goal question, and your answers at the understand, plan, and merge approvals.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};

use argonauts_core::TaskId;
use argonauts_core::agents::session_runner::Access;
use argonauts_core::runs::events::approval::{AnswerAction, ApprovalEvent, ApprovalKind};
use argonauts_core::runs::events::goal::GoalEvent;
use argonauts_core::runs::events::plan::{DivideEvent, PlanEvent};
use argonauts_core::runs::events::run::{Party, RunLifecycle, RunOutcome, WaitReason};
use argonauts_core::runs::events::session::SessionEvent;
use argonauts_core::runs::events::task::{MergeEvent, TaskEvent, TaskState};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_scenarios::{Scenario, WAIT, as_set_up, past_the_question_guard, scenario_file};
use argonauts_testkit::{FakeScenario, Key, Pty};

const REQUEST: &str = "add per-key rate limiting to the public API";

/// A crew run takes many steps; each wait covers the steps up to the next approval.
const STEPS_WAIT: Duration = Duration::from_secs(180);

/// A global config means no guided setup; it names the example's crew. The understand, plan, and
/// merge approvals wait for you, since these runs are about your answers there. The fakes
/// answer the fixed phases, so the workflow runs those. A request typed as plain text ends
/// with the verdict, which a CLI that takes no tools keeps.
const CREW_CONFIG: &str = r#"# The crew's fixed phases, whose drafts, map and judging the scenario scripts.
workflow = "fixed-phases"
crew = ["claude-code", "codex", "antigravity-cli"]

[main_session]
tools = false

[ui]
theme = "dark"

[approvals.understand]
mode = "supervised"

[approvals.plan]
mode = "supervised"

[approvals.merge]
mode = "supervised"

[agents.codex]
model = "gpt-5.6-luna"

[limits]
dependency_allowlist = ["ioredis"]

[workflows]
adapt_with = "phases"

[[checks.run]]
name = "acceptance"
command = ["sh", "scripts/acceptance.sh"]
"#;

/// Stands in for npm, offline: Argonauts Code's install of the plan's new dependency adds it to
/// the manifest and the lockfile.
const NPM: &str = r#"#!/bin/sh
for arg in "$@"; do
  case "$arg" in install|-*) ;; *) package=$arg ;; esac
done
sed "s|\"scripts\"|\"dependencies\": { \"$package\": \"^5.0.0\" }, \"scripts\"|" package.json > package.json.new
mv package.json.new package.json
printf '{ "name": "acme-api", "lockfileVersion": 3, "packages": { "node_modules/%s": { "version": "5.0.0" } } }\n' "$package" > package-lock.json
"#;

/// Stands in for the test suite: each acceptance test fails until the file it checks is built.
const ACCEPTANCE: &str = r#"for test in tests/acceptance/*/limit.test.ts; do
  [ -e "$test" ] || continue
  built=$(sed -n 's|^// checks: ||p' "$test")
  [ -s "$built" ] || { echo "$test: $built isn't built yet"; exit 1; }
done
"#;

const ENTRY: &str = r#"import express from "express";
import { auth } from "./auth";

export const app = express();
app.use(auth);
app.get("/v1/items", (_req, res) => res.json([]));
"#;

const AUTH: &str = r#"export function auth(_req: unknown, _res: unknown, next: () => void) {
  next();
}
"#;

fn crew_scenario() -> anyhow::Result<Scenario> {
    let scenario = Scenario::new("crew/rate_limit.toml")?;
    let config = scenario.repo.global_config();
    if let Some(dir) = config.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&config, as_set_up(CREW_CONFIG))?;
    crate::app_support::write_fixed_phases(&config)?;
    scenario.repo.write(
        "package.json",
        "{ \"name\": \"acme-api\", \"scripts\": { \"test\": \"vitest run\" } }\n",
    )?;
    scenario.repo.write("src/api/server.ts", ENTRY)?;
    scenario.repo.write("src/api/auth.ts", AUTH)?;
    scenario.repo.write("scripts/acceptance.sh", ACCEPTANCE)?;
    scenario.repo.commit_all("the public API")?;
    let npm = scenario.clis.bin_dir().join("npm");
    fs::write(&npm, NPM)?;
    fs::set_permissions(&npm, fs::Permissions::from_mode(0o755))?;
    Ok(scenario)
}

/// `argonauts run --crew` with the request: the same engine and tasks as the app, for scripts.
/// Without a conversation there's no estimate for the Captain-or-crew rules to read, so the
/// example's crew is asked for.
fn run(scenario: &Scenario) -> anyhow::Result<Pty> {
    scenario.launch(&["run", "--crew", REQUEST])
}

/// Waits until the run's log holds an event that `found` picks, and returns it.
fn wait_for_event<T>(
    scenario: &Scenario,
    timeout: Duration,
    found: impl Fn(&RunEvent) -> Option<T>,
) -> anyhow::Result<T> {
    let deadline = argonauts_testkit::pty::deadline::after(timeout);
    loop {
        if let Ok(events) = scenario.latest_run_events()
            && let Some(hit) = events.iter().find_map(&found)
        {
            return Ok(hit);
        }
        if Instant::now() >= deadline {
            bail!("no such event within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn opened(event: &RunEvent, kind: ApprovalKind) -> Option<u64> {
    match &event.kind {
        RunEventKind::Approval(ApprovalEvent::Opened(opened)) if opened.kind == kind => {
            Some(event.seq)
        }
        _ => None,
    }
}

fn finished(event: &RunEvent) -> Option<RunOutcome> {
    match &event.kind {
        RunEventKind::Run(RunLifecycle::Finished { outcome, .. }) => Some(*outcome),
        _ => None,
    }
}

/// The goal question: `1` picks the recommended answer, per API key.
fn answer_goal_question(pty: &mut Pty) -> anyhow::Result<()> {
    pty.wait_for(r"◆ What should the goal say\?", STEPS_WAIT)?;
    past_the_question_guard();
    pty.press(Key::Up)?;
    pty.press(Key::Char('1'))?;
    pty.wait_for(r"◆ Goal question answered · (?i:per API key)", WAIT)?;
    Ok(())
}

fn approve_plan(pty: &mut Pty) -> anyhow::Result<()> {
    pty.wait_for(r"◆ Approve this plan\?", STEPS_WAIT)?;
    past_the_question_guard();
    pty.press(Key::Up)?;
    pty.press(Key::Char('1'))?;
    Ok(())
}

/// Merge waits for Enter after its number.
fn merge(pty: &mut Pty) -> anyhow::Result<()> {
    pty.wait_for(r"◆ Merge", STEPS_WAIT)?;
    past_the_question_guard();
    pty.press(Key::Up)?;
    pty.press(Key::Char('1'))?;
    pty.wait_for("Enter to merge", WAIT)?;
    pty.press(Key::Enter)?;
    Ok(())
}

/// When the first approval of this kind was decided, and how.
fn decided(events: &[RunEvent], kind: ApprovalKind) -> Option<(u64, AnswerAction)> {
    let ids: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) if opened.kind == kind => {
                Some(opened.id.clone())
            }
            _ => None,
        })
        .collect();
    events.iter().find_map(|event| match &event.kind {
        RunEventKind::Approval(ApprovalEvent::Decided(decided)) if ids.contains(&decided.id) => {
            Some((event.seq, decided.action.clone()))
        }
        _ => None,
    })
}

fn seq_of(events: &[RunEvent], found: impl Fn(&RunEvent) -> bool) -> Option<u64> {
    events
        .iter()
        .find(|event| found(event))
        .map(|event| event.seq)
}

#[test]
fn crew_scenarios_parse() -> anyhow::Result<()> {
    for name in ["rate_limit", "mention", "steer", "status_tool"] {
        let scenario = FakeScenario::load(&scenario_file(&format!("crew/{name}.toml")))?;
        assert!(!scenario.rules.is_empty(), "{name}");
        for program in ["claude", "codex", "agy"] {
            assert!(
                scenario.version.contains_key(program),
                "{name} names {program}'s version"
            );
        }
    }
    Ok(())
}

#[test]
fn crew_rate_limit_example_end_to_end() -> anyhow::Result<()> {
    let scenario = crew_scenario()?;
    let mut pty = run(&scenario)?;
    answer_goal_question(&mut pty)?;
    approve_plan(&mut pty)?;
    merge(&mut pty)?;
    let outcome = wait_for_event(&scenario, STEPS_WAIT, finished)?;
    assert_eq!(outcome, RunOutcome::Merged);
    scenario.shot(&pty, "crew-rate-limit-merged")?;
    let events = scenario.latest_run_events()?;

    // The goal: three readings, two comparers, one scope question you answered, and the
    // Retry-After detail adopted as an assumption.
    let readings = events
        .iter()
        .filter(|event| matches!(event.kind, RunEventKind::Goal(GoalEvent::Read { .. })))
        .count();
    assert_eq!(readings, 3);
    let compared = events
        .iter()
        .filter(|event| matches!(event.kind, RunEventKind::Goal(GoalEvent::Compared { .. })))
        .count();
    assert_eq!(compared, 2);
    let (questions, assumptions) = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Goal(GoalEvent::Settled {
                questions,
                assumptions,
                ..
            }) => Some((questions.clone(), assumptions.clone())),
            _ => None,
        })
        .context("the goal was settled")?;
    assert_eq!(questions.len(), 1);
    assert!(questions[0].text.contains("per API key"));
    assert!(
        assumptions
            .iter()
            .any(|assumed| assumed.text.contains("seconds"))
    );
    let goal = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Goal(GoalEvent::Approved { file, .. }) => Some(file.clone()),
            _ => None,
        })
        .context("the goal was approved")?;
    assert!(
        goal.answers
            .iter()
            .any(|answer| answer.contains("Per API key"))
    );

    // Planning: blind drafts, a map, a critique round, revisions, an experiment per option, and a
    // judged winner that you approved.
    let sealed: Vec<u8> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Plan(PlanEvent::Sealed { round, .. }) => Some(*round),
            _ => None,
        })
        .collect();
    assert_eq!(sealed.iter().filter(|round| **round == 1).count(), 3);
    assert_eq!(sealed.iter().filter(|round| **round == 2).count(), 3);
    let has = |found: fn(&RunEventKind) -> bool| events.iter().any(|event| found(&event.kind));
    assert!(has(|kind| matches!(
        kind,
        RunEventKind::Plan(PlanEvent::Mapped { .. })
    )));
    assert!(has(|kind| matches!(
        kind,
        RunEventKind::Plan(PlanEvent::Critiqued { .. })
    )));
    assert!(has(|kind| matches!(
        kind,
        RunEventKind::Plan(PlanEvent::ExperimentProposed { .. })
    )));
    let experiments = events
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                RunEventKind::Plan(PlanEvent::ExperimentFinished { .. })
            )
        })
        .count();
    assert_eq!(experiments, 2);
    let winner = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Plan(PlanEvent::Judged(result)) => result.winner,
        _ => None,
    });
    assert_eq!(winner, Some(2));

    // Nothing builds before you approve the goal and the plan.
    let plan_decided = decided(&events, ApprovalKind::Plan)
        .filter(|(_, action)| *action == AnswerAction::Approve)
        .map(|(seq, _)| seq)
        .context("you approved the plan")?;
    let first_build = seq_of(&events, |event| {
        matches!(&event.kind, RunEventKind::Session(SessionEvent::Started(started))
            if started.access == Access::Build)
    })
    .context("tasks were built")?;
    assert!(first_build > plan_decided);

    // Five tasks from the plan, each merged; ioredis added by Argonauts Code; the final check
    // passed; the run merged on main.
    let graph = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Divide(DivideEvent::Graph(graph)) => Some(graph.clone()),
            _ => None,
        })
        .context("the task graph")?;
    let built: BTreeSet<String> = graph
        .tasks
        .iter()
        .flat_map(|question| question.owned_paths.clone())
        .collect();
    for path in [
        "src/limiter/index.ts",
        "src/limiter/store.ts",
        "src/api/rate-limit.ts",
        "docs/rate-limits.md",
    ] {
        assert!(built.contains(path), "{path} is some task's");
    }
    let merged: BTreeSet<TaskId> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Merge(MergeEvent::Merged { task, .. }) => Some(task.clone()),
            _ => None,
        })
        .collect();
    assert!(merged.len() >= 5, "{merged:?}");
    assert!(events.iter().any(|event| matches!(&event.kind,
        RunEventKind::Merge(MergeEvent::DependenciesAdded { packages, .. })
            if packages.iter().any(|package| package == "ioredis"))));
    assert!(events.iter().any(|event| matches!(
        event.kind,
        RunEventKind::Merge(MergeEvent::FinalCheck { passed: true })
    )));
    assert!(events.iter().any(|event| matches!(
        event.kind,
        RunEventKind::Merge(MergeEvent::RunMerged { .. })
    )));
    let store = scenario.repo.git(&["show", "main:src/limiter/store.ts"])?;
    assert!(store.contains("RedisStore"));

    // A crew of three agents did it.
    let called: BTreeSet<String> = scenario
        .clis
        .invocations()?
        .into_iter()
        .map(|invocation| invocation.argv0)
        .collect();
    for program in ["claude", "codex", "agy"] {
        assert!(called.contains(program), "{called:?}");
    }
    Ok(())
}

#[test]
fn crew_example_question() -> anyhow::Result<()> {
    let scenario = crew_scenario()?;
    let mut pty = scenario.launch(&[])?;
    pty.wait_for(r"(?m)^  ǁ supervised mode on|⏵⏵ supervised", WAIT)?;
    pty.send(REQUEST)?;
    pty.press(Key::Enter)?;

    // The model-independent lines for a bigger task: three readers and two comparers, the
    // detail adopted as an assumption, then the question. No row of the run's own comes first.
    pty.wait_for(
        r"(?m)^[●⏺] 3 agents started.*\n(?:\s+[├└] read:\S+ · .*\n){3}",
        STEPS_WAIT,
    )?;
    pty.wait_for(
        r"(?m)^[●⏺] 2 agents started.*\n(?:\s+[├└] compare:\S+ · .*\n){2}",
        STEPS_WAIT,
    )?;
    pty.wait_for(r"Assumed\W+Retry-After format: seconds", WAIT)?;
    let question = pty.wait_for(r"(?m)^◆ What should the goal say\?", STEPS_WAIT)?;
    let text = question.visible_text();
    assert!(text.contains("per API key"), "{text}");
    assert!(text.contains("Per API key (recommended)"), "{text}");
    assert!(text.contains("2 of 3 readings read it this way"), "{text}");
    // Choose-only: the last answer hands this and the rest to the main agent.
    assert!(
        text.contains("Let the main agent decide this and the rest"),
        "{text}"
    );
    assert!(!text.contains("Type my own answer"), "{text}");
    scenario.shot(&pty, "crew-question")?;

    // No agent works while the question waits, so the run waits until you answer.
    let waiting = wait_for_event(&scenario, WAIT, |event| match &event.kind {
        RunEventKind::Run(RunLifecycle::Waiting {
            reason: WaitReason::Approval,
            waiting_on,
        }) => Some(waiting_on.len()),
        _ => None,
    })?;
    assert_eq!(waiting, 1);

    past_the_question_guard();
    pty.press(Key::Up)?;
    pty.press(Key::Char('1'))?;
    pty.wait_for(r"◆ Goal question answered · (?i:per API key)", STEPS_WAIT)?;
    pty.wait_for(
        r"(?m)^[●⏺] Goal\([^⎿]*⎿\s+Agreed by 3 agents · 1 question answered",
        STEPS_WAIT,
    )?;
    scenario.shot(&pty, "crew-question-answered")?;
    // Planning starts with the blind drafts.
    wait_for_event(&scenario, STEPS_WAIT, |event| {
        matches!(event.kind, RunEventKind::Plan(PlanEvent::Sealed { .. })).then_some(())
    })?;
    Ok(())
}

#[test]
fn crew_goal_answer_approves() -> anyhow::Result<()> {
    let scenario = crew_scenario()?;
    let mut pty = run(&scenario)?;
    answer_goal_question(&mut pty)?;
    wait_for_event(&scenario, STEPS_WAIT, |event| {
        matches!(event.kind, RunEventKind::Plan(PlanEvent::Sealed { .. })).then_some(())
    })?;
    let events = scenario.latest_run_events()?;

    // Your answer to the last question approved the goal: no second question asked for it.
    let goal_questions = events
        .iter()
        .filter(|event| opened(event, ApprovalKind::Understand).is_some())
        .count();
    assert_eq!(goal_questions, 1);
    let (file, by) = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Goal(GoalEvent::Approved { file, by }) => {
                Some((file.clone(), by.clone()))
            }
            _ => None,
        })
        .context("the goal was approved")?;
    assert_eq!(by, argonauts_core::runs::events::run::Party::User);
    assert!(
        file.answers
            .iter()
            .any(|answer| answer.contains("Per API key"))
    );
    assert!(!file.sha256.is_empty());
    // Drafting started only after it.
    let approved = seq_of(&events, |event| {
        matches!(event.kind, RunEventKind::Goal(GoalEvent::Approved { .. }))
    })
    .context("approved")?;
    let drafted = seq_of(&events, |event| {
        matches!(event.kind, RunEventKind::Plan(PlanEvent::Sealed { .. }))
    })
    .context("drafted")?;
    assert!(drafted > approved);
    Ok(())
}

#[test]
fn crew_plan_send_back_replans() -> anyhow::Result<()> {
    let note = "Keep the middleware after auth, and put the counters in Redis.";
    let scenario = crew_scenario()?;
    let mut pty = run(&scenario)?;
    answer_goal_question(&mut pty)?;

    // Send back is the third option; Tab adds your note to it.
    pty.wait_for(r"◆ Approve this plan\?", STEPS_WAIT)?;
    let first = wait_for_event(&scenario, WAIT, |event| opened(event, ApprovalKind::Plan))?;
    past_the_question_guard();
    pty.press(Key::Up)?;
    pty.press(Key::Down)?;
    pty.press(Key::Down)?;
    pty.press(Key::Tab)?;
    pty.send(note)?;
    pty.press(Key::Enter)?;

    let sent_back = wait_for_event(&scenario, WAIT, |event| match &event.kind {
        RunEventKind::Approval(ApprovalEvent::Decided(decided))
            if decided.action == AnswerAction::SendBack =>
        {
            Some((event.seq, decided.text.clone()))
        }
        _ => None,
    })?;
    assert_eq!(sent_back.1.as_deref(), Some(note));

    // Planning runs again, with your note in the new drafts' prompts, and asks you again.
    let second = wait_for_event(&scenario, STEPS_WAIT, |event| {
        opened(event, ApprovalKind::Plan).filter(|seq| *seq > first)
    })?;
    let events = scenario.latest_run_events()?;
    let redrafted = events
        .iter()
        .filter(|event| event.seq > sent_back.0 && event.seq < second)
        .filter(|event| matches!(event.kind, RunEventKind::Plan(PlanEvent::Sealed { .. })))
        .count();
    assert!(redrafted >= 3, "every drafting agent drafts again");
    let noted = events
        .iter()
        .filter(|event| event.seq > sent_back.0)
        .any(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Input(input)) => {
                input.parts.iter().any(|part| part.text.contains(note))
            }
            _ => false,
        });
    assert!(noted, "your note reaches the new drafts");
    scenario.shot(&pty, "crew-plan-sent-back")?;
    Ok(())
}

#[test]
fn crew_merge_request_changes() -> anyhow::Result<()> {
    let scenario = crew_scenario()?;
    let mut pty = run(&scenario)?;
    answer_goal_question(&mut pty)?;
    approve_plan(&mut pty)?;

    // Request changes on some tasks: the merge rows become a checklist. Pick the second task
    // only.
    pty.wait_for(r"◆ Merge", STEPS_WAIT)?;
    let first = wait_for_event(&scenario, WAIT, |event| opened(event, ApprovalKind::Merge))?;
    past_the_question_guard();
    pty.press(Key::Up)?;
    // Request changes on some tasks, the third option.
    pty.press(Key::Char('3'))?;
    pty.press(Key::Down)?;
    pty.press(Key::Char(' '))?;
    // Enter opens the note on the ticked tasks, and Enter again sends them without one.
    pty.press(Key::Enter)?;
    pty.wait_for("Enter to send · Esc to go back", WAIT)?;
    pty.press(Key::Enter)?;

    let (decided_at, picked) = wait_for_event(&scenario, WAIT, |event| match &event.kind {
        RunEventKind::Approval(ApprovalEvent::Decided(decided))
            if decided.action == AnswerAction::RequestChanges =>
        {
            Some((event.seq, decided.tasks.clone()))
        }
        _ => None,
    })?;
    assert_eq!(picked.len(), 1, "one task picked: {picked:?}");
    let returned = picked[0].clone();

    // Only the picked task goes back to its worker; the others stay as they were.
    let again = wait_for_event(&scenario, STEPS_WAIT, |event| {
        opened(event, ApprovalKind::Merge).filter(|seq| *seq > first)
    })?;
    let events = scenario.latest_run_events()?;
    let between = || {
        events
            .iter()
            .filter(|event| event.seq > decided_at && event.seq < again)
    };
    let moved: BTreeSet<TaskId> = between()
        .filter_map(|event| match &event.kind {
            RunEventKind::Task(TaskEvent::State { .. }) => event.task_id.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(moved, BTreeSet::from([returned.clone()]));
    let rebuilt: BTreeSet<TaskId> = between()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started))
                if started.access == Access::Build =>
            {
                event.task_id.clone()
            }
            _ => None,
        })
        .collect();
    assert_eq!(rebuilt, BTreeSet::from([returned.clone()]));
    assert!(
        between().any(|event| event.task_id.as_ref() == Some(&returned)
            && matches!(
                &event.kind,
                RunEventKind::Task(TaskEvent::State {
                    to: TaskState::Merged,
                    ..
                })
            ))
    );
    scenario.shot(&pty, "crew-merge-request-changes")?;
    Ok(())
}

/// Autopilot through the binary: every approval auto, your rules deciding, and a limit that fits
/// the plan. The example's scope question, which `understand.always_ask` keeps yours in auto, is
/// the main agent's to answer, and the run goes from request to merge without a question.
#[test]
fn crew_autopilot_runs_to_merge_without_a_question() -> anyhow::Result<()> {
    let scenario = crew_scenario()?;
    let mut config = CREW_CONFIG.replace(
        "mode = \"supervised\"",
        "mode = \"autopilot\"\ndecide_with = \"rules\"",
    );
    for point in ["workflow_promotion", "stuck", "limit", "change_request"] {
        config.push_str(&format!("\n[approvals.{point}]\nmode = \"autopilot\"\n"));
    }
    // The run's limit goes in the table the hard limits already opened.
    let config = config.replace("[limits]\n", "[limits]\nspend_per_run = 100.0\n");
    fs::write(scenario.repo.global_config(), as_set_up(&config))?;
    let pty = run(&scenario)?;
    let outcome = wait_for_event(&scenario, STEPS_WAIT * 2, finished)?;
    assert_eq!(outcome, RunOutcome::Merged);
    let events = scenario.latest_run_events()?;

    // Nothing waited for you: every approval was decided by your rules, none by you.
    let by: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => Some(decided.by.clone()),
            _ => None,
        })
        .collect();
    assert!(!by.is_empty());
    assert!(by.iter().all(|by| *by == Party::Rules), "{by:?}");
    let asked = events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Approval(ApprovalEvent::Checked(check))
            if !check.auto_preview && check.verdict.is_none())
    });
    assert!(!asked, "an approval asked you");

    // The scope question is the main agent's assumption, which the goal carries for you to review.
    let goal = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Goal(GoalEvent::Approved { file, .. }) => Some(file.clone()),
            _ => None,
        })
        .context("the goal was approved")?;
    assert!(
        goal.assumptions.iter().any(
            |assumption| assumption.by_main_agent() && assumption.text.ends_with("Per API key")
        ),
        "{:?}",
        goal.assumptions
    );
    let screen = pty.screen().text();
    assert!(!screen.contains("Auto couldn't decide"), "{screen}");
    scenario.shot(&pty, "crew-autopilot-merged")?;
    Ok(())
}
