//! Rehearses a script before it runs for real. Every `agent()` answers at once with a placeholder
//! its schema allows, no CLI starts, and nothing is saved or reported. A script that throws on
//! the way, such as one that names a role the workflow doesn't have or reads a field of an
//! answer that is missing, is caught here, where it can still be written again, and not after
//! its agents did half the work.
//!
//! Placeholder answers can't follow every path a real run takes, so a rehearsal only ever says
//! "this threw". One that is still asking for agents when its allowance runs out says nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use argonauts_core::settings::crew::CrewRole;
use futures::future::BoxFuture;
use serde_json::{Map, Value, json};

use crate::script::Script;
use crate::script::call::{AgentCall, AgentEnd, Check, Checked, Host};
use crate::script::run::{RunInput, run};
use crate::script::sandbox::{Limits, ScriptEnd};

/// The most agents a rehearsal answers. Past it the script is taken to work.
const AGENTS: u32 = 200;

/// How deep a schema is followed before a placeholder stops at null.
const DEPTH: usize = 16;

/// How a rehearsal went, when the script didn't throw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rehearsal {
    /// Agents the script asked for.
    pub agents: u32,
    /// The script reached its end. `false` when it was still asking when the allowance ran out.
    pub finished: bool,
    /// The tasks its agents worked on, each once, in the order the script first named them:
    /// what it plans when its agents answer with placeholders, such as a task list you fixed.
    pub tasks: Vec<String>,
    /// Each phase that started an agent, with the role it went to: whether a phase a step
    /// requires does that step's work, not only carries its name.
    pub ran: BTreeSet<(String, CrewRole)>,
    /// Every agent it asked for, in the order it asked: what a kept step is checked by, since a
    /// planner names its phases for each request.
    pub asked: Vec<Asked>,
}

/// One agent a rehearsed script asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    pub label: String,
    pub phase: Option<String>,
    pub role: CrewRole,
    /// Which of its role's models takes it, from 0.
    pub model_index: u32,
    pub task: Option<String>,
    pub writes: bool,
    pub not_the_author: bool,
    /// Its schema asks for a yes-or-no `accept`, the one field a verdict is read by.
    pub gives_verdict: bool,
}

impl Asked {
    fn of(call: &AgentCall) -> Self {
        Self {
            label: call.label.clone(),
            phase: call.phase.clone(),
            role: call.role,
            model_index: call.model_index,
            task: call.task.clone(),
            writes: call.writes,
            not_the_author: call.not_the_author,
            gives_verdict: call.schema.as_ref().is_some_and(gives_verdict),
        }
    }
}

/// Whether a schema requires a boolean `accept` at its root, so every answer to it says yes or
/// no.
pub fn gives_verdict(schema: &Value) -> bool {
    let boolean = schema
        .pointer("/properties/accept/type")
        .and_then(Value::as_str)
        == Some("boolean");
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|required| required.iter().any(|name| name == "accept"));
    boolean && required
}

