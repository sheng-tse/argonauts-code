//! The pictures' numbers: a sample main session on Opus 5.5, in Claude Code 2.1.304's form,
//! and the spec-first run's crew, which made the rate limiter per API key between 8:30 and
//! 8:33 AM.

use argonauts_core::RunId;
use argonauts_core::Spend;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::context::{
    ContextCategory, ContextCell, ContextKind, ContextSkill, ContextSkills, ContextUsage, McpTool,
};
use argonauts_core::events::session::{
    InputPart, InputPartKind, LaunchRecord, PromptVia, SessionInput,
};
use argonauts_core::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::views::{ContextRow, ContextView, EndedHow, RunFinishedAt, SessionLeft};
use time::OffsetDateTime;
use time::macros::datetime;

use super::CrewFacts;
use crate::bridge::tools::Places;
use crate::state::{AppState, ContextCount, RunState, SessionState};

pub const RUN_TITLE: &str = "Make the rate limiter per API key";

/// One state of the pictures, with everything the tab draws from.
pub struct Example {
    pub count: Option<ContextCount>,
    pub view: ContextView,
    pub names: DisplayNames,
    pub places: Places,
    pub now: OffsetDateTime,
    /// The sessions that settled another agent's question.
    tie_breakers: Vec<SessionKey>,
    /// What Argonauts Code sent a session, by the prompt's own count.
    prompts: Vec<(SessionKey, u32)>,
}

impl Example {
    /// The screen's state as the pictures show it: the main session's count, and the run
    /// with its sessions as the screen followed them.
    pub fn state(&self) -> AppState {
        let mut state = AppState::default();
        state.names = self.names.clone();
        state.conversation.context = self.count.clone();
        state.conversation.worker = self
            .view
            .rows
            .iter()
            .find(|row| row.run.is_none())
            .map(|row| row.worker.clone());
        let mut run = RunState::new(self::run());
        run.title = RUN_TITLE.to_owned();
        for row in self.view.rows.iter().filter(|row| row.run.is_some()) {
            let role = if self.tie_breakers.contains(&row.session) {
                RoleName::TieBreaker
            } else {
                RoleName::Worker
            };
            let mut session = SessionState::new(
                row.session.clone(),
                row.worker.clone(),
                row.worker.agent.clone(),
                role,
                now(),
            );
            if let Some((_, tokens)) = self.prompts.iter().find(|(key, _)| *key == row.session) {
                session.inputs.push(prompt(*tokens));
            }
            run.sessions.insert(row.session.clone(), session);
        }
        state.runs.insert(self::run(), run);
        state
    }

    /// What the tab knows of the crew's sessions from that state.
    pub fn facts(&self) -> CrewFacts {
        CrewFacts::of(&self.state(), &self.view)
    }
}

/// A prompt of `tokens`, as its session's input side has it.
fn prompt(tokens: u32) -> SessionInput {
    SessionInput {
        launch: LaunchRecord {
            program: "codex".to_owned(),
            args: vec!["exec".to_owned()],
            cwd: "/home/you/rate-limiter".into(),
            env_set: Vec::new(),
            env_removed: Vec::new(),
            prompt_via: PromptVia::Stdin,
            sandbox: None,
        },
        parts: vec![InputPart {
            kind: InputPartKind::Prompt,
            label: "prompt".to_owned(),
            text: "Make the rate limiter per API key.".to_owned(),
            approx_tokens: tokens,
            source: None,
        }],
        queued: Vec::new(),
    }
}

/// A little after the run ended, in the zone its times show in.
pub fn now() -> OffsetDateTime {
    datetime!(2026-10-20 08:35 -4)
}

/// The run the pictures' crew worked in.
pub fn run() -> RunId {
    RunId::new("run-20261020-123036-4e50").expect("a run id")
}

/// The main session as its CLI counted it, with the crew of the run before it.
pub fn after_the_run() -> Example {
    let usage = main_usage();
    let main = ContextRow {
        run: None,
        session: SessionKey::numbered(100),
        label: "main".to_owned(),
        worker: model("claude-code/claude-opus-5-5"),
        used: usage.total_tokens,
        window: Some(usage.window),
        cap: None,
        tool_calls: 0,
        spent: Spend::default(),
        rotations: 0,
        estimated: false,
        counts: true,
        categories: Vec::new(),
        recorded: None,
        ended: None,
        answered_for: None,
    };
    let mut example = straight_to_a_run();
    example.view.rows.insert(0, main);
    example.count = Some(ContextCount {
        usage,
        counted: now() - time::Duration::seconds(6),
    });
    example
}

