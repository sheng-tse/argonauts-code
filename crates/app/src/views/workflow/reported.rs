//! A workflow the runner runs as one step of a run's own workflow. The run's own phases say
//! only that the workflow's steps run, and then the merge. What those steps are is reported as
//! they run: the workflow's own name, its phases in the order it declares them, and its
//! agents, in the shape a CLI reports its own workflow in.
//!
//! The view shows the reported phases in place of the step's one phase, so what you see is the
//! workflow that runs, whatever it is called and however many phases it has.

use argonauts_core::events::session::{VendorAgent, VendorAgentState, VendorWorkflow};
use argonauts_core::names::{SessionKey, WorkflowId};
use argonauts_core::views::{AgentRow, PhaseCount, PhaseRow, RowStatus, WorkflowRow};
use argonauts_core::workflow::{StandardPhase, StepRef};

use crate::views::sessions::{RunFacts, SessionFacts};

/// One step's workflow as the runner last reported it.
pub(super) struct StepWorkflow {
    /// The step that runs it.
    pub step: StepRef,
    pub workflow: VendorWorkflow,
    /// The session behind each agent that has one yet, by the agent's index in the report.
    pub seats: Vec<(u32, SessionKey)>,
}

/// What a reported workflow adds to the view of the workflow whose step runs it.
pub(super) struct Reported {
    /// Agents the report puts in no phase of its own. They stay in the step's own phase.
    pub loose: Vec<AgentRow>,
    /// Its phases in the order it declares them, each with the agents that have started.
    pub phases: Vec<PhaseRow>,
    /// Done of all its agents, the ones still queued among them.
    pub agents_done: u32,
    pub agents_total: u32,
}

impl StepWorkflow {
    pub fn runs_in(&self, workflow: &WorkflowId, phase: u32) -> bool {
        self.step.workflow == *workflow && self.step.phase == phase
    }

    /// Whether `row` is one of its agents: the session that works one now, or an earlier try
    /// under the same label, which the newer session stands for.
    pub fn holds(&self, row: &AgentRow) -> bool {
        let label = row.label.trim();
        self.seats
            .iter()
            .any(|(_, session)| *session == row.session)
            || (!label.is_empty()
                && self
                    .workflow
                    .agents
                    .iter()
                    .any(|agent| agent.label.trim() == label))
    }

    /// The agent's row once it has a session, under the label the workflow gave it.
    fn row(&self, agent: &VendorAgent, sessions: &RunFacts) -> Option<AgentRow> {
        let seated = self
            .seats
            .iter()
            .find(|(index, _)| *index == agent.index)
            .and_then(|(_, session)| sessions.session(session));
        let facts = seated.or_else(|| self.under_label(agent, sessions))?;
        let mut row = sessions.agent_row(facts);
        if !agent.label.trim().is_empty() {
            row.label.clone_from(&agent.label);
        }
        Some(row)
    }

    /// The step's newest session under the agent's label. A report sent before the agent's
    /// session started names no seat for it yet, and the runner reports again only when the
    /// agent ends, so until then its label is how its session is known.
    fn under_label<'a>(
        &self,
        agent: &VendorAgent,
        sessions: &'a RunFacts,
    ) -> Option<&'a SessionFacts> {
        let label = agent.label.trim();
        if label.is_empty() {
            return None;
        }
        sessions.sessions.iter().rev().find(|facts| {
            facts.started.workflow_step.as_ref() == Some(&self.step)
                && facts.started.label.as_deref().map(str::trim) == Some(label)
        })
    }

    /// Its phases as the view's rows. `standard` is the step's own phase's, which every phase
    /// of the workflow it runs shares.
    pub fn rows(&self, sessions: &RunFacts, standard: StandardPhase) -> Reported {
        let workflow = &self.workflow;
        let declared = |agent: &VendorAgent| {
            agent
                .phase
                .is_some_and(|index| workflow.phases.iter().any(|phase| phase.index == index))
        };
        let loose = workflow
            .agents
            .iter()
            .filter(|agent| !declared(agent))
            .filter_map(|agent| self.row(agent, sessions))
            .collect();
        let phases = workflow
            .phases
            .iter()
            .map(|phase| {
                let agents: Vec<(&VendorAgent, Option<AgentRow>)> = workflow
                    .agents_in(phase.index)
                    .map(|agent| (agent, self.row(agent, sessions)))
                    .collect();
                // Once the workflow is over, a phase none of its agents ran in stays, marked as
                // never run, so the workflow reads as it was declared.
                let status = if workflow.ended() && agents.is_empty() {
                    RowStatus::Skipped
                } else {
                    phase_status(&agents)
                };
                PhaseRow {
                    name: phase.title.clone(),
                    detail: phase.detail.clone(),
                    standard,
                    status,
                    attempt: None,
                    count: workflow
                        .count(phase.index)
                        .and_then(|(done, total)| PhaseCount::of(done, total)),
                    rows: agents
                        .into_iter()
                        .filter_map(|(_, row)| row.map(WorkflowRow::Agent))
                        .collect(),
                }
            })
            .collect();
        let done = workflow
            .agents
            .iter()
            .filter(|agent| agent.state == VendorAgentState::Done)
            .count();
        Reported {
            loose,
            phases,
            agents_done: u32::try_from(done).unwrap_or(u32::MAX),
            agents_total: u32::try_from(workflow.agents.len()).unwrap_or(u32::MAX),
        }
    }
}

