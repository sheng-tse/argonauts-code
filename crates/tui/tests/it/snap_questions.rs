use crate::common;
use std::path::PathBuf;

use argonauts_core::app::{AppChoice, ChoiceKind};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::agent_status::FallbackPlan;
use argonauts_core::events::approval::{
    AgentDownPacket, AnswerAction, AnswerOption, ApprovalDecided, ApprovalKind, ApprovalOpened,
    ApprovalPacket, FreeText, GoalPacket, LimitPacket, MergePacket, MergeRow,
    OriginalReturnedPacket, PausedTask, PlanPacket, QuestionPacket, StuckPacket,
};
use argonauts_core::events::goal::{
    Assumption, ChoiceOption, Criterion, GoalFile, GoalQuestion, GoalSource,
};
use argonauts_core::events::plan::{
    DesignDecision, JudgingResult, LimitLine, LimitPlan, PlanTally, TaskGraph,
};
use argonauts_core::events::run::Party;
use argonauts_core::events::task::{TaskKind, TaskLimit, TaskSpec, TaskState};
use argonauts_core::fallback::QualityDebt;
use argonauts_core::limits::{Candidate, CostEstimate, TrackRecord};
use argonauts_core::meter::{MeterAction, MeterLimit, WorkLimits, WorkUsed};
use argonauts_core::names::{
    AgentId, ApprovalId, ChoiceId, DecisionId, ModelRef, RoleName, SessionKey,
};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::fallback::{FallbackMode, FallbackStrategy};
use argonauts_core::settings::limit::{LimitAmount, LimitLineKind, LimitUnit};
use argonauts_core::settings::protocol::JudgingMethod;
use argonauts_core::steps::StepName;
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::steps::understand::DiffTag;
use argonauts_core::{DiffStat, LimitDimension, RunId, Spend, SpendStatus, TaskId};
use argonauts_tui::HistoryCell;
use argonauts_tui::model::QuestionPrompt;
use argonauts_tui::questions::{
    about_cells, approval_question, choice_question, confirm_take_over, record_cell,
    spot_check_question as spot_question,
};
use argonauts_tui::stability::{self, WIDTHS, samples};
use argonauts_tui::state::{RunState, SessionState, TaskRow};
use common::{assert_fits, still, text, theme};
use ratatui::text::Line;
use time::macros::datetime;

const SNAPSHOT_WIDTHS: [u16; 2] = [80, 120];

fn run_id() -> RunId {
    RunId::new("run-01").unwrap()
}

fn task(n: u32) -> TaskId {
    TaskId::new(format!("task-{n:03}")).unwrap()
}

fn worker(text: &str) -> ModelRef {
    text.parse().unwrap()
}

fn stat(files: u32, insertions: u32, deletions: u32) -> DiffStat {
    DiffStat {
        files_changed: files,
        insertions,
        deletions,
    }
}

fn usd(amount: f64) -> LimitAmount {
    LimitAmount {
        unit: LimitUnit::Usd,
        amount,
    }
}

fn option(key: char, label: &str, action: AnswerAction) -> AnswerOption {
    AnswerOption {
        key,
        label: label.into(),
        action,
        recommended: false,
        detail: None,
    }
}

fn recommended(mut option: AnswerOption) -> AnswerOption {
    option.recommended = true;
    option
}

fn detailed(mut option: AnswerOption, detail: &str) -> AnswerOption {
    option.detail = Some(detail.into());
    option
}

fn choose(key: char, label: &str, value: &str) -> AnswerOption {
    option(
        key,
        label,
        AnswerAction::Choose {
            value: value.into(),
        },
    )
}

fn approval(
    kind: ApprovalKind,
    title: &str,
    packet: ApprovalPacket,
    options: Vec<AnswerOption>,
) -> ApprovalOpened {
    ApprovalOpened {
        id: ApprovalId::numbered(1),
        kind,
        title: title.into(),
        task: None,
        packet,
        evidence_sha256: "e3b0c442".into(),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        options,
        free_text: FreeText::Guidance,
        needs_you: true,
    }
}

/// A run whose tasks have workers, for the `Agent:` line.
fn run_state() -> RunState {
    let mut run = RunState::new(run_id());
    // Each task's item is the name its plan gave it, which its worker's label uses.
    for (n, title, item, who) in [
        (2, "Redis store", "redis-store", "codex/gpt-5.6"),
        (4, "Parser", "parser", "codex/gpt-5.6"),
    ] {
        run.tasks.insert(
            task(n),
            TaskRow {
                id: task(n),
                title: title.into(),
                state: Some(TaskState::Running),
                worker: Some(worker(who)),
                try_number: 3,
                diff: None,
                fallback_for: None,
                debt: Vec::new(),
                checkpoints: 0,
                rotations: 0,
                last_check: None,
                reviews: 0,
                waiting_on_agent: None,
                item: Some(item.into()),
            },
        );
    }
    run
}

fn goal_file() -> GoalFile {
    GoalFile {
        run: run_id(),
        version: 1,
        summary: "Add a --json flag to the export command".into(),
        criteria: vec![Criterion::new(
            "G1",
            "`export --json` writes one JSON object per row",
        )],
        in_scope: vec!["the export command".into()],
        out_of_scope: vec!["the import command".into()],
        assumptions: vec![Assumption {
            text: "Rows keep their CSV column order".into(),
            basis: "a detail, and 2 of 3 read it that way".into(),
            by: None,
        }],
        answers: Vec::new(),
        source: GoalSource::Readings,
        path: PathBuf::from(".argonauts/goal/run-01.md"),
        sha256: "e3b0".into(),
    }
}

