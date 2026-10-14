use argonauts_core::app::{PublishedArtifact, QuotaChip};
use argonauts_core::limits::QuotaSource;
use argonauts_core::names::VendorId;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::bottom_line::BottomLineSettings;
use time::macros::datetime;

use super::*;

const T0: OffsetDateTime = datetime!(2026-09-30 10:00 UTC);

fn clock() -> Clock {
    Clock {
        now: T0 + time::Duration::minutes(1),
        utc_offset: UtcOffset::UTC,
    }
}

fn reading(vendor: &str, window: &str, used: f64) -> QuotaReading {
    QuotaReading {
        vendor: VendorId(vendor.to_owned()),
        status: QuotaStatus::Allowed,
        used: Some(used),
        resets_at: None,
        window: Some(window.to_owned()),
        source: QuotaSource::RateLimitEvent,
        observed_at: T0,
        detail: None,
        wrap_up: false,
    }
}

fn cli(label: &str, vendor: &str, readings: Vec<QuotaReading>) -> CrewCli {
    CrewCli {
        label: label.to_owned(),
        vendor: Some(vendor.to_owned()),
        health: HealthState::Healthy,
        readings,
    }
}

fn claude() -> CrewCli {
    cli(
        "Claude",
        "claude-code",
        vec![
            reading("claude-code", "seven_day", 0.56),
            reading("claude-code", "five_hour", 0.01),
        ],
    )
}

/// A window as a test reads it: its label and the percent used.
type Shown = (String, u8);

fn shown(chip: &CliUse) -> Vec<Shown> {
    chip.windows
        .iter()
        .map(|window| (window.label.clone(), (window.used * 100.0).round() as u8))
        .collect()
}

/// The CLI in use shows both its windows, the shorter first, and the others
/// their fuller one.
#[test]
fn the_cli_in_use_shows_both_windows_and_the_others_their_fuller_one() {
    let set = PlanUsageLine::default();
    let main = cli_use(&claude(), true, &set, None, &clock());
    assert!(main.main);
    assert_eq!(main.color, Role::CliClaude);
    assert_eq!(shown(&main), [("5h".to_owned(), 1), ("7d".to_owned(), 56)]);
    assert!(main.windows.iter().all(|window| window.resets.is_none()));

    let other = cli_use(&claude(), false, &set, None, &clock());
    assert_eq!(shown(&other), [("7d".to_owned(), 56)]);

    let pick = |windows| PlanUsageLine { windows, ..set };
    let fullest = cli_use(
        &claude(),
        true,
        &pick(PlanUsageWindows::Fullest),
        None,
        &clock(),
    );
    assert_eq!(shown(&fullest), [("7d".to_owned(), 56)]);
    let five = cli_use(
        &claude(),
        false,
        &pick(PlanUsageWindows::FiveHours),
        None,
        &clock(),
    );
    assert_eq!(shown(&five), [("5h".to_owned(), 1)]);
    let seven = cli_use(
        &claude(),
        true,
        &pick(PlanUsageWindows::SevenDays),
        None,
        &clock(),
    );
    assert_eq!(shown(&seven), [("7d".to_owned(), 56)]);
}

/// A reset time shows only from 90%, as Claude Code writes one, and a window near its limit
/// shows whatever the settings pick.
#[test]
fn a_window_near_its_limit_shows_its_reset_time_and_is_never_hidden() {
    let mut near = reading("claude-code", "five_hour", 0.93);
    near.resets_at = Some(datetime!(2026-09-30 16:40 UTC));
    let mut week = reading("claude-code", "seven_day", 0.56);
    week.resets_at = Some(datetime!(2026-10-06 15:45 UTC));
    let claude = cli("Claude", "claude-code", vec![week, near]);
    let set = PlanUsageLine::default();

    let chip = cli_use(&claude, true, &set, None, &clock());
    assert!(chip.needs_you());
    assert_eq!(chip.windows[0].resets.as_deref(), Some("4:40pm"));
    assert_eq!(chip.windows[1].resets, None);

    // Not in use, and set to show only the seven days: the window at 93% still shows.
    let seven = PlanUsageLine {
        windows: PlanUsageWindows::SevenDays,
        ..set
    };
    let chip = cli_use(&claude, false, &seven, None, &clock());
    assert_eq!(shown(&chip), [("5h".to_owned(), 93), ("7d".to_owned(), 56)]);

    let always = PlanUsageLine {
        resets: PlanUsageResets::Always,
        ..set
    };
    let chip = cli_use(&claude, true, &always, None, &clock());
    assert_eq!(chip.windows[1].resets.as_deref(), Some("Tue 3:45pm"));
    let never = PlanUsageLine {
        resets: PlanUsageResets::Never,
        ..set
    };
    let chip = cli_use(&claude, true, &never, None, &clock());
    assert_eq!(chip.windows[0].resets, None);
}

