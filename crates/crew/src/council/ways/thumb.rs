//! The note keeper's rule of thumb for each task's way, with its short reason. Code applies the
//! same rule to a task the plan gave no way, so every task is done some way the setup allows.

use argonauts_core::events::task::{TaskSpec, TaskWay, Way};
use argonauts_core::workflow::WorkflowRef;

use super::{MAKER_CHECKER, Offer, done_by};
use crate::divide::paths;

/// What the note keeper is told to pick each way by.
pub const RULE_OF_THUMB: &str = "Pick each task's way by this rule of thumb, and give its reason \
in a few words. Small or one file: its own session. Risky, like code every request goes \
through, money, logins or a data migration: Maker-checker. Parts that share no files: one agent \
each, up to 4. A way the user named for a task binds: keep it, and if you disagree, object with \
your reason.";

/// The way the rule of thumb gives a task, and why in a few words: "Small, one file".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposed {
    pub way: Way,
    pub why: String,
}

/// What the task touches that makes it risky, in the words its reason gives.
const RISKS: &[(&str, &[&[&str]])] = &[
    (
        "every request",
        &[
            &["every", "request"],
            &["each", "request"],
            &["all", "requests"],
            &["middleware"],
        ],
    ),
    (
        "money",
        &[
            &["money"],
            &["payment"],
            &["payments"],
            &["billing"],
            &["invoice"],
            &["invoices"],
            &["checkout"],
            &["refund"],
            &["refunds"],
            &["pricing"],
        ],
    ),
    (
        "logins",
        &[
            &["login"],
            &["logins"],
            &["log", "in"],
            &["sign", "in"],
            &["auth"],
            &["authentication"],
            &["password"],
            &["passwords"],
            &["oauth"],
        ],
    ),
    (
        "a data migration",
        &[
            &["migration"],
            &["migrations"],
            &["migrate"],
            &["backfill"],
            &["schema", "change"],
        ],
    ),
];

/// The rule of thumb for `task`, from the ways `offer` allows.
pub fn propose(task: &TaskSpec, offer: &Offer) -> Proposed {
    let maker_checker = Way::Workflow {
        name: WorkflowRef::new(MAKER_CHECKER),
    };
    if let Some(risk) = risk_of(task)
        && offer.allows(&maker_checker)
    {
        return Proposed {
            way: maker_checker,
            why: format!("Risky: {risk}"),
        };
    }
    let parts = parts(&task.owned_paths, offer.most_agents());
    if parts.len() >= 2 {
        let count = parts.len() as u8;
        let what = if parts.iter().flatten().all(|path| is_file(path)) {
            "files"
        } else {
            "parts"
        };
        return Proposed {
            way: Way::Agents { count },
            why: format!("{count} {what}, none shared"),
        };
    }
    if offer.allows(&Way::OwnSession) {
        return Proposed {
            way: Way::OwnSession,
            why: small_why(&task.owned_paths).to_owned(),
        };
    }
    match offer.ways().first() {
        Some(first) => Proposed {
            way: first.clone(),
            why: "Own session is off".to_owned(),
        },
        None => Proposed {
            way: Way::OwnSession,
            why: "Your setup offers no way".to_owned(),
        },
    }
}

/// Gives each task the plan left without a way the rule of thumb's, and moves a proposed way
/// the setup turned off onto the rule of thumb's. A way you asked for stays, even one your
/// setup turned off. Returns a line for each way code moved.
pub fn settle(tasks: &mut [TaskSpec], offer: &Offer) -> Vec<String> {
    let mut moved = Vec::new();
    for task in tasks.iter_mut() {
        let before = match &task.way {
            Some(way) if way.asked_by_you => continue,
            Some(way) if offer.allows(&way.how) && agents_misfit(task).is_none() => continue,
            Some(way) => Some(way.how.clone()),
            None => None,
        };
        let proposed = propose(task, offer);
        if let Some(before) = before {
            let refusal = offer
                .refusal(&before)
                .or_else(|| agents_misfit(task))
                .unwrap_or_default();
            moved.push(format!(
                "{} is done {}. {refusal}",
                task.title,
                done_by(&proposed.way)
            ));
        }
        task.way = Some(TaskWay {
            how: proposed.way,
            asked_by_you: false,
            steps: None,
        });
    }
    moved
}

