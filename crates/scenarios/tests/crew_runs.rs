//! Crew runs through the real binary with fake vendors: WORKFLOW 12's rate-limiting example end
//! to end, its goal question card, and your answers at the goal, plan, and merge checkpoints.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};

use argonauts_core::CardId;
use argonauts_core::events::card::{CardEvent, CardState, IntegrationEvent};
use argonauts_core::events::checkpoint::{AnswerAction, CheckpointEvent, CheckpointKind};
use argonauts_core::events::goal::GoalEvent;
use argonauts_core::events::plan::{DivideEvent, PlanEvent};
use argonauts_core::events::run::{ParkReason, RunLifecycle, RunOutcome};
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::worker::Access;
use argonauts_scenarios::{Scenario, WAIT, scenario_file};
use argonauts_testkit::{FakeScenario, Key, Pty};

const REQUEST: &str = "add per-key rate limiting to the public API";

/// A crew run takes many steps; each wait covers the steps up to the next checkpoint.
const STEPS_WAIT: Duration = Duration::from_secs(180);

/// A global config means no guided setup; it names the example's crew. The goal, plan, and
/// merge checkpoints wait for you, since these runs are about your answers there.
const CREW_CONFIG: &str = r#"default_agent = "claude-code"
crew = ["claude-code", "codex", "antigravity-cli"]

[ui]
theme = "dark"

[approvals.goal]
mode = "supervised"

[approvals.plan]
mode = "supervised"

[approvals.merge]
mode = "supervised"

[agents.codex]
model = "gpt-5.6-luna"

[limits]
dependency_allowlist = ["ioredis"]

[[gates.run]]
name = "acceptance"
command = ["sh", "scripts/acceptance.sh"]
"#;

/// Stands in for npm, offline: the conductor's install of the plan's new dependency adds it to
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
    fs::write(config, CREW_CONFIG)?;
    scenario.repo.write(
        "package.json",
        "{ \"name\": \"acme-api\", \"scripts\": { \"test\": \"vitest run\" } }\n",
    )?;
    scenario.repo.write("src/api/server.ts", ENTRY)?;
    scenario.repo.write("src/api/auth.ts", AUTH)?;
    scenario.repo.write("scripts/acceptance.sh", ACCEPTANCE)?;
    scenario.repo.commit_all("the public API")?;
    let npm = scenario.vendors.bin_dir().join("npm");
    fs::write(&npm, NPM)?;
    fs::set_permissions(&npm, fs::Permissions::from_mode(0o755))?;
    Ok(scenario)
}

/// `argonauts run --crew` with the request: the same engine and cards as the app, for scripts.
/// Without a chat there's no estimate for triage to read, so the example's crew is asked for.
fn run(scenario: &Scenario) -> anyhow::Result<Pty> {
    scenario.launch(&["run", "--crew", REQUEST])
}

