//! One change to a workflow file of steps, made in its text, so the comments and the layout
//! you wrote stay as they are. A change that would stop steps that run from running is
//! refused.

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

use crate::file::{FileError, WorkflowFile};

/// The file's own keys a change may set.
pub const FILE_FIELDS: &[&str] = &["about", "adapt", "goal", "max_rounds"];

/// A step's keys a change may set, in the order a page lists them.
pub const STEP_FIELDS: &[&str] = &[
    "name",
    "role",
    "does",
    "for_each",
    "count",
    "independent",
    "not_the_author",
    "send_back_to",
    "then",
    "until",
    "kept_by",
    "max_rounds",
    "model",
    "own_workflow",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A key of the file itself. `None` takes the key out.
    File {
        key: String,
        value: Option<String>,
    },
    /// A key of one step, counted from 0. `None` takes the key out.
    Step {
        step: usize,
        key: String,
        value: Option<String>,
    },
    /// A new step after this one, or at the end.
    Add {
        after: Option<usize>,
    },
    Remove {
        step: usize,
    },
    /// One place up, or one place down.
    Move {
        step: usize,
        up: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    /// The file doesn't read as TOML, so only an editor can mend it.
    #[error("the file doesn't read as TOML: {0}")]
    Toml(String),
    /// The change itself makes no sense, such as a count of "many".
    #[error("{0}")]
    Refused(String),
    /// The steps wouldn't run after it.
    #[error("{0}")]
    WouldNotRun(String),
}

/// The file's text with the change made. The text you pass is never touched, so a refused
/// change leaves the file as it was.
pub fn change(text: &str, change: &Change) -> Result<String, EditError> {
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|err: toml_edit::TomlError| EditError::Toml(err.message().to_owned()))?;
    match change {
        Change::File { key, value } => {
            if !FILE_FIELDS.contains(&key.as_str()) {
                return Err(refused(format!("{key} isn't a key of a workflow file")));
            }
            set(doc.as_table_mut(), key, value.as_deref())?;
        }
        Change::Step { step, key, value } => {
            if !STEP_FIELDS.contains(&key.as_str()) {
                return Err(refused(format!("{key} isn't a key of a step")));
            }
            let steps = steps(&mut doc)?;
            let renamed = (key == "name").then(|| name_of(steps, *step)).flatten();
            set(at(steps, *step)?, key, value.as_deref())?;
            if let (Some(old), Some(new)) = (renamed, value) {
                follow_rename(steps, &old, new.trim());
            }
        }
        Change::Add { after } => {
            let steps = steps(&mut doc)?;
            let end = steps.len();
            steps.push(new_step(steps));
            // It was added at the end; each swap takes it one place nearer.
            let place = after.map_or(end, |after| (after + 1).min(end));
            for index in (place..end).rev() {
                swap(steps, index, index + 1);
            }
        }
        Change::Remove { step } => {
            let steps = steps(&mut doc)?;
            at(steps, *step)?;
            steps.remove(*step);
        }
        Change::Move { step, up } => {
            let steps = steps(&mut doc)?;
            at(steps, *step)?;
            let other = if *up {
                step.checked_sub(1)
            } else {
                Some(step + 1).filter(|next| *next < steps.len())
            };
            let Some(other) = other else {
                let edge = if *up { "first" } else { "last" };
                return Err(refused(format!("it is the {edge} step already")));
            };
            swap(steps, *step, other);
        }
    }
    let changed = doc.to_string();
    let after = problems(&changed);
    // A file that was broken already takes any change, so it can be mended a key at a time.
    if after.is_empty() || !problems(text).is_empty() {
        Ok(changed)
    } else {
        Err(EditError::WouldNotRun(after.join("\n")))
    }
}

fn refused(why: impl Into<String>) -> EditError {
    EditError::Refused(why.into())
}

/// What stops the steps from running, one line each. Empty when they would run.
fn problems(text: &str) -> Vec<String> {
    match WorkflowFile::parse(text) {
        Ok(Some(_)) => Vec::new(),
        Ok(None) => vec!["a workflow needs at least one [[step]]".to_owned()],
        Err(FileError::Toml(message)) => vec![message],
        Err(FileError::Problems(problems)) => problems
            .into_iter()
            .map(|problem| problem.message)
            .collect(),
    }
}

