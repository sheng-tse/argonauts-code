//! Every built-in prompt of the crew by the name of its template file, so each can be written
//! out, listed by `/prompts`, and replaced by a file of yours.

use argonauts_core::names::RoleName;
use argonauts_core::prompt_files::Template;
use argonauts_core::prompt_parts::CrewRole;

use super::{board, card, divide, frame, goal, handoff, merge, plan, review, workflow, writer};

const fn entry(
    id: &'static str,
    role: CrewRole,
    part: &'static str,
    text: &'static str,
) -> Template {
    Template {
        id,
        role,
        part,
        text,
    }
}

/// In the order `/prompts` lists them: each role's frame, then its steps.
pub const ALL: &[Template] = &[
    entry("frames/main", CrewRole::Main, "frame", frame::MAIN),
    entry(
        "run/closing-answer",
        CrewRole::Main,
        "the closing answer",
        divide::CLOSE_RUN,
    ),
    entry(
        "run/answer-for-you",
        CrewRole::Main,
        "answer the crew for you",
        board::FOR_YOU,
    ),
    entry(
        "run/consultant",
        CrewRole::Main,
        "advise an agent",
        board::CONSULTANT,
    ),
    entry(
        "run/decide-first",
        CrewRole::Main,
        "decide, don't ask",
        board::DECIDE_FIRST,
    ),
    entry(
        "fallback/summary",
        CrewRole::Main,
        "sum up for a fallback",
        handoff::SUMMARY,
    ),
    entry(
        "frames/orchestrator",
        CrewRole::Orchestrator,
        "frame",
        frame::ORCHESTRATOR,
    ),
    entry(
        "workflow/design",
        CrewRole::Orchestrator,
        "how it plans",
        workflow::DESIGN_WORKFLOW,
    ),
    entry(
        "workflow/fix-design",
        CrewRole::Orchestrator,
        "fix its plan",
        workflow::FIX_DESIGN,
    ),
    entry(
        "workflow/write-script",
        CrewRole::Orchestrator,
        "write the workflow",
        crate::script::write::WRITE_SCRIPT,
    ),
    entry(
        "goal/restate",
        CrewRole::Orchestrator,
        "restate the goal",
        goal::RESTATE,
    ),
    entry(
        "goal/differ",
        CrewRole::Orchestrator,
        "compare restatements",
        goal::DIFFER,
    ),
    entry(
        "goal/answer",
        CrewRole::Orchestrator,
        "answer open questions",
        goal::ANSWER,
    ),
    entry(
        "goal/attempt",
        CrewRole::Orchestrator,
        "plan the next try",
        goal::ATTEMPT,
    ),
    entry(
        "plan/draft",
        CrewRole::Orchestrator,
        "draft a plan",
        plan::DRAFT,
    ),
    entry(
        "plan/revise",
        CrewRole::Orchestrator,
        "revise a plan",
        plan::REVISE,
    ),
    entry(
        "plan/map",
        CrewRole::Orchestrator,
        "map where plans differ",
        plan::MAP,
    ),
    entry(
        "plan/merge-drafts",
        CrewRole::Orchestrator,
        "merge plans",
        plan::MERGE_DRAFTS,
    ),
    entry(
        "plan/synthesize",
        CrewRole::Orchestrator,
        "sum up a step",
        plan::SYNTHESIZE,
    ),
    entry(
        "plan/coordinate",
        CrewRole::Orchestrator,
        "coordinate the crew",
        plan::COORDINATE,
    ),
    entry(
        "plan/propose-experiment",
        CrewRole::Orchestrator,
        "propose an experiment",
        plan::PROPOSE_SPIKE,
    ),
    entry(
        "divide/plan-to-tasks",
        CrewRole::Orchestrator,
        "divide the plan into tasks",
        divide::PLAN_TO_CARDS,
    ),
    entry(
        "run/review",
        CrewRole::Orchestrator,
        "review the run",
        divide::RETRO,
    ),
    entry(
        "prompts/write-instructions",
        CrewRole::Orchestrator,
        "write an agent's instructions",
        writer::WRITE_INSTRUCTIONS,
    ),
    entry("frames/workers", CrewRole::Workers, "frame", frame::WORKERS),
    entry("task/report", CrewRole::Workers, "the report", card::BUILD),
    entry(
        "task/predict",
        CrewRole::Workers,
        "predict a task",
        divide::PREDICT,
    ),
    entry(
        "task/resolve-conflict",
        CrewRole::Workers,
        "fix a merge conflict",
        card::RESOLVE_CONFLICT,
    ),
    entry(
        "merge/main-moved",
        CrewRole::Workers,
        "merge a branch that moved",
        merge::MAIN_MOVED,
    ),
    entry(
        "merge/after-main",
        CrewRole::Workers,
        "fix what a merge broke",
        merge::AFTER_MAIN,
    ),
    entry(
        "merge/commit-message",
        CrewRole::Workers,
        "write the commit message",
        merge::COMMIT_MESSAGE,
    ),
    entry(
        "plan/experiment-report",
        CrewRole::Workers,
        "report an experiment",
        plan::SPIKE_REPORT,
    ),
    entry(
        "frames/reviewers",
        CrewRole::Reviewers,
        "frame",
        frame::REVIEWERS,
    ),
    entry(
        "review/review",
        CrewRole::Reviewers,
        "review a change",
        review::REVIEW,
    ),
    entry(
        "review/adjudicate",
        CrewRole::Reviewers,
        "settle a disagreement",
        review::ADJUDICATE,
    ),
    entry(
        "review/acceptance-tests",
        CrewRole::Reviewers,
        "write acceptance tests",
        review::ACCEPTANCE_TESTS,
    ),
    entry(
        "plan/critique",
        CrewRole::Reviewers,
        "critique plans",
        plan::CRITIQUE,
    ),
    entry(
        "goal/check",
        CrewRole::Reviewers,
        "check a goal",
        goal::CHECK,
    ),
    entry("frames/judge", CrewRole::Judge, "frame", frame::JUDGE),
    entry("plan/judge", CrewRole::Judge, "score plans", plan::JUDGE),
    entry(
        "plan/rubric",
        CrewRole::Judge,
        "the scoring rubric",
        plan::RUBRIC,
    ),
    entry(
        "plan/merge-check",
        CrewRole::Judge,
        "check a merged plan",
        plan::MERGE_CHECK,
    ),
    entry(
        "plan/pick-draft",
        CrewRole::Judge,
        "pick a draft",
        plan::PICK_DRAFT,
    ),
    entry(
        "fallback/pick",
        CrewRole::Judge,
        "pick a fallback's attempt",
        handoff::PICK,
    ),
];