fn goal_question() -> ApprovalOpened {
    let question = |n: u8, text: &str, tag: DiffTag| GoalQuestion {
        n,
        of: 2,
        text: text.into(),
        options: vec![
            ChoiceOption {
                key: 'a',
                label: "Per API key".into(),
                recommended: true,
                basis: Some("2 of 3 readings read it this way".into()),
            },
            ChoiceOption {
                key: 'b',
                label: "Per user".into(),
                recommended: false,
                basis: Some("1 of 3 read it this way".into()),
            },
        ],
        tag,
        topic: None,
    };
    approval(
        ApprovalKind::Understand,
        "What should the goal say? · 1 of 2",
        ApprovalPacket::Understand(GoalPacket {
            goal: goal_file(),
            agreements: vec!["Rate limit the public API".into()],
            assumptions: vec![Assumption {
                text: "Limits reset every minute".into(),
                basis: "a detail, and 2 of 3 read it that way".into(),
                by: None,
            }],
            questions: vec![
                question(
                    1,
                    "Should the limit apply per API key or per user?",
                    DiffTag::Scope,
                ),
                question(2, "Should blocked calls be logged?", DiffTag::Data),
            ],
        }),
        vec![
            recommended(choose('a', "Per API key", "Per API key")),
            choose('b', "Per user", "Per user"),
            option(' ', "Type my own answer", AnswerAction::Answer),
        ],
    )
}

fn understand_approval() -> ApprovalOpened {
    let mut opened = goal_question();
    if let ApprovalPacket::Understand(goal) = &mut opened.packet {
        goal.questions.clear();
    }
    opened.title = "Approve this goal?".into();
    opened.free_text = FreeText::Edits;
    opened.options = vec![
        option('y', "Approve the goal", AnswerAction::Approve),
        option('e', "Approve with edits", AnswerAction::ApproveWithEdits),
        option('b', "Send back for another round", AnswerAction::SendBack),
        option('x', "Abandon", AnswerAction::Abandon),
        option('d', "Show the details", AnswerAction::ShowDiff),
    ];
    opened
}

fn spec(n: u32, title: &str, who: &str, reviewer: &str, depends_on: &[u32]) -> TaskSpec {
    TaskSpec {
        id: task(n),
        title: title.into(),
        task: title.into(),
        kind: TaskKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: depends_on.iter().map(|n| task(*n)).collect(),
        criteria: vec!["G1".into()],
        worker: Some(worker(who)),
        reviewers: vec![worker(reviewer)],
        limit: TaskLimit::default(),
        predictions: None,
        item: None,
    }
}

fn plan() -> ApprovalOpened {
    let tally = |label: u8, author: &str, mean: f64| PlanTally {
        label,
        author: worker(author),
        mean,
        spread: 0.4,
        counted: 2,
        dropped: 0,
    };
    let packet = PlanPacket {
        summary: "Stream rows through a JSON writer behind the existing exporter trait.".into(),
        judging: Some(JudgingResult {
            method: JudgingMethod::Cross,
            plans: vec![
                tally(1, "claude-code/opus", 7.8),
                tally(2, "codex/gpt-5.6", 7.6),
            ],
            winner: Some(1),
            margin: 0.2,
            near_tie: true,
        }),
        near_tie: true,
        decisions: vec![DesignDecision {
            id: DecisionId::numbered(1),
            title: "Output shape".into(),
            choice: "one object per line".into(),
            rationale: "streams without holding every row".into(),
            alternatives: vec!["one array".into()],
            path: None,
        }],
        graph: TaskGraph {
            tasks: vec![
                spec(
                    1,
                    "Exporter trait",
                    "claude-code/opus",
                    "codex/gpt-5.6",
                    &[],
                ),
                spec(2, "JSON writer", "codex/gpt-5.6", "claude-code/opus", &[1]),
                spec(
                    3,
                    "Wire --json into the CLI",
                    "antigravity-cli/gemini-3.6",
                    "codex/gpt-5.6",
                    &[2],
                ),
            ],
            critical_path: vec![task(1), task(2), task(3)],
        },
        acceptance_tests: vec!["export --json writes one object per row".into()],
        limit: LimitPlan {
            lines: vec![
                LimitLine {
                    line: LimitLineKind::Tasks,
                    amount: usd(6.0),
                },
                LimitLine {
                    line: LimitLineKind::Review,
                    amount: usd(2.0),
                },
            ],
            total: usd(10.0),
            estimate_low: 7.0,
            estimate_high: 9.5,
        },
        new_dependencies: vec!["serde_json".into()],
        flags: vec!["A comment in plan 2 addresses the judges".into()],
        workflow: None,
    };
    approval(
        ApprovalKind::Plan,
        "Approve this plan?",
        ApprovalPacket::Plan(Box::new(packet)),
        vec![
            option('y', "Approve the plan", AnswerAction::Approve),
            option('e', "Approve with edits", AnswerAction::ApproveWithEdits),
            option('b', "Send back", AnswerAction::SendBack),
            option('s', "Run it as Captain", AnswerAction::SwitchToCaptain),
            option('x', "Abandon", AnswerAction::Abandon),
            option('d', "Show the plan", AnswerAction::ShowDiff),
        ],
    )
}

fn merge_row(n: u32, title: &str, who: &str, review: Option<ReviewVerdict>) -> MergeRow {
    MergeRow {
        task: task(n),
        title: title.into(),
        kind: TaskKind::Build,
        stat: stat(2, 30 * n, 4 * n),
        worker: Some(worker(who)),
        checks_passed: true,
        review,
        debt: Vec::new(),
        questions: Vec::new(),
    }
}

