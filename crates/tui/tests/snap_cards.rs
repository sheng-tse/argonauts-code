mod common;

use std::path::PathBuf;

use argonauts_core::app::{AppChoice, ChoiceKind};
use argonauts_core::budget::{BudgetAction, BudgetLimit, WorkLimits, WorkUsed};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::card::{CardBudget, CardKind, CardSpec, CardState};
use argonauts_core::events::checkpoint::{
    AnswerAction, AnswerOption, BudgetPacket, CheckpointDecided, CheckpointKind, CheckpointOpened,
    CheckpointPacket, EscalationPacket, FreeText, GoalPacket, MergePacket, MergeRow,
    OriginalReturnedPacket, PausedCard, PlanPacket, VendorDownPacket,
};
use argonauts_core::events::goal::{
    Assumption, ChoiceOption, Criterion, GoalFile, GoalQuestion, GoalSource,
};
use argonauts_core::events::plan::{
    BudgetLine, BudgetPlan, DesignDecision, JudgingResult, PlanTally, TaskGraph,
};
use argonauts_core::events::run::Party;
use argonauts_core::events::vendor::HandoffPlan;
use argonauts_core::handoff::QualityDebt;
use argonauts_core::limits::{Candidate, CostEstimate, TrackRecord};
use argonauts_core::names::{CheckpointId, ChoiceId, DecisionId, RoleName, VendorId, WorkerRef};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::budget::{BudgetAmount, BudgetLineKind, BudgetUnit};
use argonauts_core::settings::handoff::{HandoffMode, HandoffStrategy};
use argonauts_core::settings::protocol::JudgingMethod;
use argonauts_core::steps::goal::DiffTag;
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::{BudgetDimension, BudgetStatus, CardId, DiffStat, RunId, Spend};
use argonauts_tui::HistoryCell;
use argonauts_tui::cards::{
    about_cells, checkpoint_card, choice_card, confirm_take_over, record_cell,
    spot_check_card as spot_card,
};
use argonauts_tui::model::CardPrompt;
use argonauts_tui::stability::{self, WIDTHS, samples};
use argonauts_tui::state::{CardRow, RunState};
use common::{assert_fits, still, text, theme};
use ratatui::text::Line;
use time::macros::datetime;

const SNAPSHOT_WIDTHS: [u16; 2] = [80, 120];

fn run_id() -> RunId {
    RunId::new("run-01").unwrap()
}

fn card(n: u32) -> CardId {
    CardId::new(format!("card-{n:03}")).unwrap()
}

fn worker(text: &str) -> WorkerRef {
    text.parse().unwrap()
}

fn stat(files: u32, insertions: u32, deletions: u32) -> DiffStat {
    DiffStat {
        files_changed: files,
        insertions,
        deletions,
    }
}

