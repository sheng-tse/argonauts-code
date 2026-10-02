//! Decision records: `Plan::Decided` and `.argonauts/decisions/D-NNN-<slug>.md` in the
//! integration branch, committed by the conductor.

use std::fmt::Write as _;
use std::path::PathBuf;

use argonauts_core::events::RunEventKind;
use argonauts_core::events::plan::{DesignDecision, PlanEvent};
use argonauts_core::names::DecisionId;

use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::integrate::{branch, conductor};
use crate::plan::drafts;
use crate::state::ids::IdKind;
use crate::state::plan::SealedDraft;

/// Where decision records live, relative to the repository root.
pub const DIR: &str = ".argonauts/decisions";

/// Longest slug, so file names stay readable.
const SLUG_MAX: usize = 60;

/// Records one decision under a fresh id, ignoring the `id` and `path` it comes with. A
/// decision already recorded with the same title and choice keeps its id.
pub async fn record(host: &Host, decision: DesignDecision) -> EngineResult<DecisionId> {
    let known = host.read(|state| {
        state
            .plan
            .decisions
            .iter()
            .find(|d| d.title == decision.title && d.choice == decision.choice)
            .map(|d| d.id.clone())
    });
    if let Some(id) = known {
        return Ok(id);
    }
    let id = DecisionId::numbered(host.allocate(IdKind::Decision)?);
    let relative = format!("{DIR}/{}", file_name(&id, &decision.title));
    let worktree = branch::ensure_integration(host).await?;
    let file = worktree.join(&relative);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let decision = DesignDecision {
        id: id.clone(),
        path: Some(PathBuf::from(&relative)),
        ..decision
    };
    std::fs::write(&file, markdown(&decision))?;
    let subject = format!("Record decision {id}: {}", decision.title);
    conductor::commit(host, &worktree, &[relative.as_str()], &subject).await?;
    host.record(None, None, RunEventKind::Plan(PlanEvent::Decided(decision)))?;
    Ok(id)
}

/// Records a decision for each topic the chosen plan settles: its own choices, then the map's
/// topics it took a position on. The other plans' positions are the alternatives.
pub async fn record_for(host: &Host, chosen: &SealedDraft) -> EngineResult<Vec<DecisionId>> {
    let draft = drafts::load(chosen)?;
    let topics = host.read(|state| {
        state
            .plan
            .latest_map()
            .map(|map| map.topics.clone())
            .unwrap_or_default()
    });
    let alternatives = |topic: &str, choice: &str| -> Vec<String> {
        let mut others: Vec<String> = topics
            .iter()
            .filter(|t| t.name.eq_ignore_ascii_case(topic))
            .flat_map(|t| &t.positions)
            .filter(|p| p.label != chosen.label && !p.summary.eq_ignore_ascii_case(choice))
            .map(|p| p.summary.clone())
            .collect();
        others.sort();
        others.dedup();
        others
    };
    let mut decisions: Vec<DesignDecision> = draft
        .choices
        .iter()
        .map(|choice| DesignDecision {
            id: DecisionId::numbered(0),
            title: choice.topic.clone(),
            choice: choice.choice.clone(),
            rationale: choice.rationale.clone(),
            alternatives: alternatives(&choice.topic, &choice.choice),
            path: None,
        })
        .collect();
    for topic in &topics {
        let covered = decisions
            .iter()
            .any(|d| d.title.eq_ignore_ascii_case(&topic.name));
        let position = topic.positions.iter().find(|p| p.label == chosen.label);
        if let (false, Some(position)) = (covered, position) {
            decisions.push(DesignDecision {
                id: DecisionId::numbered(0),
                title: topic.name.clone(),
                choice: position.summary.clone(),
                rationale: format!("The chosen plan says: \"{}\"", position.quote),
                alternatives: alternatives(&topic.name, &position.summary),
                path: None,
            });
        }
    }
    let mut ids = Vec::with_capacity(decisions.len());
    for decision in decisions {
        ids.push(record(host, decision).await?);
    }
    Ok(ids)
}

/// `D-NNN-<slug>.md`, the slug from the title in lowercase words joined by dashes.
pub fn file_name(id: &DecisionId, title: &str) -> String {
    let words: Vec<String> = title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let mut slug = String::new();
    for word in words {
        let extra = if slug.is_empty() { 0 } else { 1 };
        if slug.len() + extra + word.len() > SLUG_MAX {
            break;
        }
        if extra == 1 {
            slug.push('-');
        }
        slug.push_str(&word);
    }
    if slug.is_empty() {
        slug.push_str("decision");
    }
    format!("{id}-{slug}.md")
}

fn markdown(decision: &DesignDecision) -> String {
    let mut text = format!("# {}: {}\n\n", decision.id, decision.title);
    let _ = writeln!(text, "Choice: {}\n", decision.choice);
    let _ = writeln!(text, "{}\n", decision.rationale.trim());
    if !decision.alternatives.is_empty() {
        text.push_str("## Alternatives\n\n");
        for alternative in &decision.alternatives {
            let _ = writeln!(text, "- {alternative}");
        }
    }
    text.truncate(text.trim_end().len());
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_numbered_slugs() {
        let id = DecisionId::numbered(3);
        assert_eq!(
            file_name(&id, "Where counters live"),
            "D-003-where-counters-live.md"
        );
        assert_eq!(
            file_name(&id, "  Redis vs. in-memory (v2)!  "),
            "D-003-redis-vs-in-memory-v2.md"
        );
        assert_eq!(file_name(&id, "¿?"), "D-003-decision.md");
    }

    #[test]
    fn long_titles_are_cut_at_a_word() {
        let title = "word ".repeat(40);
        let name = file_name(&DecisionId::numbered(12), &title);
        let slug = name
            .strip_prefix("D-012-")
            .and_then(|s| s.strip_suffix(".md"))
            .unwrap();
        assert!(slug.len() <= SLUG_MAX);
        assert!(slug.split('-').all(|w| w == "word"));
    }

    #[test]
    fn the_record_names_its_choice_and_alternatives() {
        let text = markdown(&DesignDecision {
            id: DecisionId::numbered(1),
            title: "counters".to_owned(),
            choice: "Redis".to_owned(),
            rationale: "Survives restarts.".to_owned(),
            alternatives: vec!["in memory".to_owned()],
            path: None,
        });
        assert_eq!(
            text,
            "# D-001: counters\n\nChoice: Redis\n\nSurvives restarts.\n\n## Alternatives\n\n- in memory\n"
        );
    }
}