#[test]
fn a_cli_that_is_out_down_or_wrapping_up_says_so() {
    let set = PlanUsageLine::default();
    let mut out = reading("antigravity-cli", "daily", 1.0);
    out.status = QuotaStatus::Rejected;
    out.resets_at = Some(datetime!(2026-10-06 15:45 UTC));
    let agy = |reading: QuotaReading| cli("agy", "antigravity-cli", vec![reading]);
    let chip = cli_use(&agy(out.clone()), false, &set, None, &clock());
    assert_eq!(
        chip.state,
        CliState::Out {
            until: Some("Tue 3:45pm".into())
        }
    );
    assert_eq!(chip.color, Role::CliAgy);

    let today = QuotaReading {
        resets_at: Some(datetime!(2026-09-30 15:45 UTC)),
        ..out.clone()
    };
    let chip = cli_use(&agy(today), false, &set, None, &clock());
    assert_eq!(
        chip.state,
        CliState::Out {
            until: Some("3:45pm".into())
        }
    );
    let no_reset = QuotaReading {
        resets_at: None,
        ..out.clone()
    };
    let chip = cli_use(&agy(no_reset), false, &set, None, &clock());
    assert_eq!(chip.state, CliState::Out { until: None });

    // On the wrap-up allowance the agent finishes, so the chip says so rather than out.
    let wrapping_up = QuotaReading {
        wrap_up: true,
        ..out
    };
    let chip = cli_use(&agy(wrapping_up), false, &set, None, &clock());
    assert_eq!(chip.state, CliState::WrappingUp);

    let mut codex = cli("Codex", "codex", vec![reading("codex", "five_hour", 0.38)]);
    codex.health = HealthState::Down;
    let chip = cli_use(&codex, false, &set, None, &clock());
    assert_eq!(chip.state, CliState::Down);
    assert!(chip.needs_you());

    let warned = QuotaReading {
        status: QuotaStatus::Warning,
        used: None,
        ..reading("codex", "five_hour", 0.0)
    };
    let chip = cli_use(
        &cli("Codex", "codex", vec![warned]),
        false,
        &set,
        None,
        &clock(),
    );
    assert_eq!(chip.state, CliState::NearLimit);
}

/// A reading whose window has reset is dropped, and one that couldn't be read
/// again says how old it is.
#[test]
fn a_reading_is_dropped_once_its_window_resets_and_says_its_age_before() {
    let set = PlanUsageLine::default();
    let mut five = reading("antigravity-cli", "five_hour", 0.80);
    five.resets_at = Some(T0 + time::Duration::hours(2));
    let week = reading("antigravity-cli", "seven_day", 0.30);
    let agy = cli("agy", "antigravity-cli", vec![five, week]);

    let fresh = cli_use(&agy, false, &set, None, &clock());
    assert_eq!(shown(&fresh), [("5h".to_owned(), 80)]);
    assert_eq!(fresh.windows[0].age, None);

    // Two days on, the five-hour window has long reset, so only the week's reading is left,
    // and it says how old it is.
    let later = Clock {
        now: T0 + time::Duration::days(2) + time::Duration::hours(1),
        ..clock()
    };
    let stale = cli_use(&agy, false, &set, None, &later);
    assert_eq!(shown(&stale), [("7d".to_owned(), 30)]);
    assert_eq!(stale.windows[0].age.as_deref(), Some("2d"));
    let hours = Clock {
        now: T0 + time::Duration::hours(3),
        ..clock()
    };
    let aged = cli_use(
        &cli(
            "agy",
            "antigravity-cli",
            vec![reading("antigravity-cli", "seven_day", 0.3)],
        ),
        false,
        &set,
        None,
        &hours,
    );
    assert_eq!(aged.windows[0].age.as_deref(), Some("3h"));

    // Out until a reset that has passed is no longer out.
    let mut out = reading("codex", "five_hour", 1.0);
    out.status = QuotaStatus::Rejected;
    out.resets_at = Some(T0 + time::Duration::hours(1));
    let codex = cli("Codex", "codex", vec![out]);
    assert!(matches!(
        cli_use(&codex, false, &set, None, &clock()).state,
        CliState::Out { .. }
    ));
    assert_eq!(
        cli_use(&codex, false, &set, None, &later).state,
        CliState::Fine
    );
}

