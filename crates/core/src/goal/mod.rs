//! A goal: a condition to keep working until it holds, and how it's checked. It sits around a
//! workflow, a step, a task, or the main agent alone, on any CLI. This module holds what a goal
//! is as you set it ([`GoalSpec`]), how your words become its lines, and what the screens are
//! told about one ([`report::GoalReport`]). The engine's `goal` module checks and keeps it.

pub mod keep;
pub mod report;

use serde::{Deserialize, Serialize};

use crate::events::goal::{Criterion, CriterionCheck};
pub use crate::settings::goal::{CheckWith, GoalDefault, RoleGoal};

/// The longest condition `/goal` takes, as Codex's own goal does.
pub const MAX_CONDITION: usize = 4_000;
/// How the screens name the model that reads a goal's lines.
pub const THE_JUDGE: &str = "the judge";
/// How the screens name the project's own check commands, `checks.run`.
pub const YOUR_CHECKS: &str = "your checks";

/// A goal as it's set: what must hold, and how to check it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalSpec {
    /// The condition in the words it was set with.
    pub condition: String,
    /// What must hold, one thing a line, each with how it's checked.
    pub lines: Vec<Criterion>,
    /// What decides that a line holds.
    pub check: CheckWith,
    /// Commands for the goal as a whole, as a workflow file's `check = [...]` lists them. Every
    /// one must exit 0 for the goal to hold.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
    /// The most rounds it goes, when you set one. Unset, only the goal holding, you, or being
    /// stuck ends it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u32>,
    pub from: GoalFrom,
}

/// Where a goal came from, which the line that states it back says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalFrom {
    /// `/goal <condition>`.
    Command,
    /// The request's own words, such as "keep going until its tests pass".
    Request,
    /// `[goal] default = "from-request"`.
    Setting,
    /// `goal =` at the top of a workflow file.
    WorkflowFile,
    /// `until =` on a step.
    Step,
    /// `[roles.<role>] goal`: the task's own done-when lines.
    Role,
}

/// Who keeps the work going until the goal holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeptBy {
    /// Argonauts Code: after each try it runs the check and sends back what still fails.
    #[default]
    Argonauts,
    /// The CLI's own goal, which Codex has. Argonauts Code still runs the check.
    Cli,
}

/// What a goal sits around.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalScope {
    /// The whole workflow goes round until it holds.
    #[default]
    Workflow,
    /// One step: its agent tries, checks and tries again inside its phase.
    Step,
    /// One task, worked as a goal by its role.
    Task,
    /// The main agent alone, with no workflow.
    Captain,
}

/// What a project already has that a goal's lines can be checked with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CheckMeans {
    /// `checks.run` or `checks.full_suite` names a command.
    pub project_checks: bool,
}

impl GoalSpec {
    /// The goal `condition` sets: its lines, each with the check `check` gives it.
    pub fn from_words(
        condition: &str,
        check: CheckWith,
        means: CheckMeans,
        from: GoalFrom,
    ) -> Result<Self, String> {
        let condition: String = condition.trim().chars().take(MAX_CONDITION).collect();
        let lines = lines_of(&condition, check, means);
        if lines.is_empty() {
            return Err("a goal needs a condition, such as /goal the export tests pass".to_owned());
        }
        let spec = Self {
            condition,
            lines,
            check,
            commands: Vec::new(),
            max_rounds: None,
            from,
        };
        spec.checkable()?;
        Ok(spec)
    }

    /// A goal made of lines already written, such as a task's done-when list.
    pub fn from_lines(
        condition: &str,
        lines: Vec<Criterion>,
        check: CheckWith,
        from: GoalFrom,
    ) -> Self {
        Self {
            condition: condition.trim().to_owned(),
            lines,
            check,
            commands: Vec::new(),
            max_rounds: None,
            from,
        }
    }

    /// Refuses a goal nothing can check as it's set: commands only, and a line with none.
    pub fn checkable(&self) -> Result<(), String> {
        for command in self
            .commands
            .iter()
            .map(String::as_str)
            .chain(self.lines.iter().filter_map(|line| line.check.command()))
        {
            argv(command)?;
        }
        if self.check != CheckWith::Command || !self.commands.is_empty() {
            return Ok(());
        }
        match self
            .lines
            .iter()
            .find(|line| line.check == CriterionCheck::Review)
        {
            Some(line) => Err(format!(
                "no command checks \"{}\": name one in backticks, or let the judge read it with \
                 goal.check = \"auto\"",
                line.text
            )),
            None => Ok(()),
        }
    }

    /// Whether the judge reads any line.
    pub fn asks_the_judge(&self) -> bool {
        match self.check {
            CheckWith::Judge | CheckWith::Both => true,
            CheckWith::Command => false,
            CheckWith::Auto => {
                self.commands.is_empty()
                    && self
                        .lines
                        .iter()
                        .any(|line| line.check == CriterionCheck::Review)
            }
        }
    }

