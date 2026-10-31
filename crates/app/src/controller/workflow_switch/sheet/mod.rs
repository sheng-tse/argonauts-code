//! What a workflow's setup sheet holds while it is open, the sheet drawn from it, and the
//! settings Enter writes. Captain, Orchestrator and the workflows of steps get a row a step:
//! who fills it, from the models that are ready now. Council gets its seats, the Checks rung,
//! and How tasks are done, which Orchestrator has too.
//!
//! Every change is made here, on the draft, and the whole sheet goes to the screen again, so
//! the rows that follow a step follow it at once.

use argonauts_core::app::setup_sheet::{
    AsideRow, FillRow, Part, Pointer, SetupSheet, SheetColumn, SheetLayout, SheetRow, Tone,
    UnderLine, WayRow, Words,
};
use argonauts_core::base::names::ModelRef;
use argonauts_core::look::logos::{Logo, logo_for};
use argonauts_core::runs::events::council::SeatRole;
use argonauts_core::settings::Settings;
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::settings::ways::{MostAgents, Ways, WorkflowsAllowed};

use super::{CAPTAIN, COUNCIL, Now, ORCHESTRATOR, Present, title};

/// The most seats a council takes.
const MOST_SEATS: usize = 6;
/// The notes' measure, so a long note breaks where the sheet's pictures break it.
const NOTE_WIDTH: usize = 110;
const PICKS: &str = "The orchestrator picks";

/// What the sheet knows of the agents: who can take a turn, and every model they offer.
#[derive(Debug, Clone, Default)]
pub(crate) struct Facts {
    pub present: Present,
    /// The models of the CLIs and endpoints set up here, in the pickers' order.
    pub models: Vec<Model>,
    /// Members of the crew whose plan ran out, and when it comes back, as "3:40 PM".
    pub out_of_plan: Vec<(ModelRef, Option<String>)>,
    /// The model each CLI runs when a seat or step names none, by its name.
    pub runs: std::collections::BTreeMap<String, String>,
    /// The workflows a list of ways may name, by name.
    pub workflows: Vec<String>,
    /// `workflows.max_depth` is 0, so no task may run a workflow at all.
    pub no_nesting: bool,
}

/// One model as the sheet names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Model {
    pub worker: ModelRef,
    pub name: String,
    pub provider: String,
}

impl Facts {
    fn ready(&self, worker: &ModelRef) -> bool {
        self.present.ready(&worker.agent)
    }

    /// The models that can take a turn now.
    fn ready_models(&self) -> Vec<&Model> {
        self.models
            .iter()
            .filter(|model| self.ready(&model.worker))
            .collect()
    }

    fn model(&self, worker: &ModelRef) -> Option<&Model> {
        self.models.iter().find(|model| model.worker == *worker)
    }

    /// `Opus 5.5`; a CLI named without a model by the model it runs.
    fn name(&self, worker: &ModelRef) -> String {
        if worker.model.is_none()
            && let Some(runs) = self.runs.get(&worker.agent)
        {
            return runs.clone();
        }
        self.model(worker).map_or_else(
            || match &worker.model {
                Some(model) => model.clone(),
                None => self.present.name(&worker.agent),
            },
            |model| model.name.clone(),
        )
    }

    /// Who makes it: the model's provider, or for a CLI named without a model, its models'.
    fn provider(&self, worker: &ModelRef) -> String {
        self.model(worker)
            .or_else(|| {
                self.models
                    .iter()
                    .find(|model| model.worker.agent == worker.agent)
            })
            .map_or_else(|| worker.agent.clone(), |model| model.provider.clone())
    }

    /// Whether two seats run one model: a CLI named alone runs the model it runs by default.
    fn same(&self, a: &ModelRef, b: &ModelRef) -> bool {
        a == b || (a.agent == b.agent && self.name(a) == self.name(b))
    }

    fn seated(&self, seats: &[(ModelRef, SeatRole)], worker: &ModelRef) -> bool {
        seats.iter().any(|(seated, _)| self.same(seated, worker))
    }

    fn named(&self, worker: &ModelRef) -> Part {
        Part::named(logo_of(worker), self.name(worker))
    }
}

fn logo_of(worker: &ModelRef) -> Logo {
    logo_for(&worker.agent, worker.model.as_deref())
}

/// Who fills a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fill {
    Models(Vec<ModelRef>),
    /// Takes the models of the step it follows: workers the orchestrator's, the judge the
    /// reviewers'.
    Follows(CrewRole),
    /// The orchestrator picks a ready model for each task.
    Picks,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Step {
    pub id: String,
    pub label: String,
    pub role: CrewRole,
    pub fill: Fill,
    /// You changed it on this sheet.
    pub picked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Body {
    Steps(Vec<Step>),
    Seats(Vec<(ModelRef, SeatRole)>),
}

/// The sheet while it is open: what Enter would save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Draft {
    pub workflow: String,
    pub body: Body,
    /// How tasks are done, on the sheets that pick a way per task.
    pub ways: Option<Ways>,
    /// The workflows the Workflows line lists when it isn't Any, kept while the line is Any.
    pub listed: Vec<String>,
    /// The row last changed, for the line under the title.
    pub last: Option<String>,
    /// Where the pointer goes once: on what needs you as the sheet opens.
    pub pointer: Option<Pointer>,
    /// Crew members who would sit by default but couldn't take a turn as the sheet opened. One
    /// that logs in meanwhile takes its seat.
    waiting_seats: Vec<ModelRef>,
    /// The settings the sheet opened on, against which Enter writes only what changed.
    before: Box<(Body, Option<Ways>)>,
}

impl Draft {
    /// The sheet as your settings fill it. For `steps`, the roles a workflow's own steps name.
    pub(crate) fn new(
        settings: &Settings,
        workflow: &str,
        facts: &Facts,
        steps: Option<&[(String, CrewRole)]>,
    ) -> Self {
        let waiting_seats = match workflow {
            COUNCIL if settings.council.seats.is_empty() => super::seats(settings)
                .into_iter()
                .map(|(worker, _)| worker)
                .filter(|worker| !facts.ready(worker))
                .collect(),
            _ => Vec::new(),
        };
        let body = match workflow {
            COUNCIL => Body::Seats(ready_seats(settings, facts)),
            CAPTAIN => {
                let mut steps = vec![Step {
                    id: "main".into(),
                    label: "Main".into(),
                    role: CrewRole::Main,
                    fill: Fill::Models(vec![settings.crew.main.clone()]),
                    picked: false,
                }];
                if settings.captain.review {
                    steps.push(step(settings, "Review", CrewRole::Reviewer, false));
                }
                Body::Steps(steps)
            }
            _ => {
                let orchestrator = workflow == ORCHESTRATOR;
                Body::Steps(
                    steps
                        .unwrap_or_default()
                        .iter()
                        .map(|(label, role)| step(settings, label, *role, orchestrator))
                        .collect(),
                )
            }
        };
        let ways = match workflow {
            COUNCIL => Some(settings.council.ways.clone()),
            ORCHESTRATOR => Some(settings.crew.ways.clone()),
            _ => None,
        };
        let listed = match ways.as_ref().map(|ways| &ways.workflows) {
            Some(WorkflowsAllowed::These(names)) if !names.is_empty() => names.clone(),
            _ => Vec::new(),
        };
        let mut draft = Self {
            workflow: workflow.to_owned(),
            before: Box::new((body.clone(), ways.clone())),
            body,
            ways,
            listed,
            last: None,
            pointer: None,
            waiting_seats,
        };
        draft.pointer = draft.needs_you(facts);
        draft
    }