fn merge() -> ApprovalOpened {
    let mut debt_row = merge_row(4, "Middleware", "codex/gpt-5.6", None);
    debt_row.debt = vec![QualityDebt {
        since: 1,
        by: worker("codex/gpt-5.6"),
        needs: worker("claude-code/opus"),
    }];
    let packet = MergePacket {
        target: Some("main".into()),
        branch: "argonauts/run-01/merged".into(),
        rows: vec![
            merge_row(
                1,
                "Exporter trait",
                "claude-code/opus",
                Some(ReviewVerdict::Approve),
            ),
            merge_row(
                2,
                "JSON writer",
                "codex/gpt-5.6",
                Some(ReviewVerdict::Approve),
            ),
            merge_row(
                3,
                "Wire --json into the CLI",
                "antigravity-cli/gemini-3.6",
                Some(ReviewVerdict::Approve),
            ),
            debt_row,
        ],
        suite_passed_on_latest_main: true,
        checks: Vec::new(),
        combined: stat(9, 327, 40),
        conductor: stat(1, 27, 0),
        diff_path: PathBuf::from(".argonauts/runs/run-01/merge.diff"),
        tree_sha: "4b825dc".into(),
        spent: Spend {
            cost_usd: 0.46,
            ..Spend::default()
        },
        limit: Some(usd(3.0)),
        partial: false,
        in_the_way: Vec::new(),
        edited: Vec::new(),
        refused: None,
        both_changed: Vec::new(),
        asks_which_branch: false,
        again: None,
    };
    approval(
        ApprovalKind::Merge,
        "Merge this run into main?",
        ApprovalPacket::Merge(packet),
        vec![
            option('y', "Merge", AnswerAction::Merge),
            option('p', "Open a pull request", AnswerAction::PullRequest),
            option(
                'b',
                "Request changes on some tasks",
                AnswerAction::RequestChanges,
            ),
            option('k', "Keep the branch", AnswerAction::KeepBranch),
            option('x', "Abandon the run", AnswerAction::Abandon),
            option('d', "Show the diff", AnswerAction::ShowDiff),
        ],
    )
}

fn candidate(name: &str, plan_usage: &str, passed: u32, of: u32, cost: CostEstimate) -> Candidate {
    Candidate {
        worker: worker(name),
        plan_usage_left: None,
        plan_usage_text: plan_usage.into(),
        track_record: Some(TrackRecord { passed, of }),
        cost,
        needs_confirm: matches!(cost, CostEstimate::Usd(_)),
        may_run_out: plan_usage == "10%",
    }
}

fn fallback() -> ApprovalOpened {
    let candidates = vec![
        candidate("codex/gpt-5.6", "62%", 9, 11, CostEstimate::PlanUsage(0.06)),
        candidate(
            "antigravity-cli/gemini-3.6",
            "10%",
            4,
            6,
            CostEstimate::PlanUsage(0.09),
        ),
        candidate("opencode/kimi-k3", "API key", 2, 3, CostEstimate::Usd(1.8)),
    ];
    let options = vec![
        detailed(
            option(
                'a',
                "codex/gpt-5.6",
                AnswerAction::Candidate {
                    worker: worker("codex/gpt-5.6"),
                },
            ),
            "62% left · 9 of 11 passed · ~6% of plan usage",
        ),
        detailed(
            option(
                'b',
                "antigravity-cli/gemini-3.6",
                AnswerAction::Candidate {
                    worker: worker("antigravity-cli/gemini-3.6"),
                },
            ),
            "10% left · 4 of 6 passed · ~9% of plan usage, may run out",
        ),
        detailed(
            option(
                'c',
                "opencode/kimi-k3",
                AnswerAction::Candidate {
                    worker: worker("opencode/kimi-k3"),
                },
            ),
            "API key · 2 of 3 passed · ~$1.80, you confirm",
        ),
        option('t', "Take over myself", AnswerAction::TakeOver),
        option('w', "Wait until the reset in 2h 10m", AnswerAction::Wait),
        option('o', "Pick another agent", AnswerAction::OtherCandidates),
        option('d', "Show the diff", AnswerAction::ShowDiff),
    ];
    let mut opened = approval(
        ApprovalKind::Fallback,
        "Fallback · task-003 Middleware · needs you",
        ApprovalPacket::Fallback(FallbackPlan {
            task: task(3),
            title: "Middleware".into(),
            stopped: worker("claude-code/opus"),
            reason: "usage limit, resets in 2h 10m".into(),
            resets_at: None,
            work: stat(4, 96, 12),
            checks_ran: false,
            candidates,
            mode: FallbackMode::Ask,
            strategy: FallbackStrategy::Spread,
            race_count: 1,
            also_moving: vec![task(5), task(6)],
        }),
        options,
    );
    opened.task = Some(task(3));
    opened
}

fn agent_down() -> ApprovalOpened {
    let paused = |n: u32, title: &str, role: RoleName| PausedTask {
        task: task(n),
        title: title.into(),
        role,
    };
    approval(
        ApprovalKind::AgentDown,
        "Claude Code is down",
        ApprovalPacket::AgentDown(AgentDownPacket {
            agent_id: AgentId("claude-code".into()),
            evidence: "overload errors on 3 sessions in 2 min".into(),
            test_at: Some(datetime!(2026-09-30 14:40 UTC)),
            paused: vec![
                paused(3, "Middleware", RoleName::Worker),
                paused(2, "Redis store", RoleName::Reviewer),
            ],
            fits_all: None,
            kept_by: None,
        }),
        vec![
            option(
                'a',
                "One agent for every paused task",
                AnswerAction::OneForAll,
            ),
            option('e', "Choose per task", AnswerAction::PerTask),
            option('w', "Wait for Claude Code", AnswerAction::Wait),
        ],
    )
}

fn limit() -> ApprovalOpened {
    let status = SpendStatus {
        level: MeterAction::Stop,
        limit: MeterLimit::Usd(2.0),
        spend: Spend {
            cost_usd: 2.1,
            ..Spend::default()
        },
        fraction_used: Some(1.05),
        work: WorkUsed::default(),
        work_limits: WorkLimits::default(),
        binding: Some(LimitDimension::Spend),
    };
    approval(
        ApprovalKind::Limit,
        "task-002 reached its limit",
        ApprovalPacket::Limit(LimitPacket {
            task: Some(task(2)),
            status,
            dimension: LimitDimension::Spend,
            reserve_left: Some(usd(1.0)),
        }),
        vec![
            option('m', "Raise the limit", AnswerAction::RaiseLimit),
            option('h', "Fall back", AnswerAction::Fallback),
            option('t', "Take over myself", AnswerAction::TakeOver),
            option('x', "Stop and ask me", AnswerAction::AskYou),
            option('d', "Show the details", AnswerAction::ShowDiff),
        ],
    )
}

