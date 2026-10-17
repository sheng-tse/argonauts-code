//! The runs the workflow view's pictures show, as the views are given them: `Add due dates`,
//! an orchestrator workflow of four phases, at each moment a picture catches it.

use argonauts_core::names::{ModelRef, SessionKey, WorkflowId};
use argonauts_core::steps::plan::Severity;
use argonauts_core::views::{
    ActivityLine, AgentRow, AgentView as AgentData, CommandRow, ContextUse, Finding, Findings,
    PhaseCount, PhaseRow, ReviewGiven, RowNote, RowStatus, StatusKind, StatusLine, Tone,
    WorkflowRow, WorkflowState, WorkflowView as WorkflowData,
};
use argonauts_core::workflow::StandardPhase;
use argonauts_core::{RunId, Spend};

pub(crate) const OPUS: &str = "claude-code/claude-opus-5-5";
pub(crate) const SONNET: &str = "claude-code/claude-sonnet-5-5";
pub(crate) const GPT: &str = "codex/gpt-5.6";
pub(crate) const LUNA: &str = "codex/gpt-5.6-luna";

pub(crate) fn worker(id: &str) -> ModelRef {
    id.parse().expect("a model")
}

pub(crate) fn run() -> RunId {
    RunId::new("run-01").expect("a run id")
}

/// An agent's row: who ran it, how it stands, its tokens, and its seconds.
pub(crate) fn agent(
    session: u32,
    label: &str,
    model: &str,
    status: RowStatus,
    tokens: u64,
    seconds: u64,
) -> AgentRow {
    AgentRow {
        session: SessionKey::numbered(session),
        label: label.to_owned(),
        worker: worker(model),
        status,
        tokens,
        spent: Spend::default(),
        elapsed_ms: seconds * 1000,
        subagents: 0,
        context: None,
        fallback_for: None,
        answered: false,
        tries: Vec::new(),
        note: None,
        inner: None,
        left_out: false,
    }
}

fn noted(mut agent: AgentRow, text: &str, tone: Tone) -> WorkflowRow {
    agent.note = Some(RowNote::new(text, tone));
    WorkflowRow::Agent(agent)
}

/// A step Argonauts Code runs itself: its label, what runs, what came of it, and its seconds.
fn step(label: &str, command: &str, result: &str, tone: Tone, seconds: u64) -> WorkflowRow {
    WorkflowRow::Command(CommandRow {
        label: label.to_owned(),
        command: command.to_owned(),
        result: Some(result.to_owned()),
        tone,
        status: if tone == Tone::Bad {
            RowStatus::Failed
        } else {
            RowStatus::Done
        },
        elapsed_ms: seconds * 1000,
    })
}

fn phase(
    name: &str,
    status: RowStatus,
    count: Option<(u32, u32)>,
    rows: Vec<WorkflowRow>,
) -> PhaseRow {
    PhaseRow {
        name: name.to_owned(),
        detail: String::new(),
        standard: StandardPhase::Do,
        status,
        try_number: None,
        count: count.and_then(|(done, total)| PhaseCount::of(done, total)),
        rows,
        note: None,
    }
}

fn not_started(name: &str) -> PhaseRow {
    phase(name, RowStatus::Waiting, None, Vec::new())
}

fn workflow(agents: (u32, u32), seconds: u64, phases: Vec<PhaseRow>) -> WorkflowData {
    WorkflowData {
        run: run(),
        id: WorkflowId::numbered(1),
        name: "Add due dates".to_owned(),
        description: "add --due to the todo CLI, each part reviewed on another CLI".to_owned(),
        style: None,
        status_lines: Vec::new(),
        agents_done: agents.0,
        agents_total: agents.1,
        elapsed_ms: seconds * 1000,
        spent: Spend::default(),
        phases,
        notes: vec!["orchestrator workflow".to_owned()],
        required: Vec::new(),
        goals: Vec::new(),
        worker: None,
        state: WorkflowState::Running,
    }
}

fn says(mut data: WorkflowData, kind: StatusKind, text: &str, tone: Tone) -> WorkflowData {
    data.status_lines = vec![StatusLine::new(kind, text).toned(tone)];
    data
}

/// The two builders, the second done and the first on its second round or done too.
fn build(flag: RowStatus) -> Vec<WorkflowRow> {
    vec![
        WorkflowRow::Agent(agent(1, "build:due-flag", SONNET, flag, 57_800, 160)),
        noted(
            agent(2, "build:due-tests", GPT, RowStatus::Done, 30_300, 62),
            "passed",
            Tone::Good,
        ),
    ]
}