fn steps(doc: &mut DocumentMut) -> Result<&mut ArrayOfTables, EditError> {
    if !doc.contains_key("step") {
        doc.insert("step", Item::ArrayOfTables(ArrayOfTables::new()));
    }
    match doc.get_mut("step") {
        Some(Item::ArrayOfTables(steps)) => Ok(steps),
        _ => Err(refused(
            "the steps aren't written as [[step]] tables, so this page can't change them",
        )),
    }
}

fn at(steps: &mut ArrayOfTables, step: usize) -> Result<&mut Table, EditError> {
    let count = steps.len();
    steps.get_mut(step).ok_or_else(|| {
        refused(format!(
            "there is no step {}; the file has {count}",
            step + 1
        ))
    })
}

fn name_of(steps: &ArrayOfTables, step: usize) -> Option<String> {
    let name = steps.get(step)?.get("name")?.as_str()?;
    Some(name.to_owned())
}

/// Steps that pointed at the old name point at the new one.
fn follow_rename(steps: &mut ArrayOfTables, old: &str, new: &str) {
    if new.is_empty() || old == new {
        return;
    }
    for step in steps.iter_mut() {
        for key in ["send_back_to", "then"] {
            let points_here = step.get(key).and_then(Item::as_str) == Some(old);
            if points_here {
                put(step, key, Value::from(new));
            }
        }
    }
}

/// A step that runs as it stands: workers, under a name no other step has.
fn new_step(steps: &ArrayOfTables) -> Table {
    let taken = |name: &str| {
        steps
            .iter()
            .any(|step| step.get("name").and_then(Item::as_str) == Some(name))
    };
    let name = (steps.len() + 1..)
        .map(|number| format!("Step {number}"))
        .find(|name| !taken(name))
        .unwrap_or_default();
    let mut step = Table::new();
    step.insert("name", Item::Value(Value::from(name)));
    step.insert("role", Item::Value(Value::from("workers")));
    step
}

/// Two steps change places. Each keeps the comment written above it, and the place in the
/// file stays with the slot, so settings written between two steps stay where they were.
fn swap(steps: &mut ArrayOfTables, a: usize, b: usize) {
    let (Some(mut first), Some(mut second)) = (steps.get(a).cloned(), steps.get(b).cloned()) else {
        return;
    };
    let (place_a, place_b) = (first.position(), second.position());
    if let Some(place) = place_a {
        second.set_position(place);
    }
    if let Some(place) = place_b {
        first.set_position(place);
    }
    if let Some(slot) = steps.get_mut(a) {
        *slot = second;
    }
    if let Some(slot) = steps.get_mut(b) {
        *slot = first;
    }
}

/// What a key takes.
enum Takes {
    Text,
    /// `true` or `false`.
    Flag,
    /// A whole number, 1 or more.
    Rounds,
    /// A whole number, or `all`.
    Count,
}

fn takes(key: &str) -> Takes {
    match key {
        "adapt" | "independent" | "not_the_author" | "own_workflow" => Takes::Flag,
        "max_rounds" => Takes::Rounds,
        "count" => Takes::Count,
        _ => Takes::Text,
    }
}

fn set(table: &mut Table, key: &str, written: Option<&str>) -> Result<(), EditError> {
    let written = written.map(str::trim).filter(|text| !text.is_empty());
    let Some(written) = written else {
        table.remove(key);
        return Ok(());
    };
    let value = match takes(key) {
        Takes::Text => Value::from(written),
        Takes::Flag => match written {
            "true" => Value::from(true),
            "false" => Value::from(false),
            _ => return Err(refused(format!("{key} takes true or false"))),
        },
        Takes::Rounds => Value::from(whole(key, written)?),
        Takes::Count if written == "all" => Value::from("all"),
        Takes::Count => Value::from(whole(key, written)?),
    };
    // A flag nobody wrote is false already, so `false` adds no line.
    let unwritten_false = matches!(takes(key), Takes::Flag) && key != "adapt";
    if unwritten_false && written == "false" && !table.contains_key(key) {
        return Ok(());
    }
    put(table, key, value);
    Ok(())
}