#[test]
fn a_cli_nothing_was_read_for_keeps_its_name_and_one_without_a_plan_its_spend() {
    let set = PlanUsageLine::default();
    let codex = cli_use(
        &cli("Codex", "codex", Vec::new()),
        false,
        &set,
        Some(0.4),
        &clock(),
    );
    assert_eq!((codex.windows.len(), codex.note.as_deref()), (0, None));
    assert_eq!(codex.state, CliState::Fine);

    let unreadable = QuotaReading {
        status: QuotaStatus::Unknown,
        used: None,
        ..reading("codex", "five_hour", 0.0)
    };
    let codex = cli_use(
        &cli("Codex", "codex", vec![unreadable]),
        false,
        &set,
        None,
        &clock(),
    );
    assert!(codex.windows.is_empty());

    let openrouter = cli("OpenRouter", "openrouter", Vec::new());
    let chip = cli_use(&openrouter, false, &set, Some(0.06), &clock());
    assert_eq!(chip.note.as_deref(), Some("$0.06"));
    assert_eq!(chip.color, Role::CatOther);
    let local = cli_use(
        &cli("Local", "ollama", Vec::new()),
        false,
        &set,
        None,
        &clock(),
    );
    assert_eq!(local.note, None);
}

fn status(main: &str, quotas: Vec<QuotaChip>) -> StatusLine {
    StatusLine {
        title: String::new(),
        repo: "~/code".into(),
        workflow: "solo".into(),
        ultracode: false,
        run: None,
        main_agent: main.parse().unwrap(),
        quotas,
        line: Default::default(),
        spent: None,
        limit: None,
        narrator: NarratorMode::Off,
        approval_mode: ApprovalMode::Auto,
        awake: None,
    }
}

fn chip(label: &str, windows: Vec<QuotaReading>) -> QuotaChip {
    QuotaChip {
        label: label.into(),
        reading: binding_reading(&windows).cloned(),
        health: HealthState::Healthy,
        windows,
    }
}

fn crew_status(main: &str) -> StatusLine {
    status(
        main,
        vec![
            chip(
                "Claude",
                vec![
                    reading("claude-code", "five_hour", 0.01),
                    reading("claude-code", "seven_day", 0.56),
                ],
            ),
            chip(
                "Codex",
                vec![
                    reading("codex", "five_hour", 0.01),
                    reading("codex", "seven_day", 0.11),
                ],
            ),
            chip("agy", vec![reading("antigravity-cli", "five_hour", 0.02)]),
        ],
    )
}

fn built(state: &AppState) -> BottomLine {
    build(state, &UiSettings::default(), &BTreeMap::new(), clock().now)
}

fn chips(line: &BottomLine) -> Vec<(String, bool, Vec<Shown>)> {
    line.clis()
        .map(|cli| (cli.name.clone(), cli.main, shown(cli)))
        .collect()
}

