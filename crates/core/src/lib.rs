//! Shared types and traits for Argonauts Code. Older modules keep their root re-exports until
//! nothing uses them; newer ones are reached by path.

pub mod agents;
pub mod app;
pub mod base;
pub mod behaviors;
pub mod input;
pub mod look;
pub mod models;
pub mod prompts;
pub mod runs;
pub mod safety;
pub mod settings;
pub mod workflows;

pub use agents::adapter::BannerAccount;
pub use agents::adapter::{
    Adapter, Capabilities, KNOWN_AGENT_IDS, LaunchTweak, SetupHints, StopPlan, StopSignal,
    StopStep, StreamParser, TweakEffect, UltraMode, UltraOn, extract_version,
};
pub use agents::session::{
    Asks, AuthMode, CommandSpec, PARENT_SESSION_ENV_VARS, PROVIDER_KEY_ENV_VARS, SessionRequest,
    SessionRole, SpecFile, env_remove_except,
};
pub use base::hash::{canonical_json, sha256_hex};
pub use base::ids::{IdError, RepoId, RunId, TaskId};
pub use base::log::{
    Approval, ApprovalKind, CheckResult, CommitInfo, DecidedBy, DecisionRecord, DiffStat, EventLog,
    EventLogError, MergeDecision, MergeTask, Note, NoteLevel, Phase, PhaseEvent, PhaseStatus,
    ReadBack, RunEvent, RunEventKind, RunFinished, RunOutcome, RunStarted, SessionEnded,
    SessionStarted, SkippedLine, StopCause,
};
pub use base::paths::{
    NoHomeDir, Paths, argonauts_home, create_private_dir_all, global_config_path,
    project_config_path, project_local_config_path, run_dir, state_dir, worktree_dir,
    write_private_file,
};
pub use models::meter::{
    DEFAULT_SOFT_RATIO, LimitDimension, LimitUnit, MeterAction, MeterLimit, Spend, SpendMeter,
    SpendStatus, TokenPrice, WorkLimits, WorkUsed,
};
pub use models::pricing::builtin_price;
pub use prompts::prompt::{WRAP_UP_MESSAGE, render_captain_prompt};
pub use runs::event::{
    AgentEvent, ErrorCategory, MessageStage, PluginOutputKind, ReportedCommand, ReportedModel,
    RetryReason, TodoItem, TodoStatus,
};
pub use runs::task::{Report, ReportStatus, Task, report_json_schema, title_from_task};
pub use settings::ui::{KeyScheme, Verbosity};
