//! The status summary as an incremental fold: each recorded event updates it, so it's never
//! recomputed from the whole log. `status::summary` is a fresh folder over a slice of events.

use std::collections::VecDeque;

use serde::Serialize;

use argonauts_core::api::{CardBrief, StatusSummary};
use argonauts_core::events::card::{
    CardEvent, CardSpec, CardState, GateOutcome, IntegrationEvent, ReviewEvent,
};
use argonauts_core::events::checkpoint::{AnswerAction, CheckpointEvent, CheckpointPacket};
use argonauts_core::events::goal::GoalEvent;
use argonauts_core::events::plan::{DivideEvent, JudgingResult, PlanEvent};
use argonauts_core::events::run::{Party, PhaseChange, RunLifecycle};
use argonauts_core::events::shared::{BlackboardEvent, BlackboardKind};
use argonauts_core::events::vendor::{BudgetEvent, HandoffEvent, RunBudget, VendorEvent};
use argonauts_core::events::workflow::{StepEnd, WorkflowEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::limits::QuotaReading;
use argonauts_core::names::{CheckpointId, DecisionId};
use argonauts_core::settings::protocol::VisibilitySettings;
use argonauts_core::{CardId, PhaseStatus, RunId};

use crate::state::workflow::Workflows;
use crate::status::Viewer;
use crate::status::visibility::{self, Audience, Moment};
use crate::status::working;

/// Lines kept in `recent`, newest last.
pub const RECENT_LINES: usize = 20;

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub audience: Audience,
    pub text: String,
    /// The same line naming plan authors, for those who may see them.
    pub with_authors: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub brief: CardBrief,
    pub audience: Audience,
    pub depends_on: Vec<CardId>,
}

/// Everything any viewer's summary is filtered from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tagged {
    pub run: Option<RunId>,
    pub goal: Vec<String>,
    pub plan: Option<String>,
    pub decisions: Vec<(DecisionId, String)>,
    pub cards: Vec<Card>,
    pub recent: VecDeque<Line>,
    /// Where the run's workflow is, shown after the recent lines.
    pub now: Option<Line>,
    pub budget: Option<RunBudget>,
    pub quotas: Vec<QuotaReading>,
    pub waiting: Vec<(CheckpointId, String)>,
    pub moment: Moment,
}

#[derive(Debug, Clone, Default)]
pub struct Folder {
    tagged: Tagged,
    rules: VisibilitySettings,
    /// Every card's hidden paths. No agent sees a line that names one.
    hidden: Vec<String>,
    /// The newest draft round sealed so far.
    drafted_round: u8,
    /// Composed workflows may run no step that records a legacy phase, so the working line
    /// comes from the workflow events alone.
    workflows: Workflows,
    /// The open plan checkpoint's summary, which becomes the plan when you approve it.
    proposed_plan: Option<(CheckpointId, String)>,
    /// Who the last pushed event changed the summary for.
    changed: Option<Audience>,
}

impl Folder {
    /// A folder applying `visibility.*` from the run's settings.
    pub fn new(rules: VisibilitySettings) -> Self {
        Self {
            rules,
            ..Self::default()
        }
    }

    pub fn push(&mut self, event: &RunEvent) {
        self.changed = self.fold(event);
        if self.tagged.run.is_none() {
            self.tagged.run = Some(event.run_id.clone());
            self.changed = Some(Audience::Everyone);
        }
    }

    /// Whether the last pushed event changed what this viewer sees, so its `status.json` needs
    /// writing.
    pub fn changed_for(&self, viewer: &Viewer) -> bool {
        if let Viewer::Session { role, .. } = viewer
            && !visibility::enabled(*role, &self.rules)
        {
            return false;
        }
        self.changed.as_ref().is_some_and(|audience| {
            visibility::sees(viewer, audience, &self.tagged.moment, &self.rules)
        })
    }

    pub fn summary(&self, viewer: &Viewer) -> StatusSummary {
        visibility::filter(&self.tagged, viewer, &self.rules)
    }

