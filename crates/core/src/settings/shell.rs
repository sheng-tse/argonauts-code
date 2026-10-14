//! `[roles.<role>] shell`: what a read-only role's sessions may run in a shell. A reviewer or a
//! judge that can run the checks reads their results first-hand; one with no shell can only
//! read what the run gives it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::names::RoleName;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoleShell {
    /// The `checks.run` commands and nothing else.
    Checks,
    /// No shell.
    None,
    /// Any command, inside the role's sandbox.
    Full,
}

impl RoleShell {
    /// A role's shell when its settings name none: the checks for those who judge the work.
    pub fn default_for(role: RoleName) -> Self {
        match role {
            RoleName::Reviewer | RoleName::Judge | RoleName::Adjudicator => Self::Checks,
            _ => Self::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    #[test]
    fn reviewers_and_the_judge_get_the_checks_unless_a_role_says_otherwise() {
        assert_eq!(
            RoleShell::default_for(RoleName::Reviewer),
            RoleShell::Checks
        );
        assert_eq!(RoleShell::default_for(RoleName::Judge), RoleShell::Checks);
        assert_eq!(
            RoleShell::default_for(RoleName::Orchestrator),
            RoleShell::None
        );
        let settings: Settings = toml::from_str("[roles.reviewer]\nshell = \"full\"\n").unwrap();
        let reviewer = settings.roles.get(&RoleName::Reviewer).unwrap();
        assert_eq!(reviewer.shell, Some(RoleShell::Full));
        assert!(toml::from_str::<Settings>("[roles.judge]\nshell = \"some\"\n").is_err());
    }
}
