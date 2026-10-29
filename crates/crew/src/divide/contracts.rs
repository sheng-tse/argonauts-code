//! Contracts first (`divide.contracts_first`): one interface task (stubs that compile and fail
//! "not implemented"), then one acceptance-test task per build task, owned by that task's ring
//! reviewer, with the build task depending on it. Once the test tasks are merged, every goal
//! criterion must appear in some test task's `criteria_covered`.

use argonauts_core::TaskId;
use argonauts_core::events::goal::Criterion;
use argonauts_core::events::task::{TaskKind, TaskLimit, TaskSpec};

/// Where each build task's acceptance tests go: `tests/acceptance/<task>/`.
pub const ACCEPTANCE_DIR: &str = "tests/acceptance";

/// The graph with the test tasks added and the edges set: every other task depends on the
/// interface tasks and reads their paths, and each build task depends on its test task. The
/// order is interfaces, test tasks, then the rest. `tests` is off when nobody writes them.
pub fn contracts_first(tasks: Vec<TaskSpec>, tests: bool) -> Vec<TaskSpec> {
    let mut next = tasks.len() as u32 + 1;
    let (interfaces, rest): (Vec<TaskSpec>, Vec<TaskSpec>) = tasks
        .into_iter()
        .partition(|task| task.kind == TaskKind::Interface);
    let interface_ids: Vec<TaskId> = interfaces.iter().map(|task| task.id.clone()).collect();
    let frozen: Vec<String> = interfaces
        .iter()
        .flat_map(|task| task.owned_paths.iter().cloned())
        .collect();
    let mut test_tasks = Vec::new();
    let mut others = Vec::with_capacity(rest.len());
    for mut task in rest {
        add_missing(&mut task.depends_on, &interface_ids);
        add_missing(&mut task.read_only_paths, &frozen);
        if tests && task.kind == TaskKind::Build {
            let test = test_task(&task, next, &interface_ids, &frozen);
            next += 1;
            task.depends_on.push(test.id.clone());
            add_missing(&mut task.read_only_paths, &test.owned_paths);
            test_tasks.push(test);
        }
        others.push(task);
    }
    interfaces
        .into_iter()
        .chain(test_tasks)
        .chain(others)
        .collect()
}

/// Goal criteria no test task covers, by id. Any one blocks the build tasks and comes to
/// you. `covered` holds ids such as `G1`, or `G1: text`.
pub fn uncovered(criteria: &[Criterion], covered: &[String]) -> Vec<String> {
    let covered: Vec<&str> = covered.iter().map(|item| criterion_id(item)).collect();
    criteria
        .iter()
        .filter(|criterion| {
            !covered
                .iter()
                .any(|id| id.eq_ignore_ascii_case(criterion.id.trim()))
        })
        .map(|criterion| criterion.id.clone())
        .collect()
}

fn criterion_id(item: &str) -> &str {
    item.split(':').next().unwrap_or(item).trim()
}

fn test_task(build: &TaskSpec, n: u32, interfaces: &[TaskId], frozen: &[String]) -> TaskSpec {
    let id = TaskId::new(format!("test-{n:03}")).unwrap_or_else(|_| TaskId::numbered(n));
    let dir = format!("{ACCEPTANCE_DIR}/{}", build.id);
    let criteria = if build.criteria.is_empty() {
        "the goal criteria it serves".to_owned()
    } else {
        build.criteria.join(", ")
    };
    TaskSpec {
        id,
        title: format!("Acceptance tests for {}", build.title),
        task: format!(
            "Write acceptance tests for {} (\"{}\") in {dir}/. Test it only through its \
             interface. The tests must fail against the stubs, since nothing is built yet, and \
             together they must cover {criteria}. Don't edit the task's own paths.\n\nThe \
             task's brief:\n\n{}",
            build.id, build.title, build.task
        ),
        kind: TaskKind::AcceptanceTests {
            for_task: build.id.clone(),
        },
        owned_paths: vec![format!("{dir}/**")],
        read_only_paths: frozen.to_vec(),
        forbidden_paths: build.owned_paths.clone(),
        hidden_paths: Vec::new(),
        depends_on: interfaces.to_vec(),
        criteria: build.criteria.clone(),
        worker: None,
        reviewers: Vec::new(),
        limit: TaskLimit::default(),
        predictions: None,
        item: build.item.clone(),
        way: None,
    }
}