/// The task's paths in groups that share no files, one group per agent, up to `most`. Empty
/// when they can't be split: fewer than two paths, two that may name the same file, or agents
/// off (`most` under 2).
pub fn parts(owned: &[String], most: u8) -> Vec<Vec<String>> {
    let owned: Vec<&String> = owned
        .iter()
        .filter(|path| !path.trim().is_empty())
        .collect();
    let shared = owned
        .iter()
        .enumerate()
        .any(|(at, a)| owned[at + 1..].iter().any(|b| paths::overlap(a, b)));
    if owned.len() < 2 || most < 2 || shared {
        return Vec::new();
    }
    let count = owned.len().min(usize::from(most));
    let mut groups = vec![Vec::new(); count];
    for (at, path) in owned.into_iter().enumerate() {
        groups[at % count].push(path.clone());
    }
    groups
}

/// Why a task done by agents can't be split one part per agent, or `None` when it can.
pub fn agents_misfit(task: &TaskSpec) -> Option<String> {
    let Some(Way::Agents { count }) = task.way.as_ref().map(|way| &way.how) else {
        return None;
    };
    let split = parts(&task.owned_paths, *count).len();
    if split == usize::from(*count) {
        return None;
    }
    Some(if split < 2 {
        format!(
            "{} can't take {count} agents: its paths don't split into parts that share no files.",
            task.title
        )
    } else {
        format!(
            "{} can't take {count} agents: its paths make {split} parts that share no files.",
            task.title
        )
    })
}

fn small_why(owned: &[String]) -> &'static str {
    match owned {
        [] => "Small",
        [one] if is_file(one) => "Small, one file",
        [_] => "One folder",
        _ => "Its files overlap",
    }
}

/// A path that names one file: no glob, and an extension on its last part.
pub(super) fn is_file(path: &str) -> bool {
    let last = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path);
    !path.contains(['*', '?', '[', '{']) && last.contains('.') && !last.starts_with('.')
}

fn risk_of(task: &TaskSpec) -> Option<&'static str> {
    let text = format!(
        "{} {} {}",
        task.title,
        task.task,
        task.owned_paths.join(" ")
    )
    .to_lowercase();
    let tokens: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    RISKS.iter().find_map(|(risk, phrases)| {
        phrases
            .iter()
            .any(|phrase| tokens.windows(phrase.len()).any(|run| run == *phrase))
            .then_some(*risk)
    })
}

#[cfg(test)]
mod tests {
    use argonauts_core::TaskId;
    use argonauts_core::events::task::{TaskKind, TaskLimit};
    use argonauts_core::settings::ways::{MostAgents, Ways, WorkflowsAllowed};

    use super::*;
    use crate::council::ways::{Setup, task_workflows, words};

