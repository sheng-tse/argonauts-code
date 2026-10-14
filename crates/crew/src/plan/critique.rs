//! Critique rounds (`critique.rounds`, disputed topics only, the fixed checklist). One
//! designated critic per run argues against the leading plan, rotating across runs; the
//! rotation is kept in `state/<repo>/crew.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use futures::future::join_all;
use serde_json::{Map, Value, json};

use argonauts_core::RunId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::plan::{MappedTopic, PlanEvent};
use argonauts_core::names::{ModelRef, PlanLabel, RoleName};
use argonauts_core::session_runner::Access;
use argonauts_core::steps::plan::{Critique, CritiqueInput, LabeledPlan};
use argonauts_core::workflow::StandardPhase;

use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::plan::context::{self, warn, workers_for};
use crate::plan::{drafts, map};
use crate::prompts::plan::CRITIQUE;
use crate::workflow::{StepCall, StepDone};

/// The fixed checklist every critic gets. No setting changes it.
pub const CHECKLIST: &[&str] = &[
    "Does it meet every success criterion?",
    "What breaks first under load or failure?",
    "What is hard to test?",
    "What is more complex than it needs to be?",
    "What does it cost to run?",
];

/// `critique.critic` value that rotates the designated critic across runs.
const ROTATE: &str = "rotate";

/// This run's designated critic, advancing the rotation stored in `crew_json`. Asking again in
/// the same run gives the same critic. Other keys in the file are kept.
pub fn designated(
    crew_json: &Path,
    run: &RunId,
    critics: &[ModelRef],
) -> EngineResult<Option<ModelRef>> {
    if critics.is_empty() {
        return Ok(None);
    }
    let mut file: Map<String, Value> = match std::fs::read(crew_json) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Map::new(),
        Err(error) => return Err(error.into()),
    };
    let rotation = file.get("critic");
    let last = rotation
        .and_then(|r| r.get("last"))
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<ModelRef>().ok());
    let same_run =
        rotation.and_then(|r| r.get("run")).and_then(Value::as_str) == Some(run.as_str());
    let at = last
        .as_ref()
        .and_then(|last| critics.iter().position(|c| c == last));
    let next = match (same_run, at) {
        (true, Some(at)) => return Ok(Some(critics[at].clone())),
        (false, Some(at)) => critics[(at + 1) % critics.len()].clone(),
        (_, None) => critics[0].clone(),
    };
    file.insert(
        "critic".to_owned(),
        json!({ "last": next.to_string(), "run": run.as_str() }),
    );
    if let Some(parent) = crew_json.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(crew_json, serde_json::to_vec_pretty(&Value::Object(file))?)?;
    Ok(Some(next))
}

/// `state/<repo>/crew.json`, beside the repository's `runs/`.
pub fn crew_json(host: &Host) -> PathBuf {
    let run_dir = &host.repo().run_dir;
    run_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(run_dir)
        .join("crew.json")
}

/// The plan on the majority side of the most disputed topics, ties by label: the one a
/// designated critic argues against.
pub fn leading(topics: &[MappedTopic], labels: &[PlanLabel]) -> Option<PlanLabel> {
    let mut wins: BTreeMap<PlanLabel, u32> = labels.iter().map(|l| (*l, 0)).collect();
    for topic in topics.iter().filter(|t| t.disputed) {
        let mut sides: BTreeMap<String, Vec<PlanLabel>> = BTreeMap::new();
        for position in &topic.positions {
            let side = position.summary.trim().to_lowercase();
            sides.entry(side).or_default().push(position.label);
        }
        let largest = sides.values().map(Vec::len).max().unwrap_or(0);
        let majorities: Vec<&Vec<PlanLabel>> = sides
            .values()
            .filter(|side| side.len() == largest)
            .collect();
        if largest > 1 && majorities.len() == 1 {
            for label in majorities[0] {
                if let Some(count) = wins.get_mut(label) {
                    *count += 1;
                }
            }
        }
    }
    wins.into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
        .map(|(label, _)| label)
}

