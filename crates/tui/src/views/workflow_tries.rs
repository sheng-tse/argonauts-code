//! A step's earlier tries and rounds in the workflow view. Its row says what came before in a
//! few words, such as `tried again after a failed try` or `round 1 sent it back`, and →
//! unfolds a line for each under the row.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::ModelRef;
use argonauts_core::views::{AgentRow, RowStatus, StepTry};
use ratatui::text::{Line, Span};

use crate::bridge::agents;
use crate::text::sanitize_line;
use crate::theme::{Role, Theme};

use super::panes;
use super::states;

/// An unfolded try's line sits under its step's row, a step further in than its label.
const TRY_INDENT: &str = "      ";

/// What came before the step's newest try, in words: the rounds that sent the work back, then
/// the tries of this round. `None` for a step tried once.
pub(super) fn summary(agent: &AgentRow, names: &DisplayNames) -> Option<String> {
    let rounds = agent
        .tries
        .iter()
        .filter(|tried| tried.round.is_some())
        .count();
    // The tries since the newest round sent the work back.
    let tries: Vec<&StepTry> = match agent.tries.iter().rposition(|tried| tried.round.is_some()) {
        Some(at) => agent.tries[at + 1..].iter().collect(),
        None => agent.tries.iter().collect(),
    };
    let rounds = match rounds {
        0 => None,
        1 => Some("round 1 sent it back".to_owned()),
        2 => Some("rounds 1 and 2 sent it back".to_owned()),
        count => Some(format!("rounds 1 to {count} sent it back")),
    };
    let tried = (!tries.is_empty()).then(|| this_round(agent, &tries, names));
    match (rounds, tried) {
        (Some(rounds), Some(tried)) => Some(format!("{rounds} · {tried}")),
        (rounds, tried) => rounds.or(tried),
    }
}

/// The tries of the step's newest round in words, such as `fallback after GPT 5.6 Luna on
/// Codex failed twice`.
fn this_round(agent: &AgentRow, tries: &[&StepTry], names: &DisplayNames) -> String {
    let failed = |by: Option<&ModelRef>| {
        tries
            .iter()
            .filter(|tried| tried.status == RowStatus::Failed)
            .filter(|tried| by.is_none_or(|worker| tried.worker == *worker))
            .count()
    };
    // A fallback after failed tries counts as one too, whether or not a fallback named it.
    let original = agent.fallback_for.as_ref().or_else(|| {
        tries
            .last()
            .map(|tried| &tried.worker)
            .filter(|worker| **worker != agent.worker)
    });
    match original {
        Some(original) => {
            // The row names models alone, as every row of the view does.
            let who = names.short(original);
            match failed(Some(original)) {
                0 => format!("fallback for {who}"),
                count => format!("fallback after {who} failed {}", times(count)),
            }
        }
        None => match failed(None) {
            1 => "tried again after a failed try".to_owned(),
            count => format!("tried again after {count} failed tries"),
        },
    }
}

/// A line per earlier try, under a step's row once → unfolds them. The row itself says what
/// came before in its own words.
pub(super) fn lines(agent: &AgentRow, names: &DisplayNames, theme: &Theme) -> Vec<Line<'static>> {
    agent
        .tries
        .iter()
        .map(|tried| try_line(tried, names, theme))
        .collect()
}

/// `✗ GPT 5.6 Luna on Codex · 2s · Codex refused the answer format Argonauts Code asked for`, the
/// reason in the words the agent's own failed row gives it, and `✓ round 1 · Haiku 4.5 on
/// Claude Code · 12s · sent it back` for a round.
fn try_line(tried: &StepTry, names: &DisplayNames, theme: &Theme) -> Line<'static> {
    let dim = theme.style(Role::Dim);
    let (glyph, role) = states::mark(states::of_row(tried.status), theme);
    let round = tried
        .round
        .map(|round| format!(" round {round} ·"))
        .unwrap_or_default();
    let mut text = format!(
        "{round} {} · {}",
        names.worker(&tried.worker),
        panes::elapsed(tried.elapsed_ms)
    );
    if tried.round.is_some() {
        text.push_str(" · sent it back");
    }
    if let Some(words) = tried
        .reason
        .as_ref()
        .and_then(|reason| agents::reason_words(reason, &tried.worker.agent, names))
    {
        text.push_str(" · ");
        text.push_str(&words);
    }
    Line::from(vec![
        Span::raw(TRY_INDENT),
        Span::styled(glyph, theme.style(role)),
        Span::styled(sanitize_line(&text).into_owned(), dim),
    ])
}

fn times(count: usize) -> String {
    match count {
        1 => "once".to_owned(),
        2 => "twice".to_owned(),
        count => format!("{count} times"),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::Spend;
    use argonauts_core::names::SessionKey;

    use super::*;

    fn review(worker: &str, tries: Vec<StepTry>) -> AgentRow {
        AgentRow {
            session: SessionKey::parse("s-009").unwrap(),
            label: "review:per-key · round 2".into(),
            worker: worker.parse().unwrap(),
            status: RowStatus::Running,
            tokens: 1_000,
            spent: Spend::default(),
            elapsed_ms: 8_000,
            subagents: 0,
            context: None,
            fallback_for: None,
            answered: false,
            tries,
            note: None,
            inner: None,
            left_out: false,
            fallback_why: None,
        }
    }

    fn tried(worker: &str, status: RowStatus, round: Option<u32>) -> StepTry {
        StepTry {
            session: SessionKey::parse("s-002").unwrap(),
            worker: worker.parse().unwrap(),
            status,
            elapsed_ms: 12_000,
            reason: None,
            round,
        }
    }

    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn an_earlier_round_is_said_in_words_and_unfolds_to_its_line() {
        let names = DisplayNames::default();
        let theme = Theme::dark();
        let claude = "claude-code/claude-haiku-4-5";
        let row = review(claude, vec![tried(claude, RowStatus::Done, Some(1))]);
        assert_eq!(
            summary(&row, &names).as_deref(),
            Some("round 1 sent it back")
        );
        let unfolded = text(&lines(&row, &names, &theme));
        assert_eq!(unfolded.len(), 1);
        assert!(
            unfolded[0].contains(" round 1 · ") && unfolded[0].ends_with(" · 12s · sent it back"),
            "{unfolded:?}"
        );
    }

    #[test]
    fn a_round_and_the_tries_inside_the_next_say_both() {
        let names = DisplayNames::default();
        let claude = "claude-code/claude-haiku-4-5";
        let codex = "codex/gpt-5.6-luna";
        let row = review(
            claude,
            vec![
                tried(codex, RowStatus::Done, Some(1)),
                tried(codex, RowStatus::Failed, None),
                tried(codex, RowStatus::Failed, None),
            ],
        );
        let said = summary(&row, &names).unwrap();
        assert!(
            said.starts_with("round 1 sent it back · fallback after "),
            "{said}"
        );
        assert!(said.ends_with(" failed twice"), "{said}");
    }
}