    /// The logins were read again: a crew member that logged in takes the seat it would have
    /// had, as long as a seat is free.
    pub(crate) fn learn(&mut self, facts: &Facts) {
        let Body::Seats(seats) = &mut self.body else {
            return;
        };
        let (ready, waiting): (Vec<ModelRef>, Vec<ModelRef>) =
            std::mem::take(&mut self.waiting_seats)
                .into_iter()
                .partition(|worker| facts.ready(worker));
        self.waiting_seats = waiting;
        for worker in ready {
            if seats.len() < MOST_SEATS && !facts.seated(seats, &worker) {
                seats.push((worker, SeatRole::Proposer));
            }
        }
    }

    /// The row the pointer starts on: the first that isn't ready, else Add one while fewer than
    /// two seats can answer, else Checks when only one maker can check.
    fn needs_you(&self, facts: &Facts) -> Option<Pointer> {
        let at = |row: &str| {
            Some(Pointer {
                row: row.to_owned(),
                column: SheetColumn::Who,
            })
        };
        match &self.body {
            Body::Steps(steps) => steps
                .iter()
                .find(|step| !self.step_ready(step, facts).0)
                .and_then(|step| at(&step.id)),
            Body::Seats(seats) => {
                if let Some(at_seat) = seats.iter().position(|(worker, _)| !facts.ready(worker)) {
                    return at(&seat_id(at_seat));
                }
                if seats.len() < 2 {
                    return at("add");
                }
                matches!(checks_rung(seats, facts), Rung::SameMaker(_))
                    .then(|| at("checks"))
                    .flatten()
            }
        }
    }

    /// Whether Enter may save: every step filled by an agent that's ready, or two seats that
    /// can answer and not all skeptics. `Err` says why not.
    pub(crate) fn ready(&self, facts: &Facts) -> Result<(), String> {
        match &self.body {
            Body::Steps(steps) => {
                for step in steps {
                    match self.step_ready(step, facts) {
                        (true, _) => {}
                        (false, Some((have, needs))) => {
                            let have = match have {
                                0 => "none is".to_owned(),
                                1 => "only one is".to_owned(),
                                n => format!("only {n} are"),
                            };
                            return Err(format!(
                                "{} needs {needs} ready agents of different CLIs, and {have}",
                                step.label
                            ));
                        }
                        (false, None) => {
                            return Err(format!("{} needs a ready agent", step.label));
                        }
                    }
                }
                Ok(())
            }
            Body::Seats(seats) => {
                let answering: Vec<_> = seats
                    .iter()
                    .filter(|(worker, _)| facts.ready(worker))
                    .collect();
                if answering.len() < 2 {
                    return Err("Council needs two seats that can answer".to_owned());
                }
                if answering.iter().all(|(_, role)| *role == SeatRole::Skeptic) {
                    return Err("Council needs a seat that isn't a skeptic".to_owned());
                }
                if seats.iter().any(|(worker, _)| !facts.ready(worker)) {
                    return Err("A seat's agent isn't ready: drop it with space".to_owned());
                }
                Ok(())
            }
        }
    }

    /// Whether a step is filled now, and how many it needs and has where it counts.
    fn step_ready(&self, step: &Step, facts: &Facts) -> (bool, Option<(usize, usize)>) {
        let needs = self.needs_for(step.role);
        let have = match self.filled_by(step) {
            Fill::Models(models) => distinct_ready(&models, facts),
            Fill::Picks => facts
                .ready_models()
                .iter()
                .map(|model| model.worker.agent.clone())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            Fill::Follows(_) => 0,
        };
        (
            have >= needs,
            (needs > 1).then_some((have.min(needs), needs)),
        )
    }

    /// Two reviewers per task on Orchestrator, one for every other step.
    fn needs_for(&self, role: CrewRole) -> usize {
        match (self.workflow.as_str(), role) {
            (ORCHESTRATOR, CrewRole::Reviewer) => 2,
            _ => 1,
        }
    }

    /// The models a step ends up with, through what it follows.
    fn filled_by(&self, step: &Step) -> Fill {
        let Body::Steps(steps) = &self.body else {
            return step.fill.clone();
        };
        let mut fill = step.fill.clone();
        for _ in 0..steps.len() {
            let Fill::Follows(role) = fill else {
                return fill;
            };
            match steps.iter().find(|other| other.role == role) {
                Some(other) => fill = other.fill.clone(),
                None => return Fill::Models(Vec::new()),
            }
        }
        fill
    }

    /// ← or → on `row`'s `column`.
    pub(crate) fn change(&mut self, row: &str, column: SheetColumn, forward: bool, facts: &Facts) {
        self.last = Some(row.to_owned());
        self.pointer = None;
        if let Some(way) = row.strip_prefix("ways.") {
            self.change_way(way, forward, facts);
            return;
        }
        let workflow = self.workflow.clone();
        match &mut self.body {
            Body::Steps(steps) => {
                let Some(step) = steps.iter_mut().find(|step| step.id == row) else {
                    return;
                };
                let options = step_options(&workflow, step.role, facts);
                let Some(next) = cycle(&options, &step.fill, forward) else {
                    return;
                };
                let was = std::mem::replace(&mut step.fill, next);
                step.picked = true;
                // The judge that took the reviewers' list follows the reviewers, as before.
                if step.role == CrewRole::Reviewer
                    && let Some(judge) = steps
                        .iter_mut()
                        .find(|step| step.role == CrewRole::Judge && step.fill == was)
                {
                    judge.fill = Fill::Follows(CrewRole::Reviewer);
                }
            }
            Body::Seats(seats) => {
                if row == "checks" {
                    change_checks(seats, forward, facts);
                    return;
                }
                let Some(at) = seat_at(row) else {
                    return;
                };
                let Some((worker, role)) = seats.get_mut(at) else {
                    return;
                };
                match column {
                    SheetColumn::Role => {
                        let roles = [SeatRole::Proposer, SeatRole::Skeptic, SeatRole::Checker];
                        if let Some(next) = cycle(&roles, role, forward) {
                            *role = next;
                        }
                    }
                    SheetColumn::Who => {
                        let options: Vec<ModelRef> = facts
                            .ready_models()
                            .iter()
                            .map(|model| model.worker.clone())
                            .collect();
                        if let Some(next) = cycle(&options, worker, forward) {
                            *worker = next;
                        }
                    }
                }
            }
        }
    }