/// `critique` in planning: one round of critique of the disputed topics.
pub async fn run(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let rounds = host.settings().critique.rounds;
    if rounds == 0 {
        return Ok(StepDone::skipped("Critique is off"));
    }
    let (round, latest, disputed, done, this_round) = host.read(|state| {
        let plan = &state.plan;
        let round = plan.current_round();
        (
            round,
            plan.latest().into_iter().cloned().collect::<Vec<_>>(),
            plan.disputed().into_iter().cloned().collect::<Vec<_>>(),
            plan.critique_rounds(),
            plan.critiques_of_round(round).count(),
        )
    });
    if latest.is_empty() && call.standard != StandardPhase::Plan {
        return critique_claim(host, call).await;
    }
    if this_round > 0 {
        return Ok(StepDone::done(format!(
            "{this_round} critics already critiqued"
        )));
    }
    if latest.len() < 2 {
        return Ok(StepDone::skipped("Fewer than two plans to critique"));
    }
    if disputed.is_empty() {
        return Ok(StepDone::skipped("No disputed topics"));
    }
    if done >= rounds {
        return Ok(StepDone::skipped(format!(
            "All {rounds} critique rounds are done"
        )));
    }
    let labels: Vec<PlanLabel> = latest.iter().map(|d| d.label).collect();
    let leading = leading(&disputed, &labels).unwrap_or(labels[0]);
    let leading_author = latest
        .iter()
        .find(|d| d.label == leading)
        .map(|d| d.worker.clone());
    let mut critics = workers_for(call, host.settings(), RoleName::Critic, None);
    let designated = pick_designated(host, &critics, leading_author.as_ref())?;
    if let Some(designated) = &designated
        && !critics.contains(designated)
    {
        critics.push(designated.clone());
    }
    let plans = drafts::labeled(&latest.iter().collect::<Vec<_>>())?;
    let steps: Vec<_> = disputed.iter().map(map::to_step_topic).collect();
    let goal = host.read(context::goal_brief);
    let asks = critics.iter().map(|critic| {
        let is_designated = designated.as_ref() == Some(critic);
        let input = CritiqueInput {
            goal: goal.clone(),
            plans: plans.clone(),
            disputed: steps.clone(),
            checklist: CHECKLIST.iter().map(|s| (*s).to_owned()).collect(),
            designated: is_designated,
            leading: is_designated.then_some(leading),
        };
        let mut ask = Ask::new(
            Some(call.at.clone()),
            RoleName::Critic,
            critic.clone(),
            CRITIQUE,
            input,
        );
        ask.access = Access::NoTools;
        ask.blind = true;
        async move { (critic, is_designated, host.ask::<Critique>(ask).await) }
    });
    let mut raised = 0;
    let mut heard = 0;
    for (critic, is_designated, answer) in join_all(asks).await {
        let answered = match answer {
            Ok(answered) => answered,
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            Err(error) => {
                warn(
                    host,
                    None,
                    format!("{critic}'s critique failed and is skipped: {error}"),
                )?;
                continue;
            }
        };
        // A critic's points on its own plan are dropped: authors go easy on themselves.
        let own: Vec<PlanLabel> = latest
            .iter()
            .filter(|d| &d.worker == critic)
            .map(|d| d.label)
            .collect();
        let points: Vec<_> = answered
            .output
            .points
            .into_iter()
            .filter(|p| !own.contains(&p.plan) && labels.contains(&p.plan))
            .collect();
        raised += points.len();
        heard += 1;
        host.record(
            None,
            Some(&answered.session),
            RunEventKind::Plan(PlanEvent::Critiqued {
                critic: critic.clone(),
                designated: is_designated,
                points,
            }),
        )?;
    }
    if heard == 0 {
        return Ok(StepDone::skipped(format!(
            "No critic answered in round {round}; the round is skipped"
        )));
    }
    let designated = designated.map(|d| format!(", {d} argued against Plan {leading}"));
    Ok(StepDone::done(format!(
        "{heard} critics raised {raised} points{}",
        designated.unwrap_or_default()
    )))
}

/// The designated critic, never the leading plan's author.
fn pick_designated(
    host: &Host,
    critics: &[ModelRef],
    leading_author: Option<&ModelRef>,
) -> EngineResult<Option<ModelRef>> {
    let setting = host.settings().critique.critic.trim();
    if setting != ROTATE
        && let Ok(fixed) = setting.parse::<ModelRef>()
        && Some(&fixed) != leading_author
    {
        return Ok(Some(fixed));
    }
    let eligible: Vec<ModelRef> = critics
        .iter()
        .filter(|c| Some(*c) != leading_author)
        .cloned()
        .collect();
    match designated(&crew_json(host), host.run_id(), &eligible) {
        Ok(critic) => Ok(critic),
        Err(error) => {
            warn(
                host,
                None,
                format!(
                    "The critic rotation couldn't be read, so the first critic argues: {error}"
                ),
            )?;
            Ok(eligible.into_iter().next())
        }
    }
}

