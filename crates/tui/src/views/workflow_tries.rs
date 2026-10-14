//! A step's earlier tries, folded under its row in the workflow view: one dim line says what
//! came before, such as `⎿ took over after GPT 5.6 Luna on Codex failed twice`, and → unfolds
//! a line for each try.

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

/// What came before the step's newest try, in words. `None` for a step tried once.
pub(super) fn summary(agent: &AgentRow, names: &DisplayNames) -> Option<String> {
    if agent.tries.is_empty() {
        return None;
    }
    let failed = |by: Option<&WorkerRef>| {
        agent
            .tries
            .iter()
            .filter(|tried| tried.status == RowStatus::Failed)
            .filter(|tried| by.is_none_or(|worker| tried.worker == *worker))
            .count()
    };
    // A fallback after failed tries is a takeover too, whether or not a handoff named it.
    let original = agent.standing_in_for.as_ref().or_else(|| {
        agent
            .tries
            .last()
            .map(|tried| &tried.worker)
            .filter(|worker| **worker != agent.worker)
    });
    Some(match original {
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
    })
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
    let hint = match (selected, unfolded) {
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
/// reason in the words the agent's own failed row gives it.
fn try_line(tried: &StepTry, names: &DisplayNames, theme: &Theme) -> Line<'static> {
    let dim = theme.style(Role::Dim);
    let (glyph, role) = states::mark(states::of_row(tried.status), theme);
    let mut text = format!(
        " {} · {}",
        names.worker(&tried.worker),
        panes::elapsed(tried.elapsed_ms)
    );
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