/// The template whose built-in text this is. A step's prompt is found by its text, so a step
/// names its prompt once, as the constant it already passes.
pub fn of(text: &str) -> Option<&'static Template> {
    ALL.iter().find(|template| template.text == text)
}

pub fn by_id(id: &str) -> Option<&'static Template> {
    ALL.iter().find(|template| template.id == id)
}

/// The name a finer role's own frame goes by under `prompts/`.
const FINER_FRAMES: [(RoleName, &str); 14] = [
    (RoleName::PhaseLead, "frames/phase-lead"),
    (RoleName::Restater, "frames/restater"),
    (RoleName::Differ, "frames/differ"),
    (RoleName::Drafter, "frames/drafter"),
    (RoleName::Mapper, "frames/mapper"),
    (RoleName::Critic, "frames/critic"),
    (RoleName::Spike, "frames/spike"),
    (RoleName::Worker, "frames/worker"),
    (RoleName::Consultant, "frames/consultant"),
    (RoleName::Reviewer, "frames/reviewer"),
    (RoleName::Adjudicator, "frames/adjudicator"),
    (RoleName::Retro, "frames/retro"),
    (RoleName::Approver, "frames/approver"),
    (RoleName::Narrator, "frames/narrator"),
];

/// A finer role's own frame, such as `frames/critic`, which starts as the frame of the crew
/// role it reads. `None` for a role that goes by a crew role's name.
///
/// These aren't in [`ALL`], so the setup writes none: each would only repeat a crew role's
/// frame. `/prompts copy frames/critic` writes the one you ask for.
pub fn finer_frame(role: RoleName) -> Option<Template> {
    let (_, id) = FINER_FRAMES.iter().find(|(finer, _)| *finer == role)?;
    let crew = CrewRole::read_by(role);
    let frame = ALL
        .iter()
        .find(|template| template.is_frame() && template.role == crew)?;
    Some(Template { id, ..*frame })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::*;

    #[test]
    fn ids_are_unique_and_every_text_is_found_by_itself() {
        let ids: BTreeSet<&str> = ALL.iter().map(|template| template.id).collect();
        assert_eq!(ids.len(), ALL.len(), "two templates share an id");
        for template in ALL {
            assert_eq!(of(template.text).map(|found| found.id), Some(template.id));
            assert!(
                !template.text.contains('\u{2014}'),
                "{}: no em dashes",
                template.id
            );
        }
    }

    #[test]
    fn every_crew_role_has_a_frame() {
        for role in CrewRole::ALL {
            let id = format!("frames/{}", role.as_str());
            let frame = by_id(&id).unwrap_or_else(|| panic!("{id} is missing"));
            assert!(frame.is_frame());
            assert_eq!(frame.role, role);
        }
    }

    /// Every finer role may have a frame of its own, by the name the assembly looks for.
    #[test]
    fn every_finer_role_has_a_frame_that_starts_as_its_crew_roles() {
        let finer: Vec<RoleName> = argonauts_core::prompt_parts::finer_roles().collect();
        assert_eq!(finer.len(), FINER_FRAMES.len());
        for role in finer {
            let frame = finer_frame(role).unwrap_or_else(|| panic!("{role:?} has no frame"));
            assert_eq!(frame.id, format!("frames/{}", role.as_str()));
            let crew = by_id(&format!("frames/{}", frame.role.as_str())).unwrap();
            assert_eq!(frame.text, crew.text);
            assert!(by_id(frame.id).is_none(), "{} is a crew role's", frame.id);
        }
        assert_eq!(finer_frame(RoleName::Judge), None);
    }

    /// Nothing is locked: a prompt file added anywhere in this crate must be listed above, or
    /// it can't be written out or replaced. The approver's default rules are the one
    /// exception, since `.argonauts/approval-rules.md` already replaces them.
    #[test]
    fn every_prompt_file_is_in_the_catalog() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect(&root, &mut files);
        assert!(files.len() >= ALL.len(), "the folder holds every template");
        for file in files {
            if file.ends_with("approvals/default-rules.md") {
                continue;
            }
            let text = std::fs::read_to_string(&file).unwrap();
            assert!(
                of(&text).is_some(),
                "{} isn't in prompts::catalog::ALL: add an entry for it",
                file.display()
            );
        }
    }

    fn collect(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect(&path, files);
            } else if path.extension().is_some_and(|ext| ext == "md") {
                files.push(path);
            }
        }
    }
}
