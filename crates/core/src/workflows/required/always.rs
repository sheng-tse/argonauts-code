//! `workflows/always.toml`: the steps every workflow must contain, whoever writes it.

use std::io::ErrorKind;
use std::path::Path;

use serde::Deserialize;

use super::{RequiredStep, role_named, same_name, when_problem};
use crate::settings::crew::CrewRole;

pub const FILE: &str = "always.toml";

/// What `/setup` may leave in the workflows folder: every line a comment, so it requires
/// nothing until you write a step.
pub const STARTER: &str = r#"# Steps every workflow must contain, whoever writes it: you, the orchestrator or Claude Code.
# Argonauts Code checks a written workflow before it runs. If a step is missing, the orchestrator
# is asked once to add it, and after that Argonauts Code adds it itself.
#
# [[step]]
# name = "Benchmark"
# role = "worker"                  # main · orchestrator · worker · reviewer · judge
# does = "Run the benchmarks and report any slowdown over 5%."
# when = "before sign-off"         # first · after <step> · before <step> · last
#
# [[step]]
# name = "Secrets scan"
# role = "reviewer"
# does = "Scan the change for keys, tokens and passwords."
# when = "after build"
"#;

/// The guide's section on the file, after the workflows README.
pub const GUIDE: &str = r#"## Steps every workflow must contain

`workflows/always.toml`, in this repository's `.argonauts/` or in `~/.argonauts/`, lists steps
every workflow must contain, whoever writes it. This repository's step wins over yours of the
same name.

```toml
[[step]]
name = "Benchmark"
role = "worker"             # main · orchestrator · worker · reviewer · judge
does = "Run the benchmarks and report any slowdown over 5%."
when = "before sign-off"    # first · after <step> · before <step> · last
```

`name` is required. `role` is `worker` when left out, and `when` is before the sign-off.
Argonauts Code checks a written workflow before it runs: a missing step is asked for once, and then
Argonauts Code adds it itself. In the view such a step carries ★.

A request can say the same for itself ("every workflow must end with a benchmark run"), or fix
its task list ("do exactly these three tasks: ..."): the plan then keeps those tasks as given,
by name and order, and the orchestrator asks rather than adds one. Claude Code's own workflow
is given both as part of the request, and what it reports is checked against them.
"#;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    step: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: String,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    does: Option<String>,
    #[serde(default)]
    when: Option<String>,
}

/// The file in `folder`. No file requires nothing; a file that isn't valid is one problem
/// naming it, and none of its steps count.
pub(super) fn read(folder: &Path) -> Result<Vec<RequiredStep>, String> {
    let path = folder.join(FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text)
            .map_err(|problems| format!("{} isn't used: {}", path.display(), problems.join("; "))),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(format!("{} can't be read: {err}", path.display())),
    }
}

/// The steps in `text`, or every problem with it at once.
pub fn parse(text: &str) -> Result<Vec<RequiredStep>, Vec<String>> {
    let file: File = toml::from_str(text).map_err(|err| vec![err.message().to_owned()])?;
    let mut problems = Vec::new();
    let mut steps: Vec<RequiredStep> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for entry in file.step {
        let name = entry.name.trim().to_owned();
        if name.is_empty() {
            problems.push("a step has no name".to_owned());
            continue;
        }
        if seen.iter().any(|known| same_name(known, &name)) {
            problems.push(format!("two steps are named {name:?}"));
            continue;
        }
        seen.push(name.clone());
        let role = match entry.role.as_deref() {
            None => CrewRole::Worker,
            Some(role) => match role_named(role) {
                Some(role) => role,
                None => {
                    problems.push(format!(
                        "step {name:?}: role = {role:?} is none of main, orchestrator, worker, \
                         reviewer, judge"
                    ));
                    continue;
                }
            },
        };
        let when = entry.when.map(|when| when.trim().to_owned());
        if let Some(problem) = when.as_deref().and_then(when_problem) {
            problems.push(format!("step {name:?}: {problem}"));
            continue;
        }
        steps.push(RequiredStep {
            name,
            role,
            does: entry
                .does
                .map(|does| does.trim().to_owned())
                .filter(|does| !does.is_empty()),
            when,
        });
    }
    if problems.is_empty() {
        Ok(steps)
    } else {
        Err(problems)
    }
}

/// `steps` as `always.toml` holds them, for a file the setup writes.
pub fn steps_toml(steps: &[RequiredStep]) -> String {
    let quoted = |text: &str| toml::Value::String(text.to_owned()).to_string();
    let mut out = String::new();
    for step in steps {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("[[step]]\n");
        out.push_str(&format!("name = {}\n", quoted(&step.name)));
        out.push_str(&format!("role = {}\n", quoted(step.role.as_str())));
        if let Some(does) = &step.does {
            out.push_str(&format!("does = {}\n", quoted(does)));
        }
        if let Some(when) = &step.when {
            out.push_str(&format!("when = {}\n", quoted(when)));
        }
    }
    out
}