fn reviews() -> Vec<WorkflowRow> {
    vec![
        noted(
            agent(3, "review:due-tests", OPUS, RowStatus::Done, 18_400, 41),
            "approved",
            Tone::Good,
        ),
        noted(
            agent(4, "review:due-flag", LUNA, RowStatus::Done, 21_700, 46),
            "sent it back · 2 findings",
            Tone::Warn,
        ),
    ]
}

/// Mid-review: one reviewer approved, the other sent its part back, and the worker is on its
/// second round.
pub(crate) fn running() -> WorkflowData {
    let mut review = phase("Review", RowStatus::Running, Some((1, 2)), reviews());
    review.note = Some(
        "build:due-flag is on round 2 with the findings · its review runs again after".to_owned(),
    );
    let data = workflow(
        (2, 4),
        252,
        vec![
            phase(
                "Build",
                RowStatus::Running,
                Some((1, 2)),
                build(RowStatus::Running),
            ),
            review,
            not_started("Sign-off"),
            not_started("Merge"),
        ],
    );
    says(
        data,
        StatusKind::Note,
        "review: GPT 5.6 Luna sent build:due-flag back; round 2 is under way",
        Tone::Plain,
    )
}

/// Opus 5.5 is overloaded, so Sonnet 5.5 reviews in its place.
pub(crate) fn fallback() -> WorkflowData {
    let mut reviewer = agent(3, "review:due-tests", SONNET, RowStatus::Running, 9_300, 38);
    reviewer.fallback_for = Some(worker(OPUS));
    let data = workflow(
        (1, 3),
        146,
        vec![
            phase(
                "Build",
                RowStatus::Running,
                Some((1, 2)),
                build(RowStatus::Running),
            ),
            phase(
                "Review",
                RowStatus::Running,
                Some((0, 1)),
                vec![noted(reviewer, "overloaded", Tone::Plain)],
            ),
            not_started("Sign-off"),
            not_started("Merge"),
        ],
    );
    says(
        data,
        StatusKind::Note,
        "Opus 5.5 on Claude Code is overloaded · fell back to Sonnet 5.5 on Claude Code, as \
         same_cli asks",
        Tone::Plain,
    )
}

/// Claude Code is down: its two tasks wait for it with their work kept, and the run waits for
/// you.
pub(crate) fn cli_down() -> WorkflowData {
    let builders = vec![
        noted(
            agent(1, "build:due-flag", SONNET, RowStatus::Paused, 44_000, 106),
            "waits for Claude Code · work so far kept",
            Tone::Warn,
        ),
        noted(
            agent(2, "build:due-tests", GPT, RowStatus::Done, 30_300, 62),
            "passed",
            Tone::Good,
        ),
    ];
    let review = WorkflowRow::Agent(agent(3, "review:due-tests", OPUS, RowStatus::Paused, 0, 0));
    let mut data = workflow(
        (1, 3),
        166,
        vec![
            phase("Build", RowStatus::Running, Some((1, 2)), builders),
            phase("Review", RowStatus::Waiting, Some((0, 1)), vec![review]),
            not_started("Sign-off"),
            not_started("Merge"),
        ],
    );
    data.state = WorkflowState::Paused;
    // Its next test is 4m48s away as the view is built, and the screen counts it down.
    let waits = StatusLine::new(
        StatusKind::NeedsYou,
        "Claude Code is down · 2 tasks wait for it",
    )
    .counting("tried again in", 288_000);
    data.status_lines = vec![waits];
    data
}

/// How the run's work reached your branch or your folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Merge {
    Clean,
    /// Two tasks changed the same lines, and a model settled it.
    Conflict,
    /// Your branch moved, a test broke with it, and a model fixed it.
    BranchMoved,
    /// A folder without git, where the work is written into your files.
    NoGit,
}

