//! Scripts in the format Claude Code writes its own, run against a host that answers from a
//! table.

mod support;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU32, Ordering};

use argonauts_core::events::session::{CliAgentState, SubagentStatus};
use argonauts_core::settings::crew::CrewRole;
use argonauts_runner::{AgentEnd, Limits, RunInput, Script, ScriptEnd, run};
use serde_json::{Value, json};
use support::{Fake, accept, reject, said, text};

fn script(source: &str) -> Script {
    Script::parse(source).unwrap_or_else(|err| panic!("{err}"))
}

fn input(args: Value) -> RunInput {
    RunInput {
        id: "wf-1".to_owned(),
        args,
        ..RunInput::default()
    }
}

const UNTIL_ACCEPTED: &str = r#"
export const meta = {
  name: 'review-until-clean',
  description: 'Build, then review until the reviewer accepts',
  phases: [{ name: 'Build', detail: 'one worker' }, { name: 'Review' }],
}
const VERDICT = {
  type: 'object',
  properties: { accept: { type: 'boolean' }, findings: { type: 'array', items: { type: 'string' } } },
  required: ['accept'],
}
let findings = []
let rounds = 0
while (true) {
  rounds++
  await agent(`Build: ${args.request}\n${findings.join('\n')}`, { role: 'worker', phase: 'Build', label: 'build', tryNumber: rounds })
  const verdict = await agent('Review the change.', { role: 'reviewer', phase: 'Review', label: 'review', schema: VERDICT, tryNumber: rounds })
  if (verdict && verdict.accept) break
  findings = verdict ? verdict.findings : []
  log(`round ${rounds}: sent back`)
}
return { rounds }
"#;

#[tokio::test]
async fn a_script_loops_until_a_review_accepts() {
    let reviews = AtomicU32::new(0);
    let host = Fake::new(move |call| match call.role {
        CrewRole::Reviewer => match reviews.fetch_add(1, Ordering::Relaxed) {
            0 => reject(&["handle the empty list"]),
            1 => reject(&["name the helper"]),
            _ => accept(),
        },
        _ => text("built"),
    });
    let ended = run(
        &script(UNTIL_ACCEPTED),
        input(json!({"request": "median()"})),
        &host,
    )
    .await;

    assert_eq!(ended.result, Ok(json!({"rounds": 3})));
    assert_eq!(
        host.asked(),
        [
            "build worker 1",
            "review reviewer 1",
            "build worker 2",
            "review reviewer 2",
            "build worker 3",
            "review reviewer 3",
        ]
    );
    assert!(
        host.call("build", 2)
            .prompt
            .ends_with("handle the empty list")
    );
    assert!(host.call("build", 3).prompt.ends_with("name the helper"));
    assert_eq!(
        host.logs.lock().unwrap().as_slice(),
        ["round 1: sent back", "round 2: sent back"]
    );
    let report = ended.report;
    assert_eq!(report.name, "review-until-clean");
    assert_eq!(
        report.description,
        "Build, then review until the reviewer accepts"
    );
    assert_eq!(report.phases[0].detail, "one worker");
    assert_eq!(report.count(1), Some((3, 3)));
    assert_eq!(report.count(2), Some((3, 3)));
    assert_eq!(report.agents[3].try_number, 2);
    assert_eq!(report.summary.as_deref(), Some("round 2: sent back"));
    assert_eq!(report.status, SubagentStatus::Done);
}

#[tokio::test]
async fn parallel_is_a_barrier_pipeline_is_not_and_a_failed_agent_is_null() {
    let host = Fake::new(|call| {
        if call.prompt == "find b" {
            return AgentEnd::failed("its CLI stopped");
        }
        text(&call.prompt.to_uppercase())
    });
    let source = r#"
export const meta = { name: 'sweep', description: 'Sweep', phases: [{ name: 'Find' }, { name: 'Fix' }] }
phase('Find')
const found = await parallel(['a', 'b', 'c'].map((name) => () => agent('find ' + name)))
const thrown = await parallel([() => { throw new Error('no') }, () => agent('find d')])
const fixed = await pipeline(
  found.filter(Boolean),
  (item) => agent('fix ' + item, { phase: 'Fix' }),
  (result, item, index) => `${index}:${item}:${result}`,
)
const dropped = await pipeline([1], () => { throw new Error('stage failed') })
return { found, thrown, fixed, dropped }
"#;
    let ended = run(&script(source), input(Value::Null), &host).await;
    assert_eq!(
        ended.result,
        Ok(json!({
            "found": ["FIND A", null, "FIND C"],
            "thrown": [null, "FIND D"],
            "fixed": ["0:FIND A:FIX FIND A", "1:FIND C:FIX FIND C"],
            "dropped": [null],
        }))
    );
    let report = ended.report;
    let failed: Vec<&str> = report
        .agents
        .iter()
        .filter(|agent| agent.state == CliAgentState::Failed)
        .map(|agent| agent.label.as_str())
        .collect();
    assert_eq!(failed, ["find:2"]);
    assert_eq!(report.count(1), Some((3, 4)));
    assert_eq!(report.count(2), Some((2, 2)));
    // The workflow still ended well: the script chose to go on without the failed one.
    assert_eq!(report.status, SubagentStatus::Done);
}

