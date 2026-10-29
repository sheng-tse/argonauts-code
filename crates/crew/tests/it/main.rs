//! The tests that use this crate from outside, one module per file, built as one program so
//! that a change links it once instead of once per file.

mod support;

mod answer_formats;
mod awkward_repos;
mod engine_agent_failures;
mod engine_approvals;
mod engine_autonomy;
mod engine_autopilot;
mod engine_blackboard;
mod engine_captain;
mod engine_claude_codes;
mod engine_core;
mod engine_declared;
mod engine_delivery;
mod engine_divide;
mod engine_fallback;
mod engine_fallback_context;
mod engine_fallback_order;
mod engine_goals;
mod engine_learning;
mod engine_merge;
mod engine_merging;
mod engine_orchestrated_rules;
mod engine_plan;
mod engine_relaunch;
mod engine_replay;
mod engine_restart;
mod engine_review;
mod engine_run_review;
mod engine_script;
mod engine_sessions;
mod engine_status;
mod engine_task_goals;
mod engine_tasks;
mod engine_understand;
mod engine_until;
mod engine_workflow;
mod engine_workflow_decisions;
mod engine_workflow_files;
mod engine_wrap_up;
mod issue_draft;
mod pr_follow;
mod task_tries;
mod unmerged_endings;
