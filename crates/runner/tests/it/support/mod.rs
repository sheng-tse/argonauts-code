//! A host that answers from a function, and remembers everything it was asked.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use argonauts_core::runs::events::session::CliWorkflowRun;
use argonauts_runner::{
    AgentCall, AgentEnd, Check, Checked, Host, JournalEntry, Kept, RunInput, Script,
};
use futures::future::BoxFuture;
use serde_json::{Value, json};

type Answer = Box<dyn Fn(&AgentCall) -> AgentEnd + Send + Sync>;
type Checker = Box<dyn Fn(&Check) -> Checked + Send + Sync>;

pub struct Fake {
    answer: Answer,
    checker: Checker,
    pub workflows: HashMap<String, Script>,
    pub calls: Mutex<Vec<AgentCall>>,
    pub checks: Mutex<Vec<Check>>,
    pub reports: Mutex<Vec<CliWorkflowRun>>,
    pub journal: Mutex<Vec<JournalEntry>>,
    pub logs: Mutex<Vec<String>>,
    pub kept: Mutex<Vec<Kept>>,
    /// Each call answered from what an earlier run saved, as its number and its row.
    pub replayed: Mutex<Vec<(u32, u32)>>,
    pub tokens: AtomicU64,
    clock: AtomicU64,
}

impl Fake {
    pub fn new(answer: impl Fn(&AgentCall) -> AgentEnd + Send + Sync + 'static) -> Self {
        Self {
            answer: Box::new(answer),
            checker: Box::new(|_| Checked {
                holds: true,
                ..Checked::default()
            }),
            workflows: HashMap::new(),
            calls: Mutex::default(),
            checks: Mutex::default(),
            reports: Mutex::default(),
            journal: Mutex::default(),
            logs: Mutex::default(),
            kept: Mutex::default(),
            replayed: Mutex::default(),
            tokens: AtomicU64::new(0),
            clock: AtomicU64::new(1_000),
        }
    }

    pub fn checking(mut self, checker: impl Fn(&Check) -> Checked + Send + Sync + 'static) -> Self {
        self.checker = Box::new(checker);
        self
    }

    /// Each call as `label role try`, in the order the script asked.
    pub fn asked(&self) -> Vec<String> {
        let mut calls = self.calls.lock().unwrap().clone();
        calls.sort_by_key(|call| call.index);
        calls
            .iter()
            .map(|call| format!("{} {} {}", call.label, call.role, call.try_number))
            .collect()
    }

    pub fn call(&self, label: &str, try_number: u32) -> AgentCall {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .find(|call| call.label == label && call.try_number == try_number)
            .cloned()
            .unwrap_or_else(|| panic!("no call {label} #{try_number}"))
    }

    pub fn last_report(&self) -> CliWorkflowRun {
        self.reports
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("a report")
    }
}

impl Host for Fake {
    fn agent<'a>(&'a self, call: &'a AgentCall) -> BoxFuture<'a, AgentEnd> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(call.clone());
            let end = (self.answer)(call);
            self.tokens.fetch_add(end.tokens, Ordering::Relaxed);
            end
        })
    }

    fn check<'a>(&'a self, check: &'a Check) -> BoxFuture<'a, Checked> {
        Box::pin(async move {
            self.checks.lock().unwrap().push(check.clone());
            (self.checker)(check)
        })
    }

    fn workflow(&self, name: &str) -> Result<Script, String> {
        self.workflows
            .get(name)
            .cloned()
            .ok_or_else(|| format!("there's no workflow named {name}"))
    }

    fn replayed(&self, call: &AgentCall) {
        self.replayed.lock().unwrap().push((call.index, call.row));
    }

    fn report(&self, workflow: &CliWorkflowRun) {
        self.reports.lock().unwrap().push(workflow.clone());
    }

    fn log(&self, message: &str) {
        self.logs.lock().unwrap().push(message.to_owned());
    }

    fn keep(&self, kept: &Kept) {
        self.kept.lock().unwrap().push(kept.clone());
    }

    fn journal(&self, entry: &JournalEntry) {
        self.journal.lock().unwrap().push(entry.clone());
    }

    fn tokens_spent(&self) -> u64 {
        self.tokens.load(Ordering::Relaxed)
    }

    fn now_ms(&self) -> u64 {
        self.clock.fetch_add(10, Ordering::Relaxed)
    }
}

/// An answer with a model's name and some tokens on it, as a real agent's has.
pub fn said(value: Value) -> AgentEnd {
    AgentEnd {
        model: Some("GPT 5.6".to_owned()),
        tokens: 1_000,
        tool_calls: 2,
        duration_ms: 50,
        ..AgentEnd::answered(value)
    }
}

pub fn text(words: &str) -> AgentEnd {
    said(Value::String(words.to_owned()))
}

pub fn accept() -> AgentEnd {
    said(json!({"accept": true, "findings": []}))
}

pub fn reject(findings: &[&str]) -> AgentEnd {
    said(json!({"accept": false, "findings": findings}))
}

pub fn tasks(titles: &[&str]) -> AgentEnd {
    let tasks: Vec<Value> = titles
        .iter()
        .map(|title| json!({"title": title, "done_when": [format!("{title} works")]}))
        .collect();
    said(json!({"summary": "the plan", "tasks": tasks}))
}

/// What a file of steps is started with: the request, and how many models each role has.
pub fn request(request: &str, workers: u32, reviewers: u32) -> RunInput {
    RunInput {
        id: "wf-1".to_owned(),
        args: json!({
            "request": request,
            "crew": {"main": 1, "orchestrator": 1, "worker": workers, "reviewer": reviewers, "judge": 1},
        }),
        ..RunInput::default()
    }
}
