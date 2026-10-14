//! The judge's and the narrator's prompts by the names of their template files, so they can
//! be written out and replaced like every other prompt.

use argonauts_core::prompt_files::{Template, in_use};
use argonauts_core::settings::crew::CrewRole;

pub const JUDGE: Template = Template {
    id: "approvals/judge",
    role: CrewRole::Judge,
    part: "decide an approval",
    text: crate::judge::PROMPT,
};

pub const NARRATOR: Template = Template {
    id: "run/narrator",
    role: CrewRole::Main,
    part: "narrate the run",
    text: crate::narrator::PROMPT,
};

pub const ALL: [Template; 2] = [JUDGE, NARRATOR];

/// The template as this process sends it: yours when you wrote one.
pub(crate) fn text(template: &Template) -> String {
    in_use(template.id, template.text)
}
