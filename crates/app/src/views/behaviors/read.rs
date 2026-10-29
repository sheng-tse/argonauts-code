//! Every action Argonauts recorded, as the map reads it: the labels' own records, so the map,
//! `argonauts behaviors` and the usage reports count the same actions the same way. Each one
//! keeps who took it, in which run, project and workflow, and how its work ended.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use argonauts_core::Paths;
use argonauts_core::behaviors::Behavior;
use argonauts_core::crew_stats::StatsRole;
use serde_json::Value;
use time::OffsetDateTime;

use crate::behaviors::actions::Action;
pub(crate) use crate::behaviors::actions::{Outcome, SessionEnded};

/// One tool call, as recorded, with its label.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Recorded {
    /// Its project's state folder, run or conversation, session and call, which no other
    /// action shares.
    pub key: String,
    /// The same without the call: the session it was taken in.
    pub session: String,
    pub run: Option<String>,
    /// The CLI's adapter id and the model's id.
    pub cli: String,
    pub model: Option<String>,
    pub role: String,
    pub agent: String,
    pub workflow: String,
    pub project: String,
    pub at: OffsetDateTime,
    pub nth: usize,
    pub of: usize,
    pub tool: String,
    /// What it was given, as one line: [`input_words`] of its input, or the command or file
    /// its labels were kept with.
    pub what: String,
    pub behavior: Behavior,
    /// Its tool call failed.
    pub failed: bool,
    /// Part of a task's work, which has an outcome once the run ends.
    pub in_task: bool,
    pub outcome: Option<Outcome>,
    /// When its outcome was settled.
    pub settled_at: Option<OffsetDateTime>,
    /// How and when its session ended, once the log says.
    pub ended: Option<SessionEnded>,
    pub ended_at: Option<OffsetDateTime>,
    pub place: Option<String>,
    pub run_shows: Option<String>,
}

/// What the records held.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Records {
    pub actions: Vec<Recorded>,
    pub runs: usize,
    pub conversations: usize,
}

/// The project a state folder holds: its name, without the id's hash.
pub(crate) fn project_name(state: &Path) -> String {
    let name = state
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    crate::behaviors::records::project_name(&name).to_owned()
}

/// Every action of every project, of all time. An ended run's labels are read as they were
/// kept, so a log is labelled once however often the map opens.
pub(crate) fn every_action(paths: &Paths) -> Vec<Action> {
    crate::behaviors::records::Records::of(paths).actions(None)
}

/// The map's records of `every` action, or of those in the project whose state folder is
/// `only`.
pub(crate) fn records(every: Vec<Action>, only: Option<&Path>) -> Records {
    let only = only
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned());
    let actions = every
        .into_iter()
        .filter(|action| only.as_ref().is_none_or(|repo| action.repo == *repo))
        .collect();
    of_actions(actions)
}

/// The map's records of `actions`, each numbered within its session.
pub(crate) fn of_actions(actions: Vec<Action>) -> Records {
    let mut actions: Vec<Recorded> = actions.into_iter().map(recorded).collect();
    number_within_sessions(&mut actions);
    Records::counted(actions)
}

impl Records {
    /// Those taken at or after `since`, with the runs and conversations they came from.
    pub fn since(&self, since: Option<OffsetDateTime>) -> Records {
        let Some(since) = since else {
            return self.clone();
        };
        Records::counted(
            self.actions
                .iter()
                .filter(|recorded| recorded.at >= since)
                .cloned()
                .collect(),
        )
    }

    fn counted(actions: Vec<Recorded>) -> Records {
        let sources = |conversation: bool| {
            actions
                .iter()
                .filter(|recorded| recorded.run.is_none() == conversation)
                .map(|recorded| {
                    recorded
                        .session
                        .rsplit_once('/')
                        .map_or("", |(source, _)| source)
                })
                .collect::<BTreeSet<_>>()
                .len()
        };
        let (runs, conversations) = (sources(false), sources(true));
        Records {
            actions,
            runs,
            conversations,
        }
    }
}

/// What a run whose start named no workflow groups under.
const NO_WORKFLOW: &str = "no workflow";

fn recorded(action: Action) -> Recorded {
    let project = action.project().to_owned();
    let (run, workflow, place) = if action.conversation {
        (None, "conversation".to_owned(), Some("in your folder"))
    } else {
        let place = action.task.is_some().then_some("in the run's worktree");
        (
            Some(action.run.clone()),
            action.workflow.unwrap_or_else(|| NO_WORKFLOW.to_owned()),
            place,
        )
    };
    let run_shows = action.note.or_else(|| {
        Some(
            match action.outcome? {
                Outcome::Merged => "Its task merged.",
                Outcome::SentBack => "Review sent its task back.",
                Outcome::Dropped => "Its task was dropped.",
            }
            .to_owned(),
        )
    });
    let session = format!("{}/{}/{}", action.repo, action.run, action.session);
    Recorded {
        key: format!("{session}/{}", action.call),
        session,
        run,
        cli: action.cli,
        model: action.model,
        role: role_words(action.role).to_owned(),
        agent: action.agent,
        workflow,
        project,
        at: action.at,
        nth: 0,
        of: 0,
        tool: action.tool,
        what: match &action.input {
            Value::Null => action.what,
            input => input_words(input),
        },
        behavior: action.behavior,
        failed: action.ok == Some(false),
        in_task: action.task.is_some(),
        outcome: action.outcome,
        settled_at: action.settled_at,
        ended: action.ended,
        ended_at: action.ended_at,
        place: place.map(str::to_owned),
        run_shows,
    }
}

/// The role as the map names it.
fn role_words(role: StatsRole) -> &'static str {
    match role {
        StatsRole::Main => "main agent",
        StatsRole::Worker => "builder",
        other => other.as_str(),
    }
}

/// Each action's place among its session's, and the session's count.
fn number_within_sessions(actions: &mut [Recorded]) {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for action in actions.iter_mut() {
        let seen = counts.entry(action.session.clone()).or_default();
        *seen += 1;
        action.nth = *seen;
    }
    for action in actions.iter_mut() {
        action.of = counts[&action.session];
    }
}

/// What an action was given, as one line the map shows: a shell command without its shell, a
/// file's path, a search's pattern, or the input itself.
pub(crate) fn input_words(input: &Value) -> String {
    let field = |name: &str| input.get(name).and_then(Value::as_str);
    if let Some(command) = input.get("command").or_else(|| input.get("cmd")) {
        let line = match command {
            Value::Array(parts) => parts
                .iter()
                .filter_map(Value::as_str)
                .next_back()
                .unwrap_or_default()
                .to_owned(),
            Value::String(line) => line.clone(),
            other => other.to_string(),
        };
        return without_shell(&line);
    }
    for name in [
        "file_path",
        "filePath",
        "path",
        "pattern",
        "query",
        "url",
        "AbsolutePath",
    ] {
        if let Some(value) = field(name) {
            return value.to_owned();
        }
    }
    match input {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// `/bin/zsh -lc 'git status'` as `git status`: Codex runs every command through a login shell.
fn without_shell(line: &str) -> String {
    for shell in [
        "/bin/zsh -lc ",
        "/bin/bash -lc ",
        "bash -lc ",
        "zsh -lc ",
        "sh -c ",
    ] {
        if let Some(rest) = line.strip_prefix(shell) {
            return rest.trim_matches('\'').to_owned();
        }
    }
    line.to_owned()
}