/// Waits until the run's log holds an event that `found` picks, and returns it.
fn wait_for_event<T>(
    scenario: &Scenario,
    timeout: Duration,
    found: impl Fn(&RunEvent) -> Option<T>,
) -> anyhow::Result<T> {
    let deadline = Instant::now() + timeout;
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

fn opened(event: &RunEvent, kind: CheckpointKind) -> Option<u64> {
    match &event.kind {
        RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) if opened.kind == kind => {
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
    pty.press(Key::Up)?;
    pty.press(Key::Char('1'))?;
    pty.wait_for(r"◆ Goal question answered · (?i:per API key)", WAIT)?;
    Ok(())
}

fn approve_plan(pty: &mut Pty) -> anyhow::Result<()> {
    pty.wait_for(r"◆ Approve this plan\?", STEPS_WAIT)?;
    pty.press(Key::Up)?;
    pty.press(Key::Char('1'))?;
    Ok(())
}

/// Merge waits for Enter after its number.
fn merge(pty: &mut Pty) -> anyhow::Result<()> {
    pty.wait_for(r"◆ Merge", STEPS_WAIT)?;
    pty.press(Key::Up)?;
    pty.press(Key::Char('1'))?;
    pty.wait_for("Enter to merge", WAIT)?;
    pty.press(Key::Enter)?;
    Ok(())
}

/// When the first checkpoint of this kind was decided, and how.
fn decided(events: &[RunEvent], kind: CheckpointKind) -> Option<(u64, AnswerAction)> {
    let ids: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) if opened.kind == kind => {
                Some(opened.id.clone())
            }
            _ => None,
        })
        .collect();
    events.iter().find_map(|event| match &event.kind {
        RunEventKind::Checkpoint(CheckpointEvent::Decided(decided))
            if ids.contains(&decided.id) =>
        {
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

    // The goal: three restatements, two differs, one scope question you answered, and the
    // Retry-After detail adopted as an assumption.
    let restated = events
        .iter()
        .filter(|event| matches!(event.kind, RunEventKind::Goal(GoalEvent::Restated { .. })))
        .count();
    assert_eq!(restated, 3);
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

    // Planning: blind drafts, a map, a critique round, revisions, a spike per option, and a
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
        RunEventKind::Plan(PlanEvent::SpikeProposed { .. })
    )));
    let spikes = events
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                RunEventKind::Plan(PlanEvent::SpikeFinished { .. })
            )
        })
        .count();
    assert_eq!(spikes, 2);
    let winner = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Plan(PlanEvent::Judged(result)) => result.winner,
        _ => None,
    });
    assert_eq!(winner, Some(2));

    // Nothing builds before you approve the goal and the plan.
    let plan_decided = decided(&events, CheckpointKind::Plan)
        .filter(|(_, action)| *action == AnswerAction::Approve)
        .map(|(seq, _)| seq)
        .context("you approved the plan")?;
    let first_build = seq_of(&events, |event| {
        matches!(&event.kind, RunEventKind::Session(SessionEvent::Started(started))
            if started.access == Access::Build)
    })
    .context("cards were built")?;
    assert!(first_build > plan_decided);

    // Five cards from the plan, each merged; ioredis added by the conductor; the final check
    // passed; the run landed on main.
    let graph = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Divide(DivideEvent::Graph(graph)) => Some(graph.clone()),
            _ => None,
        })
        .context("the task graph")?;
    let built: BTreeSet<String> = graph
        .cards
        .iter()
        .flat_map(|card| card.owned_paths.clone())
        .collect();
    for path in [
        "src/limiter/index.ts",
        "src/limiter/store.ts",
        "src/api/rate-limit.ts",
        "docs/rate-limits.md",
    ] {
        assert!(built.contains(path), "{path} is some card's");
    }
    let merged: BTreeSet<CardId> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Integration(IntegrationEvent::Merged { card, .. }) => Some(card.clone()),
            _ => None,
        })
        .collect();
    assert!(merged.len() >= 5, "{merged:?}");
    assert!(events.iter().any(|event| matches!(&event.kind,
        RunEventKind::Integration(IntegrationEvent::DependenciesAdded { packages, .. })
            if packages.iter().any(|package| package == "ioredis"))));
    assert!(events.iter().any(|event| matches!(
        event.kind,
        RunEventKind::Integration(IntegrationEvent::FinalCheck { passed: true })
    )));
    assert!(events.iter().any(|event| matches!(
        event.kind,
        RunEventKind::Integration(IntegrationEvent::Landed { .. })
    )));
    let store = scenario.repo.git(&["show", "main:src/limiter/store.ts"])?;
    assert!(store.contains("RedisStore"));

    // A crew of three vendors did it.
    let called: BTreeSet<String> = scenario
        .vendors
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
fn crew_example_question_card() -> anyhow::Result<()> {
    let scenario = crew_scenario()?;
    let mut pty = scenario.launch(&[])?;
    pty.wait_for(r"(?m)^  ǁ supervised mode on|⏵⏵ supervised", WAIT)?;
    pty.send(REQUEST)?;
    pty.press(Key::Enter)?;

    // The model-independent lines for a bigger task: three
    // restaters and two differs, the detail adopted as an assumption, then the question.
    pty.wait_for(r"(?m)^[●⏺] Triage\(crew\)", STEPS_WAIT)?;
    pty.wait_for(
        r"(?m)^[●⏺] 3 agents started.*\n(?:\s+[├└] read:\S+ · .*\n){3}",
        STEPS_WAIT,
    )?;
    pty.wait_for(
        r"(?m)^[●⏺] 2 agents started.*\n(?:\s+[├└] compare:\S+ · .*\n){2}",
        STEPS_WAIT,
    )?;
    pty.wait_for(r"Assumed\W+Retry-After format: seconds", WAIT)?;
    let card = pty.wait_for(r"(?m)^◆ What should the goal say\?", STEPS_WAIT)?;
    let text = card.visible_text();
    assert!(text.contains("per API key"), "{text}");
    assert!(text.contains("Per API key (recommended)"), "{text}");
    assert!(
        text.contains("2 of 3 restatements read it this way"),
        "{text}"
    );
    // Choose-only: the last answer hands this and the rest to the lead.
    assert!(
        text.contains("Let the lead decide this and the rest"),
        "{text}"
    );
    assert!(!text.contains("Type my own answer"), "{text}");
    scenario.shot(&pty, "crew-question-card")?;

    // No agent works while the card waits, so the run parks until you answer.
    let parked = wait_for_event(&scenario, WAIT, |event| match &event.kind {
        RunEventKind::Run(RunLifecycle::Parked {
            reason: ParkReason::Checkpoint,
            waiting_on,
        }) => Some(waiting_on.len()),
        _ => None,
    })?;
    assert_eq!(parked, 1);

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

    // Your answer to the last question approved the goal: no second card asked for it.
    let goal_cards = events
        .iter()
        .filter(|event| opened(event, CheckpointKind::Goal).is_some())
        .count();
    assert_eq!(goal_cards, 1);
    let (file, by) = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Goal(GoalEvent::Approved { file, by }) => {
                Some((file.clone(), by.clone()))
            }
            _ => None,
        })
        .context("the goal was approved")?;
    assert_eq!(by, argonauts_core::events::run::Party::User);
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
    let first = wait_for_event(&scenario, WAIT, |event| opened(event, CheckpointKind::Plan))?;
    pty.press(Key::Up)?;
    pty.press(Key::Down)?;
    pty.press(Key::Down)?;
    pty.press(Key::Tab)?;
    pty.send(note)?;
    pty.press(Key::Enter)?;

    let sent_back = wait_for_event(&scenario, WAIT, |event| match &event.kind {
        RunEventKind::Checkpoint(CheckpointEvent::Decided(decided))
            if decided.action == AnswerAction::SendBack =>
        {
            Some((event.seq, decided.text.clone()))
        }
        _ => None,
    })?;
    assert_eq!(sent_back.1.as_deref(), Some(note));

    // Planning runs again, with your note in the new drafts' prompts, and asks you again.
    let second = wait_for_event(&scenario, STEPS_WAIT, |event| {
        opened(event, CheckpointKind::Plan).filter(|seq| *seq > first)
    })?;
    let events = scenario.latest_run_events()?;
    let redrafted = events
        .iter()
        .filter(|event| event.seq > sent_back.0 && event.seq < second)
        .filter(|event| matches!(event.kind, RunEventKind::Plan(PlanEvent::Sealed { .. })))
        .count();
    assert!(redrafted >= 3, "every drafter drafts again");
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

    // Request changes on some cards: the merge rows become a checklist. Pick the second card
    // only.
    pty.wait_for(r"◆ Merge", STEPS_WAIT)?;
    let first = wait_for_event(&scenario, WAIT, |event| {
        opened(event, CheckpointKind::Merge)
    })?;
    pty.press(Key::Up)?;
    // Request changes on some cards, the third option.
    pty.press(Key::Char('3'))?;
    pty.press(Key::Down)?;
    pty.press(Key::Char(' '))?;
    // Enter opens the note on the ticked cards, and Enter again sends them without one.
    pty.press(Key::Enter)?;
    pty.wait_for("Enter to send · Esc to go back", WAIT)?;
    pty.press(Key::Enter)?;

    let (decided_at, picked) = wait_for_event(&scenario, WAIT, |event| match &event.kind {
        RunEventKind::Checkpoint(CheckpointEvent::Decided(decided))
            if decided.action == AnswerAction::RequestChanges =>
        {
            Some((event.seq, decided.cards.clone()))
        }
        _ => None,
    })?;
    assert_eq!(picked.len(), 1, "one card picked: {picked:?}");
    let returned = picked[0].clone();

    // Only the picked card goes back to its owner; the others stay as they were.
    let again = wait_for_event(&scenario, STEPS_WAIT, |event| {
        opened(event, CheckpointKind::Merge).filter(|seq| *seq > first)
    })?;
    let events = scenario.latest_run_events()?;
    let between = || {
        events
            .iter()
            .filter(|event| event.seq > decided_at && event.seq < again)
    };
    let moved: BTreeSet<CardId> = between()
        .filter_map(|event| match &event.kind {
            RunEventKind::Card(CardEvent::State { .. }) => event.card_id.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(moved, BTreeSet::from([returned.clone()]));
    let rebuilt: BTreeSet<CardId> = between()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started))
                if started.access == Access::Build =>
            {
                event.card_id.clone()
            }
            _ => None,
        })
        .collect();
    assert_eq!(rebuilt, BTreeSet::from([returned.clone()]));
    assert!(
        between().any(|event| event.card_id.as_ref() == Some(&returned)
            && matches!(
                &event.kind,
                RunEventKind::Card(CardEvent::State {
                    to: CardState::Integrated,
                    ..
                })
            ))
    );
    scenario.shot(&pty, "crew-merge-request-changes")?;
    Ok(())
}