fn usd(amount: f64) -> BudgetAmount {
    BudgetAmount {
        unit: BudgetUnit::Usd,
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

fn checkpoint(
    kind: CheckpointKind,
    title: &str,
    packet: CheckpointPacket,
    options: Vec<AnswerOption>,
) -> CheckpointOpened {
    CheckpointOpened {
        id: CheckpointId::numbered(1),
        kind,
        title: title.into(),
        card: None,
        packet,
        evidence_sha256: "e3b0c442".into(),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        options,
        free_text: FreeText::Guidance,
        needs_you: true,
    }
}

/// A run whose cards have owners, for the `Agent:` line.
fn run_state() -> RunState {
    let mut run = RunState::new(run_id());
    for (n, title, owner) in [
        (2, "Redis store", "codex/gpt-5.6"),
        (4, "Parser", "codex/gpt-5.6"),
    ] {
        run.cards.insert(
            card(n),
            CardRow {
                id: card(n),
                title: title.into(),
                state: Some(CardState::Running),
                owner: Some(worker(owner)),
                attempt: 3,
                diff: None,
                standing_in_for: None,
                debt: Vec::new(),
                checkpoints: 0,
                rotations: 0,
                last_gate: None,
                reviews: 0,
                waiting_on_vendor: None,
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
        criteria: vec![Criterion {
            id: "G1".into(),
            text: "`export --json` writes one JSON object per row".into(),
        }],
        in_scope: vec!["the export command".into()],
        out_of_scope: vec!["the import command".into()],
        assumptions: vec![Assumption {
            text: "Rows keep their CSV column order".into(),
            basis: "a detail, and 2 of 3 read it that way".into(),
        }],
        answers: Vec::new(),
        source: GoalSource::Restatements,
        path: PathBuf::from(".argonauts/goal/run-01.md"),
        sha256: "e3b0".into(),
    }
}

fn goal_question() -> CheckpointOpened {
    let question = |n: u8, text: &str, tag: DiffTag| GoalQuestion {
        n,
        of: 2,
        text: text.into(),
        options: vec![
            ChoiceOption {
                key: 'a',
                label: "Per API key".into(),
                recommended: true,
                basis: Some("2 of 3 restatements read it this way".into()),
            },
            ChoiceOption {
                key: 'b',
                label: "Per user".into(),
                recommended: false,
                basis: Some("1 of 3 read it this way".into()),
            },
        ],
        tag,
    };
    checkpoint(
        CheckpointKind::Goal,
        "What should the goal say? · 1 of 2",
        CheckpointPacket::Goal(GoalPacket {
            goal: goal_file(),
            agreements: vec!["Rate limit the public API".into()],
            assumptions: vec![Assumption {
                text: "Limits reset every minute".into(),
                basis: "a detail, and 2 of 3 read it that way".into(),
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

fn goal_approval() -> CheckpointOpened {
    let mut opened = goal_question();
    if let CheckpointPacket::Goal(goal) = &mut opened.packet {
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

fn spec(n: u32, title: &str, owner: &str, reviewer: &str, depends_on: &[u32]) -> CardSpec {
    CardSpec {
        id: card(n),
        title: title.into(),
        task: title.into(),
        kind: CardKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: depends_on.iter().map(|n| card(*n)).collect(),
        criteria: vec!["G1".into()],
        owner: Some(worker(owner)),
        reviewers: vec![worker(reviewer)],
        budget: CardBudget::default(),
        predictions: None,
    }
}

fn plan() -> CheckpointOpened {
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
            cards: vec![
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
            critical_path: vec![card(1), card(2), card(3)],
        },
        acceptance_tests: vec!["export --json writes one object per row".into()],
        budget: BudgetPlan {
            lines: vec![
                BudgetLine {
                    line: BudgetLineKind::Cards,
                    amount: usd(6.0),
                },
                BudgetLine {
                    line: BudgetLineKind::Review,
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
    checkpoint(
        CheckpointKind::Plan,
        "Approve this plan?",
        CheckpointPacket::Plan(Box::new(packet)),
        vec![
            option('y', "Approve the plan", AnswerAction::Approve),
            option('e', "Approve with edits", AnswerAction::ApproveWithEdits),
            option('b', "Send back", AnswerAction::SendBack),
            option('s', "Run it solo", AnswerAction::SwitchToSolo),
            option('x', "Abandon", AnswerAction::Abandon),
            option('d', "Show the plan", AnswerAction::ShowDiff),
        ],
    )
}

fn merge_row(n: u32, title: &str, owner: &str, review: Option<ReviewVerdict>) -> MergeRow {
    MergeRow {
        card: card(n),
        title: title.into(),
        kind: CardKind::Build,
        stat: stat(2, 30 * n, 4 * n),
        owner: Some(worker(owner)),
        gates_passed: true,
        review,
        debt: Vec::new(),
    }
}

fn merge() -> CheckpointOpened {
    let mut debt_row = merge_row(4, "Middleware", "codex/gpt-5.6", None);
    debt_row.debt = vec![QualityDebt {
        since: 1,
        by: worker("codex/gpt-5.6"),
        needs: worker("claude-code/opus"),
    }];
    let packet = MergePacket {
        target: Some("main".into()),
        branch: "argonauts/run-01/integration".into(),
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
        gates: Vec::new(),
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
    };
    checkpoint(
        CheckpointKind::Merge,
        "Merge this run into main?",
        CheckpointPacket::Merge(packet),
        vec![
            option('y', "Merge", AnswerAction::Merge),
            option('p', "Open a pull request", AnswerAction::PullRequest),
            option(
                'b',
                "Request changes on some cards",
                AnswerAction::RequestChanges,
            ),
            option('k', "Keep the branch", AnswerAction::KeepBranch),
            option('x', "Abandon the run", AnswerAction::Abandon),
            option('d', "Show the diff", AnswerAction::ShowDiff),
        ],
    )
}

fn candidate(name: &str, quota: &str, passed: u32, of: u32, cost: CostEstimate) -> Candidate {
    Candidate {
        worker: worker(name),
        quota_left: None,
        quota_text: quota.into(),
        track_record: Some(TrackRecord { passed, of }),
        cost,
        needs_confirm: matches!(cost, CostEstimate::Usd(_)),
        may_run_out: quota == "10%",
    }
}

fn handoff() -> CheckpointOpened {
    let candidates = vec![
        candidate("codex/gpt-5.6", "62%", 9, 11, CostEstimate::Quota(0.06)),
        candidate(
            "antigravity-cli/gemini-3.6",
            "10%",
            4,
            6,
            CostEstimate::Quota(0.09),
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
            "62% left · 9 of 11 passed · ~6% of quota",
        ),
        detailed(
            option(
                'b',
                "antigravity-cli/gemini-3.6",
                AnswerAction::Candidate {
                    worker: worker("antigravity-cli/gemini-3.6"),
                },
            ),
            "10% left · 4 of 6 passed · ~9% of quota, may run out",
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
    let mut opened = checkpoint(
        CheckpointKind::Handoff,
        "Handoff · card-003 Middleware · needs you",
        CheckpointPacket::Handoff(HandoffPlan {
            card: card(3),
            title: "Middleware".into(),
            stopped: worker("claude-code/opus"),
            reason: "usage limit, resets in 2h 10m".into(),
            resets_at: None,
            work: stat(4, 96, 12),
            gates_ran: false,
            candidates,
            mode: HandoffMode::Ask,
            strategy: HandoffStrategy::Spread,
            race_count: 1,
            also_moving: vec![card(5), card(6)],
        }),
        options,
    );
    opened.card = Some(card(3));
    opened
}

fn vendor_down() -> CheckpointOpened {
    let paused = |n: u32, title: &str, role: RoleName| PausedCard {
        card: card(n),
        title: title.into(),
        role,
    };
    checkpoint(
        CheckpointKind::VendorDown,
        "Claude Code is down",
        CheckpointPacket::VendorDown(VendorDownPacket {
            vendor: VendorId("claude-code".into()),
            evidence: "overload errors on 3 sessions in 2 min".into(),
            test_at: Some(datetime!(2026-09-30 14:40 UTC)),
            paused: vec![
                paused(3, "Middleware", RoleName::Owner),
                paused(2, "Redis store", RoleName::Reviewer),
            ],
        }),
        vec![
            option(
                'a',
                "One agent for every paused card",
                AnswerAction::OneForAll,
            ),
            option('e', "Choose per card", AnswerAction::PerCard),
            option('w', "Wait for Claude Code", AnswerAction::Wait),
        ],
    )
}

fn budget() -> CheckpointOpened {
    let status = BudgetStatus {
        level: BudgetAction::Stop,
        limit: BudgetLimit::Usd(2.0),
        spend: Spend {
            cost_usd: 2.1,
            ..Spend::default()
        },
        fraction_used: Some(1.05),
        work: WorkUsed::default(),
        work_limits: WorkLimits::default(),
        binding: Some(BudgetDimension::Spend),
    };
    checkpoint(
        CheckpointKind::Budget,
        "card-002 reached its budget",
        CheckpointPacket::Budget(BudgetPacket {
            card: Some(card(2)),
            status,
            dimension: BudgetDimension::Spend,
            reserve_left: Some(usd(1.0)),
        }),
        vec![
            option('m', "More budget", AnswerAction::MoreBudget),
            option('h', "Hand it off", AnswerAction::HandOff),
            option('t', "Take over myself", AnswerAction::TakeOver),
            option('x', "Stop and escalate", AnswerAction::Escalate),
            option('d', "Show the details", AnswerAction::ShowDiff),
        ],
    )
}

fn escalation() -> CheckpointOpened {
    checkpoint(
        CheckpointKind::Escalation,
        "card-004 is stuck after 3 attempts",
        CheckpointPacket::Escalation(EscalationPacket {
            card: card(4),
            title: "Parser".into(),
            state: CardState::NeedsDecision,
            attempts: vec![
                "tests failed: 2 of 41".into(),
                "review asked for changes: the error type leaks".into(),
                "tests failed: 1 of 41".into(),
            ],
            last_failure: "parser::tests::nested_quotes failed".into(),
        }),
        vec![
            option('r', "Retry with a note", AnswerAction::Retry),
            option('h', "Hand it off", AnswerAction::HandOff),
            option('s', "Split", AnswerAction::Split),
            option('t', "Take over myself", AnswerAction::TakeOver),
            option('f', "Defer it", AnswerAction::Defer),
            option('x', "Cancel the card", AnswerAction::Cancel),
            option('d', "Show the details", AnswerAction::ShowDiff),
        ],
    )
}

fn original_returned() -> CheckpointOpened {
    let mut opened = checkpoint(
        CheckpointKind::OriginalReturned,
        "Claude Code is back · card-002 (Redis store) was continued by codex/gpt-5.6",
        CheckpointPacket::OriginalReturned(OriginalReturnedPacket {
            card: card(2),
            title: "Redis store".into(),
            original: worker("claude-code/opus"),
            replacement: worker("codex/gpt-5.6"),
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
                "Give the card back to claude-code/opus",
                AnswerAction::HandBack,
            ),
            option(
                'u',
                "Redo from handoff-1 with claude-code/opus",
                AnswerAction::Redo,
            ),
            option(
                'y',
                "Keep codex's work, and move on",
                AnswerAction::KeepReplacement,
            ),
            option('t', "Take over myself", AnswerAction::TakeOver),
            option('d', "Show the diff since handoff-1", AnswerAction::ShowDiff),
        ],
    );
    opened.card = Some(card(2));
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
        free_text: if kind == ChoiceKind::DetectedGates {
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
                choose('s', "Solo", "solo"),
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
            ChoiceKind::DetectedGates,
            "Use these gate commands?",
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
            "Welcome to Argonauts",
            &["Argonauts runs the coding agents you already use as one crew."],
            vec![option('c', "Continue", AnswerAction::Approve)],
        ),
        app_choice(
            ChoiceKind::Init,
            "How should the crew work?",
            &["You can change it later with /preset."],
            vec![
                recommended(choose('a', "peer-crew", "peer-crew")),
                choose('b', "coordinated-crew", "coordinated-crew"),
                choose('c', "solo", "solo"),
            ],
        ),
        app_choice(
            ChoiceKind::ConfirmRevert,
            "Revert card-002 to handoff-1?",
            &["3 commits since handoff-1 move to argonauts/run-01/card-002-replaced-1"],
            vec![
                option('y', "Revert", AnswerAction::Approve),
                option('d', "Show the diff", AnswerAction::ShowDiff),
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
                "Rules and a reviewer from another AI company decide, and you're asked when a \
                 rule fails.",
            )),
            detailed(
                choose('b', "Supervised", "supervised"),
                "You approve the goal, the plan, and every merge.",
            ),
            detailed(
                choose('c', "Shadow", "shadow"),
                "You approve, and Argonauts learns what it would have decided.",
            ),
        ],
    ));
    steps
}

fn focused(mut prompt: CardPrompt) -> CardPrompt {
    prompt.focus();
    prompt
}

/// What a checkpoint puts in the conversation, then its card with the keys.
fn checkpoint_lines(
    opened: &CheckpointOpened,
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
    lines.extend(focused(checkpoint_card(1, opened, run)).lines(&ctx));
    lines
}

fn card_lines(prompt: &CardPrompt, width: u16) -> Vec<Line<'static>> {
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

/// The same checkpoint with hostile text in its title, options, and packet.
fn hostile(mut opened: CheckpointOpened) -> CheckpointOpened {
    opened.title = long_title();
    for option in &mut opened.options {
        option.label = format!("{} {}", option.label, samples::CJK);
        if option.detail.is_some() {
            option.detail = Some(samples::LONG_PATH.into());
        }
    }
    match &mut opened.packet {
        CheckpointPacket::Merge(merge) => {
            for row in &mut merge.rows {
                row.title = long_title();
            }
            merge.branch = samples::LONG_PATH.into();
        }
        CheckpointPacket::Handoff(plan) => {
            plan.title = long_title();
            plan.reason = long_title();
        }
        CheckpointPacket::Plan(plan) => {
            plan.summary = long_title();
            for card in &mut plan.graph.cards {
                card.title = long_title();
            }
        }
        CheckpointPacket::Goal(goal) => {
            for question in &mut goal.questions {
                question.text = long_title();
            }
            goal.agreements = vec![long_title()];
        }
        CheckpointPacket::VendorDown(down) => {
            down.evidence = long_title();
            for paused in &mut down.paused {
                paused.title = long_title();
            }
        }
        CheckpointPacket::Escalation(escalation) => {
            escalation.title = long_title();
            escalation.attempts = vec![long_title()];
        }
        CheckpointPacket::OriginalReturned(back) => back.title = long_title(),
        _ => {}
    }
    opened
}

fn checkpoint_case(name: &str, opened: CheckpointOpened, run: Option<RunState>) {
    snapshot(name, |width| checkpoint_lines(&opened, run.as_ref(), width));
    let hostile = hostile(opened);
    holds_its_shape(|width| checkpoint_lines(&hostile, run.as_ref(), width));
}

#[test]
fn goal_card() {
    checkpoint_case("goal_question", goal_question(), None);
    checkpoint_case("goal_approval", goal_approval(), None);
}

/// Three scope questions whose recommended answer the differ listed last, as a live run had
/// them: the card letters its answers again with the recommended one first.
fn scope_questions() -> CheckpointOpened {
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
    };
    checkpoint(
        CheckpointKind::Goal,
        "Goal question",
        CheckpointPacket::Goal(GoalPacket {
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
        let lines = focused(checkpoint_card(1, &opened, None)).lines(&still(&theme, width));
        assert_fits(&lines, width);
        let shown = text(&lines);
        insta::assert_snapshot!(format!("goal_question_repeated_tags_{width}"), shown);
        let chips = shown.lines().find(|line| line.contains("Scope")).unwrap();
        // The current chip's box lines up with the card's text, at column 2.
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
    if let CheckpointPacket::Merge(merge) = &mut opened.packet {
        merge.limit = None;
    }
    let theme = theme();
    for width in [80, 100, 120, 200] {
        let lines = focused(checkpoint_card(1, &opened, None)).lines(&still(&theme, width));
        assert_fits(&lines, width);
        let shown = text(&lines);
        assert!(shown.lines().any(|line| line.contains(" spent")), "{shown}");
        assert!(!shown.lines().any(|line| line.trim() == "spent"), "{shown}");
    }
}

#[test]
fn plan_card() {
    checkpoint_case("plan_card", plan(), None);
}

/// The plan with the workflow it follows: its phases in plain words in the main view, and its
/// conditions and loop rules under Ctrl+O.
fn plan_with_workflow() -> CheckpointOpened {
    use argonauts_core::events::checkpoint::PlanWorkflow;
    use argonauts_core::workflow::WorkflowRef;

    let mut opened = plan();
    let CheckpointPacket::Plan(packet) = &mut opened.packet else {
        unreachable!("a plan checkpoint")
    };
    let workflow = serde_json::from_value(serde_json::json!({
        "name": "peer-crew",
        "description": "Peers draft plans blind and debate only where they disagree",
        "phases": [
            {"name": "Goal", "standard": "understand", "steps": [{
                "kind": "loop_until", "until": "goal_approved", "max": 2,
                "steps": [
                    {"kind": "restate", "role": "restater"},
                    {"kind": "differ", "role": "differ"},
                    {"kind": "checkpoint", "checkpoint": "goal"}
                ]
            }]},
            {"name": "Plan", "standard": "plan", "steps": [{
                "kind": "loop_until", "until": "plan_decided", "max": 3,
                "steps": [
                    {"kind": "draft", "role": "drafter", "count": 3, "blind": true},
                    {"kind": "judge", "role": "judge", "when": "drafts_disagree"},
                    {"kind": "checkpoint", "checkpoint": "plan"}
                ]
            }]},
            {"name": "Build", "standard": "do", "steps": [{"kind": "build", "role": "owner"}]},
            {"name": "Merge", "standard": "decide", "steps": [{"kind": "merge"}]}
        ]
    }))
    .expect("a workflow");
    packet.workflow = Some(PlanWorkflow {
        from: Some(WorkflowRef::new("peer-crew")),
        why: Some("A security-labeled API feature: blind drafts and cross-judging fit it.".into()),
        workflow,
    });
    opened
}

#[test]
fn plan_card_with_workflow() {
    checkpoint_case("plan_card_with_workflow", plan_with_workflow(), None);
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
fn merge_card() {
    checkpoint_case("merge_card", merge(), None);

    // Request changes opens the rows as a checklist.
    let opened = merge();
    snapshot("merge_card_request_changes", |width| {
        let mut prompt = focused(checkpoint_card(1, &opened, None));
        let key = crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('b'));
        prompt.handle_key(key);
        card_lines(&prompt, width)
    });
}

#[test]
fn handoff_card() {
    checkpoint_case("handoff_card", handoff(), None);
}

#[test]
fn vendor_down_card() {
    checkpoint_case("vendor_down_card", vendor_down(), None);
}

#[test]
fn budget_card() {
    checkpoint_case("budget_card", budget(), Some(run_state()));
}

#[test]
fn escalation_card() {
    checkpoint_case("escalation_card", escalation(), Some(run_state()));
}

#[test]
fn original_returned_card() {
    checkpoint_case("original_returned_card", original_returned(), None);
}

#[test]
fn spot_check_card() {
    let mut opened = merge();
    opened.mode = ApprovalMode::Auto;
    let decided = CheckpointDecided {
        id: opened.id.clone(),
        action: AnswerAction::Merge,
        text: None,
        by: Party::Rules,
        evidence_sha256: opened.evidence_sha256.clone(),
        shadow: None,
        cards: Vec::new(),
    };
    let run = run_id();
    let names = DisplayNames::default();
    snapshot("spot_check_card", |width| {
        card_lines(
            &spot_card(1, &opened, Some(&decided), Some(&run), &names),
            width,
        )
    });
    let hostile = hostile(opened.clone());
    holds_its_shape(|width| {
        card_lines(
            &focused(spot_card(1, &hostile, Some(&decided), Some(&run), &names)),
            width,
        )
    });

    // Its record, and every other card's, is one line.
    let theme = theme();
    let record = record_cell(&opened, &decided);
    assert_eq!(
        text(&record.lines(&still(&theme, 80))),
        "◆ Approved merging the run into main · your rules"
    );
}

#[test]
fn choice_cards() {
    let all = choices();
    snapshot("choice_cards", |width| {
        let mut lines = Vec::new();
        for choice in &all {
            lines.extend(card_lines(&focused(choice_card(1, choice)), width));
            lines.push(Line::default());
        }
        let run = run_id();
        let take_over = confirm_take_over(1, &run, &card(2), "codex/gpt-5.6");
        lines.extend(card_lines(&focused(take_over), width));
        lines
    });
    for mut choice in all {
        choice.title = long_title();
        choice.body = vec![long_title(), samples::LONG_PATH.into()];
        for option in &mut choice.options {
            option.label = long_title();
        }
        holds_its_shape(|width| card_lines(&focused(choice_card(1, &choice)), width));
    }
}

#[test]
fn guided_setup_step() {
    let steps = guided_steps();
    snapshot("guided_setup_step", |width| {
        let mut lines = Vec::new();
        for step in &steps {
            lines.extend(card_lines(&choice_card(1, step), width));
            lines.push(Line::default());
        }
        lines
    });
    for step in &steps {
        let card = choice_card(1, step);
        let first = &card.options[0];
        assert_eq!(card.selected, 0, "Enter takes the recommended answer");
        assert!(step.kind == ChoiceKind::TermsNote || first.recommended);
        assert_eq!(card.esc_hint, "Esc to take the recommended answers");
    }
}
