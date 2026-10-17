//! A run's first moments while its workflow is written, from the recorded Captain run's own
//! first events: the working line says the workflow is being written, and the list under the
//! input holds no row for the run until the phases are known. Compared cell for cell with
//! `tests/golden/workflow_shape_known.txt` at its width of 118 columns.

use std::time::{Duration, Instant};

use argonauts_core::AgentEvent;
use argonauts_core::app::{AppEvent, AppMode};
use argonauts_core::events::{RunEvent, RunEventKind};
use ratatui::text::Line;

use super::Bridge;
use super::transcript::picture_check::{Unlike, assert_rows_match};
use crate::bottom::ApprovalMode;
use crate::model::ViewModel;
use crate::options::UiOptions;
use crate::render::{Clock, RenderCtx};
use crate::theme::{Glyphs, Theme};

const RECORDED: &str = include_str!("../../../crew/tests/fixtures/runs/captain-review-loop.jsonl");
const PICTURE: &str = include_str!("../../tests/golden/workflow_shape_known.txt");
const COLOURS: &str = include_str!("../../tests/golden/workflow_shape_known.html");
const WIDTH: u16 = 118;

/// The events up to and with the worker's prompt, before Claude Code writes its workflow.
const FIRST: u64 = 21;

/// The picture's rows, each without the blanks that pad it to the picture's width.
fn picture() -> String {
    PICTURE
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

/// The recorded run, its placeholders filled in as a run on this machine would have them.
fn record() -> Vec<RunEvent> {
    let text = [
        ("{run}", "run-20261014-054425-0001"),
        ("{repo}", "/repo"),
        ("{run_dir}", "/state/run"),
        ("{worktrees}", "/worktrees"),
        ("{base_commit}", "eecdf983f2dfa422118f1a999438c399f3d446f8"),
        ("{branch}", "argonauts/run"),
        ("{item}", "money"),
    ]
    .iter()
    .fold(RECORDED.to_owned(), |text, (from, to)| {
        text.replace(from, to)
    });
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("a recorded event"))
        .collect()
}

struct Screen {
    bridge: Bridge,
    model: ViewModel,
    started: Instant,
    /// When the events sent now arrive.
    now: Instant,
}

impl Screen {
    fn new() -> Self {
        let started = Instant::now();
        Self {
            bridge: Bridge::new(&UiOptions::default()),
            model: ViewModel::default(),
            started,
            now: started,
        }
    }

    fn send(&mut self, event: AppEvent) {
        self.bridge.on_event(&mut self.model, &event, self.now);
    }

    fn run(&mut self, event: RunEvent) {
        self.send(AppEvent::Run {
            event: Box::new(event),
        });
    }

    /// Whether the list under the input holds a row for `run`.
    fn has_row(&self, run: &str) -> bool {
        self.model
            .bottom
            .workflows
            .rows
            .iter()
            .any(|row| row.key == run)
    }

    /// The bottom of the screen as drawn when the last events arrived.
    fn bottom(&self) -> Vec<Line<'static>> {
        let theme = theme();
        let ctx = RenderCtx::new(WIDTH, &theme, Clock::new(self.started, self.now));
        self.model.bottom.render(&ctx).lines
    }
}

/// Claude Code 2.1.301's stream for the worker's first moments, as its raw record has them: the
/// first message ends at 421 tokens, and the next begins the Workflow call, whose input streams
/// a dozen characters a piece with no usage until the message ends. Twelve seconds in, 7,104
/// characters of it have come.
fn streamed() -> Vec<serde_json::Value> {
    use serde_json::json;

    let message = |id: &str, usage: serde_json::Value| {
        json!({"type": "stream_event", "event": {"type": "message_start", "message": {
            "id": id, "model": "claude-haiku-5-5", "usage": usage}}, "parent_tool_use_id": null})
    };
    let event = |event: serde_json::Value| json!({"type": "stream_event", "event": event, "parent_tool_use_id": null});
    let mut lines = vec![
        message(
            "msg_1",
            json!({"input_tokens": 2, "cache_creation_input_tokens": 379_734, "output_tokens": 5}),
        ),
        event(
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"},
            "usage": {"input_tokens": 2, "cache_creation_input_tokens": 379_734,
                      "output_tokens": 421}}),
        ),
        event(json!({"type": "message_stop"})),
        message(
            "msg_2",
            json!({"input_tokens": 2, "cache_creation_input_tokens": 1_194,
                   "cache_read_input_tokens": 379_734, "output_tokens": 3}),
        ),
        event(
            json!({"type": "content_block_start", "index": 1, "content_block": {
            "type": "tool_use", "id": "toolu_01STzcFZA2QUxaaq8qp3oXF6", "name": "Workflow",
            "input": {}}}),
        ),
    ];
    let piece = "\\n  phases: ";
    for _ in 0..592 {
        lines.push(event(json!({"type": "content_block_delta", "index": 1,
            "delta": {"type": "input_json_delta", "partial_json": piece}})));
    }
    lines
}