/// The same run where the request went straight to it, with no session of the conversation.
pub fn straight_to_a_run() -> Example {
    let run = run();
    let mut rows = Vec::new();
    let mut tie_breakers = Vec::new();
    let mut prompts = Vec::new();
    let mut add = |label: &str, worker: &str, used: u64, window: u64, ended: (u8, u8)| {
        let n = rows.len() as u32 + 1;
        rows.push(ended_row(n, label, worker, used, window, ended));
        SessionKey::numbered(n)
    };
    let haiku = "claude-code/claude-haiku-5-5";
    let luna = "codex/gpt-6-luna";
    add("plan", haiku, 13_300, 1_000_000, (12, 30));
    add(
        "build:cap-bucket-refills",
        haiku,
        21_000,
        1_000_000,
        (12, 31),
    );
    // The local reviewer it was meant to have never started.
    add(
        "review:cap-bucket-refills",
        "opencode/ollama/qwen3-coder:30b-64k",
        0,
        0,
        (12, 30),
    );
    add("review:cap-bucket-refills", luna, 17_600, 872_000, (12, 31));
    // Two tries on one task: Haiku, resumed after each of its three questions, then Luna.
    let mut answered = Vec::new();
    let asker = add("build:per-api-key", haiku, 18_200, 1_000_000, (12, 31));
    let breaker = add(
        "build:per-api-key",
        "claude-code/claude-opus-5-5",
        3_100,
        1_000_000,
        (12, 31),
    );
    answered.push((breaker.clone(), asker));
    tie_breakers.push(breaker);
    let asker = add("build:per-api-key", haiku, 22_400, 1_000_000, (12, 32));
    let breaker = add(
        "build:per-api-key",
        "claude-code/claude-opus-5-5",
        3_300,
        1_000_000,
        (12, 32),
    );
    answered.push((breaker.clone(), asker));
    tie_breakers.push(breaker);
    let asker = add("build:per-api-key", haiku, 24_900, 1_000_000, (12, 32));
    let breaker = add(
        "build:per-api-key",
        "claude-code/claude-opus-5-5",
        3_200,
        1_000_000,
        (12, 32),
    );
    answered.push((breaker.clone(), asker));
    tie_breakers.push(breaker);
    let first = add("build:per-api-key", luna, 26_800, 872_000, (12, 32));
    prompts.push((first, 3_902));
    add("review:per-api-key", haiku, 14_800, 1_000_000, (12, 32));
    let resumed = add("build:per-api-key", luna, 30_500, 872_000, (12, 33));
    prompts.push((resumed, 1_124));
    add("review:per-api-key-2", haiku, 14_900, 1_000_000, (12, 33));
    add("main:answer", haiku, 14_600, 1_000_000, (12, 33));
    // Each tie-breaker answered the question of the try before it, as the app's rows say.
    for (breaker, asker) in answered {
        if let Some(row) = rows.iter_mut().find(|row| row.session == breaker) {
            row.answered_for = Some(asker);
        }
    }
    let mut view = ContextView::of(rows);
    view.finished.push(RunFinishedAt {
        run,
        at: datetime!(2026-10-20 12:33:40 UTC),
    });
    Example {
        count: None,
        view,
        tie_breakers,
        prompts,
        names: crate::overview::sample::names(),
        places: Places {
            folder: Some("/home/you/rate-limiter".to_owned()),
            home: Some("/home/you".to_owned()),
        },
        now: now(),
    }
}

/// A session of the run as it ended. One with no window never started.
fn ended_row(
    n: u32,
    label: &str,
    worker: &str,
    used: u64,
    window: u64,
    (hour, minute): (u8, u8),
) -> ContextRow {
    let started = window > 0;
    let at = datetime!(2026-10-20 00:00 UTC)
        + time::Duration::hours(i64::from(hour))
        + time::Duration::minutes(i64::from(minute));
    let worker = model(worker);
    ContextRow {
        run: Some(run()),
        session: SessionKey::numbered(n),
        label: label.to_owned(),
        counts: worker.agent == "claude-code",
        worker,
        used,
        window: started.then_some(window),
        cap: None,
        tool_calls: if started { 6 } else { 0 },
        spent: Spend {
            cost_usd: 0.004,
            ..Spend::default()
        },
        rotations: 0,
        estimated: false,
        categories: Vec::new(),
        recorded: None,
        ended: Some(SessionLeft {
            at,
            how: if started {
                EndedHow::Done
            } else {
                EndedHow::Failed {
                    why: "Ollama runs it without tools".to_owned(),
                }
            },
            fallback_for: None,
        }),
        answered_for: None,
    }
}

