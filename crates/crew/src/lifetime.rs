//! Session lifetimes per role: `task` (fresh per task, resumed for its retries), `run` (one
//! session per role per run), and `project` (the role's main session, used only when it isn't
//! busy, and rotated when full). Blind roles are always fresh.

use argonauts_core::TaskId;
use argonauts_core::events::session::EndReason;
use argonauts_core::main_sessions::{MainSession, MainSessionLease, MainSessions};
use argonauts_core::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::settings::Settings;
use argonauts_core::settings::sessions::SessionLifetime;

use crate::error::EngineResult;
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

/// Picks how the next session of this role, worker, and task starts. A session does one thing
/// at a time, so a busy one is never picked and the work runs in a fresh session instead.
pub fn choose(
    state: &RunState,
    settings: &Settings,
    mains: &MainSessions,
    role: RoleName,
    worker: &ModelRef,
    task: Option<&TaskId>,
    blind: bool,
) -> EngineResult<Reuse> {
    if blind {
        return Ok(Reuse::Fresh);
    }
    let newest = |mine: &dyn Fn(&SessionRecord) -> bool| {
        state
            .sessions
            .in_order()
            .into_iter()
            .rev()
            .find(|session| {
                session.started.role == role && &session.started.worker == worker && mine(session)
            })
            .cloned()
    };
    // A full session rotates, so the next one starts fresh with a fallback note.
    let reuse = |session: Option<SessionRecord>| match session {
        Some(session) if was_full(&session, settings) => Reuse::Fresh,
        Some(SessionRecord {
            key,
            cli_session: Some(cli_session),
            end: Some(_),
            ..
        }) => Reuse::Resume { key, cli_session },
        _ => Reuse::Fresh,
    };
    match lifetime(settings, role) {
        SessionLifetime::Task => Ok(match task {
            Some(task) => reuse(newest(&|session| session.task.as_ref() == Some(task))),
            None => Reuse::Fresh,
        }),
        SessionLifetime::Run => Ok(reuse(newest(&|_| true))),
        SessionLifetime::Project => {
            let Some(lease) = mains.lease(role, worker)? else {
                return Ok(Reuse::Fresh);
            };
            let cli_session = continue_main(state, settings, mains.get(role, worker));
            Ok(Reuse::Main { lease, cli_session })
        }
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
            true,
        );
        assert!(matches!(chosen, Ok(Reuse::Fresh)));
    }
}