    /// Space on `row`: adds a seat or drops one, or allows a way or keeps it out.
    pub(crate) fn toggle(&mut self, row: &str, facts: &Facts) {
        self.last = Some(row.to_owned());
        self.pointer = None;
        if let Some(way) = row.strip_prefix("ways.") {
            self.toggle_way(way);
            return;
        }
        let Body::Seats(seats) = &mut self.body else {
            return;
        };
        if row == "add" {
            if seats.len() < MOST_SEATS
                && let Some(next) = next_seat(seats, facts)
            {
                seats.push((next, SeatRole::Proposer));
            }
            return;
        }
        if let Some(at) = seat_at(row).filter(|at| *at < seats.len()) {
            seats.remove(at);
            self.pointer = Some(Pointer {
                row: if seats.is_empty() {
                    "add".to_owned()
                } else {
                    seat_id(at.min(seats.len() - 1))
                },
                column: SheetColumn::Who,
            });
        }
    }

    fn change_way(&mut self, way: &str, forward: bool, facts: &Facts) {
        let Some(ways) = self.ways.as_mut() else {
            return;
        };
        match way {
            "workflows" => {
                ways.workflows = match &ways.workflows {
                    WorkflowsAllowed::Any(_) => {
                        if self.listed.is_empty() {
                            self.listed = facts.workflows.clone();
                        }
                        WorkflowsAllowed::These(self.listed.clone())
                    }
                    WorkflowsAllowed::These(names) => {
                        self.listed = names.clone();
                        WorkflowsAllowed::ANY
                    }
                };
            }
            "agents" if !ways.agents.is_off() => {
                let counts = [2u8, 3, 4];
                let now = ways.agents.get();
                if let Some(next) = cycle(&counts, &now, forward) {
                    ways.agents = MostAgents::try_from(next).unwrap_or(MostAgents::MOST);
                }
            }
            _ => {}
        }
    }

    fn toggle_way(&mut self, way: &str) {
        let Some(ways) = self.ways.as_mut() else {
            return;
        };
        match way {
            "own_session" => ways.own_session = !ways.own_session,
            "workflows" => {
                ways.workflows = if ways.workflows.is_off() {
                    WorkflowsAllowed::ANY
                } else {
                    if let WorkflowsAllowed::These(names) = &ways.workflows {
                        self.listed = names.clone();
                    }
                    WorkflowsAllowed::These(Vec::new())
                };
            }
            "agents" => {
                ways.agents = if ways.agents.is_off() {
                    MostAgents::MOST
                } else {
                    MostAgents::NONE
                };
            }
            other => {
                let Some(name) = other.strip_prefix("workflow.") else {
                    return;
                };
                let WorkflowsAllowed::These(names) = &mut ways.workflows else {
                    return;
                };
                match names.iter().position(|listed| listed == name) {
                    Some(at) => {
                        names.remove(at);
                    }
                    None => names.push(name.to_owned()),
                }
            }
        }
    }

    /// Who does what, as the line saying the workflow is in use already ends it: `Opus 5.5 and
    /// GPT 5.6 propose, Gemini 3.6 plays the skeptic`, or `Opus 5.5 plans and builds, Codex
    /// and agy review and sign off`. A step that follows another is filled as that one is.
    pub(crate) fn who(&self, facts: &Facts) -> Option<String> {
        let said: Vec<String> = match &self.body {
            Body::Seats(seats) => [SeatRole::Proposer, SeatRole::Skeptic, SeatRole::Checker]
                .into_iter()
                .filter_map(|role| {
                    let sitting: Vec<ModelRef> = seats
                        .iter()
                        .filter(|(_, seated)| *seated == role)
                        .map(|(worker, _)| worker.clone())
                        .collect();
                    let does = match role {
                        SeatRole::Proposer => ("proposes", "propose"),
                        SeatRole::Skeptic => ("plays the skeptic", "play the skeptic"),
                        SeatRole::Checker => ("checks", "check"),
                    };
                    who_does(&sitting, facts, &[does])
                })
                .collect(),
            Body::Steps(steps) => {
                // Steps in a row filled alike are said together.
                let mut alike: Vec<(&Fill, Vec<CrewRole>)> = Vec::new();
                for step in steps {
                    let fill = match &step.fill {
                        Fill::Follows(role) => steps
                            .iter()
                            .find(|followed| followed.role == *role)
                            .map_or(&step.fill, |followed| &followed.fill),
                        fill => fill,
                    };
                    match alike.last_mut() {
                        Some((last, roles)) if *last == fill => roles.push(step.role),
                        _ => alike.push((fill, vec![step.role])),
                    }
                }
                alike
                    .into_iter()
                    .filter_map(|(fill, roles)| {
                        let verbs: Vec<(&str, &str)> = roles.into_iter().map(step_verb).collect();
                        match fill {
                            Fill::Models(models) => who_does(models, facts, &verbs),
                            Fill::Picks => {
                                let who: Vec<String> =
                                    verbs.iter().map(|(one, _)| format!("who {one}")).collect();
                                Some(format!("the orchestrator picks {}", and_list(&who)))
                            }
                            Fill::Follows(_) => None,
                        }
                    })
                    .collect()
            }
        };
        (!said.is_empty()).then(|| said.join(", "))
    }

    /// The settings Enter writes, as `(key, TOML value)`: the workflow, then what changed.
    pub(crate) fn pairs(&self) -> Vec<(String, String)> {
        let mut pairs = vec![("workflow".to_owned(), quoted(&self.workflow))];
        let (before_body, before_ways) = &*self.before;
        match (&self.body, before_body) {
            (Body::Steps(steps), Body::Steps(before)) => {
                for step in steps {
                    let was = before.iter().find(|old| old.id == step.id);
                    if was.is_some_and(|was| was.fill == step.fill) {
                        continue;
                    }
                    pairs.extend(step_pairs(step));
                }
            }
            (Body::Seats(seats), Body::Seats(before)) if seats != before => {
                pairs.push(("council.seat".to_owned(), seat_list(seats)));
            }
            _ => {}
        }
        if let (Some(ways), Some(before)) = (&self.ways, before_ways)
            && ways != before
        {
            let prefix = if self.workflow == COUNCIL {
                "council"
            } else {
                "crew"
            };
            if ways.own_session != before.own_session {
                pairs.push((
                    format!("{prefix}.ways.own_session"),
                    ways.own_session.to_string(),
                ));
            }
            if ways.workflows != before.workflows {
                let value = match &ways.workflows {
                    WorkflowsAllowed::Any(_) => quoted("any"),
                    WorkflowsAllowed::These(names) => toml_list(names.iter().map(String::as_str)),
                };
                pairs.push((format!("{prefix}.ways.workflows"), value));
            }
            if ways.agents != before.agents {
                pairs.push((
                    format!("{prefix}.ways.agents"),
                    ways.agents.get().to_string(),
                ));
            }
        }
        pairs
    }

