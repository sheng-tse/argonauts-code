//! The `@` list: workers in the run, tasks, roles, follow numbers, and `all`, from the run on
//! screen. A name that matches more than one session asks which.

use argonauts_core::app::UiCommand;
use argonauts_core::events::task::TaskState;
use argonauts_core::input::{MentionTarget, ParsedInput, parse_input};
use argonauts_core::names::{ModelRef, RoleName};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::bottom::MenuItem;
use crate::state::{AppState, RunState, SessionState};
use crate::text::{sanitize_line, truncate_line};
use crate::theme::Role;
use crate::views::{View, ViewCtx, ViewOutcome, list};

/// A council's seats for the `@` list, each by the name the conversation gives it ("Opus 5.5 ·
/// 2"), in seat order.
pub fn seats(faces: &crate::council::Faces) -> Vec<MenuItem> {
    faces
        .iter()
        .map(|(_, face)| MenuItem::new(face.label(), "a seat of the council").with_kind("seat"))
        .collect()
}

/// The seats first, then everyone else not named the same, as the `@` list reads while a
/// council talks.
pub fn seats_first(seats: Vec<MenuItem>, rest: Vec<MenuItem>) -> Vec<MenuItem> {
    let mut items = seats;
    for item in rest {
        if !items.iter().any(|seat| seat.name == item.name) {
            items.push(item);
        }
    }
    items
}

