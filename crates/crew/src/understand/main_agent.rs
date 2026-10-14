//! When the main agent answers the crew's questions, it answers the goal's open questions in one
//! read-only session. Each answer becomes an assumption in the goal, which the approval after
//! it checks. A question it can't answer goes to you, never to a placeholder answer: one that
//! turns on a fact no agent can find out, with that fact named on its question, one it left out,
//! and every one when its session fails. Only autopilot, where nothing waits for you, takes
//! the crew's recommended answer for those, as an assumption that says so.

use argonauts_core::NoteLevel;
use argonauts_core::events::approval::ApprovalKind;
use argonauts_core::events::goal::{
    Assumption, Decider, GoalFile, GoalQuestion, MAIN_AGENT_ANSWER,
};
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::steps::understand::{
    AnswerQuestions, AnswerQuestionsInput, MainAgentAnswer, OpenQuestion,
};

use crate::approvals;
use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::prompts;
use crate::workflow::StepCall;

use super::materiality::one_line;
use super::{file, round};

/// What the main agent made of one question.
#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    /// The assumption its answer becomes.
    Answered(Assumption),
    /// It goes to you, for this reason, which its question shows.
    Yours(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MainAgentAnswers {
    pub main_agent: ModelRef,
    /// One per question, in their order.
    pub picks: Vec<Pick>,
}

impl MainAgentAnswers {
    pub fn assumptions(&self) -> Vec<Assumption> {
        self.picks
            .iter()
            .filter_map(|pick| match pick {
                Pick::Answered(assumption) => Some(assumption.clone()),
                Pick::Yours(_) => None,
            })
            .collect()
    }
}

/// The main agent's answers to `questions`, or in autopilot the model you picked in its place. A
/// re-entered step gets the recorded answers back from the host. When the picked model can't
/// answer, the main agent does, after one line that says so; when the main agent's session fails,
/// the questions go to you.
pub async fn answer(
    host: &Host,
    call: &StepCall,
    drafted: &GoalFile,
    questions: &[GoalQuestion],
) -> EngineResult<MainAgentAnswers> {
    answer_in_slot(host, call, drafted, questions, 0).await
}

/// [`answer`] as a later ask of the same step, such as the questions you passed to the main agent
/// after it answered its own. In autopilot the model you picked answers in the main agent's place.
pub async fn answer_in_slot(
    host: &Host,
    call: &StepCall,
    drafted: &GoalFile,
    questions: &[GoalQuestion],
    slot: u32,
) -> EngineResult<MainAgentAnswers> {
    let autopilot = approvals::autopilot::applies_now(host, ApprovalKind::Understand);
    let picked =
        host.read(|state| crate::tie_breaker::answering(state, host.settings(), autopilot));
    let input = AnswerQuestionsInput {
        request: host.read(round::request)?,
        goal: file::render(&as_settled(drafted)),
        questions: questions.iter().map(open_question).collect(),
    };
    let ask = |worker: &ModelRef| Ask {
        slot,
        ..Ask::new(
            Some(call.at.clone()),
            RoleName::TieBreaker,
            worker.clone(),
            prompts::understand::ANSWER,
            input.clone(),
        )
    };
    let mut main_agent = picked.clone();
    let mut answered = host.ask::<AnswerQuestions>(ask(&main_agent)).await;
    let the_main_agent =
        host.read(|state| crate::tie_breaker::answering(state, host.settings(), false));
    if let Err(err) = &answered
        && !matches!(err, EngineError::Interrupted)
        && the_main_agent != picked
    {
        // Said plainly; what the session or the validator said waits for Ctrl+O.
        let line = format!(
            "{} couldn't answer the goal's questions, so the main agent answers them.",
            crate::roles::display_name(&picked)
        );
        round::note_with_detail(host, NoteLevel::Warn, &line, &err.to_string())?;
        main_agent = the_main_agent;
        answered = host.ask::<AnswerQuestions>(ask(&main_agent)).await;
    }
    let output = match answered {
        Ok(answered) => answered.output,
        Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
        Err(err) => {
            let whose = if autopilot {
                "autopilot takes the crew's recommended answers"
            } else {
                "they come to you"
            };
            let line = format!("The main agent couldn't answer the goal's questions, so {whose}.");
            round::note_with_detail(host, NoteLevel::Warn, &line, &err.to_string())?;
            let picks = questions
                .iter()
                .map(|question| {
                    let yours = Pick::Yours("the main agent couldn't answer it".to_owned());
                    in_autopilot(question, yours, autopilot)
                })
                .collect();
            return Ok(MainAgentAnswers { main_agent, picks });
        }
    };
    let picks = questions
        .iter()
        .map(|question| {
            let answer = output.answers.iter().find(|answer| answer.n == question.n);
            in_autopilot(question, pick(question, answer), autopilot)
        })
        .collect();
    Ok(MainAgentAnswers { main_agent, picks })
}

/// In autopilot a question the main agent couldn't answer takes the crew's recommended answer, else
/// its first, as an assumption whose basis says why. Nothing waits for you there.
fn in_autopilot(question: &GoalQuestion, pick: Pick, autopilot: bool) -> Pick {
    let Pick::Yours(why) = &pick else {
        return pick;
    };
    let option = question
        .options
        .iter()
        .find(|option| option.recommended)
        .or_else(|| question.options.first());
    match option {
        Some(option) if autopilot => Pick::Answered(Assumption {
            text: format!(
                "{}: {}",
                one_line(&question.text).trim_end_matches(['?', '.', ':']),
                one_line(&option.label)
            ),
            basis: format!(
                "{MAIN_AGENT_ANSWER}: autopilot took the crew's recommended answer, since {why}"
            ),
            by: Some(Decider::MainAgent),
        }),
        _ => pick,
    }
}

/// The draft as it was before anyone answered, so a re-entered step asks the same thing.
fn as_settled(drafted: &GoalFile) -> GoalFile {
    let mut settled = drafted.clone();
    settled.answers.clear();
    settled
        .assumptions
        .retain(|assumption| !assumption.by_main_agent());
    settled
}

fn open_question(question: &GoalQuestion) -> OpenQuestion {
    let recommended = question
        .options
        .iter()
        .position(|option| option.recommended);
    OpenQuestion {
        n: question.n,
        text: one_line(&question.text),
        options: question
            .options
            .iter()
            .map(|option| one_line(&option.label))
            .collect(),
        recommended: recommended.and_then(|index| u8::try_from(index).ok()),
        basis: recommended
            .and_then(|index| question.options[index].basis.clone())
            .map(|basis| one_line(&basis)),
        tag: question.tag,
    }
}

/// The main agent's pick. A question it left out, or answered with an option that isn't there,
/// goes to you, and so does one that turns on a fact only you have.
pub fn pick(question: &GoalQuestion, answer: Option<&MainAgentAnswer>) -> Pick {
    if let Some(fact) = answer
        .and_then(|answer| answer.needs_user.as_deref())
        .map(one_line)
        .filter(|fact| !fact.is_empty())
    {
        return Pick::Yours(format!("the main agent needs a fact only you have, {fact}"));
    }
    let chosen = answer.and_then(|answer| {
        let option = question.options.get(usize::from(answer.option?))?;
        Some((option, one_line(&answer.why)))
    });
    let Some((option, why)) = chosen else {
        return Pick::Yours("the main agent didn't answer it".to_owned());
    };
    let asked = one_line(&question.text);
    let basis = if why.is_empty() {
        MAIN_AGENT_ANSWER.to_owned()
    } else {
        format!("{MAIN_AGENT_ANSWER}: {why}")
    };
    Pick::Answered(Assumption {
        text: format!(
            "{}: {}",
            asked.trim_end_matches(['?', '.', ':']),
            one_line(&option.label)
        ),
        basis,
        by: Some(Decider::MainAgent),
    })
}

/// The recommended answer as the main agent's, for a question you handed it that it couldn't
/// settle.
pub fn recommended(question: &GoalQuestion) -> Assumption {
    let option = question
        .options
        .iter()
        .find(|option| option.recommended)
        .or_else(|| question.options.first());
    let asked = one_line(&question.text);
    Assumption {
        text: format!(
            "{}: {}",
            asked.trim_end_matches(['?', '.', ':']),
            option.map_or_else(String::new, |option| one_line(&option.label))
        ),
        basis: format!("{MAIN_AGENT_ANSWER}: the recommended answer"),
        by: Some(Decider::MainAgent),
    }
}

/// The goal with the main agent's answers among its assumptions, skipping ones it already holds.
pub fn with_assumptions(goal: &GoalFile, assumptions: &[Assumption]) -> GoalFile {
    let mut assumed = goal.clone();
    for assumption in assumptions {
        if !assumed.assumptions.contains(assumption) {
            assumed.assumptions.push(assumption.clone());
        }
    }
    file::hashed(assumed)
}

/// The goal without the main agent's answers, for when the questions came back to you, such as
/// after Shift+Tab to supervised and a restart. `None` when it holds none.
pub fn without_answers(goal: &GoalFile) -> Option<GoalFile> {
    if !goal.assumptions.iter().any(Assumption::by_main_agent) {
        return None;
    }
    let mut yours = goal.clone();
    yours
        .assumptions
        .retain(|assumption| !assumption.by_main_agent());
    Some(file::hashed(yours))
}

/// The line the transcript shows once the main agent, or the model you `picked` in its place, has
/// answered.
pub fn answered_line(picked: Option<&ModelRef>, answered: usize, yours: usize) -> String {
    let questions = match answered {
        1 => "1 of the goal's questions".to_owned(),
        n => format!("{n} of the goal's questions"),
    };
    let who = picked.map_or_else(|| "The main agent".to_owned(), crate::roles::display_name);
    let mut line = format!(
        "{who} answered {questions} for you. Its answers are assumptions in the goal, which \
         Ctrl+O and /status show"
    );
    match yours {
        0 => {}
        1 => line.push_str(". 1 question is still yours"),
        n => line.push_str(&format!(". {n} questions are still yours")),
    }
    line
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::goal::ChoiceOption;
    use argonauts_core::steps::understand::DiffTag;

    use super::*;

    fn question() -> GoalQuestion {
        let option = |key, label: &str, recommended| ChoiceOption {
            key,
            label: label.to_owned(),
            recommended,
            basis: recommended.then(|| "2 of 3 read it this way".to_owned()),
        };
        GoalQuestion {
            n: 1,
            of: 1,
            text: "Which log level does --verbose turn on?".to_owned(),
            options: vec![option('a', "debug", true), option('b', "trace", false)],
            tag: DiffTag::Scope,
            topic: None,
        }
    }

    fn answer(option: Option<u8>, needs_user: Option<&str>) -> MainAgentAnswer {
        MainAgentAnswer {
            n: 1,
            option,
            why: "the README says trace is for developers".to_owned(),
            needs_user: needs_user.map(str::to_owned),
        }
    }

    #[test]
    fn the_main_agents_pick_becomes_an_assumption_marked_as_its_answer() {
        let Pick::Answered(assumed) = pick(&question(), Some(&answer(Some(1), None))) else {
            panic!("the main agent answered");
        };
        assert_eq!(
            assumed.text,
            "Which log level does --verbose turn on: trace"
        );
        assert_eq!(
            assumed.basis,
            "the main agent's answer: the README says trace is for developers"
        );
        assert!(assumed.by_main_agent());
    }

    /// The recommended answer never passes for the main agent's: what it didn't answer is yours.
    #[test]
    fn a_question_left_out_or_misnumbered_goes_to_you() {
        for answer in [None, Some(answer(Some(7), None)), Some(answer(None, None))] {
            assert_eq!(
                pick(&question(), answer.as_ref()),
                Pick::Yours("the main agent didn't answer it".to_owned())
            );
        }
    }

    #[test]
    fn a_missing_fact_goes_to_you_and_says_which() {
        assert_eq!(
            pick(
                &question(),
                Some(&answer(None, Some("which customers get the flag")))
            ),
            Pick::Yours(
                "the main agent needs a fact only you have, which customers get the flag".into()
            )
        );
        // A blank fact is no fact, so the main agent's pick stands.
        assert!(matches!(
            pick(&question(), Some(&answer(Some(0), Some("  ")))),
            Pick::Answered(_)
        ));
    }

    #[test]
    fn the_transcript_line_says_who_answered_and_what_is_left() {
        assert_eq!(
            answered_line(None, 2, 1),
            "The main agent answered 2 of the goal's questions for you. Its answers are assumptions in \
             the goal, which Ctrl+O and /status show. 1 question is still yours"
        );
        assert!(answered_line(None, 1, 0).ends_with("Ctrl+O and /status show"));
        let picked: ModelRef = "codex/gpt-x".parse().expect("a model");
        assert!(answered_line(Some(&picked), 1, 0).starts_with("GPT X on Codex answered 1 of"));
    }
}