/// The mark and the second window follow the main session from Claude Code to Codex, and the
/// crew keeps its order.
#[test]
fn the_mark_and_the_second_window_follow_the_main_session() {
    let window = |label: &str, used| (label.to_owned(), used);
    let mut state = AppState::default();
    state.status = Some(crew_status("claude-code/claude-opus-5-5"));
    let line = built(&state);
    assert_eq!(line.left, [LinePart::Mode]);
    assert_eq!(
        chips(&line),
        [
            (
                "Claude".to_owned(),
                true,
                vec![window("5h", 1), window("7d", 56)]
            ),
            ("Codex".to_owned(), false, vec![window("7d", 11)]),
            ("agy".to_owned(), false, vec![window("5h", 2)]),
        ]
    );

    state.status = Some(crew_status("codex/gpt-6.1-sol"));
    assert_eq!(
        chips(&built(&state)),
        [
            ("Claude".to_owned(), false, vec![window("7d", 56)]),
            (
                "Codex".to_owned(),
                true,
                vec![window("5h", 1), window("7d", 11)]
            ),
            ("agy".to_owned(), false, vec![window("5h", 2)]),
        ]
    );
}

/// Gauges are one cell a window, so every CLI shows both of its own, the mark still on one.
#[test]
fn gauges_show_both_windows_of_every_cli() {
    let mut state = AppState::default();
    let mut status = crew_status("claude-code");
    status.line.settings = BottomLineSettings::from_toml("form = \"gauges\"").unwrap();
    state.status = Some(status);
    let windows: Vec<(String, bool, usize)> = built(&state)
        .clis()
        .map(|cli| (cli.name.clone(), cli.main, cli.windows.len()))
        .collect();
    assert_eq!(
        windows,
        [
            ("Claude".to_owned(), true, 2),
            ("Codex".to_owned(), false, 2),
            ("agy".to_owned(), false, 1),
        ]
    );
}

#[test]
fn the_main_cli_has_a_chip_even_outside_the_crew_and_newer_readings_win() {
    let mut state = AppState::default();
    let mut status = crew_status("opencode");
    status.quotas.remove(0);
    state.status = Some(status);
    // A run's event read Codex's week again, and found a CLI the controller didn't list.
    let mut newer = reading("codex", "seven_day", 0.42);
    newer.observed_at = T0 + time::Duration::seconds(30);
    state.quotas.insert("codex".into(), vec![newer]);
    state.quotas.insert(
        "claude-code".into(),
        vec![reading("claude-code", "five_hour", 0.2)],
    );
    let names: Vec<_> = chips(&built(&state))
        .into_iter()
        .map(|(name, main, windows)| (name, main, windows.last().map(|(_, used)| *used)))
        .collect();
    assert_eq!(
        names,
        [
            ("OpenCode".to_owned(), true, None),
            ("Codex".to_owned(), false, Some(42)),
            ("agy".to_owned(), false, Some(2)),
            ("Claude".to_owned(), false, Some(20)),
        ]
    );
}

#[test]
fn the_settings_pick_the_parts_their_sides_and_who_shows() {
    let mut state = AppState::default();
    let mut status = crew_status("claude-code");
    status.line.settings = BottomLineSettings::from_toml(
        "form = \"compact\"\nleft = [\"mode\", \"artifact\", \"branch\"]\n\
         right = [\"spend\", \"plan-usage\", \"awake\"]\n\
         [plan-usage]\nshow = \"main\"\nas = \"left\"\n[narrow]\ndrop = [\"branch\"]\n",
    )
    .unwrap();
    status.line.branch = Some("main".into());
    status.line.spend_days = vec![0.0, 0.2, 0.3, 0.05, 0.5, 0.8, 0.01];
    status.line.artifact = Some(PublishedArtifact {
        name: "weekly-usage-chart".into(),
        others: 2,
    });
    state.status = Some(status);
    let mut slots = BTreeMap::new();
    slots.insert(
        "awake".to_owned(),
        PartBody::Words {
            text: "awake 2h".into(),
            tone: Tone::Dim,
        },
    );
    let line = build(&state, &UiSettings::default(), &slots, clock().now);
    assert_eq!(
        line.form,
        argonauts_core::settings::bottom_line::LineForm::Compact
    );
    assert!(line.as_left);
    assert_eq!(line.drop_first, ["branch"]);
    let names = |parts: &[LinePart]| -> Vec<String> {
        parts
            .iter()
            .map(|part| match part {
                LinePart::Mode => "mode".to_owned(),
                LinePart::PlanUsage(clis) => format!("plan-usage:{}", clis.len()),
                LinePart::Added(part) => part.name.clone(),
            })
            .collect()
    };
    assert_eq!(names(&line.left), ["mode", "artifact", "branch"]);
    assert_eq!(names(&line.right), ["spend", "plan-usage:1", "awake"]);
    let LinePart::Added(spend) = &line.right[0] else {
        panic!("the spend part");
    };
    assert_eq!(
        spend.body,
        PartBody::Spark {
            text: "~$1.86".into(),
            values: vec![0.0, 0.2, 0.3, 0.05, 0.5, 0.8, 0.01],
            role: Role::CliClaude,
        }
    );
    let LinePart::Added(artifact) = &line.left[1] else {
        panic!("the artifact part");
    };
    assert_eq!(
        artifact.body,
        PartBody::Artifact {
            name: "weekly-usage-chart".into(),
            others: 2,
        }
    );
}