/// Runs `script` against placeholder answers. `workflows` loads another workflow a script runs as
/// a step. The error is what the script threw, in words its author can act on.
pub async fn rehearse(
    script: &Script,
    input: RunInput,
    workflows: &(dyn Fn(&str) -> Result<Script, String> + Sync),
) -> Result<Rehearsal, String> {
    let stage = Stage {
        workflows,
        asked: AtomicU32::new(0),
        seen: Mutex::default(),
        tasks: Mutex::default(),
        ran: Mutex::default(),
        calls: Mutex::default(),
    };
    let input = RunInput {
        replay: Vec::new(),
        token_limit: None,
        limits: Limits {
            // One more than the stage answers, so the stage sees the script go past its
            // allowance before the runner's own cap throws in the script.
            agents: AGENTS + 1,
            slice: Duration::from_secs(2),
            total: Duration::from_secs(5),
            ..input.limits
        },
        ..input
    };
    let ended = run(script, input, &stage).await;
    let agents = stage.asked.load(Ordering::Relaxed);
    let ran_out = agents > AGENTS;
    let tasks = stage
        .tasks
        .into_inner()
        .unwrap_or_else(|held| held.into_inner());
    let ran = stage
        .ran
        .into_inner()
        .unwrap_or_else(|held| held.into_inner());
    let asked = stage
        .calls
        .into_inner()
        .unwrap_or_else(|held| held.into_inner());
    match ended.result {
        Ok(_) => Ok(Rehearsal {
            agents,
            finished: true,
            tasks,
            ran,
            asked,
        }),
        Err(_) if ran_out => Ok(Rehearsal {
            agents: AGENTS,
            finished: false,
            tasks,
            ran,
            asked,
        }),
        Err(ScriptEnd::Threw(why)) => Err(format!(
            "rehearsed with placeholder answers, it threw: {}",
            first_line(&why)
        )),
        Err(why @ (ScriptEnd::TimedOut | ScriptEnd::Stuck)) => Err(why.to_string()),
        // The engine's own trouble says nothing about the script.
        Err(ScriptEnd::Stopped | ScriptEnd::Engine(_)) => Ok(Rehearsal {
            agents,
            finished: false,
            tasks,
            ran,
            asked,
        }),
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or(text).trim()
}

struct Stage<'a> {
    workflows: &'a (dyn Fn(&str) -> Result<Script, String> + Sync),
    asked: AtomicU32,
    /// How many agents each role has answered in each phase, which decides a placeholder's yes
    /// or no.
    seen: Mutex<BTreeMap<String, u32>>,
    tasks: Mutex<Vec<String>>,
    ran: Mutex<BTreeSet<(String, CrewRole)>>,
    calls: Mutex<Vec<Asked>>,
}

impl Host for Stage<'_> {
    fn agent<'a>(&'a self, call: &'a AgentCall) -> BoxFuture<'a, AgentEnd> {
        Box::pin(async move {
            if self.asked.fetch_add(1, Ordering::Relaxed) >= AGENTS {
                return AgentEnd::failed("the rehearsal is over");
            }
            self.calls
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .push(Asked::of(call));
            if let Some(task) = &call.task {
                let mut tasks = self.tasks.lock().unwrap_or_else(|held| held.into_inner());
                if !tasks.contains(task) {
                    tasks.push(task.clone());
                }
            }
            if let Some(phase) = &call.phase {
                self.ran
                    .lock()
                    .unwrap_or_else(|held| held.into_inner())
                    .insert((phase.clone(), call.role));
            }
            let earlier = {
                let place = format!("{}|{}", call.role, call.phase.as_deref().unwrap_or(""));
                let mut seen = self.seen.lock().unwrap_or_else(|held| held.into_inner());
                let earlier = seen.entry(place).or_insert(0);
                *earlier += 1;
                *earlier - 1
            };
            AgentEnd::answered(match &call.schema {
                Some(schema) => placeholder(schema, schema, earlier, 0),
                None => json!("A placeholder answer."),
            })
        })
    }

    fn check<'a>(&'a self, _check: &'a Check) -> BoxFuture<'a, Checked> {
        // A goal that holds at once keeps a rehearsal from going round.
        Box::pin(async {
            Checked {
                holds: true,
                ..Checked::default()
            }
        })
    }

    fn workflow(&self, name: &str) -> Result<Script, String> {
        (self.workflows)(name)
    }

    fn now_ms(&self) -> u64 {
        0
    }
}

