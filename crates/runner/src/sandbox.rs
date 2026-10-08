//! The JavaScript engine a workflow script runs in. It lives on a thread of its own, since the
//! engine can't move between threads, and talks to the runner in messages: the script asks for
//! an agent, and the runner settles the promise when the agent ends. A script has no files, no
//! network, no clock and no dice; its memory and its running time are capped.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rquickjs::{CatchResultExt, Context, Ctx, Function, Object, Runtime};
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::script::Script;

const PRELUDE: &str = include_str!("js/prelude.js");
const DRIVER: &str = include_str!("js/driver.js");

/// Hands the script's own promise to the host's door: its value, or what it threw.
const FINISH: &str = r#"(function (running, send) {
  running.then(
    (value) => {
      try {
        send('done', value === undefined ? null : value);
      } catch (error) {
        send('fail', "the script's result can't be written as JSON: " + error);
      }
    },
    (error) => {
      const said = error && error.stack ? String(error) + '\n' + error.stack : String(error);
      send('fail', said);
    }
  );
})"#;

/// What a script may use up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    pub memory_bytes: usize,
    pub stack_bytes: usize,
    /// The longest the script itself may run between two waits for an agent.
    pub slice: Duration,
    /// The longest the script itself may run in all. Waiting for agents doesn't count.
    pub total: Duration,
    /// The most items one `parallel` or `pipeline` takes.
    pub items: u32,
    /// The most agents one run starts.
    pub agents: u32,
    /// The most agents working at once.
    pub at_once: usize,
    /// A script may run another workflow as a step.
    pub nested: bool,
}

impl Default for Limits {
    /// Claude Code's own caps for agents and items.
    fn default() -> Self {
        Self {
            memory_bytes: 64 * 1024 * 1024,
            stack_bytes: 512 * 1024,
            slice: Duration::from_secs(5),
            total: Duration::from_secs(60),
            items: 4096,
            agents: 1000,
            at_once: 16,
            nested: true,
        }
    }
}

/// What the script asked for and waits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallKind {
    Agent,
    Check,
    Workflow,
}

#[derive(Debug)]
pub(crate) enum Event {
    Call {
        id: u64,
        kind: CallKind,
        payload: Value,
    },
    Phase(String),
    Log(String),
    /// A judge kept one of several attempts.
    Keep(Value),
    Ended(Result<Value, ScriptEnd>),
}

/// Why a script stopped without a result.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScriptEnd {
    #[error("{0}")]
    Threw(String),
    #[error("the script ran for longer than it may without waiting for an agent")]
    TimedOut,
    #[error("the script waits on something that never comes: an await with no agent behind it")]
    Stuck,
    #[error("the workflow was stopped")]
    Stopped,
    #[error("the script engine failed: {0}")]
    Engine(String),
}

pub(crate) enum Command {
    Settle {
        id: u64,
        result: Result<Value, String>,
    },
}

pub(crate) struct Sandbox {
    pub events: UnboundedReceiver<Event>,
    commands: mpsc::Sender<Command>,
    stop: Arc<AtomicBool>,
}

impl Sandbox {
    /// Starts the script. `spent` is the token count `budget.spent()` reads.
    pub fn start(
        script: &Script,
        args: Value,
        limits: &Limits,
        budget: Option<u64>,
        spent: Arc<AtomicU64>,
    ) -> Result<Self, ScriptEnd> {
        let (events_out, events) = unbounded_channel();
        let (commands, commands_in) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let job = Job {
            source: script.wrapped(),
            steps: script.runs_steps(),
            args,
            limits: limits.clone(),
            budget,
            spent,
            stop: Arc::clone(&stop),
            events: events_out,
            commands: commands_in,
        };
        thread::Builder::new()
            .name("workflow-script".to_owned())
            // Room for the engine's own frames above the script's capped stack.
            .stack_size(limits.stack_bytes + 4 * 1024 * 1024)
            .spawn(move || job.run())
            .map_err(|err| ScriptEnd::Engine(err.to_string()))?;
        Ok(Self {
            events,
            commands,
            stop,
        })
    }

