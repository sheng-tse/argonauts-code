//! The ways a setup lets a plan pick for a task, in the order ←/→ steps through them.

use argonauts_core::events::task::Way;
use argonauts_core::settings::Settings;
use argonauts_core::settings::ways::{Ways, WorkflowsAllowed};
use argonauts_core::workflow::WorkflowRef;

use super::{Setup, workflow_words};

/// Which way ←/→ moves through the offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toward {
    Previous,
    Next,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    setup: Setup,
    ways: Vec<Way>,
    /// Every workflow a task could run, offered or not.
    known: Vec<String>,
    own_session: bool,
    /// How many workflows the setup's list names, when it is a list.
    listed: Option<usize>,
    /// `workflows.max_depth` is 0, so no task runs a workflow whatever the list allows.
    no_nesting: bool,
    most_agents: u8,
}

impl Offer {
    /// The ways `ways` allows. `workflows` is every workflow a task could run, in the order to
    /// offer them, as [`super::task_workflows`] lists them.
    pub fn new(setup: Setup, ways: &Ways, workflows: &[String], max_depth: u32) -> Self {
        let no_nesting = max_depth == 0;
        let mut offered = Vec::new();
        if ways.own_session {
            offered.push(Way::OwnSession);
        }
        if !no_nesting {
            offered.extend(
                workflows
                    .iter()
                    .filter(|name| ways.workflows.allows(name))
                    .map(|name| Way::Workflow {
                        name: WorkflowRef::new(name),
                    }),
            );
        }
        if matches!(ways.workflows, WorkflowsAllowed::Any(_)) && !no_nesting {
            offered.push(Way::CliWorkflow);
        }
        let most_agents = ways.agents.get();
        offered.extend((2..=most_agents).map(|count| Way::Agents { count }));
        Self {
            setup,
            ways: offered,
            known: workflows.to_vec(),
            own_session: ways.own_session,
            listed: match &ways.workflows {
                WorkflowsAllowed::These(names) => Some(names.len()),
                WorkflowsAllowed::Any(_) => None,
            },
            no_nesting,
            most_agents,
        }
    }

    /// The offer for the run `settings` describe.
    pub fn for_run(settings: &Settings, workflows: &[String]) -> Self {
        let setup = Setup::of(settings);
        Self::new(
            setup,
            setup.ways(settings),
            workflows,
            settings.workflows.max_depth,
        )
    }

    pub fn setup(&self) -> Setup {
        self.setup
    }

    /// In the order ←/→ steps through them.
    pub fn ways(&self) -> &[Way] {
        &self.ways
    }

    pub fn allows(&self, way: &Way) -> bool {
        self.ways.contains(way)
    }

    /// The most agents a task may start, 0 when the setup allows none.
    pub fn most_agents(&self) -> u8 {
        self.most_agents
    }

    /// The way ←/→ moves to from `from`, staying put at either end. A way the offer doesn't
    /// hold, such as one you asked for that your setup turned off, steps onto the offer's
    /// first way, or its last going back.
    pub fn step(&self, from: &Way, toward: Toward) -> Way {
        let Some(last) = self.ways.len().checked_sub(1) else {
            return from.clone();
        };
        let to = match (self.ways.iter().position(|way| way == from), toward) {
            (Some(at), Toward::Next) => (at + 1).min(last),
            (Some(at), Toward::Previous) => at.saturating_sub(1),
            (None, Toward::Next) => 0,
            (None, Toward::Previous) => last,
        };
        self.ways[to].clone()
    }

