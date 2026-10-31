//! Which agent of a CLI's own workflow each ask through Argonauts Code answers. Claude Code's
//! agent that reviews calls the tool, and the crew's reviewer runs as a session of its own; the
//! workflow view draws that session in the agent's row, and the run's row in the bottom list
//! counts the two as one agent. Both join them here, so they never disagree.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::base::names::SessionKey;
use argonauts_core::runs::events::session::{AskedBy, CliWorkflowRun};

/// One ask as the join reads it: the session it started, the label it went under, when it
/// started (milliseconds since the epoch), and who the crew stamped as asking, when it did.
#[derive(Debug, Clone, Copy)]
pub struct Ask<'a> {
    pub key: &'a SessionKey,
    pub label: &'a str,
    pub started_ms: Option<u64>,
    pub asked_by: Option<&'a AskedBy>,
}

/// Which ask each agent of `workflow`, run by `host`, made, by the agent's number. In order:
/// the latest ask the crew stamped with that agent; then the latest under the agent's label
/// without its round, the second agent of a label taking the crew's `label · round 2`; then the
/// latest that started while that agent was the only one at work. Each ask fills one row.
/// `asks` are in the order they started.
pub fn joins(
    asks: &[Ask<'_>],
    host: &SessionKey,
    workflow: &CliWorkflowRun,
) -> BTreeMap<u32, SessionKey> {
    let mut joined = BTreeMap::new();
    let mut taken: BTreeSet<&SessionKey> = BTreeSet::new();
    let mine = |ask: &Ask<'_>| {
        ask.asked_by
            .is_none_or(|by| by.session == *host && by.agent.is_none())
    };
    for agent in &workflow.agents {
        let stamped = asks.iter().rev().find(|ask| {
            !taken.contains(ask.key)
                && ask.asked_by.is_some_and(|by| {
                    by.session == *host && by.agent.as_deref() == Some(agent.label.as_str())
                })
        });
        if let Some(ask) = stamped {
            taken.insert(ask.key);
            joined.insert(agent.index, ask.key.clone());
        }
    }
    let mut rounds: BTreeMap<&str, u32> = BTreeMap::new();
    for agent in &workflow.agents {
        let stem = label_stem(&agent.label);
        let round = rounds.entry(stem).or_default();
        *round += 1;
        if joined.contains_key(&agent.index) {
            continue;
        }
        let label = match *round {
            1 => stem.to_owned(),
            n => format!("{stem} · round {n}"),
        };
        let labelled = asks
            .iter()
            .rev()
            .find(|ask| !taken.contains(ask.key) && mine(ask) && ask.label.trim() == label);
        if let Some(ask) = labelled {
            taken.insert(ask.key);
            joined.insert(agent.index, ask.key.clone());
        }
    }
    for agent in &workflow.agents {
        if joined.contains_key(&agent.index) {
            continue;
        }
        let during = asks.iter().rev().find(|ask| {
            !taken.contains(ask.key)
                && mine(ask)
                && ask
                    .started_ms
                    .and_then(|at| at_work_alone(workflow, at))
                    .is_some_and(|alone| alone == agent.index)
        });
        if let Some(ask) = during {
            taken.insert(ask.key);
            joined.insert(agent.index, ask.key.clone());
        }
    }
    joined
}

/// The number of the one agent of `workflow` at work at `at`, by the times its report gives.
/// `None` when none was, or several were.
fn at_work_alone(workflow: &CliWorkflowRun, at: u64) -> Option<u32> {
    let mut working = workflow.agents.iter().filter(|agent| {
        agent.started_at_ms.is_some_and(|started| {
            started <= at && (!agent.state.ended() || at <= started + agent.duration_ms)
        })
    });
    let one = working.next()?;
    working.next().is_none().then_some(one.index)
}

/// A label without the round its script or the crew numbered it by: `review:money` for
/// `review:money#2`, and for the crew's `review:money · round 2`.
pub fn label_stem(label: &str) -> &str {
    fn numbered<'a>((stem, n): (&'a str, &str)) -> Option<&'a str> {
        n.trim().parse::<u32>().is_ok().then_some(stem)
    }
    let label = label.trim();
    label
        .rsplit_once(" · round ")
        .and_then(numbered)
        .or_else(|| label.rsplit_once('#').and_then(numbered))
        .unwrap_or(label)
        .trim_end()
}

#[cfg(test)]
mod tests {
    use argonauts_core::runs::events::session::{CliAgent, CliAgentState, SubagentStatus};

    use super::*;