    fn task(title: &str, owned: &[&str]) -> TaskSpec {
        TaskSpec {
            id: TaskId::numbered(1),
            title: title.to_owned(),
            task: format!("{title}."),
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

    fn offer(ways: &Ways) -> Offer {
        Offer::new(Setup::Council, ways, &task_workflows([]), 1)
    }

    fn proposed(title: &str, owned: &[&str]) -> (String, String) {
        let Proposed { way, why } = propose(&task(title, owned), &offer(&Ways::default()));
        (words(&way), why)
    }

    fn pair(way: &str, why: &str) -> (String, String) {
        (way.to_owned(), why.to_owned())
    }

    #[test]
    fn small_work_stays_in_its_own_session() {
        assert_eq!(
            proposed("Memory store", &["src/store/memory.ts"]),
            pair("Own session", "Small, one file")
        );
        assert_eq!(proposed("Tidy the docs", &[]), pair("Own session", "Small"));
        assert_eq!(
            proposed("Store", &["src/store/**"]),
            pair("Own session", "One folder")
        );
        assert_eq!(
            proposed("Store", &["src/store/**", "src/store/redis.ts"]),
            pair("Own session", "Its files overlap")
        );
    }

    #[test]
    fn risky_work_goes_to_maker_checker() {
        assert_eq!(
            proposed("Rate limit middleware", &["src/limit.ts"]),
            pair("Maker-checker", "Risky: every request")
        );
        assert_eq!(
            proposed("Refunds", &["src/a.ts", "src/b.ts"]),
            pair("Maker-checker", "Risky: money")
        );
        assert_eq!(
            proposed("Session timeout", &["src/auth/session.ts"]),
            pair("Maker-checker", "Risky: logins")
        );
        assert_eq!(
            proposed("Plan limits", &["db/migrations/0042_plan_limits.sql"]),
            pair("Maker-checker", "Risky: a data migration")
        );
        // A word inside another word isn't the word.
        assert_eq!(
            proposed("Author list", &["src/authors.ts"]),
            pair("Own session", "Small, one file")
        );
    }

    #[test]
    fn parts_that_share_no_files_get_one_agent_each_up_to_four() {
        assert_eq!(
            proposed(
                "Route tests",
                &[
                    "test/routes/orders.test.ts",
                    "test/routes/users.test.ts",
                    "test/routes/import.test.ts"
                ]
            ),
            pair("3 agents", "3 files, none shared")
        );
        assert_eq!(
            proposed("Clients", &["web/**", "cli/**"]),
            pair("2 agents", "2 parts, none shared")
        );
        let six: Vec<String> = (1..=6).map(|n| format!("src/p{n}.ts")).collect();
        let groups = parts(&six, 4);
        assert_eq!(groups.len(), 4);
        assert_eq!(groups[0], ["src/p1.ts", "src/p5.ts"]);
        assert_eq!(groups[3], ["src/p4.ts"]);
        assert!(parts(&six, 0).is_empty(), "agents off");
    }

    #[test]
    fn agents_need_as_many_parts_as_they_are() {
        let mut routes = task("Route tests", &["test/a.test.ts", "test/b.test.ts"]);
        routes.way = Some(TaskWay {
            how: Way::Agents { count: 2 },
            asked_by_you: true,
            steps: None,
        });
        assert_eq!(agents_misfit(&routes), None);
        routes.way = Some(TaskWay {
            how: Way::Agents { count: 3 },
            asked_by_you: true,
            steps: None,
        });
        assert_eq!(
            agents_misfit(&routes).as_deref(),
            Some("Route tests can't take 3 agents: its paths make 2 parts that share no files.")
        );
        routes.owned_paths = vec!["test/**".to_owned()];
        assert!(agents_misfit(&routes).unwrap().contains("don't split"));
    }

    #[test]
    fn a_way_the_setup_turned_off_falls_to_the_next_rule() {
        let ways = Ways {
            own_session: true,
            workflows: WorkflowsAllowed::These(Vec::new()),
            agents: MostAgents::try_from(2).unwrap(),
        };
        let Proposed { way, why } = propose(
            &task("Refunds", &["src/a.ts", "src/b.ts", "src/c.ts"]),
            &offer(&ways),
        );
        assert_eq!(
            (words(&way), why.as_str()),
            ("2 agents".to_owned(), "2 files, none shared")
        );
        let no_own = Ways {
            own_session: false,
            ..ways
        };
        let Proposed { way, why } = propose(&task("Store", &["src/a.ts"]), &offer(&no_own));
        assert_eq!(
            (way, why.as_str()),
            (Way::Agents { count: 2 }, "Own session is off")
        );
    }

    #[test]
    fn settling_fills_what_the_plan_left_and_keeps_what_you_asked() {
        let ways = Ways {
            own_session: true,
            workflows: WorkflowsAllowed::These(vec!["maker-checker".to_owned()]),
            agents: MostAgents::MOST,
        };
        let best_of_n = Way::Workflow {
            name: WorkflowRef::new("best-of-n"),
        };
        let mut tasks = vec![
            task("Memory store", &["src/store/memory.ts"]),
            task("Redis bucket", &["src/store/redis.ts"]),
            task("Import", &["src/import.ts"]),
        ];
        tasks[1].way = Some(TaskWay {
            how: best_of_n.clone(),
            asked_by_you: true,
            steps: None,
        });
        tasks[2].way = Some(TaskWay {
            how: best_of_n.clone(),
            asked_by_you: false,
            steps: None,
        });
        let moved = settle(&mut tasks, &offer(&ways));
        assert_eq!(
            tasks[0].way.as_ref().map(|way| &way.how),
            Some(&Way::OwnSession)
        );
        assert_eq!(tasks[1].way.as_ref().map(|way| &way.how), Some(&best_of_n));
        assert_eq!(
            tasks[2].way.as_ref().map(|way| &way.how),
            Some(&Way::OwnSession)
        );
        assert_eq!(
            moved,
            [
                "Import is done in its own session. Best-of-n is off in this council's setup. \
                 Set up Council changes it."
            ]
        );
    }
}
