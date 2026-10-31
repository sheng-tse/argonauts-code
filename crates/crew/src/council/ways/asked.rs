//! Ways you name in your request. Each binds: the task it is for is marked "You asked", a seat
//! may object with a reason but only you change it, and code checks the plan against them
//! before work starts.

use std::cmp::Reverse;

use argonauts_core::TaskId;
use argonauts_core::base::names::ModelRef;
use argonauts_core::runs::events::task::{TaskSpec, TaskWay, Way};
use argonauts_core::settings::ways::MostAgents;
use argonauts_core::workflows::workflow::WorkflowRef;

use super::thumb::is_file;
use super::{Offer, done_by, parts, propose, words};

/// A way your request names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskedWay {
    Exactly(Way),
    /// Agents, with no count said, as "the route tests across agents".
    Agents,
}

impl AskedWay {
    pub fn matches(&self, way: &Way) -> bool {
        match self {
            Self::Exactly(asked) => asked == way,
            Self::Agents => matches!(way, Way::Agents { .. }),
        }
    }

    fn done_by(&self) -> String {
        match self {
            Self::Exactly(way) => done_by(way),
            Self::Agents => "by agents".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    pub way: AskedWay,
    /// Your words that name it, as you wrote them.
    pub said: String,
    /// Your words for the work and its way together, as the proposal lists them: "the limiter
    /// by Maker-checker".
    pub clause: String,
    /// The words around it that say which work it is for, lowercased: "the route tests across
    /// agents" gives `route` and `tests`, and so does "3 agents split the route tests".
    pub about: Vec<String>,
    /// The model you named to do it, as "let 5.6 Luna do the plan limits in its own session":
    /// its task goes to that model.
    pub who: Option<ModelRef>,
}

/// Words that join a way to the work it is for, or say nothing of which work it is.
const CONNECTORS: &[&str] = &[
    "a", "an", "the", "by", "in", "with", "across", "using", "via", "its", "their", "own", "one",
    "do", "does", "done", "build", "make", "run", "have", "let", "should", "please", "and", "then",
    "also", "but", "for", "of", "to", "on", "as", "use", "split", "too", "each", "all",
];

/// Words that end the stretch a way's work is named in.
const CLAUSE_ENDS: &[&str] = &["and", "then", "also", "but"];

/// Words before "agents" that ask for agents without a count.
const SOME_AGENTS: &[&str] = &[
    "across", "with", "by", "using", "among", "between", "parallel", "several", "multiple",
    "separate",
];

/// The ways `request` names, once for each work it names them for, in the order it names
/// them. `workflows` is every workflow a task could run.
pub fn asked_in(request: &str, workflows: &[String]) -> Vec<Asked> {
    let words = Words::of(request);
    let names: Vec<(Vec<String>, &String)> = workflows
        .iter()
        .map(|name| (name.split('-').map(str::to_owned).collect(), name))
        .collect();
    let mut asked: Vec<Asked> = Vec::new();
    let mut at = 0;
    while at < words.len() {
        let Some((way, len)) = way_at(&words, at, &names) else {
            at += 1;
            continue;
        };
        let from = words.clause_start(at);
        let to = words.clause_end(at + len);
        let mut about = words.about(from, at);
        about.extend(words.about(at + len, to));
        if !asked
            .iter()
            .any(|known| known.way == way && known.about == about)
        {
            asked.push(Asked {
                way,
                said: words.text(at, at + len),
                clause: words.text(from, to.max(at + len)),
                about,
                who: None,
            });
        }
        at += len;
    }
    asked
}

/// The proposal's line of the ways you asked for, each in your words: "Yours, binding: the
/// limiter by Maker-checker · the route tests across agents". `None` when you asked for none.
pub fn binding_line(asked: &[Asked]) -> Option<String> {
    if asked.is_empty() {
        return None;
    }
    let clauses: Vec<&str> = asked.iter().map(|asked| asked.clause.as_str()).collect();
    Some(format!("Yours, binding: {}", clauses.join(" · ")))
}

/// Marks each task your request names a way for, where its words point at one task alone,
/// and gives it that way: your word wins over the plan's. Returns the tasks it marked.
pub fn attach(tasks: &mut [TaskSpec], asked: &[Asked]) -> Vec<TaskId> {
    let mut marked = Vec::new();
    for wanted in asked {
        let Some(at) = best_match(tasks.iter(), &wanted.about) else {
            continue;
        };
        // The model you named does the work, whoever the plan gave it to, and the reviewers
        // the plan picked for its own worker go, so code picks them again for yours.
        if let Some(who) = &wanted.who
            && tasks[at].worker.as_ref() != Some(who)
        {
            tasks[at].worker = Some(who.clone());
            tasks[at].reviewers.clear();
        }
        if met(tasks, wanted) {
            continue;
        }
        let task = &mut tasks[at];
        let how = match &wanted.way {
            AskedWay::Exactly(how) => how.clone(),
            // With no count said, the plan's count stands, else one agent per part.
            AskedWay::Agents => match task.way.as_ref().map(|way| &way.how) {
                Some(agents @ Way::Agents { .. }) => agents.clone(),
                _ => match parts(&task.owned_paths, MostAgents::MOST.get()).len() {
                    count @ 2.. => Way::Agents { count: count as u8 },
                    _ => continue,
                },
            },
        };
        task.way = Some(TaskWay {
            how,
            asked_by_you: true,
            steps: None,
            why: None,
        });
        marked.push(task.id.clone());
    }
    marked
}

/// Takes the "You asked" mark off each task whose way your request doesn't name, so it reads
/// as the note keeper's. Returns a line for each.
pub fn unmark(tasks: &mut [TaskSpec], asked: &[Asked]) -> Vec<String> {
    let mut lines = Vec::new();
    for task in tasks.iter_mut() {
        let Some(way) = task.way.as_mut().filter(|way| way.asked_by_you) else {
            continue;
        };
        if asked.iter().any(|wanted| wanted.way.matches(&way.how)) {
            continue;
        }
        way.asked_by_you = false;
        lines.push(format!(
            "{} was marked You asked, but your request names no work done {}, so it is the \
             note keeper's way.",
            task.title,
            done_by(&way.how)
        ));
    }
    lines
}

/// Whether a task is done the way `wanted` asks, marked You asked: the task its words name,
/// or any task when they name none.
fn met(tasks: &[TaskSpec], wanted: &Asked) -> bool {
    let named = (!wanted.about.is_empty())
        .then(|| best_match(tasks.iter(), &wanted.about))
        .flatten();
    tasks.iter().enumerate().any(|(at, task)| {
        named.is_none_or(|named| named == at)
            && task
                .way
                .as_ref()
                .is_some_and(|way| way.asked_by_you && wanted.way.matches(&way.how))
    })
}

/// What the plan breaks of the ways you asked for: a way the work you named it for isn't done
/// by, or a task marked "You asked" for a way your request doesn't name. `set_by_you` are the
/// tasks whose way you set at the approval, which bind like your request.
pub fn check(tasks: &[TaskSpec], asked: &[Asked], set_by_you: &[TaskId]) -> Vec<String> {
    let mut broken = Vec::new();
    for wanted in asked {
        if met(tasks, wanted) {
            continue;
        }
        let named = (!wanted.about.is_empty())
            .then(|| best_match(tasks.iter(), &wanted.about))
            .flatten();
        broken.push(match named {
            Some(at) => format!(
                "You asked for {}, and {} isn't done that way.",
                wanted.clause, tasks[at].title
            ),
            None => format!(
                "You asked for work done {}, and no task in the plan is.",
                wanted.way.done_by()
            ),
        });
    }
    for task in tasks {
        let Some(way) = task.way.as_ref().filter(|way| way.asked_by_you) else {
            continue;
        };
        if set_by_you.contains(&task.id) || asked.iter().any(|a| a.way.matches(&way.how)) {
            continue;
        }
        broken.push(format!(
            "{} is marked You asked, but your request names no work done {}.",
            task.title,
            done_by(&way.how)
        ));
    }
    broken
}

/// The plan's Why for a task's way: "You asked", "You asked · off in your setup", "You asked
/// · 3 files", "You changed it · the note keeper had Own session", or the rule of thumb's
/// reason. `had` is the way the note keeper gave the task before you changed it.
pub fn why(task: &TaskSpec, offer: &Offer, had: Option<&Way>) -> String {
    let Some(way) = &task.way else {
        return String::new();
    };
    if way.asked_by_you {
        if let Some(had) = had.filter(|had| **had != way.how) {
            return format!("You changed it · the note keeper had {}", words(had));
        }
        if !offer.allows(&way.how) {
            return "You asked · off in your setup".to_owned();
        }
        if let Way::Agents { count } = way.how {
            let split = parts(&task.owned_paths, count);
            if split.len() >= 2 {
                let what = if split.iter().flatten().all(|path| is_file(path)) {
                    "files"
                } else {
                    "parts"
                };
                return format!("You asked · {} {what}", split.len());
            }
        }
        return "You asked".to_owned();
    }
    let proposed = propose(task, offer);
    if proposed.way == way.how {
        proposed.why
    } else {
        "The note keeper's pick".to_owned()
    }
}

/// A seat's objection to a way you asked for, once your way still stands after the approval.
pub fn yours_stands(objection: &str) -> String {
    format!("{} · yours stands", objection.trim().trim_end_matches('.'))
}

fn way_at(words: &Words, at: usize, names: &[(Vec<String>, &String)]) -> Option<(AskedWay, usize)> {
    for (parts, name) in names {
        if words.run_is(at, parts) {
            let way = Way::Workflow {
                name: WorkflowRef::new(name),
            };
            return Some((AskedWay::Exactly(way), parts.len()));
        }
    }
    if words.run_is(at, &["own", "session"]) {
        return Some((AskedWay::Exactly(Way::OwnSession), 2));
    }
    if words.run_is(at, &["session", "of", "its", "own"]) {
        return Some((AskedWay::Exactly(Way::OwnSession), 4));
    }
    let next_is_agents = words.is(at + 1, "agents") && !words.names_a_file(at + 1);
    if !next_is_agents {
        return None;
    }
    let count = match words.lower(at)? {
        "2" | "two" => Some(2),
        "3" | "three" => Some(3),
        "4" | "four" => Some(4),
        _ => None,
    };
    match count {
        Some(count) => Some((AskedWay::Exactly(Way::Agents { count }), 2)),
        None if SOME_AGENTS.contains(&words.lower(at)?) => Some((AskedWay::Agents, 2)),
        None => None,
    }
}

/// Where the one task sits that `text` names: the one whose words share the most with its
/// content words, as a way's work names its task.
pub fn task_named<'a>(tasks: impl IntoIterator<Item = &'a TaskSpec>, text: &str) -> Option<usize> {
    let words = Words::of(text);
    best_match(tasks, &words.about(0, words.len()))
}

/// Where the one task sits whose words share the most with `about`, when no other shares as
/// many. Of two that share as many, the one whose title says less else wins, so "the store" is
/// the store's task and not its acceptance tests'.
pub(super) fn best_match<'a>(
    tasks: impl IntoIterator<Item = &'a TaskSpec>,
    about: &[String],
) -> Option<usize> {
    let scores: Vec<(usize, Reverse<usize>)> = tasks
        .into_iter()
        .map(|task| (score(task, about), Reverse(stems(&task.title).len())))
        .collect();
    let best = *scores.iter().max()?;
    if best.0 == 0 || scores.iter().filter(|score| **score == best).count() > 1 {
        return None;
    }
    scores.iter().position(|score| *score == best)
}

