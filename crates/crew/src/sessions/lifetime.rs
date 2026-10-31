//! Session lifetimes per role: `task` (fresh per task, resumed for its retries), `run` (one
//! session per role per run), and `project` (the role's main session, used only when it isn't
//! busy, and rotated when full). Blind roles, and roles whose `fresh_session` says so, are
//! always fresh. A council's seat keeps one session of its own, found by the seat alone.

use std::path::Path;

use argonauts_core::TaskId;
use argonauts_core::agents::session_runner::Access;
use argonauts_core::base::names::{ModelRef, RoleName, SeatId, SessionKey};
use argonauts_core::runs::events::session::EndReason;
use argonauts_core::settings::Settings;
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::workflows::workflow::StepRef;

use crate::engine::error::EngineResult;
use crate::sessions::main_sessions::{MainSession, MainSessionLease, MainSessions};
use crate::state::RunState;
use crate::state::sessions::SessionRecord;

/// Whether a new session starts fresh or continues an earlier one.
#[derive(Debug)]
pub enum Reuse {
    Fresh,
    /// An earlier session of this run: the task's own for its retries, or the role's for `run`.
    Resume {
        key: SessionKey,
        cli_session: String,
    },
    /// The role's main session across runs, held with its lease while this run uses it.
    /// `agent_session` is `None` when the main session was full and starts over with a
    /// fallback note.
    Main {
        lease: MainSessionLease,
        cli_session: Option<String>,
    },
}

pub fn lifetime(settings: &Settings, role: RoleName) -> SessionLifetime {
    settings
        .roles
        .get(&role)
        .and_then(|role| role.lifetime)
        .unwrap_or(settings.sessions.lifetime)
}

/// Whether every session of `role` starts fresh, never carrying on an earlier one:
/// `roles.<role>.fresh_session`, which is yes for the judge unless you say otherwise.
pub fn always_fresh(settings: &Settings, role: RoleName) -> bool {
    settings
        .roles
        .get(&role)
        .and_then(|role| role.fresh_session)
        .unwrap_or(role == RoleName::Judge)
}

/// Picks how the next session of this role, worker, and task starts, in `workdir`, or `None`
/// for a scratch folder of its own, which a session it carries on keeps. A session does one
/// thing at a time, so a busy one is never picked and the work runs in a fresh session instead.
/// One that worked in another folder isn't carried on either: a CLI such as OpenCode keeps each
/// session with the folder it began in and can't open it from another.
#[allow(clippy::too_many_arguments)]
pub fn choose(
    state: &RunState,
    settings: &Settings,
    mains: &MainSessions,
    role: RoleName,
    worker: &ModelRef,
    task: Option<&TaskId>,
    workdir: Option<&Path>,
    blind: bool,
) -> EngineResult<Reuse> {
    if blind || always_fresh(settings, role) {
        return Ok(Reuse::Fresh);
    }
    let newest = |mine: &dyn Fn(&SessionRecord) -> bool| {
        state
            .sessions
            .in_order()
            .into_iter()
            .rev()
            .find(|session| {
                session.started.role == role
                    && &session.started.worker == worker
                    && same_folder(session, workdir)
                    && mine(session)
            })
            .cloned()
    };
    let reuse = |session: Option<SessionRecord>| carry_on(session, settings);
    match lifetime(settings, role) {
        SessionLifetime::Task => Ok(match task {
            Some(task) => reuse(newest(&|session| session.task.as_ref() == Some(task))),
            None => Reuse::Fresh,
        }),
        SessionLifetime::Run | SessionLifetime::Seat => Ok(reuse(newest(&|_| true))),
        SessionLifetime::Project => {
            let Some(lease) = mains.lease(role, worker)? else {
                return Ok(Reuse::Fresh);
            };
            let cli_session = continue_main(state, settings, mains.get(role, worker));
            Ok(Reuse::Main { lease, cli_session })
        }
    }
}