    pub fn settle(&self, id: u64, result: Result<Value, String>) {
        // A script that already ended has nobody left to tell.
        let _ = self.commands.send(Command::Settle { id, result });
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

struct Job {
    source: String,
    steps: bool,
    args: Value,
    limits: Limits,
    budget: Option<u64>,
    spent: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    events: UnboundedSender<Event>,
    commands: mpsc::Receiver<Command>,
}

#[derive(Default)]
struct Door {
    next_id: u64,
    waiting: u64,
    ended: Option<Result<Value, String>>,
}

/// Stops the script when it runs too long: checked by the engine as it runs.
struct Watch {
    started: Instant,
    /// Nanoseconds after `started` at which the current slice is over.
    slice_ends: Arc<AtomicU64>,
    timed_out: Arc<AtomicBool>,
    used: Duration,
    limits: Limits,
}

impl Watch {
    fn begin_slice(&mut self) -> Instant {
        let left = self.limits.total.saturating_sub(self.used);
        let slice = self.limits.slice.min(left);
        let ends = self.started.elapsed() + slice;
        self.slice_ends.store(
            u64::try_from(ends.as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        Instant::now()
    }

    fn end_slice(&mut self, began: Instant) {
        self.used += began.elapsed();
        self.slice_ends.store(u64::MAX, Ordering::Relaxed);
    }
}

impl Job {
    fn run(self) {
        let events = self.events.clone();
        let ended = self.script();
        let _ = events.send(Event::Ended(ended));
    }

    fn script(self) -> Result<Value, ScriptEnd> {
        let engine = |err: rquickjs::Error| ScriptEnd::Engine(err.to_string());
        let runtime = Runtime::new().map_err(engine)?;
        runtime.set_memory_limit(self.limits.memory_bytes);
        runtime.set_max_stack_size(self.limits.stack_bytes);
        let mut watch = Watch {
            started: Instant::now(),
            slice_ends: Arc::new(AtomicU64::new(u64::MAX)),
            timed_out: Arc::new(AtomicBool::new(false)),
            used: Duration::ZERO,
            limits: self.limits.clone(),
        };
        let (started, slice_ends) = (watch.started, Arc::clone(&watch.slice_ends));
        let (timed_out, stop) = (Arc::clone(&watch.timed_out), Arc::clone(&self.stop));
        runtime.set_interrupt_handler(Some(Box::new(move || {
            let now = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            let late = now >= slice_ends.load(Ordering::Relaxed);
            if late {
                timed_out.store(true, Ordering::Relaxed);
            }
            late || stop.load(Ordering::Relaxed)
        })));
        let context = Context::full(&runtime).map_err(engine)?;
        context.with(|ctx| self.drive(&ctx, &mut watch))
    }

    fn drive(&self, ctx: &Ctx<'_>, watch: &mut Watch) -> Result<Value, ScriptEnd> {
        let door = Rc::new(RefCell::new(Door::default()));
        let began = watch.begin_slice();
        let settle = self.open(ctx, &door);
        watch.end_slice(began);
        let settle = settle.map_err(|said| self.why(watch, said))?;
        loop {
            let began = watch.begin_slice();
            while ctx.execute_pending_job() {}
            watch.end_slice(began);
            if watch.timed_out.load(Ordering::Relaxed) {
                return Err(ScriptEnd::TimedOut);
            }
            if self.stop.load(Ordering::Relaxed) {
                return Err(ScriptEnd::Stopped);
            }
            if let Some(ended) = door.borrow_mut().ended.take() {
                return ended.map_err(ScriptEnd::Threw);
            }
            if door.borrow().waiting == 0 {
                return Err(ScriptEnd::Stuck);
            }
            let Ok(Command::Settle { id, result }) = self.commands.recv() else {
                return Err(ScriptEnd::Stopped);
            };
            door.borrow_mut().waiting -= 1;
            let (ok, said) = match result {
                Ok(value) => (true, value.to_string()),
                Err(why) => (false, why),
            };
            let began = watch.begin_slice();
            let settled = settle
                .call::<_, ()>((id.to_string(), ok, said))
                .catch(ctx)
                .map_err(|err| err.to_string());
            watch.end_slice(began);
            settled.map_err(|said| self.why(watch, said))?;
        }
    }

    /// Sets the script going and returns the function that settles what it waits on.
    fn open<'js>(&self, ctx: &Ctx<'js>, door: &Rc<RefCell<Door>>) -> Result<Function<'js>, String> {
        let said = |err: rquickjs::Error| err.to_string();
        let host = self.host(ctx, door).map_err(said)?;
        let limits = Object::new(ctx.clone()).map_err(said)?;
        limits.set("items", self.limits.items).map_err(said)?;
        // Tokens as a JavaScript number, which holds every count a run could reach.
        limits
            .set("budget", self.budget.map(|tokens| tokens as f64))
            .map_err(said)?;
        let prelude: Function = ctx.eval(PRELUDE).map_err(said)?;
        let doors: Object = prelude
            .call((host, limits))
            .catch(ctx)
            .map_err(|err| err.to_string())?;
        let settle: Function = doors.get("settle").map_err(said)?;
        let send: Function = doors.get("send").map_err(said)?;
        if self.steps {
            let driver: Function = ctx.eval(DRIVER).map_err(said)?;
            driver
                .call::<_, ()>((send.clone(),))
                .catch(ctx)
                .map_err(|err| err.to_string())?;
        }
        let main: Function = ctx
            .eval(self.source.as_bytes())
            .catch(ctx)
            .map_err(|err| err.to_string())?;
        let args = ctx.json_parse(self.args.to_string()).map_err(said)?;
        let running: rquickjs::Value = main
            .call((args,))
            .catch(ctx)
            .map_err(|err| err.to_string())?;
        let finish: Function = ctx.eval(FINISH).map_err(said)?;
        finish
            .call::<_, ()>((running, send))
            .catch(ctx)
            .map_err(|err| err.to_string())?;
        Ok(settle)
    }

    /// The one door out of the sandbox: every message the script's helpers send.
    fn host<'js>(
        &self,
        ctx: &Ctx<'js>,
        door: &Rc<RefCell<Door>>,
    ) -> rquickjs::Result<Function<'js>> {
        let door = Rc::clone(door);
        let events = self.events.clone();
        let spent = Arc::clone(&self.spent);
        Function::new(
            ctx.clone(),
            move |kind: String, payload: String| -> String {
                let payload: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
                let call = |kind: CallKind, payload: Value| {
                    let mut door = door.borrow_mut();
                    door.next_id += 1;
                    door.waiting += 1;
                    let id = door.next_id;
                    let _ = events.send(Event::Call { id, kind, payload });
                    id.to_string()
                };
                let text = |key: &str| {
                    payload
                        .get(key)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                match kind.as_str() {
                    "agent" => return call(CallKind::Agent, payload),
                    "check" => return call(CallKind::Check, payload),
                    "workflow" => return call(CallKind::Workflow, payload),
                    "spent" => return spent.load(Ordering::Relaxed).to_string(),
                    "phase" => {
                        let _ = events.send(Event::Phase(text("title")));
                    }
                    "log" => {
                        let _ = events.send(Event::Log(text("message")));
                    }
                    "keep" => {
                        let _ = events.send(Event::Keep(payload));
                    }
                    "done" => door.borrow_mut().ended = Some(Ok(payload)),
                    "fail" => {
                        let why = payload.as_str().unwrap_or("the script failed").to_owned();
                        door.borrow_mut().ended = Some(Err(why));
                    }
                    _ => {}
                }
                String::new()
            },
        )
    }

    fn why(&self, watch: &Watch, said: String) -> ScriptEnd {
        if watch.timed_out.load(Ordering::Relaxed) {
            ScriptEnd::TimedOut
        } else if self.stop.load(Ordering::Relaxed) {
            ScriptEnd::Stopped
        } else {
            ScriptEnd::Threw(said)
        }
    }
}

/// A plain literal's value, such as a script's `meta`. Anything but a literal has nothing to
/// call or read here, so it fails.
pub(crate) fn literal(text: &str) -> Result<Value, String> {
    quick(|ctx| {
        let said = |err: rquickjs::Error| err.to_string();
        let bare: Function = ctx
            .eval(
                "(function () { const all = globalThis; \
                 for (const name of Object.getOwnPropertyNames(all)) { \
                 if (name !== 'JSON') { try { delete all[name]; } catch (_) {} } } })",
            )
            .map_err(said)?;
        bare.call::<_, ()>(())
            .catch(ctx)
            .map_err(|err| err.to_string())?;
        let json: Option<String> = ctx
            .eval(format!("JSON.stringify(({text}\n))"))
            .catch(ctx)
            .map_err(|err| err.to_string())?;
        let json = json.ok_or("it isn't a value")?;
        serde_json::from_str(&json).map_err(|err| err.to_string())
    })
}

/// Whether the wrapped script is JavaScript the engine accepts, without running it.
pub(crate) fn syntax(wrapped: &str) -> Result<(), String> {
    quick(|ctx| {
        ctx.eval::<Function, _>(wrapped.as_bytes())
            .catch(ctx)
            .map(|_| ())
            .map_err(|err| err.to_string())
    })
}

/// A short job in an engine of its own, small and soon over.
fn quick<T>(job: impl FnOnce(&Ctx<'_>) -> Result<T, String>) -> Result<T, String> {
    let runtime = Runtime::new().map_err(|err| err.to_string())?;
    runtime.set_memory_limit(8 * 1024 * 1024);
    runtime.set_max_stack_size(256 * 1024);
    let ends = Instant::now() + Duration::from_secs(1);
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= ends)));
    let context = Context::full(&runtime).map_err(|err| err.to_string())?;
    context.with(|ctx| job(&ctx))
}