    pub fn tagged(&self) -> &Tagged {
        &self.tagged
    }

    fn fold(&mut self, event: &RunEvent) -> Option<Audience> {
        let card = event.card_id.as_ref();
        match &event.kind {
            RunEventKind::Run(e) => self.run(e),
            RunEventKind::Phase(e) => self.phase(e),
            RunEventKind::Goal(e) => self.goal(e),
            RunEventKind::Plan(e) => self.plan(e),
            RunEventKind::Divide(DivideEvent::Owner { card, worker, .. }) => {
                let known = self.card_mut(card)?;
                known.brief.owner = Some(worker.clone());
                Some(known.audience.clone())
            }
            RunEventKind::Divide(_) => None,
            RunEventKind::Card(e) => self.card(card?, e),
            RunEventKind::Gate(outcome) => self.gate(card, outcome),
            RunEventKind::Review(e) => self.review(card?, e),
            RunEventKind::Integration(e) => self.integration(e),
            RunEventKind::Checkpoint(e) => self.checkpoint(e),
            RunEventKind::Handoff(e) => self.handoff(e),
            RunEventKind::Vendor(e) => self.vendor(e),
            RunEventKind::Budget(e) => self.budget(e),
            RunEventKind::Workflow(e) => self.workflow(e, event),
            RunEventKind::Blackboard(BlackboardEvent::Posted(message)) => {
                // Only settled records count as decisions; the traffic itself isn't state.
                let record = message.kind == BlackboardKind::Decision
                    && matches!(message.from, Party::Conductor | Party::User);
                if !record {
                    return None;
                }
                let id = DecisionId::parse(message.id.as_str())?;
                self.tagged.decisions.push((id, message.text.clone()));
                Some(Audience::Everyone)
            }
            RunEventKind::Blackboard(BlackboardEvent::Delivered { .. })
            | RunEventKind::Session(_)
            | RunEventKind::Agent(_)
            | RunEventKind::Retro(_)
            | RunEventKind::Note(_) => None,
        }
    }

    fn run(&mut self, event: &RunLifecycle) -> Option<Audience> {
        match event {
            RunLifecycle::Finished { outcome, .. } => {
                self.line(format!("Run finished: {}", words(outcome)))
            }
            _ => None,
        }
    }

    fn phase(&mut self, change: &PhaseChange) -> Option<Audience> {
        let summary = change.summary.as_ref()?;
        if !matches!(change.status, PhaseStatus::Finished | PhaseStatus::Failed) {
            return None;
        }
        self.line(format!("{}: {summary}", words(&change.phase)))
    }

    fn workflow(&mut self, event: &WorkflowEvent, envelope: &RunEvent) -> Option<Audience> {
        self.workflows.apply(event, envelope);
        // Steps that finish well show up through their cards, gates, and checkpoints. The
        // step's own summary stays out, since a draft or review summary can name authors.
        let pushed = match event {
            WorkflowEvent::StepFinished(finished) if finished.end == StepEnd::Failed => {
                let at = working::place(
                    &self.workflows,
                    &finished.step,
                    finished.kind,
                    finished.round,
                );
                self.line(format!("{at} failed"))
            }
            WorkflowEvent::Changed(changed) => {
                self.line(format!("The workflow changed: {}", changed.reason))
            }
            _ => None,
        };
        let now = working::line(&self.workflows).map(|text| {
            self.guarded(Line {
                audience: Audience::Everyone,
                text: format!("Now: {text}"),
                with_authors: None,
            })
        });
        if now == self.tagged.now {
            return pushed;
        }
        self.tagged.now = now;
        Some(Audience::Everyone)
    }

