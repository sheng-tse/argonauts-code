//! A phase's rows as an outline, the one way every workflow inside another is drawn. A workflow
//! inside is a row in the phase where it started, right under the agent whose CLI runs it, `▸`
//! while folded and `▾` while open. Open, its phases are headings drawn like Claude Code's phase
//! rows, with their agents two cells further in; a workflow that reports no phases, as Claude
//! Code's child, lists its agents straight under its row. One label column serves every level,
//! worked out from every row, folded ones too, so the models and times never move.

use argonauts_core::names::WorkflowId;
use argonauts_core::views::{
    AgentRow, NestedWorkflowRow, PhaseCount, PhaseRow, RowStatus, WorkflowRow,
};
use ratatui::text::{Line, Span};

use crate::text::{sanitize_line, str_width};
use crate::theme::{Role, Theme};

use super::layout::left;
use super::panes::Marks;
use super::workflow_rows::{glyph, row_label};

/// Cells each level sits further in than the one holding it.
pub(super) const STEP: usize = 2;

/// Cells a heading's name takes before its count, as Claude Code lays out a phase row.
const HEADING_NAME: usize = 8;

/// One line of a phase's outline.
#[derive(Debug, Clone)]
pub(super) enum Item<'a> {
    /// A row the cursor rests on: an agent, a check or the merge, or a workflow inside.
    Row {
        row: &'a WorkflowRow,
        indent: usize,
        /// The CLI whose own workflow holds the row, such as `claude-code`, when one does.
        own_cli: Option<&'a str>,
        /// The workflows inside the phase that hold the row, outermost first.
        within: Vec<&'a WorkflowId>,
    },
    /// A phase of a workflow inside, which the cursor passes over.
    Heading {
        phase: &'a PhaseRow,
        /// From 1, as the workflow numbers its phases.
        number: usize,
        indent: usize,
    },
}

impl<'a> Item<'a> {
    /// The row, for a line the cursor can rest on.
    pub(super) fn row(&self) -> Option<&'a WorkflowRow> {
        match self {
            Self::Row { row, .. } => Some(row),
            Self::Heading { .. } => None,
        }
    }

    pub(super) fn indent(&self) -> usize {
        match self {
            Self::Row { indent, .. } | Self::Heading { indent, .. } => *indent,
        }
    }
}

/// The outline of `rows`, the rows of one phase, whose own workflow is `own_cli`'s when a CLI
/// runs it. `open` says which workflows inside show what they hold.
pub(super) fn items<'a>(
    rows: &[&'a WorkflowRow],
    own_cli: Option<&'a str>,
    open: &dyn Fn(&NestedWorkflowRow) -> bool,
) -> Vec<Item<'a>> {
    let mut items = Vec::new();
    push_rows(&mut items, rows.iter().copied(), 0, own_cli, &[], open);
    items
}

fn push_rows<'a>(
    items: &mut Vec<Item<'a>>,
    rows: impl Iterator<Item = &'a WorkflowRow>,
    indent: usize,
    own_cli: Option<&'a str>,
    within: &[&'a WorkflowId],
    open: &dyn Fn(&NestedWorkflowRow) -> bool,
) {
    for row in rows {
        let WorkflowRow::Workflow(nested) = row else {
            items.push(Item::Row {
                row,
                indent,
                own_cli,
                within: within.to_vec(),
            });
            continue;
        };
        // A workflow an agent's own CLI runs hangs under that agent's row.
        let at = if nested.parent.is_some() {
            indent + STEP
        } else {
            indent
        };
        items.push(Item::Row {
            row,
            indent: at,
            own_cli,
            within: within.to_vec(),
        });
        if !open(nested) {
            continue;
        }
        let mut inside = within.to_vec();
        inside.push(&nested.id);
        // Inside a CLI's own workflow its agents are that CLI's; inside one of Argonauts
        // Code's they name their models alone.
        let cli = nested.worker.as_deref();
        if nested.phases.is_empty() {
            push_rows(items, nested.rows.iter(), at + STEP, cli, &inside, open);
            continue;
        }
        for (at_phase, phase) in nested.phases.iter().enumerate() {
            items.push(Item::Heading {
                phase,
                number: at_phase + 1,
                indent: at + STEP,
            });
            push_rows(items, phase.rows.iter(), at + 2 * STEP, cli, &inside, open);
        }
    }
}

/// The label column, from where a row of the phase's own starts its label: the most cells any
/// row of any phase takes to the end of its label, its level's cells included and every
/// workflow inside opened, so opening or folding one never moves the models.
pub(super) fn label_column(phases: &[PhaseRow]) -> usize {
    phases
        .iter()
        .flat_map(|phase| {
            let rows: Vec<&WorkflowRow> = phase.rows.iter().collect();
            items(&rows, None, &|_| true)
                .into_iter()
                .filter_map(|item| {
                    let row = item.row()?;
                    Some(item.indent() + str_width(&row_label(row)))
                })
                .collect::<Vec<_>>()
        })
        .max()
        .unwrap_or(0)
}