/// Where a phase stands by its agents. One with a session goes by what its session says,
/// which knows when it waits for you; one still queued has only the report's word.
fn phase_status(agents: &[(&VendorAgent, Option<AgentRow>)]) -> RowStatus {
    let statuses: Vec<RowStatus> = agents
        .iter()
        .map(|(agent, row)| match row {
            Some(row) => row.status,
            None => match agent.state {
                VendorAgentState::Queued => RowStatus::Queued,
                VendorAgentState::Running | VendorAgentState::Other => RowStatus::Running,
                VendorAgentState::Done => RowStatus::Done,
                VendorAgentState::Failed => RowStatus::Failed,
            },
        })
        .collect();
    let any = |status: RowStatus| statuses.contains(&status);
    let all = |status: RowStatus| statuses.iter().all(|known| *known == status);
    if any(RowStatus::NeedsYou) {
        RowStatus::NeedsYou
    } else if any(RowStatus::Running) {
        RowStatus::Running
    } else if all(RowStatus::Queued) {
        // None started, or it has no agents yet.
        RowStatus::Waiting
    } else if any(RowStatus::Queued) {
        // Between two of its agents.
        RowStatus::Running
    } else if all(RowStatus::Failed) {
        RowStatus::Failed
    } else {
        RowStatus::Done
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::session::{
        EndReason, SessionEvent, SubagentStatus, VendorPhase, VendorWorkflow,
    };
    use argonauts_core::events::workflow::{WorkflowEvent, WorkflowStarted};
    use argonauts_core::events::{RunEvent, RunEventKind};
    use argonauts_core::names::RoleName;
    use argonauts_core::views::WorkflowView;
    use argonauts_core::workflow::Workflow;

    use super::super::{WorkflowLog, view_from};
    use super::*;
    use crate::client::test_support::{event, run_id, session_ended, session_started};

    fn outer() -> WorkflowId {
        WorkflowId::numbered(1)
    }

    /// The step of the run's own workflow that runs the workflow in use.
    fn work() -> StepRef {
        StepRef {
            workflow: outer(),
            phase: 0,
            path: vec![0],
        }
    }

    /// A run of the kind the runner makes: the workflow's own steps as one step, then the merge.
    fn run_started() -> RunEvent {
        let workflow: Workflow = serde_json::from_value(serde_json::json!({
            "name": "script",
            "phases": [
                {"name": "Work", "standard": "do", "steps": [{"kind": "build"}]},
                {"name": "Merge", "standard": "decide", "steps": [{"kind": "merge"}]}
            ]
        }))
        .unwrap();
        event(
            0,
            RunEventKind::Workflow(WorkflowEvent::Started(Box::new(WorkflowStarted {
                id: outer(),
                from: None,
                version: "v1".to_owned(),
                parent: None,
                workflow,
            }))),
        )
    }

    /// A session the Work step started, under `label`.
    fn seated(seq: u64, session: u32, label: &str) -> RunEvent {
        let key = SessionKey::numbered(session);
        let mut event = session_started(seq, &key, None, RoleName::Worker, "codex");
        if let RunEventKind::Session(SessionEvent::Started(started)) = &mut event.kind {
            started.label = Some(label.to_owned());
            started.workflow_step = Some(work());
        }
        event
    }

    /// A report naming `phases` from 1, as the runner numbers them, and `agents` by label,
    /// phase and state, from 1 too.
    fn report(
        name: &str,
        phases: &[&str],
        agents: &[(&str, Option<u32>, VendorAgentState)],
    ) -> VendorWorkflow {
        VendorWorkflow {
            id: "steps".into(),
            call: None,
            name: name.into(),
            description: "what the workflow does".into(),
            phases: phases
                .iter()
                .zip(1..)
                .map(|(title, index)| VendorPhase {
                    index,
                    title: (*title).to_owned(),
                    detail: String::new(),
                })
                .collect(),
            agents: agents
                .iter()
                .zip(1..)
                .map(|((label, phase, state), index)| VendorAgent {
                    index,
                    label: (*label).to_owned(),
                    phase: *phase,
                    model: None,
                    state: *state,
                    tokens: 0,
                    tool_calls: 0,
                    duration_ms: 0,
                    started_at_ms: None,
                    attempt: 1,
                    prompt: None,
                    result: None,
                })
                .collect(),
            status: SubagentStatus::Running,
            tokens: 0,
            tool_calls: 0,
            duration_ms: 0,
            summary: None,
            started_at_ms: None,
        }
    }

    /// The run's view once the runner reported `workflow`, with `seats` behind its agents.
    fn view(events: &[RunEvent], workflow: &VendorWorkflow, seats: &[(u32, u32)]) -> WorkflowView {
        let mut log = WorkflowLog::of(events);
        let seats = seats
            .iter()
            .map(|(agent, session)| (*agent, SessionKey::numbered(*session)))
            .collect();
        log.note_report(&work(), workflow, seats);
        view_from(&log, events, &run_id(), None).unwrap()
    }

    fn names(view: &WorkflowView) -> Vec<&str> {
        view.phases
            .iter()
            .map(|phase| phase.name.as_str())
            .collect()
    }

    fn labels(phase: &PhaseRow) -> Vec<&str> {
        phase
            .rows
            .iter()
            .map(|row| match row {
                WorkflowRow::Agent(agent) => agent.label.as_str(),
                _ => "not an agent",
            })
            .collect()
    }

    use VendorAgentState::{Done, Queued, Running};

    /// A sample moment: the orchestrator wrote the steps, one builder is done, one
    /// builds, one waits its turn, and the reviewers haven't started.
    #[test]
    fn the_view_shows_the_phases_of_the_workflow_a_step_runs() {
        let events = vec![
            run_started(),
            seated(1, 1, "plan:orchestrator"),
            session_ended(2, &SessionKey::numbered(1), EndReason::Completed, 1_000),
            seated(3, 2, "build:store"),
            session_ended(4, &SessionKey::numbered(2), EndReason::Completed, 1_000),
            seated(5, 3, "build:cli"),
        ];
        let workflow = report(
            "add-due-dates",
            &["Build", "Review"],
            &[
                ("build:store", Some(1), Done),
                ("build:cli", Some(1), Running),
                ("build:docs", Some(1), Queued),
                ("review:store", Some(2), Queued),
                ("review:cli", Some(2), Queued),
            ],
        );
        let view = view(&events, &workflow, &[(1, 2), (2, 3)]);

        // The workflow's own phases in its order are the view's: no phase of
        // the run's own around them. The agent that wrote the steps goes first in the first.
        assert_eq!(names(&view), ["Build", "Review"]);
        let [build, review] = view.phases.as_slice() else {
            panic!("two phases");
        };
        assert_eq!(
            labels(build),
            ["plan:orchestrator", "build:store", "build:cli"]
        );

        // An agent has a row once it has a session, and the count knows the ones that wait.
        assert_eq!(build.count, PhaseCount::of(1, 3));
        assert_eq!(build.status, RowStatus::Running);
        assert!(review.rows.is_empty());
        assert_eq!(review.count, PhaseCount::of(0, 2));
        assert_eq!(review.status, RowStatus::Waiting);

        // Only the workflow's own five count.
        assert_eq!((view.agents_done, view.agents_total), (1, 5));
        // An unnamed run goes by the name of the workflow it runs.
        assert_eq!(view.name, "add-due-dates");
        assert_eq!(view.description, "what the workflow does");
    }

    /// The runner reports an agent as running before its session starts, and names its seat
    /// only in a later report. Until then the agent's label finds its session, so the phase
    /// shows who works in it.
    #[test]
    fn an_agent_whose_seat_isnt_reported_yet_is_found_by_its_label() {
        let events = vec![
            run_started(),
            seated(1, 1, "plan:orchestrator"),
            session_ended(2, &SessionKey::numbered(1), EndReason::Completed, 1_000),
            seated(3, 2, "build:rate-limit"),
        ];
        let workflow = report(
            "rate-limit-per-key",
            &["Build", "Review"],
            &[("build:rate-limit", Some(1), Running)],
        );
        let view = view(&events, &workflow, &[]);
        let [build, _review] = view.phases.as_slice() else {
            panic!("two phases");
        };
        assert_eq!(labels(build), ["plan:orchestrator", "build:rate-limit"]);
        assert_eq!(build.status, RowStatus::Running);
    }

    /// Nothing about the phases is known ahead: one, twelve, or the same name twice, in the
    /// workflow's own order, each agent in the phase it was put in.
    #[test]
    fn whatever_phases_the_workflow_reports_are_the_views() {
        let events = vec![run_started(), seated(1, 1, "translate:changelog")];
        let one = report(
            "translate",
            &["Translate the changelog"],
            &[("translate:changelog", Some(1), Running)],
        );
        let shown = view(&events, &one, &[(1, 1)]);
        assert_eq!(names(&shown), ["Translate the changelog"]);
        assert_eq!(labels(&shown.phases[0]), ["translate:changelog"]);

        let twelve = [
            "Survey", "Outline", "Draft", "Critique", "Draft", "Critique", "Numbers", "Figures",
            "Proof", "Typeset", "Package", "Announce",
        ];
        // The agent sits in the second Draft, the fifth phase, not the first of that name.
        let events = vec![run_started(), seated(1, 1, "draft:two")];
        let paper = report(
            "write-the-paper",
            &twelve,
            &[("draft:two", Some(5), Running)],
        );
        let shown = view(&events, &paper, &[(1, 1)]);
        assert_eq!(names(&shown), twelve);
        let with_agents: Vec<usize> = shown
            .phases
            .iter()
            .enumerate()
            .filter(|(_, phase)| !phase.rows.is_empty())
            .map(|(i, _)| i)
            .collect();
        assert_eq!(with_agents, [4]);
        assert_eq!(shown.phases[2].status, RowStatus::Waiting);
        assert_eq!(shown.phases[4].status, RowStatus::Running);
    }

    /// Before the workflow says what its phases are, the step's own phase holds what there
    /// is, and an agent it puts in no phase stays there too.
    #[test]
    fn a_workflow_that_reports_no_phases_yet_keeps_the_steps_own_phase() {
        let events = vec![run_started()];
        let nothing = report("", &[], &[]);
        let shown = view(&events, &nothing, &[]);
        assert_eq!(names(&shown), ["Work", "Merge"]);
        assert_eq!((shown.agents_done, shown.agents_total), (0, 0));
        // It gave no name either, so the run's own stands.
        assert_eq!(shown.name, "script");

        let events = vec![run_started(), seated(1, 1, "scout")];
        let loose = report("warming-up", &[], &[("scout", None, Running)]);
        let shown = view(&events, &loose, &[(1, 1)]);
        assert_eq!(names(&shown), ["Work", "Merge"]);
        assert_eq!(labels(&shown.phases[0]), ["scout"]);
        assert_eq!((shown.agents_done, shown.agents_total), (0, 1));

        // A phase it names later takes the agents put in it, and one in none stays.
        let events = vec![
            run_started(),
            seated(1, 1, "scout"),
            seated(2, 2, "build:a"),
        ];
        let grown = report(
            "warming-up",
            &["Build"],
            &[("scout", None, Done), ("build:a", Some(1), Running)],
        );
        let shown = view(&events, &grown, &[(1, 1), (2, 2)]);
        // Once it names one, its phases are the view's, and an agent in none goes first in
        // the first of them.
        assert_eq!(names(&shown), ["Build"]);
        assert_eq!(labels(&shown.phases[0]), ["scout", "build:a"]);
    }

    /// An agent tried again is one row, the session that works it now, and a newer report of
    /// the same workflow replaces the older one.
    #[test]
    fn a_retried_agent_is_one_row_and_the_newest_report_stands() {
        let events = vec![
            run_started(),
            seated(1, 1, "build:cli"),
            session_ended(
                2,
                &SessionKey::numbered(1),
                EndReason::Crashed {
                    message: "the CLI exited".to_owned(),
                },
                1_000,
            ),
            seated(3, 2, "build:cli"),
        ];
        let mut log = WorkflowLog::of(&events);
        let first = report(
            "add-due-dates",
            &["Build"],
            &[("build:cli", Some(1), Running)],
        );
        log.note_report(&work(), &first, vec![(1, SessionKey::numbered(1))]);
        let again = report(
            "add-due-dates",
            &["Build", "Review"],
            &[("build:cli", Some(1), Running)],
        );
        log.note_report(&work(), &again, vec![(1, SessionKey::numbered(2))]);
        let shown = view_from(&log, &events, &run_id(), None).unwrap();
        assert_eq!(names(&shown), ["Build", "Review"]);
        let WorkflowRow::Agent(row) = &shown.phases[0].rows[0] else {
            panic!("an agent");
        };
        assert_eq!(shown.phases[0].rows.len(), 1);
        assert_eq!(row.session, SessionKey::numbered(2));
        assert_eq!(row.status, RowStatus::Running);
        assert_eq!((shown.agents_done, shown.agents_total), (0, 1));
    }

    /// The runner's own event is all the view needs: recorded in the run, its report shows
    /// with no other word to the view.
    #[test]
    fn the_runners_report_event_reaches_the_view() {
        use argonauts_core::events::workflow::{AgentSession, WorkflowReport};

        let workflow = report(
            "rate-limit-per-key",
            &["Build", "Review"],
            &[
                ("build:rate-limit", Some(1), Done),
                ("review:rate-limit", Some(2), Running),
            ],
        );
        let seat = |agent: u32, session: u32| AgentSession {
            agent,
            session: SessionKey::numbered(session),
        };
        let events = vec![
            run_started(),
            seated(1, 1, "plan:orchestrator"),
            session_ended(2, &SessionKey::numbered(1), EndReason::Completed, 1_000),
            seated(3, 2, "build:rate-limit"),
            session_ended(4, &SessionKey::numbered(2), EndReason::Completed, 1_000),
            seated(5, 3, "review:rate-limit"),
            event(
                6,
                RunEventKind::Workflow(WorkflowEvent::Reported(Box::new(WorkflowReport {
                    step: work(),
                    workflow,
                    sessions: vec![seat(1, 2), seat(2, 3)],
                    size_asked: None,
                }))),
            ),
        ];
        let shown = super::super::workflow_view(&events, &run_id(), None).unwrap();
        assert_eq!(names(&shown), ["Build", "Review"]);
        assert_eq!(
            labels(&shown.phases[0]),
            ["plan:orchestrator", "build:rate-limit"]
        );
        assert_eq!(labels(&shown.phases[1]), ["review:rate-limit"]);
        assert_eq!(shown.phases[1].status, RowStatus::Running);
        assert_eq!((shown.agents_done, shown.agents_total), (1, 2));
        assert_eq!(shown.name, "rate-limit-per-key");
    }

    /// Once the workflow is over, a phase none of its agents ran in stays, marked as never run.
    #[test]
    fn a_finished_workflow_marks_the_phases_nothing_ran_in() {
        let events = vec![
            run_started(),
            seated(1, 1, "build:a"),
            session_ended(2, &SessionKey::numbered(1), EndReason::Completed, 1_000),
        ];
        let mut done = report(
            "small-fix",
            &["Build", "Review", "Sign off"],
            &[("build:a", Some(1), Done)],
        );
        done.status = SubagentStatus::Done;
        let shown = view(&events, &done, &[(1, 1)]);
        assert_eq!(names(&shown), ["Build", "Review", "Sign off"]);
        assert_eq!(shown.phases[0].status, RowStatus::Done);
        assert_eq!(shown.phases[1].status, RowStatus::Skipped);
        assert_eq!(shown.phases[2].status, RowStatus::Skipped);
    }
}