    /// Why `way` can't be picked, or `None` when it can.
    pub fn refusal(&self, way: &Way) -> Option<String> {
        if self.allows(way) {
            return None;
        }
        let whose = self.setup.whose();
        let changes = format!("{} changes it.", self.setup.sheet());
        Some(match way {
            Way::OwnSession => format!("Own session is off in {whose}. {changes}"),
            Way::Workflow { name } if !self.known.iter().any(|known| known == name.as_str()) => {
                format!("There's no workflow named {name} that a task can run.")
            }
            Way::Workflow { .. } | Way::CliWorkflow if self.no_nesting => {
                "No task runs a workflow while workflows.max_depth is 0.".to_owned()
            }
            Way::Workflow { name } => format!(
                "{} is off in {whose}. {changes}",
                workflow_words(name.as_str())
            ),
            Way::CliWorkflow => format!(
                "A CLI's own workflow is offered only where {whose} allows any workflow. {changes}"
            ),
            Way::Agents { .. } if self.most_agents == 0 => {
                format!("Agents are off in {whose}. {changes}")
            }
            Way::Agents { .. } => format!(
                "{} allows up to {} agents. {changes}",
                capitalized(whose),
                self.most_agents
            ),
        })
    }

    /// What the setup leaves out of the offer, and where to change it: "Not offered:
    /// Best-of-n, Draft-and-vote · this council's setup lists two workflows · Set up Council
    /// changes it". `None` when it leaves nothing out.
    pub fn not_offered_line(&self) -> Option<String> {
        let mut left_out: Vec<String> = Vec::new();
        let mut because: Vec<String> = Vec::new();
        if !self.own_session {
            left_out.push("Own session".to_owned());
            because.push("turns own session off".to_owned());
        }
        if self.no_nesting {
            left_out.push("workflows".to_owned());
        } else if self.listed == Some(0) {
            left_out.push("workflows".to_owned());
            because.push("allows no workflows".to_owned());
        } else if let Some(listed) = self.listed {
            let unlisted: Vec<String> = self
                .known
                .iter()
                .filter(|name| !self.ways.iter().any(|way| names(way, name)))
                .map(|name| workflow_words(name))
                .collect();
            if !unlisted.is_empty() {
                left_out.extend(unlisted);
                because.push(format!("lists {}", counted(listed, "workflow")));
            }
        }
        if self.most_agents == 0 {
            left_out.push("agents".to_owned());
            because.push("allows no agents".to_owned());
        }
        if left_out.is_empty() {
            return None;
        }
        let mut line = format!("Not offered: {}", left_out.join(", "));
        if !because.is_empty() {
            line.push_str(&format!(
                " · {} {} · {} changes it",
                self.setup.whose(),
                because.join(" and "),
                self.setup.sheet()
            ));
        }
        if self.no_nesting {
            line.push_str(" · workflows.max_depth is 0");
        }
        Some(line)
    }
}

fn names(way: &Way, workflow: &str) -> bool {
    matches!(way, Way::Workflow { name } if name.as_str() == workflow)
}

fn counted(n: usize, thing: &str) -> String {
    const WORDS: [&str; 10] = [
        "no", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
    ];
    let count = WORDS
        .get(n)
        .map_or_else(|| n.to_string(), |word| (*word).to_owned());
    if n == 1 {
        format!("{count} {thing}")
    } else {
        format!("{count} {thing}s")
    }
}

