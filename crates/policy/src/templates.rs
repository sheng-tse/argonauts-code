//! The approver's and the narrator's prompts by the names of their template files, so they can
//! be written out and replaced like every other prompt.

use argonauts_core::prompt_files::{Template, in_use};
use argonauts_core::prompt_parts::CrewRole;

pub const APPROVER: Template = Template {
    id: "approvals/approver",
    role: CrewRole::Judge,
    part: "decide an approval",
    text: crate::approver::PROMPT,
};

pub const NARRATOR: Template = Template {
    id: "run/narrator",
    role: CrewRole::Main,
    part: "narrate the run",
    text: crate::narrator::PROMPT,
};

pub const ALL: [Template; 2] = [APPROVER, NARRATOR];

/// The template as this process sends it: yours when you wrote one.
pub(crate) fn text(template: &Template) -> String {
    in_use(template.id, template.text)
}
