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
pub(crate) use crate::behaviors::actions::Outcome;

/// One tool call, as recorded, with its label.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Recorded {
    /// Its project's state folder, run or conversation, session and call, which no other
    /// action shares.
    pub key: String,
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
    /// Part of a task's work, which has an outcome once the run ends.
    pub in_task: bool,
    pub outcome: Option<Outcome>,
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

/// The actions of every project, or of the one whose state folder is `only`, since `since`
/// when it's given. An ended run's labels are read as they were kept, so a log is labelled
/// once however often the map opens.
pub(crate) fn records(
    paths: &Paths,
    only: Option<&Path>,
    since: Option<OffsetDateTime>,
) -> Records {
    let only = only
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned());
    let actions = crate::behaviors::records::Records::of(paths)
        .actions(since)
        .into_iter()
        .filter(|action| only.as_ref().is_none_or(|repo| action.repo == *repo))
        .collect();
    of_actions(actions)
}

/// The map's records of `actions`, each numbered within its session.
pub(crate) fn of_actions(actions: Vec<Action>) -> Records {
    let sources = |conversation: bool| {
        actions
            .iter()
            .filter(|action| action.conversation == conversation)
            .map(|action| (action.repo.as_str(), action.run.as_str()))
            .collect::<BTreeSet<_>>()
            .len()
    };
    let (runs, conversations) = (sources(false), sources(true));
    let mut actions: Vec<Recorded> = actions.into_iter().map(recorded).collect();
    number_within_sessions(&mut actions);
    Records {
        actions,
        runs,
        conversations,
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
    Recorded {
        key: format!(
            "{}/{}/{}/{}",
            action.repo, action.run, action.session, action.call
        ),
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
        in_task: action.task.is_some(),
        outcome: action.outcome,
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
    let session = |key: &str| key.rsplit_once('/').map_or(key, |(s, _)| s).to_owned();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for action in actions.iter_mut() {
        let seen = counts.entry(session(&action.key)).or_default();
        *seen += 1;
        action.nth = *seen;
    }
    for action in actions.iter_mut() {
        action.of = counts[&session(&action.key)];
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