fn stuck() -> ApprovalOpened {
    approval(
        ApprovalKind::Stuck,
        "Parser needs a decision",
        ApprovalPacket::Stuck(StuckPacket {
            task: task(4),
            title: "Parser".into(),
            state: TaskState::NeedsDecision,
            tries: vec![
                "tests failed: 2 of 41".into(),
                "review asked for changes: the error type leaks".into(),
                "tests failed: 1 of 41".into(),
            ],
            last_failure: "parser::tests::nested_quotes failed".into(),
        }),
        vec![
            option('r', "Retry with a note", AnswerAction::Retry),
            option('h', "Fall back", AnswerAction::Fallback),
            option('s', "Split", AnswerAction::Split),
            option('t', "Take over myself", AnswerAction::TakeOver),
            option('f', "Defer it", AnswerAction::Defer),
            option('x', "Cancel the task", AnswerAction::Cancel),
            option('d', "Show the details", AnswerAction::ShowDiff),
        ],
    )
}

fn original_returned() -> ApprovalOpened {
    let mut opened = approval(
        ApprovalKind::OriginalReturned,
        "Claude Code is back · task-002 (Redis store) was continued by codex/gpt-5.6",
        ApprovalPacket::OriginalReturned(OriginalReturnedPacket {
            task: task(2),
            title: "Redis store".into(),
            original: worker("claude-code/opus"),
            fallback: worker("codex/gpt-5.6"),
            since: 1,
            commits: 3,
            changed: stat(3, 84, 12),
            tests_passed: Some(true),
            debt: vec![QualityDebt {
                since: 1,
                by: worker("codex/gpt-5.6"),
                needs: worker("claude-code/opus"),
            }],
        }),
        vec![
            option(
                'r',
                "Let claude-code/opus review codex's work",
                AnswerAction::UpgradeReview,
            ),
            option(
                'o',
                "Give the task back to claude-code/opus",
                AnswerAction::GiveBack,
            ),
            option(
                'u',
                "Redo from fallback-1 with claude-code/opus",
                AnswerAction::Redo,
            ),
            option(
                'y',
                "Keep codex's work, and move on",
                AnswerAction::KeepFallback,
            ),
            option('t', "Take over myself", AnswerAction::TakeOver),
            option(
                'd',
                "Show the diff since fallback-1",
                AnswerAction::ShowDiff,
            ),
        ],
    );
    opened.task = Some(task(2));
    opened
}

fn app_choice(
    kind: ChoiceKind,
    title: &str,
    body: &[&str],
    options: Vec<AnswerOption>,
) -> AppChoice {
    AppChoice {
        id: ChoiceId::numbered(1),
        kind,
        title: title.into(),
        body: body.iter().map(|line| (*line).to_owned()).collect(),
        options,
        free_text: if kind == ChoiceKind::DetectedChecks {
            FreeText::Edits
        } else {
            FreeText::None
        },
        bell: kind != ChoiceKind::SettingScope,
    }
}

/// One of every `ChoiceKind` the controller sends, in the table's order.
fn choices() -> Vec<AppChoice> {
    vec![
        app_choice(
            ChoiceKind::StartWork,
            "Start work on this?",
            &["\"make the export faster\" could be a question or a change."],
            vec![
                choose('s', "Captain", "captain"),
                choose('c', "Crew run", "crew"),
                choose('n', "No, just asking", "no"),
            ],
        ),
        app_choice(
            ChoiceKind::SettingScope,
            "Save effort = high where?",
            &[],
            vec![
                choose('s', "Just this session", "session"),
                recommended(choose('m', "This project, just me", "project_local")),
                choose('p', "This project, shared", "project"),
                choose('e', "Everywhere", "global"),
            ],
        ),
        app_choice(
            ChoiceKind::ConfirmSafetySetting,
            "Change a safety setting?",
            &["approvals.merge.mode: supervised → auto"],
            vec![
                option('y', "Change it", AnswerAction::Approve),
                option('n', "Keep it as it is", AnswerAction::Cancel),
            ],
        ),
        app_choice(
            ChoiceKind::TrustProject,
            "Trust this folder's settings?",
            &[
                ".argonauts/config.toml sets 2 safety keys:",
                "approvals.merge.mode = \"auto\"",
                "limits.protected_paths = []",
            ],
            vec![
                option('y', "Trust them", AnswerAction::Approve),
                option('n', "Not now", AnswerAction::Cancel),
                option('d', "Show the file", AnswerAction::ShowDiff),
            ],
        ),
        app_choice(
            ChoiceKind::DetectedChecks,
            "Use these check commands?",
            &[
                "cargo fmt --check",
                "cargo clippy -- -D warnings",
                "cargo test",
            ],
            vec![
                option('y', "Use these", AnswerAction::Approve),
                option('e', "Edit them", AnswerAction::ApproveWithEdits),
                option('n', "Skip", AnswerAction::Cancel),
            ],
        ),
        app_choice(
            ChoiceKind::TermsNote,
            "Welcome to Argonauts Code",
            &["Argonauts Code runs the coding agents you already use as one crew."],
            vec![option('c', "Continue", AnswerAction::Approve)],
        ),
        app_choice(
            ChoiceKind::Init,
            "How should the crew work?",
            &["You can change it later with /workflow."],
            vec![
                recommended(choose('a', "council", "council")),
                choose('b', "orchestrator", "orchestrator"),
                choose('c', "captain", "captain"),
            ],
        ),
        app_choice(
            ChoiceKind::ConfirmRewind,
            "Rewind task-002 to fallback-1?",
            &["3 commits since fallback-1 move to argonauts/run-01/task-002-replaced-1"],
            vec![
                option('y', "Rewind", AnswerAction::Approve),
                option('d', "Show the diff", AnswerAction::ShowDiff),
            ],
        ),
        app_choice(
            ChoiceKind::NeededUpdate,
            "Codex 0.158.0 is older than Argonauts Code supports (0.160.0)",
            &["Runs with it may misread what it does. Nothing has started yet."],
            vec![
                detailed(
                    choose('u', "Update it now", "update"),
                    "brew upgrade codex, then the run starts",
                ),
                detailed(
                    choose('l', "Leave Codex out of this run", "leave_out"),
                    "its roles go to the next model in your fallback order",
                ),
                detailed(
                    choose('r', "Run with it anyway", "run"),
                    "and ask again before the next run",
                ),
            ],
        ),
    ]
}

