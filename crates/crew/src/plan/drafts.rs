//! Sealing drafts: the plan text goes to `runs/<run>/drafts/<draft>.md` (mode 0600, outside every
//! sandbox), is hashed, and recorded as `Plan::Sealed`. No drafting agent sees another's draft. The
//! structured draft sits beside it as `<draft>.json`, for dividing the work.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use argonauts_core::RunId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::plan::PlanEvent;
use argonauts_core::names::{DraftId, ModelRef, PlanLabel, SessionKey};
use argonauts_core::steps::plan::{LabeledPlan, PlanDraft};
use argonauts_core::{create_private_dir_all, sha256_hex, write_private_file};

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::state::ids::IdKind;
use crate::state::plan::SealedDraft;

pub fn seal(
    host: &Host,
    label: PlanLabel,
    worker: &ModelRef,
    round: u8,
    draft: &PlanDraft,
    session: Option<&SessionKey>,
) -> EngineResult<DraftId> {
    let id = DraftId::numbered(host.allocate(IdKind::Draft)?);
    let dir = host.repo().run_dir.join("drafts");
    create_private_dir_all(&dir)?;
    let text = render(draft);
    let path = dir.join(format!("{id}.md"));
    write_private_file(&path, text.as_bytes())?;
    write_private_file(&structured_path(&path), &serde_json::to_vec_pretty(draft)?)?;
    host.record(
        None,
        session,
        RunEventKind::Plan(PlanEvent::Sealed {
            draft: id.clone(),
            label,
            worker: worker.clone(),
            round,
            summary: draft.summary.clone(),
            path,
            sha256: sha256_hex(text.as_bytes()),
        }),
    )?;
    Ok(id)
}

fn structured_path(text: &Path) -> PathBuf {
    text.with_extension("json")
}

/// The sealed text, refused when it no longer matches the hash it was sealed with.
pub fn read_text(sealed: &SealedDraft) -> EngineResult<String> {
    let text = std::fs::read_to_string(&sealed.path)?;
    if sha256_hex(text.as_bytes()) != sealed.sha256 {
        return Err(EngineError::Refused(format!(
            "draft {} changed after it was sealed",
            sealed.draft
        )));
    }
    Ok(text)
}

/// The structured draft sealed beside the text.
pub fn load(sealed: &SealedDraft) -> EngineResult<PlanDraft> {
    let bytes = std::fs::read(structured_path(&sealed.path))?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// The sealed plans under their anonymous labels, as the mapping agent, critics, and judges see
/// them.
pub fn labeled(drafts: &[&SealedDraft]) -> EngineResult<Vec<LabeledPlan>> {
    drafts
        .iter()
        .map(|sealed| {
            Ok(LabeledPlan {
                label: sealed.label,
                text: read_text(sealed)?,
            })
        })
        .collect()
}

/// The sealed plans as agents read them, the mapping agent, critics, judges and a merger: under
/// their labels, with who wrote each hidden, so a plan's "As Claude" or its signature never
/// reaches a judge. You read them as written.
pub fn labeled_for_agents(host: &Host, drafts: &[&SealedDraft]) -> EngineResult<Vec<LabeledPlan>> {
    let lexicon = crate::council::lexicon::of_run(host);
    Ok(labeled(drafts)?
        .into_iter()
        .map(|plan| LabeledPlan {
            text: crate::council::relay::hide_who_wrote(&lexicon, &plan.text),
            ..plan
        })
        .collect())
}

/// The plans in the order `reader` reads them, drawn for it from the run, so no plan always
/// reads first and no two readers need read them alike. The labels stay the run's own, drawn
/// at random when the plans were sealed, and a resumed run draws the same order.
pub fn read_by(run: &RunId, reader: &str, plans: &[LabeledPlan]) -> Vec<LabeledPlan> {
    crate::council::letters::drawn_order(&format!("{run}\n{reader}"), plans, |plan| {
        plan.label.to_string()
    })
}

/// The sealed text as the mapping agent and judges see it. It never names the author.
pub fn render(draft: &PlanDraft) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "## Summary\n\n{}\n", draft.summary.trim());
    let _ = writeln!(text, "## Approach\n\n{}\n", draft.approach.trim());
    if !draft.pieces.is_empty() {
        text.push_str("## Pieces\n\n");
        for piece in &draft.pieces {
            let _ = writeln!(text, "### {}\n\n{}\n", piece.name, piece.description.trim());
            list_line(&mut text, "Paths", &piece.paths);
            list_line(&mut text, "Interfaces", &piece.interfaces);
            list_line(&mut text, "Depends on", &piece.depends_on);
            text.push('\n');
        }
    }
    if !draft.choices.is_empty() {
        text.push_str("## Choices\n\n");
        for choice in &draft.choices {
            let _ = writeln!(
                text,
                "- {}: {}. {}",
                choice.topic,
                choice.choice,
                choice.rationale.trim()
            );
        }
        text.push('\n');
    }
    bullets(&mut text, "Risks", &draft.risks);
    let _ = writeln!(text, "## Test strategy\n\n{}\n", draft.test_strategy.trim());
    bullets(&mut text, "New dependencies", &draft.new_dependencies);
    text.truncate(text.trim_end().len());
    text.push('\n');
    text
}

