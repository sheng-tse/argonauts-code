//! The live region's own rows as the status sync builds them from the app state.

mod common;

use argonauts_core::app::{AppEvent, AppMode, QuotaChip, StatusLine};
use argonauts_core::events::card::CardState;
use argonauts_core::events::checkpoint::{
    CheckpointKind, CheckpointOpened, CheckpointPacket, FreeText, QuestionPacket,
};
use argonauts_core::events::run::{RunMode, RunPhase};
use argonauts_core::events::vendor::RunBudget;
use argonauts_core::limits::{HealthState, QuotaReading, QuotaSource, QuotaStatus};
use argonauts_core::names::{CheckpointId, RoleName, SessionKey, TurnId, VendorId, WorkerRef};
use argonauts_core::policy::{NarratorLine, NarratorLineKind, NarratorStatus};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit};
use argonauts_core::settings::ui::{NarratorMode, Theme as ThemeChoice, UiSettings};
use argonauts_core::views::ContextUse;
use argonauts_core::{CardId, KeyScheme, RunId, Spend};
use argonauts_tui::bottom::status_line;
use argonauts_tui::bridge::settings::theme_for;
use argonauts_tui::bridge::{Cx, Effects, status};
use argonauts_tui::model::{ViewModel, VimMode};
use argonauts_tui::stability;
use argonauts_tui::state::{AppState, CardRow, Doing, OpenCheckpoint, RunState, SessionState};
use argonauts_tui::theme::{ColorEnv, Role};
use common::{at, ctx, fg, style_of, styled, text, theme};
use ratatui::text::{Line, Span};
use time::macros::datetime;
use time::{Duration, OffsetDateTime};

/// 4m 12s into the run, with the spinner mid-sweep.
const NOW: f64 = 252.48;

const T0: OffsetDateTime = datetime!(2026-09-30 10:00 UTC);

fn run_id(n: u32) -> RunId {
    RunId::new(format!("run-20260930-100000-{n:04}")).unwrap()
}

fn worker(name: &str, model: &str) -> WorkerRef {
    WorkerRef::new(name, Some(model.to_owned()))
}

fn session(n: u32, who: WorkerRef, tokens_out: u64) -> SessionState {
    let vendor = who.worker.clone();
    let mut session = SessionState::new(SessionKey::numbered(n), who, vendor, RoleName::Owner, T0);
    session.follow = Some(n as u8);
    session.status = argonauts_tui::state::SessionStatus::Working;
    session.tokens_out = tokens_out;
    session
}

fn status_line_with(quotas: Vec<QuotaChip>) -> StatusLine {
    StatusLine {
        title: "Fix the export race".into(),
        repo: "~/argonauts".into(),
        preset: "peer-crew".into(),
        all_hands: false,
        run: None,
        default_agent: worker("claude-code", "opus"),
        quotas,
        spent: None,
        limit: None,
        narrator: NarratorMode::OnDemand,
        approval_mode: ApprovalMode::Supervised,
    }
}

fn reading(vendor: &str, window: &str, used: f64, age_minutes: i64) -> QuotaReading {
    QuotaReading {
        vendor: VendorId(vendor.into()),
        status: QuotaStatus::Allowed,
        used: Some(used),
        resets_at: None,
        window: Some(window.into()),
        source: QuotaSource::RateLimitEvent,
        observed_at: OffsetDateTime::now_utc() - Duration::minutes(age_minutes),
        detail: None,
    }
}

fn chip(label: &str, reading: Option<QuotaReading>) -> QuotaChip {
    QuotaChip {
        label: label.into(),
        reading,
        health: HealthState::Healthy,
    }
}

