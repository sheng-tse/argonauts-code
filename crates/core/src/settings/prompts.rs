//! Who writes what an agent reads, and the two prompt files a learned workflow may replace.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

use crate::names::{ModelRef, RoleName};
use crate::settings::crew::CrewRole;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PromptSettings {
    /// Who writes each agent's instructions: `orchestrator` (its own words for that agent, the
    /// default), `built-in` (Argonauts Code's own templates), or a model such as `codex/gpt-5.6`.
    pub writer: Writer,
    /// A Markdown file in the repo that replaces the drafting prompt. Empty uses the built-in.
    pub drafting: String,
    /// A Markdown file in the repo that replaces the build prompt. Empty uses the built-in.
    pub build: String,
    /// One role can be different, as `[prompts.judge]` does, by a crew role's name (`worker`,
    /// `reviewer`, `judge`, `orchestrator`, `main`) or a finer role's own.
    #[serde(flatten)]
    pub roles: BTreeMap<String, RolePrompts>,
}

/// What one role's table under `[prompts]` may set.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RolePrompts {
    /// Who writes this role's instructions, over `prompts.writer`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writer: Option<Writer>,
    /// What a request said for this role, such as "focus on date parsing". A request sets it
    /// for its own run, and the role's agents read it last.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
}

impl PromptSettings {
    /// Who writes `role`'s instructions: the role's own table, then the table of the crew role
    /// it follows, then `prompts.writer`.
    pub fn writer_for(&self, role: RoleName) -> &Writer {
        [role.as_str(), CrewRole::read_by(role).as_str()]
            .into_iter()
            .find_map(|name| self.roles.get(name)?.writer.as_ref())
            .unwrap_or(&self.writer)
    }

    /// What the request said for `role`: for the crew role it follows, then for the role
    /// itself, since the narrower words come last.
    pub fn request_for(&self, role: RoleName) -> Vec<String> {
        let mut names = vec![CrewRole::read_by(role).as_str(), role.as_str()];
        names.dedup();
        names
            .into_iter()
            .filter_map(|name| self.roles.get(name)?.request.as_deref())
            .map(str::trim)
            .filter(|words| !words.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// The key a request's words for `role` are kept under for its run.
    pub fn request_key(role: CrewRole) -> String {
        format!("prompts.{}.request", role.as_str())
    }

    /// Who writes for a crew role as a whole, which is what `/prompts` lists.
    pub fn writer_of(&self, role: CrewRole) -> &Writer {
        self.roles
            .get(role.as_str())
            .and_then(|table| table.writer.as_ref())
            .unwrap_or(&self.writer)
    }
}

/// Who writes an agent's instructions. Written as `orchestrator`, `built-in`, or a model such
/// as `codex/gpt-5.6`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Writer {
    /// The orchestrator's own words for the agent. Where it wrote none, Argonauts Code's template.
    #[default]
    Orchestrator,
    /// Argonauts Code's own templates, as they are or as you changed them.
    BuiltIn,
    /// This model writes each agent's instructions before the agent starts.
    Model(ModelRef),
}

impl Writer {
    pub const ORCHESTRATOR: &'static str = "orchestrator";
    pub const BUILT_IN: &'static str = "built-in";

    /// What `/prompts` says under "written by".
    pub fn shown(&self) -> String {
        match self {
            Self::Orchestrator => "the orchestrator".to_owned(),
            Self::BuiltIn => "built in".to_owned(),
            Self::Model(model) => model.to_string(),
        }
    }
}

impl fmt::Display for Writer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Orchestrator => f.write_str(Self::ORCHESTRATOR),
            Self::BuiltIn => f.write_str(Self::BUILT_IN),
            Self::Model(model) => fmt::Display::fmt(model, f),
        }
    }
}

impl TryFrom<String> for Writer {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let value = value.trim();
        if value.eq_ignore_ascii_case(Self::ORCHESTRATOR) {
            return Ok(Self::Orchestrator);
        }
        if value.eq_ignore_ascii_case(Self::BUILT_IN) || value.eq_ignore_ascii_case("built_in") {
            return Ok(Self::BuiltIn);
        }
        value.parse().map(Self::Model).map_err(|_| {
            format!(
                "expected `orchestrator`, `built-in`, or a model such as `codex/gpt-5.6`, not \
                 {value:?}"
            )
        })
    }
}

impl From<Writer> for String {
    fn from(value: Writer) -> String {
        value.to_string()
    }
}

impl JsonSchema for Writer {
    fn schema_name() -> Cow<'static, str> {
        "Writer".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        String::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> PromptSettings {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn the_orchestrator_writes_unless_you_say_otherwise() {
        let settings = PromptSettings::default();
        assert_eq!(settings.writer, Writer::Orchestrator);
        assert_eq!(
            settings.writer_for(RoleName::Reviewer),
            &Writer::Orchestrator
        );
    }

    #[test]
    fn one_role_can_be_different() {
        let settings = parsed(
            "writer = \"orchestrator\"\n\n[judge]\nwriter = \"built-in\"\n\n[reviewer]\nwriter = \
             \"codex/gpt-5.6\"\n\n[critic]\nwriter = \"built-in\"\n\n[main]\nwriter = \"codex/gpt-5.6\"\n",
        );
        assert_eq!(settings.writer_for(RoleName::Judge), &Writer::BuiltIn);
        assert_eq!(settings.writer_for(RoleName::Worker), &Writer::Orchestrator);
        // A finer role follows its crew role's table unless it has its own.
        let model = Writer::Model("codex/gpt-5.6".parse().unwrap());
        assert_eq!(settings.writer_for(RoleName::Reviewer), &model);
        assert_eq!(settings.writer_for(RoleName::Consultant), &model);
        assert_eq!(settings.writer_for(RoleName::Critic), &Writer::BuiltIn);
    }

    #[test]
    fn a_writer_reads_back_as_it_was_written() {
        for word in ["orchestrator", "built-in", "codex/gpt-5.6"] {
            let writer = Writer::try_from(word.to_owned()).unwrap();
            assert_eq!(writer.to_string(), word);
        }
        let settings = parsed("drafting = \"docs/draft.md\"\n\n[judge]\nwriter = \"built-in\"\n");
        let text = toml::to_string(&settings).unwrap();
        assert_eq!(parsed(&text), settings);
        assert_eq!(settings.drafting, "docs/draft.md");
    }

    #[test]
    fn a_requests_words_reach_the_roles_that_follow() {
        let settings = parsed(
            "[reviewer]\nrequest = \"focus on date parsing\"\n\n[critic]\nrequest = \"be \
             blunt\"\n",
        );
        assert_eq!(
            settings.request_for(RoleName::Reviewer),
            ["focus on date parsing"]
        );
        assert_eq!(
            settings.request_for(RoleName::Critic),
            ["focus on date parsing", "be blunt"]
        );
        assert!(settings.request_for(RoleName::Worker).is_empty());
        assert_eq!(
            PromptSettings::request_key(CrewRole::Reviewer),
            "prompts.reviewer.request"
        );
    }

    #[test]
    fn a_role_table_takes_only_what_it_knows() {
        let err = toml::from_str::<PromptSettings>("[judge]\nwritten_by = \"built-in\"\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("written_by"), "{err}");
    }
}