/// Outside planning: one critic tries to disprove the step's input.
async fn critique_claim(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let Some(critic) = workers_for(call, host.settings(), RoleName::Critic, Some(1))
        .into_iter()
        .next()
    else {
        return Ok(StepDone::skipped("No critic is available"));
    };
    let claim = context::step_input(host, call).await?;
    let input = CritiqueInput {
        goal: host.read(context::goal_brief),
        plans: vec![LabeledPlan {
            label: 1,
            text: claim,
        }],
        disputed: Vec::new(),
        checklist: CHECKLIST.iter().map(|s| (*s).to_owned()).collect(),
        designated: true,
        leading: Some(1),
    };
    let mut ask = Ask::new(
        Some(call.at.clone()),
        RoleName::Critic,
        critic.clone(),
        CRITIQUE,
        input,
    );
    ask.access = Access::NoTools;
    ask.blind = true;
    let answered = host.ask::<Critique>(ask).await?;
    let points = answered.output.points;
    let summary = format!("{critic} raised {} points against the claim", points.len());
    host.record(
        None,
        Some(&answered.session),
        RunEventKind::Plan(PlanEvent::Critiqued {
            critic,
            designated: true,
            points,
        }),
    )?;
    Ok(StepDone::done(summary))
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::plan::MappedPosition;

    use super::*;

    fn workers(names: &[&str]) -> Vec<ModelRef> {
        names.iter().map(|n| n.parse().unwrap()).collect()
    }

    fn run(n: u32) -> RunId {
        RunId::new(format!("run-{n:02}")).unwrap()
    }

    #[test]
    fn the_designated_critic_rotates_across_runs_and_stays_within_one() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state/repo-x/crew.json");
        let critics = workers(&["claude-code", "codex", "antigravity-cli"]);
        let pick = |n| {
            designated(&file, &run(n), &critics)
                .unwrap()
                .unwrap()
                .to_string()
        };
        assert_eq!(pick(1), "claude-code");
        assert_eq!(pick(1), "claude-code");
        assert_eq!(pick(2), "codex");
        assert_eq!(pick(3), "antigravity-cli");
        assert_eq!(pick(4), "claude-code");
    }

    #[test]
    fn the_rotation_keeps_other_keys_and_survives_a_changed_crew() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("crew.json");
        std::fs::write(
            &file,
            r#"{"routing":{"x":1},"critic":{"last":"gone","run":"run-01"}}"#,
        )
        .unwrap();
        let critics = workers(&["codex"]);
        assert_eq!(
            designated(&file, &run(2), &critics).unwrap(),
            Some("codex".parse().unwrap())
        );
        let saved: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(saved["routing"]["x"], 1);
        assert_eq!(designated(&file, &run(3), &[]).unwrap(), None);
    }

    #[test]
    fn the_leading_plan_holds_the_most_majority_positions() {
        let position = |label, summary: &str| MappedPosition {
            label,
            summary: summary.to_owned(),
            quote: summary.to_owned(),
            quote_verified: true,
        };
        let topic = |name: &str, positions| MappedTopic {
            name: name.to_owned(),
            disputed: true,
            positions,
        };
        let topics = vec![
            topic(
                "counters",
                vec![
                    position(1, "Redis"),
                    position(2, "memory"),
                    position(3, "redis "),
                ],
            ),
            topic(
                "middleware",
                vec![
                    position(1, "after auth"),
                    position(2, "before auth"),
                    position(3, "after auth"),
                ],
            ),
            topic("errors", vec![position(1, "429"), position(2, "503")]),
        ];
        assert_eq!(leading(&topics, &[1, 2, 3]), Some(1));
        assert_eq!(leading(&topics[2..], &[1, 2]), Some(1));
        assert_eq!(leading(&[], &[]), None);
    }

    #[test]
    fn the_checklist_is_fixed() {
        assert_eq!(CHECKLIST.len(), 5);
        assert!(CHECKLIST[0].contains("success criterion"));
    }
}
