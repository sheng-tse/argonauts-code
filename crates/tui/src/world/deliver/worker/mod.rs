//! A drawing worker: pictures drawn on a thread of their own, so no frame waits on one, and the
//! drawings no longer wanted stopped within a few milliseconds.
//!
//! Each drawing goes by a key, so asking again for one under way changes nothing, and its job
//! is handed a [`Cancel`] to ask every millisecond or so whether to give up. A view draws its
//! scenes here and hands the screen the newest one finished; the screen packs its new pictures
//! here, and stops them all once the map leaves. A worker's thread that has finished all it was
//! asked for wakes the screen through [`arrivals`], so no frame is drawn to look for a drawing,
//! and the pictures a scene asked for at once arrive in one frame.

use std::cell::Cell;
use std::collections::HashMap;
use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use tokio::sync::watch;

/// Tells a drawing under way whether it is still wanted.
#[derive(Debug, Clone)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

type Run<T> = Box<dyn FnOnce(&Cancel) -> Option<T> + Send>;

struct Job<T> {
    key: u64,
    cancel: Cancel,
    run: Run<T>,
    /// Its drawing wakes the screen once done.
    wakes: bool,
}

pub struct Worker<T> {
    shared: Arc<Shared<T>>,
    /// Starts with the first drawing; none on a worker that draws in line.
    sender: Mutex<Option<Sender<Job<T>>>>,
    in_line: bool,
}

struct Shared<T> {
    state: Mutex<State<T>>,
}

struct State<T> {
    /// Drawings asked for and not yet done, by key, with the stop they were asked under.
    under_way: HashMap<u64, Cancel>,
    /// What every drawing asked for since the last stop shares.
    current: Cancel,
    done: Vec<(u64, T)>,
    /// A job panicked: its drawing gave nothing, and the drawings after it went on.
    failed: bool,
}

impl<T> Shared<T> {
    fn state(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<T> fmt::Debug for Worker<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.shared.state();
        f.debug_struct("Worker")
            .field("under_way", &state.under_way.len())
            .field("done", &state.done.len())
            .field("in_line", &self.in_line)
            .finish()
    }
}

impl<T: Send + 'static> Worker<T> {
    /// A worker with a thread of its own, started with its first drawing.
    pub fn threaded() -> Self {
        Self::new(false)
    }

    /// A worker that draws each job the moment it is asked, for tests that read every frame
    /// whole.
    pub fn in_line() -> Self {
        Self::new(true)
    }

    /// A worker for what the screen draws off the frame: on a thread of its own, and in line in
    /// the crate's tests, which read every frame whole, unless the test asked for threads.
    pub fn off_the_frame() -> Self {
        Self::new(in_line_here())
    }

    fn new(in_line: bool) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    under_way: HashMap::new(),
                    current: Cancel(Arc::new(AtomicBool::new(false))),
                    done: Vec::new(),
                    failed: false,
                }),
            }),
            sender: Mutex::new(None),
            in_line,
        }
    }

    /// Draws `key` with `job`, after the drawings asked before it, unless it is under way
    /// already. A job that returns `None`, as one stopped part way does, leaves nothing done.
    pub fn draw(&self, key: u64, job: impl FnOnce(&Cancel) -> Option<T> + Send + 'static) {
        self.ask(key, true, Box::new(job));
    }

    /// As [`draw`](Self::draw), for a drawing the screen takes on its next frame, whenever that
    /// comes: it doesn't wake the screen.
    pub fn draw_quietly(&self, key: u64, job: impl FnOnce(&Cancel) -> Option<T> + Send + 'static) {
        self.ask(key, false, Box::new(job));
    }

    fn ask(&self, key: u64, wakes: bool, drawing: Run<T>) {
        let cancel = {
            let mut state = self.shared.state();
            if state.under_way.contains_key(&key) {
                return;
            }
            let cancel = state.current.clone();
            state.under_way.insert(key, cancel.clone());
            cancel
        };
        let job = Job {
            key,
            cancel,
            run: drawing,
            wakes,
        };
        if self.in_line {
            run(&self.shared, job);
            return;
        }
        let mut sender = self.sender.lock().unwrap_or_else(PoisonError::into_inner);
        let job = match sender.as_ref() {
            Some(running) => match running.send(job) {
                Ok(()) => return,
                // The thread ended, as after a panic in a job: another starts.
                Err(mpsc::SendError(job)) => job,
            },
            None => job,
        };
        match start(&self.shared) {
            Some(started) => {
                // The thread just started holds the other end, so this reaches it.
                let _ = started.send(job);
                *sender = Some(started);
            }
            // No thread to be had: drawn here rather than never.
            None => {
                run(&self.shared, job);
            }
        }
    }

    /// The drawings finished since the last call, oldest first.
    pub fn take(&self) -> Vec<(u64, T)> {
        std::mem::take(&mut self.shared.state().done)
    }

    /// Whether a job has panicked since the worker began.
    pub fn failed(&self) -> bool {
        self.shared.state().failed
    }

    /// Whether any drawing asked for isn't done.
    pub fn busy(&self) -> bool {
        !self.shared.state().under_way.is_empty()
    }

    /// Stops every drawing asked for so far: those waiting never start, the one under way gives
    /// up when it next asks, and none of them leaves anything done.
    pub fn stop(&self) {
        let mut state = self.shared.state();
        state.current.0.store(true, Ordering::Relaxed);
        state.current = Cancel(Arc::new(AtomicBool::new(false)));
        state.under_way.clear();
        state.done.clear();
    }
}

impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        self.shared.state().current.0.store(true, Ordering::Relaxed);
    }
}

/// A thread that draws each job it is sent, until the worker goes.
fn start<T: Send + 'static>(shared: &Arc<Shared<T>>) -> Option<Sender<Job<T>>> {
    let (sender, jobs) = mpsc::channel::<Job<T>>();
    let shared = Arc::clone(shared);
    std::thread::Builder::new()
        .name("world drawing".to_owned())
        .spawn(move || {
            let mut drew = false;
            for job in jobs {
                let wakes = job.wakes;
                drew |= run(&shared, job) && wakes;
                if drew && shared.state().under_way.is_empty() {
                    drew = false;
                    arrived().send_modify(|count| *count = count.wrapping_add(1));
                }
            }
        })
        .ok()?;
    Some(sender)
}

thread_local! {
    /// A job runs on this thread now, so a panic in it is caught and ends that drawing alone.
    static DRAWING: Cell<bool> = const { Cell::new(false) };
}

/// Whether the panic under way came from a drawing's job, which the worker catches: it ends
/// that drawing alone, so it must neither hand the terminal back nor free the pictures kept.
pub fn panicking_in_a_drawing() -> bool {
    DRAWING.with(Cell::get)
}

/// Runs a job, a panic in it caught: `Err` when it panicked.
fn guarded<T>(job: Run<T>, cancel: &Cancel) -> Result<Option<T>, ()> {
    let was = DRAWING.with(|drawing| drawing.replace(true));
    let drawn = panic::catch_unwind(AssertUnwindSafe(|| job(cancel)));
    DRAWING.with(|drawing| drawing.set(was));
    drawn.map_err(drop)
}

/// Runs `job`: whether it left a drawing done.
fn run<T>(shared: &Shared<T>, job: Job<T>) -> bool {
    let (drawn, panicked) = if job.cancel.cancelled() {
        (None, false)
    } else {
        match guarded(job.run, &job.cancel) {
            Ok(drawn) => (drawn, false),
            Err(()) => (None, true),
        }
    };
    let mut state = shared.state();
    state.failed |= panicked;
    // A stop since may have asked for the same key again, under its own stop.
    let asked_again = state
        .under_way
        .get(&job.key)
        .is_some_and(|cancel| !Arc::ptr_eq(&cancel.0, &job.cancel.0));
    if !asked_again {
        state.under_way.remove(&job.key);
    }
    match drawn.filter(|_| !job.cancel.cancelled()) {
        Some(drawn) => {
            state.done.push((job.key, drawn));
            true
        }
        None => false,
    }
}

/// Counts the times a worker's thread finished all it was asked for, having drawn something
/// that wakes the screen.
fn arrived() -> &'static watch::Sender<u64> {
    static ARRIVED: OnceLock<watch::Sender<u64>> = OnceLock::new();
    ARRIVED.get_or_init(|| watch::Sender::new(0))
}

/// Changes each time a worker's thread has finished all it was asked for, having drawn
/// something that wakes the screen, from now on, so the screen draws the frame that shows it.
pub fn arrivals() -> watch::Receiver<u64> {
    arrived().subscribe()
}

#[cfg(test)]
thread_local! {
    /// The test on this thread draws off the frame on threads, as the app does.
    static ON_THREADS: Cell<bool> = const { Cell::new(false) };
}

/// Whether the workers made off the frame on this thread draw in line: only in the crate's
/// tests, unless the test asked for threads.
pub fn in_line_here() -> bool {
    cfg!(test) && !threads_asked()
}

#[cfg(test)]
fn threads_asked() -> bool {
    ON_THREADS.with(Cell::get)
}

#[cfg(not(test))]
fn threads_asked() -> bool {
    false
}

/// The workers this test's thread makes off the frame from now on draw on threads of their
/// own, as the app's do.
#[cfg(test)]
pub fn threads_for_this_test() {
    ON_THREADS.with(|on| on.set(true));
}

#[cfg(test)]
mod tests;