    /// The sheet as the screen draws it now.
    pub(crate) fn sheet(&self, facts: &Facts, saves_to: &str, refresh: bool) -> SetupSheet {
        let ready = self.ready(facts);
        let (layout, mut rows, about, notes, keys) = match &self.body {
            Body::Seats(seats) => {
                let (rows, notes) = seats_rows(seats, facts);
                (
                    SheetLayout::Seats,
                    rows,
                    vec![
                        "Each seat answers alone first, then they talk until every seat agrees, \
                         then they split the work."
                            .to_owned(),
                        "Seats see each other only as Seat letters, never a name, a maker or a \
                         CLI. You see real names."
                            .to_owned(),
                    ],
                    notes,
                    "space adds or drops · tab to the next column · ←  →  to change · ↑  ↓  to \
                     move · enter to switch · esc to leave it"
                        .to_owned(),
                )
            }
            Body::Steps(steps) => {
                let rows = steps
                    .iter()
                    .map(|step| SheetRow::Fill(self.step_row(step, facts)))
                    .collect();
                let keys = if self
                    .last
                    .as_deref()
                    .is_some_and(|row| row.starts_with("ways."))
                {
                    format!(
                        "space allows it or not · ←  →  any or a list · ↑  ↓  step · enter to \
                         switch to {} · esc to leave it",
                        title(&self.workflow)
                    )
                } else {
                    format!(
                        "←  →  who · ↑  ↓  step · enter to switch to {} · ctrl+s for this \
                         session · esc to leave it",
                        title(&self.workflow)
                    )
                };
                (
                    SheetLayout::Steps,
                    rows,
                    vec![about_steps(&self.workflow)],
                    Vec::new(),
                    keys,
                )
            }
        };
        let mut notes = notes;
        if let Some(ways) = &self.ways {
            rows.push(SheetRow::Blank);
            rows.extend(ways_rows(ways, &self.listed, facts));
            let who = match self.workflow.as_str() {
                COUNCIL => "The note keeper",
                _ => "The orchestrator",
            };
            notes.push(format!(
                "{who} picks a way for each task from these and gives its reason in a few words."
            ));
            notes.push(
                "A list of workflows means any of them. A way you ask for in your prompt, or \
                 pick at the plan, decides that task."
                    .to_owned(),
            );
        }
        SetupSheet {
            workflow: self.workflow.clone(),
            title: format!("Set up {}", title(&self.workflow)),
            about,
            status: self.status(facts, saves_to, &ready),
            layout,
            rows,
            notes,
            keys,
            pointer: self.pointer.clone(),
            ready: ready.is_ok(),
            not_yet: ready.err(),
            refresh,
        }
    }

    /// The line under what the workflow does: what the last change did, or what it waits for.
    fn status(&self, facts: &Facts, saves_to: &str, ready: &Result<(), String>) -> Vec<Words> {
        if let Err(why) = ready {
            return vec![Words::plain(format!("{why}."))];
        }
        let Body::Steps(steps) = &self.body else {
            return Vec::new();
        };
        let picks = |role: CrewRole| {
            steps
                .iter()
                .any(|step| step.role == role && step.fill == Fill::Picks)
        };
        match self.last.as_deref() {
            Some(row) if row.starts_with("ways.") => {
                let all = self
                    .ways
                    .as_ref()
                    .is_some_and(|ways| *ways == Ways::default());
                if all {
                    vec![
                        Words::plain("Every way is "),
                        Words::new("allowed", Tone::Ok),
                        Words::plain(
                            ", as by default. Space keeps one out of every plan; Enter saves and \
                             switches.",
                        ),
                    ]
                } else {
                    vec![Words::plain(
                        "Some ways are kept out of every plan. Enter saves and switches.",
                    )]
                }
            }
            Some(_) if picks(CrewRole::Worker) && picks(CrewRole::Reviewer) => vec![
                Words::plain("The orchestrator now "),
                Words::new("picks", Tone::Ok),
                Words::plain(
                    " who builds and who reviews each task. Enter saves both and switches.",
                ),
            ],
            Some(_) if picks(CrewRole::Worker) || picks(CrewRole::Reviewer) => {
                let who = if picks(CrewRole::Worker) {
                    "builds"
                } else {
                    "reviews"
                };
                vec![
                    Words::plain("The orchestrator now "),
                    Words::new("picks", Tone::Ok),
                    Words::plain(format!(
                        " who {who} each task. Enter saves it to {saves_to} and switches."
                    )),
                ]
            }
            Some(row) => {
                let Some(step) = steps.iter().find(|step| step.id == row) else {
                    return Vec::new();
                };
                let who = match self.filled_by(step) {
                    Fill::Models(models) => models
                        .first()
                        .map(|worker| facts.present.name(&worker.agent))
                        .unwrap_or_default(),
                    _ => return Vec::new(),
                };
                vec![Words::plain(format!(
                    "{who} is picked for {}. Enter saves it to {saves_to} and switches.",
                    step.label
                ))]
            }
            None => Vec::new(),
        }
    }

    fn step_row(&self, step: &Step, facts: &Facts) -> FillRow {
        let (ready, count) = self.step_ready(step, facts);
        let state = match (ready, count) {
            (true, _) if step.picked && matches!(step.fill, Fill::Models(_)) => {
                Words::new("Picked", Tone::Ok)
            }
            (true, _) => Words::new("Ready", Tone::Ok),
            (false, Some((have, needs))) => Words::new(format!("{have} of {needs}"), Tone::Warn),
            (false, None) => Words::new("Not ready", Tone::Warn),
        };
        let (who, who_tone) = match &step.fill {
            Fill::Models(models) => (joined(models, facts, ", "), Tone::Text),
            Fill::Picks => (vec![Part::text(PICKS)], Tone::Text),
            Fill::Follows(_) => match self.filled_by(step) {
                Fill::Models(models) => (joined(&models, facts, ", "), Tone::Text),
                Fill::Picks => (vec![Part::text(PICKS)], Tone::Dim),
                Fill::Follows(_) => (Vec::new(), Tone::Dim),
            },
        };
        let what = match (&step.fill, step.role) {
            (Fill::Follows(CrewRole::Reviewer), _) => {
                vec![Words::dim("Follows"), Words::plain(" the reviewers")]
            }
            (Fill::Follows(_), _) => {
                vec![Words::dim("Follows"), Words::plain(" the orchestrator")]
            }
            (Fill::Picks, CrewRole::Worker) => vec![Words::plain("A ready model for each task")],
            (_, role) => vec![Words::plain(step_does(&self.workflow, role))],
        };
        let under = match (&step.fill, step.role) {
            (Fill::Picks, CrewRole::Worker) => picks_under(facts),
            (Fill::Picks, CrewRole::Reviewer) => vec![UnderLine {
                label: String::new(),
                parts: vec![Part::text(
                    "Never the task's worker, and never from its provider",
                )],
            }],
            _ => Vec::new(),
        };
        FillRow {
            id: step.id.clone(),
            label: step.label.clone(),
            role: Words::dim(role_word(step.role)),
            who,
            who_tone,
            what,
            state: Some(state),
            columns: vec![SheetColumn::Who],
            lit: false,
            under,
        }
    }
}