    /// The commands that check it, each once, in the order the lines name them.
    pub fn named_commands(&self) -> Vec<&str> {
        if self.check == CheckWith::Judge {
            return Vec::new();
        }
        let mut commands: Vec<&str> = Vec::new();
        let own = self.lines.iter().filter_map(|line| line.check.command());
        for command in own.chain(self.commands.iter().map(String::as_str)) {
            if !commands.contains(&command) {
                commands.push(command);
            }
        }
        commands
    }

    fn uses_project_checks(&self) -> bool {
        self.check != CheckWith::Judge
            && self.lines.iter().any(|line| line.check.by_project_checks())
    }

    /// `checked by cargo test -p export, then by the judge`.
    pub fn checked_by(&self) -> String {
        let mut by: Vec<String> = self
            .named_commands()
            .into_iter()
            .map(str::to_owned)
            .collect();
        if self.uses_project_checks() {
            by.push(YOUR_CHECKS.to_owned());
        }
        let judge = self.asks_the_judge();
        match (by.is_empty(), judge) {
            (true, _) => format!("checked by {THE_JUDGE}"),
            (false, false) => format!("checked by {}", by.join(", ")),
            (false, true) => format!("checked by {}, then by {THE_JUDGE}", by.join(", ")),
        }
    }

    /// What checks `line`: its command, `your checks`, or `the judge`.
    pub fn line_checked_by(&self, line: &Criterion) -> String {
        if self.check == CheckWith::Judge {
            return THE_JUDGE.to_owned();
        }
        let by_command = line
            .check
            .command()
            .map(str::to_owned)
            .or_else(|| {
                line.check
                    .by_project_checks()
                    .then(|| YOUR_CHECKS.to_owned())
            })
            .or_else(|| (!self.commands.is_empty()).then(|| self.commands.join(", ")));
        match (by_command, self.check) {
            (Some(command), CheckWith::Both) => format!("{command}, then {THE_JUDGE}"),
            (Some(command), _) => command,
            (None, _) => THE_JUDGE.to_owned(),
        }
    }

    /// `goal with 2 lines`.
    pub fn size_words(&self) -> String {
        match self.lines.len() {
            1 => "goal with 1 line".to_owned(),
            n => format!("goal with {n} lines"),
        }
    }
}

impl CriterionCheck {
    /// The command that checks the line, when it names one.
    pub fn command(&self) -> Option<&str> {
        match self {
            CriterionCheck::Command(command) => Some(command),
            CriterionCheck::Test(_) | CriterionCheck::Checks | CriterionCheck::Review => None,
        }
    }

    /// Whether the project's own checks decide the line: it says so, or it names a test, which
    /// passes when the suite that holds it does.
    pub fn by_project_checks(&self) -> bool {
        matches!(self, CriterionCheck::Checks | CriterionCheck::Test(_))
    }
}

/// A command line as an argument vector. No shell runs it, so a pipe or a redirect is refused
/// rather than passed to the program as a word.
pub fn argv(command: &str) -> Result<Vec<String>, String> {
    let words = shell_words::split(command)
        .map_err(|err| format!("the command `{command}` doesn't read: {err}"))?;
    if words.is_empty() {
        return Err("a check's command is empty".to_owned());
    }
    if let Some(word) = words.iter().find(|word| {
        matches!(
            word.as_str(),
            "|" | "||" | "&&" | ";" | ">" | ">>" | "<" | "&"
        )
    }) {
        return Err(format!(
            "the command `{command}` uses `{word}`, and no shell runs a check: name one program \
             and its arguments, or a script"
        ));
    }
    Ok(words)
}

