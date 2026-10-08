//! How big a workflow may be. A request can say it ("use a small workflow, 5
//! agents max"), and otherwise `workflow_size` does. It is a guideline, as in Claude Code: the
//! orchestrator keeps to it, a request can ask for another size, and a hard stop is a limit
//! you set. A workflow file that fixes its own steps keeps its own counts.

use argonauts_core::settings::workflows::WorkflowSize;

/// The size a request asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Asked {
    pub scale: Option<WorkflowSize>,
    pub max_agents: Option<u32>,
}

impl Asked {
    /// As the workflow's row says it, such as "a small workflow, 5 agents at most".
    pub fn said(&self) -> String {
        let scale = self.scale.map(|scale| match scale {
            WorkflowSize::None => "no size guideline".to_owned(),
            scale => format!("a {} workflow", scale.as_str()),
        });
        let agents = self.max_agents.map(|max| match max {
            1 => "1 agent at most".to_owned(),
            max => format!("{max} agents at most"),
        });
        [scale, agents]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// What the request says about the workflow's size, if anything.
pub fn asked(request: &str) -> Option<Asked> {
    let words = words(request);
    let asked = Asked {
        scale: scale(&words),
        max_agents: max_agents(&words),
    };
    (asked != Asked::default()).then_some(asked)
}

/// The orchestrator's line about size: the request's own words when it said any, else the
/// default.
pub fn guideline(default: WorkflowSize, asked: Option<Asked>) -> String {
    let Some(asked) = asked else {
        return format!(
            "{} This is a guideline, and a request that asks for another size gets it.",
            limit(default)
        );
    };
    let mut lines = vec![format!(
        "The request asks for {}. Keep to it: the user chose this size.",
        asked.said()
    )];
    if let Some(max) = asked.max_agents {
        lines.push(format!("Use at most {max} agents in all."));
    } else if let Some(scale) = asked.scale {
        lines.push(limit(scale).to_owned());
    }
    lines.join(" ")
}

fn limit(size: WorkflowSize) -> &'static str {
    match size {
        WorkflowSize::Small => "Keep the workflow small: under 5 agents in all.",
        WorkflowSize::Medium => "Keep the workflow under 10 agents in all.",
        WorkflowSize::Large => "The workflow may be large: up to 50 agents in all.",
        WorkflowSize::None => "Size the workflow from the request alone.",
    }
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

/// "small workflow", "a large dynamic workflow", "no size guideline".
fn scale(words: &[String]) -> Option<WorkflowSize> {
    for (at, word) in words.iter().enumerate() {
        let size = match word.as_str() {
            "small" | "smaller" | "tiny" => WorkflowSize::Small,
            "medium" | "mid-sized" | "medium-sized" => WorkflowSize::Medium,
            "large" | "big" | "larger" | "bigger" => WorkflowSize::Large,
            _ => continue,
        };
        let names_a_workflow = words
            .iter()
            .skip(at + 1)
            .take(2)
            .any(|next| next == "workflow" || next == "workflows");
        if names_a_workflow {
            return Some(size);
        }
    }
    let unbounded = words.windows(3).any(|three| {
        three[0] == "no" && three[1] == "size" && matches!(three[2].as_str(), "guideline" | "limit")
    });
    unbounded.then_some(WorkflowSize::None)
}

/// "5 agents max", "at most 5 agents", "no more than five agents", "up to 5 agents", "under
/// 5 agents", "max 5 agents", "5 agents or fewer".
fn max_agents(words: &[String]) -> Option<u32> {
    for (at, word) in words.iter().enumerate() {
        let Some(count) = number(word) else {
            continue;
        };
        if !words
            .get(at + 1)
            .is_some_and(|next| next == "agents" || next == "agent")
        {
            continue;
        }
        let before = |back: usize| at.checked_sub(back).and_then(|i| words.get(i));
        let after = |ahead: usize| words.get(at + ahead);
        let is =
            |word: Option<&String>, any: &[&str]| word.is_some_and(|w| any.contains(&w.as_str()));
        let below = is(before(1), &["under", "below"])
            || (is(before(2), &["fewer", "less"]) && is(before(1), &["than"]));
        if below {
            return Some(count.saturating_sub(1).max(1));
        }
        let capped_before = is(before(1), &["max", "maximum", "most", "to"])
            || (is(before(1), &["than"]) && is(before(2), &["more"]))
            || (is(before(1), &["of"]) && is(before(2), &["maximum", "max"]));
        let capped_after = is(after(2), &["max", "maximum", "tops", "most"])
            || (is(after(2), &["or"]) && is(after(3), &["fewer", "less"]))
            || (is(after(2), &["at"]) && is(after(3), &["most"]));
        if capped_before || capped_after {
            return Some(count);
        }
    }
    None
}

fn number(word: &str) -> Option<u32> {
    const WORDS: [&str; 20] = [
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
        "twenty",
    ];
    word.parse::<u32>()
        .ok()
        .filter(|count| *count > 0)
        .or_else(|| {
            WORDS
                .iter()
                .position(|known| *known == word)
                .map(|at| at as u32 + 1)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(scale: Option<WorkflowSize>, max_agents: Option<u32>) -> Option<Asked> {
        Some(Asked { scale, max_agents })
    }

    #[test]
    fn a_request_says_its_size_in_its_own_words() {
        let small = Some(WorkflowSize::Small);
        assert_eq!(
            asked("clean up the date helpers, use a small workflow, 5 agents max"),
            size(small, Some(5))
        );
        assert_eq!(
            asked("Use a LARGE workflow for this"),
            size(Some(WorkflowSize::Large), None)
        );
        assert_eq!(asked("at most 3 agents please"), size(None, Some(3)));
        assert_eq!(asked("no more than five agents"), size(None, Some(5)));
        assert_eq!(asked("up to 8 agents"), size(None, Some(8)));
        assert_eq!(asked("keep it under 5 agents"), size(None, Some(4)));
        assert_eq!(asked("4 agents or fewer"), size(None, Some(4)));
        assert_eq!(asked("6 agents at most"), size(None, Some(6)));
        assert_eq!(
            asked("no size guideline this time"),
            size(Some(WorkflowSize::None), None)
        );
    }

    #[test]
    fn words_about_something_else_say_nothing() {
        for request in [
            "add due dates to tasks",
            "the small helper in workflow.rs is wrong",
            "5 agents reviewed it last time",
            "fix the small bug",
        ] {
            assert_eq!(asked(request), None, "{request}");
        }
    }

    #[test]
    fn the_row_and_the_orchestrator_read_it_plainly() {
        let asked = Asked {
            scale: Some(WorkflowSize::Small),
            max_agents: Some(5),
        };
        assert_eq!(asked.said(), "a small workflow, 5 agents at most");
        assert_eq!(
            guideline(WorkflowSize::Medium, Some(asked)),
            "The request asks for a small workflow, 5 agents at most. Keep to it: the user \
             chose this size. Use at most 5 agents in all."
        );
        assert_eq!(
            guideline(WorkflowSize::Medium, None),
            "Keep the workflow under 10 agents in all. This is a guideline, and a request that \
             asks for another size gets it."
        );
    }
}