fn guided_steps() -> Vec<AppChoice> {
    let mut steps = choices();
    steps.retain(|choice| matches!(choice.kind, ChoiceKind::TermsNote | ChoiceKind::Init));
    steps.push(app_choice(
        ChoiceKind::Init,
        "Approvals",
        &[
            "With auto, the agents work and merge on their own under the rules. Change that any \
           time in /permissions and /approvals.",
        ],
        vec![
            recommended(detailed(
                choose('a', "Auto", "auto"),
                "Rules and a reviewer from another provider decide, and you're asked when a \
                 rule fails.",
            )),
            detailed(
                choose('b', "Supervised", "supervised"),
                "You approve the goal, the plan, and every merge.",
            ),
            detailed(
                choose('c', "Auto preview", "auto_preview"),
                "You approve, and Argonauts Code learns what it would have decided.",
            ),
        ],
    ));
    steps
}

fn focused(mut prompt: QuestionPrompt) -> QuestionPrompt {
    prompt.focus();
    prompt
}

/// What an approval puts in the conversation, then its question with the keys.
fn approval_lines(
    opened: &ApprovalOpened,
    run: Option<&RunState>,
    width: u16,
) -> Vec<Line<'static>> {
    let theme = theme();
    let ctx = still(&theme, width);
    let mut lines = Vec::new();
    for cell in about_cells(opened, run) {
        lines.extend(cell.lines(&ctx));
        lines.push(Line::default());
    }
    lines.extend(focused(approval_question(1, opened, run)).lines(&ctx));
    lines
}

fn question_lines(prompt: &QuestionPrompt, width: u16) -> Vec<Line<'static>> {
    let theme = theme();
    prompt.lines(&still(&theme, width))
}

fn snapshot(name: &str, render: impl Fn(u16) -> Vec<Line<'static>>) {
    for width in SNAPSHOT_WIDTHS {
        let lines = render(width);
        assert_fits(&lines, width);
        insta::assert_snapshot!(format!("{name}_{width}"), text(&lines));
    }
}

/// Every width, with CJK and a long unbroken word where titles and labels go.
fn holds_its_shape(render: impl Fn(u16) -> Vec<Line<'static>>) {
    for width in WIDTHS {
        let lines = render(width);
        stability::assert_lines(&lines, width);
        assert_fits(&lines, width);
    }
}

fn long_title() -> String {
    format!(
        "{} {} {}",
        samples::CJK,
        samples::long_word(),
        samples::EMOJI
    )
}

/// The same approval with hostile text in its title, options, and packet.
fn hostile(mut opened: ApprovalOpened) -> ApprovalOpened {
    opened.title = long_title();
    for option in &mut opened.options {
        option.label = format!("{} {}", option.label, samples::CJK);
        if option.detail.is_some() {
            option.detail = Some(samples::LONG_PATH.into());
        }
    }
    match &mut opened.packet {
        ApprovalPacket::Merge(merge) => {
            for row in &mut merge.rows {
                row.title = long_title();
            }
            merge.branch = samples::LONG_PATH.into();
        }
        ApprovalPacket::Fallback(plan) => {
            plan.title = long_title();
            plan.reason = long_title();
        }
        ApprovalPacket::Plan(plan) => {
            plan.summary = long_title();
            for task in &mut plan.graph.tasks {
                task.title = long_title();
            }
        }
        ApprovalPacket::Understand(goal) => {
            for question in &mut goal.questions {
                question.text = long_title();
            }
            goal.agreements = vec![long_title()];
        }
        ApprovalPacket::AgentDown(down) => {
            down.evidence = long_title();
            for paused in &mut down.paused {
                paused.title = long_title();
            }
        }
        ApprovalPacket::Stuck(stuck) => {
            stuck.title = long_title();
            stuck.tries = vec![long_title()];
        }
        ApprovalPacket::OriginalReturned(back) => back.title = long_title(),
        _ => {}
    }
    opened
}

fn approval_case(name: &str, opened: ApprovalOpened, run: Option<RunState>) {
    snapshot(name, |width| approval_lines(&opened, run.as_ref(), width));
    let hostile = hostile(opened);
    holds_its_shape(|width| approval_lines(&hostile, run.as_ref(), width));
}

#[test]
fn goal_question_and_understand_approval() {
    approval_case("goal_question", goal_question(), None);
    approval_case("understand_approval", understand_approval(), None);
}

/// The bottom region with a goal question in it, under an input labeled with your workflow.
fn bottom_with(question: QuestionPrompt) -> argonauts_tui::bottom::BottomState {
    let mut bottom = argonauts_tui::bottom::BottomState::new();
    bottom.composer.label = Some("council · ultracode".into());
    bottom.question = Some(question);
    bottom
}

fn bottom_lines(bottom: &argonauts_tui::bottom::BottomState, width: u16) -> Vec<Line<'static>> {
    let theme = theme();
    bottom.render(&still(&theme, width)).lines
}

/// While a goal question holds the keys it stands in for the input, as Claude Code's
/// permission prompts do: the input's rules and prompt line go, and nothing else moves. Passed
/// on, it leaves the screen and the input comes back.
#[test]
fn a_goal_question_holding_the_keys_stands_in_for_the_input() {
    let opened = goal_question();
    let holding = bottom_with(focused(approval_question(1, &opened, None)));
    let waiting = bottom_with(approval_question(1, &opened, None));
    let mut aside = focused(approval_question(1, &opened, None));
    let esc = crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Esc);
    aside.handle_key(esc);
    assert!(aside.dismissed);
    let aside = bottom_with(aside);
    for width in WIDTHS {
        for bottom in [&holding, &waiting, &aside] {
            let lines = bottom_lines(bottom, width);
            stability::assert_lines(&lines, width);
            assert_fits(&lines, width);
        }
        let held = text(&bottom_lines(&holding, width));
        let input = text(&bottom_lines(&waiting, width));
        assert!(!held.contains("council · ultracode ─"), "{held}");
        assert!(input.contains("council · ultracode ─"), "{input}");
        // Only the input's rows go: its two rules and its prompt line.
        assert_eq!(
            input.lines().count() - held.lines().count(),
            3,
            "{input}\n---\n{held}"
        );
    }
    for width in [80, 100, 120] {
        insta::assert_snapshot!(
            format!("goal_question_holds_the_keys_{width}"),
            text(&bottom_lines(&holding, width))
        );
        insta::assert_snapshot!(
            format!("goal_question_set_aside_{width}"),
            text(&bottom_lines(&aside, width))
        );
    }
}