/// What the worker's stream says `at` into the run, in the record's place, as Claude Code's
/// parser reads it: what it has written so far, and the Workflow call it has begun, whose
/// script isn't whole yet.
fn writing(start: &RunEvent, after: &RunEvent, at: Duration) -> Vec<RunEvent> {
    use argonauts_core::Adapter;

    let mut parser = argonauts_adapter_claude_code::ClaudeCodeAdapter.parser();
    let events = streamed()
        .iter()
        .flat_map(|line| parser.parse_stdout_line(&line.to_string()))
        .collect::<Vec<_>>();
    events
        .into_iter()
        .zip(after.seq + 1..)
        .map(|(event, seq)| RunEvent {
            seq,
            ts: start.ts + at,
            source: "claude-code".to_owned(),
            kind: RunEventKind::Agent(event),
            ..after.clone()
        })
        .collect()
}

/// The picture's working line goes on to offer Esc. Esc leaves a run's agents going, so the
/// line closes after its tokens and the line under the input offers `? for shortcuts`.
const NO_ESC: Unlike = Unlike {
    row: 0,
    cols: (42, WIDTH as usize - 1),
    why: "Esc leaves a run's agents going, so the line offers no Esc and closes after its tokens",
};

/// The picture ends the line under the input with `↓ to manage`, which only a row under the
/// input has to offer, and the picture has no row.
const NOTHING_TO_MANAGE: Unlike = Unlike {
    row: 5,
    cols: (56, WIDTH as usize - 1),
    why: "with no row under the input there is nothing to manage, so ↓ to manage is left out",
};

/// `⏵⏵ autopilot on` on the line under the input.
const AUTOPILOT: Unlike = Unlike {
    row: 5,
    cols: (2, 16),
    why: "autopilot keeps its own colour, where the picture paints it amber",
};

/// The input's rules, rows 5 and 7 of the picture.
fn rule(row: usize) -> Unlike {
    Unlike {
        row,
        cols: (0, WIDTH as usize - 1),
        why: "the input's rules keep their own grey, a step brighter than the picture's",
    }
}

/// Nothing of the run is drawn before its workflow has started, nor while Claude Code writes
/// the workflow's script: the working line alone says so, with no phase, tasks or agents of the
/// plan the script replaces, and the list under the input holds no row until the first report
/// names the phases.
#[test]
fn a_run_draws_no_row_until_its_workflow_has_its_phases() {
    let events = record();
    let run = events[0].run_id.clone();
    let mut screen = Screen::new();
    screen.model.bottom.mode.approval = ApprovalMode::Autopilot;
    let first: Vec<RunEvent> = events.iter().filter(|e| e.seq <= FIRST).cloned().collect();
    for event in &first {
        screen.run(event.clone());
        if event.seq == 0 {
            screen.send(AppEvent::Mode {
                mode: AppMode::Working { run: run.clone() },
            });
        }
        assert!(
            !screen.has_row(run.as_str()),
            "a row at seq {}: {:?}",
            event.seq,
            screen.model.bottom.workflows.rows
        );
    }
    // Twelve seconds in, Claude Code is writing the script, its tokens still coming.
    let twelve = Duration::from_secs(12);
    screen.now = screen.started + twelve;
    for event in writing(&first[0], first.last().expect("the first events"), twelve) {
        screen.run(event);
    }
    assert!(!screen.has_row(run.as_str()));

    let lines = screen.bottom();
    assert_rows_match(
        "workflow_shape_known",
        &lines,
        (&picture(), COLOURS),
        3..9,
        &theme(),
        &[NO_ESC, rule(2), rule(4), AUTOPILOT, NOTHING_TO_MANAGE],
    );

    // The crew's reviewers Claude Code's workflow asks for come and go before it reports its
    // phases, and still nothing of the run shows. Its first report with phases brings the row.
    let shaped = events
        .iter()
        .find(|event| {
            matches!(&event.kind, RunEventKind::Agent(AgentEvent::Workflow { workflow })
                if !workflow.phases.is_empty())
        })
        .expect("a report with phases")
        .seq;
    for event in events.iter().filter(|e| e.seq > FIRST && e.seq <= shaped) {
        assert!(
            !screen.has_row(run.as_str()),
            "a row before seq {}",
            event.seq
        );
        screen.run(event.clone());
    }
    assert!(
        screen.has_row(run.as_str()),
        "no row once the phases are known"
    );
}

/// The council run's own record, the one its resumed conversation is drawn from.
const COUNCIL: &str = include_str!("../../golden/resumed_council/events.jsonl");

fn council() -> Vec<RunEvent> {
    COUNCIL
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("a recorded event"))
        .collect()
}

/// The seq of the first report that names a workflow's phases.
fn shaped_at(events: &[RunEvent]) -> u64 {
    events
        .iter()
        .find(|event| match &event.kind {
            RunEventKind::Agent(AgentEvent::Workflow { workflow }) => !workflow.phases.is_empty(),
            RunEventKind::Workflow(argonauts_core::events::workflow::WorkflowEvent::Reported(
                report,
            )) => !report.workflow.phases.is_empty(),
            _ => false,
        })
        .expect("a report with phases")
        .seq
}

