//! Runs a script to its end: starts the agents it asks for, a few at a time, settles each call
//! with its answer, saves every finished call, and reports the whole state on every change.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use argonauts_core::events::session::CliWorkflowRun;
use argonauts_core::settings::crew::CrewRole;
use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{Semaphore, watch};

use crate::call::{AgentCall, AgentEnd, Check, Checked, Host, JournalEntry, Kept, fingerprint};
use crate::report::{AgentTotals, Progress};
use crate::sandbox::{CallKind, Event, Limits, Sandbox, ScriptEnd};
use crate::script::Script;

/// What one run of a script starts from.
#[derive(Debug, Clone, Default)]
pub struct RunInput {
    /// The workflow's id in its reports, unique in the run that starts it.
    pub id: String,
    /// The script's `args`. A file of steps reads `request`, `crew` and `tasks` from it.
    pub args: Value,
    pub limits: Limits,
    /// What an earlier run of this script finished, reused call by call while they match.
    pub replay: Vec<JournalEntry>,
    /// The roles its agents may name. A script the orchestrator wrote for a workflow file keeps
    /// to the file's roles.
    pub roles: Option<BTreeSet<CrewRole>>,
    /// A token target for the run, which `limit.total` reads. Past it, `agent()` throws.
    pub token_limit: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ended {
    /// What the script returned, or why it has no result.
    pub result: Result<Value, ScriptEnd>,
    /// The last state reported.
    pub report: CliWorkflowRun,
    /// Agents that ran live. Calls answered from an earlier run don't count.
    pub agents_run: u32,
}

/// How a call ended, for the script, and its place in the saved order when its answer came
/// from an earlier run.
type Settled = (u64, Result<Value, String>, Option<usize>);

/// How long saved answers may wait for a call that never comes before they stop waiting.
const STALLED: Duration = Duration::from_secs(2);

/// The most lines one script may log, and the most calls it may have refused, before it is
/// taken for a loop that will never end. Past the first, lines are dropped. Past the second,
/// the script is stopped.
const LINES: u32 = 2_000;
const REFUSALS: u32 = 1_000;

/// How far apart the call numbers of two workflows run as steps start, and of the script that
/// ran them. A run starts far fewer agents than this, so no two calls share a number.
const BLOCK: u32 = 1 << 16;

struct Shared<'a, H: Host + ?Sized> {
    host: &'a H,
    /// What an earlier run of this script finished and can answer again, in the order it
    /// first finished: each call's newest saved answer, up to the first call that failed,
    /// which runs again with everything after it.
    saved: Vec<JournalEntry>,
    progress: Mutex<Progress>,
    slots: Semaphore,
    spent: Arc<AtomicU64>,
    live: AtomicU64,
    replay: Replay,
}

/// Hands saved answers back in the order the calls first finished. A script asks for agents
/// side by side, and which finished first decided what it asked next, so only the same order
/// has it ask the same calls again.
struct Replay {
    /// The place in the saved order whose answer goes back next. `usize::MAX` once the order
    /// no longer matters, and every saved answer goes back at once.
    next: watch::Sender<usize>,
}

impl Replay {
    fn new() -> Self {
        Self {
            next: watch::channel(0).0,
        }
    }

    /// Waits until every saved answer before `place` has gone back.
    async fn turn(&self, place: usize) {
        let mut next = self.next.subscribe();
        // The sender lives as long as the run, so this only ends when the turn comes.
        let _ = next.wait_for(|next| *next >= place).await;
    }

    fn went_back(&self, place: usize) {
        self.next.send_if_modified(|next| {
            let was_its_turn = *next == place;
            if was_its_turn {
                *next += 1;
            }
            was_its_turn
        });
    }

    fn stop_waiting(&self) {
        self.next.send_replace(usize::MAX);
    }

    fn waits_before(&self, saved: usize) -> bool {
        *self.next.borrow() < saved
    }
}

impl<H: Host + ?Sized> Shared<'_, H> {
    fn progress(&self) -> MutexGuard<'_, Progress> {
        // A panic while reporting leaves the rows as they were, which is still worth drawing.
        self.progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn report(&self) {
        let workflow = {
            let mut progress = self.progress();
            progress.clock(self.host.now_ms());
            progress.workflow().clone()
        };
        self.host.report(&workflow);
    }
}