/// How a council seat's next session starts: carrying on the seat's own last session, found
/// by the seat alone, never by its role or model, since two seats may run the same model in
/// the same folder. Each seat keeps one folder for the whole council, so one that worked
/// elsewhere starts fresh rather than fail to open there.
pub fn of_seat(
    state: &RunState,
    settings: &Settings,
    seat: &SeatId,
    workdir: Option<&Path>,
) -> Reuse {
    let newest = state
        .sessions
        .in_order()
        .into_iter()
        .rev()
        .find(|session| {
            session.started.seat.as_ref() == Some(seat) && same_folder(session, workdir)
        })
        .cloned();
    carry_on(newest, settings)
}

/// The last session of `seat` in the council that talked at `talk`, which a task the seat does
/// carries on. Every council numbers its seats from one, so the council tells them apart.
pub fn seat_session(state: &RunState, talk: &StepRef, seat: &SeatId) -> Option<SessionKey> {
    state
        .sessions
        .in_order()
        .into_iter()
        .rev()
        .find(|session| {
            session.started.seat.as_ref() == Some(seat)
                && session.started.workflow_step.as_ref() == Some(talk)
        })
        .map(|session| session.key.clone())
}

/// Resumes `session` when it ended with a CLI session to carry on. A full one rotates, so the
/// next starts fresh with a fallback note.
fn carry_on(session: Option<SessionRecord>, settings: &Settings) -> Reuse {
    match session {
        Some(session) if was_full(&session, settings) => Reuse::Fresh,
        Some(SessionRecord {
            key,
            cli_session: Some(cli_session),
            end: Some(_),
            ..
        }) => Reuse::Resume { key, cli_session },
        _ => Reuse::Fresh,
    }
}

/// Whether `session` worked where the next one will: in `workdir`, or for `None` in a scratch
/// folder of its own. One whose folder isn't known counts, as before folders were kept.
fn same_folder(session: &SessionRecord, workdir: Option<&Path>) -> bool {
    match (session.workdir.as_deref(), workdir) {
        (None, _) => true,
        (Some(was), Some(next)) => was == next,
        (Some(_), None) => session.started.access == Access::NoTools,
    }
}

/// The main session's CLI session, or `None` when there is none or this run's log saw it
/// fill up, so it starts over with a fallback note.
fn continue_main(
    state: &RunState,
    settings: &Settings,
    main: Option<MainSession>,
) -> Option<String> {
    let main = main?;
    let full = state
        .sessions
        .by_key
        .values()
        .filter(|session| session.cli_session.as_ref() == Some(&main.cli_session))
        .any(|session| passed_the_cap(session, settings));
    (!full).then_some(main.cli_session)
}

/// Whether the session ended with its context full, or this run's log saw it pass the cap.
fn was_full(session: &SessionRecord, settings: &Settings) -> bool {
    let ended_full = session
        .end
        .as_ref()
        .is_some_and(|end| end.reason == EndReason::ContextFull);
    ended_full || passed_the_cap(session, settings)
}

/// Whether the log saw the session's context pass the cap, against the window it reported.
fn passed_the_cap(session: &SessionRecord, settings: &Settings) -> bool {
    session
        .context_used
        .is_some_and(|used| is_full(used, session.context_window, settings))
}