    fn goal(&mut self, event: &GoalEvent) -> Option<Audience> {
        match event {
            GoalEvent::Approved { file, .. } => {
                self.tagged.goal = std::iter::once(file.summary.clone())
                    .chain(
                        file.criteria
                            .iter()
                            .map(|criterion| format!("{}: {}", criterion.id, criterion.text)),
                    )
                    .collect();
                self.line(format!("Goal approved, version {}", file.version))
            }
            GoalEvent::ChangeResolved { id, resolution, .. } => {
                self.line(format!("Goal change {id}: {}", words(resolution)))
            }
            _ => None,
        }
    }

    fn plan(&mut self, event: &PlanEvent) -> Option<Audience> {
        match event {
            PlanEvent::Sealed {
                label,
                worker,
                round,
                summary,
                ..
            } => {
                self.drafted_round = self.drafted_round.max(*round);
                let audience = Audience::Draft {
                    label: *label,
                    round: *round,
                };
                self.push_line(Line {
                    audience,
                    text: format!("Plan {label} sealed: {summary}"),
                    with_authors: Some(format!("Plan {label} ({worker}) sealed: {summary}")),
                })
            }
            PlanEvent::Mapped { round, topics, .. } => {
                self.all_sealed(*round);
                let disputed = topics.iter().filter(|topic| topic.disputed).count();
                self.line(format!("Drafts mapped: {disputed} disputed topics"))
            }
            PlanEvent::FastPath { .. } => {
                self.all_sealed(self.drafted_round);
                self.line("No disputed topics, so the drafts were merged".to_owned())
            }
            PlanEvent::Critiqued { .. }
            | PlanEvent::SpikeProposed { .. }
            | PlanEvent::Scored { .. } => {
                self.all_sealed(self.drafted_round);
                Some(Audience::Everyone)
            }
            PlanEvent::SpikeFinished { card, report } => self.line(format!(
                "{card} spike on {}: {}",
                report.topic, report.finding
            )),
            PlanEvent::Judged(result) => {
                self.all_sealed(self.drafted_round);
                self.judged(result)
            }
            PlanEvent::Decided(decision) => {
                self.tagged.decisions.push((
                    decision.id.clone(),
                    format!("{}: {}", decision.title, decision.choice),
                ));
                Some(Audience::Everyone)
            }
            PlanEvent::Coordinated { rejection, .. } => match rejection {
                None => self.line("The coordinator's plan was accepted".to_owned()),
                Some(rule) => self.line(format!("The coordinator's plan was rejected: {rule}")),
            },
        }
    }

    fn judged(&mut self, result: &JudgingResult) -> Option<Audience> {
        if result.near_tie {
            return self.line("Judging ended in a near tie, which you break".to_owned());
        }
        let winner = result.winner?;
        let author = result
            .plans
            .iter()
            .find(|tally| tally.label == winner)
            .map(|tally| tally.author.to_string());
        let margin = result.margin;
        self.push_line(Line {
            audience: Audience::Everyone,
            text: format!("Plan {winner} won by {margin:.2}"),
            with_authors: author
                .map(|author| format!("Plan {winner} ({author}) won by {margin:.2}")),
        })
    }

