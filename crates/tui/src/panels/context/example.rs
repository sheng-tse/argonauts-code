//! The pictures' numbers: a sample main session on Opus 5.5, in Claude Code 2.1.304's form,
//! and the spec-first run's crew, which made the rate limiter per API key between 8:30 and
//! 8:33 AM.

use argonauts_core::RunId;
use argonauts_core::Spend;
use argonauts_core::app::views::{ContextRow, ContextView, EndedHow, RunFinishedAt, SessionLeft};
use argonauts_core::base::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::runs::events::context::{
    ContextCategory, ContextCell, ContextKind, ContextSkill, ContextSkills, ContextUsage, McpTool,
    MemoryFile,
};
use argonauts_core::runs::events::session::{
    InputPart, InputPartKind, LaunchRecord, PromptVia, SessionInput,
};
use time::OffsetDateTime;
use time::macros::datetime;

use super::CrewFacts;
use crate::frame::state::Places;
use crate::frame::state::{AppState, ContextCount, RunState, SessionState};

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

/// The main session alone, as Claude Code 2.1.311 counted it on Opus 5.5 at its start.
pub fn claude_code_alone() -> Example {
    let usage = claude_code_usage();
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
    Example {
        count: Some(ContextCount {
            usage,
            counted: now() - time::Duration::seconds(6),
        }),
        view: ContextView::of(vec![main]),
        names: crate::testing::sample::names(),
        places: Places {
            folder: Some("/home/you/rate-limiter".to_owned()),
            home: Some("/home/you".to_owned()),
        },
        now: now(),
        tie_breakers: Vec::new(),
        prompts: Vec::new(),
    }
}

/// Claude Code's count of a fresh session: every category it lists, with no grid of its own,
/// so the tab lays the grid out by Claude Code's rule.
fn claude_code_usage() -> ContextUsage {
    let category = |name: &str, tokens: u64, color: &str, kind| ContextCategory {
        name: name.to_owned(),
        tokens,
        color: color.to_owned(),
        kind,
    };
    let mut categories = vec![
        category("System prompt", 4_380, "promptBorder", ContextKind::Used),
        category("System tools", 19_380, "inactive", ContextKind::Used),
        category(
            "MCP tools",
            MCP_TOOLS.iter().map(|(_, tokens)| tokens).sum(),
            "cyan_FOR_SUBAGENTS_ONLY",
            ContextKind::Used,
        ),
        category(
            "MCP server instructions",
            MCP_NOTE,
            "green_FOR_SUBAGENTS_ONLY",
            ContextKind::Used,
        ),
        category("Memory files", 503, "claude", ContextKind::Used),
        category("Skills", skills().tokens, "warning", ContextKind::Used),
        category(
            "Messages",
            10,
            "purple_FOR_SUBAGENTS_ONLY",
            ContextKind::Used,
        ),
    ];
    let total: u64 = categories.iter().map(|part| part.tokens).sum();
    categories.push(category(
        "Autocompact buffer",
        33_000,
        "inactive",
        ContextKind::Buffer,
    ));
    categories.push(category(
        "Free space",
        1_000_000 - total - 33_000,
        "promptBorder",
        ContextKind::Free,
    ));
    ContextUsage {
        categories,
        total_tokens: total,
        window: 1_000_000,
        raw_window: 1_000_000,
        percentage: 3,
        model: Some("claude-opus-5-5".to_owned()),
        auto_compact: true,
        auto_compact_threshold: Some(967_000),
        auto_compact_source: "settings".to_owned(),
        memory_files: vec![MemoryFile {
            path: "/home/you/rate-limiter/CLAUDE.md".to_owned(),
            scope: "Project".to_owned(),
            tokens: 503,
        }],
        mcp_tools: mcp_tools(),
        skills: Some(skills()),
        ..ContextUsage::default()
    }
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
        "opencode/ollama/qwen3-coder:30b",
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
        names: crate::testing::sample::names(),
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

/// A sample main session in Claude Code 2.1.304's form: 182.5k of a 1m window on Opus 5.5, in the
/// made-up setup the other samples share, with Argonauts Code's own 5 tools beside its 9, all
/// loading when used.
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
            MCP_NOTE,
            "green_FOR_SUBAGENTS_ONLY",
            ContextKind::Used,
        ),
        category("Skills", skills().tokens, "warning", ContextKind::Used),
        category(
            "Messages",
            155_938,
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
        mcp_tools: MCP_TOOLS
            .iter()
            .map(|&(name, _)| name.to_owned())
            .chain((0..5).map(|at| format!("mcp__argonauts__tool_{at}")))
            .map(|name| McpTool {
                server: server_of(&name),
                name,
                tokens: 120,
                loaded: false,
            })
            .collect(),
        skills: Some(skills()),
        ..ContextUsage::default()
    }
}

/// The made-up setup every sample count shares: 2 MCP servers with 9 tools, all loaded, and 4
/// skills of its own beside Claude Code's 14.
const MCP_TOOLS: [(&str, u64); 9] = [
    ("mcp__tickets__open_ticket", 212),
    ("mcp__tickets__find_tickets", 245),
    ("mcp__tickets__comment", 198),
    ("mcp__tickets__close_ticket", 306),
    ("mcp__tickets__assign", 187),
    ("mcp__warehouse__stock_level", 264),
    ("mcp__warehouse__reserve", 229),
    ("mcp__warehouse__release", 281),
    ("mcp__warehouse__list_bins", 193),
];
const MCP_NOTE: u64 = 318;
const OWN_SKILLS: [(&str, u64); 4] = [
    ("release-notes", 96),
    ("db-migrate", 121),
    ("lint-fix", 88),
    ("changelog", 143),
];
/// Claude Code 2.1.289's 14 built-in skills, as its count before the first answer gives them.
const BUILT_IN_SKILLS: u64 = 1_656;

fn mcp_tools() -> Vec<McpTool> {
    MCP_TOOLS
        .iter()
        .map(|&(name, tokens)| McpTool {
            name: name.to_owned(),
            server: server_of(name),
            tokens,
            loaded: true,
        })
        .collect()
}

/// The server in a tool's name, `mcp__<server>__<tool>`.
fn server_of(tool: &str) -> String {
    tool.split("__").nth(1).unwrap_or(tool).to_owned()
}

fn skills() -> ContextSkills {
    let skill = |name: String, source: &str, tokens| ContextSkill {
        name,
        source: source.to_owned(),
        plugin: None,
        tokens,
    };
    let mut items: Vec<ContextSkill> = (0..14u64)
        .map(|at| {
            let tokens = BUILT_IN_SKILLS / 14 + u64::from(at < BUILT_IN_SKILLS % 14);
            skill(format!("built-in-{at}"), "bundled", tokens)
        })
        .collect();
    items.extend(
        OWN_SKILLS
            .iter()
            .map(|&(name, tokens)| skill(name.to_owned(), "userSettings", tokens)),
    );
    ContextSkills {
        total: items.len() as u32,
        included: items.len() as u32,
        tokens: items.iter().map(|item| item.tokens).sum(),
        items,
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