/// Everyone you can mention in the attached run, in the list's order: workers, tasks, roles,
/// follow numbers, then `all`. Empty with no run on screen, since a mention needs one.
pub fn candidates(state: &AppState) -> Vec<MenuItem> {
    let Some(run) = state.attached_run() else {
        return Vec::new();
    };
    let live: Vec<&SessionState> = run.sessions_in_order().filter(|s| s.is_running()).collect();
    let mut items = Vec::new();

    let mut workers: Vec<&str> = Vec::new();
    for session in &live {
        if !workers.contains(&session.worker.agent.as_str()) {
            workers.push(&session.worker.agent);
        }
    }
    for worker in workers {
        let sessions: Vec<&&SessionState> =
            live.iter().filter(|s| s.worker.agent == worker).collect();
        let description = match sessions.as_slice() {
            [one] => session_summary(one),
            many => format!(
                "{} sessions: {}",
                many.len(),
                many.iter()
                    .map(|s| session_place(s))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        items.push(MenuItem::new(worker, description).with_kind("agent"));
    }

    for task in run.tasks.values().filter(|task| !finished(task.state)) {
        items.push(MenuItem::new(task.id.as_str(), task.title.clone()).with_kind("task"));
    }

    let mut roles: Vec<RoleName> = Vec::new();
    for session in &live {
        if !roles.contains(&session.role) {
            roles.push(session.role);
        }
    }
    for role in roles {
        let who: Vec<String> = live
            .iter()
            .filter(|s| s.role == role)
            .map(|s| worker_place(s))
            .collect();
        items.push(MenuItem::new(role.as_str(), who.join(", ")).with_kind("role"));
    }

    let mut numbered: Vec<&&SessionState> = live.iter().filter(|s| s.follow.is_some()).collect();
    numbered.sort_by_key(|s| s.follow);
    for session in numbered {
        if let Some(key) = session.follow {
            items.push(MenuItem::new(key.to_string(), worker_place(session)).with_kind("follow"));
        }
    }

    items.push(MenuItem::new("all", "Every agent in this run").with_kind("run"));
    items
}

fn finished(state: Option<TaskState>) -> bool {
    matches!(state, Some(TaskState::Merged | TaskState::Cancelled))
}

/// `worker on task-002`, or the role alone.
fn session_place(session: &SessionState) -> String {
    match &session.task {
        Some(task) => format!("{} on {task}", session.role.as_str()),
        None => session.role.as_str().to_owned(),
    }
}

/// `opus · worker on task-002`.
fn session_summary(session: &SessionState) -> String {
    match &session.worker.model {
        Some(model) => format!("{model} · {}", session_place(session)),
        None => session_place(session),
    }
}

/// `codex on task-003`, or the worker alone.
fn worker_place(session: &SessionState) -> String {
    match &session.task {
        Some(task) => format!("{} on {task}", session.worker),
        None => session.worker.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Resolved {
    One(MentionTarget),
    /// Several sessions answer to the name; a picker asks which.
    Ambiguous(Vec<(String, MentionTarget)>),
    Unknown,
}

/// `name` as typed after the `@`.
pub fn resolve(name: &str, state: &AppState) -> Resolved {
    match parse_input(&format!("@{name}")) {
        Ok(ParsedInput::Mention { target, .. }) => resolve_target(&target, state),
        _ => Resolved::Unknown,
    }
}

/// A worker name matches every session of that worker, so with more than one it's ambiguous:
/// the picker offers each session and all of them. A task or follow number must be in the run.
/// A role or a worker nobody runs yet is fine, since the message waits for its next turn.
pub fn resolve_target(target: &MentionTarget, state: &AppState) -> Resolved {
    let Some(run) = state.attached_run() else {
        return Resolved::Unknown;
    };
    match target {
        MentionTarget::Worker(worker) => resolve_worker(target, worker, run),
        MentionTarget::Task(task) => {
            let known = run.tasks.contains_key(task)
                || run.sessions.values().any(|s| s.task.as_ref() == Some(task));
            if known {
                Resolved::One(target.clone())
            } else {
                Resolved::Unknown
            }
        }
        MentionTarget::Number(key) => {
            if run.sessions.values().any(|s| s.follow == Some(*key)) {
                Resolved::One(target.clone())
            } else {
                Resolved::Unknown
            }
        }
        MentionTarget::Role(_) | MentionTarget::All => Resolved::One(target.clone()),
    }
}

fn resolve_worker(target: &MentionTarget, worker: &ModelRef, run: &RunState) -> Resolved {
    let sessions: Vec<&SessionState> = run
        .sessions_in_order()
        .filter(|s| s.is_running() && answers_to(worker, &s.worker))
        .collect();
    if sessions.len() < 2 {
        return Resolved::One(target.clone());
    }
    let mut choices = vec![(
        format!("All {} {worker} sessions", sessions.len()),
        target.clone(),
    )];
    for session in sessions {
        if let Some(target) = session_target(session) {
            choices.push((
                format!("{} · {}", session.worker, session_place(session)),
                target,
            ));
        }
    }
    if choices.len() < 2 {
        return Resolved::One(target.clone());
    }
    Resolved::Ambiguous(choices)
}

fn answers_to(name: &ModelRef, worker: &ModelRef) -> bool {
    name.agent == worker.agent && (name.model.is_none() || name.model == worker.model)
}

/// A target that reaches this one session: its follow key, or the task it owns.
fn session_target(session: &SessionState) -> Option<MentionTarget> {
    if let Some(key) = session.follow {
        return Some(MentionTarget::Number(key));
    }
    let task = session
        .task
        .clone()
        .filter(|_| session.role == RoleName::Worker)?;
    Some(MentionTarget::Task(task))
}

/// Asks which session a mention meant: numbered rows with a `❯` pointer, as a question asks.
#[derive(Debug, Clone, PartialEq)]
pub struct MentionPicker {
    /// As typed after the `@`.
    pub name: String,
    pub text: String,
    pub choices: Vec<(String, MentionTarget)>,
    pub selected: usize,
}

impl MentionPicker {
    pub fn new(
        name: impl Into<String>,
        text: impl Into<String>,
        choices: Vec<(String, MentionTarget)>,
    ) -> Self {
        Self {
            name: name.into(),
            text: text.into(),
            choices,
            selected: 0,
        }
    }

    pub fn lines(&self, ctx: &ViewCtx<'_>, width: u16) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let dim = theme.style(Role::Dim);
        let name = sanitize_line(&self.name).into_owned();
        let text = sanitize_line(&self.text).into_owned();
        let mut lines = vec![
            Line::styled(
                format!("@{name} matches more than one agent"),
                theme.style(Role::Question),
            ),
            Line::styled(format!("Who gets \"{text}\"?"), dim),
            Line::default(),
        ];
        for (index, (label, _)) in self.choices.iter().enumerate() {
            let pointer = if index == self.selected {
                theme.glyphs.prompt
            } else {
                " "
            };
            let style = if index == self.selected {
                theme.style(Role::Question)
            } else {
                theme.style(Role::Text)
            };
            let label = sanitize_line(label).into_owned();
            lines.push(Line::from(vec![
                Span::styled(format!("{pointer} {}. ", index + 1), style),
                Span::styled(label, style),
            ]));
        }
        lines.push(Line::default());
        lines.push(Line::styled("Enter to send · Esc to cancel", dim));
        lines
            .iter()
            .map(|line| truncate_line(line, usize::from(width)))
            .collect()
    }

    fn send(&self, index: usize) -> ViewOutcome {
        match self.choices.get(index) {
            Some((_, target)) => ViewOutcome::Send(UiCommand::Mention {
                target: target.clone(),
                text: self.text.clone(),
            }),
            None => ViewOutcome::Consumed,
        }
    }
}

impl View for MentionPicker {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        for (row, line) in self.lines(ctx, area.width).iter().enumerate() {
            let Ok(row) = u16::try_from(row) else { break };
            if row >= area.height {
                break;
            }
            buf.set_line(area.x, area.y + row, line, area.width);
        }
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let count = self.choices.len();
        if count == 0 {
            return ViewOutcome::Close;
        }
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.checked_sub(1).unwrap_or(count - 1);
                ViewOutcome::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1) % count;
                ViewOutcome::Consumed
            }
            KeyCode::Home => {
                self.selected = 0;
                ViewOutcome::Consumed
            }
            KeyCode::End => {
                self.selected = count - 1;
                ViewOutcome::Consumed
            }
            KeyCode::Enter => self.send(self.selected),
            KeyCode::Char(c @ '1'..='9') => {
                let index = usize::from(c as u8 - b'1');
                if index < count {
                    self.selected = index;
                    self.send(index)
                } else {
                    ViewOutcome::Consumed
                }
            }
            KeyCode::Esc => ViewOutcome::Close,
            _ => ViewOutcome::Ignored,
        }
    }
}