/// A word in the title counts twice, a word in the task's text once.
fn score(task: &TaskSpec, about: &[String]) -> usize {
    let title = stems(&task.title);
    let text = stems(&format!(
        "{} {}",
        task.task,
        task.item.as_deref().unwrap_or("")
    ));
    about
        .iter()
        .map(|word| {
            let word = stem(word);
            2 * usize::from(title.contains(&word)) + usize::from(text.contains(&word))
        })
        .sum()
}

fn stems(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(stem)
        .collect()
}

fn stem(word: &str) -> String {
    let word = word.to_lowercase();
    match word.strip_suffix('s') {
        Some(stem) if stem.len() >= 3 => stem.to_owned(),
        _ => word,
    }
}

/// A request's words, lowercased, with where each sits in the text.
struct Words<'a> {
    text: &'a str,
    spans: Vec<(usize, usize)>,
    lower: Vec<String>,
}

impl<'a> Words<'a> {
    fn of(text: &'a str) -> Self {
        let mut spans = Vec::new();
        let mut start = None;
        for (at, c) in text.char_indices() {
            let inside = c.is_alphanumeric() || (c == '\'' && start.is_some());
            match (inside, start) {
                (true, None) => start = Some(at),
                (false, Some(from)) => {
                    spans.push((from, at));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(from) = start {
            spans.push((from, text.len()));
        }
        let lower = spans
            .iter()
            .map(|(from, to)| text[*from..*to].to_lowercase())
            .collect();
        Self { text, spans, lower }
    }

    fn len(&self) -> usize {
        self.spans.len()
    }

    fn lower(&self, at: usize) -> Option<&str> {
        self.lower.get(at).map(String::as_str)
    }

    fn is(&self, at: usize, word: &str) -> bool {
        self.lower(at) == Some(word)
    }

    fn run_is<S: AsRef<str>>(&self, at: usize, run: &[S]) -> bool {
        !run.is_empty()
            && run
                .iter()
                .enumerate()
                .all(|(n, word)| self.is(at + n, word.as_ref()))
    }

    /// The text from word `from` up to word `to`, as written.
    fn text(&self, from: usize, to: usize) -> String {
        self.text[self.spans[from].0..self.spans[to - 1].1].to_owned()
    }

    /// Whether word `at` is a file's name, as `AGENTS.md`.
    fn names_a_file(&self, at: usize) -> bool {
        let end = self.spans[at].1;
        let mut after = self.text[end..].chars();
        after.next() == Some('.') && after.next().is_some_and(char::is_alphanumeric)
    }

    /// Whether a clause ends between word `at` and the one before it: a stop, comma, colon or
    /// semicolon, a line break, or a bullet.
    fn breaks_before(&self, at: usize) -> bool {
        let between = &self.text[self.spans[at - 1].1..self.spans[at].0];
        between.contains([',', ';', ':', '!', '?', '\n', '·', '(', ')'])
            || between.trim_start().starts_with('.') && between.ends_with(char::is_whitespace)
    }

    /// Where the clause word `at` sits in begins: after a stop, a comma or the like, or after a
    /// word such as "and" that starts a new one.
    fn clause_start(&self, at: usize) -> usize {
        let mut word = at;
        while word > 0
            && !self.breaks_before(word)
            && !CLAUSE_ENDS.contains(&self.lower[word - 1].as_str())
        {
            word -= 1;
        }
        word
    }

    /// Where the clause that word `at` sits in ends: before a stop, a comma or the like, or
    /// before a word such as "and" that starts a new one.
    fn clause_end(&self, at: usize) -> usize {
        let mut word = at;
        while word < self.len()
            && !self.breaks_before(word)
            && !CLAUSE_ENDS.contains(&self.lower[word].as_str())
        {
            word += 1;
        }
        word
    }

    /// The content words from word `from` up to word `at`.
    fn about(&self, from: usize, at: usize) -> Vec<String> {
        self.lower[from..at]
            .iter()
            .filter(|lower| {
                lower.chars().count() >= 3
                    && !lower.chars().all(|c| c.is_ascii_digit())
                    && !CONNECTORS.contains(&lower.as_str())
            })
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::runs::events::task::{TaskKind, TaskLimit};
    use argonauts_core::settings::ways::Ways;

    use super::*;
    use crate::council::ways::{Setup, task_workflows};

    const REQUEST: &str = "Rate limit the API: the limiter by Maker-checker, the plan limits by \
                           5.6 Luna in its own session, and the route tests across agents.";

    fn workflow(name: &str) -> Way {
        Way::Workflow {
            name: WorkflowRef::new(name),
        }
    }

    fn task(n: u32, title: &str, text: &str, owned: &[&str]) -> TaskSpec {
        TaskSpec {
            id: TaskId::numbered(n),
            title: title.to_owned(),
            task: text.to_owned(),
            kind: TaskKind::Build,
            owned_paths: owned.iter().map(|path| (*path).to_owned()).collect(),
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: Vec::new(),
            worker: None,
            reviewers: Vec::new(),
            limit: TaskLimit::default(),
            predictions: None,
            item: None,
            way: None,
        }
    }

    fn plan() -> Vec<TaskSpec> {
        vec![
            task(
                1,
                "Redis bucket",
                "A token bucket limiter per API key, kept in Redis.",
                &["src/limit/redis.ts"],
            ),
            task(
                2,
                "Plan limits",
                "Read each key's plan and its limits.",
                &["src/limit/plans.ts"],
            ),
            task(
                3,
                "Memory store",
                "An in-memory store for tests.",
                &["src/store/memory.ts"],
            ),
            task(
                4,
                "Route tests",
                "Tests for each route.",
                &[
                    "test/routes/orders.test.ts",
                    "test/routes/users.test.ts",
                    "test/routes/import.test.ts",
                ],
            ),
        ]
    }

    fn known() -> Vec<String> {
        task_workflows([])
    }

    fn how(task: &TaskSpec) -> Option<(&Way, bool)> {
        task.way.as_ref().map(|way| (&way.how, way.asked_by_you))
    }

    #[test]
    fn a_request_names_its_ways_and_the_work_each_is_for() {
        let asked = asked_in(REQUEST, &known());
        let found: Vec<(&AskedWay, &str, Vec<&str>)> = asked
            .iter()
            .map(|asked| {
                let about = asked.about.iter().map(String::as_str).collect();
                (&asked.way, asked.said.as_str(), about)
            })
            .collect();
        assert_eq!(
            found,
            [
                (
                    &AskedWay::Exactly(workflow("maker-checker")),
                    "Maker-checker",
                    vec!["limiter"]
                ),
                (
                    &AskedWay::Exactly(Way::OwnSession),
                    "own session",
                    vec!["plan", "limits", "luna"]
                ),
                (&AskedWay::Agents, "across agents", vec!["route", "tests"]),
            ]
        );
    }

    #[test]
    fn the_proposal_lists_your_ways_in_your_words() {
        let asked = asked_in(REQUEST, &known());
        assert_eq!(
            binding_line(&asked).as_deref(),
            Some(
                "Yours, binding: the limiter by Maker-checker · the plan limits by 5.6 Luna in \
                 its own session · the route tests across agents"
            )
        );
        assert_eq!(binding_line(&[]), None);
    }

    #[test]
    fn counts_and_other_names_are_read_and_files_are_not_ways() {
        let asked = asked_in(
            "Split the docs over three agents, keep AGENTS.md, and try best of n.",
            &known(),
        );
        let ways: Vec<&AskedWay> = asked.iter().map(|asked| &asked.way).collect();
        assert_eq!(
            ways,
            [
                &AskedWay::Exactly(Way::Agents { count: 3 }),
                &AskedWay::Exactly(workflow("best-of-n")),
            ]
        );
        assert!(asked_in("Fix the agents list in the panel.", &known()).is_empty());
    }

    #[test]
    fn each_way_goes_to_the_one_task_its_words_name() {
        let mut tasks = plan();
        tasks[3].way = Some(TaskWay {
            how: Way::Agents { count: 3 },
            asked_by_you: false,
            steps: None,
            why: None,
        });
        let asked = asked_in(REQUEST, &known());
        let marked = attach(&mut tasks, &asked);
        assert_eq!(
            marked,
            [
                TaskId::numbered(1),
                TaskId::numbered(2),
                TaskId::numbered(4)
            ]
        );
        assert_eq!(how(&tasks[0]), Some((&workflow("maker-checker"), true)));
        assert_eq!(how(&tasks[1]), Some((&Way::OwnSession, true)));
        assert_eq!(how(&tasks[2]), None);
        assert_eq!(how(&tasks[3]), Some((&Way::Agents { count: 3 }, true)));
        assert!(check(&tasks, &asked, &[]).is_empty());
    }

    #[test]
    fn the_task_a_way_names_beats_its_acceptance_tests() {
        let mut tasks = vec![
            task(1, "store", "Build the store.", &["src/store/**"]),
            task(
                2,
                "Acceptance tests for store",
                "Write the acceptance tests for task-001 (store): Build the store.",
                &["tests/acceptance/task-001/**"],
            ),
        ];
        let asked = asked_in("Rate-limit the API: the store by Best-of-n.", &known());
        assert_eq!(attach(&mut tasks, &asked), [TaskId::numbered(1)]);
    }

    #[test]
    fn agents_with_no_count_take_one_per_part_and_a_mark_you_never_made_comes_off() {
        let mut tasks = plan();
        tasks[2].way = Some(TaskWay {
            how: workflow("best-of-n"),
            asked_by_you: true,
            steps: None,
            why: None,
        });
        let asked = asked_in(REQUEST, &known());
        attach(&mut tasks, &asked);
        assert_eq!(how(&tasks[3]), Some((&Way::Agents { count: 3 }, true)));
        assert_eq!(
            unmark(&mut tasks, &asked),
            [
                "Memory store was marked You asked, but your request names no work done by \
              Best-of-n, so it is the note keeper's way."
            ]
        );
        assert_eq!(how(&tasks[2]), Some((&workflow("best-of-n"), false)));
        assert!(check(&tasks, &asked, &[]).is_empty());
    }

    #[test]
    fn the_check_names_a_way_left_out_and_a_mark_you_never_asked_for() {
        let mut tasks = plan();
        let asked = asked_in(REQUEST, &known());
        tasks[0].way = Some(TaskWay {
            how: workflow("maker-checker"),
            asked_by_you: true,
            steps: None,
            why: None,
        });
        tasks[2].way = Some(TaskWay {
            how: workflow("best-of-n"),
            asked_by_you: true,
            steps: None,
            why: None,
        });
        assert_eq!(
            check(&tasks, &asked, &[]),
            [
                "You asked for the plan limits by 5.6 Luna in its own session, and Plan limits \
                 isn't done that way.",
                "You asked for the route tests across agents, and Route tests isn't done that \
                 way.",
                "Memory store is marked You asked, but your request names no work done by \
                 Best-of-n.",
            ]
        );
        // A way you set at the approval binds like one in your request.
        let broken = check(&tasks, &asked, &[TaskId::numbered(3)]);
        assert_eq!(broken.len(), 2, "{broken:?}");
    }

    /// One way asked for two pieces of work is two asks, each checked against its own task.
    #[test]
    fn a_way_asked_twice_binds_each_work_it_names() {
        let asked = asked_in(
            "Build the limiter with Maker-checker, and the memory store with Maker-checker too.",
            &known(),
        );
        let about: Vec<Vec<&str>> = asked
            .iter()
            .map(|asked| asked.about.iter().map(String::as_str).collect())
            .collect();
        assert_eq!(about, [vec!["limiter"], vec!["memory", "store"]]);
        let mut tasks = plan();
        tasks[0].way = Some(TaskWay {
            how: workflow("maker-checker"),
            asked_by_you: true,
            steps: None,
            why: None,
        });
        let broken = check(&tasks, &asked, &[]);
        assert_eq!(
            broken,
            [
                "You asked for the memory store with Maker-checker too, and Memory store isn't done \
              that way."
            ]
        );
        let marked = attach(&mut tasks, &asked);
        assert_eq!(marked, [TaskId::numbered(3)]);
        assert!(check(&tasks, &asked, &[]).is_empty());
    }

    /// The work may come after the way, and "a session of its own" is a task's own session.
    #[test]
    fn the_work_after_a_way_names_its_task() {
        let about = |request: &str| -> Vec<(AskedWay, Vec<String>)> {
            asked_in(request, &known())
                .into_iter()
                .map(|asked| (asked.way, asked.about))
                .collect()
        };
        assert_eq!(
            about("Use Maker-checker for the limiter."),
            [(
                AskedWay::Exactly(workflow("maker-checker")),
                vec!["limiter".to_owned()]
            )]
        );
        assert_eq!(
            about("Have 3 agents split the route tests."),
            [(
                AskedWay::Exactly(Way::Agents { count: 3 }),
                vec!["route".to_owned(), "tests".to_owned()]
            )]
        );
        assert_eq!(
            about("Do the limiter in a session of its own."),
            [(
                AskedWay::Exactly(Way::OwnSession),
                vec!["limiter".to_owned()]
            )]
        );
    }

    #[test]
    fn why_says_who_chose_each_way() {
        let offer = Offer::new(
            Setup::Council,
            &Ways {
                workflows: argonauts_core::settings::ways::WorkflowsAllowed::These(vec![
                    "maker-checker".to_owned(),
                ]),
                ..Ways::default()
            },
            &known(),
            1,
        );
        let mut tasks = plan();
        let set = |task: &mut TaskSpec, how: Way, asked: bool| {
            task.way = Some(TaskWay {
                how,
                asked_by_you: asked,
                steps: None,
                why: None,
            });
        };
        set(&mut tasks[0], workflow("maker-checker"), true);
        set(&mut tasks[1], workflow("best-of-n"), true);
        set(&mut tasks[2], Way::OwnSession, false);
        set(&mut tasks[3], Way::Agents { count: 3 }, true);
        assert_eq!(why(&tasks[0], &offer, None), "You asked");
        assert_eq!(
            why(&tasks[1], &offer, None),
            "You asked · off in your setup"
        );
        assert_eq!(why(&tasks[2], &offer, None), "Small, one file");
        assert_eq!(why(&tasks[3], &offer, None), "You asked · 3 files");
        set(&mut tasks[2], workflow("maker-checker"), true);
        assert_eq!(
            why(&tasks[2], &offer, Some(&Way::OwnSession)),
            "You changed it · the note keeper had Own session"
        );
        set(&mut tasks[2], Way::Agents { count: 2 }, false);
        assert_eq!(why(&tasks[2], &offer, None), "The note keeper's pick");
        assert_eq!(
            yours_stands("every request reads this table, so Maker-checker."),
            "every request reads this table, so Maker-checker · yours stands"
        );
    }
}