    /// Haiku's workflow in the Captain run: each agent by label, state, and when it started and
    /// for how long, in seconds after 12:18:42.
    fn haiku(agents: &[(&str, CliAgentState, u64, u64)]) -> CliWorkflowRun {
        CliWorkflowRun {
            id: "worx86tbc".to_owned(),
            call: None,
            name: "fix-money-rounding".to_owned(),
            description: String::new(),
            phases: Vec::new(),
            agents: agents
                .iter()
                .zip(1..)
                .map(|(&(label, state, from, seconds), index)| CliAgent {
                    index,
                    label: label.to_owned(),
                    state,
                    started_at_ms: Some(from * 1000),
                    duration_ms: seconds * 1000,
                    ..CliAgent::default()
                })
                .collect(),
            status: SubagentStatus::Running,
            tokens: 0,
            tool_calls: 0,
            duration_ms: 0,
            summary: None,
            started_at_ms: None,
        }
    }

    fn ask<'a>(key: &'a SessionKey, label: &'a str, at: u64) -> Ask<'a> {
        Ask {
            key,
            label,
            started_ms: Some(at * 1000),
            asked_by: None,
        }
    }

    /// The script numbers its agents by round and asks under the bare label: each ask still
    /// fills its own agent's row.
    #[test]
    fn an_ask_joins_its_agent_by_the_label_without_its_round() {
        use CliAgentState::{Done, Running};
        let workflow = haiku(&[
            ("build:fix-the-money", Done, 0, 10),
            ("review:fix-the-money#1", Done, 11, 12),
            ("sign-off:fix-the-money#1", Running, 23, 0),
        ]);
        let (host, codex, agy) = (
            SessionKey::numbered(1),
            SessionKey::numbered(2),
            SessionKey::numbered(3),
        );
        let asks = [
            ask(&codex, "review:fix-the-money", 14),
            ask(&agy, "sign-off:fix-the-money", 25),
        ];
        let joined = joins(&asks, &host, &workflow);
        assert_eq!(joined.get(&2), Some(&codex));
        assert_eq!(joined.get(&3), Some(&agy));
        assert_eq!(joined.len(), 2);
    }

    /// An ask under a label no agent has joins the one agent at work when it came, and one the
    /// crew stamped joins the agent it names whatever its label and time.
    #[test]
    fn an_ask_under_another_label_joins_by_who_was_at_work_or_by_its_stamp() {
        use CliAgentState::Done;
        let workflow = haiku(&[
            ("review:final-1", Done, 0, 10),
            ("sign-off:money-1", Done, 10, 10),
            ("review:final-2", Done, 20, 10),
        ]);
        let host = SessionKey::numbered(1);
        let (first, second, stamped) = (
            SessionKey::numbered(2),
            SessionKey::numbered(3),
            SessionKey::numbered(4),
        );
        let by = AskedBy {
            session: host.clone(),
            workflow: None,
            agent: Some("review:final-2".to_owned()),
        };
        let asks = [
            ask(&first, "review:fix-the-money", 3),
            ask(&second, "sign-off:fix-the-money", 14),
            Ask {
                asked_by: Some(&by),
                ..ask(&stamped, "review:fix-the-money · round 2", 1)
            },
        ];
        let joined = joins(&asks, &host, &workflow);
        assert_eq!(joined.get(&1), Some(&first));
        assert_eq!(joined.get(&2), Some(&second));
        assert_eq!(joined.get(&3), Some(&stamped));
    }

    /// Two agents at work at once leave an ask under neither's label unjoined, as does one
    /// stamped by another session's workflow.
    #[test]
    fn an_ask_nothing_says_is_whose_joins_no_row() {
        use CliAgentState::Running;
        let workflow = haiku(&[("review:a", Running, 0, 0), ("review:b", Running, 0, 0)]);
        let host = SessionKey::numbered(1);
        let lone = SessionKey::numbered(2);
        let elsewhere = AskedBy {
            session: SessionKey::numbered(9),
            workflow: None,
            agent: None,
        };
        let asks = [
            ask(&lone, "review:c", 5),
            Ask {
                asked_by: Some(&elsewhere),
                ..ask(&lone, "review:a", 5)
            },
        ];
        assert!(joins(&asks, &host, &workflow).is_empty());
    }

    #[test]
    fn a_labels_stem_drops_either_rounds_number() {
        assert_eq!(label_stem("review:money#2"), "review:money");
        assert_eq!(label_stem("review:money · round 3"), "review:money");
        assert_eq!(label_stem("review:money"), "review:money");
        assert_eq!(label_stem("review:issue#x"), "review:issue#x");
    }
}
