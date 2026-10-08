//! What a script asks the host for, and what the host answers.

use argonauts_core::canonical_json;
use argonauts_core::events::session::VendorWorkflow;
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::sha256_hex;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::sandbox::CallKind;
use crate::script::Script;

/// One `agent()` call: who should take it and what it's asked.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentCall {
    /// Which call of the run this is, the same when a run picks its script up again: from 1 in
    /// the order the script called, and in a block of its own for each workflow the script
    /// ran as a step.
    pub index: u32,
    /// Its row in the workflow's report, from 1. Rows fill in the order agents queue, which
    /// can differ between two runs of the same script, so nothing is kept by it.
    pub row: u32,
    /// What its row is called, such as `build:add-due`.
    pub label: String,
    pub phase: Option<String>,
    /// The role whose CLI and model run it.
    pub role: CrewRole,
    /// Which of the role's seats, from 0, when a step starts several at once.
    pub seat: u32,
    pub prompt: String,
    /// The JSON Schema its answer must fit. Without one the answer is its final text.
    pub schema: Option<Value>,
    /// A model of its own, as `cli/model`, in place of the role's.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// The task it works on. A writer gets that task's worktree, and a reviewer its change.
    pub task: Option<String>,
    /// A fresh worktree of its own even without a task.
    pub worktree: bool,
    /// It changes files. Workers do unless the call says `access: 'read'`, such as one that
    /// writes a plan or reviews. Nobody else does.
    pub writes: bool,
    /// It mustn't be the agent that built the task.
    pub not_the_author: bool,
    /// It doesn't see what its peers made.
    pub independent: bool,
    /// 1 the first time. Counts up when the work is sent back or a goal doesn't hold yet.
    pub attempt: u32,
    /// The goal its step keeps it to, and who keeps it (`cli` or `argonauts`).
    pub until: Option<String>,
    pub kept_by: Option<String>,
    /// A step you require in every workflow.
    pub required: bool,
    /// It may run a workflow of its own, where its CLI has one.
    pub own_workflow: bool,
    pub agent_type: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Asked {
    prompt: String,
    label: Option<String>,
    phase: Option<String>,
    role: Option<String>,
    seat: Option<u32>,
    schema: Option<Value>,
    model: Option<String>,
    effort: Option<String>,
    task: Option<String>,
    isolation: Option<String>,
    access: Option<String>,
    #[serde(default)]
    not_the_author: bool,
    #[serde(default)]
    independent: bool,
    attempt: Option<u32>,
    until: Option<String>,
    kept_by: Option<String>,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    own_workflow: bool,
    agent_type: Option<String>,
}

impl AgentCall {
    /// Reads what the script passed to `agent()`, the `index`th its script asked for. The
    /// error is what the script's call throws.
    pub(crate) fn read(index: u32, payload: &Value) -> Result<Self, String> {
        let asked: Asked = serde_json::from_value(payload.clone())
            .map_err(|err| format!("agent(prompt, opts): {err}"))?;
        // A script saved in Claude Code names a kind of agent, and a role's name there counts.
        let named = asked.role.as_deref().or(asked.agent_type.as_deref());
        let role = match named.map(str::parse::<CrewRole>) {
            Some(Ok(role)) => role,
            Some(Err(err)) if asked.role.is_some() => return Err(format!("agent(): {err}")),
            _ => CrewRole::Workers,
        };
        if let Some(schema) = &asked.schema {
            check_schema(schema)?;
        }
        let label = asked.label.unwrap_or_else(|| match &asked.phase {
            Some(phase) => format!("{}:{index}", phase.to_lowercase().replace(' ', "-")),
            None => format!("{role}:{index}"),
        });
        Ok(Self {
            index,
            row: 0,
            label,
            phase: asked.phase,
            role,
            seat: asked.seat.unwrap_or(0),
            prompt: asked.prompt,
            schema: asked.schema,
            model: asked.model,
            effort: asked.effort,
            task: asked.task,
            worktree: asked.isolation.as_deref() == Some("worktree"),
            writes: role.writes() && asked.access.as_deref() != Some("read"),
            not_the_author: asked.not_the_author,
            independent: asked.independent,
            attempt: asked.attempt.unwrap_or(1).max(1),
            until: asked.until,
            kept_by: asked.kept_by,
            required: asked.required,
            own_workflow: asked.own_workflow,
            agent_type: asked.agent_type,
        })
    }
}

/// A schema an answer could never fit is refused when the agent is asked for, as in Claude Code:
/// an object at the root, and nothing required that isn't a property.
fn check_schema(schema: &Value) -> Result<(), String> {
    let refuse = |why: &str| Err(format!("agent(): the schema {why}"));
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return refuse("needs { type: 'object', properties: {…} } at its root");
    }
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return refuse("needs properties at its root");
    };
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    match required
        .iter()
        .filter_map(Value::as_str)
        .find(|name| !properties.contains_key(*name))
    {
        Some(missing) => refuse(&format!(
            "requires {missing}, which isn't one of its properties"
        )),
        None => Ok(()),
    }
}

/// How an agent ended.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentEnd {
    /// Its answer: text, or the object its schema asked for. `Err` says why there is none, and
    /// the script's `agent()` returns null.
    pub result: Result<Value, String>,
    /// The model that ran it, as the view should name it.
    pub model: Option<String>,
    /// Input plus output tokens.
    pub tokens: u64,
    pub tool_calls: u32,
    pub duration_ms: u64,
}

impl AgentEnd {
    pub fn answered(result: Value) -> Self {
        Self {
            result: Ok(result),
            model: None,
            tokens: 0,
            tool_calls: 0,
            duration_ms: 0,
        }
    }

