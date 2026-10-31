//! A run's crew on the Context tab: one folded line for the run, a line an agent once it opens,
//! and an agent's own block in the main session's form.
//!
//! An agent is every session one label had in one run. Its tries count as one agent, with the
//! newest try's numbers, and a tie-breaker counts under the agent whose question it answered,
//! as its row says.

use std::collections::BTreeMap;

use argonauts_core::RunId;
use argonauts_core::app::views::{ContextRow, ContextView, EndedHow};
use argonauts_core::base::names::{ModelRef, SessionKey};
use argonauts_core::look::display_names::DisplayNames;
use ratatui::text::Span;
use time::OffsetDateTime;

use super::block::{self, Block, INDENT, Part};
use super::lists::{branch, folded, title};
use super::words::{cc_style, count, time_of};
use crate::base::format::context_tokens;
use crate::base::kit::legend::{BEFORE_NUMBERS, BETWEEN_WORDS};
use crate::base::kit::logo::{ModelLabel, NoPicture};
use crate::base::kit::{Drawn, Kit, ljust, rjust};
use crate::base::text::{sanitize_line, spans_width, str_width, truncate_spans};
use crate::frame::state::AppState;

/// What the screen knows of the crew's sessions beyond their rows: how much Argonauts Code
/// sent each.
#[derive(Debug, Clone, Default)]
pub struct CrewFacts {
    /// The tokens of every prompt part sent, by its own count.
    prompts: BTreeMap<(RunId, SessionKey), u64>,
}

impl CrewFacts {
    /// The facts of the sessions `view` lists, as the screen's state has them.
    pub fn of(state: &AppState, view: &ContextView) -> Self {
        let mut facts = Self::default();
        for row in &view.rows {
            let Some(run) = &row.run else {
                continue;
            };
            let Some(session) = state
                .runs
                .get(run)
                .and_then(|known| known.sessions.get(&row.session))
            else {
                continue;
            };
            let key = (run.clone(), row.session.clone());
            let sent: u64 = session
                .inputs
                .iter()
                .flat_map(|input| &input.parts)
                .map(|part| u64::from(part.approx_tokens))
                .sum();
            if sent > 0 {
                facts.prompts.insert(key, sent);
            }
        }
        facts
    }

    fn prompt(&self, row: &ContextRow) -> Option<u64> {
        let run = row.run.as_ref()?;
        self.prompts
            .get(&(run.clone(), row.session.clone()))
            .copied()
    }
}

/// One crew agent: the sessions one label had in one run.
pub(super) struct Agent<'a> {
    pub run: &'a RunId,
    pub label: &'a str,
    /// Its tries' sessions, in the order they started.
    sessions: Vec<&'a ContextRow>,
    /// How many tie-breakers answered its questions.
    tie_breakers: usize,
}

impl<'a> Agent<'a> {
    /// The session its numbers come from: the newest that started.
    fn newest(&self) -> Option<&'a ContextRow> {
        self.sessions
            .iter()
            .rev()
            .find(|row| !never_started(row))
            .copied()
    }

    /// The last session it had, started or not.
    fn last(&self) -> &'a ContextRow {
        self.sessions[self.sessions.len() - 1]
    }

    /// When its last session ended, once every one has.
    pub fn ended(&self) -> Option<OffsetDateTime> {
        self.sessions
            .iter()
            .map(|row| row.ended.as_ref().map(|left| left.at))
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .max()
    }

    /// Whether any of its sessions started: one that never did has no numbers to draw.
    pub fn started(&self) -> bool {
        self.newest().is_some()
    }

    /// What its window holds, by its newest session.
    pub fn used(&self) -> u64 {
        self.newest().map_or(0, |row| row.used)
    }

    /// Its tries, a try a model in a row: each model with whether it started.
    fn tries(&self) -> Vec<(&'a ModelRef, bool)> {
        let mut tries: Vec<(&ModelRef, bool)> = Vec::new();
        for row in &self.sessions {
            let started = !never_started(row);
            match tries.last_mut() {
                Some((worker, any)) if *worker == &row.worker => *any |= started,
                _ => tries.push((&row.worker, started)),
            }
        }
        tries
    }
}

/// A session that never started: it failed before its CLI said anything, so it has no numbers
/// to draw.
fn never_started(row: &ContextRow) -> bool {
    let failed = matches!(
        row.ended.as_ref().map(|left| &left.how),
        Some(EndedHow::Failed { .. })
    );
    failed && row.used == 0 && row.window.is_none() && row.tool_calls == 0
}

