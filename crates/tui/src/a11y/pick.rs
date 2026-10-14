//! A typed line read as an answer to the question that waits: an option's number alone, with a
//! note only after a colon (`1: the note`), and for Request changes the tasks it sends back
//! before the colon (`3 task-002 task-004: the note`). Anything else is a message, so
//! `1 more thing` or `y` never answers a question.

use argonauts_core::TaskId;
use argonauts_core::events::approval::AnswerAction;

use super::questions::QuestionList;
use super::words::spoken;

#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    Option {
        index: usize,
        note: Option<String>,
        /// For Request changes, the tasks named. Empty means every task.
        tasks: Vec<TaskId>,
    },
    /// Type my own answer. `None` until the answer is typed.
    OwnAnswer { text: Option<String> },
    /// Show the diff, details, file, or evidence: read out here and never sent.
    View { index: usize },
    /// Switches the fallback question's Remember this pick.
    Remember,
}

/// What a typed line comes to while a question waits.
#[derive(Debug, Clone, PartialEq)]
pub enum Typed {
    /// Not an answer, so it goes on as a message.
    Message,
    Pick(Pick),
    /// Set apart as an answer but not one this question takes. Nothing is sent.
    Warning(String),
}

/// Where a number points.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Item {
    Option(usize),
    Remember,
    OwnAnswer,
}

impl QuestionList {
    pub fn parse(&self, typed: &str) -> Typed {
        let typed = typed.trim();
        let (head, note, set_apart) = match typed.split_once(':') {
            Some((head, note)) => {
                let note = Some(note.trim()).filter(|note| !note.is_empty());
                (head, note.map(str::to_owned), true)
            }
            None => (typed, None, false),
        };
        let mut words = head.split_whitespace();
        let Some(first) = words.next() else {
            return Typed::Message;
        };
        let names: Vec<&str> = words.collect();
        let Some(item) = self.item(first.strip_suffix(['.', ')']).unwrap_or(first)) else {
            return Typed::Message;
        };
        let names_tasks = self
            .request_changes()
            .is_some_and(|at| item == Item::Option(at));
        let tasks = if names.is_empty() {
            Vec::new()
        } else if !names_tasks {
            return Typed::Message;
        } else {
            match self.named_tasks(&names) {
                Ok(tasks) => tasks,
                Err(warning) if set_apart => return Typed::Warning(warning),
                Err(_) => return Typed::Message,
            }
        };
        match item {
            Item::OwnAnswer => Typed::Pick(Pick::OwnAnswer { text: note }),
            Item::Remember if note.is_some() => {
                self.no_note(self.remember_number().unwrap_or_default().to_string())
            }
            Item::Remember => Typed::Pick(Pick::Remember),
            Item::Option(index) => self.option(index, note, tasks),
        }
    }

    fn option(&self, index: usize, note: Option<String>, tasks: Vec<TaskId>) -> Typed {
        let Some(option) = self.options.get(index) else {
            return Typed::Message;
        };
        match option.action {
            AnswerAction::Answer => Typed::Pick(Pick::OwnAnswer { text: note }),
            AnswerAction::ShowDiff if note.is_none() => Typed::Pick(Pick::View { index }),
            AnswerAction::ShowDiff => self.no_note((index + 1).to_string()),
            _ if note.is_some() && !self.takes_note() => self.no_note((index + 1).to_string()),
            _ => Typed::Pick(Pick::Option { index, note, tasks }),
        }
    }

    fn no_note(&self, alone: String) -> Typed {
        Typed::Warning(format!(
            "that takes no note, so nothing was sent. Type {alone} alone to pick it."
        ))
    }

    fn item(&self, word: &str) -> Option<Item> {
        let number = word.parse::<usize>().ok()?;
        if Some(number) == self.own_answer_number() {
            Some(Item::OwnAnswer)
        } else if Some(number) == self.remember_number() {
            Some(Item::Remember)
        } else if (1..=self.options.len()).contains(&number) {
            Some(Item::Option(number - 1))
        } else {
            None
        }
    }

    /// The merge's tasks `names` picks for Request changes.
    fn named_tasks(&self, names: &[&str]) -> Result<Vec<TaskId>, String> {
        names
            .iter()
            .map(|name| {
                let name = name.trim_end_matches(',');
                self.checklist
                    .iter()
                    .find(|task| task.to_string().eq_ignore_ascii_case(name))
                    .cloned()
                    .ok_or_else(|| {
                        let known: Vec<String> =
                            self.checklist.iter().map(ToString::to_string).collect();
                        format!(
                            "{} isn't one of this merge's tasks ({}), so nothing was sent.",
                            spoken(name),
                            known.join(", ")
                        )
                    })
            })
            .collect()
    }
}