/// The commands a workflow file lists for its goal, `check = [...]` beside `goal = "…"` at its
/// top, when that goal is `condition`. Every one must exit 0 for the goal to hold. Empty when
/// the file states another goal or none, lists no commands, or doesn't read.
pub fn checks_beside(file: &str, condition: &str) -> Vec<String> {
    let Ok(table) = file.parse::<toml::Table>() else {
        return Vec::new();
    };
    let states_it = table
        .get("goal")
        .and_then(toml::Value::as_str)
        .is_some_and(|goal| goal.trim() == condition.trim());
    let listed = table.get("check").and_then(toml::Value::as_array);
    match listed {
        Some(commands) if states_it => commands
            .iter()
            .filter_map(toml::Value::as_str)
            .map(str::trim)
            .filter(|command| !command.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// The condition's lines: one for each thing that must hold, split at line ends, semicolons,
/// and an "and" that joins two whole clauses.
fn lines_of(condition: &str, check: CheckWith, means: CheckMeans) -> Vec<Criterion> {
    split_clauses(condition)
        .into_iter()
        .enumerate()
        .map(|(index, clause)| {
            let check = line_check(&clause, check, means);
            Criterion {
                id: format!("G{}", index + 1),
                text: clause,
                short: String::new(),
                check,
            }
        })
        .collect()
}

/// How one line is checked. A command in backticks checks it whatever the setting, short of
/// `judge`; words about tests or checks passing take the project's own checks.
fn line_check(clause: &str, check: CheckWith, means: CheckMeans) -> CriterionCheck {
    if check == CheckWith::Judge {
        return CriterionCheck::Review;
    }
    if let Some(command) = backticked(clause) {
        return CriterionCheck::Command(command);
    }
    if means.project_checks && says_checks_pass(clause) {
        return CriterionCheck::Checks;
    }
    CriterionCheck::Review
}

/// The first backticked span that reads as a command.
fn backticked(clause: &str) -> Option<String> {
    clause
        .split('`')
        .skip(1)
        .step_by(2)
        .map(str::trim)
        .find(|span| !span.is_empty() && argv(span).is_ok())
        .map(str::to_owned)
}

/// "the tests pass", "its own tests pass", "every check passes", "the build is green".
fn says_checks_pass(clause: &str) -> bool {
    let words: Vec<String> = clause
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    let has = |any: &[&str]| words.iter().any(|word| any.contains(&word.as_str()));
    has(&[
        "test", "tests", "check", "checks", "suite", "build", "lint", "ci",
    ]) && has(&["pass", "passes", "passing", "green", "succeed", "succeeds"])
}

fn split_clauses(condition: &str) -> Vec<String> {
    let mut clauses = Vec::new();
    for part in outside_backticks(condition, &['\n', ';']) {
        let part = part
            .trim()
            .trim_start_matches(['-', '*', '•'])
            .trim()
            .to_owned();
        if part.is_empty() {
            continue;
        }
        clauses.extend(split_and(&part));
    }
    clauses
        .into_iter()
        .map(|clause| tidy(&clause))
        .filter(|clause| !clause.is_empty())
        .collect()
}

/// `text` cut at each of `at` that isn't inside backticks.
fn outside_backticks(text: &str, at: &[char]) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut quoted = false;
    for c in text.chars() {
        if c == '`' {
            quoted = !quoted;
        }
        if !quoted && at.contains(&c) {
            parts.push(String::new());
        } else if let Some(part) = parts.last_mut() {
            part.push(c);
        }
    }
    parts
}

/// Splits at " and " only where both sides read as whole clauses: three words or more each, so
/// "the add and remove commands work" stays one line.
fn split_and(clause: &str) -> Vec<String> {
    const AND: &str = " and ";
    let mut quoted = false;
    let lower = clause.to_lowercase();
    let mut cuts = Vec::new();
    for (at, c) in lower.char_indices() {
        if c == '`' {
            quoted = !quoted;
        }
        if !quoted && lower[at..].starts_with(AND) {
            cuts.push(at);
        }
    }
    let whole = |text: &str| text.split_whitespace().count() >= 3;
    let mut parts = Vec::new();
    let mut from = 0;
    for cut in cuts {
        let (left, right) = (&clause[from..cut], &clause[cut + AND.len()..]);
        let next = right.to_lowercase();
        let next = next.split(AND).next().unwrap_or_default();
        if whole(left) && whole(next) {
            parts.push(left.to_owned());
            from = cut + AND.len();
        }
    }
    parts.push(clause[from..].to_owned());
    parts
}

fn tidy(clause: &str) -> String {
    let clause = clause.trim().trim_end_matches(['.', ',']).trim();
    let lower = clause.to_lowercase();
    for opener in ["until ", "that ", "then "] {
        if lower.starts_with(opener) {
            return clause[opener.len()..].trim().to_owned();
        }
    }
    clause.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(condition: &str, check: CheckWith, project_checks: bool) -> GoalSpec {
        GoalSpec::from_words(
            condition,
            check,
            CheckMeans { project_checks },
            GoalFrom::Command,
        )
        .expect("a goal")
    }

    fn texts(spec: &GoalSpec) -> Vec<&str> {
        spec.lines.iter().map(|line| line.text.as_str()).collect()
    }

    #[test]
    fn a_condition_becomes_one_line_for_each_thing_that_must_hold() {
        let goal = spec(
            "the export tests pass and the CSV has a header row",
            CheckWith::Auto,
            false,
        );
        assert_eq!(
            texts(&goal),
            ["the export tests pass", "the CSV has a header row"]
        );
        assert_eq!(goal.lines[0].id, "G1");
        assert_eq!(goal.lines[1].id, "G2");
        assert_eq!(goal.size_words(), "goal with 2 lines");
    }

    #[test]
    fn an_and_inside_one_thing_is_left_alone() {
        let goal = spec("the add and remove commands work", CheckWith::Auto, false);
        assert_eq!(texts(&goal), ["the add and remove commands work"]);
        let goal = spec(
            "`cargo test add and remove` passes; the README explains the flag.",
            CheckWith::Auto,
            false,
        );
        assert_eq!(
            texts(&goal),
            [
                "`cargo test add and remove` passes",
                "the README explains the flag"
            ]
        );
    }

    #[test]
    fn auto_takes_a_named_command_then_your_checks_then_the_judge() {
        let goal = spec(
            "`cargo test -p export` passes and the CSV has a header row",
            CheckWith::Auto,
            true,
        );
        assert_eq!(
            goal.lines[0].check,
            CriterionCheck::Command("cargo test -p export".to_owned())
        );
        assert_eq!(goal.lines[1].check, CriterionCheck::Review);
        assert_eq!(
            goal.checked_by(),
            "checked by cargo test -p export, then by the judge"
        );

        let goal = spec("its own tests pass", CheckWith::Auto, true);
        assert_eq!(goal.lines[0].check, CriterionCheck::Checks);
        assert_eq!(goal.checked_by(), "checked by your checks");

        // With no checks set up, only the judge can tell.
        let goal = spec("its own tests pass", CheckWith::Auto, false);
        assert_eq!(goal.lines[0].check, CriterionCheck::Review);
        assert_eq!(goal.checked_by(), "checked by the judge");
    }

    #[test]
    fn the_judge_alone_reads_every_line_and_both_adds_it_to_the_commands() {
        let judge = spec("`make test` passes", CheckWith::Judge, true);
        assert_eq!(judge.lines[0].check, CriterionCheck::Review);
        assert_eq!(judge.checked_by(), "checked by the judge");
        assert!(judge.named_commands().is_empty());

        let both = spec("`make test` passes", CheckWith::Both, true);
        assert_eq!(both.checked_by(), "checked by make test, then by the judge");
    }

    #[test]
    fn commands_only_refuses_a_line_no_command_checks() {
        let refused = GoalSpec::from_words(
            "the README explains the new flag",
            CheckWith::Command,
            CheckMeans::default(),
            GoalFrom::Command,
        )
        .expect_err("nothing checks it");
        assert!(refused.contains("no command checks"), "{refused}");
        // The workflow file's own commands check every line.
        let mut goal = GoalSpec::from_lines(
            "every acceptance test passes",
            vec![Criterion::new("G1", "every acceptance test passes")],
            CheckWith::Command,
            GoalFrom::WorkflowFile,
        );
        goal.commands = vec!["cargo test --test acceptance".to_owned()];
        assert_eq!(goal.checkable(), Ok(()));
        assert_eq!(goal.checked_by(), "checked by cargo test --test acceptance");
    }

    #[test]
    fn a_command_is_an_argument_vector_and_never_a_shell_line() {
        assert_eq!(
            argv("cargo test --test 'two words'"),
            Ok(vec![
                "cargo".to_owned(),
                "test".to_owned(),
                "--test".to_owned(),
                "two words".to_owned()
            ])
        );
        assert!(argv("cargo test | tee out.txt").is_err());
        assert!(argv("make && make test").is_err());
        assert!(argv("").is_err());
        // A backticked span that isn't a command leaves the line to the judge.
        let goal = spec("the `a | b` filter is documented", CheckWith::Auto, false);
        assert_eq!(goal.lines[0].check, CriterionCheck::Review);
    }

    /// A goal for the whole workflow, and the command that decides it.
    #[test]
    fn a_workflow_file_lists_the_commands_beside_its_goal() {
        let file = "goal  = \"every acceptance test passes\"\n\
                    check = [\"cargo test --test acceptance\", \" make lint \", \"\"]\n\n\
                    [[step]]\nname = \"Build\"\nrole = \"worker\"\n";
        assert_eq!(
            checks_beside(file, "every acceptance test passes"),
            ["cargo test --test acceptance", "make lint"]
        );
        // Another condition isn't this file's goal, so its commands say nothing about it.
        assert!(checks_beside(file, "the docs build").is_empty());
        // `check` in a settings table is the way to check, not a list of commands.
        let settings = "[goal]\ncheck = \"auto\"\n";
        assert!(checks_beside(settings, "auto").is_empty());
        assert!(checks_beside("goal = \"it works\"\n", "it works").is_empty());
        assert!(checks_beside("goal = [", "it works").is_empty());
    }

    #[test]
    fn an_empty_condition_is_refused() {
        let refused = GoalSpec::from_words(
            "  ",
            CheckWith::Auto,
            CheckMeans::default(),
            GoalFrom::Command,
        );
        assert!(refused.is_err());
    }
}
