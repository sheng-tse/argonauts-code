//! The peek Space opens under an agent's row: only what the row can't show, its verdict and the
//! findings it gave, as Claude Code's agent view peeks at an agent. Enter still opens the agent.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::SessionKey;
use argonauts_core::text::sentence_case;
use argonauts_core::views::{AgentRow, AgentView as AgentData, InnerAgent, ReviewGiven, RowStatus};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Padding, Widget};

use crate::text::sanitize_line;
use crate::theme::{Role, Theme};

use super::layout::cut_at_word;
use super::panes;

/// The cells the peek's box stands in from the pane's left edge, under the row's mark.
pub(super) const INDENT: u16 = 2;

/// What the peek's lower edge says.
const KEYS: &str = " space closes · enter opens the agent ";

const ASCII_EDGES: border::Set = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// A peek at one agent of the open phase.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Peek {
    session: SessionKey,
    inner: Option<InnerAgent>,
    /// The review the agent gave, once its view has come.
    review: Option<ReviewGiven>,
}

impl Peek {
    pub(super) fn of(agent: &AgentRow) -> Self {
        Self {
            session: agent.session.clone(),
            inner: agent.inner.clone(),
            review: None,
        }
    }

    pub(super) fn is_of(&self, agent: &AgentRow) -> bool {
        self.session == agent.session && self.inner == agent.inner
    }

    /// Takes the agent's view when it is the peeked agent's, for the findings in it.
    pub(super) fn accept(&mut self, view: &AgentData) -> bool {
        if !self.is_of(&view.row) {
            return false;
        }
        self.review = view.review.clone();
        true
    }

    /// The lines inside the box: the verdict, then each finding numbered.
    pub(super) fn words(&self, agent: &AgentRow, names: &DisplayNames) -> Vec<String> {
        let mut words = vec![sentence_case(&verdict(agent))];
        if let Some(first) = &agent.fallback_for {
            let mut place = format!("In place of {}", sanitize_line(&names.short(first)));
            if let Some(why) = agent.fallback_why.as_deref().map(str::trim)
                && !why.is_empty()
            {
                place.push_str(&format!(" · {}", sanitize_line(why)));
            }
            words.push(place);
        }
        let findings = self.review.iter().flat_map(|review| &review.findings);
        for (at, finding) in findings.enumerate() {
            words.push(format!(
                "{}. {}",
                at + 1,
                sanitize_line(finding.text.trim())
            ));
        }
        words
    }

    /// The rows the peek takes in the pane, its edges among them.
    pub(super) fn height(&self, agent: &AgentRow, names: &DisplayNames) -> usize {
        self.words(agent, names).len() + 2
    }

    /// The box in `area`, its lower edge saying how to close it and open the agent. Rows past
    /// the area's end are left out, as the pane cuts a list.
    pub(super) fn draw(
        &self,
        buf: &mut Buffer,
        area: Rect,
        agent: &AgentRow,
        names: &DisplayNames,
        theme: &Theme,
    ) {
        let edges = if theme.glyphs.is_ascii() {
            ASCII_EDGES
        } else {
            border::ROUNDED
        };
        let room = usize::from(area.width.saturating_sub(4));
        let mut block = Block::bordered()
            .border_set(edges)
            .border_style(theme.style(Role::Faint))
            .padding(Padding::horizontal(1));
        if usize::from(area.width) > KEYS.chars().count() + 4 {
            block = block.title_bottom(
                Line::from(Span::styled(KEYS, theme.style(Role::Dim))).right_aligned(),
            );
        }
        let inner = block.inner(area);
        block.render(area, buf);
        let lines: Vec<Line<'static>> = self
            .words(agent, names)
            .iter()
            .map(|words| Line::from(cut_at_word(words, room)))
            .collect();
        panes::put_lines(buf, inner, &lines);
    }
}

/// What the agent's row would say in words: its note, such as `sent it back · 2 findings`, or
/// how it stands where it has none.
fn verdict(agent: &AgentRow) -> String {
    if let Some(note) = &agent.note {
        let mut said: Vec<String> = std::iter::once(&note.text)
            .chain(&note.then)
            .map(|words| sanitize_line(words.trim()).into_owned())
            .filter(|words| !words.is_empty())
            .collect();
        // A job left out says why in its note, and a job not needed says so in its own.
        if agent.left_out {
            said.insert(0, "left out".to_owned());
        }
        if !said.is_empty() {
            return said.join(" · ");
        }
    }
    let state = match agent.status {
        _ if agent.left_out => "left out",
        _ if agent.not_needed => "not needed",
        RowStatus::Running => "at work, with no verdict yet",
        RowStatus::Waiting | RowStatus::Queued => "not started yet",
        RowStatus::Paused => "held, with its work so far kept",
        RowStatus::NeedsYou => "waiting for your answer",
        RowStatus::Done => "done",
        RowStatus::Failed => "failed",
        RowStatus::Skipped => "skipped",
    };
    state.to_owned()
}

#[cfg(test)]
mod tests {
    use argonauts_core::display_names::DisplayNames;
    use argonauts_core::views::{RowNote, RowStatus, Tone};

    use super::Peek;
    use crate::views::workflow_examples::{self as examples, OPUS, SONNET};

    /// What a peek says where an agent gave no review: how it stands, whose place it took and
    /// why, and why a job was left out.
    #[test]
    fn a_peek_says_how_an_agent_stands_where_it_gave_no_review() {
        let names = DisplayNames::default();
        let running = examples::agent(1, "build:due-flag", SONNET, RowStatus::Running, 9_300, 38);
        assert_eq!(
            Peek::of(&running).words(&running, &names),
            ["At work, with no verdict yet"]
        );

        let mut fallback = running.clone();
        fallback.fallback_for = Some(examples::worker(OPUS));
        fallback.fallback_why = Some("Opus 5.5 is overloaded".to_owned());
        fallback.note = Some(RowNote::new("overloaded", Tone::Warn));
        assert_eq!(
            Peek::of(&fallback).words(&fallback, &names),
            [
                "Overloaded",
                "In place of Opus 5.5 · Opus 5.5 is overloaded"
            ]
        );

        let mut left_out = running;
        left_out.left_out = true;
        left_out.note = Some(RowNote::new("agy is signed out", Tone::Plain));
        assert_eq!(
            Peek::of(&left_out).words(&left_out, &names),
            ["Left out · agy is signed out"]
        );
    }
}
