//! What every planning step builds its asks from: the goal brief, the crew for a role, prompt
//! parts, recorded decisions, and the planning phase's start and end.

use argonauts_core::events::RunEventKind;
use argonauts_core::events::run::{PhaseChange, RunPhase};
use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::names::{ModelRef, PlanLabel, RoleName};
use argonauts_core::settings::Settings;
use argonauts_core::sha256_hex;
use argonauts_core::steps::plan::GoalBrief;
use argonauts_core::workflow::{Count, Step, StepModels};
use argonauts_core::{Note, NoteLevel, PhaseStatus, TaskId};

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::roles;
use crate::state::RunState;
use crate::status::Viewer;
use crate::tie_breaker;
use crate::workflow::StepCall;

/// Notes longer than this are cut, so memory can't crowd out the task.
const NOTE_MAX_CHARS: usize = 2_000;
const NOTES_MAX: usize = 20;

pub fn part(kind: InputPartKind, label: &str, text: String) -> InputPart {
    InputPart {
        kind,
        label: label.to_owned(),
        approx_tokens: u32::try_from(text.len().div_ceil(4)).unwrap_or(u32::MAX),
        text,
        source: None,
    }
}

/// Planning works from the approved goal only, never from a draft you haven't approved.
pub fn require_approved_goal(state: &RunState) -> EngineResult<()> {
    if state.goal.approved.is_some() {
        Ok(())
    } else {
        Err(EngineError::Refused(
            "The goal isn't approved yet, so planning can't start.".to_owned(),
        ))
    }
}

/// The approved goal, or your request when the run has no approved goal. A draft you haven't
/// approved is never the brief.
pub fn goal_brief(state: &RunState) -> GoalBrief {
    match state.goal.approved.as_ref() {
        Some(file) => GoalBrief {
            summary: file.summary.clone(),
            criteria: file
                .criteria
                .iter()
                .map(|c| format!("{}: {}", c.id, c.text))
                .collect(),
            assumptions: file
                .assumptions
                .iter()
                .map(|a| a.text.clone())
                .chain(file.answers.iter().cloned())
                .collect(),
            sha256: file.sha256.clone(),
        },
        None => {
            let request = state
                .run
                .started
                .as_ref()
                .map(|started| started.request.clone())
                .unwrap_or_default();
            GoalBrief {
                sha256: sha256_hex(request.as_bytes()),
                summary: request,
                criteria: Vec::new(),
                assumptions: Vec::new(),
            }
        }
    }
}

/// The goal in a few lines, for reviewers and the judge who settles them.
pub fn goal_text(state: &RunState) -> String {
    let brief = goal_brief(state);
    let mut text = brief.summary;
    for criterion in brief.criteria {
        text.push_str("\n- ");
        text.push_str(&criterion);
    }
    text
}

/// Recorded decisions, one line each.
pub fn decision_lines(state: &RunState) -> Vec<String> {
    state
        .plan
        .decisions
        .iter()
        .map(|d| format!("{}: {}: {}", d.id, d.title, d.choice))
        .collect()
}

/// Who may fill `role`: `roles.<role>.models` when set, else the crew.
pub fn crew_for(settings: &Settings, role: RoleName) -> Vec<ModelRef> {
    let candidates = roles::candidates(settings, role);
    if candidates.is_empty() {
        settings.crew.members()
    } else {
        candidates
    }
}

/// The step's named workers, else the role's crew, each once.
pub fn candidates_for(call: &StepCall, settings: &Settings, role: RoleName) -> Vec<ModelRef> {
    let named = match &call.step.models {
        StepModels::Named(workers) if !workers.is_empty() => Some(workers.clone()),
        _ => None,
    };
    let mut workers = named.unwrap_or_else(|| crew_for(settings, role));
    let mut seen = Vec::new();
    workers.retain(|worker| {
        let fresh = !seen.contains(worker);
        seen.push(worker.clone());
        fresh
    });
    workers
}

/// How many agents the step starts: its count, else the role's count, else `fallback`.
pub fn agent_count(
    call: &StepCall,
    settings: &Settings,
    role: RoleName,
    fallback: Option<u32>,
) -> Option<u32> {
    match call.step.count {
        Some(Count::Fixed(n)) => Some(n),
        _ => settings
            .roles
            .get(&role)
            .and_then(|role| role.count)
            .or(fallback),
    }
}

/// The step's candidates cut to its count.
pub fn workers_for(
    call: &StepCall,
    settings: &Settings,
    role: RoleName,
    fallback_count: Option<u32>,
) -> Vec<ModelRef> {
    let mut workers = candidates_for(call, settings, role);
    if let Some(count) = agent_count(call, settings, role, fallback_count) {
        workers.truncate(count as usize);
    }
    workers
}

