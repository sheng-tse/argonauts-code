//! Runs a workflow. A workflow is a file of steps that each name a role, or a script in the
//! format Claude Code writes its own; a file of steps becomes a script, and one engine runs
//! both. The engine is a sandbox: a script can ask for agents and nothing else.
//!
//! - [`WorkflowFile`] reads and checks a file of `[[step]]` entries.
//! - [`Script`] reads a script, or builds one from a file's steps.
//! - [`run`] runs a script against a [`Host`], which sends each `agent()` to the CLI and model
//!   its role names. It reports the whole state on every change, in the shape Claude Code
//!   reports its own workflows, and hands the host every finished call to save.
//! - [`dry_run`] says what a file would run, and runs nothing.
//! - [`rehearse`] runs a script against placeholder answers, to catch one that throws before any
//!   agent starts, and [`left_out`] and [`without_verdicts`] read what its agents did.
//! - [`Library`] finds workflows on disk and the built-in ones.
//! - [`change`] makes one change to a file of steps in its text, comments kept.

mod call;
mod edit;
mod file;
mod kept;
mod library;
mod plan;
mod rehearse;
mod report;
mod require;
mod run;
mod sandbox;
mod script;

pub use call::{AgentCall, AgentEnd, Check, Checked, Host, JournalEntry, Kept};
pub use edit::{Change, EditError, FILE_FIELDS, STEP_FIELDS, change};
pub use file::{Count, FileError, ForEach, KeptBy, Problem, Step, WORKFLOW_KEYS, WorkflowFile};
pub use kept::{left_out, without_verdicts};
pub use library::{
    Library, LibraryError, Listed, Loaded, Origin, README, built_in, built_ins, check_name,
    template,
};
pub use plan::{DryRun, Estimate, Row, dry_run, how};
pub use rehearse::{Asked, Rehearsal, gives_verdict, rehearse};
pub use require::{Required, RequiredStep};
pub use run::{Ended, RunInput, run};
pub use sandbox::{CallKind, Limits, ScriptEnd};
pub use script::{Meta, MetaPhase, Script, ScriptError};
