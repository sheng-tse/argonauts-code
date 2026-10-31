//! What an agent reads first, in order: the frame, its instructions, your file for its role,
//! and what your request said for it. Each part says where it came from, so the agent's Input
//! tab can show them apart.

use crate::base::names::RoleName;
use crate::runs::events::session::{InputPart, InputPartKind};
use crate::settings::crew::CrewRole;

/// What a prompt asks of a crew role. Every finer role reads the frame and the file of one of
/// the five unless it has its own ([`CrewRole::read_by`]).
impl CrewRole {
    /// The crew role whose frame and file `role` reads, by the kind of work it does: everyone
    /// who shapes the plan reads the orchestrator's, everyone who judges someone else's work
    /// the reviewers', and whoever signs off the judge's.
    ///
    /// This isn't whose model it runs on. A drafting agent in a council runs on a worker's model
    /// and still plans read-only, so it reads the orchestrator's frame, never the one that
    /// tells a worker to build in its worktree.
    pub fn read_by(role: RoleName) -> Self {
        match role {
            RoleName::Main | RoleName::Consultant | RoleName::Narrator => Self::Main,
            RoleName::Orchestrator
            | RoleName::TieBreaker
            | RoleName::Reader
            | RoleName::Comparer
            | RoleName::Draft
            | RoleName::Map
            | RoleName::RunReview => Self::Orchestrator,
            RoleName::Worker | RoleName::Experiment => Self::Worker,
            RoleName::Reviewer | RoleName::Critic => Self::Reviewer,
            RoleName::Judge => Self::Judge,
        }
    }

    /// The role's agents in a sentence: `the reviewers`, `the main agent`.
    pub fn the_agents(self) -> &'static str {
        match self {
            Self::Main => "the main agent",
            Self::Orchestrator => "the orchestrator",
            Self::Worker => "the workers",
            Self::Reviewer => "the reviewers",
            Self::Judge => "the judge",
        }
    }

    /// The crew role whose agents read a file named `name`, such as `roles/<name>.md`: one of
    /// the five, or the one a finer role such as `critic` reads the frame of.
    pub fn of_file(name: &str) -> Option<Self> {
        Readers::of_file(name).map(Readers::crew)
    }
}

/// Who a file of yours speaks to, by the name it goes by (`roles/<name>.md`,
/// `prompts/frames/<name>.md`, `[prompts.<name>]`): one of the crew's five roles with every
/// finer role that reads its frame, or one finer role alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Readers {
    Crew(CrewRole),
    Finer(RoleName),
}

impl Readers {
    /// A crew role's own name comes first: `reviewer` is the reviewer with the critic.
    pub fn of_file(name: &str) -> Option<Self> {
        CrewRole::parse(name)
            .map(Self::Crew)
            .or_else(|| RoleName::parse(name).map(Self::Finer))
    }

    /// The name its files go by.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Crew(role) => role.as_str(),
            Self::Finer(role) => role.as_str(),
        }
    }

    /// The readers as a screen heads their row: a crew role's people, such as `reviewers`, or
    /// a finer role's own name.
    pub fn heading(self) -> &'static str {
        match self {
            Self::Crew(role) => role.plural(),
            Self::Finer(role) => role.as_str(),
        }
    }

    /// The crew role these readers belong to.
    pub fn crew(self) -> CrewRole {
        match self {
            Self::Crew(role) => role,
            Self::Finer(role) => CrewRole::read_by(role),
        }
    }

    /// The readers in a sentence: `the reviewers`, `the critic role`.
    pub fn the_agents(self) -> String {
        match self {
            Self::Crew(role) => role.the_agents().to_owned(),
            Self::Finer(role) => format!("the {} role", role.as_str()),
        }
    }
}

impl From<CrewRole> for Readers {
    fn from(role: CrewRole) -> Self {
        Self::Crew(role)
    }
}

/// The finer roles that may have a frame and a file of their own: every role whose name
/// isn't a crew role's.
pub fn finer_roles() -> impl Iterator<Item = RoleName> {
    RoleName::ALL
        .into_iter()
        .filter(|role| CrewRole::parse(role.as_str()).is_none())
}

/// What the screen says when a part is Argonauts Code's own text.
pub const BUILT_IN: &str = "built in";

/// The frame: the role, what it may touch, and the shape of its answer. `file` is your own
/// template when you changed it.
pub fn frame(text: impl Into<String>, file: Option<String>) -> InputPart {
    part(InputPartKind::Frame, "Frame", text, file)
}