fn whole(key: &str, written: &str) -> Result<i64, EditError> {
    let also = if key == "count" { ", or all" } else { "" };
    written
        .parse::<u32>()
        .ok()
        .filter(|number| *number >= 1)
        .map(i64::from)
        .ok_or_else(|| {
            refused(format!(
                "{key} takes a whole number, 1 or more{also}: {written:?} isn't one"
            ))
        })
}

/// Writes the value where the old one was, with the comment that follows it on its line.
fn put(table: &mut Table, key: &str, mut value: Value) {
    if let Some(Item::Value(old)) = table.get(key) {
        *value.decor_mut() = old.decor().clone();
    }
    table.insert(key, Item::Value(value));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::template;

    fn file() -> String {
        template("spec-first", ".argonauts/workflows")
    }

    fn step(step: usize, key: &str, value: &str) -> Change {
        Change::Step {
            step,
            key: key.to_owned(),
            value: Some(value.to_owned()),
        }
    }

    fn names(text: &str) -> Vec<String> {
        let file = WorkflowFile::parse(text).unwrap().unwrap();
        file.steps.into_iter().map(|step| step.name).collect()
    }

    #[test]
    fn a_value_changes_and_the_comment_on_its_line_stays() {
        let changed = change(&file(), &step(0, "role", "judge")).unwrap();
        assert!(
            changed.contains("role = \"judge\"    # orchestrator · workers · reviewers · judge"),
            "{changed}"
        );
        // Every other line is as it was written.
        let differing = file()
            .lines()
            .zip(changed.lines())
            .filter(|(before, after)| before != after)
            .count();
        assert_eq!(differing, 1, "{changed}");
        assert_eq!(file().lines().count(), changed.lines().count());
    }

    #[test]
    fn a_new_key_is_written_into_its_step_and_an_empty_one_is_taken_out() {
        let with = change(&file(), &step(1, "until", "its own tests pass")).unwrap();
        let read = WorkflowFile::parse(&with).unwrap().unwrap();
        assert_eq!(read.steps[1].until.as_deref(), Some("its own tests pass"));
        let without = change(&with, &step(1, "until", " ")).unwrap();
        let read = WorkflowFile::parse(&without).unwrap().unwrap();
        assert_eq!(read.steps[1].until, None);
    }

    #[test]
    fn numbers_and_flags_are_written_as_numbers_and_flags() {
        let text = change(&file(), &step(2, "count", "2")).unwrap();
        let text = change(&text, &step(2, "max_rounds", "3")).unwrap();
        let text = change(&text, &step(2, "independent", "true")).unwrap();
        assert!(text.contains("count = 2"), "{text}");
        assert!(text.contains("max_rounds = 3"), "{text}");
        assert!(text.contains("independent = true"), "{text}");
        let all = change(&text, &step(2, "count", "all")).unwrap();
        assert!(all.contains("count = \"all\""), "{all}");
        // False is what an unwritten flag means, so it adds no line.
        let unchanged = change(&file(), &step(0, "independent", "false")).unwrap();
        assert_eq!(unchanged, file());
    }

    #[test]
    fn a_value_of_the_wrong_kind_is_refused_with_what_the_key_takes() {
        let err = change(&file(), &step(2, "count", "many")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "count takes a whole number, 1 or more, or all: \"many\" isn't one"
        );
        let err = change(&file(), &step(2, "independent", "yes")).unwrap_err();
        assert_eq!(err.to_string(), "independent takes true or false");
        let err = change(&file(), &step(0, "colour", "red")).unwrap_err();
        assert_eq!(err.to_string(), "colour isn't a key of a step");
        let err = change(&file(), &step(7, "role", "judge")).unwrap_err();
        assert_eq!(err.to_string(), "there is no step 8; the file has 3");
    }

    #[test]
    fn a_change_the_steps_could_not_run_after_is_refused() {
        let err = change(&file(), &step(2, "send_back_to", "Biuld")).unwrap_err();
        assert!(
            matches!(&err, EditError::WouldNotRun(why) if why.contains("no step is called that")),
            "{err}"
        );
        let err = change(&file(), &step(0, "role", "captain")).unwrap_err();
        assert!(matches!(err, EditError::WouldNotRun(_)), "{err}");
        // Review sends work back to Build, so Build can't just go.
        let err = change(&file(), &Change::Remove { step: 1 }).unwrap_err();
        assert!(
            err.to_string().contains("send_back_to names Build"),
            "{err}"
        );
    }

    #[test]
    fn renaming_a_step_takes_the_steps_that_point_at_it_along() {
        let renamed = change(&file(), &step(1, "name", "Make")).unwrap();
        let read = WorkflowFile::parse(&renamed).unwrap().unwrap();
        assert_eq!(read.steps[1].name, "Make");
        assert_eq!(read.steps[2].send_back_to.as_deref(), Some("Make"));
        assert!(
            renamed.contains("send_back_to = \"Make\"   # until the reviewer accepts"),
            "{renamed}"
        );
    }

    #[test]
    fn a_step_is_added_where_asked_under_a_name_of_its_own() {
        let at_end = change(&file(), &Change::Add { after: None }).unwrap();
        assert_eq!(names(&at_end), ["Plan", "Build", "Review", "Step 4"]);
        // The note that closes the file still closes it.
        assert!(at_end.trim_end().ends_with("explains each."), "{at_end}");
        let after_plan = change(&at_end, &Change::Add { after: Some(0) }).unwrap();
        assert_eq!(
            names(&after_plan),
            ["Plan", "Step 5", "Build", "Review", "Step 4"]
        );
        let position = |name: &str| after_plan.find(&format!("\"{name}\"")).unwrap();
        assert!(position("Plan") < position("Step 5"), "{after_plan}");
        assert!(position("Step 5") < position("Build"), "{after_plan}");
    }

    #[test]
    fn a_step_moves_with_its_comments_and_stops_at_the_ends() {
        let moved = change(&file(), &Change::Move { step: 2, up: true }).unwrap();
        assert_eq!(names(&moved), ["Plan", "Review", "Build"]);
        let place = |text: &str| moved.find(text).unwrap();
        assert!(place("# until the reviewer accepts") < place("# one agent per task"));
        let back = change(&moved, &Change::Move { step: 1, up: false }).unwrap();
        assert_eq!(names(&back), ["Plan", "Build", "Review"]);
        let err = change(&file(), &Change::Move { step: 0, up: true }).unwrap_err();
        assert_eq!(err.to_string(), "it is the first step already");
        let err = change(&file(), &Change::Move { step: 2, up: false }).unwrap_err();
        assert_eq!(err.to_string(), "it is the last step already");
    }

    #[test]
    fn the_files_own_keys_change_and_settings_below_the_steps_stay() {
        let text = format!("{}\n[crew]\nworkers = [\"codex/gpt-5.6\"]\n", file());
        let changed = change(
            &text,
            &Change::File {
                key: "goal".to_owned(),
                value: Some("every test passes".to_owned()),
            },
        )
        .unwrap();
        let read = WorkflowFile::parse(&changed).unwrap().unwrap();
        assert_eq!(read.goal.as_deref(), Some("every test passes"));
        assert!(
            changed.contains("[crew]\nworkers = [\"codex/gpt-5.6\"]"),
            "{changed}"
        );
        let adapted = change(
            &changed,
            &Change::File {
                key: "adapt".to_owned(),
                value: Some("true".to_owned()),
            },
        )
        .unwrap();
        assert!(
            adapted.contains("adapt = true            # false:"),
            "{adapted}"
        );
        let added = change(&adapted, &Change::Add { after: None }).unwrap();
        assert!(
            added.find("Step 4").unwrap() < added.find("[crew]").unwrap(),
            "{added}"
        );
        let err = change(
            &text,
            &Change::File {
                key: "name".to_owned(),
                value: Some("other".to_owned()),
            },
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "name isn't a key of a workflow file");
    }

    #[test]
    fn a_broken_file_is_mended_a_key_at_a_time() {
        let broken = file()
            .replace("send_back_to = \"Build\"", "send_back_to = \"Gone\"")
            .replace("role = \"orchestrator\"", "role = \"captain\"");
        let half = change(&broken, &step(0, "role", "orchestrator")).unwrap();
        assert!(WorkflowFile::parse(&half).is_err());
        let whole = change(&half, &step(2, "send_back_to", "Build")).unwrap();
        assert!(WorkflowFile::parse(&whole).unwrap().is_some());
        let err = change("name = ", &step(0, "role", "judge")).unwrap_err();
        assert!(matches!(err, EditError::Toml(_)), "{err}");
    }
}