/// The `run` mod on the line is the workflow under way: its name and how many of its tasks
/// are merged, from the plan's first task until the run ends. A cancelled task doesn't count.
#[test]
fn the_run_mod_draws_the_workflow_under_way() {
    use crate::state::{CardRow, RunState};
    use argonauts_core::events::card::CardState;
    use argonauts_core::events::run::{RunNamed, RunOutcome};
    use argonauts_core::settings::workflows::RunStyle;
    use argonauts_core::{CardId, RunId};

    let run_id = RunId::new("run-20260930-101500-3fa2").unwrap();
    let mut status = crew_status("claude-code");
    status.line.settings =
        BottomLineSettings::from_toml("right = [\"run\", \"plan-usage\"]").unwrap();
    let mut state = AppState::default();
    state.status = Some(status);
    state.attached = Some(run_id.clone());
    let mut run = RunState::new(run_id.clone());
    run.title = "add due dates to every task".into();
    state.runs.insert(run_id.clone(), run);
    let part = |state: &AppState| match built(state).right.first() {
        Some(LinePart::Added(part)) if part.name == RUN => Some(part.body.clone()),
        _ => None,
    };
    assert_eq!(part(&state), None, "no tasks to count yet");

    let states = [
        Some(CardState::Integrated),
        Some(CardState::Integrated),
        Some(CardState::Integrated),
        None,
        None,
        Some(CardState::Cancelled),
    ];
    let run = state.runs.get_mut(&run_id).unwrap();
    for (n, card_state) in (1..).zip(states) {
        let mut card = CardRow::new(CardId::numbered(n), "a task");
        card.state = card_state;
        run.cards.insert(card.id.clone(), card);
    }
    let meter = |label: &str| PartBody::Meter {
        label: label.into(),
        label_role: Role::BarFill,
        ratio: 0.6,
        value: "3/5".into(),
        fill: Role::BarFill,
    };
    assert_eq!(part(&state), Some(meter("add due dates to every task")));

    let run = state.runs.get_mut(&run_id).unwrap();
    run.named = Some(RunNamed {
        name: "add-due-dates".into(),
        description: "Add due dates to every task".into(),
        style: RunStyle::Workflow,
        based_on: None,
        done_when: Vec::new(),
    });
    assert_eq!(part(&state), Some(meter("add-due-dates")));

    // What other code drew for the name wins, and a finished run leaves the line.
    let mut slots = BTreeMap::new();
    let drawn = PartBody::Drawn(vec![ratatui::text::Span::raw("add-due-dates 3/5")]);
    slots.insert(RUN.to_owned(), drawn.clone());
    let line = build(&state, &UiSettings::default(), &slots, clock().now);
    assert_eq!(
        line.right.first(),
        Some(&LinePart::Added(AddedPart {
            name: RUN.to_owned(),
            body: drawn
        }))
    );
    state.runs.get_mut(&run_id).unwrap().finished = Some(RunOutcome::Merged);
    assert_eq!(part(&state), None);
}