/// Whether a workflow inside is still going, which keeps it open unless you fold it.
pub(super) fn at_work(nested: &NestedWorkflowRow) -> bool {
    matches!(
        nested.status,
        RowStatus::Running | RowStatus::Queued | RowStatus::NeedsYou | RowStatus::Paused
    )
}

/// A workflow inside's mark: `▾` open and `▸` folded, lavender while it runs, green once done,
/// red if it failed.
pub(super) fn mark(nested: &NestedWorkflowRow, open: bool, theme: &Theme) -> (&'static str, Role) {
    let marks = Marks::for_theme(theme);
    let mark = if open { marks.open } else { marks.folded };
    let role = match nested.status {
        RowStatus::Done => Role::StateDone,
        RowStatus::Failed => Role::StateFailed,
        RowStatus::Skipped => Role::Faint,
        _ => Role::Question,
    };
    (mark, role)
}

/// What a workflow inside's row says where an agent's model would be: whose workflow it is,
/// then its agents done of all.
pub(super) fn whose(nested: &NestedWorkflowRow, cli_name: &dyn Fn(&str) -> String) -> String {
    let whose = match nested.worker.as_deref() {
        Some(cli) => format!("{}'s workflow", cli_name(cli)),
        None => "workflow".to_owned(),
    };
    format!("{whose} · {}", agents_done(nested))
}

/// `2/3 agents`, a workflow inside's agents done of all.
pub(super) fn agents_done(nested: &NestedWorkflowRow) -> String {
    format!("{}/{} agents", nested.agents_done, nested.agents_total)
}

/// A phase of a workflow inside, as Claude Code draws a phase row: `✔` or its number, its
/// name, and its agents done of all once it has any. `slot` is what stands before it, the
/// pointer's cells.
pub(super) fn heading(
    phase: &PhaseRow,
    number: usize,
    indent: usize,
    slot: usize,
    theme: &Theme,
) -> Line<'static> {
    let (mark, role) = match phase.status {
        RowStatus::Done | RowStatus::Failed | RowStatus::NeedsYou => {
            let (mark, role) = glyph(phase.status, false, theme);
            (mark.to_owned(), role)
        }
        _ => (number.to_string(), Role::Faint),
    };
    let started = phase.count.is_some() || phase.status != RowStatus::Waiting;
    let name = sanitize_line(&phase.name).trim().to_owned();
    let room = HEADING_NAME.max(str_width(&name) + 1);
    let mut spans = vec![
        Span::raw(" ".repeat(slot + indent)),
        Span::styled(mark, theme.style(role)),
        Span::raw(" "),
        Span::styled(
            left(&name, room),
            theme.style(if started { Role::Text } else { Role::Dim }),
        ),
    ];
    if let Some(PhaseCount { done, total }) = phase.count {
        spans.push(Span::styled(
            format!("{done}/{total}"),
            theme.style(Role::Dim),
        ));
    }
    Line::from(spans)
}

/// How deep a workflow inside sits: one level for one the phase holds, two for one inside that.
pub(super) fn depth(item: &Item<'_>) -> usize {
    match item {
        Item::Row { row, within, .. } => {
            within.len() + usize::from(matches!(row, WorkflowRow::Workflow(_)))
        }
        Item::Heading { .. } => 0,
    }
}

/// `date-cases'` or `lint's`, the possessive of a name.
pub(super) fn possessive(name: &str) -> String {
    if name.ends_with('s') {
        format!("{name}'")
    } else {
        format!("{name}'s")
    }
}

/// What the pane says once when it holds a workflow two levels inside the run, the deepest a
/// run nests: that its agents can't start one more.
pub(super) fn deepest_note(items: &[Item<'_>]) -> Option<String> {
    items.iter().find_map(|item| match item {
        Item::Row {
            row: WorkflowRow::Workflow(nested),
            ..
        } if depth(item) >= DEEPEST => Some(format!(
            "two levels inside the run is the deepest: {} agents can't start a workflow",
            possessive(sanitize_line(&nested.name).trim())
        )),
        _ => None,
    })
}

/// Two levels inside the run are the most a run nests.
const DEEPEST: usize = 2;

/// One line of an agent view's left pane, the phase's outline by its labels.
#[derive(Debug, Clone)]
enum OutlineLine {
    /// The agent at this place among the agent view's agents.
    Agent { at: usize, indent: usize },
    Workflow {
        name: String,
        status: RowStatus,
        open: bool,
        indent: usize,
    },
    Heading {
        phase: PhaseRow,
        number: usize,
        indent: usize,
    },
}

/// Where an agent of the outline sits.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Place {
    /// The workflows inside the phase that hold it, outermost first, by name.
    pub within: Vec<String>,
    /// The worker whose own CLI runs the outermost of them, by its label.
    pub host: Option<String>,
}