fn merge_rows(merge: Merge) -> Vec<WorkflowRow> {
    let passing = |pass: &str, seconds: u64| {
        let result = format!("{pass} pass, as at the start");
        step("final check", "npm test", &result, Tone::Plain, seconds)
    };
    let model = |session: u32, label: &str, tokens: u64, seconds: u64| {
        agent(session, label, OPUS, RowStatus::Done, tokens, seconds)
    };
    let message = |words: Option<&str>| match words {
        Some(words) => noted(model(7, "commit message", 11_800, 19), words, Tone::Plain),
        None => WorkflowRow::Agent(model(7, "commit message", 11_800, 19)),
    };
    let onto_main = |result: &str| step("merge onto main", "git", result, Tone::Plain, 2);
    match merge {
        Merge::Clean => vec![
            passing("15 of 16", 7),
            message(Some("given the diff, no tools")),
            onto_main("2 tasks · no conflicts"),
        ],
        Merge::Conflict => vec![
            step(
                "merge the tasks",
                "git",
                "conflict in src/cli.js",
                Tone::Warn,
                1,
            ),
            noted(
                model(6, "settle conflict", 9_600, 41),
                "kept both changes to the --due parsing",
                Tone::Plain,
            ),
            passing("15 of 16", 7),
            message(None),
            onto_main("2 tasks · 1 conflict settled"),
        ],
        Merge::BranchMoved => vec![
            step(
                "catch up with main",
                "git",
                "main moved 2 commits · no conflicts",
                Tone::Plain,
                1,
            ),
            step(
                "final check",
                "npm test",
                "1 test broke after main came in",
                Tone::Bad,
                7,
            ),
            noted(
                model(6, "fix after main moved", 14_200, 52),
                "1 file",
                Tone::Plain,
            ),
            passing("16 of 17", 8),
            message(None),
            onto_main("2 tasks · after your OK"),
        ],
        Merge::NoGit => vec![
            passing("15 of 16", 7),
            step(
                "write into your folder",
                "history kept by Argonauts",
                "2 tasks",
                Tone::Plain,
                2,
            ),
        ],
    }
}

/// What the Merge phase says under its steps in a folder without git.
pub(crate) const NO_COMMIT_MESSAGE: &str =
    "no model writes a commit message here: only /rewind reads the history";

/// The run over: built, reviewed, signed off, and merged as `merge` says.
pub(crate) fn done(merge: Merge) -> WorkflowData {
    let models = match merge {
        Merge::Clean => 1,
        Merge::Conflict | Merge::BranchMoved => 2,
        Merge::NoGit => 0,
    };
    let sign_off = noted(
        agent(
            5,
            "sign-off:add-due-dates",
            OPUS,
            RowStatus::Done,
            12_100,
            33,
        ),
        "approved",
        Tone::Good,
    );
    let mut approved = reviews();
    approved[1] = noted(
        agent(4, "review:due-flag", LUNA, RowStatus::Done, 20_900, 44),
        "approved",
        Tone::Good,
    );
    let mut data = workflow(
        (5 + models, 5 + models),
        380,
        vec![
            phase(
                "Build",
                RowStatus::Done,
                Some((2, 2)),
                build(RowStatus::Done),
            ),
            phase("Review", RowStatus::Done, Some((2, 2)), approved),
            phase("Sign-off", RowStatus::Done, Some((1, 1)), vec![sign_off]),
            phase(
                "Merge",
                RowStatus::Done,
                Some((models, models)),
                merge_rows(merge),
            ),
        ],
    );
    data.state = WorkflowState::Done;
    if merge == Merge::NoGit
        && let Some(merge) = data.phases.last_mut()
    {
        merge.note = Some(NO_COMMIT_MESSAGE.to_owned());
    }
    let ended = match merge {
        Merge::NoGit => "Written into your folder · 4 files +52 −6 · /rewind puts it back",
        _ => "Merged onto main · 3f9c2a1 · 4 files +52 −6",
    };
    says(data, StatusKind::Ended, ended, Tone::Good)
}