    fn card(&mut self, id: &CardId, event: &CardEvent) -> Option<Audience> {
        match event {
            CardEvent::Created(spec) => Some(self.created(spec)),
            CardEvent::State { to, reason, .. } => {
                let known = self.card_mut(id)?;
                known.brief.state = *to;
                // A waiting card's note lists what it waits for; the refresh below writes it.
                known.brief.note = match to {
                    CardState::Blocked
                    | CardState::NeedsDecision
                    | CardState::Failed
                    | CardState::Escalated
                    | CardState::Deferred => reason.clone(),
                    _ => String::new(),
                };
                let audience = known.audience.clone();
                self.refresh_waiting_notes();
                match to {
                    CardState::Integrated
                    | CardState::Failed
                    | CardState::Escalated
                    | CardState::NeedsDecision
                    | CardState::Blocked => {
                        let reason = if reason.is_empty() {
                            String::new()
                        } else {
                            format!(": {reason}")
                        };
                        self.card_line(audience, format!("{id} {}{reason}", words(to)))
                    }
                    _ => Some(audience),
                }
            }
            CardEvent::Attempt {
                attempt, worker, ..
            } if *attempt > 1 => {
                let audience = self.card_mut(id)?.audience.clone();
                self.card_line(audience, format!("{id} attempt {attempt} on {worker}"))
            }
            CardEvent::Reported(report) => {
                let audience = self.card_mut(id)?.audience.clone();
                // The summary stays out: reviewers never see the owner's notes.
                self.card_line(audience, format!("{id} reported {}", words(&report.status)))
            }
            CardEvent::Reassigned { to, .. } => {
                let known = self.card_mut(id)?;
                known.brief.owner = Some(to.clone());
                let audience = known.audience.clone();
                self.card_line(audience, format!("{id} reassigned to {to}"))
            }
            CardEvent::TakenOver { .. } => {
                let audience = self.card_mut(id)?.audience.clone();
                self.card_line(audience, format!("{id} taken over by you"))
            }
            CardEvent::TakeOverEnded { .. } => {
                let audience = self.card_mut(id)?.audience.clone();
                self.card_line(audience, format!("{id} handed back after your take over"))
            }
            CardEvent::Reverted { to, .. } => {
                let audience = self.card_mut(id)?.audience.clone();
                self.card_line(audience, format!("{id} reverted to handoff-{to}"))
            }
            _ => None,
        }
    }

    fn created(&mut self, spec: &CardSpec) -> Audience {
        let hidden = !spec.hidden_paths.is_empty()
            && spec
                .owned_paths
                .iter()
                .all(|owned| spec.hidden_paths.iter().any(|path| covers(path, owned)));
        self.hidden.extend(spec.hidden_paths.iter().cloned());
        let audience = if hidden {
            Audience::UserOnly
        } else {
            Audience::Everyone
        };
        let card = Card {
            brief: CardBrief {
                card: spec.id.clone(),
                title: spec.title.clone(),
                state: CardState::Waiting,
                owner: spec.owner.clone(),
                note: String::new(),
            },
            audience: audience.clone(),
            depends_on: spec.depends_on.clone(),
        };
        match self
            .tagged
            .cards
            .iter_mut()
            .find(|known| known.brief.card == spec.id)
        {
            Some(known) => *known = card,
            None => self.tagged.cards.push(card),
        }
        self.refresh_waiting_notes();
        audience
    }

    /// "needs card-002 and card-003" on each waiting card, for dependencies not integrated yet.
    fn refresh_waiting_notes(&mut self) {
        let integrated: Vec<CardId> = self
            .tagged
            .cards
            .iter()
            .filter(|card| card.brief.state == CardState::Integrated)
            .map(|card| card.brief.card.clone())
            .collect();
        for card in &mut self.tagged.cards {
            if card.brief.state != CardState::Waiting {
                continue;
            }
            let missing: Vec<String> = card
                .depends_on
                .iter()
                .filter(|dependency| !integrated.contains(dependency))
                .map(ToString::to_string)
                .collect();
            card.brief.note = if missing.is_empty() {
                String::new()
            } else {
                format!("needs {}", and_list(&missing))
            };
        }
    }

    fn gate(&mut self, card: Option<&CardId>, outcome: &GateOutcome) -> Option<Audience> {
        let verdict = if outcome.passed { "passed" } else { "failed" };
        let scope = card.map(|card| format!("{card} ")).unwrap_or_default();
        let text = format!(
            "{scope}gate {} {verdict}: {}",
            outcome.name, outcome.summary
        );
        let audience = card
            .and_then(|card| self.card_mut(card))
            .map(|known| known.audience.clone())
            .unwrap_or(Audience::Everyone);
        self.card_line(audience, text)
    }

