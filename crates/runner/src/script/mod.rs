//! A workflow as a script, in the format Claude Code writes its own: `export const meta = {…}`
//! first, then a body that calls `agent()`, `parallel()`, `pipeline()`, `phase()` and `log()`. A
//! file of steps becomes one too, so one engine runs both.

pub(crate) mod call;
pub(crate) mod kept;
pub(crate) mod rehearse;
pub(crate) mod report;
pub(crate) mod run;
pub(crate) mod sandbox;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::files::file::WorkflowFile;
use crate::files::plan;

/// What a script says it is, read before anything runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub name: String,
    pub description: String,
    #[serde(default, rename = "whenToUse", skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,
    /// In the order the script declares them. A `phase()` it doesn't list gets a group of its
    /// own when it starts.
    #[serde(default)]
    pub phases: Vec<MetaPhase>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaPhase {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    pub meta: Meta,
    /// The source as written.
    pub source: String,
    /// The source with `export` taken off `meta`, which is what the engine can run.
    body: String,
    /// Built from a file of steps, so it runs with the steps driver.
    steps: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScriptError {
    #[error(
        "a workflow script starts with `export const meta = {{ name, description }}`, before \
         anything else"
    )]
    NoMeta,
    #[error("meta must be a plain literal, with no variables or calls: {0}")]
    MetaNotLiteral(String),
    #[error("meta needs a {0}")]
    MetaMissing(&'static str),
    #[error("the script doesn't read as JavaScript: {0}")]
    Syntax(String),
}

const EXPORT: &str = "export";

impl Script {
    /// Reads a script and its `meta`, and checks it is JavaScript. Nothing runs.
    pub fn parse(source: &str) -> Result<Self, ScriptError> {
        let start = skip_blank(source, 0);
        let rest = &source[start..];
        let after_export = rest.strip_prefix(EXPORT).ok_or(ScriptError::NoMeta)?;
        let declared = after_export.trim_start();
        let after_const = declared.strip_prefix("const").ok_or(ScriptError::NoMeta)?;
        let after_name = after_const
            .trim_start()
            .strip_prefix("meta")
            .ok_or(ScriptError::NoMeta)?;
        let after_equals = after_name
            .trim_start()
            .strip_prefix('=')
            .ok_or(ScriptError::NoMeta)?;
        let literal_at = source.len() - after_equals.len();
        let literal_at = skip_blank(source, literal_at);
        let end = literal_end(source, literal_at).ok_or(ScriptError::NoMeta)?;
        let value =
            sandbox::literal(&source[literal_at..end]).map_err(ScriptError::MetaNotLiteral)?;
        let meta = read_meta(value)?;

        // Spaces in place of `export` keep every line and column where the author wrote it.
        let mut body = String::with_capacity(source.len());
        body.push_str(&source[..start]);
        body.push_str(&" ".repeat(EXPORT.len()));
        body.push_str(after_export);
        let script = Self {
            meta,
            source: source.to_owned(),
            body,
            steps: false,
        };
        sandbox::syntax(&script.wrapped()).map_err(ScriptError::Syntax)?;
        Ok(script)
    }

    /// The script that runs a file's steps exactly as written: each step is a phase, and the
    /// steps driver walks them.
    pub fn from_steps(file: &WorkflowFile) -> Self {
        let phases: Vec<MetaPhase> = (0..file.steps.len())
            .map(|index| MetaPhase {
                name: file.steps[index].name.clone(),
                detail: format!(
                    "{} · {}",
                    file.steps[index].role.plural(),
                    plan::how(file, index)
                ),
                model: file.steps[index].model.as_ref().map(ToString::to_string),
            })
            .collect();
        let description = if file.about.is_empty() {
            format!("The steps of {}", file.name)
        } else {
            file.about.clone()
        };
        let meta = Meta {
            name: file.name.clone(),
            description,
            when_to_use: None,
            phases,
        };
        let indexes = 0..file.steps.len();
        let table = json!({
            "name": file.name,
            "goal": file.goal,
            "max_rounds": file.max_rounds,
            "steps": file.steps,
            "per_task": indexes.clone().map(|index| file.per_task(index)).collect::<Vec<_>>(),
            "only_when_sent_back": indexes
                .map(|index| file.only_when_sent_back(index))
                .collect::<Vec<_>>(),
            "planner": file.planner(),
        });
        let meta_json = serde_json::to_string_pretty(&meta).unwrap_or_default();
        let table_json = serde_json::to_string_pretty(&table).unwrap_or_default();
        let source = format!(
            "export const meta = {meta_json}\n\nconst FILE = {table_json}\n\nreturn await \
             steps(FILE, args)\n"
        );
        let body = source.replacen(EXPORT, &" ".repeat(EXPORT.len()), 1);
        Self {
            meta,
            source,
            body,
            steps: true,
        }
    }

    /// The body inside an async function of `args`, on the author's own first line so an
    /// error's line number is theirs.
    pub(crate) fn wrapped(&self) -> String {
        format!("(async function (args) {{ {}\n}})", self.body)
    }