/// The spend part is the last seven days, a run on screen or not. A run with a limit shows
/// its own spend against it, since that is the number that can stop it.
#[test]
fn the_spend_part_is_the_week_unless_the_run_has_a_limit() {
    use crate::state::RunState;
    use argonauts_core::RunId;

    let run_id = RunId::new("run-20260930-101500-3fa2").unwrap();
    let mut status = crew_status("claude-code");
    status.line.settings = BottomLineSettings::from_toml("right = [\"spend\"]").unwrap();
    status.line.spend_days = vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.42, 1.44];
    let mut state = AppState::default();
    state.status = Some(status);
    state.attached = Some(run_id.clone());
    let mut run = RunState::new(run_id.clone());
    run.spent.cost_usd = 0.21;
    state.runs.insert(run_id, run);
    let spend = |state: &AppState| match built(state).right.first() {
        Some(LinePart::Added(part)) => part.body.clone(),
        other => panic!("the spend part, not {other:?}"),
    };
    assert_eq!(
        spend(&state),
        PartBody::Spark {
            text: "~$1.86".into(),
            values: vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.42, 1.44],
            role: Role::CliClaude,
        }
    );

    state.status.as_mut().unwrap().limit = Some(BudgetAmount {
        unit: BudgetUnit::Usd,
        amount: 5.0,
    });
    assert_eq!(
        spend(&state),
        PartBody::Words {
            text: "$0.21/$5.00".into(),
            tone: Tone::Dim
        }
    );

    // With no day on record yet, the run's own spend stands in.
    let status = state.status.as_mut().unwrap();
    status.limit = None;
    status.line.spend_days.clear();
    assert_eq!(
        spend(&state),
        PartBody::Words {
            text: "$0.21".into(),
            tone: Tone::Dim
        }
    );
}

/// The artifact part is on the line as it ships, and draws only once a session published one.
#[test]
fn parts_with_nothing_to_show_draw_nothing() {
    let mut state = AppState::default();
    state.status = Some(crew_status("claude-code"));
    let line = built(&state);
    assert_eq!(line.left, [LinePart::Mode]);
    assert_eq!(line.right.len(), 1, "plan usage alone");
    let unset = built(&AppState::default());
    assert_eq!(
        (unset.left, unset.right),
        (vec![LinePart::Mode], Vec::new())
    );
}

/// A turn's own reading shows before the controller's first status names the crew.
#[test]
fn a_chat_turns_reading_shows_before_any_status() {
    let mut state = AppState::default();
    state.quotas.insert(
        "claude-code".into(),
        vec![reading("claude-code", "five_hour", 0.71)],
    );
    assert_eq!(
        chips(&built(&state)),
        [("Claude".to_owned(), false, vec![("5h".to_owned(), 71)])]
    );
}

/// A Claude Code mod's status text nobody placed still shows, last and first to go, unless
/// the settings hide mods' output.
#[test]
fn a_mods_status_text_sits_last_unless_you_place_it() {
    let mut state = AppState::default();
    state.status = Some(crew_status("claude-code"));
    state.chat.mod_status = vec![("weather".to_owned(), "sunny".to_owned())];
    let line = built(&state);
    let Some(LinePart::Added(last)) = line.right.last() else {
        panic!("the mod's text");
    };
    assert_eq!(*last, AddedPart::words("weather", "sunny", Tone::Dim));
    assert_eq!(line.drop_first.first().map(String::as_str), Some("weather"));

    let mut status = crew_status("claude-code");
    status.line.settings = BottomLineSettings::from_toml("left = [\"mode\", \"weather\"]").unwrap();
    state.status = Some(status);
    let line = built(&state);
    assert_eq!(line.left.len(), 2);
    assert_eq!(line.right.len(), 1, "placed once, on the left");

    let hidden = UiSettings {
        mod_output: ModOutput::Off,
        ..UiSettings::default()
    };
    let line = build(&state, &hidden, &BTreeMap::new(), clock().now);
    assert_eq!(line.left, [LinePart::Mode]);
}

