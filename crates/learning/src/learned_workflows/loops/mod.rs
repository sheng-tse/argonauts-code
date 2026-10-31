//! Starlark for custom loops. A `.star` file builds a workflow out of the same step kinds a
//! YAML one uses, with Starlark's loops and functions, and the engine validates and runs the
//! result like any workflow. Building, not driving: the script never calls a model or waits on
//! a check, so log replay stays exact and every branch the run takes is still code reading a
//! checked field.
//!
//! `starlark_syntax` parses the file and a small interpreter here evaluates it. The interpreter
//! has no file, network, clock, or randomness to offer, and a step limit, so the same file
//! always builds the same workflow. The `starlark` crate's own evaluator isn't used: it turns on
//! serde_json's `arbitrary_precision` for the whole workspace, which breaks numbers elsewhere.
//!
//! A script calls `workflow(name, phases, description, params)` once. Each phase is
//! `phase(name, standard, steps)`, and each step comes from the builder named after its kind,
//! such as `review(role = "reviewer", blind = True)`; a `workflow` step is `workflow_step`.

mod builtins;
mod interp;
mod value;

use std::path::{Path, PathBuf};

use starlark_syntax::codemap::Span;
use starlark_syntax::syntax::{AstModule, Dialect};

use argonauts_core::workflows::workflow::Workflow;

use crate::store::error::{LearningError, Result};

/// Statements, expressions, and calls one build may evaluate. A workflow builder needs a few
/// hundred; a runaway loop stops here instead of hanging the run that asked for it.
pub const STEP_LIMIT: u64 = 100_000;
/// Items one list, or bytes one string, may hold.
const MAX_LEN: usize = 100_000;
/// Nested calls. Starlark has no recursion, so this only bounds a long chain of helpers.
const MAX_DEPTH: usize = 64;

/// An evaluation error at a place in the file.
struct EvalError {
    span: Span,
    message: String,
}

type Eval<T> = std::result::Result<T, EvalError>;

fn fail<T>(span: Span, message: impl Into<String>) -> Eval<T> {
    Err(EvalError {
        span,
        message: message.into(),
    })
}

/// Builds the workflow `source` describes. `name` is the file, for errors.
pub fn build(name: &str, source: &str) -> Result<Workflow> {
    let path = PathBuf::from(name);
    let refuse = |message: String| LearningError::Parse {
        path: path.clone(),
        message,
    };
    // No `load`: a builder has only its own file. Top-level `for` and `if` are allowed, since a
    // workflow is often assembled at the top level.
    let dialect = Dialect {
        enable_load: false,
        enable_top_level_stmt: true,
        ..Dialect::Standard
    };
    let module = AstModule::parse(name, source.to_owned(), &dialect).map_err(|error| {
        let message = match error.span() {
            Some(span) => {
                let at = span.resolve_span().begin;
                format!("line {}: {}", at.line + 1, error.kind())
            }
            None => error.kind().to_string(),
        };
        refuse(message)
    })?;

    let mut interp = interp::Interp::new();
    interp.run(module.statement()).map_err(|error| {
        let at = module.file_span(error.span).resolve_span().begin;
        refuse(format!("line {}: {}", at.line + 1, error.message))
    })?;
    let Some(value) = interp.into_workflow() else {
        return Err(refuse("the script never calls workflow(...)".to_owned()));
    };
    let json = value.to_json().map_err(&refuse)?;
    serde_json::from_value(json)
        .map_err(|error| refuse(format!("the workflow doesn't fit: {error}")))
}

/// Reads and builds a `.star` file.
pub fn build_file(path: &Path) -> Result<Workflow> {
    let source = std::fs::read_to_string(path)?;
    build(&path.display().to_string(), &source)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built(source: &str) -> Workflow {
        match build("test.star", source) {
            Ok(workflow) => workflow,
            Err(error) => panic!("{error}"),
        }
    }

    fn refused(source: &str) -> String {
        match build("test.star", source) {
            Ok(_) => panic!("built a workflow from {source:?}"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn values_and_control_flow() {
        let workflow = built(
            r#"
names = []
for i in range(5):
    if i % 2 == 0:
        continue
    names.append("n%d" % i)
pairs = {k: v for k, v in zip(["a", "b"], [1, 2])}
total = 0
for k in pairs:
    total += pairs[k]
label = "-".join(names) + ":" + str(total) + ":" + str(len(names[0:1]))
picked = [x for x in range(10) if x > 6 and x != 8]
f = lambda x, y = 2: x * y
workflow(name = label, description = str(picked) + str(f(3)) + str(sorted([3, 1, 2], reverse = True)), phases = [])
"#,
        );
        assert_eq!(workflow.name, "n1-n3:3:1");
        assert_eq!(workflow.description, "[7, 9]6[3, 2, 1]");
    }

    #[test]
    fn functions_take_defaults_args_and_kwargs() {
        let workflow = built(
            r#"
def describe(first, *rest, sep = "/", **extra):
    parts = [first] + list(rest)
    keys = sorted(extra.keys())
    return sep.join(parts) + "|" + ",".join(["%s=%s" % (k, extra[k]) for k in keys])

def outer(n):
    def inner(x):
        return x + n
    return inner(1)

workflow(name = describe("a", "b", "c", sep = "+", z = 1, y = 2), description = str(outer(41)), phases = [])
"#,
        );
        assert_eq!(workflow.name, "a+b+c|y=2,z=1");
        assert_eq!(workflow.description, "42");
    }

    #[test]
    fn errors_name_the_line() {
        let message = refused("x = 1\ny = x + \"a\"\nworkflow(name = \"w\", phases = [])\n");
        assert!(message.contains("line 2"), "{message}");
        assert!(
            refused("workflow(name = \"w\", phases = [])\nworkflow(name = \"v\", phases = [])\n")
                .contains("once")
        );
        assert!(refused("x = 1\n").contains("never calls workflow"));
        assert!(
            refused("workflow(name = \"w\", phases = [phase(\"P\", \"nowhere\", [])])\n")
                .contains("doesn't fit")
        );
        assert!(refused("review(rol = \"reviewer\")\n").contains("rol"));
        assert!(refused("fail(\"stop here\")\n").contains("stop here"));
    }
}