/// The status summary as a prompt part, filtered for this role.
pub fn status_part(host: &Host, role: RoleName, label: Option<PlanLabel>) -> InputPart {
    let viewer = Viewer::Session {
        role,
        task: None,
        label,
    };
    part(InputPartKind::Status, "status", host.status_text(&viewer))
}

/// The crew's kept notes and the ones in `.argonauts/notes/` in the run's base, cut to size: the
/// crew's sourced notes as a quoted block of reference data, a file you wrote as it is. Drafting
/// agents get them; judges never do, so no judge reads what the drafting agent it judges wrote
/// down.
pub fn notes(host: &Host) -> Vec<String> {
    crate::run_review::notes::read_all(
        &host.repo().base_worktree,
        &crate::divide::repo_state_dir(host),
    )
    .into_iter()
    .take(NOTES_MAX)
    .map(|file| {
        let text = if file.notes.is_empty() {
            file.text
        } else {
            argonauts_learning::memory::quoted(&file.notes)
        };
        let name = file
            .relative
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_owned();
        let text: String = text.chars().take(NOTE_MAX_CHARS).collect();
        format!("{name}: {}", text.trim())
    })
    .collect()
}

/// Records the planning phase starting, once, naming the main agent.
pub fn phase_started(host: &Host) -> EngineResult<()> {
    let started = host.read(|state| state.run.phase == Some(RunPhase::Planning));
    if started {
        return Ok(());
    }
    let main_agent = host.settings().crew.main.clone();
    host.record(
        None,
        None,
        RunEventKind::Phase(PhaseChange {
            phase: RunPhase::Planning,
            status: PhaseStatus::Started,
            summary: Some(format!("Planning, with {main_agent} as the main agent")),
        }),
    )?;
    Ok(())
}

/// Records the planning phase's end with its one-line summary and the main agent.
pub fn phase_finished(host: &Host, summary: &str) -> EngineResult<()> {
    let main_agent = host
        .read(|state| tie_breaker::tie_breaker(state, host.settings()))
        .unwrap_or_else(|| host.settings().crew.main.clone());
    host.record(
        None,
        None,
        RunEventKind::Phase(PhaseChange {
            phase: RunPhase::Planning,
            status: PhaseStatus::Finished,
            summary: Some(format!("{summary}. The main agent was {main_agent}")),
        }),
    )?;
    Ok(())
}

/// What a step outside planning works on: its task, its `for_each` item, and its `input`,
/// which is `goal`, `plan` (the chosen plan's text), `diff` (the run branch against
/// the run's base), or an earlier step's name (that step's summaries).
pub async fn step_input(host: &Host, call: &StepCall) -> EngineResult<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(task) = &call.step.task {
        parts.push(task.clone());
    }
    if let Some(item) = &call.item {
        parts.push(match item {
            serde_json::Value::String(text) => text.clone(),
            other => other.to_string(),
        });
    }
    match call.step.input.as_deref() {
        None => {}
        Some("goal") => parts.push(host.read(goal_text)),
        Some("plan") => {
            let chosen = host.read(|state| state.plan.chosen().cloned());
            if let Some(chosen) = chosen {
                parts.push(crate::plan::drafts::read_text(&chosen)?);
            }
        }
        Some("diff") => {
            let base = host.read(|state| {
                state
                    .run
                    .started
                    .as_ref()
                    .map(|started| started.base_commit.clone())
            });
            if let Some(base) = base {
                let tree = crate::merge::branch::ensure_run_branch(host).await?;
                parts.push(host.git().diff(&tree, &base, "HEAD").await?);
            }
        }
        Some(name) => parts.extend(host.read(|state| step_summaries(state, call, name))),
    }
    Ok(parts.join("\n\n"))
}

/// The summaries of every finished round of the step called `name` in the call's workflow.
fn step_summaries(state: &RunState, call: &StepCall, name: &str) -> Vec<String> {
    let Some(record) = state.workflows.by_id.get(call.workflow()) else {
        return Vec::new();
    };
    let mut found = None;
    for (index, phase) in record.workflow.phases.iter().enumerate() {
        if let Some(path) = find_step(&phase.steps, name, &mut Vec::new()) {
            found = Some((index as u32, path));
            break;
        }
    }
    let Some((phase, path)) = found else {
        return Vec::new();
    };
    record
        .steps
        .iter()
        .filter(|progress| progress.step.phase == phase && progress.step.path == path)
        .filter_map(|progress| progress.summary.clone())
        .collect()
}

fn find_step(steps: &[Step], name: &str, prefix: &mut Vec<u32>) -> Option<Vec<u32>> {
    for (index, step) in steps.iter().enumerate() {
        prefix.push(index as u32);
        if step.name.as_deref() == Some(name) {
            return Some(prefix.clone());
        }
        if let Some(path) = find_step(&step.steps, name, prefix) {
            return Some(path);
        }
        prefix.pop();
    }
    None
}

