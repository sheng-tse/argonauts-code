//! Shared types and traits for Argonauts Code. Older modules keep their root re-exports until
//! nothing uses them; newer ones are reached by path.

pub mod adapter;
pub mod agent_goal;
pub mod agent_panel;
pub mod agent_sessions;
pub mod agents;
pub mod answer_format;
pub mod api;
pub mod app;
pub mod artifacts;
pub mod asking;
pub mod build_output;
pub mod cheapest;
pub mod cli_commands;
pub mod cli_sessions;
pub mod clock;
pub mod confine;
pub mod control;
pub mod crew_stats;
pub mod display_names;
pub mod driver;
pub mod engine;
pub mod event;
pub mod events;
pub mod fallback;
pub mod future;
pub mod goal;
pub mod hash;
pub mod hidden_git;
pub mod ids;
pub mod input;
pub mod journal;
pub mod keywords;
pub mod labels;
pub mod layout;
pub mod leave_out;
pub mod limits;
pub mod log;
pub mod main_sessions;
pub mod meter;
pub mod model_catalog;
pub mod model_memory;
pub mod model_picker;
pub mod names;
pub mod overview;
pub mod patch_files;
pub mod paths;
pub mod pictures;
pub mod plan_usage;
pub mod policy;
pub mod pricing;
pub mod prompt;
pub mod prompt_files;
pub mod prompt_list;
pub mod prompt_parts;
pub mod redact;
pub mod registry;
pub mod required;
pub mod runtimes;
pub mod sanitize;
pub mod screen;
pub mod server_requests;
pub mod session;
pub mod session_runner;
pub mod settings;
pub mod sign_in;
pub mod status_tab;
pub mod steps;
pub mod task;
pub mod template_list;
pub mod templates;
pub mod text;
pub mod unborn;
pub mod views;
pub mod window_size;
pub mod workflow;
pub mod workflow_files;
pub mod workflow_page;

pub use adapter::BannerAccount;
pub use adapter::{
    Adapter, Capabilities, KNOWN_AGENT_IDS, LaunchTweak, SetupHints, StopPlan, StopSignal,
    StopStep, StreamParser, TweakEffect, UltraMode, UltraOn, extract_version,
};
pub use event::{
    AgentEvent, ErrorCategory, MessageStage, PluginOutputKind, ReportedCommand, ReportedModel,
    RetryReason, TodoItem, TodoStatus,
};
pub use hash::{canonical_json, sha256_hex};
pub use ids::{IdError, RepoId, RunId, TaskId};
pub use log::{
    Approval, ApprovalKind, CheckResult, CommitInfo, DecidedBy, DecisionRecord, DiffStat, EventLog,
    EventLogError, MergeDecision, MergeTask, Note, NoteLevel, Phase, PhaseEvent, PhaseStatus,
    ReadBack, RunEvent, RunEventKind, RunFinished, RunOutcome, RunStarted, SessionEnded,
    SessionStarted, SkippedLine, StopCause,
};
pub use meter::{
    DEFAULT_SOFT_RATIO, LimitDimension, LimitUnit, MeterAction, MeterLimit, Spend, SpendMeter,
    SpendStatus, TokenPrice, WorkLimits, WorkUsed,
};
pub use paths::{
    NoHomeDir, Paths, argonauts_home, create_private_dir_all, global_config_path,
    project_config_path, project_local_config_path, run_dir, state_dir, worktree_dir,
    write_private_file,
};
pub use pricing::builtin_price;
pub use prompt::{WRAP_UP_MESSAGE, render_captain_prompt};
pub use session::{
    Asks, AuthMode, CommandSpec, PARENT_SESSION_ENV_VARS, PROVIDER_KEY_ENV_VARS, SessionRequest,
    SessionRole, SpecFile, env_remove_except,
};
pub use settings::ui::{KeyScheme, Verbosity};
pub use task::{Report, ReportStatus, Task, report_json_schema, title_from_task};