/// With vim keys on, the input's `-- INSERT --` goes with the input while a question holds the
/// keys, since nothing there is typed into.
#[test]
fn a_question_holding_the_keys_leaves_the_vim_mode_off() {
    let mut holding = bottom_with(focused(approval_question(1, &goal_question(), None)));
    holding.mode.vim = Some(argonauts_tui::model::VimMode::Insert);
    for width in [80, 100, 120] {
        let lines = bottom_lines(&holding, width);
        stability::assert_lines(&lines, width);
        assert_fits(&lines, width);
        let shown = text(&lines);
        assert!(!shown.contains("-- INSERT --"), "{shown}");
        insta::assert_snapshot!(format!("goal_question_holds_the_keys_vim_{width}"), shown);
    }
}

/// Three scope questions whose recommended answer the comparer listed last, as a live run had
/// them: the question letters its answers again with the recommended one first.
fn scope_questions() -> ApprovalOpened {
    let choice = |key: char, label: &str, basis: Option<&str>| ChoiceOption {
        key,
        label: label.into(),
        recommended: basis.is_some(),
        basis: basis.map(str::to_owned),
    };
    let question = |n: u8, text: &str| GoalQuestion {
        n,
        of: 3,
        text: text.into(),
        options: vec![
            choice('a', "Yes, include type hints", None),
            choice('b', "No, leave them out", None),
            choice(
                'c',
                "Match what mean() does",
                Some("2 of 3 read it this way"),
            ),
        ],
        tag: DiffTag::Scope,
        topic: None,
    };
    approval(
        ApprovalKind::Understand,
        "Goal question",
        ApprovalPacket::Understand(GoalPacket {
            goal: goal_file(),
            agreements: Vec::new(),
            assumptions: Vec::new(),
            questions: vec![
                question(1, "Should mode() have type hints?"),
                question(2, "Should mode() live in core.py?"),
                question(3, "Should ties return the smallest value?"),
            ],
        }),
        vec![
            recommended(detailed(
                choose('a', "Match what mean() does", "Match what mean() does"),
                "2 of 3 read it this way",
            )),
            choose('b', "Yes, include type hints", "Yes, include type hints"),
            choose('c', "No, leave them out", "No, leave them out"),
            option('d', "Type my own answer", AnswerAction::Answer),
        ],
    )
}

#[test]
fn repeated_tags_number_their_chips_and_each_answer_keeps_its_own_basis() {
    let opened = scope_questions();
    let theme = theme();
    for width in [80, 100, 120, 200] {
        let lines = focused(approval_question(1, &opened, None)).lines(&still(&theme, width));
        assert_fits(&lines, width);
        let shown = text(&lines);
        insta::assert_snapshot!(format!("goal_question_repeated_tags_{width}"), shown);
        let chips = shown.lines().find(|line| line.contains("Scope")).unwrap();
        // The current chip's box lines up with the question's text, at column 2.
        assert_eq!(chips.find('☐'), Some(2), "{shown}");
        assert!(
            chips.contains("Scope 1") && chips.contains("Scope 3"),
            "{shown}"
        );
        assert_eq!(
            shown.matches("2 of 3 read it this way").count(),
            1,
            "{shown}"
        );
    }
}

#[test]
fn a_summary_wraps_between_its_parts() {
    let mut opened = merge();
    if let ApprovalPacket::Merge(merge) = &mut opened.packet {
        merge.limit = None;
    }
    let theme = theme();
    for width in [80, 100, 120, 200] {
        let lines = focused(approval_question(1, &opened, None)).lines(&still(&theme, width));
        assert_fits(&lines, width);
        let shown = text(&lines);
        assert!(shown.lines().any(|line| line.contains(" spent")), "{shown}");
        assert!(!shown.lines().any(|line| line.trim() == "spent"), "{shown}");
    }
}

#[test]
fn plan_question() {
    approval_case("plan_question", plan(), None);
}

/// The plan with the workflow it follows: its phases in plain words in the main view, and its
/// conditions and loop rules under Ctrl+O.
fn plan_with_workflow() -> ApprovalOpened {
    use argonauts_core::events::approval::PlanWorkflow;
    use argonauts_core::workflow::WorkflowRef;

    let mut opened = plan();
    let ApprovalPacket::Plan(packet) = &mut opened.packet else {
        unreachable!("a plan approval")
    };
    let workflow = serde_json::from_value(serde_json::json!({
        "name": "council",
        "description": "Peers draft plans blind and debate only where they disagree",
        "phases": [
            {"name": "Understand", "standard": "understand", "steps": [{
                "kind": "loop_until", "until": "understood", "max": 2,
                "steps": [
                    {"kind": "read", "role": "reader"},
                    {"kind": "compare", "role": "comparer"},
                    {"kind": "approval", "approval": "understand"}
                ]
            }]},
            {"name": "Plan", "standard": "plan", "steps": [{
                "kind": "loop_until", "until": "plan_decided", "max": 3,
                "steps": [
                    {"kind": "draft", "role": "draft", "count": 3, "blind": true},
                    {"kind": "judge", "role": "judge", "when": "drafts_disagree"},
                    {"kind": "approval", "approval": "plan"}
                ]
            }]},
            {"name": "Build", "standard": "do", "steps": [{"kind": "build", "role": "worker"}]},
            {"name": "Merge", "standard": "decide", "steps": [{"kind": "merge"}]}
        ]
    }))
    .expect("a workflow");
    packet.workflow = Some(PlanWorkflow {
        from: Some(WorkflowRef::new("council")),
        why: Some("A security-labeled API feature: blind drafts and cross-judging fit it.".into()),
        workflow,
    });
    opened
}

