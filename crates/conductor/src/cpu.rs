//! One CPU sampler per run host process. It walks every watched process tree every 30 seconds,
//! so the inactivity watchdogs of a crew of many agents share one walk.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use nix::unistd::Pid;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

pub const SAMPLE_EVERY: Duration = Duration::from_secs(30);

/// A tree counts as busy when it used at least 1% of one core since the previous sample, and at
/// least this many milliseconds of CPU. An idle Node process spends a few milliseconds on its
/// timers in 30 seconds.
const BUSY_SHARE_DIVISOR: u128 = 100;
const BUSY_FLOOR_MS: u64 = 50;

/// `Default` makes a sampler that samples only when [`CpuSampler::sample`] is called.
#[derive(Debug, Default)]
pub struct CpuSampler {
    state: Arc<Mutex<Watched>>,
}

/// Watches one process group's tree. Dropping it stops the watch.
#[derive(Debug)]
pub struct CpuWatch {
    pgid: u32,
    state: Arc<Mutex<Watched>>,
}

#[derive(Debug, Default)]
struct Watched {
    groups: HashMap<u32, Tree>,
}

#[derive(Debug)]
struct Tree {
    watches: usize,
    sampled_at: Instant,
    last_busy: Option<Instant>,
    /// Each process's start time and CPU time in milliseconds at the last sample. The start
    /// time tells a reused pid from the process seen before.
    cpu_ms: HashMap<u32, (u64, u64)>,
}

impl CpuSampler {
    /// The process-wide sampler, started on first use.
    pub fn shared() -> Arc<CpuSampler> {
        static SHARED: OnceLock<Arc<CpuSampler>> = OnceLock::new();
        SHARED.get_or_init(|| Self::start(SAMPLE_EVERY)).clone()
    }

    /// A sampler with a thread of its own that samples every `every`. The thread ends once the
    /// sampler and all its watches are dropped.
    pub fn start(every: Duration) -> Arc<CpuSampler> {
        let sampler = Arc::new(CpuSampler::default());
        let state = Arc::downgrade(&sampler.state);
        let spawned = std::thread::Builder::new()
            .name("argonauts-cpu".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(every);
                    let Some(state) = state.upgrade() else {
                        return;
                    };
                    sample_into(&state, Instant::now());
                }
            });
        if let Err(err) = spawned {
            // Inactivity then rests on events and output alone.
            tracing::warn!("could not start the CPU sampler: {err}");
        }
        sampler
    }

    pub fn watch(&self, pgid: u32) -> CpuWatch {
        lock(&self.state)
            .groups
            .entry(pgid)
            .or_insert_with(|| Tree {
                watches: 0,
                sampled_at: Instant::now(),
                last_busy: None,
                cpu_ms: HashMap::new(),
            })
            .watches += 1;
        CpuWatch {
            pgid,
            state: Arc::clone(&self.state),
        }
    }

    /// Samples every watched tree now. Blocks while it reads the process table.
    pub fn sample(&self) {
        sample_into(&self.state, Instant::now());
    }
}

impl CpuWatch {
    pub fn pgid(&self) -> u32 {
        self.pgid
    }

    /// When the tree last used CPU, as of the latest sample. `None` until a sample saw it busy.
    pub fn last_busy(&self) -> Option<Instant> {
        lock(&self.state)
            .groups
            .get(&self.pgid)
            .and_then(|tree| tree.last_busy)
    }
}

impl Drop for CpuWatch {
    fn drop(&mut self) {
        let mut watched = lock(&self.state);
        if let Some(tree) = watched.groups.get_mut(&self.pgid) {
            tree.watches = tree.watches.saturating_sub(1);
            if tree.watches == 0 {
                watched.groups.remove(&self.pgid);
            }
        }
    }
}

fn lock(state: &Mutex<Watched>) -> MutexGuard<'_, Watched> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

fn sample_into(state: &Mutex<Watched>, now: Instant) {
    let groups: Vec<u32> = lock(state).groups.keys().copied().collect();
    if groups.is_empty() {
        return;
    }
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cpu(),
    );
    let table = ProcessTable::read(&system);
    let mut watched = lock(state);
    for pgid in groups {
        let Some(tree) = watched.groups.get_mut(&pgid) else {
            continue;
        };
        let mut used_ms = 0;
        let mut cpu_ms = HashMap::new();
        for pid in table.tree(pgid) {
            let Some(process) = system.process(sysinfo::Pid::from_u32(pid)) else {
                continue;
            };
            let (started, total) = (process.start_time(), process.accumulated_cpu_time());
            let before = match tree.cpu_ms.get(&pid) {
                Some(&(seen_start, seen_total)) if seen_start == started => seen_total,
                // A process new to the tree counts with all the CPU it used so far.
                _ => 0,
            };
            used_ms += total.saturating_sub(before);
            cpu_ms.insert(pid, (started, total));
        }
        tree.cpu_ms = cpu_ms;
        let elapsed_ms = now.saturating_duration_since(tree.sampled_at).as_millis();
        let busy_ms = u64::try_from(elapsed_ms / BUSY_SHARE_DIVISOR)
            .unwrap_or(u64::MAX)
            .max(BUSY_FLOOR_MS);
        tree.sampled_at = now;
        if used_ms >= busy_ms {
            tree.last_busy = Some(now);
        }
    }
}