/// Past `sessions.rotate_at`, so it rotates: near the end of its window unless you set a
/// cap. `window` is the one the session last reported, held to `sessions.context`. A share
/// of a window nobody reported is never reached: the CLI compacts by itself meanwhile, and a
/// session that ends with its context full starts over either way.
pub fn is_full(context_tokens: u64, window: Option<u64>, settings: &Settings) -> bool {
    let sessions = &settings.sessions;
    let cap = sessions.rotate_at.tokens(sessions.context.window(window));
    cap.ok().flatten().is_some_and(|cap| context_tokens >= cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    use argonauts_core::settings::context::ContextSize;
    use argonauts_core::settings::roles::RoleSettings;
    use argonauts_core::settings::sessions::RotateAt;
    use time::OffsetDateTime;

    fn project_settings() -> Settings {
        let mut settings = Settings::default();
        let consultant = RoleSettings {
            lifetime: Some(SessionLifetime::Project),
            ..RoleSettings::default()
        };
        settings.roles.insert(RoleName::Consultant, consultant);
        settings
    }

    #[test]
    fn main_session_rotates_when_full() {
        let mut settings = project_settings();
        let window = Some(1_000_000);
        assert!(!is_full(899_999, window, &settings));
        assert!(is_full(900_000, window, &settings), "near its window's end");
        assert!(
            !is_full(5_000_000, None, &settings),
            "a share of a window nobody reported is never reached"
        );
        settings.sessions.context = ContextSize::Text("200k".to_owned());
        assert!(is_full(180_000, window, &settings), "your cap lowers it");
        assert!(is_full(180_000, None, &settings));
        settings.sessions.context = ContextSize::default();
        settings.sessions.rotate_at = RotateAt::Tokens(50_000);
        assert!(is_full(50_000, None, &settings));
        settings.sessions.rotate_at = RotateAt::Tokens(200_000);

        let dir = tempfile::tempdir().unwrap();
        let mains = MainSessions::open(dir.path());
        let worker = ModelRef::new("claude-code", None);
        let main = MainSession {
            role: RoleName::Consultant,
            worker: worker.clone(),
            cli_session: "cli-main".to_owned(),
            started: OffsetDateTime::UNIX_EPOCH,
            runs: 3,
        };
        mains.put(&main).unwrap();
        let settings = project_settings();
        let state = RunState::default();
        match choose(
            &state,
            &settings,
            &mains,
            RoleName::Consultant,
            &worker,
            None,
            Some(Path::new("/work")),
            false,
        ) {
            Ok(Reuse::Main {
                cli_session: Some(agent_id),
                ..
            }) => assert_eq!(agent_id, "cli-main"),
            other => panic!("{other:?}"),
        }

        // The same main session, reported full in this run's log, starts over.
        let mut state = RunState::default();
        let mut record =
            crate::state::tests::session_record("s-001", &worker, RoleName::Consultant);
        record.cli_session = Some("cli-main".to_owned());
        record.context_window = Some(200_000);
        record.context_used = Some(150_000);
        state.sessions.by_key.insert(record.key.clone(), record);
        assert_eq!(
            continue_main(&state, &settings, Some(main.clone())).as_deref(),
            Some("cli-main")
        );
        if let Some(record) = state.sessions.by_key.values_mut().next() {
            record.context_used = Some(210_000);
        }
        assert_eq!(continue_main(&state, &settings, Some(main)), None);
        assert_eq!(continue_main(&state, &settings, None), None);
    }

    #[test]
    fn fresh_session_starts_every_session_of_the_role_afresh() {
        let worker = ModelRef::new("claude-code", None);
        let task = TaskId::numbered(1);
        let mut state = RunState::default();
        let mut record = crate::state::tests::session_record("s-001", &worker, RoleName::Worker);
        record.cli_session = Some("cli-1".to_owned());
        record.task = Some(task.clone());
        record.end = Some(crate::state::tests::end(
            EndReason::Completed,
            Some("cli-1"),
        ));
        state.sessions.by_key.insert(record.key.clone(), record);
        let dir = tempfile::tempdir().unwrap();
        let mains = MainSessions::open(dir.path());
        let pick = |settings: &Settings| {
            choose(
                &state,
                settings,
                &mains,
                RoleName::Worker,
                &worker,
                Some(&task),
                Some(Path::new("/work")),
                false,
            )
            .unwrap()
        };
        let mut settings = Settings::default();
        assert!(
            matches!(pick(&settings), Reuse::Resume { .. }),
            "by default"
        );
        settings.roles.insert(
            RoleName::Worker,
            RoleSettings {
                fresh_session: Some(true),
                ..RoleSettings::default()
            },
        );
        assert!(matches!(pick(&settings), Reuse::Fresh));

        // The judge is fresh unless you say otherwise; nobody else is.
        let defaults = Settings::default();
        assert!(always_fresh(&defaults, RoleName::Judge));
        assert!(!always_fresh(&defaults, RoleName::Reviewer));
        let mut kept = Settings::default();
        kept.roles.insert(
            RoleName::Judge,
            RoleSettings {
                fresh_session: Some(false),
                ..RoleSettings::default()
            },
        );
        assert!(!always_fresh(&kept, RoleName::Judge));
    }

    /// A task's session is carried on only where it worked: a build after its prediction, which
    /// ran in a scratch folder of its own, starts fresh, as OpenCode can't open a session from
    /// another folder; a second build in the same worktree carries on the first.
    #[test]
    fn a_task_session_is_carried_on_only_in_the_folder_it_worked_in() {
        let worker = ModelRef::new("opencode", Some("ollama/qwen3-coder:30b".to_owned()));
        let task = TaskId::numbered(1);
        let tree = Path::new("/worktrees/run-1/task-001");
        let ended = |key: &str, access: Access, workdir: &Path| {
            let mut record = crate::state::tests::session_record(key, &worker, RoleName::Worker);
            record.started.access = access;
            record.cli_session = Some(format!("ses-{key}"));
            record.task = Some(task.clone());
            record.workdir = Some(workdir.to_path_buf());
            record.end = Some(crate::state::tests::end(
                EndReason::Completed,
                Some(&format!("ses-{key}")),
            ));
            record
        };
        let dir = tempfile::tempdir().unwrap();
        let mains = MainSessions::open(dir.path());
        let settings = Settings::default();
        let pick = |state: &RunState, workdir: Option<&Path>| {
            choose(
                state,
                &settings,
                &mains,
                RoleName::Worker,
                &worker,
                Some(&task),
                workdir,
                false,
            )
            .unwrap()
        };
        let mut state = RunState::default();
        let predicted = ended(
            "s-001",
            Access::NoTools,
            Path::new("/runs/run-1/sessions/s-001/work"),
        );
        state
            .sessions
            .by_key
            .insert(predicted.key.clone(), predicted);
        assert!(matches!(pick(&state, Some(tree)), Reuse::Fresh));
        // Another prediction keeps to a scratch folder too, its own carried on.
        assert!(matches!(pick(&state, None), Reuse::Resume { .. }));

        let built = ended("s-002", Access::Build, tree);
        state.sessions.by_key.insert(built.key.clone(), built);
        match pick(&state, Some(tree)) {
            Reuse::Resume { cli_session, .. } => assert_eq!(cli_session, "ses-s-002"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_busy_main_session_leaves_the_work_to_a_fresh_one() {
        let dir = tempfile::tempdir().unwrap();
        let mains = MainSessions::open(dir.path());
        let settings = project_settings();
        let worker = ModelRef::new("claude-code", None);
        let state = RunState::default();
        let held = choose(
            &state,
            &settings,
            &mains,
            RoleName::Consultant,
            &worker,
            None,
            Some(Path::new("/work")),
            false,
        );
        assert!(matches!(held, Ok(Reuse::Main { .. })));
        let second = choose(
            &state,
            &settings,
            &mains,
            RoleName::Consultant,
            &worker,
            None,
            Some(Path::new("/work")),
            false,
        );
        assert!(matches!(second, Ok(Reuse::Fresh)), "{second:?}");
    }

    #[test]
    fn a_full_task_session_rotates_instead_of_resuming() {
        let dir = tempfile::tempdir().unwrap();
        let mains = MainSessions::open(dir.path());
        let settings = Settings::default();
        let worker = ModelRef::new("codex", None);
        let task = TaskId::numbered(1);
        let mut record = crate::state::tests::session_record("s-001", &worker, RoleName::Worker);
        record.task = Some(task.clone());
        record.cli_session = Some("cli-1".to_owned());
        let agent_id = Some("cli-1");
        record.end = Some(crate::state::tests::end(EndReason::Completed, agent_id));
        let mut state = RunState::default();
        state
            .sessions
            .by_key
            .insert(record.key.clone(), record.clone());
        let pick = |state: &RunState| {
            choose(
                state,
                &settings,
                &mains,
                RoleName::Worker,
                &worker,
                Some(&task),
                Some(Path::new("/work")),
                false,
            )
        };
        assert!(matches!(pick(&state), Ok(Reuse::Resume { .. })));

        record.end = Some(crate::state::tests::end(EndReason::ContextFull, agent_id));
        state
            .sessions
            .by_key
            .insert(record.key.clone(), record.clone());
        assert!(matches!(pick(&state), Ok(Reuse::Fresh)));

        record.end = Some(crate::state::tests::end(EndReason::Stopped, agent_id));
        record.context_window = Some(200_000);
        record.context_used = Some(250_000);
        state.sessions.by_key.insert(record.key.clone(), record);
        assert!(matches!(pick(&state), Ok(Reuse::Fresh)));
    }

    /// Two seats of one model in one folder are two sessions: each carries on its own, never
    /// the other's, whatever role or lifetime the model has.
    #[test]
    fn a_seat_carries_on_its_own_session_and_never_another_seats() {
        let worker = ModelRef::new("claude-code", Some("opus".to_owned()));
        let base = Path::new("/worktrees/run-1/base");
        let talk_at = |at: u32| StepRef {
            workflow: argonauts_core::base::names::WorkflowId::numbered(1),
            phase: 1,
            path: vec![at],
        };
        let talk = talk_at(0);
        let ended = |key: &str, seat: u32| {
            let mut record = crate::state::tests::session_record(key, &worker, RoleName::Worker);
            record.started.seat = Some(SeatId::numbered(seat));
            record.started.workflow_step = Some(talk.clone());
            record.started.lifetime = SessionLifetime::Seat;
            record.cli_session = Some(format!("ses-{key}"));
            record.workdir = Some(base.to_path_buf());
            record.end = Some(crate::state::tests::end(
                EndReason::Completed,
                Some(&format!("ses-{key}")),
            ));
            record
        };
        let mut state = RunState::default();
        for record in [ended("s-001", 1), ended("s-002", 2)] {
            state.sessions.by_key.insert(record.key.clone(), record);
        }
        let settings = Settings::default();
        let resumed = |state: &RunState, seat: u32| match of_seat(
            state,
            &settings,
            &SeatId::numbered(seat),
            Some(base),
        ) {
            Reuse::Resume { cli_session, .. } => cli_session,
            other => panic!("{other:?}"),
        };
        assert_eq!(resumed(&state, 1), "ses-s-001");
        assert_eq!(resumed(&state, 2), "ses-s-002");
        assert!(matches!(
            of_seat(&state, &settings, &SeatId::numbered(3), Some(base)),
            Reuse::Fresh
        ));
        assert_eq!(
            seat_session(&state, &talk, &SeatId::numbered(2)).map(|key| key.as_str().to_owned()),
            Some("s-002".to_owned())
        );
        // A later council numbers its seats from one too, and its seat 2 is another seat.
        let mut later = ended("s-009", 2);
        later.started.workflow_step = Some(talk_at(1));
        state.sessions.by_key.insert(later.key.clone(), later);
        assert_eq!(
            seat_session(&state, &talk, &SeatId::numbered(2)).map(|key| key.as_str().to_owned()),
            Some("s-002".to_owned())
        );

        // A session of the same model with no seat is never a seat's.
        let mut plain = crate::state::tests::session_record("s-003", &worker, RoleName::Worker);
        plain.cli_session = Some("ses-s-003".to_owned());
        plain.workdir = Some(base.to_path_buf());
        plain.end = Some(crate::state::tests::end(
            EndReason::Completed,
            Some("ses-s-003"),
        ));
        state.sessions.by_key.insert(plain.key.clone(), plain);
        assert_eq!(resumed(&state, 1), "ses-s-001");
    }

    #[test]
    fn blind_roles_are_always_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let mains = MainSessions::open(dir.path());
        let settings = project_settings();
        let worker = ModelRef::new("claude-code", None);
        let chosen = choose(
            &RunState::default(),
            &settings,
            &mains,
            RoleName::Consultant,
            &worker,
            None,
            Some(Path::new("/work")),
            true,
        );
        assert!(matches!(chosen, Ok(Reuse::Fresh)));
    }
}