    fn review(&mut self, card: &CardId, event: &ReviewEvent) -> Option<Audience> {
        match event {
            ReviewEvent::Reviewed {
                reviewer,
                blind,
                review,
                ..
            } => {
                let audience = if *blind {
                    Audience::BlindReview(card.clone())
                } else {
                    Audience::Everyone
                };
                self.push_line(Line {
                    audience,
                    text: format!("{card} reviewed by {reviewer}: {}", words(&review.verdict)),
                    with_authors: None,
                })
            }
            ReviewEvent::Adjudicated { ruling, .. } => {
                self.tagged.moment.adjudicated.insert(card.clone());
                self.line(format!("{card} adjudicated: {}", words(&ruling.verdict)))
            }
            ReviewEvent::TestsWritten {
                reviewer,
                for_card,
                files,
            } => self.line(format!(
                "{reviewer} wrote {} acceptance test files for {for_card}",
                files.len()
            )),
        }
    }

    fn integration(&mut self, event: &IntegrationEvent) -> Option<Audience> {
        let text = match event {
            IntegrationEvent::BranchCreated { branch, .. } => {
                format!("Integration branch {branch} created")
            }
            IntegrationEvent::Merged { card, .. } => {
                format!("{card} merged into the integration branch")
            }
            IntegrationEvent::Conflict {
                card,
                files,
                assigned_to,
            } => format!(
                "{card} conflicts in {} files, assigned to {assigned_to}",
                files.len()
            ),
            IntegrationEvent::MainMoved { commits, .. } => {
                format!("main moved {commits} commits since the run started")
            }
            IntegrationEvent::MainMergedIn { .. } => {
                "main merged into the integration branch".to_owned()
            }
            IntegrationEvent::FinalCheck { passed: true } => "The final check passed".to_owned(),
            IntegrationEvent::FinalCheck { passed: false } => "The final check failed".to_owned(),
            IntegrationEvent::Landed { target, .. } => format!("Landed on {target}"),
            IntegrationEvent::PullRequest { url: Some(url), .. } => {
                format!("Pull request opened: {url}")
            }
            IntegrationEvent::PullRequest { branch, .. } => {
                format!("A pull request for {branch} is ready to open")
            }
            IntegrationEvent::Abandoned { .. } => "The run was abandoned, branches kept".to_owned(),
            IntegrationEvent::DependenciesAdded { packages, .. } => {
                format!("Added {}", and_list(packages))
            }
        };
        self.line(text)
    }

    fn checkpoint(&mut self, event: &CheckpointEvent) -> Option<Audience> {
        match event {
            CheckpointEvent::Opened(opened) => {
                if let CheckpointPacket::Plan(packet) = &opened.packet {
                    self.all_sealed(self.drafted_round);
                    self.proposed_plan = Some((opened.id.clone(), packet.summary.clone()));
                }
                self.tagged
                    .waiting
                    .push((opened.id.clone(), opened.title.clone()));
                Some(Audience::Everyone)
            }
            CheckpointEvent::Decided(decided) => {
                let title = self.close(&decided.id);
                let approved = matches!(
                    decided.action,
                    AnswerAction::Approve | AnswerAction::ApproveWithEdits
                );
                if let Some((id, summary)) = self.proposed_plan.take() {
                    if id != decided.id {
                        self.proposed_plan = Some((id, summary));
                    } else if approved {
                        self.tagged.plan = Some(summary);
                    }
                }
                let title = title.unwrap_or_else(|| format!("Checkpoint {}", decided.id));
                self.line(format!("{title}: {}", words(&decided.action)))
            }
            CheckpointEvent::Withdrawn { id, .. } => {
                self.close(id);
                Some(Audience::Everyone)
            }
            CheckpointEvent::Checked(_) | CheckpointEvent::SpotCheckQueued { .. } => None,
        }
    }

    /// Takes a checkpoint off the waiting list and returns its title.
    fn close(&mut self, id: &CheckpointId) -> Option<String> {
        let at = self
            .tagged
            .waiting
            .iter()
            .position(|(open, _)| open == id)?;
        Some(self.tagged.waiting.remove(at).1)
    }