/// A step as your settings fill it.
fn step(settings: &Settings, label: &str, role: CrewRole, orchestrator: bool) -> Step {
    let picks = settings.crew.orchestrator_picks;
    let fine = role.fine_role();
    let own = settings
        .roles
        .get(&fine)
        .is_some_and(|role| !role.models.is_empty());
    let fill = match role {
        CrewRole::Worker if orchestrator && picks.worker => Fill::Picks,
        CrewRole::Reviewer if orchestrator && picks.reviewer => Fill::Picks,
        _ if own => Fill::Models(argonauts_crew::roles::candidates(settings, fine)),
        _ => match settings.crew.followed(role) {
            Some(followed) => Fill::Follows(followed),
            None => Fill::Models(argonauts_crew::roles::candidates(settings, fine)),
        },
    };
    Step {
        id: label.to_lowercase(),
        label: label.to_owned(),
        role,
        fill,
        picked: false,
    }
}

/// What ← and → go through on a step: following the step it follows, each ready model, and on
/// Orchestrator's Build and Review the orchestrator's own pick.
fn step_options(workflow: &str, role: CrewRole, facts: &Facts) -> Vec<Fill> {
    let mut options = Vec::new();
    match role {
        CrewRole::Worker if workflow != CAPTAIN => {
            options.push(Fill::Follows(CrewRole::Orchestrator));
        }
        CrewRole::Judge => options.push(Fill::Follows(CrewRole::Reviewer)),
        _ => {}
    }
    if role == CrewRole::Reviewer {
        let all: Vec<ModelRef> = one_per_agent(facts);
        if all.len() > 1 {
            options.push(Fill::Models(all));
        }
    }
    options.extend(
        facts
            .ready_models()
            .iter()
            .map(|model| Fill::Models(vec![model.worker.clone()])),
    );
    if workflow == ORCHESTRATOR && matches!(role, CrewRole::Worker | CrewRole::Reviewer) {
        options.push(Fill::Picks);
    }
    options
}

/// The first ready model of each agent, in the pickers' order.
fn one_per_agent(facts: &Facts) -> Vec<ModelRef> {
    let mut seen: Vec<String> = Vec::new();
    let mut models = Vec::new();
    for model in facts.ready_models() {
        if !seen.contains(&model.worker.agent) {
            seen.push(model.worker.agent.clone());
            models.push(model.worker.clone());
        }
    }
    models
}

fn step_pairs(step: &Step) -> Vec<(String, String)> {
    let models = |models: &[ModelRef]| toml_list(models.iter().map(|worker| worker.to_string()));
    let key = |role: CrewRole| format!("roles.{}.models", role.fine_role().as_str());
    let picks = |role: CrewRole, on: bool| match role {
        CrewRole::Worker => Some(("crew.orchestrator_picks.worker".to_owned(), on.to_string())),
        CrewRole::Reviewer => Some((
            "crew.orchestrator_picks.reviewer".to_owned(),
            on.to_string(),
        )),
        _ => None,
    };
    match (&step.fill, step.role) {
        (Fill::Models(list), CrewRole::Main) => list
            .first()
            .map(|main| ("crew.main".to_owned(), quoted(&main.to_string())))
            .into_iter()
            .collect(),
        (Fill::Models(list), role) => [Some((key(role), models(list))), picks(role, false)]
            .into_iter()
            .flatten()
            .collect(),
        // Both lists empty: the role's own and the crew's, so nothing holds it apart.
        (Fill::Follows(_), role) => [
            Some((key(role), models(&[]))),
            Some((format!("crew.{}", role.as_str()), models(&[]))),
            picks(role, false),
        ]
        .into_iter()
        .flatten()
        .collect(),
        (Fill::Picks, role) => picks(role, true).into_iter().collect(),
    }
}

fn picks_under(facts: &Facts) -> Vec<UnderLine> {
    let can: Vec<ModelRef> = one_per_agent(facts)
        .into_iter()
        .filter(|worker| !facts.out_of_plan.iter().any(|(out, _)| out == worker))
        .collect();
    let mut lines = vec![UnderLine {
        label: "Can pick".to_owned(),
        parts: joined(&can, facts, " · "),
    }];
    for (worker, until) in &facts.out_of_plan {
        let mut parts = vec![facts.named(worker)];
        parts.push(Part::text(match until {
            Some(until) => format!(", out of plan until {until}"),
            None => ", out of plan".to_owned(),
        }));
        lines.push(UnderLine {
            label: "Not now".to_owned(),
            parts,
        });
    }
    lines.push(UnderLine {
        label: "Picks by".to_owned(),
        parts: vec![Part::text(
            "Past results on this kind of task, plan left, then cost",
        )],
    });
    lines
}

fn joined(models: &[ModelRef], facts: &Facts, between: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    for (at, worker) in models.iter().enumerate() {
        if at > 0 {
            parts.push(Part::text(between));
        }
        parts.push(facts.named(worker));
    }
    parts
}

fn distinct_ready(models: &[ModelRef], facts: &Facts) -> usize {
    let mut agents: Vec<&str> = models
        .iter()
        .filter(|worker| facts.ready(worker))
        .map(|worker| worker.agent.as_str())
        .collect();
    agents.sort_unstable();
    agents.dedup();
    agents.len()
}

fn about_steps(workflow: &str) -> String {
    match workflow {
        ORCHESTRATOR => "One orchestrator plans and hands out the tasks, workers build them, and \
                         two reviewers check each one."
            .to_owned(),
        CAPTAIN => "The CLI you talk to does the work itself.".to_owned(),
        other => format!(
            "{} runs each step with the agents its role names.",
            title(other)
        ),
    }
}