/// Instructions the orchestrator wrote for this agent and this request. `model` is the
/// orchestrator's model as the screen names it.
pub fn from_orchestrator(text: impl Into<String>, model: Option<String>) -> InputPart {
    part(
        InputPartKind::Instructions,
        "From the orchestrator",
        text,
        model,
    )
}

pub const FROM_THE_WRITER: &str = "From the writer";

/// Instructions written by the model `[prompts] writer` names.
pub fn from_writer(text: impl Into<String>, model: String) -> InputPart {
    part(
        InputPartKind::Instructions,
        FROM_THE_WRITER,
        text,
        Some(model),
    )
}

/// Argonauts Code's own instructions for the step. `file` is your own template when you changed it.
pub fn built_in(text: impl Into<String>, file: Option<String>) -> InputPart {
    part(InputPartKind::Instructions, OWN_INSTRUCTIONS, text, file)
}

const OWN_INSTRUCTIONS: &str = "Instructions";

/// Whether someone wrote these instructions for this agent and this request: the
/// orchestrator, a model you named, or a workflow's step. Argonauts Code's own template for a step
/// is the same for every request, so it isn't.
pub fn written_for_the_agent(part: &InputPart) -> bool {
    part.kind == InputPartKind::Instructions && part.label != OWN_INSTRUCTIONS
}

pub const FROM_THE_WORKFLOW: &str = "From the workflow";

/// What the workflow's own step says its agents do. `step` names the workflow and the step,
/// such as `council, step review`.
pub fn from_workflow(text: impl Into<String>, step: String) -> InputPart {
    part(
        InputPartKind::Instructions,
        FROM_THE_WORKFLOW,
        text,
        Some(step),
    )
}

/// The crew role a request names, in the words people use: "the reviewers", "reviewer",
/// "workers", "the judge", "the orchestrator", "the main agent".
pub fn role_named(words: &str) -> Option<CrewRole> {
    let name = words
        .trim()
        .to_ascii_lowercase()
        .trim_start_matches("the ")
        .trim_end_matches(" agent")
        .trim_end_matches(" agents")
        .to_owned();
    match name.as_str() {
        "main" => Some(CrewRole::Main),
        "orchestrator" | "planner" => Some(CrewRole::Orchestrator),
        "worker" | "workers" | "builder" | "builders" => Some(CrewRole::Worker),
        "reviewer" | "reviewers" | "critic" | "critics" => Some(CrewRole::Reviewer),
        "judge" | "judges" => Some(CrewRole::Judge),
        _ => None,
    }
}

/// The line the screen shows for a request's words to a role: `For the reviewers: focus on
/// date parsing`.
pub fn for_role_line(role: CrewRole, words: &str) -> String {
    format!("For {}: {}", role.the_agents(), words.trim())
}

/// Your file for the role, by the path the screen shows, such as `roles/reviewer.md`.
pub fn role_file(text: impl Into<String>, file: String) -> InputPart {
    part(InputPartKind::RoleFile, "Your file", text, Some(file))
}

/// The one-line form of a role file, from the setting `roles.<role>.instructions`.
pub fn role_setting(text: impl Into<String>, key: String) -> InputPart {
    part(InputPartKind::RoleFile, "Your settings", text, Some(key))
}

/// What your request said for the role.
pub fn from_request(text: impl Into<String>) -> InputPart {
    part(InputPartKind::Request, "From your request", text, None)
}

/// The line the Input tab puts above a part, such as `Frame · built in` or
/// `Your file · roles/reviewer.md`. `None` for a part that isn't one of these four.
pub fn heading(part: &InputPart) -> Option<String> {
    let from = part.source.as_deref();
    match part.kind {
        InputPartKind::Frame | InputPartKind::Instructions => {
            Some(format!("{} · {}", part.label, from.unwrap_or(BUILT_IN)))
        }
        InputPartKind::RoleFile | InputPartKind::Request => Some(match from {
            Some(from) => format!("{} · {from}", part.label),
            None => part.label.clone(),
        }),
        _ => None,
    }
}

fn part(
    kind: InputPartKind,
    label: &str,
    text: impl Into<String>,
    source: Option<String>,
) -> InputPart {
    let text = text.into();
    InputPart {
        kind,
        label: label.to_owned(),
        approx_tokens: u32::try_from(approx_tokens(&text)).unwrap_or(u32::MAX),
        text,
        source,
    }
}