#[tokio::test]
async fn a_phase_the_script_never_declared_gets_a_group_of_its_own_at_the_end() {
    let host = Fake::new(|_| text("ok"));
    let source = "export const meta = { name: 'a', description: 'b', phases: [{ name: 'Second' }] }\n\
                  phase('Extra')\nawait agent('one')\nawait agent('two', { phase: 'Second' })\n";
    let ended = run(&script(source), input(Value::Null), &host).await;
    let titles: Vec<&str> = ended
        .report
        .phases
        .iter()
        .map(|p| p.title.as_str())
        .collect();
    assert_eq!(titles, ["Second", "Extra"]);
    assert_eq!(ended.report.agents[0].phase, Some(2));
    assert_eq!(ended.report.agents[1].phase, Some(1));
    assert_eq!(ended.result, Ok(Value::Null));
}

#[tokio::test]
async fn a_run_that_stopped_picks_up_from_its_saved_calls() {
    let source = r#"
export const meta = { name: 'three', description: 'Three in a row' }
const a = await agent('first')
const b = await agent('second after ' + a)
const c = await agent(args.last + ' after ' + b)
return [a, b, c]
"#;
    let first = Fake::new(|call| said(json!(call.prompt.to_uppercase())));
    let ended = run(&script(source), input(json!({"last": "third"})), &first).await;
    assert_eq!(ended.agents_run, 3);
    let saved = first.journal.lock().unwrap().clone();
    assert_eq!(saved.len(), 3);
    assert_eq!((saved[1].index, saved[1].label.as_str()), (1, "worker:2"));
    assert_eq!(saved[1].tokens, 1_000);

    // The same script and args: every answer comes from the journal, and no agent runs.
    let again = Fake::new(|_| panic!("nothing should run"));
    let replayed = RunInput {
        replay: saved.clone(),
        ..input(json!({"last": "third"}))
    };
    let same = run(&script(source), replayed, &again).await;
    assert_eq!(same.result, ended.result);
    assert_eq!(same.agents_run, 0);
    assert_eq!(same.report.agents[2].state, CliAgentState::Done);
    assert_eq!(same.report.tokens, 3_000);
    assert!(again.journal.lock().unwrap().is_empty());
    // The host hears which row each saved call took, since no agent() comes for them.
    assert_eq!(*again.replayed.lock().unwrap(), [(1, 1), (2, 2), (3, 3)]);

    // A changed third call runs live, and the two before it are still reused.
    let changed = Fake::new(|call| said(json!(call.prompt.to_uppercase())));
    let replayed = RunInput {
        replay: saved,
        ..input(json!({"last": "fourth"}))
    };
    let partly = run(&script(source), replayed, &changed).await;
    assert_eq!(partly.agents_run, 1);
    assert_eq!(changed.asked(), ["worker:3 worker 1"]);
    assert_eq!(changed.journal.lock().unwrap()[0].index, 2);
    assert_eq!(
        partly.result.expect("ended")[2],
        json!("FOURTH AFTER SECOND AFTER FIRST")
    );
}

