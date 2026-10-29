//! Every built-in prompt of the crew by the name of its template file, so each can be written
//! out, listed by `/prompts`, and replaced by a file of yours.

use argonauts_core::names::RoleName;
use argonauts_core::prompt_files::Template;
use argonauts_core::settings::crew::CrewRole;

use super::{
    board, council, divide, fallback, frame, goal, merge, plan, review, task, understand, workflow,
    writer,
};
use crate::council::talk;

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
        fallback::SUMMARY,
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
        "understand/read",
        CrewRole::Orchestrator,
        "read the request back",
        understand::READ,
    ),
    entry(
        "understand/compare",
        CrewRole::Orchestrator,
        "compare readings",
        understand::COMPARE,
    ),
    entry(
        "understand/answer",
        CrewRole::Orchestrator,
        "answer open questions",
        understand::ANSWER,
    ),
    entry(
        "goal/next-try",
        CrewRole::Orchestrator,
        "plan the next try",
        goal::NEXT_TRY,
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
        "map where plans disagree",
        plan::MAP,
    ),
    entry(
        "council/talk-first",
        CrewRole::Orchestrator,
        "answer alone in a council",
        talk::FIRST_ANSWER,
    ),
    entry(
        "council/talk-points",
        CrewRole::Orchestrator,
        "list the points a council agrees on",
        talk::POINTS,
    ),
    entry(
        "council/talk-message",
        CrewRole::Orchestrator,
        "write to the council",
        talk::MESSAGE,
    ),
    entry(
        "council/talk-tapback",
        CrewRole::Orchestrator,
        "react to a council's message",
        talk::TAPBACK,
    ),
    entry(
        "council/talk-private",
        CrewRole::Orchestrator,
        "answer the user in private",
        talk::PRIVATE,
    ),
    entry(
        "council/talk-plan",
        CrewRole::Orchestrator,
        "write the council's plan",
        talk::PLAN,
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
        "plan/orchestrate",
        CrewRole::Orchestrator,
        "orchestrate the crew",
        plan::ORCHESTRATE,
    ),
    entry(
        "plan/propose-experiment",
        CrewRole::Orchestrator,
        "propose an experiment",
        plan::PROPOSE_EXPERIMENT,
    ),
    entry(
        "divide/plan-to-tasks",
        CrewRole::Orchestrator,
        "divide the plan into tasks",
        divide::PLAN_TO_TASKS,
    ),
    entry(
        "run/review",
        CrewRole::Orchestrator,
        "review the run",
        divide::RUN_REVIEW,
    ),
    entry(
        "writer/write-instructions",
        CrewRole::Orchestrator,
        "write an agent's instructions",
        writer::WRITE_INSTRUCTIONS,
    ),
    entry("frames/worker", CrewRole::Worker, "frame", frame::WORKER),
    entry("task/report", CrewRole::Worker, "the report", task::BUILD),
    entry(
        "task/predict",
        CrewRole::Worker,
        "predict a task",
        divide::PREDICT,
    ),
    entry(
        "task/resolve-conflict",
        CrewRole::Worker,
        "fix a merge conflict",
        task::RESOLVE_CONFLICT,
    ),
    entry(
        "merge/main-moved",
        CrewRole::Worker,
        "merge a branch that moved",
        merge::MAIN_MOVED,
    ),
    entry(
        "merge/after-main",
        CrewRole::Worker,
        "fix what a merge broke",
        merge::AFTER_MAIN,
    ),
    entry(
        "merge/commit-message",
        CrewRole::Worker,
        "write the commit message",
        merge::COMMIT_MESSAGE,
    ),
    entry(
        "plan/experiment-report",
        CrewRole::Worker,
        "report an experiment",
        plan::EXPERIMENT_REPORT,
    ),
    entry(
        "council/seat",
        CrewRole::Worker,
        "sit in a council",
        council::SEAT,
    ),
    entry(
        "council/skeptic-note",
        CrewRole::Worker,
        "who the skeptic is",
        council::SKEPTIC_NOTE,
    ),
    entry(
        "council/proposer",
        CrewRole::Worker,
        "a proposer's role",
        council::PROPOSER,
    ),
    entry(
        "council/skeptic",
        CrewRole::Worker,
        "a skeptic's role",
        council::SKEPTIC,
    ),
    entry(
        "council/checker",
        CrewRole::Worker,
        "a checker's role",
        council::CHECKER,
    ),
    entry(
        "council/notes",
        CrewRole::Worker,
        "keep the council's notes",
        council::NOTES,
    ),
    entry(
        "council/turn",
        CrewRole::Worker,
        "a turn of the council's talk",
        council::TURN,
    ),
    entry(
        "frames/reviewer",
        CrewRole::Reviewer,
        "frame",
        frame::REVIEWER,
    ),
    entry(
        "review/review",
        CrewRole::Reviewer,
        "review a change",
        review::REVIEW,
    ),
    entry(
        "council/check",
        CrewRole::Reviewer,
        "check a council's task blind",
        council::CHECK,
    ),
    entry(
        "review/settle",
        CrewRole::Judge,
        "settle a disagreement",
        review::SETTLE,
    ),
    entry(
        "review/acceptance-tests",
        CrewRole::Reviewer,
        "write acceptance tests",
        review::ACCEPTANCE_TESTS,
    ),
    entry(
        "plan/critique",
        CrewRole::Reviewer,
        "critique plans",
        plan::CRITIQUE,
    ),
    entry(
        "goal/check",
        CrewRole::Reviewer,
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
        "pick a fallback's try",
        fallback::PICK,
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
const FINER_FRAMES: [(RoleName, &str); 10] = [
    (RoleName::TieBreaker, "frames/tie-breaker"),
    (RoleName::Reader, "frames/reader"),
    (RoleName::Comparer, "frames/comparer"),
    (RoleName::Draft, "frames/draft"),
    (RoleName::Map, "frames/map"),
    (RoleName::Critic, "frames/critic"),
    (RoleName::Experiment, "frames/experiment"),
    (RoleName::Consultant, "frames/consultant"),
    (RoleName::RunReview, "frames/run-review"),
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

    /// An id is where its file sits under `crates/core/templates/prompts/`, so the name
    /// `/prompts` shows is the one to look for on disk.
    #[test]
    fn every_template_sits_at_the_path_its_id_names() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/templates/prompts");
        for template in ALL {
            let path = root.join(format!("{}.md", template.id));
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: no file at {}: {e}", template.id, path.display()));
            assert_eq!(text, template.text, "{}", template.id);
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
}