impl Place {
    /// What an agent view says of where it sits, after its model: `in build:due-flag's
    /// workflow add-due-flag`, or for two levels in, `two levels inside the run` and then a
    /// line of its own naming them.
    pub(super) fn words(&self) -> (Option<String>, Option<String>) {
        let whose = |name: &str| match &self.host {
            Some(host) => format!("{}'s workflow {name}", host),
            None => format!("the workflow {name}"),
        };
        match self.within.as_slice() {
            [] => (None, None),
            [only] => (Some(format!("in {}", whose(only))), None),
            [outer, .., inner] => (
                Some("two levels inside the run".to_owned()),
                Some(format!("in {inner}, which {} started", whose(outer))),
            ),
        }
    }
}

/// A phase's outline as an agent view lists it on its left: the same rows, labels only,
/// with the agents ↑↓ moves between.
#[derive(Debug, Clone, Default)]
pub(super) struct AgentOutline {
    lines: Vec<OutlineLine>,
    /// Its agents in the order the cursor meets them.
    pub agents: Vec<AgentRow>,
    /// Where each of them sits, in the same order.
    pub places: Vec<Place>,
    /// The workflows the phase's agents' own CLIs run.
    hosted: Vec<NestedWorkflowRow>,
}

impl AgentOutline {
    /// The agent view's outline of `items`, `open` saying which workflows inside show what they hold.
    pub(super) fn of(items: &[Item<'_>], open: &dyn Fn(&NestedWorkflowRow) -> bool) -> Self {
        let mut outline = Self::default();
        let named = |id: &WorkflowId| {
            items.iter().find_map(|item| match item.row() {
                Some(WorkflowRow::Workflow(nested)) if nested.id == *id => Some(nested),
                _ => None,
            })
        };
        for item in items {
            match item {
                Item::Heading {
                    phase,
                    number,
                    indent,
                } => outline.lines.push(OutlineLine::Heading {
                    phase: PhaseRow {
                        rows: Vec::new(),
                        ..(*phase).clone()
                    },
                    number: *number,
                    indent: *indent,
                }),
                Item::Row {
                    row: WorkflowRow::Workflow(nested),
                    indent,
                    ..
                } => {
                    if nested.parent.is_some() {
                        outline.hosted.push(NestedWorkflowRow {
                            phases: nested.phases.clone(),
                            rows: Vec::new(),
                            ..(*nested).clone()
                        });
                    }
                    outline.lines.push(OutlineLine::Workflow {
                        name: sanitize_line(&nested.name).trim().to_owned(),
                        status: nested.status,
                        open: open(nested),
                        indent: *indent,
                    });
                }
                Item::Row {
                    row: WorkflowRow::Agent(agent),
                    indent,
                    within,
                    ..
                } => {
                    let outermost = within.first().and_then(|id| named(id));
                    let host = outermost
                        .and_then(|nested| nested.parent.as_ref())
                        .and_then(|session| {
                            items.iter().find_map(|item| match item.row() {
                                Some(WorkflowRow::Agent(host))
                                    if host.session == *session && host.inner.is_none() =>
                                {
                                    Some(sanitize_line(&host.label).trim().to_owned())
                                }
                                _ => None,
                            })
                        });
                    let within = within
                        .iter()
                        .filter_map(|id| named(id))
                        .map(|nested| sanitize_line(&nested.name).trim().to_owned())
                        .collect();
                    outline.lines.push(OutlineLine::Agent {
                        at: outline.agents.len(),
                        indent: *indent,
                    });
                    outline.agents.push(agent.clone());
                    outline.places.push(Place { within, host });
                }
                // A check or the merge has no agent view.
                Item::Row { .. } => {}
            }
        }
        outline
    }

    /// The workflow `agent`'s own CLI runs, when it runs one.
    pub(super) fn hosted_by(&self, agent: &AgentRow) -> Option<&NestedWorkflowRow> {
        if agent.inner.is_some() {
            return None;
        }
        self.hosted
            .iter()
            .find(|nested| nested.parent.as_ref() == Some(&agent.session))
    }

    /// The cells its widest line takes, `margin` being the cursor's and the glyph's before a
    /// label, and `labels` the agents' labels as the agent view shows them.
    pub(super) fn width(&self, margin: usize, labels: &[String]) -> usize {
        self.lines
            .iter()
            .map(|line| match line {
                OutlineLine::Agent { at, indent } => {
                    margin + indent + labels.get(*at).map_or(0, |label| str_width(label))
                }
                OutlineLine::Workflow { name, indent, .. } => margin + indent + str_width(name),
                OutlineLine::Heading { phase, indent, .. } => {
                    let name = str_width(sanitize_line(&phase.name).trim());
                    let count = phase
                        .count
                        .map_or(0, |count| format!("{}/{}", count.done, count.total).len());
                    margin + indent + HEADING_NAME.max(name + 1) + count
                }
            })
            .max()
            .unwrap_or(0)
    }

    /// The lines the left pane shows, `height` rows of them around the selected agent:
    /// `agent` draws an agent's own line from its place and indent.
    pub(super) fn lines(
        &self,
        selected: usize,
        height: usize,
        theme: &Theme,
        agent: &dyn Fn(usize, usize) -> Line<'static>,
    ) -> Vec<Line<'static>> {
        let at = self
            .lines
            .iter()
            .position(|line| matches!(line, OutlineLine::Agent { at, .. } if *at == selected))
            .unwrap_or(0);
        let (first, last) = super::workflow_rows::window(at, self.lines.len(), height);
        self.lines[first..last]
            .iter()
            .map(|line| match line {
                OutlineLine::Agent { at, indent } => agent(*at, *indent),
                OutlineLine::Workflow {
                    name,
                    status,
                    open,
                    indent,
                } => {
                    let marks = Marks::for_theme(theme);
                    let mark = if *open { marks.open } else { marks.folded };
                    let role = match status {
                        RowStatus::Done => Role::StateDone,
                        RowStatus::Failed => Role::StateFailed,
                        RowStatus::Skipped => Role::Faint,
                        _ => Role::Question,
                    };
                    Line::from(vec![
                        Span::raw(" ".repeat(2 + indent)),
                        Span::styled(format!("{mark} "), theme.style(role)),
                        Span::styled(name.clone(), theme.style(Role::Text)),
                    ])
                }
                OutlineLine::Heading {
                    phase,
                    number,
                    indent,
                } => heading(phase, *number, *indent, 2, theme),
            })
            .collect()
    }
}