/// Runs `script` until it returns, throws, or is stopped by dropping this future.
pub async fn run<H: Host + ?Sized>(script: &Script, input: RunInput, host: &H) -> Ended {
    let spent = Arc::new(AtomicU64::new(host.tokens_spent()));
    let shared = Shared {
        host,
        saved: answerable(&input.replay),
        progress: Mutex::new(Progress::new(&input.id, &script.meta, host.now_ms())),
        slots: Semaphore::new(input.limits.at_once.max(1)),
        spent: Arc::clone(&spent),
        live: AtomicU64::new(0),
        replay: Replay::new(),
    };
    shared.report();
    let result = drive(script, &input, &shared, Nesting::TOP).await;
    let summary = match &result {
        Ok(_) => None,
        Err(why) => Some(why.to_string()),
    };
    shared.progress().finish(result.is_ok(), summary.as_deref());
    shared.report();
    let report = shared.progress().workflow().clone();
    Ended {
        result,
        report,
        agents_run: u32::try_from(shared.live.load(Ordering::Relaxed)).unwrap_or(u32::MAX),
    }
}

/// Where a script stands among the run's workflows.
#[derive(Debug, Clone, Copy)]
struct Nesting {
    /// 0 for the run's own script, 1 for a workflow it ran as a step.
    depth: u32,
    /// What its calls are numbered from.
    base: u32,
}

impl Nesting {
    const TOP: Self = Self { depth: 0, base: 0 };

    /// The `nth` workflow the run's script ran as a step, from 1.
    fn step(nth: u32) -> Self {
        Self {
            depth: 1,
            base: nth.saturating_mul(BLOCK),
        }
    }
}

/// One script in its own engine.
fn drive<'a, H: Host + ?Sized>(
    script: &'a Script,
    input: &'a RunInput,
    shared: &'a Shared<'a, H>,
    nesting: Nesting,
) -> BoxFuture<'a, Result<Value, ScriptEnd>> {
    Box::pin(async move {
        let depth = nesting.depth;
        let mut sandbox = Sandbox::start(
            script,
            input.args.clone(),
            &input.limits,
            input.token_limit,
            Arc::clone(&shared.spent),
        )?;
        let mut calls: FuturesUnordered<BoxFuture<'a, Settled>> = FuturesUnordered::new();
        let mut journal = Journal::new(nesting.base);
        let saved = shared.saved.len();
        let (mut agents, mut steps) = (0u32, 0u32);
        let (mut lines, mut refused) = (0u32, 0u32);
        // The group a nested workflow's agents show under, as Claude Code draws them.
        let group = (depth > 0).then(|| format!("▸ {}", script.meta.name));
        loop {
            let event = tokio::select! {
                biased;
                event = sandbox.events.recv() => event,
                Some((id, result, place)) = calls.next(), if !calls.is_empty() => {
                    sandbox.settle(id, result);
                    if let Some(place) = place {
                        shared.replay.went_back(place);
                    }
                    continue;
                }
                // A saved answer waits for a call the script no longer makes, which holds
                // back every answer saved after it.
                () = tokio::time::sleep(STALLED), if depth == 0 && shared.replay.waits_before(saved) => {
                    shared.replay.stop_waiting();
                    continue;
                }
            };
            let Some(event) = event else {
                return Err(ScriptEnd::Engine(
                    "the script's engine stopped without a word".to_owned(),
                ));
            };
            match event {
                Event::Ended(ended) => return ended,
                Event::Phase(name) => {
                    if group.is_none() {
                        shared.progress().phase(&name);
                        shared.report();
                    }
                }
                Event::Log(message) => {
                    lines += 1;
                    if lines <= LINES {
                        shared.host.log(&message);
                        shared.progress().say(&message);
                        shared.report();
                    }
                }
                Event::Keep(payload) => {
                    if let Ok(kept) = serde_json::from_value::<Kept>(payload) {
                        shared.host.keep(&kept);
                    }
                }
                Event::Call {
                    id,
                    kind: CallKind::Agent,
                    payload,
                } => {
                    agents += 1;
                    let asked = agent_call(agents, &payload, input, shared);
                    let mut call = match asked {
                        Ok(call) => call,
                        Err(why) => {
                            refused += 1;
                            if refused > REFUSALS {
                                return Err(ScriptEnd::Threw(format!(
                                    "the script kept asking for agents it couldn't have: {why}"
                                )));
                            }
                            sandbox.settle(id, Err(why));
                            continue;
                        }
                    };
                    call.index = nesting.base.saturating_add(agents);
                    if let Some(group) = &group {
                        call.phase = Some(group.clone());
                    }
                    let saved = journal.next(CallKind::Agent, &payload, shared);
                    calls.push(Box::pin(agent(shared, call, saved, id)));
                }
                Event::Call {
                    id,
                    kind: CallKind::Check,
                    payload,
                } => {
                    let saved = journal.next(CallKind::Check, &payload, shared);
                    calls.push(Box::pin(check(shared, payload, saved, id)));
                }
                Event::Call {
                    id,
                    kind: CallKind::Workflow,
                    payload,
                } => {
                    let refusal = if depth > 0 {
                        Some("a workflow started by another can't start one of its own")
                    } else if !input.limits.nested {
                        Some("this run doesn't let a workflow run another as a step")
                    } else {
                        None
                    };
                    if let Some(why) = refusal {
                        sandbox.settle(id, Err(why.to_owned()));
                        continue;
                    }
                    steps += 1;
                    calls.push(Box::pin(nested(shared, payload, input, id, steps)));
                }
            }
        }
    })
}