#[tokio::test]
async fn saved_answers_go_back_in_the_order_the_calls_first_finished() {
    use std::sync::Mutex;

    /// An agent on `b` answers at once and one on `a` takes a while, so `b` gets ahead.
    struct Uneven(Mutex<Vec<argonauts_runner::JournalEntry>>);
    impl argonauts_runner::Host for Uneven {
        fn agent<'a>(
            &'a self,
            call: &'a argonauts_runner::AgentCall,
        ) -> futures::future::BoxFuture<'a, AgentEnd> {
            Box::pin(async move {
                if call.prompt.ends_with(" a") {
                    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                }
                AgentEnd::answered(json!(call.prompt.to_uppercase()))
            })
        }
        fn check<'a>(
            &'a self,
            _: &'a argonauts_runner::Check,
        ) -> futures::future::BoxFuture<'a, argonauts_runner::Checked> {
            Box::pin(async { argonauts_runner::Checked::default() })
        }
        fn journal(&self, entry: &argonauts_runner::JournalEntry) {
            self.0.lock().unwrap().push(entry.clone());
        }
    }
    let source = r#"
export const meta = { name: 'two-stages', description: 'Two stages for each of two items' }
return await pipeline(['a', 'b'], (item) => agent('one ' + item), (out, item) => agent('two ' + item))
"#;
    let first = Uneven(Mutex::default());
    let ended = run(&script(source), input(Value::Null), &first).await;
    assert_eq!(ended.result, Ok(json!(["TWO A", "TWO B"])));
    let saved = first.0.into_inner().unwrap();
    // `b` finished its first stage first, so its second stage was the third call asked.
    let order: Vec<(u32, &str)> = saved
        .iter()
        .map(|entry| {
            (
                entry.index,
                entry.result.as_ref().and_then(Value::as_str).unwrap(),
            )
        })
        .collect();
    assert_eq!(
        order,
        [(1, "ONE B"), (2, "TWO B"), (0, "ONE A"), (3, "TWO A")]
    );

    // Nothing runs again: the answers go back in that order, so the script asks the same calls.
    let again = Fake::new(|call| panic!("{} ran again", call.prompt));
    let replayed = RunInput {
        replay: saved,
        ..input(Value::Null)
    };
    let same = run(&script(source), replayed, &again).await;
    assert_eq!(same.result, Ok(json!(["TWO A", "TWO B"])));
    assert_eq!(same.agents_run, 0);
}

#[tokio::test]
async fn a_failed_agent_runs_again_on_resume_with_everything_after_it() {
    let source = "export const meta = { name: 'two', description: 'Two' }\n\
                  const a = await agent('first')\nconst b = await agent('second')\nreturn [a, b]\n";
    let first = Fake::new(|call| match call.prompt.as_str() {
        "first" => AgentEnd::failed("out of plan usage"),
        _ => text("two"),
    });
    run(&script(source), input(Value::Null), &first).await;
    let saved = first.journal.lock().unwrap().clone();
    assert!(saved[0].result.is_none());

    let again = Fake::new(|_| text("now"));
    let replayed = RunInput {
        replay: saved,
        ..input(Value::Null)
    };
    let ended = run(&script(source), replayed, &again).await;
    assert_eq!(ended.agents_run, 2);
    assert_eq!(ended.result, Ok(json!(["now", "now"])));
}

#[tokio::test]
async fn a_script_written_for_a_workflow_file_keeps_to_the_files_roles() {
    let host = Fake::new(|_| text("ok"));
    let source = "export const meta = { name: 'a', description: 'b' }\n\
                  await agent('build', { role: 'worker' })\n\
                  try { await agent('sign off', { role: 'judge' }) } catch (error) { return String(error.message) }\n";
    let limited = RunInput {
        roles: Some(BTreeSet::from([CrewRole::Worker, CrewRole::Reviewer])),
        ..input(Value::Null)
    };
    let ended = run(&script(source), limited, &host).await;
    assert_eq!(
        ended.result,
        Ok(json!(
            "agent(): this workflow's file has no judge step; its roles are worker, reviewer"
        ))
    );
    assert_eq!(host.asked(), ["worker:1 worker 1"]);
}