/// A run on screen with three live agents, two of them codex, for tests here and in the input
/// router.
#[cfg(test)]
pub(crate) mod fixture {
    use argonauts_core::app::AppMode;
    use argonauts_core::events::task::TaskState;
    use argonauts_core::names::{ModelRef, RoleName, SessionKey};
    use argonauts_core::{RunId, TaskId};
    use time::OffsetDateTime;

    use crate::state::{AppState, Doing, RunState, SessionState, SessionStatus, TaskRow};

    pub fn run_id() -> RunId {
        RunId::new("run-20260930-101500-3fa2").unwrap()
    }

    pub fn session(
        n: u32,
        worker: &str,
        model: Option<&str>,
        role: RoleName,
        task: Option<&str>,
        follow: Option<u8>,
    ) -> SessionState {
        SessionState {
            key: SessionKey::numbered(n),
            follow,
            worker: ModelRef::new(worker, model.map(str::to_owned)),
            model_assumed: false,
            model_guessed: false,
            agent_id: worker.to_owned(),
            role,
            step: None,
            task: task.map(|task| TaskId::new(task).unwrap()),
            workflow_step: None,
            label: None,
            status: SessionStatus::Working,
            started: OffsetDateTime::UNIX_EPOCH,
            ended: None,
            last_event: OffsetDateTime::UNIX_EPOCH,
            tokens_in: 0,
            tokens_out: 0,
            cost_usd: None,
            billing: None,
            context: None,
            context_window: None,
            rotations: 0,
            todos: Vec::new(),
            tools: Vec::new(),
            messages: Vec::new(),
            streaming: String::new(),
            diffs: Vec::new(),
            patches: Vec::new(),
            subagents: Vec::new(),
            workflows: Vec::new(),
            inputs: Vec::new(),
            queued: Vec::new(),
            retry: None,
            fallback_for: None,
            end: None,
            unknown: Vec::new(),
            doing: Doing::Nothing,
            turn_over: false,
            stalled: false,
            cli_session: None,
            sent_back: false,
            seat: None,
        }
    }