/// A bigger run, `Add recurring tasks`: six parts built, their six reviews under way.
pub(crate) fn big() -> WorkflowData {
    let parts = ["add", "parse", "store", "list", "done", "tests"];
    let builders = parts
        .iter()
        .enumerate()
        .map(|(at, part)| {
            let label = format!("build:every-{part}");
            let model = if at % 2 == 0 { SONNET } else { GPT };
            let row = agent(at as u32 + 1, &label, model, RowStatus::Done, 40_000, 120);
            WorkflowRow::Agent(row)
        })
        .collect();
    let review = |at: u32, part: &str, model: &str, status: RowStatus, tokens, seconds| {
        agent(
            at,
            &format!("review:every-{part}"),
            model,
            status,
            tokens,
            seconds,
        )
    };
    let approved = |row: AgentRow| noted(row, "approved", Tone::Good);
    let reviewers = vec![
        approved(review(7, "add", OPUS, RowStatus::Done, 16_900, 58)),
        approved(review(8, "parse", LUNA, RowStatus::Done, 19_400, 76)),
        approved(review(9, "store", OPUS, RowStatus::Done, 17_600, 67)),
        WorkflowRow::Agent(review(10, "list", LUNA, RowStatus::Running, 18_200, 116)),
        WorkflowRow::Agent(review(11, "done", LUNA, RowStatus::Running, 21_400, 112)),
        WorkflowRow::Agent(review(12, "tests", OPUS, RowStatus::Queued, 0, 0)),
    ];
    let mut data = workflow(
        (9, 12),
        452,
        vec![
            phase("Build", RowStatus::Done, Some((6, 6)), builders),
            phase("Review", RowStatus::Running, Some((3, 6)), reviewers),
            not_started("Sign-off"),
            not_started("Merge"),
        ],
    );
    data.name = "Add recurring tasks".to_owned();
    data.description = "add --every to the todo CLI, six parts, each reviewed".to_owned();
    says(
        data,
        StatusKind::Note,
        "review: 2 reviewers at work; review:every-tests waits for a free slot",
        Tone::Plain,
    )
}

/// The two findings the reviewer of `build:due-flag` raised.
fn findings() -> Vec<Finding> {
    let finding = |severity, line: u32, text: &str| Finding {
        severity,
        place: format!("src/cli.js:{line}"),
        text: text.to_owned(),
    };
    vec![
        finding(Severity::High, 42, "--due with no date isn't rejected"),
        finding(Severity::Low, 12, "--help doesn't mention --due"),
    ]
}

fn call(tool: &str, summary: &str, ok: Option<bool>) -> ActivityLine {
    ActivityLine {
        tool: tool.to_owned(),
        summary: summary.to_owned(),
        ok,
        note: None,
    }
}

/// A prompt of `lines` lines that opens with `first`.
fn prompt(first: &str, lines: usize) -> String {
    let rest = (2..=lines).map(|line| format!("Its line {line}."));
    std::iter::once(first.to_owned())
        .chain(rest)
        .collect::<Vec<_>>()
        .join("\n")
}

fn opened(row: AgentRow, prompt: String, activity: Vec<ActivityLine>, calls: u32) -> AgentData {
    AgentData {
        run: run(),
        row,
        prompt,
        activity,
        tool_calls: calls,
        diff: None,
        subagents: None,
        outcome: None,
        goal: None,
        plan: None,
        facts: Vec::new(),
        to_fix: None,
        asked_for: None,
        earlier: None,
        moved: None,
        review: None,
    }
}

/// `build:due-flag` of [`running`], opened: on its second round, with the findings it was sent
/// back for.
pub(crate) fn worker_opened() -> AgentData {
    let mut row = agent(1, "build:due-flag", SONNET, RowStatus::Running, 57_800, 160);
    row.context = Some(ContextUse {
        used: 62_000,
        cap: 200_000,
    });
    let activity = vec![
        call("Read", "src/cli.js", Some(true)),
        call("Edit", "src/cli.js", Some(true)),
        call("Bash", "npm test", None),
    ];
    let first = "Add --due <date> to todo add, and show the date in todo list.";
    let mut data = opened(row, prompt(first, 9), activity, 23);
    data.facts = ["worker", "worktree", "round 2 (sent back)"]
        .map(str::to_owned)
        .to_vec();
    data.to_fix = Some(Findings {
        from: "review:due-flag".to_owned(),
        round: 1,
        items: findings(),
    });
    data
}

/// `review:due-flag` of [`running`], opened: the reviewer that sent the work back.
pub(crate) fn reviewer_opened() -> AgentData {
    let row = agent(4, "review:due-flag", LUNA, RowStatus::Done, 21_700, 46);
    let activity = vec![
        call("Read", "src/cli.js", Some(true)),
        call("Bash", "npm test -- --grep due", Some(true)),
        call("Read", "test/cli.test.js", Some(true)),
    ];
    let first = "Review build:due-flag's change: the --due flag on todo add and todo list.";
    let mut data = opened(row, prompt(first, 6), activity, 9);
    data.facts = ["reviewer", "read-only"].map(str::to_owned).to_vec();
    data.review = Some(ReviewGiven {
        verdict: "sent it back".to_owned(),
        tone: Tone::Warn,
        findings: findings(),
    });
    data
}
