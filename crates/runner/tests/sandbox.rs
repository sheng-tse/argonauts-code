//! What a script can't do: read a clock, roll dice, reach a file or the network, run forever,
//! or take all the memory.

mod support;

use std::time::{Duration, Instant};

use argonauts_runner::{Limits, RunInput, Script, ScriptEnd, run};
use serde_json::{Value, json};
use support::{Fake, text};

fn script(body: &str) -> Script {
    let source = format!(
        "export const meta = {{ name: 'probe', description: 'Probes the sandbox' }}\n{body}\n"
    );
    Script::parse(&source).unwrap_or_else(|err| panic!("{err}"))
}

async fn result(body: &str, limits: Limits) -> Result<Value, ScriptEnd> {
    let host = Fake::new(|_| text("ok"));
    let input = RunInput {
        id: "wf-1".to_owned(),
        limits,
        ..RunInput::default()
    };
    run(&script(body), input, &host).await.result
}

#[tokio::test]
async fn there_is_no_clock_and_no_dice() {
    let body = r#"
const tried = {}
for (const [name, probe] of Object.entries({
  now: () => Date.now(),
  today: () => new Date(),
  random: () => Math.random(),
  // Ways round to the engine's own Date.
  behind: () => Object.getPrototypeOf(Date).now(),
  called: () => Date(),
  constructor: () => new Date.prototype.constructor(),
  constructorNow: () => Date.prototype.constructor.now(),
})) {
  try { probe(); tried[name] = 'worked' } catch (error) { tried[name] = error.message.split(',')[0] }
}
// A date the script names itself is still a date.
tried.fixed = new Date(Date.UTC(2026, 9, 7)).toISOString()
tried.parsed = Date.parse('2026-10-07T00:00:00Z')
return tried
"#;
    assert_eq!(
        result(body, Limits::default()).await,
        Ok(json!({
            "now": "Date.now() isn't available in a workflow",
            "today": "new Date() isn't available in a workflow",
            "random": "Math.random() isn't available in a workflow",
            "behind": "not a function",
            "called": "Date() isn't available in a workflow",
            "constructor": "new Date() isn't available in a workflow",
            "constructorNow": "Date.now() isn't available in a workflow",
            "fixed": "2026-10-07T00:00:00.000Z",
            "parsed": 1_791_331_200_000_i64,
        }))
    );
}

#[tokio::test]
async fn nothing_reaches_a_file_the_network_or_the_process() {
    let body = r#"
const names = ['require', 'process', 'fetch', 'XMLHttpRequest', 'WebSocket', 'Deno', 'Bun', 'std',
  'os', 'performance', 'setTimeout', 'setInterval', 'queueMicrotask', 'print', 'console',
  'navigator', 'Atomics', 'SharedArrayBuffer', 'WeakRef', 'FinalizationRegistry', 'scriptArgs',
  'bjson', 'gc']
const there = names.filter((name) => typeof globalThis[name] !== 'undefined')
let imported
try { await import('fs'); imported = 'worked' } catch (error) { imported = 'refused' }
const hooks = Object.getOwnPropertyNames(globalThis).filter((name) => name.startsWith('__'))
return { there, imported, hooks }
"#;
    assert_eq!(
        result(body, Limits::default()).await,
        Ok(json!({"there": [], "imported": "refused", "hooks": []}))
    );
}

#[tokio::test]
async fn the_helpers_cannot_be_swapped_out() {
    let body = r#"
'use strict'
let swapped = 'no'
try { agent = () => 'mine'; swapped = 'yes' } catch (_) {}
try { globalThis.agent = () => 'mine'; swapped = 'yes' } catch (_) {}
return { swapped, answer: await agent('real') }
"#;
    assert_eq!(
        result(body, Limits::default()).await,
        Ok(json!({"swapped": "no", "answer": "ok"}))
    );
}

#[tokio::test]
async fn a_script_that_never_waits_is_stopped() {
    let limits = Limits {
        slice: Duration::from_millis(150),
        ..Limits::default()
    };
    let began = Instant::now();
    assert_eq!(
        result("while (true) {}", limits).await,
        Err(ScriptEnd::TimedOut)
    );
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "{:?}",
        began.elapsed()
    );
}

#[tokio::test]
async fn the_time_a_script_runs_adds_up_across_its_waits() {
    let limits = Limits {
        slice: Duration::from_millis(400),
        total: Duration::from_millis(300),
        ..Limits::default()
    };
    // Each turn is short, but together they pass the total.
    let body = "for (let turn = 0; turn < 50; turn++) {\n  let n = 0\n  for (let i = 0; i < 3e6; i++) n += i\n  await agent('turn ' + turn)\n}\nreturn 'finished'";
    assert_eq!(result(body, limits).await, Err(ScriptEnd::TimedOut));
}

#[tokio::test]
async fn a_script_cannot_take_more_memory_than_its_cap() {
    let limits = Limits {
        memory_bytes: 8 * 1024 * 1024,
        ..Limits::default()
    };
    let body = "const hoard = []\nwhile (true) hoard.push(new Array(10000).fill('x'.repeat(64)))";
    match result(body, limits).await {
        Err(ScriptEnd::Threw(said)) => assert!(said.contains("out of memory"), "{said}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_script_cannot_recurse_past_its_stack() {
    let body = "const down = (n) => down(n + 1) + 1\ntry { down(0) } catch (error) { return error.constructor.name }";
    assert_eq!(
        result(body, Limits::default()).await,
        Ok(json!("RangeError"))
    );
}

#[tokio::test]
async fn an_await_with_no_agent_behind_it_ends_the_run() {
    assert_eq!(
        result("await new Promise(() => {})", Limits::default()).await,
        Err(ScriptEnd::Stuck)
    );
}

#[tokio::test]
async fn dropping_the_run_stops_a_script_and_its_agents() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Never(Arc<AtomicBool>);
    struct Flag(Arc<AtomicBool>);
    impl Drop for Flag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    impl argonauts_runner::Host for Never {
        fn agent<'a>(
            &'a self,
            _: &'a argonauts_runner::AgentCall,
        ) -> futures::future::BoxFuture<'a, argonauts_runner::AgentEnd> {
            let flag = Flag(Arc::clone(&self.0));
            Box::pin(async move {
                let _held = flag;
                std::future::pending().await
            })
        }
        fn check<'a>(
            &'a self,
            _: &'a argonauts_runner::Check,
        ) -> futures::future::BoxFuture<'a, argonauts_runner::Checked> {
            Box::pin(async { argonauts_runner::Checked::default() })
        }
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let host = Never(Arc::clone(&cancelled));
    let script = script("await agent('forever')");
    let input = RunInput {
        id: "wf-1".to_owned(),
        ..RunInput::default()
    };
    let stopped =
        tokio::time::timeout(Duration::from_millis(200), run(&script, input, &host)).await;
    assert!(
        stopped.is_err(),
        "the agent never ends, so only the timeout does"
    );
    assert!(
        cancelled.load(Ordering::SeqCst),
        "its agent was dropped with the run"
    );
}