/// A hold on the computer's sleep that you asked for shows while it lasts, before the chips,
/// even where the settings name no `awake` part. Named there, it sits where they put it.
#[test]
fn a_hold_on_sleep_shows_wherever_the_settings_leave_it_out() {
    let mut state = AppState::default();
    state.status = Some(crew_status("claude-code"));
    let held = PartBody::Words {
        text: "☕ awake until you quit".to_owned(),
        tone: Tone::Code,
    };
    let slots = BTreeMap::from([(AWAKE.to_owned(), held.clone())]);
    let with_hold = |state: &AppState| build(state, &UiSettings::default(), &slots, clock().now);
    let awake = LinePart::Added(AddedPart {
        name: AWAKE.to_owned(),
        body: held,
    });

    let line = with_hold(&state);
    assert_eq!(line.right.first(), Some(&awake));
    assert!(matches!(line.right.get(1), Some(LinePart::PlanUsage(_))));
    assert!(!line.left.contains(&awake));
    assert!(!built(&state).right.contains(&awake), "nothing holds it");

    let mut status = crew_status("claude-code");
    status.line.settings = BottomLineSettings::from_toml("left = [\"mode\", \"awake\"]").unwrap();
    state.status = Some(status);
    let line = with_hold(&state);
    assert_eq!(line.left, [LinePart::Mode, awake.clone()]);
    assert!(!line.right.contains(&awake), "placed once, on the left");
}

#[test]
fn reset_times_read_as_claude_code_writes_them() {
    let at = |time: OffsetDateTime| clock_time(time, &clock());
    assert_eq!(at(datetime!(2026-09-30 16:40 UTC)), "4:40pm");
    assert_eq!(at(datetime!(2026-09-30 00:05 UTC)), "12:05am");
    assert_eq!(at(datetime!(2026-09-30 12:00 UTC)), "12:00pm");
    assert_eq!(at(datetime!(2026-10-01 09:00 UTC)), "Thu 9:00am");
    let tokyo = Clock {
        utc_offset: UtcOffset::from_hms(9, 0, 0).unwrap(),
        ..clock()
    };
    assert_eq!(
        clock_time(datetime!(2026-09-30 16:40 UTC), &tokyo),
        "Thu 1:40am"
    );
}

/// A page the main agent publishes with Claude Code's Artifact tool shows on the line beside
/// the mode, named by the newest one with how many came before it.
#[test]
fn a_page_the_conversation_published_names_the_artifact_part() {
    use argonauts_core::AgentEvent;
    use argonauts_core::app::{AppEvent, ChatEvent};
    use argonauts_core::names::TurnId;

    let mut state = AppState::default();
    state.status = Some(crew_status("claude-code"));
    let said = |event| AppEvent::Chat {
        event: ChatEvent::Agent {
            turn: TurnId::numbered(1),
            event,
        },
    };
    state.apply(&AppEvent::Chat {
        event: ChatEvent::TurnStarted {
            turn: TurnId::numbered(1),
            text: "write it up".into(),
            worker: "claude-code/opus".parse().unwrap(),
            read_only: false,
            commit: "abc".into(),
            uncommitted: Vec::new(),
            settings_sha256: None,
            continues: false,
        },
    });
    for (id, file) in [("t1", "test-report"), ("t2", "weekly-usage-chart")] {
        state.apply(&said(AgentEvent::ToolStarted {
            id: id.into(),
            name: "Artifact".into(),
            input: serde_json::json!({"file_path": format!("out/{file}.html")}),
            parent: None,
        }));
        state.apply(&said(AgentEvent::ToolFinished {
            id: id.into(),
            ok: true,
            output: Some(format!(
                "Published out/{file}.html at https://claude.ai/code/artifact/{id} (Version 1)"
            )),
            parent: None,
        }));
    }
    let line = built(&state);
    let artifact = line.left.iter().find_map(|part| match part {
        LinePart::Added(added) if added.name == "artifact" => Some(added.body.clone()),
        _ => None,
    });
    assert_eq!(
        artifact,
        Some(PartBody::Artifact {
            name: "weekly-usage-chart".into(),
            others: 1,
        }),
        "{line:?}"
    );
}

