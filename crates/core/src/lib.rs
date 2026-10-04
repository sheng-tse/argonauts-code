//! Shared types and traits for Argonauts. Older modules keep their root re-exports until
//! nothing uses them; newer ones are reached by path.

pub mod adapter;
pub mod agent_sessions;
pub mod agents;
pub mod api;
pub mod app;
pub mod asking;
pub mod budget;
pub mod build_output;
pub mod card;
pub mod clock;
pub mod config;
pub mod control;
pub mod crew_panel;
pub mod display_names;
pub mod driver;
pub mod engine;
pub mod event;
pub mod events;
pub mod future;
pub mod handoff;
pub mod hash;
pub mod ids;
pub mod input;
pub mod journal;
pub mod keywords;
pub mod labels;
pub mod layout;
pub mod limits;
pub mod log;
pub mod main_sessions;
pub mod model_catalog;
pub mod model_picker;
pub mod names;
pub mod paths;
pub mod policy;
pub mod pricing;
pub mod prompt;
pub mod redact;
pub mod registry;
pub mod sanitize;
pub mod screen;
pub mod session;
pub mod settings;
pub mod steps;
pub mod text;
pub mod vendor_commands;
pub mod vendor_sessions;
pub mod views;
pub mod worker;
pub mod workflow;

pub use adapter::{
    Adapter, Capabilities, KNOWN_AGENT_IDS, LaunchTweak, SetupHints, StopPlan, StopSignal,
    StopStep, StreamParser, TweakEffect, extract_version,
};
pub use budget::{
    BudgetAction, BudgetDimension, BudgetLimit, BudgetMeter, BudgetStatus, BudgetUnit,
    DEFAULT_SOFT_RATIO, Spend, TokenPrice, WorkLimits, WorkUsed,
};
pub use card::{Card, Report, ReportStatus, report_json_schema, title_from_task};
pub use config::{
    AgentConfig, BudgetConfig, Config, ConfigError, ConfigSources, GatesConfig, KeyScheme,
    LayerInfo, LoadedConfig, MergeConfig, MergeMode, Override, SoloConfig, UiConfig, Verbosity,
};
pub use event::{
    AgentEvent, ErrorCategory, MessageStage, ModOutputKind, ReportedCommand, ReportedModel,
    RetryReason, TodoItem, TodoStatus,
};
pub use hash::{canonical_json, sha256_hex};
pub use ids::{CardId, IdError, RepoId, RunId};
pub use log::{
    Checkpoint, CheckpointKind, CommitInfo, DecidedBy, DecisionRecord, DiffStat, EventLog,
    EventLogError, GateResult, MergeCard, MergeDecision, Note, NoteLevel, Phase, PhaseEvent,
    PhaseStatus, ReadBack, RunEvent, RunEventKind, RunFinished, RunOutcome, RunStarted,
    SessionEnded, SessionStarted, SkippedLine, StopCause,
};
pub use paths::{
    NoHomeDir, Paths, argonauts_home, create_private_dir_all, global_config_path,
    project_config_path, project_local_config_path, run_dir, state_dir, worktree_dir,
    write_private_file,
};
pub use pricing::builtin_price;
pub use prompt::{WRAP_UP_MESSAGE, render_solo_prompt};
pub use session::{
    Asks, AuthMode, CommandSpec, PARENT_SESSION_ENV_VARS, SessionRequest, SessionRole, SpecFile,
    VENDOR_KEY_ENV_VARS, env_remove_except,
};