/// `Its workflow · add-due-flag · 2/3 agents · 25.9k tok`, then its phases on one line, `✔
/// Write 2/2 · 2 Check 0/1`, for the agent view of the agent whose CLI runs it.
pub(super) fn its_workflow(
    nested: &NestedWorkflowRow,
    theme: &Theme,
) -> (String, Vec<Line<'static>>) {
    let tokens: u64 = nested
        .phases
        .iter()
        .flat_map(|phase| &phase.rows)
        .chain(&nested.rows)
        .map(tokens_in)
        .sum();
    let mut title = format!(
        "Its workflow · {} · {}/{} agents",
        sanitize_line(&nested.name).trim(),
        nested.agents_done,
        nested.agents_total
    );
    if tokens > 0 {
        title.push_str(&format!(" · {}", super::panes::tokens(tokens)));
    }
    let mut spans = vec![Span::raw("  ")];
    for (at, phase) in nested.phases.iter().enumerate() {
        if at > 0 {
            spans.push(Span::styled(" · ", theme.style(Role::Dim)));
        }
        let (mark, role) = match phase.status {
            RowStatus::Done | RowStatus::Failed | RowStatus::NeedsYou => {
                let (mark, role) = glyph(phase.status, false, theme);
                (mark.to_owned(), role)
            }
            _ => ((at + 1).to_string(), Role::Faint),
        };
        spans.push(Span::styled(format!("{mark} "), theme.style(role)));
        let name = sanitize_line(&phase.name).trim().to_owned();
        match phase.count {
            Some(PhaseCount { done, total }) => spans.push(Span::styled(
                format!("{name} {done}/{total}"),
                theme.style(Role::Text),
            )),
            None => spans.push(Span::styled(name, theme.style(Role::Dim))),
        }
    }
    let body = if nested.phases.is_empty() {
        Vec::new()
    } else {
        vec![Line::from(spans)]
    };
    (title, body)
}

/// The tokens a row's agents spent, those of a workflow inside included.
fn tokens_in(row: &WorkflowRow) -> u64 {
    match row {
        WorkflowRow::Agent(agent) => agent.tokens,
        WorkflowRow::Workflow(nested) => nested
            .phases
            .iter()
            .flat_map(|phase| &phase.rows)
            .chain(&nested.rows)
            .map(tokens_in)
            .sum(),
        WorkflowRow::Command(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_ending_in_s_takes_only_the_apostrophe() {
        assert_eq!(possessive("date-cases"), "date-cases'");
        assert_eq!(possessive("lint"), "lint's");
    }
}
