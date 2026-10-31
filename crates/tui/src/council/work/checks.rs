//! The lines the council's work writes in the conversation: the plan you approved, and what
//! each check of a task found. A check is a fresh session that never reads the talk or learns
//! who made the change, and its line says so; where only the maker of the change is set up, it
//! says that the check may favour its own style.

use argonauts_core::base::names::SeatId;
use argonauts_core::runs::events::council::CheckRung;
use argonauts_core::workflows::steps::judgment::ReviewVerdict;

use super::super::{Face, Item, Piece, Tone};

/// Who checks a task: a fresh session of a seat's model, named by that seat, or of a model that
/// sits nowhere, or more than once, by its own face.
#[derive(Debug, Clone, PartialEq)]
pub enum Checker {
    Seat(SeatId),
    Model(Face),
}

/// The plan you approved, as its line sums it up.
#[derive(Debug, Clone, PartialEq)]
pub struct Approved {
    /// When you approved it, on the wall clock: "10:51 AM".
    pub at: String,
    pub tasks: usize,
    /// The model whose fresh sessions check every task, by its whole name, and whether it is
    /// the maker of every task's work too; `None` while the checks are different from task to task.
    pub checker: Option<(String, bool)>,
    /// The tasks whose way you asked for stay as you asked, though a seat objected.
    pub kept: Vec<String>,
}

/// "10:51 AM · You approved the plan · 3 tasks · Gemini 3.8 Flash checks each one", or what
/// stays as you asked where a seat objected to it.
pub fn approved(plan: &Approved) -> Item {
    let mut text = format!("{} · You approved the plan · ", plan.at);
    if plan.kept.is_empty() {
        text.push_str(&counted(plan.tasks, "task"));
        match &plan.checker {
            Some((name, true)) => {
                text.push_str(&format!(" · a fresh {name} session checks each one"));
            }
            Some((name, false)) => text.push_str(&format!(" · {name} checks each one")),
            None => {}
        }
    } else {
        let stays = if plan.kept.len() == 1 {
            "stays"
        } else {
            "stay"
        };
        text.push_str(&format!("{} {stays} as you asked", listed(&plan.kept)));
    }
    Item::Line(vec![Piece::dim(text)])
}

/// What a check of a task found.
#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    pub checker: Checker,
    pub task: String,
    pub rung: CheckRung,
    pub verdict: ReviewVerdict,
    /// The whole name of the model that made the change, which a check of that very maker
    /// names.
    pub maker: String,
    /// The council's first check, whose lines say what a check reads.
    pub first: bool,
}

/// The lines of one check: its mark, who checked the task without knowing who made it, and
/// what it found; then, where the check is of the change's own maker, that it may favour its
/// own style, or else, after the council's first check, what a check reads. None for a task
/// you check yourself, which your own question shows.
pub fn checked(check: &Checked) -> Vec<Item> {
    if check.rung == CheckRung::You {
        return Vec::new();
    }
    let (mark, tone) = match check.verdict {
        ReviewVerdict::Approve => ("✔  ", Tone::Ok),
        ReviewVerdict::RequestChanges | ReviewVerdict::Reject => ("✘  ", Tone::Warn),
    };
    let found = match check.verdict {
        ReviewVerdict::Approve => "accepted it",
        ReviewVerdict::RequestChanges => "sent it back",
        ReviewVerdict::Reject => "turned it down",
    };
    let what = format!(
        " checked {} without knowing who made it, and {found}",
        check.task
    );
    let mut line = vec![Piece::tone(mark, tone)];
    match (&check.checker, check.rung) {
        (Checker::Model(face), CheckRung::SameMaker) => {
            line.push(Piece::Logo(face.logo.clone()));
            line.push(Piece::dim(format!(" A fresh {} session{what}", face.name)));
        }
        (Checker::Model(face), _) => {
            line.push(Piece::Model(face.clone(), Tone::Dim));
            line.push(Piece::dim(what));
        }
        (Checker::Seat(seat), _) => {
            line.push(Piece::Called(seat.clone(), Tone::Dim));
            line.push(Piece::dim(what));
        }
    }
    let mut lines = vec![Item::Line(line)];
    if check.rung == CheckRung::SameMaker {
        lines.push(Item::Line(vec![
            Piece::tone("Same maker", Tone::Warn),
            Piece::dim(format!(
                ": {} made it too, as no other maker is set up · it may favour its own style",
                check.maker
            )),
        ]));
    } else if check.first {
        lines.push(Item::Line(vec![Piece::dim(
            "It read the plan, the task, the change and the test output · not the talk, not \
             who made it",
        )]));
    }
    lines
}