/// The agents of `run` among `rows`, in the order they first started. A tie-breaker counts
/// under the agent whose question it answered, not as a try.
pub(super) fn agents<'a>(rows: &'a [ContextRow], run: &'a RunId) -> Vec<Agent<'a>> {
    let mut agents: Vec<Agent<'a>> = Vec::new();
    let mut breakers: Vec<(&'a SessionKey, &'a ContextRow)> = Vec::new();
    for row in rows.iter().filter(|row| row.run.as_ref() == Some(run)) {
        if let Some(asker) = &row.answered_for {
            breakers.push((asker, row));
            continue;
        }
        match agents.iter_mut().find(|agent| agent.label == row.label) {
            Some(agent) => agent.sessions.push(row),
            None => agents.push(Agent {
                run,
                label: &row.label,
                sessions: vec![row],
                tie_breakers: 0,
            }),
        }
    }
    for (asker, row) in breakers {
        let asked = agents
            .iter()
            .position(|agent| agent.sessions.iter().any(|try_| try_.session == *asker));
        match asked {
            Some(at) => agents[at].tie_breakers += 1,
            // One whose asker the tab doesn't list is an agent of its own.
            None => agents.push(Agent {
                run,
                label: &row.label,
                sessions: vec![row],
                tie_breakers: 0,
            }),
        }
    }
    agents
}

/// How many agents of the runs in `rows` are at work and how many have ended, each run's tries
/// and tie-breakers counted as the tab counts them.
pub(super) fn tally(rows: &[ContextRow]) -> (usize, usize) {
    let mut runs: Vec<&RunId> = Vec::new();
    for run in rows.iter().filter_map(|row| row.run.as_ref()) {
        if !runs.contains(&run) {
            runs.push(run);
        }
    }
    let mut tally = (0, 0);
    for run in runs {
        for agent in agents(rows, run) {
            match agent.ended() {
                // An agent that never started didn't end any work either.
                Some(_) if !agent.started() => {}
                Some(_) => tally.1 += 1,
                None => tally.0 += 1,
            }
        }
    }
    tally
}

/// One run's crew, as the tab lists it.
pub(super) struct Crew<'a> {
    pub run: &'a RunId,
    pub title: String,
    pub agents: Vec<Agent<'a>>,
    /// The tab has no session of the conversation, so the run's answer came from its own.
    pub no_conversation: bool,
}