    fn task(id: &str, title: &str, state: TaskState) -> TaskRow {
        TaskRow {
            id: TaskId::new(id).unwrap(),
            title: title.to_owned(),
            state: Some(state),
            worker: None,
            try_number: 1,
            diff: None,
            fallback_for: None,
            debt: Vec::new(),
            checkpoints: 0,
            rotations: 0,
            last_check: None,
            reviews: 0,
            waiting_on_agent: None,
            item: None,
            way: None,
            todos: None,
            reviewers: 0,
        }
    }

    pub fn working_state() -> AppState {
        let mut run = RunState::new(run_id());
        let sessions = [
            session(
                1,
                "claude-code",
                Some("opus"),
                RoleName::Worker,
                Some("task-002"),
                Some(1),
            ),
            session(
                2,
                "codex",
                None,
                RoleName::Worker,
                Some("task-003"),
                Some(2),
            ),
            session(
                3,
                "codex",
                None,
                RoleName::Reviewer,
                Some("task-002"),
                Some(3),
            ),
        ];
        let mut done = session(4, "agy", None, RoleName::Critic, None, None);
        done.status = SessionStatus::Done;
        for session in sessions.into_iter().chain([done]) {
            run.session_order.push(session.key.clone());
            run.sessions.insert(session.key.clone(), session);
        }
        for task in [
            task("task-002", "Add the JSON writer", TaskState::Running),
            task("task-003", "Fix the flaky export test", TaskState::InReview),
            task("task-001", "Set up the crate", TaskState::Merged),
        ] {
            run.tasks.insert(task.id.clone(), task);
        }
        let mut state = AppState::default();
        state.attached = Some(run_id());
        state.mode = Some(AppMode::Working { run: run_id() });
        state.runs.insert(run_id(), run);
        state
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_councils_seats_come_first_in_the_at_list() {
        use crate::council::{Face, Faces};
        use argonauts_core::logos::Logo;
        use argonauts_core::names::SeatId;
        let faces = Faces::new([
            (
                SeatId::numbered(1),
                Face::new("Opus 5.5", "claude-code", Logo::dot("claude-code")).numbered(1),
            ),
            (
                SeatId::numbered(2),
                Face::new("5.6 Luna", "codex", Logo::dot("codex")),
            ),
        ]);
        let items = seats_first(
            seats(&faces),
            vec![
                MenuItem::new("codex", "1 session").with_kind("agent"),
                MenuItem::new("5.6 Luna", "the same seat"),
            ],
        );
        let names: Vec<&str> = items.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(names, ["Opus 5.5 · 1", "5.6 Luna", "codex"]);
        assert_eq!(items[0].kind.as_deref(), Some("seat"));
    }

    use super::*;

    #[test]
    fn the_ambiguous_name_picker_takes_vim_keys_when_they_are_on() {
        let mut state = fixture::working_state();
        let Resolved::Ambiguous(choices) = resolve("codex", &state) else {
            panic!("codex answers to two sessions");
        };
        let last = choices.len() - 1;
        let theme = crate::theme::Theme::dark();
        let press = |picker: &mut MentionPicker, c: char, state: &AppState| {
            let ctx = ViewCtx {
                theme: &theme,
                state,
                now: std::time::Instant::now(),
            };
            picker.handle_key(KeyEvent::from(KeyCode::Char(c)), &ctx)
        };

        // `j` and `k` always moved here; `h`, `l`, `g`, and `G` are new with vim keys.
        state.ui = list::keys_state(false).ui;
        let mut picker = MentionPicker::new("codex", "rebase first", choices.clone());
        press(&mut picker, 'j', &state);
        assert_eq!(picker.selected, 1);
        for c in ['h', 'l', 'g', 'G'] {
            assert!(
                matches!(press(&mut picker, c, &state), ViewOutcome::Ignored),
                "{c}"
            );
        }

        state.ui = list::keys_state(true).ui;
        let mut picker = MentionPicker::new("codex", "rebase first", choices);
        press(&mut picker, 'G', &state);
        assert_eq!(picker.selected, last);
        press(&mut picker, 'g', &state);
        assert_eq!(picker.selected, 0);
        press(&mut picker, 'j', &state);
        assert!(matches!(
            press(&mut picker, 'l', &state),
            ViewOutcome::Send(UiCommand::Mention {
                target: MentionTarget::Number(2),
                ..
            })
        ));
        assert!(matches!(
            press(&mut picker, 'h', &state),
            ViewOutcome::Close
        ));
    }

    #[test]
    fn mention_candidates_from_run() {
        let state = fixture::working_state();
        let rows: Vec<(String, String, String)> = candidates(&state)
            .into_iter()
            .map(|item| (item.name, item.description, item.kind.unwrap_or_default()))
            .collect();
        let row = |name: &str, description: &str, kind: &str| {
            (name.to_owned(), description.to_owned(), kind.to_owned())
        };
        assert_eq!(
            rows,
            [
                row("claude-code", "opus · worker on task-002", "agent"),
                row(
                    "codex",
                    "2 sessions: worker on task-003, reviewer on task-002",
                    "agent"
                ),
                row("task-002", "Add the JSON writer", "task"),
                row("task-003", "Fix the flaky export test", "task"),
                row(
                    "worker",
                    "claude-code/opus on task-002, codex on task-003",
                    "role"
                ),
                row("reviewer", "codex on task-002", "role"),
                row("1", "claude-code/opus on task-002", "follow"),
                row("2", "codex on task-003", "follow"),
                row("3", "codex on task-002", "follow"),
                row("all", "Every agent in this run", "run"),
            ]
        );
        assert!(candidates(&AppState::default()).is_empty());
    }

    #[test]
    fn ambiguous_mention_lists_sessions() {
        let state = fixture::working_state();
        let codex = ModelRef::new("codex", None);
        let Resolved::Ambiguous(choices) = resolve("codex", &state) else {
            panic!("codex runs twice");
        };
        assert_eq!(
            choices,
            [
                (
                    "All 2 codex sessions".to_owned(),
                    MentionTarget::Worker(codex)
                ),
                (
                    "codex · worker on task-003".to_owned(),
                    MentionTarget::Number(2)
                ),
                (
                    "codex · reviewer on task-002".to_owned(),
                    MentionTarget::Number(3)
                ),
            ]
        );
        assert_eq!(
            resolve("claude-code", &state),
            Resolved::One(MentionTarget::Worker(ModelRef::new("claude-code", None)))
        );
        assert_eq!(resolve("9", &state), Resolved::Unknown);
        assert_eq!(resolve("task-404", &state), Resolved::Unknown);
        assert!(matches!(resolve("reviewer", &state), Resolved::One(_)));

        let theme = crate::theme::Theme::dark();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: std::time::Instant::now(),
        };
        let mut picker = MentionPicker::new("codex", "rebase first", choices);
        let shown: Vec<String> = picker
            .lines(&ctx, 60)
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(shown[0], "@codex matches more than one agent");
        assert!(shown[3].ends_with("1. All 2 codex sessions"), "{shown:?}");
        assert!(
            shown[5].ends_with("3. codex · reviewer on task-002"),
            "{shown:?}"
        );
        picker.handle_key(KeyEvent::from(KeyCode::Down), &ctx);
        let ViewOutcome::Send(command) = picker.handle_key(KeyEvent::from(KeyCode::Enter), &ctx)
        else {
            panic!("Enter sends");
        };
        assert_eq!(
            command,
            UiCommand::Mention {
                target: MentionTarget::Number(2),
                text: "rebase first".into(),
            }
        );
        assert!(matches!(
            picker.handle_key(KeyEvent::from(KeyCode::Char('3')), &ctx),
            ViewOutcome::Send(UiCommand::Mention {
                target: MentionTarget::Number(3),
                ..
            })
        ));
        assert!(matches!(
            picker.handle_key(KeyEvent::from(KeyCode::Esc), &ctx),
            ViewOutcome::Close
        ));
    }
}