fn add_missing<T: Clone + PartialEq>(into: &mut Vec<T>, items: &[T]) {
    for item in items {
        if !into.contains(item) {
            into.push(item.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::Settings;
    use argonauts_core::steps::plan::{ProposedKind, TaskGraphProposal, TaskProposal};

    use super::*;
    use crate::divide::graph;

    fn proposal() -> TaskGraphProposal {
        let task =
            |key: &str, kind, owned: &str, depends: &[&str], criteria: &[&str]| TaskProposal {
                key: key.to_owned(),
                title: key.to_owned(),
                task: format!("Do {key}."),
                kind,
                owned_paths: vec![owned.to_owned()],
                read_only_paths: Vec::new(),
                depends_on: depends.iter().map(|d| (*d).to_owned()).collect(),
                criteria: criteria.iter().map(|c| (*c).to_owned()).collect(),
            };
        TaskGraphProposal {
            tasks: vec![
                task("store", ProposedKind::Build, "src/store/**", &[], &["G1"]),
                task("api", ProposedKind::Interface, "src/api.rs", &[], &[]),
                task(
                    "middleware",
                    ProposedKind::Build,
                    "src/mw/**",
                    &["store"],
                    &["G2", "G3"],
                ),
                task(
                    "docs",
                    ProposedKind::Docs,
                    "docs/rate.md",
                    &["middleware"],
                    &["G3"],
                ),
            ],
        }
    }

    fn tasks() -> Vec<TaskSpec> {
        graph::convert(&proposal()).0
    }

    #[test]
    fn divide_contracts_first() {
        let tasks = contracts_first(tasks(), true);
        let ids: Vec<&str> = tasks.iter().map(|task| task.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "task-002", "test-005", "test-006", "task-001", "task-003", "task-004"
            ]
        );
        let get = |id: &str| tasks.iter().find(|task| task.id.as_str() == id).unwrap();
        let interface = TaskId::new("task-002").unwrap();
        let test = get("test-005");
        assert_eq!(
            test.kind,
            TaskKind::AcceptanceTests {
                for_task: TaskId::numbered(1)
            }
        );
        assert_eq!(test.owned_paths, ["tests/acceptance/task-001/**"]);
        assert_eq!(test.forbidden_paths, ["src/store/**"]);
        assert_eq!(test.depends_on, std::slice::from_ref(&interface));
        assert_eq!(test.read_only_paths, ["src/api.rs"]);
        let store = get("task-001");
        assert_eq!(
            store.depends_on,
            [interface.clone(), TaskId::new("test-005").unwrap()]
        );
        assert!(store.read_only_paths.contains(&"src/api.rs".to_owned()));
        assert!(
            store
                .read_only_paths
                .contains(&"tests/acceptance/task-001/**".to_owned())
        );
        // Docs aren't built, so they get no test task, but still wait for the interface.
        assert!(get("task-004").depends_on.contains(&interface));
        assert_eq!(tasks.len(), 6);
        assert!(graph::rules(&tasks, &Settings::default()).is_empty());

        let without_tests = contracts_first(self::tasks(), false);
        assert_eq!(without_tests.len(), 4);
        assert_eq!(without_tests[0].kind, TaskKind::Interface);
    }

    #[test]
    fn goal_criteria_become_acceptance_tests() {
        let tasks = contracts_first(tasks(), true);
        let tested: Vec<String> = tasks
            .iter()
            .filter(|task| matches!(task.kind, TaskKind::AcceptanceTests { .. }))
            .flat_map(|task| task.criteria.clone())
            .collect();
        assert_eq!(tested, ["G1", "G2", "G3"]);
        let criterion = |id: &str| Criterion::new(id, format!("criterion {id}"));
        let goal = [
            criterion("G1"),
            criterion("G2"),
            criterion("G3"),
            criterion("G4"),
        ];
        // What the test tasks reported covering once they were merged.
        let covered = ["G1".to_owned(), "g2: limits per key".to_owned()];
        assert_eq!(uncovered(&goal, &covered), ["G3", "G4"]);
        let all = ["G1", "G2", "G3", "G4"].map(str::to_owned);
        assert!(uncovered(&goal, &all).is_empty());
    }
}