fn list_line(text: &mut String, name: &str, items: &[String]) {
    if !items.is_empty() {
        let _ = writeln!(text, "- {name}: {}", items.join(", "));
    }
}

fn bullets(text: &mut String, heading: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    let _ = writeln!(text, "## {heading}\n");
    for item in items {
        let _ = writeln!(text, "- {}", item.trim());
    }
    text.push('\n');
}

#[cfg(test)]
pub(crate) mod tests {
    use argonauts_core::steps::plan::{PlanChoice, PlanPiece};

    use super::*;

    pub(crate) fn draft(summary: &str) -> PlanDraft {
        PlanDraft {
            summary: summary.to_owned(),
            approach: "Add a middleware after auth.".to_owned(),
            pieces: vec![PlanPiece {
                name: "limiter".to_owned(),
                description: "The token bucket.".to_owned(),
                paths: vec!["src/limit/".to_owned()],
                interfaces: vec!["Limiter::check".to_owned()],
                depends_on: Vec::new(),
            }],
            choices: vec![PlanChoice {
                topic: "counters".to_owned(),
                choice: "Redis".to_owned(),
                rationale: "Survives restarts.".to_owned(),
            }],
            risks: vec!["Redis down means no limits.".to_owned()],
            test_strategy: "Unit tests for the bucket.".to_owned(),
            new_dependencies: Vec::new(),
        }
    }

    #[test]
    fn render_lists_every_part_in_order() {
        let text = render(&draft("Rate limit the API."));
        let order = [
            "## Summary",
            "Rate limit the API.",
            "## Approach",
            "## Pieces",
            "### limiter",
            "- Paths: src/limit/",
            "- Interfaces: Limiter::check",
            "## Choices",
            "- counters: Redis. Survives restarts.",
            "## Risks",
            "## Test strategy",
        ];
        let mut from = 0;
        for part in order {
            let at = text[from..]
                .find(part)
                .unwrap_or_else(|| panic!("{part}\n{text}"));
            from += at + part.len();
        }
        assert!(!text.contains("New dependencies"));
        assert!(text.ends_with("bucket.\n"));
    }

    #[test]
    fn a_changed_sealed_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dr-001.md");
        std::fs::write(&path, "the plan\n").unwrap();
        let sealed = SealedDraft {
            draft: DraftId::numbered(1),
            label: 1,
            worker: "codex".parse().unwrap(),
            round: 1,
            summary: String::new(),
            path: path.clone(),
            sha256: sha256_hex(b"the plan\n"),
            session: None,
        };
        assert_eq!(read_text(&sealed).unwrap(), "the plan\n");
        std::fs::write(&path, "the plan, edited\n").unwrap();
        assert!(matches!(read_text(&sealed), Err(EngineError::Refused(_))));
    }

    /// The same model reads the plans in its own order in each part it plays, under the run's
    /// labels, and in the same order again on resume.
    #[test]
    fn each_reader_reads_the_plans_in_an_order_of_its_own() {
        let run = RunId::new("run-01").unwrap();
        let plans: Vec<LabeledPlan> = (1..=4)
            .map(|label| LabeledPlan {
                label,
                text: format!("Plan {label}'s approach."),
            })
            .collect();
        let readers = [
            "map codex/gpt-5.6",
            "critic codex/gpt-5.6",
            "critic claude-code/opus",
            "merger codex/gpt-5.6",
            "judge codex/gpt-5.6",
        ];
        let orders: std::collections::BTreeSet<Vec<PlanLabel>> = readers
            .iter()
            .map(|reader| {
                let read = read_by(&run, reader, &plans);
                assert_eq!(read, read_by(&run, reader, &plans), "{reader} again");
                let mut labels: Vec<PlanLabel> = read.iter().map(|plan| plan.label).collect();
                let order = labels.clone();
                labels.sort_unstable();
                assert_eq!(labels, [1, 2, 3, 4], "{reader} reads every plan once");
                order
            })
            .collect();
        assert!(orders.len() > 1, "readers read the plans in other orders");
    }
}