/// `Opus 5.5 and GPT 5.6 propose`: each model named once, one that sits more than once as `two
/// Opus 5.5 seats`, then what they do, each verb as `does` gives it for one model or for more.
fn who_does(models: &[ModelRef], facts: &Facts, does: &[(&str, &str)]) -> Option<String> {
    let mut named: Vec<String> = Vec::new();
    let mut counted: Vec<&ModelRef> = Vec::new();
    for worker in models {
        if counted.iter().any(|seen| facts.same(seen, worker)) {
            continue;
        }
        counted.push(worker);
        let name = facts.name(worker);
        let sitting = models
            .iter()
            .filter(|other| facts.same(other, worker))
            .count();
        named.push(match sitting {
            1 => name,
            many => format!("{} {name} seats", count_word(many).to_lowercase()),
        });
    }
    if named.is_empty() {
        return None;
    }
    let verbs: Vec<String> = does
        .iter()
        .map(|(one, more)| if models.len() == 1 { *one } else { *more }.to_owned())
        .collect();
    Some(format!("{} {}", and_list(&named), and_list(&verbs)))
}

/// `a`, `a and b`, `a, b and c`.
fn and_list(items: &[String]) -> String {
    match items.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    }
}

/// What a step's role does, said of one model and of more.
fn step_verb(role: CrewRole) -> (&'static str, &'static str) {
    match role {
        CrewRole::Main => ("does the work itself", "do the work themselves"),
        CrewRole::Orchestrator => ("plans", "plan"),
        CrewRole::Worker => ("builds", "build"),
        CrewRole::Reviewer => ("reviews", "review"),
        CrewRole::Judge => ("signs off", "sign off"),
    }
}

fn role_word(role: CrewRole) -> &'static str {
    match role {
        CrewRole::Main => "Main agent",
        CrewRole::Orchestrator => "Orchestrator",
        CrewRole::Worker => "Workers",
        CrewRole::Reviewer => "Reviewers",
        CrewRole::Judge => "Judge",
    }
}

fn step_does(workflow: &str, role: CrewRole) -> &'static str {
    match (workflow, role) {
        (_, CrewRole::Main) => "Does the work in your checkout",
        (_, CrewRole::Orchestrator) => "Plans and hands out the tasks",
        (_, CrewRole::Worker) => "Build the tasks",
        (ORCHESTRATOR, CrewRole::Reviewer) => "2 per task, from another provider",
        (_, CrewRole::Reviewer) => "Check each change, from another provider",
        (_, CrewRole::Judge) => "Decides and signs off",
    }
}

/// Council's seats as your settings fill them, without the agents that can't take a turn:
/// those show under the seats, with what makes them ready. Seats you listed stay as listed.
fn ready_seats(settings: &Settings, facts: &Facts) -> Vec<(ModelRef, SeatRole)> {
    let seats = super::seats(settings);
    if !settings.council.seats.is_empty() {
        return seats;
    }
    seats
        .into_iter()
        .filter(|(worker, _)| facts.ready(worker))
        .collect()
}

pub(crate) fn seat_id(at: usize) -> String {
    format!("seat-{}", at + 1)
}

fn seat_at(row: &str) -> Option<usize> {
    row.strip_prefix("seat-")?
        .parse::<usize>()
        .ok()?
        .checked_sub(1)
}

/// The model Add one seats: the first ready one whose agent has no seat yet, else the last
/// seat's model again, else the first ready model.
fn next_seat(seats: &[(ModelRef, SeatRole)], facts: &Facts) -> Option<ModelRef> {
    let fresh = one_per_agent(facts)
        .into_iter()
        .find(|worker| !seats.iter().any(|(seated, _)| seated.agent == worker.agent));
    fresh
        .or_else(|| seats.last().map(|(worker, _)| worker.clone()))
        .or_else(|| one_per_agent(facts).into_iter().next())
}

/// How each task is checked, as the council's ladder picks it for each seat that builds: the
/// checker seat, a seat or model of another maker, or a fresh session of the one maker set up.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Rung {
    /// The checker seat checks every task: no seat that builds shares its maker.
    CheckerSeat(ModelRef),
    /// The checker seat checks the tasks of other makers' seats; the tasks of seats from its
    /// own maker go to another maker.
    CheckerAndOther(ModelRef),
    OtherMaker(Option<ModelRef>),
    SameMaker(ModelRef),
}

fn checks_rung(seats: &[(ModelRef, SeatRole)], facts: &Facts) -> Rung {
    let makers: Vec<String> = seats
        .iter()
        .map(|(worker, _)| facts.provider(worker))
        .collect();
    let mut distinct = makers.clone();
    distinct.sort();
    distinct.dedup();
    let other = one_per_agent(facts)
        .into_iter()
        .find(|worker| !makers.contains(&facts.provider(worker)));
    // Every seat comes from one maker and no other is set up: each task's own model checks it,
    // in a fresh session.
    if distinct.len() < 2 && other.is_none() {
        return match seats.first() {
            Some((worker, _)) => Rung::SameMaker(worker.clone()),
            None => Rung::OtherMaker(None),
        };
    }
    if let Some((checker, _)) = seats.iter().find(|(_, role)| *role == SeatRole::Checker) {
        let theirs = facts.provider(checker);
        let shares = seats
            .iter()
            .any(|(worker, role)| *role != SeatRole::Checker && facts.provider(worker) == theirs);
        return if shares {
            Rung::CheckerAndOther(checker.clone())
        } else {
            Rung::CheckerSeat(checker.clone())
        };
    }
    if distinct.len() > 1 {
        return Rung::OtherMaker(None);
    }
    Rung::OtherMaker(other)
}

/// ← or → on Checks: the rung code picks by itself, or a ready model that isn't seated yet,
/// which takes the checker seat.
fn change_checks(seats: &mut Vec<(ModelRef, SeatRole)>, forward: bool, facts: &Facts) {
    let checker = seats
        .iter()
        .position(|(_, role)| *role == SeatRole::Checker);
    let mut options: Vec<Option<ModelRef>> = vec![None];
    options.extend(
        facts
            .ready_models()
            .iter()
            .map(|model| model.worker.clone())
            .filter(|worker| {
                checker.is_some_and(|at| facts.same(&seats[at].0, worker))
                    || !facts.seated(seats, worker)
            })
            .map(Some),
    );
    let now = checker.map(|at| seats[at].0.clone());
    let Some(next) = cycle(&options, &now, forward) else {
        return;
    };
    match (next, checker) {
        (None, Some(at)) => seats[at].1 = SeatRole::Proposer,
        (Some(worker), Some(at)) => seats[at].0 = worker,
        (Some(worker), None) if seats.len() < MOST_SEATS => {
            seats.push((worker, SeatRole::Checker));
        }
        _ => {}
    }
}