/// "1 task", "3 tasks".
fn counted(count: usize, what: &str) -> String {
    match count {
        1 => format!("1 {what}"),
        n => format!("{n} {what}s"),
    }
}

/// "Plan limits", "Plan limits and Route tests", "A, B and C".
fn listed(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::look::logos::Logo;

    use super::*;

    fn plan(checker: Option<(&str, bool)>, kept: &[&str]) -> String {
        let item = approved(&Approved {
            at: "10:51 AM".to_owned(),
            tasks: 3,
            checker: checker.map(|(name, same)| (name.to_owned(), same)),
            kept: kept.iter().map(|&task| task.to_owned()).collect(),
        });
        let Item::Line(pieces) = item else {
            panic!("a line");
        };
        pieces
            .iter()
            .map(|piece| match piece {
                Piece::Words(words, _) => words.clone(),
                _ => String::new(),
            })
            .collect()
    }

    #[test]
    fn the_approved_plan_counts_its_tasks_and_names_who_checks_them() {
        assert_eq!(
            plan(Some(("Gemini 3.8 Flash", false)), &[]),
            "10:51 AM · You approved the plan · 3 tasks · Gemini 3.8 Flash checks each one"
        );
        assert_eq!(
            plan(Some(("Opus 5.5", true)), &[]),
            "10:51 AM · You approved the plan · 3 tasks · a fresh Opus 5.5 session checks each one"
        );
        assert_eq!(
            plan(None, &[]),
            "10:51 AM · You approved the plan · 3 tasks"
        );
        assert_eq!(
            plan(Some(("Gemini 3.8 Flash", false)), &["Plan limits"]),
            "10:51 AM · You approved the plan · Plan limits stays as you asked"
        );
        assert_eq!(
            plan(None, &["Plan limits", "Route tests"]),
            "10:51 AM · You approved the plan · Plan limits and Route tests stay as you asked"
        );
    }

    fn opus() -> Face {
        Face::new("Opus 5.5", "claude-code", Logo::dot("claude-code"))
    }

    fn check(checker: Checker, rung: CheckRung, verdict: ReviewVerdict, first: bool) -> Checked {
        Checked {
            checker,
            task: "Redis bucket".to_owned(),
            rung,
            verdict,
            maker: "Opus 5.5".to_owned(),
            first,
        }
    }

    fn words(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|item| {
                let Item::Line(pieces) = item else {
                    panic!("a line");
                };
                pieces
                    .iter()
                    .map(|piece| match piece {
                        Piece::Words(words, _) => words.clone(),
                        Piece::Called(..) | Piece::Model(..) => "@".to_owned(),
                        _ => "▐▌".to_owned(),
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_check_says_it_never_knew_who_made_the_change_and_what_it_found() {
        let seat = Checker::Seat(SeatId::numbered(3));
        let first = check(
            seat.clone(),
            CheckRung::OtherMaker,
            ReviewVerdict::Approve,
            true,
        );
        assert_eq!(
            words(&checked(&first)),
            [
                "✔  @ checked Redis bucket without knowing who made it, and accepted it",
                "It read the plan, the task, the change and the test output · not the talk, not \
                 who made it",
            ]
        );
        let later = check(
            seat,
            CheckRung::OtherMaker,
            ReviewVerdict::RequestChanges,
            false,
        );
        assert_eq!(
            words(&checked(&later)),
            ["✘  @ checked Redis bucket without knowing who made it, and sent it back"]
        );
        let named = check(
            Checker::Model(opus()),
            CheckRung::Named,
            ReviewVerdict::Reject,
            false,
        );
        assert_eq!(
            words(&checked(&named)),
            ["✘  @ checked Redis bucket without knowing who made it, and turned it down"]
        );
    }

    #[test]
    fn a_check_of_the_changes_own_maker_says_it_may_favour_its_style_each_time() {
        let same = check(
            Checker::Model(opus()),
            CheckRung::SameMaker,
            ReviewVerdict::Approve,
            false,
        );
        assert_eq!(
            words(&checked(&same)),
            [
                "✔  ▐▌ A fresh Opus 5.5 session checked Redis bucket without knowing who made it, \
                 and accepted it",
                "Same maker: Opus 5.5 made it too, as no other maker is set up · it may favour \
                 its own style",
            ]
        );
        let yours = check(
            Checker::Model(opus()),
            CheckRung::You,
            ReviewVerdict::Approve,
            true,
        );
        assert!(checked(&yours).is_empty());
    }
}