    pub fn failed(why: impl Into<String>) -> Self {
        Self {
            result: Err(why.into()),
            ..Self::answered(Value::Null)
        }
    }
}

/// A condition to check: a step's `until`, or a file's `goal`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub condition: String,
    /// The step it's on. `None` for the file's goal.
    pub step: Option<String>,
    pub task: Option<String>,
    /// The agent that just tried, by its label.
    pub label: Option<String>,
    pub kept_by: Option<String>,
    /// Which try of the step this follows, from 1.
    pub attempt: Option<u32>,
    /// Which round of the whole file this follows, from 1.
    pub round: Option<u32>,
    /// Its place among the script's calls, from 0, which is the same when a run picks the
    /// script up again.
    #[serde(default)]
    pub index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Checked {
    pub holds: bool,
    /// What still fails, for the next try.
    #[serde(default)]
    pub failed: String,
    /// Stop trying even though it doesn't hold: it is stuck, or you paused it.
    #[serde(default)]
    pub stop: bool,
}

/// One attempt a judge kept of several.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Kept {
    /// The step whose attempts were compared.
    pub step: String,
    /// The seat kept, from 0.
    pub seat: u32,
    /// The label of the agent that made it.
    pub label: String,
    #[serde(default)]
    pub why: Option<String>,
}

/// A finished call, saved so a run that stops can pick up where it was: the same call, asked
/// the same way, gets the saved answer and nothing runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// The call's number in the run: from 0 in the order its script called, and in a block of
    /// its own for each workflow run as a step.
    pub index: u32,
    pub kind: CallKind,
    /// A hash of what was asked. A call asked differently runs again, and so does every call
    /// after it.
    pub fingerprint: String,
    pub label: String,
    /// `None` for an agent that failed, which runs again.
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub tokens: u64,
    #[serde(default)]
    pub tool_calls: u32,
    #[serde(default)]
    pub duration_ms: u64,
}

pub(crate) fn fingerprint(kind: CallKind, payload: &Value) -> String {
    sha256_hex(canonical_json(&json!({"kind": kind, "asked": payload})).as_bytes())
}

/// Where a script's calls go. The crew answers with its sessions; a test answers from a table.
pub trait Host: Sync {
    /// Runs one agent to its end, on the CLI and model its role names.
    fn agent<'a>(&'a self, call: &'a AgentCall) -> BoxFuture<'a, AgentEnd>;

    /// Whether a condition holds yet.
    fn check<'a>(&'a self, check: &'a Check) -> BoxFuture<'a, Checked>;

    /// Another workflow a script runs as a step, by name.
    fn workflow(&self, name: &str) -> Result<Script, String> {
        Err(format!("there's no workflow named {name}"))
    }

    /// An agent an earlier run of this script finished, now answered from what was saved. No
    /// [`Host::agent`] comes for it, and this says which row it took.
    fn replayed(&self, _call: &AgentCall) {}

    /// The whole state of the workflow, on every change.
    fn report(&self, _workflow: &VendorWorkflow) {}

    /// A line the script logged.
    fn log(&self, _message: &str) {}

    /// A judge kept one attempt of several.
    fn keep(&self, _kept: &Kept) {}

    /// A call that just finished, to save for a later resume.
    fn journal(&self, _entry: &JournalEntry) {}

    /// Tokens the run has used so far, which `budget.spent()` reads.
    fn tokens_spent(&self) -> u64 {
        0
    }

    /// Milliseconds since the Unix epoch. Only the runner reads a clock, never the script.
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| u64::try_from(since.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_call_reads_its_role_and_gets_a_label() {
        let call = AgentCall::read(
            3,
            &json!({"prompt": "review it", "role": "reviewers", "phase": "Sign Off", "task": "add-due",
                    "notTheAuthor": true, "isolation": "worktree"}),
        )
        .expect("reads");
        assert_eq!(call.role, CrewRole::Reviewers);
        assert_eq!(call.label, "sign-off:3");
        assert!(call.not_the_author && call.worktree);
        assert!(!call.writes, "a reviewer reads");
        assert_eq!(call.attempt, 1);
        let plain = AgentCall::read(1, &json!({"prompt": "do it"})).expect("reads");
        assert_eq!(
            (plain.role, plain.label.as_str()),
            (CrewRole::Workers, "workers:1")
        );
        assert!(plain.writes);
        let planning = json!({"prompt": "propose a plan", "role": "workers", "access": "read"});
        assert!(!AgentCall::read(1, &planning).expect("reads").writes);
    }

    #[test]
    fn a_role_nobody_has_or_a_schema_nothing_fits_is_refused() {
        let role = AgentCall::read(1, &json!({"prompt": "x", "role": "owner"})).unwrap_err();
        assert!(role.contains("unknown role"), "{role}");
        // A kind of agent Claude Code knows and Argonauts doesn't is a worker.
        let typed = AgentCall::read(1, &json!({"prompt": "x", "agentType": "code-reviewer"}));
        assert_eq!(typed.expect("reads").role, CrewRole::Workers);
        let schema = json!({"type": "object", "properties": {"a": {}}, "required": ["b"]});
        let refused = AgentCall::read(1, &json!({"prompt": "x", "schema": schema})).unwrap_err();
        assert!(refused.contains("requires b"), "{refused}");
    }

    #[test]
    fn the_same_ask_has_the_same_fingerprint_whatever_the_key_order() {
        let a = fingerprint(CallKind::Agent, &json!({"prompt": "x", "role": "judge"}));
        let b = fingerprint(CallKind::Agent, &json!({"role": "judge", "prompt": "x"}));
        let c = fingerprint(CallKind::Agent, &json!({"role": "judge", "prompt": "y"}));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