fn capitalized(text: &str) -> String {
    workflow_words(text)
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::ways::MostAgents;

    use super::*;
    use crate::council::ways::task_workflows;

    fn workflow(name: &str) -> Way {
        Way::Workflow {
            name: WorkflowRef::new(name),
        }
    }

    fn known() -> Vec<String> {
        task_workflows(["spec-first".to_owned()])
    }

    fn listing(names: &[&str], agents: u8) -> Ways {
        Ways {
            own_session: true,
            workflows: WorkflowsAllowed::These(names.iter().map(|n| (*n).to_owned()).collect()),
            agents: MostAgents::try_from(agents).unwrap(),
        }
    }

    #[test]
    fn every_way_is_offered_by_default_in_order() {
        let offer = Offer::new(Setup::Council, &Ways::default(), &known(), 1);
        assert_eq!(
            offer.ways(),
            [
                Way::OwnSession,
                workflow("maker-checker"),
                workflow("best-of-n"),
                workflow("draft-and-vote"),
                workflow("spec-first"),
                Way::CliWorkflow,
                Way::Agents { count: 2 },
                Way::Agents { count: 3 },
                Way::Agents { count: 4 },
            ]
        );
        assert_eq!(offer.not_offered_line(), None);
    }

    #[test]
    fn a_list_offers_only_its_workflows_and_says_what_it_leaves_out() {
        let ways = listing(&["maker-checker", "spec-first"], 4);
        let offer = Offer::new(Setup::Council, &ways, &known(), 1);
        let shown: Vec<String> = offer.ways().iter().map(super::super::words).collect();
        assert_eq!(
            shown,
            [
                "Own session",
                "Maker-checker",
                "Spec-first",
                "2 agents",
                "3 agents",
                "4 agents"
            ]
        );
        assert_eq!(
            offer.not_offered_line().as_deref(),
            Some(
                "Not offered: Best-of-n, Draft-and-vote · this council's setup lists two \
                 workflows · Set up Council changes it"
            )
        );
        assert_eq!(
            offer.refusal(&workflow("best-of-n")).as_deref(),
            Some("Best-of-n is off in this council's setup. Set up Council changes it.")
        );
        assert!(offer.refusal(&workflow("spec-first")).is_none());
    }

    #[test]
    fn arrows_step_through_the_offer_and_stop_at_its_ends() {
        let ways = listing(&["maker-checker"], 2);
        let offer = Offer::new(Setup::Orchestrator, &ways, &known(), 1);
        let next = |way: &Way| offer.step(way, Toward::Next);
        let back = |way: &Way| offer.step(way, Toward::Previous);
        assert_eq!(next(&Way::OwnSession), workflow("maker-checker"));
        assert_eq!(next(&workflow("maker-checker")), Way::Agents { count: 2 });
        assert_eq!(next(&Way::Agents { count: 2 }), Way::Agents { count: 2 });
        assert_eq!(back(&Way::OwnSession), Way::OwnSession);
        // A way you asked for that the setup turned off steps onto the offer.
        assert_eq!(next(&workflow("best-of-n")), Way::OwnSession);
        assert_eq!(back(&workflow("best-of-n")), Way::Agents { count: 2 });
    }

    #[test]
    fn no_nesting_offers_no_workflow_and_says_why() {
        let offer = Offer::new(Setup::Council, &Ways::default(), &known(), 0);
        assert!(
            offer
                .ways()
                .iter()
                .all(|way| !matches!(way, Way::Workflow { .. } | Way::CliWorkflow))
        );
        assert_eq!(
            offer.not_offered_line().as_deref(),
            Some("Not offered: workflows · workflows.max_depth is 0")
        );
        assert_eq!(
            offer.refusal(&workflow("maker-checker")).as_deref(),
            Some("No task runs a workflow while workflows.max_depth is 0.")
        );
    }

    #[test]
    fn ways_turned_off_are_named_with_their_reason() {
        let ways = Ways {
            own_session: false,
            workflows: WorkflowsAllowed::These(Vec::new()),
            agents: MostAgents::NONE,
        };
        let offer = Offer::new(Setup::Orchestrator, &ways, &known(), 1);
        assert!(offer.ways().is_empty());
        assert_eq!(offer.step(&Way::OwnSession, Toward::Next), Way::OwnSession);
        assert_eq!(
            offer.not_offered_line().as_deref(),
            Some(
                "Not offered: Own session, workflows, agents · the orchestrator's setup turns \
                 own session off and allows no workflows and allows no agents · Set up \
                 Orchestrator changes it"
            )
        );
        assert_eq!(
            offer.refusal(&Way::Agents { count: 2 }).as_deref(),
            Some("Agents are off in the orchestrator's setup. Set up Orchestrator changes it.")
        );
        let two = Offer::new(Setup::Council, &listing(&[], 2), &known(), 1);
        assert_eq!(
            two.refusal(&Way::Agents { count: 3 }).as_deref(),
            Some("This council's setup allows up to 2 agents. Set up Council changes it.")
        );
        assert_eq!(
            two.refusal(&workflow("release-notes")).as_deref(),
            Some("There's no workflow named release-notes that a task can run.")
        );
    }
}