#[tokio::test]
async fn a_script_the_orchestrator_wrote_never_picks_a_model_or_a_nested_workflow() {
    let host = Fake::new(|_| text("ok"));
    let source = "export const meta = { name: 'a', description: 'b' }\n\
                  const said = []\n\
                  for (const opts of [{ model: 'claude-code/opus' }, { effort: 'max' }, { ownWorkflow: true }]) {\n\
                    try { await agent('build', { role: 'worker', ...opts }) } catch (error) { said.push(error.message) }\n\
                  }\n\
                  return said\n";
    let written = RunInput {
        roles: Some(BTreeSet::from([CrewRole::Worker])),
        ..input(Value::Null)
    };
    let ended = run(&script(source), written, &host).await;
    assert_eq!(
        ended.result,
        Ok(json!([
            "agent(): the settings pick the model; leave model out",
            "agent(): the settings pick the effort; leave effort out",
            "agent(): the settings pick the ownWorkflow; leave ownWorkflow out",
        ]))
    );
    assert!(host.asked().is_empty());
    // Your own script may pick them.
    let yours = run(&script(source), input(Value::Null), &host).await;
    assert_eq!(yours.result, Ok(json!([])));
}

#[tokio::test]
async fn a_script_the_orchestrator_wrote_keeps_to_the_tasks_you_fixed() {
    let host = Fake::new(|_| text("ok"));
    let source = "export const meta = { name: 'a', description: 'b' }\n\
                  await agent('build', { role: 'worker', task: 'switch-reads' })\n\
                  try { await agent('split', { role: 'worker', task: 'backfill-rows' }) } catch (error) { return String(error.message) }\n";
    let fixed = RunInput {
        args: json!({"request": "r", "tasks": ["switch reads to the new table"]}),
        roles: Some(BTreeSet::from([CrewRole::Worker])),
        ..input(Value::Null)
    };
    let ended = run(&script(source), fixed, &host).await;
    assert_eq!(
        ended.result,
        Ok(json!(
            "agent(): the user fixed the task list; ask instead of adding one"
        ))
    );
    assert_eq!(host.asked().len(), 1);
}

#[tokio::test]
async fn the_caps_on_agents_and_items_are_errors_the_script_sees() {
    let host = Fake::new(|_| text("ok"));
    let source = r#"
export const meta = { name: 'many', description: 'Too many' }
const said = []
for (let n = 0; n < 4; n++) {
  try { await agent('go ' + n) } catch (error) { said.push(error.message) }
}
try { await parallel(new Array(6).fill(() => 1)) } catch (error) { said.push(error.message) }
return said
"#;
    let capped = RunInput {
        limits: Limits {
            agents: 3,
            items: 5,
            ..Limits::default()
        },
        ..input(Value::Null)
    };
    let ended = run(&script(source), capped, &host).await;
    assert_eq!(
        ended.result,
        Ok(json!([
            "this workflow already started 3 agents, the most one run may",
            "parallel(tasks) takes at most 5 items, and got 6",
        ]))
    );
    assert_eq!(ended.agents_run, 3);
}

#[tokio::test]
async fn a_script_that_keeps_asking_past_the_cap_is_stopped() {
    let host = Fake::new(|_| text("ok"));
    let source = "export const meta = { name: 'greedy', description: 'Never stops asking' }\n\
                  while (true) { try { await agent('more') } catch (_) { log('refused') } }\n";
    let capped = RunInput {
        limits: Limits {
            agents: 2,
            ..Limits::default()
        },
        ..input(Value::Null)
    };
    let ended = run(&script(source), capped, &host).await;
    let Err(ScriptEnd::Threw(said)) = &ended.result else {
        panic!("{:?}", ended.result);
    };
    assert!(
        said.starts_with("the script kept asking for agents it couldn't have"),
        "{said}"
    );
    assert_eq!(ended.agents_run, 2);
    assert_eq!(
        host.logs.lock().unwrap().len(),
        1_000,
        "one line for each refusal it saw"
    );
}

