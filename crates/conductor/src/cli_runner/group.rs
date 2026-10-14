//! The agent's process group while it runs: recorded in the session's `process.json`, listed in
//! `~/.argonauts/agents/`, guarded against the run host's death, and watched for CPU. Before
//! the runner signals it, its whole tree is noted, so releasing it kills whatever is left, in
//! the group or out of it, even what its dead parents can no longer reach. Everything that
//! scans the process table or forks runs off the async runtime.

use std::io;
use std::sync::Arc;

use argonauts_core::StopSignal;
use argonauts_core::agents::AgentGroup;
use argonauts_core::names::SessionKey;

use super::context::Context;
use super::outbox::Outbox;
use crate::agents::{self, AgentEntry, Registration};
use crate::cpu::{CpuSampler, CpuWatch};
use crate::process::{AgentProcess, signal_group, write_process_file};
use crate::reaper::{self, ExitGuard, TreeSnapshot};

#[derive(Debug)]
pub struct Group {
    pgid: u32,
    /// The id in the launch's `ARGONAUTS_LAUNCH`, unique across runs and run hosts.
    launch: String,
    /// `None` when the head exited before it could be recorded, so there was nothing left to
    /// list or guard.
    held: Option<Held>,
}

#[derive(Debug)]
struct Held {
    /// Finds children that left the group, through the head's session and descendants.
    record: AgentGroup,
    registration: Option<Registration>,
    _guard: ExitGuard,
    cpu: CpuWatch,
    /// Every process seen in the tree before a signal, with its start time.
    seen: TreeSnapshot,
}

impl Group {
    /// Fails when a running group can't be recorded, listed, or guarded, since then a crash
    /// could leave it running unseen. It kills the agent's tree before it fails.
    pub async fn register(
        ctx: &Arc<Context>,
        process: &AgentProcess,
        out: &Outbox,
    ) -> Result<Self, String> {
        let pgid = process.id();
        let launch = process.launch_id().to_owned();
        let key = ctx.spec.key.clone();
        let record = match process.record() {
            Ok(record) => record,
            // A quick agent can finish before its start time is read.
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    pgid,
                    launch,
                    held: None,
                });
            }
            Err(err) => {
                let problems = blocking(move || kill_tree(&key, pgid, None, &launch)).await;
                notify(out, problems);
                return Err(format!("could not read the agent's process record: {err}"));
            }
        };
        let (hold_ctx, hold_launch) = (ctx.clone(), launch.clone());
        // On macOS the guard forks this whole process.
        let held = blocking(move || hold(&hold_ctx, &record, &hold_launch))
            .await
            .map_err(|err| format!("could not record the agent's process group: {err}"))
            .and_then(|held| held);
        match held {
            Ok(held) => Ok(Self {
                pgid,
                launch,
                held: Some(held),
            }),
            Err(message) => {
                let problems =
                    blocking(move || kill_tree(&key, pgid, Some(&record), &launch)).await;
                notify(out, problems);
                Err(message)
            }
        }
    }

    /// When the process tree last used CPU, from the shared sampler.
    pub fn last_busy(&self) -> Option<std::time::Instant> {
        self.held.as_ref().and_then(|held| held.cpu.last_busy())
    }

    /// Notes the tree as it is now. Call it before signalling the group: once a signal kills a
    /// parent, a child that started a session of its own can't be traced to the agent anymore.
    pub async fn note_tree(&mut self) -> Result<(), String> {
        let Some(held) = self.held.as_mut() else {
            return Ok(());
        };
        let record = held.record;
        let seen = blocking(move || reaper::snapshot_tree(&record))
            .await
            .and_then(|seen| seen)
            .map_err(|err| format!("could not read the agent's process tree: {err}"))?;
        held.seen.extend(seen);
        Ok(())
    }

    /// Kills what's left of the group and anything that left it, found through the head's
    /// session and descendants, the tree noted before each signal, or the launch's marker, then
    /// removes the group's listing.
    pub async fn release(self, key: &SessionKey, out: &Outbox) {
        let key = key.clone();
        let problems = blocking(move || self.sweep(&key)).await;
        notify(out, problems);
    }

    /// Blocks while it reads the process table.
    fn sweep(mut self, key: &SessionKey) -> Vec<String> {
        let held = self.held.as_ref();
        let mut problems = kill_tree(key, self.pgid, held.map(|held| &held.record), &self.launch);
        if let Some(held) = held {
            match reaper::kill_noted(&held.seen, &held.record) {
                Ok(0) => {}
                Ok(killed) => {
                    tracing::info!("killed {killed} processes session {key}'s tree left behind")
                }
                Err(err) => problems.push(format!(
                    "could not sweep the processes session {key}'s tree left behind: {err}"
                )),
            }
        }
        if let Some(registration) = self.held.as_mut().and_then(|held| held.registration.take())
            && let Err(err) = registration.remove()
        {
            problems.push(format!(
                "could not remove the agent's process listing: {err}"
            ));
        }
        problems
    }
}

fn hold(ctx: &Context, record: &AgentGroup, launch: &str) -> Result<Held, String> {
    write_process_file(&ctx.spec.session_dir, record)
        .map_err(|err| format!("could not write the session's process.json: {err}"))?;
    let entry = AgentEntry {
        group: *record,
        session: ctx.spec.key.to_string(),
        agent_id: ctx.adapter.id().to_owned(),
    };
    let registration = agents::register(&ctx.home, &entry)
        .map_err(|err| format!("could not list the agent's process group: {err}"))?;
    let guard = reaper::guard_group(record, launch)
        .map_err(|err| format!("could not tie the agent's process group to this process: {err}"))?;
    Ok(Held {
        record: *record,
        registration: Some(registration),
        _guard: guard,
        cpu: CpuSampler::shared().watch(record.pgid),
        seen: TreeSnapshot::new(),
    })
}

/// The session and descendants reach what left the group while the head is recorded. The
/// launch's marker finds the rest where the OS shows other processes' environments (Linux).
/// Returns what went wrong. Blocks while it reads the process table.
fn kill_tree(
    key: &SessionKey,
    pgid: u32,
    record: Option<&AgentGroup>,
    launch: &str,
) -> Vec<String> {
    let mut problems = Vec::new();
    match record {
        Some(record) => match reaper::kill_process_tree(record) {
            Ok(0) => {}
            Ok(killed) => {
                tracing::info!("killed {killed} processes that left session {key}'s group")
            }
            Err(err) => problems.push(format!(
                "could not sweep session {key}'s process tree: {err}"
            )),
        },
        // The head was gone before it was recorded, so a process with its pid now is another
        // one, maybe leading a group with the same id.
        None if crate::liveness::start_time(pgid).is_none() => {
            if let Err(err) = signal_group(pgid, StopSignal::Kill) {
                problems.push(format!("could not kill the agent's process group: {err}"));
            }
        }
        None => {}
    }
    match reaper::kill_launch_leftovers(launch) {
        Ok(0) => {}
        Ok(killed) => tracing::info!("killed {killed} leftover processes of session {key}"),
        Err(err) => problems.push(format!(
            "could not sweep session {key}'s leftover processes: {err}"
        )),
    }
    problems
}

fn notify(out: &Outbox, problems: io::Result<Vec<String>>) {
    let problems = problems
        .unwrap_or_else(|err| vec![format!("could not sweep the agent's processes: {err}")]);
    for problem in problems {
        out.notice(problem);
    }
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> io::Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(io::Error::other)
}