    fn handoff(&mut self, event: &HandoffEvent) -> Option<Audience> {
        match event {
            HandoffEvent::Needed(plan) => {
                self.line(format!("{} stopped: {}", plan.card, plan.reason))
            }
            HandoffEvent::HandedOff { card, to, .. } => {
                if let Some(known) = self.card_mut(card) {
                    known.brief.owner = Some(to.clone());
                }
                self.line(format!("{card} handed off to {to}"))
            }
            HandoffEvent::Waiting { card, vendor, .. } => {
                self.line(format!("{card} waits for {vendor}"))
            }
            HandoffEvent::Packet { .. } => None,
        }
    }

    fn vendor(&mut self, event: &VendorEvent) -> Option<Audience> {
        match event {
            VendorEvent::Quota(reading) => {
                match self
                    .tagged
                    .quotas
                    .iter_mut()
                    .find(|known| known.vendor == reading.vendor)
                {
                    Some(known) => *known = reading.clone(),
                    None => self.tagged.quotas.push(reading.clone()),
                }
                Some(Audience::Everyone)
            }
            VendorEvent::Disabled { vendor, reason } => {
                self.line(format!("{vendor} disabled for new cards: {reason}"))
            }
            VendorEvent::Health(_) => None,
        }
    }

    fn budget(&mut self, event: &BudgetEvent) -> Option<Audience> {
        match event {
            BudgetEvent::Run(budget) => {
                self.tagged.budget = Some(budget.clone());
                Some(Audience::Everyone)
            }
            BudgetEvent::Hit {
                card, dimension, ..
            } => self.line(format!("{card} hit its {} budget", words(dimension))),
            BudgetEvent::Extended { card, .. } => self.line(format!("{card} got more budget")),
            BudgetEvent::Deferred { cards } => {
                let cards: Vec<String> = cards.iter().map(ToString::to_string).collect();
                self.line(format!(
                    "Deferred until budget frees up: {}",
                    and_list(&cards)
                ))
            }
        }
    }

    fn all_sealed(&mut self, round: u8) {
        let moment = &mut self.tagged.moment;
        moment.sealed_through = moment.sealed_through.max(round);
    }

    fn card_mut(&mut self, id: &CardId) -> Option<&mut Card> {
        self.tagged
            .cards
            .iter_mut()
            .find(|card| &card.brief.card == id)
    }

    fn line(&mut self, text: String) -> Option<Audience> {
        self.card_line(Audience::Everyone, text)
    }

    fn card_line(&mut self, audience: Audience, text: String) -> Option<Audience> {
        self.push_line(Line {
            audience,
            text,
            with_authors: None,
        })
    }

    fn push_line(&mut self, line: Line) -> Option<Audience> {
        let line = self.guarded(line);
        let audience = line.audience.clone();
        self.tagged.recent.push_back(line);
        while self.tagged.recent.len() > RECENT_LINES {
            self.tagged.recent.pop_front();
        }
        Some(audience)
    }

    /// A line that names a hidden path is for you only.
    fn guarded(&self, mut line: Line) -> Line {
        let named = line.with_authors.as_deref().unwrap_or_default();
        if self.names_hidden(&line.text) || self.names_hidden(named) {
            line.audience = Audience::UserOnly;
        }
        line
    }

    fn names_hidden(&self, text: &str) -> bool {
        self.hidden.iter().any(|pattern| {
            let fixed = literal_prefix(pattern);
            !fixed.is_empty() && text.contains(fixed)
        })
    }
}

/// The part of a glob before its first wildcard, without a trailing `/`.
fn literal_prefix(pattern: &str) -> &str {
    let end = pattern.find(['*', '?', '[', '{']).unwrap_or(pattern.len());
    pattern[..end].trim_end_matches('/')
}