/// Groups and children, read once per sample for every watched tree.
struct ProcessTable {
    members: HashMap<u32, Vec<u32>>,
    children: HashMap<u32, Vec<u32>>,
}

impl ProcessTable {
    fn read(system: &System) -> Self {
        let mut members: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for (pid, process) in system.processes() {
            let pid = pid.as_u32();
            if let Some(parent) = process.parent() {
                children.entry(parent.as_u32()).or_default().push(pid);
            }
            let pgid = i32::try_from(pid)
                .ok()
                .and_then(|raw| nix::unistd::getpgid(Some(Pid::from_raw(raw))).ok());
            if let Some(pgid) = pgid {
                members.entry(pgid.as_raw() as u32).or_default().push(pid);
            }
        }
        Self { members, children }
    }

    /// The group's members and all their descendants, which may have moved to groups of their
    /// own.
    fn tree(&self, pgid: u32) -> Vec<u32> {
        let mut found: HashSet<u32> = HashSet::new();
        let mut stack: Vec<u32> = self.members.get(&pgid).cloned().unwrap_or_default();
        while let Some(pid) = stack.pop() {
            if found.insert(pid) {
                stack.extend(self.children.get(&pid).into_iter().flatten());
            }
        }
        found.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::StopSignal;

    use super::*;
    use crate::process::AgentProcess;
    use crate::testutil::sh_spec;

    /// Kills a spawned test process with everything it started, even when an assertion fails
    /// first: a child in a group of its own outlives a kill of the head's group.
    struct Tree {
        process: AgentProcess,
        record: argonauts_core::agents::AgentGroup,
    }

    impl Tree {
        fn spawn(dir: &std::path::Path, script: &str) -> Self {
            let process = AgentProcess::spawn(&sh_spec(dir, script)).unwrap();
            let record = process.record().unwrap();
            Self { process, record }
        }

        fn kill(&self) {
            crate::reaper::kill_process_tree(&self.record).unwrap();
            crate::process::signal_group(self.record.pgid, StopSignal::Kill).unwrap();
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = crate::reaper::kill_process_tree(&self.record);
            let _ = crate::process::signal_group(self.record.pgid, StopSignal::Kill);
        }
    }

    /// Waits for `pid`, started at `started`, to be gone.
    async fn gone(pid: u32, started: u64) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while crate::liveness::is_same_process(pid, started) {
            if Instant::now() > deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        true
    }

    #[tokio::test]
    async fn cpu_sampler_sees_a_busy_tree_and_not_an_idle_one() {
        let dir = tempfile::tempdir().unwrap();
        let child_file = dir.path().join("child");
        // The busy loop runs in a child with a group of its own, which still counts.
        let mut busy = Tree::spawn(
            dir.path(),
            "set -m; (while :; do :; done) & echo $! > child.tmp && mv child.tmp child; wait",
        );
        let idle = Tree::spawn(dir.path(), "sleep 30");
        let child = wait_for_pid(&child_file).await;
        let child_started = crate::liveness::start_time(child).expect("the busy child runs");
        let sampler = CpuSampler::default();
        let busy_watch = sampler.watch(busy.record.pgid);
        let idle_watch = sampler.watch(idle.record.pgid);
        assert_eq!(busy_watch.pgid(), busy.record.pgid);
        sampler.sample();
        tokio::time::sleep(Duration::from_millis(500)).await;
        sampler.sample();
        assert!(busy_watch.last_busy().is_some());
        assert_eq!(idle_watch.last_busy(), None);

        let second = sampler.watch(busy.record.pgid);
        drop(busy_watch);
        assert!(second.last_busy().is_some());
        drop(second);
        assert!(!lock(&sampler.state).groups.contains_key(&busy.record.pgid));

        busy.kill();
        while busy.process.next_event().await.is_some() {}
        assert!(
            gone(child, child_started).await,
            "the busy child in its own group survived the test"
        );
        assert!(
            !crate::liveness::is_same_process(busy.record.pid, busy.record.start_time),
            "the busy head survived the test"
        );
    }

    async fn wait_for_pid(file: &std::path::Path) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(pid) = std::fs::read_to_string(file)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                return pid;
            }
            assert!(Instant::now() < deadline, "the busy child never started");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn sampler_thread_samples_on_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let busy = AgentProcess::spawn(&sh_spec(dir.path(), "while :; do :; done")).unwrap();
        let sampler = CpuSampler::start(Duration::from_millis(100));
        let watch = sampler.watch(busy.id());
        let deadline = Instant::now() + Duration::from_secs(5);
        while watch.last_busy().is_none() {
            assert!(
                Instant::now() < deadline,
                "the sampler never saw the busy loop"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(Arc::ptr_eq(&CpuSampler::shared(), &CpuSampler::shared()));
    }
}
