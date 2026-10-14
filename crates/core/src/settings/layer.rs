//! The names of the layers a setting can come from, as a `ConfigEntry` carries them.

pub const BUILT_IN: &str = "built-in";
pub const GLOBAL: &str = "global";
pub const PROJECT: &str = "project";
pub const PROJECT_LOCAL: &str = "project-local";
/// The workflow in use, which sets its own values under every file.
pub const WORKFLOW: &str = "workflow";
pub const FLAG: &str = "flag";
pub const REQUEST: &str = "request";
/// A flag this session's `/config` set, shown apart from the start's flags.
pub const SESSION: &str = "session";