/// A crew run 4m 12s in: Build, 2 of 5 cards merged, 3 agents, $0.21 of $5.00 spent.
fn crew() -> AppState {
    let mut state = AppState::default();
    let id = run_id(1);
    let mut run = RunState::new(id.clone());
    run.title = "Fix the export race".into();
    run.mode = Some(RunMode::Crew);
    run.preset = Some("peer-crew".into());
    run.phase = Some(RunPhase::Build);
    run.started = Some(T0);
    run.updated = Some(T0 + Duration::seconds(252));
    for n in 1..=5 {
        let card = CardId::new(format!("card-{n:03}")).unwrap();
        let mut row = CardRow::new(card.clone(), "Card");
        row.state = Some(if n <= 2 {
            CardState::Integrated
        } else {
            CardState::Running
        });
        run.cards.insert(card, row);
    }
    for session in [
        session(1, worker("claude-code", "opus"), 18_100),
        session(2, worker("codex", "gpt-5.6"), 12_900),
        session(3, worker("agy", "gemini-3.6"), 17_200),
    ] {
        run.session_order.push(session.key.clone());
        run.sessions.insert(session.key.clone(), session);
    }
    run.budget = Some(RunBudget {
        spent: Spend {
            cost_usd: 0.21,
            ..Spend::default()
        },
        limit: Some(BudgetAmount {
            unit: BudgetUnit::Usd,
            amount: 5.0,
        }),
        lines: Vec::new(),
        reserve_left: None,
    });
    state.runs.insert(id.clone(), run);
    state.attached = Some(id.clone());
    state.mode = Some(AppMode::Working { run: id });
    state.status = Some(status_line_with(vec![chip(
        "Claude",
        Some(reading("claude-code", "five_hour", 0.38, 0)),
    )]));
    state
}

fn sync(model: &mut ViewModel, state: &AppState, ui: &UiSettings, seconds: f64) -> Effects {
    let mut out = Effects::default();
    let mut cx = Cx {
        model,
        state,
        settings: ui,
        now: at(seconds),
        out: &mut out,
    };
    status::sync(&mut cx);
    out
}

fn synced(state: &AppState, ui: &UiSettings, seconds: f64) -> ViewModel {
    let mut model = ViewModel::default();
    sync(&mut model, state, ui, seconds);
    model
}

fn assert_stable(render: impl Fn(u16) -> Vec<Line<'static>>) {
    for width in stability::WIDTHS {
        stability::assert_lines(&render(width), width);
    }
}