#[test]
fn plan_question_with_workflow() {
    approval_case("plan_question_with_workflow", plan_with_workflow(), None);
    let opened = plan_with_workflow();
    snapshot("plan_workflow_expanded", |width| {
        let theme = theme();
        let ctx = still(&theme, width).with_expanded(true);
        let cells = about_cells(&opened, None);
        let item = cells
            .iter()
            .find(|cell| text(&cell.lines(&ctx)).contains("Workflow"))
            .expect("the plan's item");
        item.lines(&ctx)
    });
}

#[test]
fn merge_question() {
    approval_case("merge_question", merge(), None);

    // Request changes, picked by its number, opens the rows as a checklist.
    let opened = merge();
    snapshot("merge_question_request_changes", |width| {
        let mut prompt = focused(approval_question(1, &opened, None));
        let key = crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('3'));
        prompt.handle_key(key);
        question_lines(&prompt, width)
    });
}

#[test]
fn fallback_question() {
    approval_case("fallback_question", fallback(), None);
}

#[test]
fn agent_down_question() {
    approval_case("agent_down_question", agent_down(), None);
}

#[test]
fn limit_question() {
    approval_case("limit_question", limit(), Some(run_state()));
}

#[test]
fn stuck_question() {
    approval_case("stuck_question", stuck(), Some(run_state()));
}

#[test]
fn original_returned_question() {
    approval_case("original_returned_question", original_returned(), None);
}

#[test]
fn spot_check_question() {
    let mut opened = merge();
    opened.mode = ApprovalMode::Auto;
    let decided = ApprovalDecided {
        id: opened.id.clone(),
        action: AnswerAction::Merge,
        text: None,
        by: Party::Rules,
        evidence_sha256: opened.evidence_sha256.clone(),
        auto_preview: None,
        tasks: Vec::new(),
    };
    let run = run_id();
    let names = DisplayNames::default();
    snapshot("spot_check_question", |width| {
        question_lines(
            &spot_question(1, &opened, Some(&decided), Some(&run), &names),
            width,
        )
    });
    let hostile = hostile(opened.clone());
    holds_its_shape(|width| {
        question_lines(
            &focused(spot_question(
                1,
                &hostile,
                Some(&decided),
                Some(&run),
                &names,
            )),
            width,
        )
    });

    // Its record, and every other question's, is one line.
    let theme = theme();
    let record = record_cell(&opened, &decided);
    assert_eq!(
        text(&record.lines(&still(&theme, 80))),
        "◆ Approved merging the run into main · your rules"
    );
}

#[test]
fn choice_questions() {
    let all = choices();
    snapshot("choice_questions", |width| {
        let mut lines = Vec::new();
        for choice in &all {
            lines.extend(question_lines(&focused(choice_question(1, choice)), width));
            lines.push(Line::default());
        }
        let run = run_id();
        let take_over = confirm_take_over(1, &run, &task(2), "codex/gpt-5.6");
        lines.extend(question_lines(&focused(take_over), width));
        lines
    });
    for mut choice in all {
        choice.title = long_title();
        choice.body = vec![long_title(), samples::LONG_PATH.into()];
        for option in &mut choice.options {
            option.label = long_title();
        }
        holds_its_shape(|width| question_lines(&focused(choice_question(1, &choice)), width));
    }
}

#[test]
fn guided_setup_step() {
    let steps = guided_steps();
    snapshot("guided_setup_step", |width| {
        let mut lines = Vec::new();
        for step in &steps {
            lines.extend(question_lines(&choice_question(1, step), width));
            lines.push(Line::default());
        }
        lines
    });
    for step in &steps {
        let question = choice_question(1, step);
        let first = &question.options[0];
        assert_eq!(question.selected, 0, "Enter takes the recommended answer");
        assert!(step.kind == ChoiceKind::TermsNote || first.recommended);
        // Esc still takes them, but a step's key line names only Enter.
        let esc = match step.kind {
            ChoiceKind::TermsNote => "Esc to take the recommended answers",
            _ => "",
        };
        assert_eq!(question.esc_hint, esc);
    }
}

/// What a direct turn's CLI asks, as Claude Code asks it, and agy's refused commands after its
/// answer, at 100 and 120 columns.
#[test]
fn direct_turn_questions() {
    let questions = [
        app_choice(
            ChoiceKind::Permission,
            "Make this edit to invoice/money.py?",
            &[
                "- sign = 1",
                "+ negative = text.startswith(\"-\")",
                "+ text = text.lstrip(\"-\")",
            ],
            vec![
                choose('y', "Yes", "yes"),
                choose(
                    'a',
                    "Yes, and allow edits for the rest of this conversation",
                    "always",
                ),
                choose(
                    'n',
                    "No, and tell Claude Code what to do instead (esc)",
                    "no",
                ),
            ],
        ),
        app_choice(
            ChoiceKind::Permission,
            "Run this command?",
            &["python -m pytest -q"],
            vec![
                choose('y', "Yes", "yes"),
                choose(
                    'a',
                    "Yes, and don't ask again for python -m pytest in this project",
                    "always",
                ),
                choose(
                    'n',
                    "No, and tell Claude Code what to do instead (esc)",
                    "no",
                ),
            ],
        ),
        app_choice(
            ChoiceKind::RefusedCommands,
            "Let Antigravity CLI run what it was refused?",
            &["python -m pytest -q"],
            vec![
                AnswerOption {
                    detail: Some(
                        "one more turn in which agy runs commands without asking, in our sandbox"
                            .into(),
                    ),
                    ..choose('y', "Yes, and continue", "continue")
                },
                choose('n', "No", "no"),
            ],
        ),
    ];
    for width in [100, 120] {
        let mut lines = Vec::new();
        for choice in &questions {
            lines.extend(question_lines(&focused(choice_question(1, choice)), width));
            lines.push(Line::default());
        }
        assert_fits(&lines, width);
        insta::assert_snapshot!(format!("direct_turn_questions_{width}"), text(&lines));
    }
    let permission = choice_question(1, &questions[0]);
    assert_eq!(permission.esc_hint, "Esc for No");
    for choice in questions {
        holds_its_shape(|width| question_lines(&focused(choice_question(1, &choice)), width));
    }
}