#[tokio::test]
async fn only_so_many_agents_work_at_once() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    struct Slow {
        working: AtomicUsize,
        most: AtomicUsize,
    }
    impl argonauts_runner::Host for Slow {
        fn agent<'a>(
            &'a self,
            _: &'a argonauts_runner::AgentCall,
        ) -> futures::future::BoxFuture<'a, AgentEnd> {
            Box::pin(async move {
                let now = self.working.fetch_add(1, Ordering::SeqCst) + 1;
                self.most.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                self.working.fetch_sub(1, Ordering::SeqCst);
                AgentEnd::answered(json!("ok"))
            })
        }
        fn check<'a>(
            &'a self,
            _: &'a argonauts_runner::Check,
        ) -> futures::future::BoxFuture<'a, argonauts_runner::Checked> {
            Box::pin(async { argonauts_runner::Checked::default() })
        }
    }
    let host = Arc::new(Slow {
        working: AtomicUsize::new(0),
        most: AtomicUsize::new(0),
    });
    let source = "export const meta = { name: 'wide', description: 'Wide' }\n\
                  return (await parallel(new Array(9).fill(() => agent('go')))).length\n";
    let narrow = RunInput {
        limits: Limits {
            at_once: 3,
            ..Limits::default()
        },
        ..input(Value::Null)
    };
    let ended = run(&script(source), narrow, host.as_ref()).await;
    assert_eq!(ended.result, Ok(json!(9)));
    assert_eq!(host.most.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_token_target_is_a_hard_ceiling() {
    let host = Fake::new(|_| said(json!("ok")));
    let source = r#"
export const meta = { name: 'deep', description: 'As deep as the tokens allow' }
let done = 0
while (limit.total && limit.remaining() > 0) {
  await agent('dig')
  done++
}
let refused = null
try { await agent('one more') } catch (error) { refused = error.message }
return { done, total: limit.total, left: limit.remaining(), refused }
"#;
    let targeted = RunInput {
        token_limit: Some(2_500),
        ..input(Value::Null)
    };
    let ended = run(&script(source), targeted, &host).await;
    assert_eq!(
        ended.result,
        Ok(json!({
            "done": 3,
            "total": 2500,
            "left": 0,
            "refused": "the run's 2500 tokens are spent, so no more agents start",
        }))
    );
}

#[tokio::test]
async fn a_workflow_runs_another_as_one_step() {
    let mut host = Fake::new(|call| text(&call.prompt));
    let child = "export const meta = { name: 'lint', description: 'Lint', phases: [{ name: 'Lint' }] }\n\
                 phase('Lint')\nconst out = await agent('lint ' + args.path)\nreturn out + '!'\n";
    host.workflows.insert("lint".to_owned(), script(child));
    let source = r#"
export const meta = { name: 'ship', description: 'Ship', phases: [{ name: 'Build' }] }
await agent('build', { phase: 'Build' })
const linted = await workflow('lint', { path: 'src' })
let missing = null
try { await workflow('nope') } catch (error) { missing = error.message }
return { linted, missing }
"#;
    let ended = run(&script(source), input(Value::Null), &host).await;
    assert_eq!(
        ended.result,
        Ok(json!({"linted": "lint src!", "missing": "there's no workflow named nope"}))
    );
    let report = ended.report;
    let titles: Vec<&str> = report.phases.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(titles, ["Build", "▸ lint"]);
    assert_eq!(report.agents[1].phase, Some(2));
    assert_eq!(report.name, "ship");
}

/// Two workflows run as steps each start counting their calls at one. A host keeps sessions
/// and finished work by a call's number, so no two calls of a run may share one.
#[tokio::test]
async fn calls_of_workflows_run_as_steps_are_numbered_apart() {
    let mut host = Fake::new(|call| text(&call.prompt));
    let child = |name: &str| {
        script(&format!(
            "export const meta = {{ name: '{name}', description: 'One agent' }}\n\
             return await agent('{name} ' + args.path)\n"
        ))
    };
    host.workflows.insert("lint".to_owned(), child("lint"));
    host.workflows.insert("docs".to_owned(), child("docs"));
    let source = r#"
export const meta = { name: 'ship', description: 'Ship' }
await agent('build')
const [linted, written] = await parallel([
  () => workflow('lint', { path: 'src' }),
  () => workflow('docs', { path: 'README.md' }),
])
return [linted, written]
"#;
    let ended = run(&script(source), input(Value::Null), &host).await;
    assert_eq!(ended.result, Ok(json!(["lint src", "docs README.md"])));
    let mut calls: Vec<(String, u32, u32)> = host
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| (call.prompt.clone(), call.index, call.row))
        .collect();
    calls.sort();
    let numbers: Vec<(&str, u32)> = calls
        .iter()
        .map(|(prompt, index, _)| (prompt.as_str(), *index))
        .collect();
    assert_eq!(
        numbers,
        [
            ("build", 1),
            ("docs README.md", 2 * 65_536 + 1),
            ("lint src", 65_536 + 1),
        ]
    );
    // Each call is told its row in the report, whichever of the two queued first.
    for (prompt, _, row) in &calls {
        let agent = &ended.report.agents[*row as usize - 1];
        assert_eq!(agent.index, *row, "{prompt}");
        let group = ended
            .report
            .phases
            .iter()
            .find(|phase| Some(phase.index) == agent.phase)
            .map(|phase| phase.title.as_str());
        match prompt.as_str() {
            "build" => assert_eq!(group, None),
            "lint src" => assert_eq!(group, Some("▸ lint")),
            _ => assert_eq!(group, Some("▸ docs")),
        }
    }
}