/// A mod of yours from `mods/` that the line lists draws its chip there, with the theme.
#[test]
fn a_mod_of_yours_the_line_lists_is_its_chip() {
    use argonauts_core::settings::mods::view::{ModRow, ModRowKind, ModsView};
    use argonauts_core::settings::mods::{
        ModBody, ModOrigin, ModPlace, ModReading, builtin, check,
    };

    let (name, text) = builtin::STARTERS
        .iter()
        .find(|(starter, _)| *starter == "checks")
        .unwrap();
    let mut file = check::read(name, text).unwrap();
    file.places = vec![ModPlace::ModeLine];
    let reading = ModReading::sample(ModBody::Stack);
    let mut state = AppState::default();
    let mut status = crew_status("claude-code");
    status.line.settings = BottomLineSettings::from_toml("left = [\"mode\", \"checks\"]").unwrap();
    state.status = Some(status);
    state.mods = Some(ModsView {
        rows: vec![ModRow {
            name: (*name).to_owned(),
            origin: ModOrigin::Yours,
            path: format!("~/.argonauts/mods/{name}.toml"),
            kind: ModRowKind::Card {
                file: Box::new(file.clone()),
                built_in: None,
                reading: Some(reading.clone()),
                note: None,
                needs_trust: false,
                asks_for_line: false,
                hidden_by_line: false,
            },
        }],
        ..ModsView::default()
    });
    let line = built(&state);
    assert_eq!(
        line.left[1],
        LinePart::Added(AddedPart {
            name: "checks".into(),
            body: PartBody::Mod {
                file: Box::new(file),
                reading: Box::new(reading),
            },
        })
    );
}

/// The `stats` part says what the stats mod says from a hidden band, once the numbers carry
/// the crew's stats, and nothing before.
#[test]
fn the_stats_part_says_the_periods_tokens() {
    use crate::overview::sample;
    use std::sync::Arc;

    let mut status = crew_status("claude-code");
    status.line.settings = BottomLineSettings::from_toml("right = [\"stats\"]").unwrap();
    let mut state = AppState::default();
    state.status = Some(status);
    assert_eq!(built(&state).right, [], "no numbers yet");

    let mut usage = sample::usage();
    usage.stats = Some(sample::crew_stats());
    state.usage = Some(Arc::new(usage));
    state.names = sample::names();
    let Some(LinePart::Added(part)) = built(&state).right.first().cloned() else {
        panic!("the stats part");
    };
    assert_eq!(part.name, "stats");
    let PartBody::Words { text, tone } = part.body else {
        panic!("words, not {:?}", part.body);
    };
    assert!(
        text.starts_with("stats ") && text.ends_with(" tok"),
        "{text}"
    );
    assert_eq!(tone, Tone::Dim);
}

/// With no run on screen, the `context` part is the conversation's own session: its categories
/// against where its CLI compacts, the free space left out.
#[test]
fn the_context_part_reads_the_conversation_without_a_run() {
    use argonauts_core::events::context::{ContextCategory, ContextPart};
    let mut state = AppState::default();
    let mut status = crew_status("claude-code");
    status.line.settings =
        BottomLineSettings::from_toml("left = [\"mode\", \"context\"]\n").unwrap();
    state.status = Some(status);
    state.chat.context = Some(crate::state::ContextBreakdown {
        categories: vec![
            ContextCategory {
                part: ContextPart::Messages,
                tokens: 90_000,
            },
            ContextCategory {
                part: ContextPart::SystemPrompt,
                tokens: 10_000,
            },
            ContextCategory {
                part: ContextPart::Free,
                tokens: 100_000,
            },
        ],
        auto_compact_at: Some(160_000),
        window: 200_000,
        ..Default::default()
    });
    let line = built(&state);
    let context = line
        .left
        .iter()
        .find_map(|part| match part {
            LinePart::Added(part) if part.name == "context" => Some(part.body.clone()),
            _ => None,
        })
        .expect("the context part draws");
    let PartBody::Meter { value, .. } = context else {
        panic!("a meter: {context:?}");
    };
    assert_eq!(value, "63%");
}