/// A run whose task-001, `Fix money rounding`, Haiku 4.5 on Claude Code owns. The event names
/// the CLI alone, as a Captain run's question does, and the session says the model.
fn money_run() -> RunState {
    let mut run = RunState::new(run_id());
    let mut row = TaskRow::new(task(1), "Fix money rounding");
    row.worker = Some(worker("claude-code"));
    run.tasks.insert(task(1), row);
    let key = SessionKey::numbered(1);
    let mut session = SessionState::new(
        key.clone(),
        worker("claude-code/claude-haiku-4-5"),
        "claude-code",
        RoleName::Worker,
        datetime!(2026-10-04 9:00 UTC),
    );
    session.task = Some(task(1));
    // The worker's label, as the crew names its build session.
    session.step = Some(StepName::Build);
    session.label = Some("build:fix-money".into());
    run.sessions.insert(key.clone(), session);
    run.session_order.push(key);
    run
}

/// An agent's question as the crew opens it, titled by its task.
fn agent_question(choices: &[&str], own_answer: bool) -> ApprovalOpened {
    let mut options: Vec<AnswerOption> = choices
        .iter()
        .zip('a'..)
        .map(|(label, key)| choose(key, label, label))
        .collect();
    if own_answer {
        let key = char::from(b'a' + options.len() as u8);
        options.push(option(key, "Type my own answer", AnswerAction::Answer));
    }
    let mut opened = approval(
        ApprovalKind::Question,
        "Question from task-001",
        ApprovalPacket::Question(QuestionPacket {
            from: worker("claude-code"),
            task: task(1),
            text: "Should totals round half to even, as the ledger does, or half up?".into(),
            options: choices.iter().map(ToString::to_string).collect(),
        }),
        options,
    );
    opened.task = Some(task(1));
    opened.free_text = if own_answer {
        FreeText::Answer
    } else {
        FreeText::None
    };
    opened
}

/// A question shows only what answers it: its field open when it's free text, its numbered
/// choices, or its choices with your own answer last. It names the agent by label and model,
/// never the task's id, and holds the keys with the input hidden.
#[test]
fn an_agent_question_shows_only_what_answers_it() {
    let run = money_run();
    let cases = [
        ("agent_question_free_text", agent_question(&[], true)),
        (
            "agent_question_choices",
            agent_question(&["Half to even", "Half up"], false),
        ),
        (
            "agent_question_choices_and_own_answer",
            agent_question(&["Half to even", "Half up"], true),
        ),
    ];
    for (name, opened) in &cases {
        let holding = bottom_with(focused(approval_question(1, opened, Some(&run))));
        for width in WIDTHS {
            let lines = bottom_lines(&holding, width);
            stability::assert_lines(&lines, width);
            assert_fits(&lines, width);
        }
        for width in [80, 100, 120] {
            let shown = text(&bottom_lines(&holding, width));
            assert!(!shown.contains("task-001"), "{shown}");
            assert!(!shown.contains("council · ultracode"), "{shown}");
            insta::assert_snapshot!(format!("{name}_{width}"), shown);
        }
    }
    let free_text = text(&bottom_lines(
        &bottom_with(focused(approval_question(1, &cases[0].1, Some(&run)))),
        100,
    ));
    assert!(
        free_text.contains("◆ Question from build:fix-money"),
        "{free_text}"
    );
    assert!(
        free_text.contains("Agent: build:fix-money · Haiku 4.5 on Claude Code"),
        "{free_text}"
    );
    assert!(!free_text.contains("Type my own answer"), "{free_text}");
    assert!(!free_text.contains("Enter to type"), "{free_text}");

    let mut hostile = agent_question(&[], true);
    if let ApprovalPacket::Question(question) = &mut hostile.packet {
        question.text = long_title();
    }
    holds_its_shape(|width| {
        bottom_lines(
            &bottom_with(focused(approval_question(1, &hostile, Some(&run)))),
            width,
        )
    });
}

/// The go-ahead autopilot left to you, as the council's "Run this workflow?" was: it asks, so
/// it takes the keys as it shows and the input box goes, the way Claude Code's permission
/// prompt stands in for its input. Nothing waits for ↑ under a ruled input.
#[test]
fn the_go_ahead_holds_the_keys_with_the_input_hidden() {
    let mut opened = understand_approval();
    opened.title = "Run this workflow?".into();
    opened.mode = ApprovalMode::Autopilot;
    opened.decide_with = DecideWith::RulesAndModel;
    opened.free_text = FreeText::None;
    opened.options = vec![
        option('y', "Yes, run it", AnswerAction::Approve),
        choose(
            'l',
            "Yes, and let the main agent answer",
            "Yes, and let the main agent answer",
        ),
        option('b', "Send back for another round", AnswerAction::SendBack),
        option('d', "Show the goal", AnswerAction::ShowDiff),
        option('x', "No, stop here", AnswerAction::Abandon),
    ];
    let mut bottom = argonauts_tui::model::BottomState::new();
    bottom.composer.label = Some("council".into());
    bottom.mode.approval = argonauts_tui::bottom::ApprovalMode::Autopilot;
    bottom.question = Some(approval_question(1, &opened, None));
    assert!(bottom.hand_keys_to_question(std::time::Instant::now()));
    let theme = theme();
    for width in [100, 120] {
        let view = bottom.render(&still(&theme, width));
        assert_fits(&view.lines, width);
        let shown = text(&view.lines);
        assert!(!shown.contains("to answer the question"), "{shown}");
        assert!(!shown.lines().any(|row| row.starts_with('❯')), "{shown}");
        assert!(shown.contains("❯ 1. Yes, run it"), "{shown}");
        insta::assert_snapshot!(format!("go_ahead_holds_the_keys_{width}"), shown);
    }
}