/// What `text` takes of a context window, at a token per 4 characters: how Argonauts Code
/// counts what it sends.
pub fn approx_tokens(text: &str) -> u64 {
    u64::try_from(text.chars().count().div_ceil(4)).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A role file goes by the name the settings use for its role, so the two must agree.
    #[test]
    fn every_role_is_found_by_the_name_the_settings_use() {
        for role in RoleName::ALL {
            let name = serde_json::to_value(role).unwrap();
            assert_eq!(name.as_str(), Some(role.as_str()));
            assert_eq!(RoleName::parse(role.as_str()), Some(role));
            assert_eq!(
                CrewRole::of_file(role.as_str()),
                Some(CrewRole::read_by(role))
            );
        }
        assert_eq!(CrewRole::of_file("reviewers"), None);
        assert_eq!(CrewRole::of_file("reviwers"), None);
    }

    #[test]
    fn every_part_is_headed_by_where_it_came_from() {
        let headings: Vec<String> = [
            frame("You review one change.", None),
            from_orchestrator("Review the due-date change.", Some("Opus 5.5".to_owned())),
            role_file(
                "A missing test is a required change.",
                "roles/reviewer.md".to_owned(),
            ),
            from_request("focus on date parsing"),
        ]
        .iter()
        .filter_map(heading)
        .collect();
        assert_eq!(
            headings,
            [
                "Frame · built in",
                "From the orchestrator · Opus 5.5",
                "Your file · roles/reviewer.md",
                "From your request",
            ]
        );
    }

    #[test]
    fn a_changed_template_is_headed_by_its_file() {
        let changed = frame("You review.", Some("prompts/frames/reviewer.md".to_owned()));
        assert_eq!(
            heading(&changed).as_deref(),
            Some("Frame · prompts/frames/reviewer.md")
        );
        let own = built_in("Review the change.", None);
        assert_eq!(heading(&own).as_deref(), Some("Instructions · built in"));
    }

    #[test]
    fn only_words_written_for_the_agent_count_as_written() {
        let orchestrator = from_orchestrator("Review the due-date change.", None);
        let writer = from_writer("Review it for races.", "Astra".to_owned());
        assert!(written_for_the_agent(&orchestrator));
        assert!(written_for_the_agent(&writer));
        assert!(!written_for_the_agent(&built_in(
            "Review the change.",
            None
        )));
        assert!(!written_for_the_agent(&frame("You review.", None)));
    }

    #[test]
    fn a_request_names_a_role_in_plain_words() {
        for (words, role) in [
            ("the reviewers", CrewRole::Reviewer),
            ("Reviewer", CrewRole::Reviewer),
            ("workers", CrewRole::Worker),
            ("the judge", CrewRole::Judge),
            ("the main agent", CrewRole::Main),
            ("orchestrator", CrewRole::Orchestrator),
        ] {
            assert_eq!(role_named(words), Some(role), "{words}");
        }
        assert_eq!(role_named("everyone"), None);
        assert_eq!(
            for_role_line(CrewRole::Reviewer, " focus on date parsing "),
            "For the reviewers: focus on date parsing"
        );
    }

    #[test]
    fn every_role_reads_the_frame_of_one_of_the_five() {
        assert_eq!(CrewRole::read_by(RoleName::Critic), CrewRole::Reviewer);
        assert_eq!(CrewRole::read_by(RoleName::Experiment), CrewRole::Worker);
        assert_eq!(CrewRole::read_by(RoleName::Draft), CrewRole::Orchestrator);
        assert_eq!(
            CrewRole::read_by(RoleName::RunReview),
            CrewRole::Orchestrator
        );
        for role in CrewRole::ALL {
            assert_eq!(CrewRole::parse(role.as_str()), Some(role));
        }
    }

    /// A name is a crew role's first, so `reviewer` speaks to the reviewer and the critic, and a
    /// finer role is spoken to alone by the name the settings use for it.
    #[test]
    fn a_file_speaks_to_a_crew_role_or_to_one_finer_role() {
        assert_eq!(
            Readers::of_file("judge"),
            Some(Readers::Crew(CrewRole::Judge))
        );
        let critic = Readers::of_file("critic").unwrap();
        assert_eq!(critic, Readers::Finer(RoleName::Critic));
        assert_eq!(critic.crew(), CrewRole::Reviewer);
        assert_eq!(critic.the_agents(), "the critic role");
        assert_eq!(Readers::from(CrewRole::Worker).the_agents(), "the workers");
        assert_eq!(Readers::of_file("reviwers"), None);

        let finer: Vec<&str> = finer_roles().map(|role| role.as_str()).collect();
        assert_eq!(finer.len(), 10);
        assert!(finer.contains(&"critic") && finer.contains(&"tie-breaker"));
        for crew in ["main", "orchestrator", "worker", "reviewer", "judge"] {
            assert!(!finer.contains(&crew), "{crew}");
        }
    }
}
