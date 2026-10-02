//! Comparing two workflows step by step. Phases pair up by name and steps by a longest common
//! subsequence, so inserting one step is one change rather than a shift of everything after it.
//! A step that only moved is one change too, which is how gate order can evolve.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use argonauts_core::workflow::{Step, StepKind, Workflow};

use super::keys::WORKFLOW_KEY;
use super::{Change, ChangeClass};

/// Step kinds that produce the evidence an approval reads or guard what lands. The mutation
/// rules refuse removing or weakening any of them.
pub const PROTECTED_KINDS: &[StepKind] = &[
    StepKind::Gate,
    StepKind::Review,
    StepKind::Judge,
    StepKind::Refute,
    StepKind::Differ,
    StepKind::Map,
    StepKind::Checkpoint,
    StepKind::Merge,
];

pub(super) fn compare(before: &Workflow, after: &Workflow, out: &mut Vec<Change>) {
    let params: BTreeSet<&String> = before
        .params
        .0
        .keys()
        .chain(after.params.0.keys())
        .collect();
    for param in params {
        let (old, new) = (before.params.0.get(param), after.params.0.get(param));
        if old != new {
            out.push(step_change(
                old.map(|value| json!({ "param": param, "value": value })),
                new.map(|value| json!({ "param": param, "value": value })),
            ));
        }
    }

    let names = |workflow: &Workflow| -> Vec<String> {
        workflow
            .phases
            .iter()
            .map(|phase| phase.name.clone())
            .collect()
    };
    for edit in align(&names(before), &names(after), |a, b| a == b) {
        match edit {
            Edit::Same(i, j) => {
                let (old, new) = (&before.phases[i], &after.phases[j]);
                if old.standard != new.standard {
                    out.push(step_change(
                        Some(json!({ "phase": old.name, "standard": old.standard })),
                        Some(json!({ "phase": new.name, "standard": new.standard })),
                    ));
                }
                compare_steps(&old.steps, &new.steps, out);
            }
            Edit::Removed(i) => {
                let phase = &before.phases[i];
                if phase.steps.is_empty() {
                    out.push(step_change(Some(json!({ "phase": phase.name })), None));
                }
                for step in &phase.steps {
                    out.push(step_change(Some(step_json(step)), None));
                }
            }
            Edit::Added(j) => {
                let phase = &after.phases[j];
                if phase.steps.is_empty() {
                    out.push(step_change(None, Some(json!({ "phase": phase.name }))));
                }
                for step in &phase.steps {
                    out.push(step_change(None, Some(step_json(step))));
                }
            }
        }
    }
}

fn compare_steps(before: &[Step], after: &[Step], out: &mut Vec<Change>) {
    let old: Vec<Value> = before.iter().map(step_json).collect();
    let new: Vec<Value> = after.iter().map(step_json).collect();

    // Unmatched steps, grouped by the gap between two steps both versions share.
    let mut gaps: Vec<(Vec<usize>, Vec<usize>)> = vec![(Vec::new(), Vec::new())];
    for edit in align(&old, &new, |a, b| a == b) {
        match edit {
            Edit::Same(..) => gaps.push((Vec::new(), Vec::new())),
            Edit::Removed(i) => {
                if let Some(gap) = gaps.last_mut() {
                    gap.0.push(i);
                }
            }
            Edit::Added(j) => {
                if let Some(gap) = gaps.last_mut() {
                    gap.1.push(j);
                }
            }
        }
    }

    let mut moved_from = BTreeSet::new();
    let mut moved_to = BTreeSet::new();
    for &i in gaps.iter().flat_map(|gap| &gap.0) {
        let target = gaps
            .iter()
            .flat_map(|gap| &gap.1)
            .copied()
            .find(|j| !moved_to.contains(j) && new[*j] == old[i]);
        if let Some(j) = target {
            moved_from.insert(i);
            moved_to.insert(j);
            out.push(step_change(Some(old[i].clone()), Some(new[j].clone())));
        }
    }

    for (removed, added) in &gaps {
        let removed: Vec<usize> = removed
            .iter()
            .copied()
            .filter(|i| !moved_from.contains(i))
            .collect();
        let added: Vec<usize> = added
            .iter()
            .copied()
            .filter(|j| !moved_to.contains(j))
            .collect();
        let paired = removed.len().min(added.len());
        for (&i, &j) in removed.iter().zip(&added) {
            if same_but_nested(&before[i], &after[j], &old[i], &new[j]) {
                compare_steps(&before[i].steps, &after[j].steps, out);
            } else {
                out.push(step_change(Some(old[i].clone()), Some(new[j].clone())));
            }
        }
        for &i in &removed[paired..] {
            out.push(step_change(Some(old[i].clone()), None));
        }
        for &j in &added[paired..] {
            out.push(step_change(None, Some(new[j].clone())));
        }
    }
}

/// Two loop or `for_each` steps that differ only inside: compare what they repeat instead.
fn same_but_nested(before: &Step, after: &Step, old: &Value, new: &Value) -> bool {
    if before.kind != after.kind || (before.steps.is_empty() && after.steps.is_empty()) {
        return false;
    }
    let outer = |value: &Value| {
        let mut value = value.clone();
        if let Value::Object(map) = &mut value {
            map.remove("steps");
        }
        value
    };
    outer(old) == outer(new)
}

pub(super) fn step_json(step: &Step) -> Value {
    serde_json::to_value(step).unwrap_or(Value::Null)
}

fn step_change(before: Option<Value>, after: Option<Value>) -> Change {
    Change {
        key: WORKFLOW_KEY.to_owned(),
        class: ChangeClass::StepGraph,
        before,
        after,
    }
}

enum Edit {
    Same(usize, usize),
    Removed(usize),
    Added(usize),
}

/// A shortest edit script by longest common subsequence. Workflows have tens of steps, so the
/// quadratic table is cheap.
fn align<T>(a: &[T], b: &[T], same: impl Fn(&T, &T) -> bool) -> Vec<Edit> {
    let (n, m) = (a.len(), b.len());
    let mut common = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            common[i][j] = if same(&a[i], &b[j]) {
                common[i + 1][j + 1] + 1
            } else {
                common[i + 1][j].max(common[i][j + 1])
            };
        }
    }
    let mut edits = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && same(&a[i], &b[j]) {
            edits.push(Edit::Same(i, j));
            i += 1;
            j += 1;
        } else if i < n && (j == m || common[i + 1][j] >= common[i][j + 1]) {
            edits.push(Edit::Removed(i));
            i += 1;
        } else {
            edits.push(Edit::Added(j));
            j += 1;
        }
    }
    edits
}

/// How many steps of each protected kind a step-graph value holds, nested steps included.
pub fn protected_counts(value: Option<&Value>) -> Vec<(StepKind, usize)> {
    let mut kinds = Vec::new();
    if let Some(value) = value {
        collect_kinds(value, &mut kinds);
    }
    PROTECTED_KINDS
        .iter()
        .map(|kind| (*kind, kinds.iter().filter(|k| *k == kind).count()))
        .collect()
}

fn collect_kinds(value: &Value, kinds: &mut Vec<StepKind>) {
    let Value::Object(map) = value else {
        return;
    };
    if let Some(kind) = map
        .get("kind")
        .and_then(|kind| serde_json::from_value::<StepKind>(kind.clone()).ok())
    {
        kinds.push(kind);
    }
    if let Some(Value::Array(steps)) = map.get("steps") {
        for step in steps {
            collect_kinds(step, kinds);
        }
    }
}