/// Records a warning the run's log and views show, such as a skipped critic.
pub fn warn(host: &Host, task: Option<&TaskId>, text: String) -> EngineResult<()> {
    host.record(
        task,
        None,
        RunEventKind::Note(Note {
            level: NoteLevel::Warn,
            text,
        }),
    )?;
    Ok(())
}

/// Providers of workers, from the registry.
/// Who may own and who may review the tasks, as a planner reads it, after the line that says
/// which rule gave way when the rules couldn't all hold.
pub fn task_roles_lines(host: &Host) -> EngineResult<Vec<String>> {
    let task_roles = roles::tasks::for_run(host);
    task_roles.announce(host)?;
    let mut lines = task_roles.lines(&providers(host));
    lines.push(task_roles.review_line());
    Ok(lines)
}

pub fn providers(host: &Host) -> impl Fn(&ModelRef) -> String + '_ {
    move |worker| roles::provider(host, worker)
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::goal::{Assumption, Criterion, GoalFile, GoalSource};
    use argonauts_core::workflow::{StepKind, StepRef};
    use argonauts_core::{RunId, names::WorkflowId};

    use super::*;
    use crate::workflow::StepAt;

    fn call(workers: &[&str], count: Option<u32>) -> StepCall {
        let mut step = serde_json::json!({ "kind": "draft", "role": "draft" });
        if !workers.is_empty() {
            step["models"] = serde_json::json!(workers);
        }
        if let Some(count) = count {
            step["count"] = serde_json::json!(count);
        }
        let step: Step = serde_json::from_value(step).unwrap();
        assert_eq!(step.kind, StepKind::Draft);
        StepCall {
            at: StepAt {
                step: StepRef {
                    workflow: WorkflowId::numbered(1),
                    phase: 0,
                    path: vec![0],
                },
                round: 1,
            },
            depth: 0,
            standard: argonauts_core::workflow::StandardPhase::Plan,
            step,
            item: None,
        }
    }

    fn settings() -> Settings {
        Settings {
            crew: ["claude-code", "codex", "antigravity-cli"]
                .iter()
                .map(|w| w.parse().unwrap())
                .collect(),
            ..Settings::default()
        }
    }

    fn names(workers: Vec<ModelRef>) -> Vec<String> {
        workers.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn step_workers_win_then_the_crew_cut_to_the_count() {
        let settings = settings();
        assert_eq!(
            names(workers_for(
                &call(&["codex", "codex"], None),
                &settings,
                RoleName::Draft,
                None
            )),
            ["codex"]
        );
        assert_eq!(
            names(workers_for(
                &call(&[], Some(2)),
                &settings,
                RoleName::Draft,
                None
            ))
            .len(),
            2
        );
        assert_eq!(
            names(workers_for(
                &call(&[], None),
                &settings,
                RoleName::Draft,
                Some(1)
            ))
            .len(),
            1
        );
    }

    #[test]
    fn the_brief_comes_from_the_approved_goal_or_the_request() {
        let mut state = RunState::default();
        let brief = goal_brief(&state);
        assert_eq!(brief.summary, "");
        assert!(require_approved_goal(&state).is_err());
        let approved = GoalFile {
            run: RunId::new("run-01").unwrap(),
            version: 1,
            summary: "Rate limit the API".to_owned(),
            criteria: vec![Criterion::new("G1", "429 after 100 calls")],
            in_scope: Vec::new(),
            out_of_scope: Vec::new(),
            assumptions: vec![Assumption {
                text: "per API key".to_owned(),
                basis: "2 of 3".to_owned(),
                by: None,
            }],
            answers: vec!["Redis is available".to_owned()],
            source: GoalSource::Readings,
            path: "goal.md".into(),
            sha256: "abc".to_owned(),
        };
        // A draft you haven't approved, such as one you sent back, is never the brief.
        state.goal.drafted = Some(GoalFile {
            summary: "Rate limit every route".to_owned(),
            sha256: "draft".to_owned(),
            ..approved.clone()
        });
        assert_eq!(goal_brief(&state).summary, "");
        assert!(require_approved_goal(&state).is_err());

        state.goal.approved = Some(approved);
        assert!(require_approved_goal(&state).is_ok());
        let brief = goal_brief(&state);
        assert_eq!(brief.criteria, ["G1: 429 after 100 calls"]);
        assert_eq!(brief.assumptions, ["per API key", "Redis is available"]);
        assert_eq!(brief.sha256, "abc");
        assert_eq!(
            goal_text(&state),
            "Rate limit the API\n- G1: 429 after 100 calls"
        );
    }

    #[test]
    fn parts_estimate_tokens() {
        let part = part(InputPartKind::Goal, "goal", "x".repeat(9));
        assert_eq!(part.approx_tokens, 3);
    }
}