fn labeled(rows: &[(&str, Line<'static>)]) -> String {
    rows.iter()
        .map(|(label, line)| format!("{label}:\n{}", styled(std::slice::from_ref(line))))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn palette_roles_dark_and_light() {
    let env = ColorEnv {
        no_color: false,
        colorterm: Some("truecolor".into()),
        term: Some("xterm-256color".into()),
        colorfgbg: None,
    };
    let pick = |choice| UiSettings {
        theme: choice,
        ..UiSettings::default()
    };
    let dark = theme_for(&pick(ThemeChoice::Dark), &env, Some(true));
    let light = theme_for(&pick(ThemeChoice::Light), &env, Some(false));
    let lines: Vec<Line<'static>> = Role::ALL
        .iter()
        .map(|role| {
            Line::from(vec![
                Span::raw(format!("{:<18}", role.name())),
                Span::styled("dark", dark.style(*role)),
                Span::raw(" "),
                Span::styled("light", light.style(*role)),
            ])
        })
        .collect();
    assert_stable(|_| lines.clone());
    insta::assert_snapshot!("palette_roles_dark_and_light", styled(&lines));

    // The palette follows the terminal's own background when the theme is auto.
    let auto = UiSettings::default();
    assert_eq!(theme_for(&auto, &env, Some(true)), light);
    assert_eq!(theme_for(&auto, &env, Some(false)), dark);
}

#[test]
fn budget_meter_context_size() {
    let mut state = crew();
    let run = state.runs.get_mut(&run_id(1)).unwrap();
    let contexts = [(1, 150_000), (2, 90_000), (3, 40_000)];
    for (n, used) in contexts {
        let session = run.sessions.get_mut(&SessionKey::numbered(n)).unwrap();
        session.context = Some(ContextUse { used, cap: 200_000 });
    }
    let ui = UiSettings::default();
    let model = synced(&state, &ui, NOW);
    let theme = theme();
    let render = |width| {
        status_line(&model.bottom.status_line, &ctx(&theme, width, NOW))
            .into_iter()
            .collect::<Vec<_>>()
    };
    assert_stable(render);
    for width in [80, 120] {
        insta::assert_snapshot!(
            format!("budget_meter_context_size_{width}"),
            styled(&render(width))
        );
    }
    let row = text(&render(120));
    assert!(row.contains("$0.21/$5.00 · ctx 150k/200k"), "{row}");

    // A session near its cap turns the meter the warning color.
    let run = state.runs.get_mut(&run_id(1)).unwrap();
    let session = run.sessions.get_mut(&SessionKey::numbered(2)).unwrap();
    session.context = Some(ContextUse {
        used: 185_000,
        cap: 200_000,
    });
    let model = synced(&state, &ui, NOW);
    let line = status_line(&model.bottom.status_line, &ctx(&theme, 120, NOW)).unwrap();
    assert_eq!(
        style_of(&[line], "ctx 185k/200k").and_then(|style| style.fg),
        fg(Role::Warning)
    );
}

#[test]
fn working_line_states() {
    let ui = UiSettings::default();
    let theme = theme();

    // A chat turn: thinking, then a tool, then text between thinking spells.
    let mut chat = AppState::default();
    chat.mode = Some(AppMode::Answering {
        turn: TurnId::numbered(1),
    });
    chat.chat.turn = Some(TurnId::numbered(1));
    chat.chat.tokens_out = 3_600;
    chat.chat.doing = Doing::Thinking;
    let mut thinking = synced(&chat, &ui, 0.0);
    let mut tool_state = chat.clone();
    tool_state.chat.doing = Doing::RunningTool;
    let mut tool = thinking.clone_model();
    sync(&mut tool, &tool_state, &ui, 9.0);
    let mut thought_state = chat.clone();
    thought_state.chat.doing = Doing::Thinking;
    sync(&mut thinking, &thought_state, &ui, 1.0);
    thought_state.chat.doing = Doing::Writing;
    let mut thought = thinking.clone_model();
    sync(&mut thought, &thought_state, &ui, 3.0);

    let crew_state = crew();
    let crew = synced(&crew_state, &ui, NOW);
    let stall = |state: &mut AppState, sessions: usize| {
        let run = state.runs.get_mut(&run_id(1)).unwrap();
        for session in run.sessions.values_mut().take(sessions) {
            session.stalled = true;
        }
    };
    let mut stalled_state = crew_state.clone();
    stall(&mut stalled_state, 3);
    let stalled = synced(&stalled_state, &ui, NOW - 40.0);
    let mut pausing_state = crew_state.clone();
    pausing_state.mode = Some(AppMode::Pausing { run: run_id(1) });
    let pausing = synced(&pausing_state, &ui, NOW);

    let line = |model: &ViewModel, seconds: f64, width: u16| {
        model
            .bottom
            .working
            .as_ref()
            .expect("a working line")
            .line(&ctx(&theme, width, seconds))
    };
    let rows = |width| {
        vec![
            ("chat thinking", line(&thinking, 12.0, width)),
            ("chat running a tool", line(&tool, 12.0, width)),
            ("chat between thinking spells", line(&thought, 4.0, width)),
            ("crew run", line(&crew, NOW, width)),
            ("crew run stalled for 40s", line(&stalled, NOW, width)),
            ("pausing", line(&pausing, NOW, width)),
        ]
    };
    assert_stable(|width| rows(width).into_iter().map(|(_, line)| line).collect());
    for width in [80, 120] {
        insta::assert_snapshot!(
            format!("working_line_states_{width}"),
            labeled(&rows(width))
        );
    }
    let crew_text = text(&[line(&crew, NOW, 120)]);
    assert!(
        crew_text.ends_with("(4m 12s · ↓ 48.2k tokens · Build · 2/5 cards · 3 agents · $0.21)"),
        "{crew_text}"
    );
    assert_eq!(
        text(&[line(&pausing, NOW, 120)])
            .split_once(' ')
            .map(|(_, rest)| rest),
        Some("Pausing… (esc again to stop now)")
    );
    let glyph = line(&stalled, NOW, 120).spans[0].style.fg;
    assert_eq!(glyph, fg(Role::Warning));
    // The chat line steps aside while its answer streams, and the crew line stays.
    assert!(!thought.bottom.working.as_ref().unwrap().shows(true));
    assert!(crew.bottom.working.as_ref().unwrap().shows(true));

    // Syncing again keeps the clock and the spell's start.
    let mut again = crew.clone_model();
    sync(&mut again, &crew_state, &ui, NOW + 5.0);
    assert_eq!(
        again.bottom.working.as_ref().map(|line| line.started_at),
        crew.bottom.working.as_ref().map(|line| line.started_at)
    );
    let stalled_since = |model: &ViewModel| {
        model
            .bottom
            .working
            .as_ref()
            .and_then(|line| line.stalled_since)
    };
    assert_eq!(stalled_since(&again), None, "quiet alone is no stall");
    // A stall keeps fading from when it was reported.
    let mut later = stalled.clone_model();
    sync(&mut later, &stalled_state, &ui, NOW);
    assert_eq!(stalled_since(&later), Some(at(NOW - 40.0)));
    // While one of the crew's agents still works, the run doesn't stall.
    let mut one_stalled = crew_state.clone();
    stall(&mut one_stalled, 2);
    assert_eq!(stalled_since(&synced(&one_stalled, &ui, NOW)), None);
}

#[test]
fn mode_line_states() {
    let theme = theme();
    let ui = UiSettings::default();
    let vim = UiSettings {
        keys: KeyScheme::Vim,
        ..UiSettings::default()
    };

    let idle = {
        let mut state = AppState::default();
        state.mode = Some(AppMode::Idle);
        state.status = Some(status_line_with(Vec::new()));
        synced(&state, &ui, NOW)
    };

    let mut shadow_state = crew();
    if let Some(status) = shadow_state.status.as_mut() {
        status.approval_mode = ApprovalMode::Shadow;
    }
    let insert = synced(&shadow_state, &vim, NOW);

    // A card waits in another run, and the screen's own vim state survives a sync.
    let mut auto_state = shadow_state.clone();
    if let Some(status) = auto_state.status.as_mut() {
        status.approval_mode = ApprovalMode::Auto;
    }
    let other = run_id(2);
    let mut waiting = RunState::new(other.clone());
    waiting.checkpoints.push(OpenCheckpoint {
        opened: Box::new(question()),
        set_aside: false,
        replayed: false,
    });
    auto_state.runs.insert(other.clone(), waiting);
    let mut normal = insert.clone_model();
    normal.bottom.mode.vim = Some(VimMode::Normal);
    sync(&mut normal, &auto_state, &vim, NOW);

    let row = |model: &ViewModel, width: u16| {
        let view = model.bottom.render(&ctx(&theme, width, NOW));
        view.lines
            .into_iter()
            .find(|line| text(std::slice::from_ref(line)).contains("mode on"))
            .expect("a mode line")
    };
    let rows = |width| {
        vec![
            ("idle", row(&idle, width)),
            ("vim insert, shadow, agents running", row(&insert, width)),
            ("vim normal, auto", row(&normal, width)),
        ]
    };
    assert_stable(|width| rows(width).into_iter().map(|(_, line)| line).collect());
    for width in [80, 120] {
        insta::assert_snapshot!(format!("mode_line_states_{width}"), labeled(&rows(width)));
    }
    assert_eq!(normal.bottom.mode.vim, Some(VimMode::Normal));
    let idle_row = text(&[row(&idle, 120)]);
    assert!(
        idle_row.starts_with("  ǁ supervised mode on (shift+tab to cycle) · ? for shortcuts  "),
        "{idle_row:?}"
    );
    assert!(idle_row.ends_with("  Opus on Claude Code"), "{idle_row:?}");
    assert!(text(&[row(&insert, 120)]).contains("-- INSERT -- ǁ shadow mode on"));
    assert!(text(&[row(&insert, 120)]).contains("← 3 agents"));
    let normal_row = row(&normal, 120);
    assert!(text(std::slice::from_ref(&normal_row)).starts_with("  -- NORMAL -- ⏵⏵ auto mode on"));
    assert_eq!(
        style_of(&[normal_row], "1 needs you").and_then(|style| style.fg),
        fg(Role::Warning)
    );
}

#[test]
fn the_status_line_shows_only_what_the_header_lacks() {
    let mut state = AppState::default();
    state.mode = Some(AppMode::Idle);
    state.apply(&AppEvent::Welcome {
        version: "0.0.1".into(),
        repo: "~/argonauts".into(),
        preset: "peer-crew".into(),
        agents: Vec::new(),
        runs_going: 0,
        runs_need_you: 0,
        example_prompt: None,
        default_agent: None,
        effort: None,
    });
    let theme = theme();
    let ui = UiSettings::default();
    let row = |state: &AppState| {
        let model = synced(state, &ui, NOW);
        status_line(&model.bottom.status_line, &ctx(&theme, 120, NOW)).map(|line| text(&[line]))
    };

    // The header already names the agent and the preset, so with no quotas there's no row.
    let status = status_line_with(Vec::new());
    state.apply(&AppEvent::Status {
        status: status.clone(),
    });
    assert_eq!(row(&state), None);

    let quotas = vec![chip(
        "Claude",
        Some(reading("claude-code", "five_hour", 0.57, 0)),
    )];
    state.apply(&AppEvent::Status {
        status: status_line_with(quotas.clone()),
    });
    assert_eq!(row(&state).as_deref(), Some("  Claude 5h 57% used"));

    // A default agent that differs from the header's comes back. The preset never does, since
    // the input's top rule names it.
    let mut changed = status_line_with(quotas);
    changed.default_agent = worker("codex", "gpt-5.6");
    changed.preset = "solo".into();
    state.apply(&AppEvent::Status { status: changed });
    assert_eq!(
        row(&state).as_deref(),
        Some("  GPT 5.6 on Codex · Claude 5h 57% used")
    );

    // Without a header, as when nothing welcomed us, the status line names the agent.
    state.welcome = None;
    state.apply(&AppEvent::Status { status });
    assert_eq!(row(&state).as_deref(), Some("  Opus on Claude Code"));
}

#[test]
fn status_line_chips() {
    let mut state = AppState::default();
    state.mode = Some(AppMode::Idle);
    let out_until = QuotaReading {
        status: QuotaStatus::Rejected,
        resets_at: Some(datetime!(2030-01-01 15:45 UTC)),
        ..reading("antigravity-cli", "daily", 1.0, 0)
    };
    let quotas = vec![
        chip("Claude", Some(reading("claude-code", "five_hour", 0.71, 0))),
        chip("Codex", Some(reading("codex", "five_hour", 0.92, 0))),
        chip("agy", Some(out_until)),
        chip("OpenCode", None),
        QuotaChip {
            health: HealthState::Down,
            ..chip("OpenRouter", None)
        },
        chip("Local", None),
    ];
    let mut status = status_line_with(quotas);
    status.spent = Some(Spend {
        cost_usd: 0.46,
        ..Spend::default()
    });
    state.status = Some(status);
    state.narrator_status = Some(NarratorStatus {
        mode: NarratorMode::Live,
        model: worker("claude-code", "haiku"),
        spent: Spend {
            cost_usd: 0.06,
            ..Spend::default()
        },
        cap: "0.50 usd".into(),
        capped: false,
        interval_s: 45,
    });
    let model = synced(&state, &UiSettings::default(), NOW);
    let theme = theme();
    let render = |width| {
        status_line(&model.bottom.status_line, &ctx(&theme, width, NOW))
            .into_iter()
            .collect::<Vec<_>>()
    };
    assert_stable(render);
    for width in [40, 80, 120, 200] {
        insta::assert_snapshot!(format!("status_line_chips_{width}"), styled(&render(width)));
    }
    let wide = text(&render(200));
    assert_eq!(
        wide,
        "  Opus on Claude Code · Claude 5h 71% used · Codex 5h 92% used · \
         agy out until Tue 3:45 PM · OpenCode · OpenRouter down · Local · $0.46 · \
         narrator $0.06/$0.50"
    );
    // A narrow screen keeps the chips that warn, and the model.
    let narrow = text(&render(80));
    assert!(narrow.contains("Codex 5h 92% used"), "{narrow}");
    assert!(narrow.contains("agy out until"), "{narrow}");
    assert!(!narrow.contains("Claude 5h"), "{narrow}");
}

#[test]
fn narrator_live_line() {
    let mut state = crew();
    state.narrator = Some(NarratorLine {
        ts: T0,
        run: Some(run_id(1)),
        kind: NarratorLineKind::Update,
        text: "Codex took over card-003 while Claude waits for its 5h reset; two cards are merged."
            .into(),
        model: worker("claude-code", "haiku"),
        spend: Spend::default(),
    });
    let live = UiSettings {
        narrator: NarratorMode::Live,
        ..UiSettings::default()
    };
    let model = synced(&state, &live, NOW);
    let theme = theme();
    let render = |width| model.bottom.render(&ctx(&theme, width, NOW)).lines;
    assert_stable(render);
    for width in [80, 120] {
        let lines = render(width);
        let narrator: Vec<Line<'static>> = lines
            .iter()
            .take_while(|line| !text(std::slice::from_ref(*line)).contains("(4m 12s"))
            .cloned()
            .collect();
        insta::assert_snapshot!(
            format!("narrator_live_line_{width}"),
            format!("{}\n---\n{}", text(&lines), styled(&narrator))
        );
    }
    let lines = render(120);
    let all = text(&lines);
    let narrator_row = all.find("※ narrator:").expect("the narrator's line");
    let working_row = all.find(" (4m 12s").expect("the working line");
    assert!(narrator_row < working_row, "{all}");

    // Only in live mode.
    let quiet = synced(&state, &UiSettings::default(), NOW);
    assert_eq!(quiet.bottom.narrator, None);
}

fn question() -> CheckpointOpened {
    let card = CardId::new("card-001").unwrap();
    CheckpointOpened {
        id: CheckpointId::numbered(1),
        kind: CheckpointKind::Question,
        title: "Which cache?".into(),
        card: Some(card.clone()),
        packet: CheckpointPacket::Question(QuestionPacket {
            from: worker("codex", "gpt-5.6"),
            card,
            text: "Which cache?".into(),
            options: Vec::new(),
        }),
        evidence_sha256: String::new(),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        options: Vec::new(),
        free_text: FreeText::Answer,
        needs_you: true,
    }
}

#[test]
fn the_working_line_steps_aside_while_the_run_waits_on_you() {
    let ui = UiSettings::default();
    let mut state = crew();
    let open = |state: &mut AppState, needs_you: bool, set_aside: bool| {
        let run = state.runs.get_mut(&run_id(1)).unwrap();
        run.checkpoints = vec![OpenCheckpoint {
            opened: Box::new(CheckpointOpened {
                needs_you,
                ..question()
            }),
            set_aside,
            replayed: false,
        }];
    };
    let working = |state: &AppState| synced(state, &ui, NOW).bottom.working.is_some();
    let card_state = |state: &mut AppState, card: &str, to: CardState| {
        let run = state.runs.get_mut(&run_id(1)).unwrap();
        let card = CardId::new(card).unwrap();
        run.cards.get_mut(&card).unwrap().state = Some(to);
    };

    // One agent's question while the others work: the run goes on.
    open(&mut state, true, false);
    assert!(working(&state));

    // With every agent done and every card held up, nothing goes on but the wait, set aside or
    // not.
    let run = state.runs.get_mut(&run_id(1)).unwrap();
    for session in run.sessions.values_mut() {
        session.status = argonauts_tui::state::SessionStatus::Done;
    }
    card_state(&mut state, "card-003", CardState::Blocked);
    card_state(&mut state, "card-004", CardState::Integrated);
    card_state(&mut state, "card-005", CardState::Waiting);
    assert!(!working(&state));
    open(&mut state, true, true);
    assert!(!working(&state));

    // Gates running for another card are work too.
    card_state(&mut state, "card-004", CardState::Gating);
    assert!(working(&state));
    card_state(&mut state, "card-004", CardState::Integrated);

    // While the policy still decides, something is at work.
    open(&mut state, false, false);
    assert!(working(&state));

    // Answered, the run goes on.
    state.runs.get_mut(&run_id(1)).unwrap().checkpoints.clear();
    assert!(working(&state));
}

/// `ViewModel` isn't `Clone`, since history holds cells; these tests only need the bottom.
trait CloneModel {
    fn clone_model(&self) -> ViewModel;
}

impl CloneModel for ViewModel {
    fn clone_model(&self) -> ViewModel {
        ViewModel {
            title: self.title.clone(),
            bottom: self.bottom.clone(),
            verbs: self.verbs,
            ..ViewModel::default()
        }
    }
}

#[test]
fn sync_fills_the_rest_and_keeps_what_the_screen_changed() {
    use argonauts_core::events::run::Party;
    use argonauts_core::events::session::{Delivery, QueuedMessage};
    use argonauts_core::input::MentionTarget;
    use argonauts_core::names::MessageId;
    use argonauts_tui::model::ApprovalMode as ScreenMode;

    let ui = UiSettings::default();
    let mut state = crew();
    let run = state.runs.get_mut(&run_id(1)).unwrap();
    run.queued.push(QueuedMessage {
        id: MessageId::numbered(1),
        from: Party::User,
        to: MentionTarget::Number(3),
        text: "check the retry path".into(),
        queued_at: T0,
        delivery: Delivery::NextTurn,
    });
    state.chat.queued.push("and the docs".into());
    let mut model = synced(&state, &ui, NOW);
    let bottom = &model.bottom;
    assert_eq!(model.title, "Fix the export race");
    assert_eq!(bottom.composer.label.as_deref(), Some("peer-crew"));
    assert_eq!(bottom.queued, vec!["check the retry path", "and the docs"]);
    assert_eq!(
        bottom.queued_where,
        vec![
            Some("to Gemini 3.6 on agy, at its next turn".to_owned()),
            None
        ]
    );
    assert_eq!(
        bottom.enter_sends.as_deref(),
        Some("Enter sends guidance to the run")
    );
    assert_eq!(bottom.exit_note.as_deref(), Some("the run keeps going"));
    assert_eq!(bottom.mode.background_agents, 3);

    // Shift+Tab changed the mode on screen; syncing again keeps it until the controller moves.
    model.bottom.mode.approval = ScreenMode::Shadow;
    sync(&mut model, &state, &ui, NOW + 1.0);
    assert_eq!(model.bottom.mode.approval, ScreenMode::Shadow);
    if let Some(status) = state.status.as_mut() {
        status.approval_mode = ApprovalMode::Auto;
    }
    sync(&mut model, &state, &ui, NOW + 2.0);
    assert_eq!(model.bottom.mode.approval, ScreenMode::Auto);

    // The input keeps your preset whatever the run follows, and a finished run leaves nothing
    // working.
    let run = state.runs.get_mut(&run_id(1)).unwrap();
    run.mode = Some(RunMode::Solo);
    run.finished = Some(argonauts_core::events::run::RunOutcome::Merged);
    sync(&mut model, &state, &ui, NOW + 3.0);
    assert_eq!(model.bottom.composer.label.as_deref(), Some("peer-crew"));
    assert!(model.bottom.working.is_none());
    assert_eq!(model.bottom.exit_note, None);
}

/// A direct turn's CLI waits on your answer to what it asked, and its stream says nothing
/// meanwhile: the working line steps aside rather than fade as stalled, and comes back once
/// you answer.
#[test]
fn the_working_line_steps_aside_while_the_cli_asks_you() {
    use argonauts_core::app::{AppChoice, ChatEvent, ChoiceKind};
    use argonauts_core::events::checkpoint::FreeText;
    use argonauts_core::names::ChoiceId;

    let ui = UiSettings::default();
    let mut state = AppState::default();
    let turn = TurnId::numbered(1);
    state.apply(&AppEvent::Mode {
        mode: AppMode::Answering { turn: turn.clone() },
    });
    state.apply(&AppEvent::Chat {
        event: ChatEvent::TurnStarted {
            turn,
            text: "fix the rounding".into(),
            worker: worker("claude-code", "haiku"),
            read_only: false,
            commit: String::new(),
            uncommitted: Vec::new(),
            settings_sha256: None,
            continues: false,
        },
    });
    let working = |state: &AppState| synced(state, &ui, NOW).bottom.working.is_some();
    assert!(working(&state));

    let id = ChoiceId::numbered(1);
    state.apply(&AppEvent::Choice {
        choice: AppChoice {
            id: id.clone(),
            kind: ChoiceKind::Permission,
            title: "Make this edit to invoice/money.py?".into(),
            body: vec!["- sign = 1".into(), "+ negative = True".into()],
            options: Vec::new(),
            free_text: FreeText::None,
            bell: true,
        },
    });
    assert!(!working(&state));

    state.apply(&AppEvent::ChoiceClosed {
        id,
        outcome: "Yes".into(),
    });
    assert!(working(&state));
}
