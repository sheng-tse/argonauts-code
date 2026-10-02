//! A snapshot of the process table in memory the caller owns, and one agent's tree within it.
//! Reading and marking allocate nothing, so the exit watcher can use them after a fork.

use std::io;

use super::procs::{self, PID_SCRATCH, Proc};

/// Processes a snapshot holds at most. On a machine running more, some are left out.
pub const CAPACITY: usize = 1 << 16;

/// All-zero is a valid row, which the exit watcher relies on for memory it maps itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct Row {
    proc: Proc,
    doomed: bool,
}

pub struct Table<'a> {
    rows: &'a mut [Row],
    pids: &'a mut [i32],
    len: usize,
}

impl<'a> Table<'a> {
    pub fn new(rows: &'a mut [Row], pids: &'a mut [i32]) -> Self {
        Self { rows, pids, len: 0 }
    }

    /// Reads the process table afresh, sorted by pid, with nothing marked.
    pub fn read(&mut self) -> io::Result<()> {
        let Self { rows, pids, len } = self;
        *len = 0;
        procs::each_process(pids, |proc| {
            let Some(row) = rows.get_mut(*len) else {
                return false;
            };
            *row = Row {
                proc,
                doomed: false,
            };
            *len += 1;
            true
        })?;
        rows[..*len].sort_unstable_by_key(|row| row.proc.pid);
        Ok(())
    }

    pub fn procs(&self) -> impl Iterator<Item = Proc> + '_ {
        self.rows[..self.len].iter().map(|row| row.proc)
    }

    /// Marks the leader, everything in its session or group, and all their descendants. The
    /// session id is the leader's pid, since every agent leads a session of its own.
    pub fn mark_tree(&mut self, leader: i32, pgid: i32) {
        // Session and group 1 hold the system's own processes.
        if leader <= 1 || pgid <= 1 {
            return;
        }
        self.mark_with(|proc| proc.pid == leader || proc.sid == leader || proc.pgid == pgid);
    }

    /// Marks every process in `noted` (pid and start time) that's still the same process, and
    /// all their descendants.
    pub fn mark_noted(&mut self, noted: &[(i32, u64)]) {
        if !noted.is_empty() {
            self.mark_with(|proc| proc.pid > 1 && noted.contains(&(proc.pid, proc.start)));
        }
    }

    /// Whether `pid` is in the table with the start time `start`.
    pub fn has(&self, pid: i32, start: u64) -> bool {
        let rows = &self.rows[..self.len];
        rows.binary_search_by_key(&pid, |row| row.proc.pid)
            .is_ok_and(|at| rows[at].proc.start == start)
    }

    fn mark_with(&mut self, root: impl Fn(&Proc) -> bool) {
        let mut changed = false;
        for row in &mut self.rows[..self.len] {
            if !row.doomed && root(&row.proc) {
                row.doomed = true;
                changed = true;
            }
        }
        while changed {
            changed = false;
            for at in 0..self.len {
                let Row { proc, doomed } = self.rows[at];
                if !doomed && self.is_doomed(proc.ppid) {
                    self.rows[at].doomed = true;
                    changed = true;
                }
            }
        }
    }

    pub fn doomed(&self) -> impl Iterator<Item = Proc> + '_ {
        self.rows[..self.len]
            .iter()
            .filter(|row| row.doomed)
            .map(|row| row.proc)
    }

    fn is_doomed(&self, pid: i32) -> bool {
        let rows = &self.rows[..self.len];
        rows.binary_search_by_key(&pid, |row| row.proc.pid)
            .is_ok_and(|at| rows[at].doomed)
    }
}

/// A table's memory, allocated for use in this process.
pub struct Storage {
    rows: Vec<Row>,
    pids: Vec<i32>,
}

impl Storage {
    pub fn allocate() -> Self {
        Self {
            rows: vec![Row::default(); CAPACITY],
            pids: vec![0; PID_SCRATCH],
        }
    }

    pub fn table(&mut self) -> Table<'_> {
        Table::new(&mut self.rows, &mut self.pids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_of(procs: &[(i32, i32, i32, i32)]) -> (Vec<Row>, usize) {
        let mut rows: Vec<Row> = procs
            .iter()
            .map(|&(pid, ppid, pgid, sid)| Row {
                proc: Proc {
                    pid,
                    ppid,
                    pgid,
                    sid,
                    // Distinct from the pid, so a test can tell a reused pid apart.
                    start: pid as u64 * 10,
                },
                doomed: false,
            })
            .collect();
        rows.sort_unstable_by_key(|row| row.proc.pid);
        let len = rows.len();
        (rows, len)
    }

    #[test]
    fn tree_takes_session_group_and_descendants() {
        let (mut rows, len) = table_of(&[
            (1, 0, 1, 1),
            (50, 1, 50, 50),
            // The leader, a child in its group, and one in a group of its own in the session.
            (100, 50, 100, 100),
            (101, 100, 100, 100),
            (102, 100, 102, 100),
            // A descendant that left the session, and its child with a lower pid.
            (103, 102, 103, 103),
            (90, 103, 103, 103),
            // An orphan still in the session, and an unrelated process.
            (104, 1, 104, 100),
            (105, 50, 105, 50),
        ]);
        let mut pids = [];
        let mut table = Table::new(&mut rows, &mut pids);
        table.len = len;
        table.mark_tree(100, 100);
        let mut doomed: Vec<i32> = table.doomed().map(|proc| proc.pid).collect();
        doomed.sort_unstable();
        assert_eq!(doomed, [90, 100, 101, 102, 103, 104]);

        table.mark_tree(1, 1);
        assert_eq!(table.doomed().count(), 6);
    }

    #[test]
    fn noted_processes_and_their_descendants_are_marked_while_still_the_same() {
        let (mut rows, len) = table_of(&[
            (1, 0, 1, 1),
            // An orphan that left the agent's session, and its child.
            (200, 1, 200, 200),
            (201, 200, 200, 200),
            // A pid reused since it was noted, and an unrelated process.
            (300, 1, 300, 300),
            (400, 1, 400, 400),
        ]);
        let mut pids = [];
        let mut table = Table::new(&mut rows, &mut pids);
        table.len = len;
        assert!(table.has(200, 2000));
        assert!(!table.has(300, 1));
        assert!(!table.has(999, 9990));

        table.mark_noted(&[(200, 2000), (300, 1), (1, 10), (999, 9990)]);
        let mut doomed: Vec<i32> = table.doomed().map(|proc| proc.pid).collect();
        doomed.sort_unstable();
        assert_eq!(doomed, [200, 201]);
    }

    #[test]
    fn live_table_holds_this_process() {
        let mut storage = Storage::allocate();
        let mut table = storage.table();
        table.read().unwrap();
        let me = std::process::id() as i32;
        let this = table.procs().find(|proc| proc.pid == me).unwrap();
        assert_eq!(this.ppid, nix::unistd::getppid().as_raw());
        assert_eq!(this.pgid, nix::unistd::getpgrp().as_raw());
        let pids: Vec<i32> = table.procs().map(|proc| proc.pid).collect();
        assert!(pids.is_sorted());
    }
}