/// Whether the hidden pattern covers this owned path or pattern.
fn covers(hidden: &str, owned: &str) -> bool {
    if hidden == owned {
        return true;
    }
    let fixed = literal_prefix(hidden);
    !fixed.is_empty() && literal_prefix(owned).starts_with(fixed)
}

/// A serialized enum's name in plain words, such as "needs decision".
pub(crate) fn words<T: Serialize>(value: &T) -> String {
    let name = match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        Ok(serde_json::Value::Object(fields)) => fields
            .values()
            .find_map(|field| field.as_str().map(str::to_owned))
            .unwrap_or_default(),
        _ => String::new(),
    };
    name.replace(['_', '-'], " ")
}

fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::OffsetDateTime;

    use argonauts_core::events::workflow::{StepFinished, StepStarted, WorkflowStarted};
    use argonauts_core::names::{RoleName, WorkflowId};
    use argonauts_core::workflow::{StandardPhase, StepKind, StepRef, Workflow};

    use super::*;

    fn event(seq: u64, kind: WorkflowEvent) -> RunEvent {
        RunEvent {
            seq,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new("run-01").unwrap(),
            card_id: None,
            session: None,
            source: "conductor".to_owned(),
            kind: RunEventKind::Workflow(kind),
        }
    }

    fn at(workflow: &WorkflowId, phase: u32, index: u32) -> StepRef {
        StepRef {
            workflow: workflow.clone(),
            phase,
            path: vec![index],
        }
    }

    fn started_workflow(
        id: &WorkflowId,
        parent: Option<StepRef>,
        workflow: Workflow,
    ) -> WorkflowEvent {
        WorkflowEvent::Started(Box::new(WorkflowStarted {
            id: id.clone(),
            from: None,
            version: "v1".to_owned(),
            parent,
            workflow,
        }))
    }

    fn started(
        step: StepRef,
        kind: StepKind,
        standard: StandardPhase,
        round: u32,
    ) -> WorkflowEvent {
        WorkflowEvent::StepStarted(StepStarted {
            step,
            kind,
            standard,
            round,
        })
    }

    fn finished(
        step: StepRef,
        kind: StepKind,
        standard: StandardPhase,
        end: StepEnd,
    ) -> WorkflowEvent {
        WorkflowEvent::StepFinished(StepFinished {
            step,
            kind,
            standard,
            round: 1,
            end,
            summary: Some("codex said the plan by claude is better".to_owned()),
        })
    }

    fn now(folder: &Folder, viewer: &Viewer) -> Option<String> {
        folder
            .summary(viewer)
            .recent
            .last()
            .filter(|line| line.starts_with("Now: "))
            .cloned()
    }

    /// Only runner steps, so no legacy phase event says where the run is: the workflow
    /// events alone give the standard phase, the phase, the step number, and the breadcrumb.
    #[test]
    fn status_follows_a_workflow_of_runner_steps() {
        let outer_id = WorkflowId::numbered(1);
        let inner_id = WorkflowId::numbered(2);
        let outer: Workflow = serde_json::from_value(json!({
            "name": "composed",
            "phases": [
                {"name": "Look", "standard": "check", "steps": [
                    {"kind": "gate"},
                    {"kind": "fan_out", "name": "Second opinions", "roles": ["reviewer"]},
                    {"kind": "review"},
                ]},
                {"name": "Settle", "standard": "decide", "steps": [
                    {"kind": "workflow", "workflow": "review-panel"},
                ]},
            ],
        }))
        .unwrap();
        let inner: Workflow = serde_json::from_value(json!({
            "name": "review-panel",
            "phases": [{"name": "Panel", "standard": "check", "steps": [{"kind": "review"}]}],
        }))
        .unwrap();
        let owner = Viewer::Session {
            role: RoleName::Owner,
            card: None,
            label: None,
        };
        let mut folder = Folder::default();
        folder.push(&event(1, started_workflow(&outer_id, None, outer)));
        assert_eq!(now(&folder, &Viewer::User), None);

        let gate = at(&outer_id, 0, 0);
        folder.push(&event(
            2,
            started(gate.clone(), StepKind::Gate, StandardPhase::Check, 1),
        ));
        assert!(folder.changed_for(&owner));
        assert_eq!(
            now(&folder, &owner).as_deref(),
            Some("Now: check · Look, step 1 of 3: gate")
        );
        folder.push(&event(
            3,
            finished(gate, StepKind::Gate, StandardPhase::Check, StepEnd::Done),
        ));
        assert_eq!(now(&folder, &owner), None);

        let fan_out = at(&outer_id, 0, 1);
        folder.push(&event(
            4,
            started(fan_out.clone(), StepKind::FanOut, StandardPhase::Check, 1),
        ));
        assert_eq!(
            now(&folder, &owner).as_deref(),
            Some("Now: check · Look, step 2 of 3: Second opinions")
        );
        folder.push(&event(
            5,
            finished(
                fan_out,
                StepKind::FanOut,
                StandardPhase::Check,
                StepEnd::Failed,
            ),
        ));
        let recent = folder.summary(&owner).recent;
        assert_eq!(recent, ["Look, step 2 of 3: Second opinions failed"]);

        let parent = at(&outer_id, 1, 0);
        folder.push(&event(
            6,
            started(parent.clone(), StepKind::Workflow, StandardPhase::Decide, 1),
        ));
        folder.push(&event(7, started_workflow(&inner_id, Some(parent), inner)));
        assert_eq!(
            now(&folder, &owner).as_deref(),
            Some("Now: decide · Settle, step 1 of 1: workflow")
        );
        folder.push(&event(
            8,
            started(
                at(&inner_id, 0, 0),
                StepKind::Review,
                StandardPhase::Check,
                2,
            ),
        ));
        assert_eq!(
            now(&folder, &Viewer::Narrator).as_deref(),
            Some("Now: check · Settle › review-panel › Panel, step 1 of 1: review, round 2")
        );
        // The step summaries never reach the status: they can name plan authors.
        let text = crate::status::render(&folder.summary(&owner));
        assert!(!text.contains("claude"), "{text}");
    }

    /// A step or phase named after a hidden path keeps the working line from agents.
    #[test]
    fn working_line_naming_hidden_paths_is_for_you() {
        let id = WorkflowId::numbered(1);
        let workflow: Workflow = serde_json::from_value(json!({
            "name": "hidden",
            "phases": [{"name": "Check tests/hidden", "standard": "check", "steps": [{"kind": "gate"}]}],
        }))
        .unwrap();
        let mut folder = Folder::default();
        folder.hidden.push("tests/hidden/**".to_owned());
        folder.push(&event(1, started_workflow(&id, None, workflow)));
        folder.push(&event(
            2,
            started(at(&id, 0, 0), StepKind::Gate, StandardPhase::Check, 1),
        ));
        let owner = Viewer::Session {
            role: RoleName::Owner,
            card: None,
            label: None,
        };
        assert_eq!(now(&folder, &owner), None);
        assert!(now(&folder, &Viewer::User).is_some());
    }

    #[test]
    fn words_reads_plain_and_tagged_enums() {
        assert_eq!(words(&CardState::NeedsDecision), "needs decision");
        assert_eq!(words(&AnswerAction::RequestChanges), "request changes");
    }

    #[test]
    fn hidden_patterns_cover_paths_below_them() {
        assert!(covers("tests/hidden/**", "tests/hidden/limits.rs"));
        assert!(covers("tests/hidden/**", "tests/hidden/**"));
        assert!(!covers("tests/hidden/**", "tests/public.rs"));
        assert_eq!(literal_prefix("tests/hidden/**"), "tests/hidden");
    }

    #[test]
    fn and_list_joins_like_a_sentence() {
        let items = ["a".to_owned(), "b".to_owned(), "c".to_owned()];
        assert_eq!(and_list(&items), "a, b and c");
        assert_eq!(and_list(&items[..1]), "a");
    }
}