/// A value the schema allows, made to keep a script moving: every property filled, one item
/// in each list, and a yes-or-no that says no the first time its role answers in a phase and
/// yes after, so a loop that waits for a yes goes round once and then ends.
fn placeholder(root: &Value, schema: &Value, earlier: u32, depth: usize) -> Value {
    if depth > DEPTH {
        return Value::Null;
    }
    let next = |schema: &Value| placeholder(root, schema, earlier, depth + 1);
    if let Some(target) = schema.get("$ref").and_then(Value::as_str) {
        return resolve(root, target).map_or(Value::Null, next);
    }
    if let Some(constant) = schema.get("const") {
        return constant.clone();
    }
    if let Some(first) = schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
    {
        return first.clone();
    }
    for any in ["anyOf", "oneOf", "allOf"] {
        if let Some(first) = schema
            .get(any)
            .and_then(Value::as_array)
            .and_then(|branches| branches.iter().find(|branch| !is_null(branch)))
        {
            return next(first);
        }
    }
    match kind(schema) {
        Some("object") => {
            let properties = schema.get("properties").and_then(Value::as_object);
            let filled: Map<String, Value> = properties
                .into_iter()
                .flatten()
                .map(|(name, property)| (name.clone(), next(property)))
                .collect();
            Value::Object(filled)
        }
        Some("array") => {
            let none = schema.get("maxItems").and_then(Value::as_u64) == Some(0);
            match schema.get("items") {
                Some(items) if !none => json!([next(items)]),
                _ => json!([]),
            }
        }
        Some("boolean") => json!(earlier > 0),
        Some("integer" | "number") => schema.get("minimum").cloned().unwrap_or_else(|| json!(1)),
        Some("string") => json!("placeholder"),
        _ => Value::Null,
    }
}

/// The schema's type, or the first one that isn't null when it lists several.
fn kind(schema: &Value) -> Option<&str> {
    match schema.get("type")? {
        Value::String(kind) => Some(kind.as_str()),
        Value::Array(kinds) => kinds
            .iter()
            .filter_map(Value::as_str)
            .find(|kind| *kind != "null"),
        _ => None,
    }
}

fn is_null(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("null")
}

/// `#/$defs/name` and `#/definitions/name`, which are all a schema in a script uses.
fn resolve<'a>(root: &'a Value, target: &str) -> Option<&'a Value> {
    root.pointer(target.strip_prefix('#')?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An answer may leave out a field the format doesn't require, and then says neither yes
    /// nor no.
    #[test]
    fn a_verdict_needs_accept_required_and_boolean() {
        let accept = json!({"accept": {"type": "boolean"}, "findings": {"type": "array"}});
        let required = json!({"type": "object", "properties": accept, "required": ["accept"]});
        assert!(gives_verdict(&required));
        let optional = json!({"type": "object", "properties": accept});
        assert!(!gives_verdict(&optional));
        let others = json!({"type": "object", "properties": accept, "required": ["findings"]});
        assert!(!gives_verdict(&others));
        let text = json!({
            "type": "object",
            "properties": {"accept": {"type": "string"}},
            "required": ["accept"],
        });
        assert!(!gives_verdict(&text));
    }

    #[test]
    fn a_placeholder_fills_every_property_and_one_item_of_each_list() {
        let schema = json!({
            "type": "object",
            "properties": {
                "summary": {"type": "string"},
                "tasks": {"type": "array", "items": {"$ref": "#/$defs/task"}},
                "risk": {"enum": ["low", "high"]},
                "rounds": {"type": "integer", "minimum": 2},
                "note": {"type": ["null", "string"]},
                "extra": {"anyOf": [{"type": "null"}, {"type": "number"}]},
            },
            "required": ["tasks"],
            "$defs": {"task": {"type": "object", "properties": {"title": {"type": "string"}}}},
        });
        assert_eq!(
            placeholder(&schema, &schema, 0, 0),
            json!({
                "summary": "placeholder",
                "tasks": [{"title": "placeholder"}],
                "risk": "low",
                "rounds": 2,
                "note": "placeholder",
                "extra": 1,
            })
        );
    }

    #[test]
    fn a_yes_or_no_says_no_first_and_yes_after() {
        let schema = json!({"type": "object", "properties": {"accept": {"type": "boolean"}}});
        assert_eq!(
            placeholder(&schema, &schema, 0, 0),
            json!({"accept": false})
        );
        assert_eq!(placeholder(&schema, &schema, 1, 0), json!({"accept": true}));
    }

    #[test]
    fn a_schema_that_points_at_itself_ends() {
        let schema = json!({
            "type": "object",
            "properties": {"child": {"$ref": "#"}},
        });
        // It stops at the depth cap instead of going down forever.
        let made = placeholder(&schema, &schema, 0, 0);
        assert!(made.get("child").is_some());
    }
}
