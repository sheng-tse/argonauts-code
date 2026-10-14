//! A rehearsal runs a script against placeholder answers, so one that throws is caught before any
//! agent starts.

use std::collections::BTreeSet;

use argonauts_core::settings::crew::CrewRole;
use argonauts_runner::{Rehearsal, RunInput, Script, WorkflowFile, built_ins, rehearse};
use serde_json::json;

fn script(source: &str) -> Script {
    Script::parse(source).unwrap_or_else(|err| panic!("{err}"))
}

fn input(roles: Option<&[CrewRole]>) -> RunInput {
    RunInput {
        id: "rehearsal".to_owned(),
        args: json!({
            "request": "add due dates",
            "crew": {"main": 1, "orchestrator": 1, "worker": 2, "reviewer": 2, "judge": 1},
            "tasks": [],
        }),
        roles: roles.map(|roles| roles.iter().copied().collect::<BTreeSet<_>>()),
        ..RunInput::default()
    }
}

fn no_workflows(name: &str) -> Result<Script, String> {
    Err(format!("there's no workflow named {name}"))
}

const UNTIL_ACCEPTED: &str = r#"
export const meta = {
  name: 'review-until-clean',
  description: 'Build, then review until the reviewer accepts',
  phases: [{ name: 'Plan' }, { name: 'Build' }, { name: 'Review' }],
}
const PLAN = {
  type: 'object',
  properties: { tasks: { type: 'array', items: { type: 'object', properties: { title: { type: 'string' } } } } },
  required: ['tasks'],
}
const VERDICT = {
  type: 'object',
  properties: { accept: { type: 'boolean' }, findings: { type: 'array', items: { type: 'string' } } },
  required: ['accept', 'findings'],
}
const plan = await agent('Split it: ' + args.request, { role: 'orchestrator', phase: 'Plan', schema: PLAN })
let rounds = 0
for (const task of plan.tasks) {
  let findings = []
  while (true) {
    rounds++
    await agent(`Build ${task.title}\n${findings.join('\n')}`, { role: 'worker', phase: 'Build', task: 'one' })
    const verdict = await agent('Review it.', { role: 'reviewer', phase: 'Review', task: 'one', schema: VERDICT })
    if (verdict && verdict.accept) break
    findings = verdict ? verdict.findings : []
  }
}
return { rounds }
"#;

#[tokio::test]
async fn a_script_that_loops_until_a_review_accepts_goes_round_once_and_ends() {
    let rehearsed = rehearse(&script(UNTIL_ACCEPTED), input(None), &no_workflows).await;
    // The plan lists one placeholder task; its first review says no and its second says yes.
    assert_eq!(
        rehearsed,
        Ok(Rehearsal {
            agents: 5,
            finished: true,
            tasks: vec!["one".to_owned()],
            ran: BTreeSet::from([
                ("Build".to_owned(), CrewRole::Worker),
                ("Plan".to_owned(), CrewRole::Orchestrator),
                ("Review".to_owned(), CrewRole::Reviewer),
            ]),
        })
    );
}

#[tokio::test]
async fn a_role_the_workflow_does_not_have_is_caught_before_anything_runs() {
    let roles = [CrewRole::Orchestrator, CrewRole::Worker, CrewRole::Judge];
    let rehearsed = rehearse(&script(UNTIL_ACCEPTED), input(Some(&roles)), &no_workflows).await;
    let why = rehearsed.expect_err("the script names reviewers");
    assert_eq!(
        why,
        "rehearsed with placeholder answers, it threw: Error: agent(): this workflow's file has \
         no reviewer step; its roles are orchestrator, worker, judge"
    );
}

#[tokio::test]
async fn reading_a_field_of_an_answer_that_is_plain_text_is_caught() {
    let source = r#"
export const meta = { name: 'careless', description: 'Reads what it never asked for' }
const plan = await agent('List the tasks.', { role: 'orchestrator' })
return plan.tasks.map((task) => task.title)
"#;
    let why = rehearse(&script(source), input(None), &no_workflows)
        .await
        .expect_err("a text answer has no tasks");
    assert!(
        why.starts_with("rehearsed with placeholder answers, it threw: TypeError"),
        "{why}"
    );
    assert!(!why.contains('\n'), "one line: {why}");
}

#[tokio::test]
async fn a_script_still_asking_when_the_allowance_runs_out_is_let_through() {
    let source = r#"
export const meta = { name: 'patient', description: 'Asks until someone says stop' }
let asked = 0
while (true) {
  asked++
  const answer = await agent('Are we there yet?', { role: 'judge' })
  if (answer === 'stop') break
}
return asked
"#;
    let rehearsed = rehearse(&script(source), input(None), &no_workflows).await;
    assert_eq!(
        rehearsed,
        Ok(Rehearsal {
            agents: 200,
            finished: false,
            tasks: Vec::new(),
            ran: BTreeSet::new(),
        })
    );
}

#[tokio::test]
async fn a_workflow_run_as_a_step_is_rehearsed_with_it() {
    let source = r#"
export const meta = { name: 'outer', description: 'Runs another as a step' }
return await workflow('inner', { request: args.request })
"#;
    let inner = |name: &str| match name {
        "inner" => Ok(script(
            "export const meta = { name: 'inner', description: 'One agent' }\n\
             return await agent('Do it: ' + args.request, { role: 'worker' })",
        )),
        other => Err(format!("there's no workflow named {other}")),
    };
    let rehearsed = rehearse(&script(source), input(None), &inner).await;
    assert_eq!(
        rehearsed,
        Ok(Rehearsal {
            agents: 1,
            finished: true,
            tasks: Vec::new(),
            // A workflow run as a step is a phase of its own.
            ran: BTreeSet::from([("▸ inner".to_owned(), CrewRole::Worker)]),
        })
    );
    let missing = rehearse(&script(source), input(None), &no_workflows).await;
    assert!(
        missing
            .expect_err("no such workflow")
            .contains("there's no workflow named inner")
    );
}

/// The steps the built-in workflows ship with are what runs when no script can be used, so
/// they have to get through a rehearsal themselves.
#[tokio::test]
async fn every_built_in_workflows_own_steps_get_through_a_rehearsal() {
    for (name, text) in built_ins() {
        let file = WorkflowFile::parse(text)
            .unwrap_or_else(|err| panic!("{name}: {err}"))
            .unwrap_or_else(|| panic!("{name} has no steps"));
        let script = Script::from_steps(&file);
        let roles: Vec<CrewRole> = file.roles().into_iter().collect();
        let rehearsed = rehearse(&script, input(Some(&roles)), &no_workflows).await;
        let rehearsed = rehearsed.unwrap_or_else(|why| panic!("{name}: {why}"));
        assert!(
            rehearsed.finished && rehearsed.agents > 0,
            "{name}: {rehearsed:?}"
        );
    }
}