impl Crew<'_> {
    /// `Crew agents · the last run, Make the rate limiter per API key`.
    pub fn title(&self, kit: &Kit<'_>) -> Vec<Span<'static>> {
        let which = if self.agents.iter().all(|agent| agent.ended().is_some()) {
            "the last run"
        } else {
            "the run at work"
        };
        let name = sanitize_line(&self.title).trim().to_owned();
        title(kit, "Crew agents", &format!(" \u{b7} {which}, {name}"))
    }

    /// `└ 7 agents · 126.7k tokens · all ended by 8:33 AM`, `└ 1 agent · 13.3k tokens · ended
    /// 8:31 AM`, or `└ 1 agent · couldn't start` when none of them started.
    pub fn folded(&self, kit: &Kit<'_>, now: OffsetDateTime) -> Vec<Span<'static>> {
        let tokens: u64 = self.agents.iter().map(Agent::used).sum();
        let at_work = self
            .agents
            .iter()
            .filter(|agent| agent.ended().is_none())
            .count();
        let answered_by = self
            .agents
            .iter()
            .find(|agent| agent.label == ANSWER)
            .filter(|_| self.no_conversation && at_work == 0);
        let agents = count(self.agents.len(), "agent");
        if at_work == 0 && !self.agents.iter().any(Agent::started) {
            return folded(kit, &format!("{agents} \u{b7} couldn't start"));
        }
        let last = self.agents.iter().filter_map(Agent::ended).max();
        let state = match (answered_by, last) {
            (Some(answer), _) => format!("its answer came from {}", answer.label),
            _ if at_work > 0 => format!("{at_work} at work"),
            (None, Some(last)) if self.agents.len() == 1 => format!("ended {}", time_of(last, now)),
            (None, Some(last)) => format!("all ended by {}", time_of(last, now)),
            (None, None) => String::new(),
        };
        let mut whole = format!("{agents} \u{b7} {} tokens", context_tokens(tokens));
        if !state.is_empty() {
            whole.push_str(&format!(" \u{b7} {state}"));
        }
        folded(kit, &whole)
    }

    /// A line an agent: its label, its model, what its window holds and when it ended. A label
    /// longer than [`LABEL_AT_MOST`] is cut, and where `width` can't hold every column the model
    /// goes first, so what the window holds and when it ended stay.
    pub fn lines(
        &self,
        kit: &Kit<'_>,
        names: &DisplayNames,
        now: OffsetDateTime,
        width: usize,
    ) -> Vec<Vec<Span<'static>>> {
        let labels: Vec<String> = self
            .agents
            .iter()
            .map(|agent| {
                let label = sanitize_line(agent.label).trim().to_owned();
                truncate_spans(&[Span::raw(label)], LABEL_AT_MOST)
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        // Where logos draw as pictures, each model has its logo before it.
        let (models, logos): (Vec<String>, Vec<Vec<Span<'static>>>) = self
            .agents
            .iter()
            .map(|agent| {
                let worker = &agent.newest().unwrap_or_else(|| agent.last()).worker;
                let label = ModelLabel::of(names, worker, kit.theme, NoPicture::Nothing);
                let logo = label.logo.clone();
                let model = label
                    .beside_picture()
                    .unwrap_or_else(|| on_cli(names, worker));
                (model, logo)
            })
            .unzip();
        let logo_cells = logos
            .iter()
            .map(|logo| spans_width(logo))
            .max()
            .unwrap_or(0);
        let label_cells = labels
            .iter()
            .map(|label| str_width(label))
            .max()
            .unwrap_or(0)
            + BETWEEN_WORDS;
        let mut model_cells = models
            .iter()
            .map(|model| str_width(model))
            .max()
            .unwrap_or(0)
            + BEFORE_NUMBERS
            + logo_cells;
        // What the window holds and when it ended, or that it couldn't start.
        let tails: Vec<(Option<String>, String)> = self
            .agents
            .iter()
            .map(|agent| match agent.newest() {
                Some(row) => (
                    Some(used_of_window(row)),
                    match agent.ended() {
                        Some(at) => format!(" \u{b7} ended {}", time_of(at, now)),
                        None => " \u{b7} at work".to_owned(),
                    },
                ),
                None => {
                    let at = agent.last().ended.as_ref().map(|left| left.at);
                    let when = at.map(|at| format!(" \u{b7} {}", time_of(at, now)));
                    (None, format!("couldn't start{}", when.unwrap_or_default()))
                }
            })
            .collect();
        let widest_tail = tails
            .iter()
            .map(|(used, said)| used.as_deref().map_or(0, str_width) + str_width(said))
            .max()
            .unwrap_or(0);
        // What each window holds stands right-aligned, as a column of numbers does.
        let used_cells = tails
            .iter()
            .filter_map(|(used, _)| used.as_deref().map(str_width))
            .max()
            .unwrap_or(0);
        if INDENT + BRANCH + label_cells + model_cells + widest_tail > width {
            model_cells = 0;
        }
        (0..self.agents.len())
            .map(|at| {
                let mut line = vec![
                    Span::raw(" ".repeat(INDENT)),
                    kit.dim(branch(at, self.agents.len())),
                    Span::raw(ljust(&labels[at], label_cells)),
                ];
                if model_cells > 0 {
                    let logo = &logos[at];
                    line.extend(logo.iter().cloned());
                    let room = model_cells.saturating_sub(spans_width(logo));
                    line.push(kit.dim(ljust(&models[at], room)));
                }
                let (used, said) = &tails[at];
                if let Some(used) = used {
                    line.push(Span::raw(rjust(used, used_cells)));
                }
                line.push(kit.dim(said.clone()));
                line
            })
            .collect()
    }
}

/// The label of the run's closing answer.
const ANSWER: &str = "main:answer";
/// The most cells a label takes in the crew's list; a longer one is cut.
const LABEL_AT_MOST: usize = 28;
/// The cells of the branch before each agent's label.
const BRANCH: usize = 2;

/// `13.3k/1m (1%)`, or the count alone while the window isn't known.
fn used_of_window(row: &ContextRow) -> String {
    match row.window.filter(|window| *window > 0) {
        Some(window) => format!(
            "{}/{} ({}%)",
            context_tokens(row.used),
            context_tokens(window),
            (row.used as f64 / window as f64 * 100.0).round()
        ),
        None => context_tokens(row.used),
    }
}

/// `Haiku 5.5 on Claude Code`.
fn on_cli(names: &DisplayNames, worker: &ModelRef) -> String {
    let cli = names.cli(&worker.agent);
    match model_name(names, worker) {
        Some(model) => sanitize_line(&format!("{model} on {cli}"))
            .trim()
            .to_owned(),
        None => sanitize_line(&cli).trim().to_owned(),
    }
}

/// The model's name, or its id where the names don't know it.
fn model_name(names: &DisplayNames, worker: &ModelRef) -> Option<String> {
    names
        .model(&worker.agent, worker.model.as_deref())
        .or_else(|| worker.model.clone())
}

/// An agent's own block, in the form of the main session's: the grid and what fills it, then
/// what the tab knows of its tries beside.
pub(super) fn agent_block(
    kit: &Kit<'_>,
    agent: &Agent<'_>,
    names: &DisplayNames,
    facts: &CrewFacts,
    width: usize,
) -> Drawn {
    let label = sanitize_line(agent.label).trim().to_owned();
    let mut drawn = Drawn::new();
    drawn.push(usage_title(kit, Some(&label)));
    let Some(row) = agent.newest() else {
        drawn.push(vec![
            Span::raw(" ".repeat(INDENT)),
            kit.dim(format!(
                "{} couldn't start, so it holds nothing.",
                on_cli(names, &agent.last().worker)
            )),
        ]);
        return drawn;
    };
    // What was sent to the newest try, every session of it.
    let sent: u64 = agent
        .sessions
        .iter()
        .rev()
        .take_while(|session| session.worker == row.worker)
        .filter_map(|session| facts.prompt(session))
        .sum();
    let filled = Filled {
        row,
        model: ModelLabel::of(names, &row.worker, kit.theme, NoPicture::Nothing)
            .beside_picture()
            .unwrap_or_else(|| on_cli(names, &row.worker)),
        sent: (sent > 0).then_some(sent),
        notes: history(agent, names),
    };
    drawn.append(row_usage(kit, &filled, names, width));
    drawn
}

/// `Context Usage`, and `· build:per-api-key` for an agent's own.
pub(super) fn usage_title(kit: &Kit<'_>, label: Option<&str>) -> Vec<Span<'static>> {
    let mut title = vec![
        Span::raw(" ".repeat(INDENT)),
        Span::styled("Context Usage", kit.bold(crate::base::theme::Role::Text)),
    ];
    if let Some(label) = label {
        title.push(kit.dim(format!(" \u{b7} {label}")));
    }
    title
}

/// A session the tab knows by its row: what its CLI said fills it, or how full it is.
pub(super) struct Filled<'a> {
    pub row: &'a ContextRow,
    /// The model as the block names it.
    pub model: String,
    /// What Argonauts Code sent it, where the screen counted it.
    pub sent: Option<u64>,
    /// Lines the block ends with.
    pub notes: Vec<String>,
}

/// A session's block from its row: Claude Code's categories where its CLI said them, else
/// its whole fill, said to be only that.
pub(super) fn row_usage(
    kit: &Kit<'_>,
    filled: &Filled<'_>,
    names: &DisplayNames,
    width: usize,
) -> Drawn {
    use argonauts_core::runs::events::context::ContextKind;

    let row = filled.row;
    let cli = sanitize_line(&names.cli(&row.worker.agent))
        .trim()
        .to_owned();
    let mut notes = Vec::new();
    let held = |kind| {
        row.categories
            .iter()
            .filter(|category| category.kind == kind)
            .map(|category| category.tokens)
            .sum::<u64>()
    };
    let (heading, parts, free, buffer) = if row.categories.is_empty() {
        notes.push(if row.counts {
            format!("{cli} says what fills it when its answer ends.")
        } else {
            format!("{cli} reports how full, not with what.")
        });
        if let Some(sent) = filled.sent {
            notes.push(format!(
                "Our prompt in it: {} tokens.",
                context_tokens(sent)
            ));
        }
        let used = Part {
            name: "Used".to_owned(),
            tokens: row.used,
            style: cc_style(kit, USED_COLOR),
        };
        let free = row.window.unwrap_or(0).saturating_sub(row.used);
        (format!("As {cli} reports it"), vec![used], free, 0)
    } else {
        let parts = row
            .categories
            .iter()
            .filter(|category| category.kind == ContextKind::Used && category.tokens > 0)
            .map(|category| Part {
                name: category.name.clone(),
                tokens: category.tokens,
                style: cc_style(kit, &category.color),
            })
            .collect();
        let buffer = held(ContextKind::Buffer);
        let free = match held(ContextKind::Free) {
            0 => row
                .window
                .unwrap_or(0)
                .saturating_sub(row.used)
                .saturating_sub(buffer),
            free => free,
        };
        (
            "Estimated usage by category".to_owned(),
            parts,
            free,
            buffer,
        )
    };
    notes.extend(filled.notes.iter().cloned());
    let Some(window) = row.window.filter(|window| *window > 0) else {
        let mut drawn = Drawn::new();
        let line = |spans: Vec<Span<'static>>| {
            let mut line = vec![Span::raw(" ".repeat(INDENT))];
            line.extend(spans);
            line
        };
        drawn.push(line(vec![Span::raw(filled.model.clone())]));
        drawn.push(line(vec![
            kit.dim(format!("{} tokens", context_tokens(row.used))),
        ]));
        for note in notes {
            drawn.push(line(vec![kit.dim(note)]));
        }
        return drawn;
    };
    let block = Block {
        model: Some(filled.model.clone()),
        model_id: row.worker.model.as_deref(),
        logo: Some(ModelLabel::of(names, &row.worker, kit.theme, NoPicture::Nothing).whose),
        used: row.used,
        window: Some(window),
        percent: None,
        heading,
        parts,
        free,
        buffer,
        grid: None,
        usage: None,
        notes,
        parts_ours: false,
        used_ours: row.estimated,
        used_tail: String::new(),
    };
    block::rows(kit, &block, width)
}

/// The colour Claude Code's messages draw in, which a session's whole fill takes where its
/// CLI says only how full it is.
const USED_COLOR: &str = "purple_FOR_SUBAGENTS_ONLY";

/// `2 tries: Haiku 5.5, then GPT 6 Luna;` and `3 tie-breakers answered its questions.`
fn history(agent: &Agent<'_>, names: &DisplayNames) -> Vec<String> {
    let tries = agent.tries();
    let mut lines = Vec::new();
    if tries.len() > 1 {
        let named: Vec<String> = tries
            .iter()
            .map(|(worker, started)| {
                let name = model_name(names, worker).unwrap_or_else(|| names.cli(&worker.agent));
                let name = sanitize_line(&name).trim().to_owned();
                if *started {
                    name
                } else {
                    format!("{name}, which couldn't start")
                }
            })
            .collect();
        let (last, first) = named.split_last().expect("two tries or more");
        lines.push(format!(
            "{} tries: {}, then {last}",
            tries.len(),
            first.join(", ")
        ));
    }
    match agent.tie_breakers {
        0 => {}
        1 => lines.push("1 tie-breaker answered its question".to_owned()),
        n => lines.push(format!("{n} tie-breakers answered its questions")),
    }
    let last = lines.len().saturating_sub(1);
    for (at, line) in lines.iter_mut().enumerate() {
        line.push(if at == last { '.' } else { ';' });
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::Spend;
    use argonauts_core::app::views::SessionLeft;
    use time::macros::datetime;

    fn row(n: u32, label: &str, worker: &str, used: u64, ended: Option<EndedHow>) -> ContextRow {
        ContextRow {
            run: Some(RunId::new("run-1").unwrap()),
            session: SessionKey::numbered(n),
            label: label.to_owned(),
            worker: worker.parse().unwrap(),
            used,
            window: (used > 0).then_some(1_000_000),
            cap: None,
            tool_calls: u32::from(used > 0),
            spent: Spend::default(),
            rotations: 0,
            estimated: false,
            counts: false,
            categories: Vec::new(),
            recorded: None,
            ended: ended.map(|how| SessionLeft {
                at: datetime!(2026-10-20 12:30 UTC) + time::Duration::minutes(i64::from(n)),
                how,
                fallback_for: None,
            }),
            answered_for: None,
        }
    }

    /// Tries are one agent with the newest try's numbers, a tie-breaker counts under the agent
    /// whose question it answered, and a start that never happened draws nothing.
    #[test]
    fn tries_and_tie_breakers_make_one_agent() {
        let run = RunId::new("run-1").unwrap();
        let failed = || {
            Some(EndedHow::Failed {
                why: "refused".to_owned(),
            })
        };
        let rows = vec![
            row(
                1,
                "build:x",
                "claude-code/claude-haiku-5-5",
                20_000,
                Some(EndedHow::Done),
            ),
            row(2, "build:x", "claude-code/claude-opus-5-5", 0, failed()),
            row(
                3,
                "build:x",
                "codex/gpt-6-luna",
                30_000,
                Some(EndedHow::Done),
            ),
            ContextRow {
                answered_for: Some(SessionKey::numbered(3)),
                ..row(
                    4,
                    "build:x",
                    "claude-code/claude-opus-5-5",
                    9_000,
                    Some(EndedHow::Done),
                )
            },
            row(5, "review:x", "ollama/qwen3-coder:30b", 0, failed()),
        ];
        let agents = agents(&rows, &run);
        assert_eq!(agents.len(), 2);
        let build = &agents[0];
        assert_eq!(build.used(), 30_000);
        assert_eq!(
            build.newest().map(|row| &row.session),
            Some(&SessionKey::numbered(3))
        );
        assert_eq!(build.tie_breakers, 1);
        let names = crate::testing::sample::names();
        assert_eq!(
            history(build, &names),
            [
                "3 tries: Haiku 5.5, Opus 5.5, which couldn't start, then GPT 6 Luna;",
                "1 tie-breaker answered its question."
            ]
        );
        assert!(agents[1].newest().is_none());
        // The agent that never started ended no work, so the ended count leaves it out.
        assert_eq!(tally(&rows), (0, 1));
    }

    fn text(spans: &[Span<'_>]) -> String {
        spans.iter().map(|span| span.content.as_ref()).collect()
    }

    fn crew<'a>(rows: &'a [ContextRow], run: &'a RunId) -> Crew<'a> {
        Crew {
            run,
            title: "Make the rate limiter per API key".to_owned(),
            agents: agents(rows, run),
            no_conversation: false,
        }
    }

    /// A crew none of whose agents started says so, and a crew of one says when it ended.
    #[test]
    fn a_folded_crew_says_it_couldnt_start_or_when_its_one_agent_ended() {
        let theme = crate::base::theme::Theme::dark();
        let kit = Kit::new(&theme);
        let run = RunId::new("run-1").unwrap();
        let now = datetime!(2026-10-20 13:00 UTC);
        let failed = Some(EndedHow::Failed {
            why: "refused".to_owned(),
        });
        let never = [row(1, "review:x", "ollama/qwen3-coder:30b", 0, failed)];
        let folded = text(&crew(&never, &run).folded(&kit, now));
        assert!(
            folded.ends_with("1 agent \u{b7} couldn't start"),
            "{folded}"
        );
        assert_eq!(tally(&never), (0, 0));

        let one = [row(
            1,
            "build:x",
            "claude-code/claude-haiku-5-5",
            20_000,
            Some(EndedHow::Done),
        )];
        let folded = text(&crew(&one, &run).folded(&kit, now));
        assert!(
            folded.ends_with("1 agent \u{b7} 20k tokens \u{b7} ended 12:31 PM"),
            "{folded}"
        );
    }

    /// A label past the cap is cut, and a list too narrow for every column gives up the model
    /// first, keeping what the window holds and when the agent ended.
    #[test]
    fn a_long_label_is_cut_and_a_narrow_list_gives_up_the_model() {
        let theme = crate::base::theme::Theme::dark();
        let kit = Kit::new(&theme);
        let names = crate::testing::sample::names();
        let run = RunId::new("run-1").unwrap();
        let now = datetime!(2026-10-20 13:00 UTC);
        let long = format!("review:{}", "a-very-long-task-name-".repeat(3));
        let rows = [
            row(
                1,
                "review:task-1",
                "claude-code/claude-haiku-5-5",
                20_000,
                Some(EndedHow::Done),
            ),
            row(2, &long, "codex/gpt-6-luna", 30_000, Some(EndedHow::Done)),
        ];
        let crew = crew(&rows, &run);
        let wide: Vec<String> = crew
            .lines(&kit, &names, now, 118)
            .iter()
            .map(|line| text(line))
            .collect();
        let cut = &wide[1];
        assert!(
            cut.contains("review:a-very-long-task-nam\u{2026}  "),
            "{cut}"
        );
        assert!(
            wide.iter()
                .all(|line| line.contains("Haiku 5.5") || line.contains("GPT 6 Luna"))
        );
        assert!(wide.iter().all(|line| str_width(line) <= 118), "{wide:#?}");
        let narrow: Vec<String> = crew
            .lines(&kit, &names, now, 80)
            .iter()
            .map(|line| text(line))
            .collect();
        assert!(
            narrow
                .iter()
                .all(|line| !line.contains("Haiku") && !line.contains("Luna")),
            "{narrow:#?}"
        );
        assert!(narrow[0].ends_with(" \u{b7} ended 12:31 PM"), "{narrow:#?}");
        assert!(
            narrow.iter().all(|line| str_width(line) <= 80),
            "{narrow:#?}"
        );
    }
}