fn model(worker: &str) -> ModelRef {
    worker.parse().expect("a model")
}

/// A sample main session in Claude Code 2.1.304's form: 182.5k of a 1m window on Opus 5.5,
/// with 237 MCP tools that load when used and 32 skills.
fn main_usage() -> ContextUsage {
    let category = |name: &str, tokens: u64, color: &str, kind| ContextCategory {
        name: name.to_owned(),
        tokens,
        color: color.to_owned(),
        kind,
    };
    let used = [
        category("System prompt", 4_400, "promptBorder", ContextKind::Used),
        category("System tools", 19_700, "inactive", ContextKind::Used),
        category(
            "MCP server instructions",
            960,
            "green_FOR_SUBAGENTS_ONLY",
            ContextKind::Used,
        ),
        category("Skills", 6_000, "warning", ContextKind::Used),
        category(
            "Messages",
            151_400,
            "purple_FOR_SUBAGENTS_ONLY",
            ContextKind::Used,
        ),
    ];
    let total: u64 = used.iter().map(|part| part.tokens).sum();
    let buffer = category(
        "Autocompact buffer",
        33_000,
        "inactive",
        ContextKind::Buffer,
    );
    let free = category(
        "Free space",
        1_000_000 - total - 33_000,
        "promptBorder",
        ContextKind::Free,
    );
    let grid = grid(&used, &buffer, &free);
    let mut categories = used.to_vec();
    categories.push(buffer);
    categories.push(free);
    let mut mcp_tools = Vec::new();
    for (server, n) in [("crm", 193), ("weather-forecast", 35), ("argonauts", 9)] {
        for at in 0..n {
            mcp_tools.push(McpTool {
                name: format!("mcp__{server}__tool_{at}"),
                server: server.to_owned(),
                tokens: 120,
                loaded: false,
            });
        }
    }
    let skill = |source: &str, tokens| ContextSkill {
        name: "skill".to_owned(),
        source: source.to_owned(),
        plugin: None,
        tokens,
    };
    let mut items = vec![skill("userSettings", 110)];
    items.extend((0..11).map(|at| skill("syncedSkills", if at == 0 { 230 } else { 297 })));
    items.extend((0..20).map(|_| skill("built-in", 135)));
    ContextUsage {
        categories,
        total_tokens: total,
        window: 1_000_000,
        raw_window: 1_000_000,
        percentage: 18,
        grid,
        model: Some("claude-opus-5-5".to_owned()),
        auto_compact: true,
        auto_compact_threshold: Some(967_000),
        auto_compact_source: "model-default".to_owned(),
        mcp_tools,
        skills: Some(ContextSkills {
            total: 32,
            included: 32,
            tokens: 6_000,
            items,
        }),
        ..ContextUsage::default()
    }
}

/// Claude Code's grid for the count: each part's cells in order, a 5k share each and at least
/// one, the buffer's last, and free space between.
fn grid(
    used: &[ContextCategory],
    buffer: &ContextCategory,
    free: &ContextCategory,
) -> Vec<Vec<ContextCell>> {
    let cell = |of: &ContextCategory| ContextCell {
        category: of.name.clone(),
        color: of.color.clone(),
        filled: of.kind != ContextKind::Free,
        tokens: of.tokens,
        percentage: 0,
        fullness: 1.0,
    };
    let mut cells = Vec::new();
    for part in used {
        let n = ((part.tokens as f64 / 5_000.0).round() as usize).max(1);
        cells.extend(std::iter::repeat_n(cell(part), n));
    }
    let held = (buffer.tokens as f64 / 5_000.0).round() as usize;
    let room = 200 - cells.len() - held;
    cells.extend(std::iter::repeat_n(cell(free), room));
    cells.extend(std::iter::repeat_n(cell(buffer), held));
    cells.chunks(20).map(<[ContextCell]>::to_vec).collect()
}
