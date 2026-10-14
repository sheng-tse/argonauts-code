//! A step's earlier tries and rounds, folded under its row in the workflow view: one dim line
//! says what came before, such as `⎿ took over after GPT 5.6 Luna on Codex failed twice` or
//! `⎿ round 1 sent it back`, and → unfolds a line for each.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::WorkerRef;
use argonauts_core::views::{AgentRow, RowStatus, StepTry};
use ratatui::text::{Line, Span};

use crate::bridge::agents;
use crate::text::sanitize_line;
use crate::theme::{Role, Theme};

use super::panes;
use super::states;

/// Under the row's own line, as far in as its label.
const INDENT: &str = "    ";
/// An unfolded try's line, a step further in.
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

/// The tries of the step's newest round in words, such as `took over after GPT 5.6 Luna on
/// Codex failed twice`.
fn this_round(agent: &AgentRow, tries: &[&StepTry], names: &DisplayNames) -> String {
    let failed = |by: Option<&WorkerRef>| {
        tries
            .iter()
            .filter(|tried| tried.status == RowStatus::Failed)
            .filter(|tried| by.is_none_or(|worker| tried.worker == *worker))
            .count()
    };
    // A fallback after failed tries is a takeover too, whether or not a handoff named it.
    let original = agent.standing_in_for.as_ref().or_else(|| {
        tries
            .last()
            .map(|tried| &tried.worker)
            .filter(|worker| **worker != agent.worker)
    });
    match original {
        Some(original) => {
            let who = names.worker(original);
            match failed(Some(original)) {
                0 => format!("took over from {who}"),
                count => format!("took over after {who} failed {}", times(count)),
            }
        }
        None => match failed(None) {
            1 => "tried again after a failed try".to_owned(),
            count => format!("tried again after {count} failed tries"),
        },
    }
}

/// The dim lines under a step's row: what came before, the key that unfolds it on the selected
/// row, and once unfolded, a line per try.
pub(super) fn lines(
    agent: &AgentRow,
    selected: bool,
    unfolded: bool,
    names: &DisplayNames,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let Some(summary) = summary(agent, names) else {
        return Vec::new();
    };
    let dim = theme.style(Role::Dim);
    let only_rounds = agent.tries.iter().all(|tried| tried.round.is_some());
    let hint = match (selected, unfolded) {
        (true, false) if only_rounds => " · → shows the rounds",
        (true, false) => " · → shows the tries",
        (true, true) => " · ← folds them",
        (false, _) => "",
    };
    let mut lines = vec![Line::from(Span::styled(
        format!("{INDENT}{} {summary}{hint}", theme.glyphs.result),
        dim,
    ))];
    if unfolded {
        lines.extend(
            agent
                .tries
                .iter()
                .map(|tried| try_line(tried, names, theme)),
        );
    }
    lines
}

/// `✗ GPT 5.6 Luna on Codex · 2s · Codex refused the answer format Argonauts asked for`, the
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
    let cli = names.harness(&tried.worker.worker);
    if let Some(words) = tried
        .reason
        .as_ref()
        .and_then(|reason| agents::reason_words(reason, &cli))
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
            standing_in_for: None,
            answered: None,
            tries,
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
    fn an_earlier_round_folds_beneath_and_unfolds_to_its_line() {
        let names = DisplayNames::default();
        let theme = Theme::dark();
        let claude = "claude-code/claude-haiku-4-5";
        let row = review(claude, vec![tried(claude, RowStatus::Done, Some(1))]);
        assert_eq!(
            summary(&row, &names).as_deref(),
            Some("round 1 sent it back")
        );
        let folded = text(&lines(&row, true, false, &names, &theme));
        assert_eq!(folded.len(), 1);
        assert!(
            folded[0].ends_with("⎿ round 1 sent it back · → shows the rounds"),
            "{folded:?}"
        );
        let unfolded = text(&lines(&row, true, true, &names, &theme));
        assert_eq!(unfolded.len(), 2);
        assert!(
            unfolded[1].contains(" round 1 · ") && unfolded[1].ends_with(" · 12s · sent it back"),
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
            said.starts_with("round 1 sent it back · took over after "),
            "{said}"
        );
        assert!(said.ends_with(" failed twice"), "{said}");
    }
}