/// What the working line said after an event, by the event's seq: its word and its details.
type Said = (u64, Option<(String, Vec<String>)>);

impl Screen {
    /// Each event of `events` in turn, the run at work from its start, with what the working
    /// line said after each.
    fn lines_after(&mut self, events: &[RunEvent]) -> Vec<Said> {
        let mut said = Vec::new();
        for event in events {
            self.run(event.clone());
            if event.seq == 0 {
                self.send(AppEvent::Mode {
                    mode: AppMode::Working {
                        run: event.run_id.clone(),
                    },
                });
            }
            let line = self.model.bottom.working.as_ref();
            said.push((
                event.seq,
                line.map(|line| (line.verb.clone(), line.details.clone())),
            ));
        }
        said
    }
}

/// From the run's first agent until the shape is known, the workflow is being written, and the
/// working line says so the whole time: while Claude Code's script streams and its reviewers
/// answer, and while the council's planner writes its script and the runner starts it. The
/// list under the input holds no row for the run meanwhile.
#[test]
fn until_the_shape_is_known_the_line_says_the_workflow_is_being_written() {
    for (name, events) in [("captain", record()), ("council", council())] {
        let shaped = shaped_at(&events);
        let run = events[0].run_id.clone();
        let mut screen = Screen::new();
        let before: Vec<RunEvent> = events
            .iter()
            .filter(|event| event.seq < shaped)
            .cloned()
            .collect();
        let said = screen.lines_after(&before);
        let at_work: Vec<_> = said
            .iter()
            .filter(|(_, line)| line.is_some())
            .skip_while(|(seq, _)| {
                // From the first session's start on.
                !events.iter().any(|event| {
                    event.seq <= *seq
                        && matches!(
                            &event.kind,
                            RunEventKind::Session(
                                argonauts_core::events::session::SessionEvent::Started(_)
                            )
                        )
                })
            })
            .collect();
        assert!(!at_work.is_empty(), "{name}: no working line");
        for (seq, line) in at_work {
            let (verb, details) = line.as_ref().expect("a working line");
            assert_eq!(
                verb,
                super::cli_workflow_run::WRITING,
                "{name} at seq {seq}"
            );
            assert!(details.is_empty(), "{name} at seq {seq}: {details:?}");
        }
        assert!(
            !screen.has_row(run.as_str()),
            "{name}: a row before its shape"
        );
    }
}

/// Once its workflow has its shape, the working line names the phase its view shows at work.
/// A run that is its task's Claude Code workflow names that workflow's phase, Build while its
/// first agent builds, and never the plan it carries out, Work and Merge, nor that plan's one
/// task. The council names its script's phases from the first report, never the Work that
/// held their place.
#[test]
fn the_working_line_names_the_phase_its_view_shows() {
    let events = record();
    let shaped = shaped_at(&events);
    let mut screen = Screen::new();
    let before: Vec<RunEvent> = events
        .iter()
        .filter(|event| event.seq < shaped)
        .cloned()
        .collect();
    screen.lines_after(&before);
    // Claude Code's first report while its first agent builds, as its stream sends it before
    // the one the record keeps.
    let mut building = events
        .iter()
        .find(|event| event.seq == shaped)
        .cloned()
        .expect("the first report");
    building.seq = before.last().map_or(shaped, |last| last.seq + 1);
    if let RunEventKind::Agent(AgentEvent::Workflow { workflow }) = &mut building.kind {
        use argonauts_core::events::session::{CliAgentState, SubagentStatus};
        workflow.status = SubagentStatus::Running;
        workflow.agents.truncate(1);
        workflow.agents[0].state = CliAgentState::Running;
    }
    let said = screen.lines_after(std::slice::from_ref(&building));
    let (_, details) = said[0].1.clone().expect("a working line");
    assert_eq!(
        details.first().map(String::as_str),
        Some("Build"),
        "{details:?}"
    );

    let after: Vec<RunEvent> = events
        .iter()
        .filter(|event| event.seq >= shaped)
        .cloned()
        .collect();
    for (seq, line) in screen.lines_after(&after) {
        let Some((verb, details)) = line else {
            continue;
        };
        assert_ne!(verb, super::cli_workflow_run::WRITING, "at seq {seq}");
        for detail in &details {
            assert!(
                !["Work", "Merge"].contains(&detail.as_str()) && !detail.ends_with(" tasks"),
                "at seq {seq}: {details:?}"
            );
        }
    }

    let events = council();
    let shaped = shaped_at(&events);
    let mut screen = Screen::new();
    let said = screen.lines_after(&events);
    let from_shape: Vec<_> = said
        .iter()
        .filter(|(seq, _)| *seq >= shaped)
        .filter_map(|(seq, line)| line.as_ref().map(|line| (seq, line)))
        .collect();
    assert!(!from_shape.is_empty());
    let (_, (_, first)) = from_shape[0];
    assert_eq!(
        first.first().map(String::as_str),
        Some("Propose"),
        "{first:?}"
    );
    for (seq, (_, details)) in from_shape {
        assert!(
            !details.iter().any(|detail| detail == "Work"),
            "at seq {seq}: {details:?}"
        );
    }
}