#[tokio::test]
async fn a_workflow_run_as_a_step_answers_from_what_it_saved_too() {
    let child = "export const meta = { name: 'lint', description: 'Lint, then fix' }\n\
                 const found = await agent('lint ' + args.path)\n\
                 return await agent('fix ' + found)\n";
    let source = r#"
export const meta = { name: 'ship', description: 'Ship' }
const built = await agent('build')
const linted = await workflow('lint', { path: built })
return await agent('sign off ' + linted)
"#;
    let mut first = Fake::new(|call| said(json!(call.prompt.to_uppercase())));
    first.workflows.insert("lint".to_owned(), script(child));
    let ended = run(&script(source), input(Value::Null), &first).await;
    assert_eq!(ended.result, Ok(json!("SIGN OFF FIX LINT BUILD")));
    let saved = first.journal.lock().unwrap().clone();
    let numbers: Vec<u32> = saved.iter().map(|entry| entry.index).collect();
    assert_eq!(numbers, [0, 65_536, 65_537, 1]);

    // Picked up again, the step's own calls answer from what was saved, and its rows are back.
    let mut again = Fake::new(|_| panic!("nothing should run"));
    again.workflows.insert("lint".to_owned(), script(child));
    let replayed = RunInput {
        replay: saved.clone(),
        ..input(Value::Null)
    };
    let same = run(&script(source), replayed, &again).await;
    assert_eq!(same.result, ended.result);
    assert_eq!(same.agents_run, 0);
    assert_eq!(same.report.agents.len(), 4);
    assert_eq!(same.report.tokens, 4_000);

    // Stopped inside the step: its first call is saved, and only what came after runs.
    let mut resumed = Fake::new(|call| said(json!(call.prompt.to_uppercase())));
    resumed.workflows.insert("lint".to_owned(), script(child));
    let partly = RunInput {
        replay: saved[..2].to_vec(),
        ..input(Value::Null)
    };
    let finished = run(&script(source), partly, &resumed).await;
    assert_eq!(finished.result, ended.result);
    assert_eq!(finished.agents_run, 2);
    let ran: Vec<String> = resumed
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| call.prompt.clone())
        .collect();
    assert_eq!(ran, ["fix LINT BUILD", "sign off FIX LINT BUILD"]);
}

#[tokio::test]
async fn a_run_that_keeps_workflows_from_nesting_refuses_the_call() {
    let mut host = Fake::new(|call| text(&call.prompt));
    host.workflows.insert(
        "lint".to_owned(),
        script("export const meta = { name: 'lint', description: 'Lint' }\nreturn 1\n"),
    );
    let source = r#"
export const meta = { name: 'ship', description: 'Ship' }
try { return await workflow('lint') } catch (error) { return error.message }
"#;
    let flat = RunInput {
        limits: Limits {
            nested: false,
            ..Limits::default()
        },
        ..input(Value::Null)
    };
    let ended = run(&script(source), flat, &host).await;
    assert_eq!(
        ended.result,
        Ok(json!(
            "this run doesn't let a workflow run another as a step"
        ))
    );
}

#[tokio::test]
async fn a_script_that_throws_fails_the_workflow_with_its_line() {
    let host = Fake::new(|_| text("ok"));
    let source = "export const meta = { name: 'bad', description: 'Throws' }\n\
                  await agent('one')\nconst list = undefined\nreturn list.length\n";
    let ended = run(&script(source), input(Value::Null), &host).await;
    let Err(ScriptEnd::Threw(said)) = &ended.result else {
        panic!("{:?}", ended.result);
    };
    assert!(said.contains("TypeError"), "{said}");
    assert!(said.contains(":4"), "the author's own line number: {said}");
    assert_eq!(ended.report.status, SubagentStatus::Failed);
    assert_eq!(ended.report.agents[0].state, CliAgentState::Done);
    assert!(
        ended
            .report
            .summary
            .as_deref()
            .is_some_and(|s| s.contains("TypeError"))
    );
}