    pub(crate) fn runs_steps(&self) -> bool {
        self.steps
    }
}

fn read_meta(mut value: Value) -> Result<Meta, ScriptError> {
    let text = |key: &'static str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or(ScriptError::MetaMissing(key))
    };
    text("name")?;
    text("description")?;
    name_claude_code_phases(&mut value);
    serde_json::from_value(value).map_err(|err| ScriptError::MetaNotLiteral(err.to_string()))
}

/// Claude Code's own scripts give each phase a `title`, which is its name here.
fn name_claude_code_phases(meta: &mut Value) {
    let Some(phases) = meta.get_mut("phases").and_then(Value::as_array_mut) else {
        return;
    };
    for phase in phases.iter_mut().filter_map(Value::as_object_mut) {
        if !phase.contains_key("name")
            && let Some(title) = phase.remove("title")
        {
            phase.insert("name".to_owned(), title);
        }
    }
}

/// The first place at or after `from` that isn't whitespace or a comment.
fn skip_blank(source: &str, from: usize) -> usize {
    let bytes = source.as_bytes();
    let mut at = from;
    loop {
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        if source[at..].starts_with("//") {
            at += source[at..].find('\n').unwrap_or(source.len() - at);
        } else if source[at..].starts_with("/*") {
            match source[at + 2..].find("*/") {
                Some(end) => at += end + 4,
                None => return source.len(),
            }
        } else {
            return at;
        }
    }
}

/// Where the literal that opens at `from` closes: one past its last bracket. Strings and
/// comments may hold brackets of their own.
fn literal_end(source: &str, from: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    if !matches!(bytes.get(from), Some(b'{')) {
        return None;
    }
    let mut depth = 0usize;
    let mut at = from;
    while at < bytes.len() {
        match bytes[at] {
            b'{' | b'[' | b'(' => depth += 1,
            b'}' | b']' | b')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(at + 1);
                }
            }
            quote @ (b'"' | b'\'' | b'`') => {
                at += 1;
                while at < bytes.len() && bytes[at] != quote {
                    if bytes[at] == b'\\' {
                        at += 1;
                    }
                    at += 1;
                }
            }
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                at += source[at..].find('\n').unwrap_or(source.len() - at);
                continue;
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                at += source[at + 2..].find("*/")? + 4;
                continue;
            }
            _ => {}
        }
        at += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = "// Finds flaky tests.\nexport const meta = {\n  name: 'find-flaky',\n  \
                          description: 'Find flaky tests } and fix them',\n  phases: [{ name: \
                          'Scan', detail: 'read the logs' }, { name: 'Fix' }],\n}\n\
                          phase('Scan')\nconst found = await agent('look', { role: 'worker' \
                          })\nreturn found\n";

    #[test]
    fn a_claude_code_script_names_its_phases_with_title() {
        let script = Script::parse(
            "export const meta = { name: 'a', description: 'b', phases: [{ title: 'Scan' }] }\n",
        )
        .expect("parses");
        assert_eq!(script.meta.phases[0].name, "Scan");
    }

    #[test]
    fn meta_is_read_before_anything_runs() {
        let script = Script::parse(SCRIPT).expect("parses");
        assert_eq!(script.meta.name, "find-flaky");
        assert_eq!(script.meta.description, "Find flaky tests } and fix them");
        let names: Vec<&str> = script.meta.phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Scan", "Fix"]);
        assert_eq!(script.meta.phases[0].detail, "read the logs");
        // Every line stays where its author wrote it, so an error names their line.
        let wrapped = script.wrapped();
        assert_eq!(wrapped.lines().nth(6), SCRIPT.lines().nth(6));
        assert!(
            wrapped
                .lines()
                .nth(1)
                .is_some_and(|line| line.ends_with("const meta = {"))
        );
    }

    #[test]
    fn a_script_without_meta_first_is_refused() {
        assert_eq!(
            Script::parse("const x = 1\nexport const meta = {name: 'a', description: 'b'}"),
            Err(ScriptError::NoMeta)
        );
        assert_eq!(
            Script::parse("export const meta = {name: 'a'}\n"),
            Err(ScriptError::MetaMissing("description"))
        );
    }

    #[test]
    fn meta_that_computes_anything_is_refused() {
        let computed = "export const meta = {name: 'a', description: String(1 + 1)}\n";
        assert!(matches!(
            Script::parse(computed),
            Err(ScriptError::MetaNotLiteral(_))
        ));
        let variable = "export const meta = {name: other, description: 'b'}\n";
        assert!(matches!(
            Script::parse(variable),
            Err(ScriptError::MetaNotLiteral(_))
        ));
    }

    #[test]
    fn a_script_that_is_not_javascript_is_refused_with_its_line() {
        let typed = "export const meta = {name: 'a', description: 'b'}\nconst n: number = 1\n";
        match Script::parse(typed) {
            Err(ScriptError::Syntax(said)) => assert!(said.contains(":2"), "{said}"),
            other => panic!("{other:?}"),
        }
    }
}