fn seats_rows(seats: &[(ModelRef, SeatRole)], facts: &Facts) -> (Vec<SheetRow>, Vec<String>) {
    let mut rows = Vec::new();
    let keeper = seats
        .iter()
        .position(|(_, role)| *role == SeatRole::Proposer);
    for (at, (worker, role)) in seats.iter().enumerate() {
        let sitting = seats
            .iter()
            .filter(|(other, _)| facts.same(other, worker))
            .count();
        let number = (sitting > 1).then(|| {
            let nth = seats[..=at]
                .iter()
                .filter(|(other, _)| facts.same(other, worker))
                .count();
            u8::try_from(nth).unwrap_or(u8::MAX)
        });
        let mut name = facts.name(worker);
        if let Some(number) = number {
            name = format!("{name} · {number}");
        }
        let (role_word, what) = match role {
            _ if Some(at) == keeper => {
                ("Note keeper", "Proposes, lists the points, writes the plan")
            }
            SeatRole::Proposer => ("Proposer", "Says what it thinks is right"),
            SeatRole::Skeptic => ("Skeptic", "Argues against where most seats lean"),
            SeatRole::Checker => ("Checker", "Checks each task in a fresh session"),
        };
        let state = match facts.present.cli(&worker.agent).map(|cli| &cli.now) {
            Some(Now::LoggedOut { .. }) => Words::new("Logged out", Tone::Warn),
            Some(Now::NotInstalled { .. }) => Words::new("Not installed", Tone::Warn),
            Some(Now::Off) => Words::new("Off", Tone::Warn),
            _ => Words::new("Ready", Tone::Ok),
        };
        rows.push(SheetRow::Fill(FillRow {
            id: seat_id(at),
            label: "Seat".to_owned(),
            role: Words::plain(role_word),
            who: vec![Part::Named {
                logo: logo_of(worker),
                name,
                number,
            }],
            who_tone: Tone::Text,
            what: vec![Words::dim(what)],
            state: Some(state),
            columns: vec![SheetColumn::Who, SheetColumn::Role],
            lit: false,
            under: Vec::new(),
        }));
    }
    let mut add = vec![Part::text("Up to 6, the same model again too")];
    let fresh = one_per_agent(facts)
        .into_iter()
        .find(|worker| !seats.iter().any(|(seated, _)| seated.agent == worker.agent));
    if let Some(fresh) = &fresh {
        add.push(Part::text(" · next ready: "));
        add.push(facts.named(fresh));
    }
    rows.push(SheetRow::Aside(AsideRow {
        id: Some("add".to_owned()),
        logo: None,
        name: "Add one".to_owned(),
        parts: add,
    }));
    for cli in facts.present.clis.iter().flatten() {
        let why = match &cli.now {
            Now::Ready | Now::Off => continue,
            Now::LoggedOut { login: Some(login) } => {
                format!("Logged out · run {login}, and it shows here")
            }
            Now::LoggedOut { login: None } => "Logged out".to_owned(),
            Now::NotInstalled { add } => format!("Not installed · {add}"),
        };
        rows.push(SheetRow::Aside(AsideRow {
            id: None,
            logo: Some(logo_for(&cli.id, None)),
            name: cli.name.clone(),
            parts: vec![Part::text(why)],
        }));
    }
    let rung = checks_rung(seats, facts);
    let (who, what, state) = match &rung {
        Rung::CheckerSeat(worker) => (
            Part::named(
                logo_of(worker),
                format!("{}, the checker seat", facts.name(worker)),
            ),
            "Another maker for every task, fresh each time",
            Words::new("Ready", Tone::Ok),
        ),
        Rung::CheckerAndOther(worker) => (
            Part::named(
                logo_of(worker),
                format!(
                    "{}, the checker seat, and another maker",
                    facts.name(worker)
                ),
            ),
            "Another maker for every task, fresh each time",
            Words::new("Ready", Tone::Ok),
        ),
        Rung::OtherMaker(Some(worker)) => (
            Part::named(
                logo_of(worker),
                format!("{}, another maker", facts.name(worker)),
            ),
            "Another maker for every task, fresh each time",
            Words::new("Ready", Tone::Ok),
        ),
        Rung::OtherMaker(None) => (
            Part::text("A seat of another maker"),
            "Another maker for every task, fresh each time",
            Words::new("Ready", Tone::Ok),
        ),
        Rung::SameMaker(worker) => (
            Part::named(
                logo_of(worker),
                format!("A fresh {} session", facts.name(worker)),
            ),
            "Blind to who made it; no other maker set up",
            Words::new("Same maker", Tone::Warn),
        ),
    };
    rows.push(SheetRow::Fill(FillRow {
        id: "checks".to_owned(),
        label: "Checks".to_owned(),
        role: Words::plain(""),
        who: vec![who],
        who_tone: Tone::Text,
        what: vec![Words::dim(what)],
        state: Some(state),
        columns: vec![SheetColumn::Who],
        lit: true,
        under: Vec::new(),
    }));
    (rows, seats_notes(seats, &rung, facts))
}

fn seats_notes(seats: &[(ModelRef, SeatRole)], rung: &Rung, facts: &Facts) -> Vec<String> {
    let skeptic = seats.iter().any(|(_, role)| *role == SeatRole::Skeptic);
    if let Rung::SameMaker(worker) = rung {
        let name = whole(&facts.name(worker));
        let cli = whole(&facts.present.name(&worker.agent));
        let mut paragraph = format!(
            "Only {cli} is set up, so a fresh {name} session checks each task. It never sees the \
             talk or who made the change, but it may still favour its own style, and each check is marked as such."
        );
        let sibling = facts
            .ready_models()
            .into_iter()
            .find(|model| {
                model.provider == facts.provider(worker) && !facts.seated(seats, &model.worker)
            })
            .map(|model| whole(&model.name));
        if let Some(sibling) = sibling {
            paragraph.push_str(&format!(
                " ←  →  offers {sibling}: another model, but the same maker."
            ));
        }
        let same = seats
            .iter()
            .filter(|(seated, _)| facts.same(seated, worker))
            .count();
        if same == seats.len() && same > 1 {
            let helps = if skeptic {
                "the skeptic helps"
            } else {
                "a skeptic helps"
            };
            paragraph.push_str(&format!(
                " {} {name} seats tend to make the same mistakes; {helps}.",
                count_word(same)
            ));
        }
        return wrap(&paragraph, NOTE_WIDTH);
    }
    let mut notes = Vec::new();
    let mut counted: Vec<&ModelRef> = Vec::new();
    for (worker, _) in seats {
        if counted.iter().any(|seen| facts.same(seen, worker)) {
            continue;
        }
        counted.push(worker);
        let sitting = seats
            .iter()
            .filter(|(other, _)| facts.same(other, worker))
            .count();
        if sitting < 2 {
            continue;
        }
        let name = facts.name(worker);
        let numbers: Vec<String> = (1..=sitting).map(|n| format!("· {n}")).collect();
        let (last, first) = numbers.split_last().expect("two or more");
        // Claude Code and Codex seats start with their memory writing off; the other CLIs may
        // keep a memory of their own across sessions.
        let share = if memory_off(&worker.agent) {
            "share nothing, not even memory".to_owned()
        } else {
            format!(
                "share no session or folder, though {} may keep a memory of its own",
                whole(&facts.present.name(&worker.agent))
            )
        };
        notes.push(format!(
            "{name} sits {}: {} sessions that {share}. You see them as {name} {} and {last}.",
            times_word(sitting),
            count_word(sitting).to_lowercase(),
            first.join(", ")
        ));
    }
    let skeptics = seats
        .iter()
        .filter(|(_, role)| *role == SeatRole::Skeptic)
        .count();
    if skeptics > 1 {
        notes.push(format!(
            "{} seats play the skeptic, where one is usual: each argues the other side, so the \
             talk spends more of itself against where the seats lean.",
            count_word(skeptics)
        ));
    }
    if skeptic {
        notes.push(
            "The skeptic answers alone first, like every seat. In the talk it argues the other \
             side; its own stance counts."
                .to_owned(),
        );
        notes.push(
            "The seats are told which seat is the skeptic, and never which model runs any seat."
                .to_owned(),
        );
    }
    notes.push(
        "Each check is a fresh session: the plan and the change, never the talk, never who made \
         it. Another maker first."
            .to_owned(),
    );
    notes
}