/// Reads an `agent()` call and holds it to the run's caps. The error is thrown in the script.
fn agent_call<H: Host + ?Sized>(
    number: u32,
    payload: &Value,
    input: &RunInput,
    shared: &Shared<'_, H>,
) -> Result<AgentCall, String> {
    if number > input.limits.agents {
        return Err(format!(
            "this workflow already started {} agents, the most one run may",
            input.limits.agents
        ));
    }
    if let Some(limit) = input.token_limit
        && shared.spent.load(Ordering::Relaxed) >= limit
    {
        return Err(format!(
            "the run's {limit} tokens are spent, so no more agents start"
        ));
    }
    let call = AgentCall::read(number, payload)?;
    if let Some(roles) = &input.roles
        && !roles.contains(&call.role)
    {
        let allowed: Vec<&str> = roles.iter().map(|role| role.as_str()).collect();
        return Err(format!(
            "agent(): this workflow's file has no {} step; its roles are {}",
            call.role,
            allowed.join(", ")
        ));
    }
    // A task list you fixed is the plan: the orchestrator's script works on those tasks, and
    // a branch the rehearsal didn't take can't add or split one.
    if input.roles.is_some()
        && let Some(task) = &call.task
    {
        let fixed: Vec<String> = input
            .args
            .get("tasks")
            .and_then(Value::as_array)
            .map(|tasks| {
                tasks
                    .iter()
                    .filter_map(|task| task.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if !argonauts_core::required::check::is_one_of(&fixed, task) {
            return Err(
                "agent(): the user fixed the task list; ask instead of adding one".to_owned(),
            );
        }
    }
    // Your settings give each role its models and the file says which steps nest a workflow,
    // so a script the orchestrator wrote names roles alone.
    if input.roles.is_some() {
        let chosen = [
            (call.model.is_some(), "model"),
            (call.effort.is_some(), "effort"),
            (call.own_workflow, "ownWorkflow"),
        ];
        if let Some((_, option)) = chosen.iter().find(|(set, _)| *set) {
            return Err(format!(
                "agent(): the settings pick the {option}; leave {option} out"
            ));
        }
    }
    Ok(call)
}

/// One agent from its turn in the queue to its answer. A failed agent's answer is null, as in
/// Claude Code, so the script decides what a missing answer means.
async fn agent<H: Host + ?Sized>(
    shared: &Shared<'_, H>,
    mut call: AgentCall,
    saved: Saved,
    id: u64,
) -> Settled {
    let row = shared.progress().queue(
        &call.label,
        call.phase.as_deref(),
        call.try_number,
        &call.prompt,
    );
    call.row = u32::try_from(row + 1).unwrap_or(u32::MAX);
    if saved.reused.is_some() {
        shared.host.replayed(&call);
    }
    shared.report();
    let Saved {
        index,
        fingerprint,
        reused,
    } = saved;
    let turn = reused.as_ref().map(|(turn, _)| *turn);
    let end = match reused {
        Some((turn, entry)) => {
            shared.replay.turn(turn).await;
            AgentEnd {
                result: entry.result.ok_or_else(String::new),
                model: entry.model,
                tokens: entry.tokens,
                tool_calls: entry.tool_calls,
                duration_ms: entry.duration_ms,
            }
        }
        None => {
            // A closed check means the run is ending, and the agent never starts.
            let Ok(_slot) = shared.slots.acquire().await else {
                return (id, Ok(Value::Null), None);
            };
            shared
                .progress()
                .started(row, call.model.clone(), shared.host.now_ms());
            shared.report();
            let end = shared.host.agent(&call).await;
            shared.live.fetch_add(1, Ordering::Relaxed);
            shared.host.journal(&JournalEntry {
                index,
                kind: CallKind::Agent,
                fingerprint,
                label: call.label.clone(),
                result: end.result.clone().ok(),
                model: end.model.clone(),
                tokens: end.tokens,
                tool_calls: end.tool_calls,
                duration_ms: end.duration_ms,
            });
            end
        }
    };
    let totals = AgentTotals {
        model: end.model.clone(),
        tokens: end.tokens,
        tool_calls: end.tool_calls,
        duration_ms: end.duration_ms,
    };
    let answer = end.result.as_ref().ok().map(|value| match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    });
    shared.progress().ended(row, &totals, answer.as_deref());
    shared
        .spent
        .store(shared.host.tokens_spent(), Ordering::Relaxed);
    shared.report();
    (id, Ok(end.result.unwrap_or(Value::Null)), turn)
}

async fn check<H: Host + ?Sized>(
    shared: &Shared<'_, H>,
    payload: Value,
    saved: Saved,
    id: u64,
) -> Settled {
    let mut asked: Check = match serde_json::from_value(payload) {
        Ok(asked) => asked,
        Err(err) => return (id, Err(format!("check(condition, about): {err}")), None),
    };
    let Saved {
        index,
        fingerprint,
        reused,
    } = saved;
    asked.index = index;
    let turn = reused.as_ref().map(|(turn, _)| *turn);
    let reused = reused
        .and_then(|(_, entry)| entry.result)
        .and_then(|result| serde_json::from_value::<Checked>(result).ok());
    let checked = match reused {
        Some(checked) => {
            if let Some(turn) = turn {
                shared.replay.turn(turn).await;
            }
            checked
        }
        None => {
            let checked = shared.host.check(&asked).await;
            shared.host.journal(&JournalEntry {
                index,
                kind: CallKind::Check,
                fingerprint,
                label: asked.label.clone().unwrap_or_else(|| "goal".to_owned()),
                result: serde_json::to_value(&checked).ok(),
                model: None,
                tokens: 0,
                tool_calls: 0,
                duration_ms: 0,
            });
            checked
        }
    };
    (id, Ok(json!(checked)), turn)
}

/// A workflow a script runs as a step, the `nth` it ran: its agents join this run's rows, caps
/// and limit.
async fn nested<'a, H: Host + ?Sized>(
    shared: &'a Shared<'a, H>,
    payload: Value,
    input: &'a RunInput,
    id: u64,
    nth: u32,
) -> Settled {
    let Some(name) = payload.get("ref").and_then(Value::as_str) else {
        let why = "workflow(name, args) takes the name of a workflow in workflows/";
        return (id, Err(why.to_owned()), None);
    };
    let script = match shared.host.workflow(name) {
        Ok(script) => script,
        Err(why) => return (id, Err(why), None),
    };
    let child = RunInput {
        id: input.id.clone(),
        args: payload.get("args").cloned().unwrap_or(Value::Null),
        limits: input.limits.clone(),
        replay: Vec::new(),
        roles: input.roles.clone(),
        token_limit: input.token_limit,
    };
    match drive(&script, &child, shared, Nesting::step(nth)).await {
        Ok(value) => (id, Ok(value), None),
        Err(why) => (id, Err(format!("{name}: {why}")), None),
    }
}

/// A call's number among the run's calls, and the saved answer, with its place in the saved
/// order, when an earlier run asked the same.
struct Saved {
    index: u32,
    fingerprint: String,
    reused: Option<(usize, JournalEntry)>,
}

/// What can be answered again of what an earlier run saved, in the order it first finished:
/// each call's newest answer, up to the first call that failed.
fn answerable(saved: &[JournalEntry]) -> Vec<JournalEntry> {
    let mut earlier: Vec<JournalEntry> = Vec::with_capacity(saved.len());
    for entry in saved {
        earlier.retain(|known| known.index != entry.index);
        earlier.push(entry.clone());
    }
    let good = earlier
        .iter()
        .position(|entry| entry.result.is_none())
        .unwrap_or(earlier.len());
    earlier.truncate(good);
    earlier
}

/// Matches one script's calls to the ones an earlier run finished. A call asked the same way
/// in the same place gets its saved answer. The first one asked differently runs live, and so
/// does every call after it, since they may depend on its answer. A workflow run as a step
/// has a journal of its own, over its own block of call numbers.
struct Journal {
    /// What this script's calls are numbered from.
    base: u32,
    next: u32,
    diverged: bool,
}

impl Journal {
    fn new(base: u32) -> Self {
        Self {
            base,
            next: 0,
            diverged: false,
        }
    }

    fn next<H: Host + ?Sized>(
        &mut self,
        kind: CallKind,
        payload: &Value,
        shared: &Shared<'_, H>,
    ) -> Saved {
        let index = self.base.saturating_add(self.next);
        self.next += 1;
        let fingerprint = fingerprint(kind, payload);
        let place = shared.saved.iter().position(|entry| entry.index == index);
        let reused = place.filter(|_| !self.diverged).and_then(|place| {
            let entry = &shared.saved[place];
            let same = entry.kind == kind && entry.fingerprint == fingerprint;
            same.then(|| (place, entry.clone()))
        });
        // A call that was never saved, such as one still running when the run stopped, says
        // nothing about the calls after it. One that was saved and is now asked differently
        // does: the script took another path.
        if place.is_some() && reused.is_none() && !self.diverged {
            self.diverged = true;
            shared.replay.stop_waiting();
        }
        Saved {
            index,
            fingerprint,
            reused,
        }
    }
}