fn ways_rows(ways: &Ways, listed: &[String], facts: &Facts) -> Vec<SheetRow> {
    let mut rows = vec![
        SheetRow::Heading {
            label: "How tasks are done".to_owned(),
            text: "Picked per task, from what you allow".to_owned(),
        },
        SheetRow::Way(WayRow {
            id: "ways.own_session".to_owned(),
            allowed: ways.own_session,
            name: "Own session".to_owned(),
            value: None,
            hint: None,
            inner: false,
        }),
    ];
    let examples: Vec<String> = facts
        .workflows
        .iter()
        .take(2)
        .map(|name| title(name))
        .collect();
    let list_hint = match examples.as_slice() {
        [] => "or a list".to_owned(),
        names => format!("or a list: {}, …", names.join(", ")),
    };
    let (value, hint) = match &ways.workflows {
        _ if facts.no_nesting => (
            Some("Off".to_owned()),
            Some("workflows.max_depth is 0, so no task runs one".to_owned()),
        ),
        WorkflowsAllowed::Any(_) => (Some("Any".to_owned()), Some(list_hint)),
        WorkflowsAllowed::These(names) if names.is_empty() => (
            Some("None".to_owned()),
            Some("space allows them again".to_owned()),
        ),
        WorkflowsAllowed::These(_) => (
            Some("A list".to_owned()),
            Some("any of those marked, picked per task".to_owned()),
        ),
    };
    rows.push(SheetRow::Way(WayRow {
        id: "ways.workflows".to_owned(),
        allowed: !ways.workflows.is_off() && !facts.no_nesting,
        name: "Workflows".to_owned(),
        value,
        hint,
        inner: false,
    }));
    if let WorkflowsAllowed::These(names) = &ways.workflows
        && !names.is_empty()
    {
        let mut every: Vec<String> = listed.to_vec();
        for name in &facts.workflows {
            if !every.contains(name) {
                every.push(name.clone());
            }
        }
        for name in every {
            rows.push(SheetRow::Way(WayRow {
                id: format!("ways.workflow.{name}"),
                allowed: names.contains(&name),
                name: title(&name),
                value: None,
                hint: None,
                inner: true,
            }));
        }
    }
    let agents = if ways.agents.is_off() {
        "None".to_owned()
    } else {
        format!("Up to {}", ways.agents.get())
    };
    rows.push(SheetRow::Way(WayRow {
        id: "ways.agents".to_owned(),
        allowed: !ways.agents.is_off(),
        name: "Agents".to_owned(),
        value: Some(agents),
        hint: None,
        inner: false,
    }));
    rows
}

fn seat_list(seats: &[(ModelRef, SeatRole)]) -> String {
    let tables: Vec<String> = seats
        .iter()
        .map(|(worker, role)| {
            let role = match role {
                SeatRole::Proposer => "proposer",
                SeatRole::Skeptic => "skeptic",
                SeatRole::Checker => "checker",
            };
            format!(
                "{{ model = {}, role = {} }}",
                quoted(&worker.to_string()),
                quoted(role)
            )
        })
        .collect();
    format!("[{}]", tables.join(", "))
}

fn quoted(text: &str) -> String {
    toml::Value::String(text.to_owned()).to_string()
}

fn toml_list<'a>(items: impl Iterator<Item = impl AsRef<str> + 'a>) -> String {
    toml::Value::Array(
        items
            .map(|item| toml::Value::String(item.as_ref().to_owned()))
            .collect(),
    )
    .to_string()
}

/// The next option after `now`, or the one before it. When `now` isn't one, → starts at the
/// first and ← at the last.
fn cycle<T: Clone + PartialEq>(options: &[T], now: &T, forward: bool) -> Option<T> {
    if options.is_empty() {
        return None;
    }
    let len = options.len();
    let next = match options.iter().position(|option| option == now) {
        Some(at) if forward => (at + 1) % len,
        Some(at) => (at + len - 1) % len,
        None if forward => 0,
        None => len - 1,
    };
    Some(options[next].clone())
}

/// Whether the CLI's seat sessions start with its memory writing off, so two of its seats
/// share nothing through it.
fn memory_off(agent: &str) -> bool {
    matches!(agent, "claude-code" | "codex")
}

fn count_word(count: usize) -> String {
    match count {
        2 => "Two",
        3 => "Three",
        4 => "Four",
        5 => "Five",
        6 => "Six",
        _ => return count.to_string(),
    }
    .to_owned()
}

fn times_word(count: usize) -> String {
    match count {
        2 => "twice".to_owned(),
        3 => "three times".to_owned(),
        n => format!("{n} times"),
    }
}

/// A name that a note never breaks, such as `Sonnet 5`.
fn whole(name: &str) -> String {
    name.replace(' ', "\u{a0}")
}

/// `text` broken at spaces into lines of at most `width`. Names made [`whole`] and the double
/// space between arrows stay on one line.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.replace("←  →", "←\u{a0}\u{a0}→").split(' ') {
        let word = word.replace('\u{a0}', " ");
        let wider = if line.is_empty() {
            word.chars().count()
        } else {
            line.chars().count() + 1 + word.chars().count()
        };
        if wider > width && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests;
