//! The bottom line's parts from the app state and `mods/bottom-line.toml`: one chip per CLI
//! in the crew with its plan windows, and the parts you added.
//!
//! A reading is trusted only while its window lasts: one whose window has reset is dropped,
//! and one that couldn't be read again for an hour says how old it is.

use std::collections::BTreeMap;
use std::time::Duration;

use argonauts_core::Spend;
use argonauts_core::app::{HeaderAndBottomLine, LineFacts};
use argonauts_core::limits::{HealthState, PlanUsageReading, PlanUsageStatus};
use argonauts_core::settings::bottom_line::{
    ARTIFACT, AWAKE, BRANCH, CONTEXT, LineForm, MODE, PLAN_USAGE, PlanUsageAs, PlanUsageLine,
    PlanUsageResets, PlanUsageShow, PlanUsageWindows, SPEND, STATS, TOKENS,
};
use argonauts_core::settings::limit::{LimitAmount, LimitUnit};
use argonauts_core::settings::mods::ModPlace;
use argonauts_core::settings::ui::{NarratorMode, PluginOutput, UiSettings};
use argonauts_core::views::ContextUse;
use time::{OffsetDateTime, UtcOffset};

use super::status::{agent_for_label, focus_run, label_for_agent};
use crate::bottom::{AddedPart, BottomLine, CliState, CliUse, LinePart, MAIN, PartBody, WindowUse};
use crate::cell::Tone;
use crate::format;
use crate::kit::Levels;
use crate::overview::stats::pinned::StatsCard;
use crate::state::{AppState, binding_reading};
use crate::text::sanitize_line;
use crate::theme::Role;

mod preview;

pub use preview::{Sample, SampleCli, preview};

/// The built-in mod that shows the run on screen. On the line it is the running workflow.
pub use argonauts_core::settings::bottom_line::RUN;

/// A reading older than this says how old it is, since the plan may have moved on.
pub const AGE_SHOWN_AFTER: Duration = Duration::from_secs(60 * 60);

/// What a reading can't say about itself: when now is, and on which clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Clock {
    pub now: OffsetDateTime,
    pub utc_offset: UtcOffset,
}

/// The line as the state and its settings have it now. `slots` holds what other code draws
/// for the parts this module has no numbers for, by part name. A settings page draws its
/// preview with [`preview`].
pub fn build(
    state: &AppState,
    settings: &UiSettings,
    slots: &BTreeMap<String, PartBody>,
    now: OffsetDateTime,
) -> BottomLine {
    // Before the controller's first status the line is as it ships, with whatever a turn's
    // events already read.
    let unset = LineFacts::default();
    let status = state.status.as_ref();
    let facts = status.map_or(&unset, |status| &status.line);
    let set = &facts.settings;
    let clock = Clock {
        now,
        utc_offset: state.terminal.utc_offset.unwrap_or(UtcOffset::UTC),
    };
    let mods =
        (settings.plugin_output == PluginOutput::Show).then_some(&state.conversation.plugin_status);
    let source = Source {
        state,
        status,
        facts,
        slots,
        mods,
        clock,
    };
    let side = |names: &[String]| -> Vec<LinePart> {
        names.iter().flat_map(|name| source.parts(name)).collect()
    };
    let mut line = BottomLine {
        form: set.form,
        left: side(&set.left),
        right: side(&set.right),
        drop_first: set.narrow.drop.clone(),
        as_left: set.plan_usage.r#as == PlanUsageAs::Left,
    };
    // A mod's status text nobody placed sits last, dim, and is the first thing to go.
    for (name, text) in mods
        .into_iter()
        .flatten()
        .filter(|(name, _)| !set.has(name) && name != AWAKE)
    {
        line.right
            .push(LinePart::Added(AddedPart::words(name, text, Tone::Dim)));
        line.drop_first.insert(0, name.clone());
    }
    // A hold on the computer's sleep is yours to place. One nobody placed still shows while
    // it lasts, before the chips, since you asked for it.
    if !set.has(AWAKE) {
        line.right.splice(0..0, source.parts(AWAKE));
    }
    with_mode(line)
}

/// The mode can be moved, never left out: it says whether Argonauts Code will ask.
fn with_mode(mut line: BottomLine) -> BottomLine {
    let has_mode = |parts: &[LinePart]| parts.contains(&LinePart::Mode);
    if !has_mode(&line.left) && !has_mode(&line.right) {
        line.left.insert(0, LinePart::Mode);
    }
    line
}

struct Source<'a> {
    state: &'a AppState,
    status: Option<&'a HeaderAndBottomLine>,
    facts: &'a LineFacts,
    slots: &'a BTreeMap<String, PartBody>,
    /// The main session's Claude Code mods' status texts, while the settings show them.
    mods: Option<&'a Vec<(String, String)>>,
    clock: Clock,
}

impl Source<'_> {
    /// What the part named `name` draws now. A part with nothing to show draws nothing.
    fn parts(&self, name: &str) -> Vec<LinePart> {
        let added = |body: Option<PartBody>| {
            body.map(|body| {
                LinePart::Added(AddedPart {
                    name: name.to_owned(),
                    body,
                })
            })
        };
        let facts = self.facts;
        match name {
            MODE => vec![LinePart::Mode],
            PLAN_USAGE => {
                let clis = self.clis();
                if clis.is_empty() {
                    Vec::new()
                } else {
                    vec![LinePart::PlanUsage(clis)]
                }
            }
            ARTIFACT => added(self.artifact()).into_iter().collect(),
            BRANCH => added(facts.branch.as_ref().map(|branch| PartBody::Words {
                text: branch.clone(),
                tone: Tone::Dim,
            }))
            .into_iter()
            .collect(),
            SPEND => added(self.spend())
                .into_iter()
                .chain(self.narrator_spend())
                .collect(),
            CONTEXT => added(self.context()).into_iter().collect(),
            TOKENS => added(self.tokens()).into_iter().collect(),
            STATS => added(self.stats()).into_iter().collect(),
            // Anything else is a mod's: what other code drew for it, the `run` mod's own
            // line, or the status text a Claude Code mod pushed.
            other => {
                let drawn = self.slots.get(other).cloned();
                let run = || (other == RUN).then(|| self.run()).flatten();
                // A mod of yours from `mods/` that the line lists, with numbers to draw.
                let yours = || {
                    let view = self.state.mods.as_ref()?;
                    let (file, reading) = crate::mods::placed(view, ModPlace::BottomLine)
                        .find(|(file, _)| file.name == other)?;
                    Some(PartBody::Mod {
                        file: Box::new(file.clone()),
                        reading: Box::new(reading.clone()),
                    })
                };
                let said = || {
                    let (_, text) = self.mods?.iter().find(|(name, _)| name == other)?;
                    Some(PartBody::Words {
                        text: text.clone(),
                        tone: Tone::Dim,
                    })
                };
                added(drawn.or_else(run).or_else(yours).or_else(said))
                    .into_iter()
                    .collect()
            }
        }
    }

    /// The page the conversation published last, with how many came before it: what the
    /// screen saw published, else what the controller's status names.
    fn artifact(&self) -> Option<PartBody> {
        if let Some((name, others)) = self.state.artifacts.line_part() {
            return Some(PartBody::Artifact {
                name: name.to_owned(),
                others,
            });
        }
        let artifact = self.facts.artifact.as_ref()?;
        Some(PartBody::Artifact {
            name: artifact.name.clone(),
            others: artifact.others,
        })
    }

    /// The workflow under way by name with how many of its tasks are merged, once its plan
    /// has tasks to count: `add-due-dates ▄▄▄▖   3/5`.
    fn run(&self) -> Option<PartBody> {
        let run = focus_run(self.state).filter(|run| run.finished.is_none())?;
        let (done, total) = run.tasks_done();
        let named = run.named.as_ref().map(|named| named.name.trim());
        let name = named
            .filter(|name| !name.is_empty())
            .unwrap_or(run.title.trim());
        (total > 0 && !name.is_empty()).then(|| PartBody::Meter {
            label: name.to_owned(),
            label_role: Role::BarFill,
            ratio: done as f64 / total as f64,
            value: format!("{done}/{total}"),
            fill: Role::BarFill,
        })
    }

    /// One chip per CLI in the crew, in the crew's order, as the settings show them.
    fn clis(&self) -> Vec<CliUse> {
        let set = &self.facts.settings.plan_usage;
        // Gauges are one cell a window, so every CLI has room for both of its own.
        let both = self.facts.settings.form == LineForm::Gauges;
        let main = self.status.map(|status| status.main_agent.agent.as_str());
        crew(self.state, self.status)
            .iter()
            .map(|cli| {
                let is_main = main.is_some() && cli.agent_id.as_deref() == main;
                let spent = agent_spend(self.state, cli);
                let mut chip = cli_use(cli, is_main || both, set, spent, &self.clock);
                chip.main = is_main;
                chip
            })
            .filter(|cli| shows(cli, set.show))
            .collect()
    }

    /// The last seven days with their sparkline. While a run with a limit has the screen it is
    /// that run's spend against the limit, the number that can stop it. Where no day is on
    /// record it is what the run, or else this session, spent.
    fn spend(&self) -> Option<PartBody> {
        let words = |text: String| PartBody::Words {
            text,
            tone: Tone::Dim,
        };
        let status = self.status;
        let run = focus_run(self.state).map(|run| {
            let spend = run.spend.as_ref();
            let limit = spend
                .and_then(|spend| spend.limit)
                .or_else(|| status?.limit);
            let tokens = spend.map_or(run.spent.total_tokens(), |spend| spend.spent.total_tokens());
            (spend_text(&run.spend(), tokens, limit.as_ref()), limit)
        });
        if let Some((text, Some(_))) = &run {
            return Some(words(text.clone()));
        }
        let days = &self.facts.spend_days;
        let week: f64 = days.iter().sum();
        if let Some(status) = status.filter(|_| week > 0.0) {
            return Some(PartBody::Spark {
                text: format!("~{}", format::usd(week)),
                values: days.clone(),
                role: Role::for_cli(&status.main_agent.agent),
            });
        }
        if let Some((text, _)) = run {
            return Some(words(text));
        }
        let status = status?;
        let spent = status.spent.as_ref()?;
        let tokens = spent.input_tokens + spent.output_tokens;
        Some(words(spend_text(spent, tokens, status.limit.as_ref())))
    }

    /// `narrator $0.06/$0.50` while it's on, in the warning color once capped.
    fn narrator_spend(&self) -> Option<LinePart> {
        let narrator = self.state.narrator_status.as_ref()?;
        let on = match narrator.mode {
            NarratorMode::Off => false,
            NarratorMode::Live => true,
            NarratorMode::OnDemand => narrator.spent.cost_usd > 0.0,
        };
        let tone = if narrator.capped {
            Tone::Warning
        } else {
            Tone::Dim
        };
        let text = format!(
            "narrator {}/{}",
            format::spend(&narrator.spent),
            cap_text(&narrator.cap)
        );
        on.then(|| LinePart::Added(AddedPart::words("narrator", text, tone)))
    }

    /// Whose context is closest to where something happens to it, by name. With a run on
    /// screen that is its busiest live session, against where it starts afresh. The main
    /// session's shows with no run on screen, and when it alone is close to compacting.
    fn context(&self) -> Option<PartBody> {
        let main = self.state.conversation.context.as_ref().and_then(|count| {
            let limit = count.usage.limit();
            (limit > 0).then(|| count.usage.total_tokens as f64 / limit as f64)
        });
        let share = |context: ContextUse| context.used as f64 / context.cap as f64;
        let busiest = focus_run(self.state).and_then(|run| {
            run.running_sessions()
                .filter_map(|session| {
                    let context = session.context.filter(|context| context.cap > 0)?;
                    Some((session, share(context)))
                })
                .max_by(|a, b| a.1.total_cmp(&b.1))
        });
        let level = |ratio: f64| Levels::CONTEXT.level(ratio * 100.0);
        let (who, ratio) = match (main, busiest) {
            (Some(main), Some((_, crew))) if level(main) > level(crew) => (MAIN.to_owned(), main),
            (_, Some((session, crew))) => {
                let who = match &session.label {
                    Some(label) => label.clone(),
                    None => {
                        crate::state::task_label(session.role, session.step, session.task.as_ref())
                    }
                };
                (who, crew)
            }
            (Some(main), None) => (MAIN.to_owned(), main),
            (None, None) => return None,
        };
        Some(PartBody::Fill {
            label: CONTEXT.to_owned(),
            who,
            who_role: Role::Label,
            percent: (ratio.max(0.0) * 100.0).round() as u32,
            level: level(ratio),
        })
    }

    fn tokens(&self) -> Option<PartBody> {
        let tokens = match focus_run(self.state) {
            Some(run) => run.spent.total_tokens(),
            None => {
                let spent = self.status?.spent.as_ref()?;
                spent.input_tokens + spent.output_tokens
            }
        };
        (tokens > 0).then(|| PartBody::Words {
            text: format!("{} tokens", format::tokens(tokens)),
            tone: Tone::Dim,
        })
    }

    /// The stats mod as a hidden band says it: `stats 1.2M tok` for the period
    /// `mods/stats.toml` sets. The band asks for those numbers while the line lists it.
    fn stats(&self) -> Option<PartBody> {
        let usage = self.state.usage.as_deref()?;
        usage.stats.as_ref()?;
        let card = StatsCard {
            usage,
            names: &self.state.names,
            now: self.clock.now.to_offset(self.clock.utc_offset),
        };
        Some(PartBody::Words {
            text: card.key_words(),
            tone: Tone::Dim,
        })
    }
}

/// A CLI and every plan window known for it, the newest reading of each.
#[derive(Debug, Clone, PartialEq)]
pub struct CrewCli {
    /// The short name its chip shows.
    pub label: String,
    /// Its adapter id, where the label or a reading tells.
    pub agent_id: Option<String>,
    pub health: HealthState,
    pub readings: Vec<PlanUsageReading>,
}

/// The crew's CLIs in the controller's order, with newer readings and health from the run's
/// events folded in, and the main session's CLI first when the crew leaves it out. A CLI with
/// more than one chip (agy splits by model group) keeps the controller's readings.
fn crew(state: &AppState, status: Option<&HeaderAndBottomLine>) -> Vec<CrewCli> {
    let mut clis: Vec<CrewCli> = status
        .into_iter()
        .flat_map(|status| &status.plan_chips)
        .map(|chip| {
            let reading = chip.reading.iter().chain(&chip.windows).next();
            let agent_id = reading
                .map(|reading| reading.agent_id.0.clone())
                .or_else(|| agent_for_label(&chip.label).map(str::to_owned));
            let mut readings = chip.windows.clone();
            readings.extend(chip.reading.clone());
            CrewCli {
                label: chip.label.trim().to_owned(),
                agent_id,
                health: chip.health,
                readings,
            }
        })
        .collect();
    let chips_of = |agent_id: &str, clis: &[CrewCli]| {
        clis.iter()
            .filter(|cli| cli.agent_id.as_deref() == Some(agent_id))
            .count()
    };
    let counts: Vec<usize> = clis
        .iter()
        .map(|cli| {
            cli.agent_id
                .as_deref()
                .map_or(0, |agent_id| chips_of(agent_id, &clis))
        })
        .collect();
    for (cli, chips) in clis.iter_mut().zip(counts) {
        let Some(agent_id) = cli.agent_id.as_deref().filter(|_| chips == 1) else {
            continue;
        };
        cli.readings.extend(
            state
                .plan_windows
                .get(agent_id)
                .into_iter()
                .flatten()
                .cloned(),
        );
        if let Some(health) = state.health.get(agent_id) {
            cli.health = health.state;
        }
    }
    for (agent_id, readings) in &state.plan_windows {
        if chips_of(agent_id, &clis) == 0 {
            clis.push(CrewCli {
                label: label_for_agent(agent_id),
                agent_id: Some(agent_id.clone()),
                health: state
                    .health
                    .get(agent_id)
                    .map_or(HealthState::Healthy, |health| health.state),
                readings: readings.clone(),
            });
        }
    }
    if let Some(main) = status.map(|status| &status.main_agent.agent)
        && chips_of(main, &clis) == 0
    {
        clis.insert(
            0,
            CrewCli {
                label: label_for_agent(main),
                agent_id: Some(main.clone()),
                health: state
                    .health
                    .get(main)
                    .map_or(HealthState::Healthy, |health| health.state),
                readings: Vec::new(),
            },
        );
    }
    for cli in &mut clis {
        cli.readings = newest_per_window(std::mem::take(&mut cli.readings));
    }
    clis
}

fn newest_per_window(readings: Vec<PlanUsageReading>) -> Vec<PlanUsageReading> {
    let mut newest: Vec<PlanUsageReading> = Vec::new();
    for reading in readings {
        match newest
            .iter_mut()
            .find(|known| known.window == reading.window)
        {
            Some(known) if known.observed_at <= reading.observed_at => *known = reading,
            Some(_) => {}
            None => newest.push(reading),
        }
    }
    newest
}

/// What this app's runs spent on a CLI, for one without a plan.
fn agent_spend(state: &AppState, cli: &CrewCli) -> Option<f64> {
    let agent_id = cli.agent_id.as_deref().unwrap_or(&cli.label);
    let spend: f64 = state
        .runs
        .values()
        .flat_map(|run| run.sessions.values())
        .filter(|session| session.agent_id.eq_ignore_ascii_case(agent_id))
        .filter_map(|session| session.cost_usd)
        .sum();
    (spend > 0.0).then_some(spend)
}

/// A CLI's chip: its state, and the windows the settings show for it. The CLI in use shows
/// both its windows and the others their fuller one; a window at 90% always shows.
pub fn cli_use(
    cli: &CrewCli,
    main: bool,
    set: &PlanUsageLine,
    spent_usd: Option<f64>,
    clock: &Clock,
) -> CliUse {
    let key = cli.agent_id.as_deref().unwrap_or(&cli.label);
    let mut chip = CliUse::new(sanitize_line(&cli.label).trim(), Role::for_cli(key));
    chip.main = main;
    // A window that has reset since it was read says nothing about the plan now.
    let live: Vec<PlanUsageReading> = cli
        .readings
        .iter()
        .filter(|reading| reading.resets_at.is_none_or(|at| at > clock.now))
        .cloned()
        .collect();
    let binding = binding_reading(&live);
    let shows_reset = |near: bool| match set.resets {
        PlanUsageResets::NearLimit => near,
        PlanUsageResets::Always => true,
        PlanUsageResets::Never => false,
    };
    chip.state = match binding {
        _ if cli.health == HealthState::Down => CliState::Down,
        Some(reading) if reading.status == PlanUsageStatus::Rejected && reading.wrap_up => {
            CliState::WrappingUp
        }
        Some(reading) if reading.status == PlanUsageStatus::Rejected => CliState::Out {
            until: reading
                .resets_at
                .filter(|_| shows_reset(true))
                .map(|at| clock_time(at, clock)),
        },
        // Some readings say only that the CLI warned, not how much is used.
        Some(reading) if reading.status == PlanUsageStatus::Warning && reading.used.is_none() => {
            CliState::NearLimit
        }
        _ => CliState::Fine,
    };
    let mut windows: Vec<WindowUse> = live
        .iter()
        .filter(|reading| reading.status != PlanUsageStatus::Unknown)
        .filter_map(|reading| {
            let label = window_name(reading.window.as_deref()?);
            let mut window = WindowUse::new(label, reading.used?);
            window.resets = reading
                .resets_at
                .filter(|_| shows_reset(window.near))
                .map(|at| clock_time(at, clock));
            window.age = age(reading.observed_at, clock.now);
            Some(window)
        })
        .collect();
    windows.sort_by_key(|window| window_minutes(&window.label));
    chip.windows = shown_windows(windows, main, set.windows);
    if chip.windows.is_empty() && !has_plan(&cli.label) {
        chip.note = spent_usd.filter(|spent| *spent > 0.0).map(format::usd);
    }
    chip
}

/// Whether a CLI has a chip, by `[plan-usage] show`.
fn shows(cli: &CliUse, show: PlanUsageShow) -> bool {
    match show {
        PlanUsageShow::All => true,
        PlanUsageShow::Main => cli.main,
        PlanUsageShow::NearLimit => cli.needs_you(),
    }
}

/// The windows a chip shows, by `[plan-usage] windows`. Whatever the setting, a window at
/// 90% is never hidden.
fn shown_windows(windows: Vec<WindowUse>, main: bool, show: PlanUsageWindows) -> Vec<WindowUse> {
    let fullest = (0..windows.len())
        .max_by(|&a, &b| windows[a].used.total_cmp(&windows[b].used).then(b.cmp(&a)));
    let plain = |window: &WindowUse| matches!(window.label.as_str(), "5h" | "7d");
    windows
        .iter()
        .enumerate()
        .filter(|(at, window)| {
            window.near
                || match show {
                    PlanUsageWindows::Both if main => plain(window) || Some(*at) == fullest,
                    PlanUsageWindows::Both | PlanUsageWindows::Fullest => Some(*at) == fullest,
                    PlanUsageWindows::FiveHours => window.label == "5h",
                    PlanUsageWindows::SevenDays => window.label == "7d",
                }
        })
        .map(|(_, window)| window.clone())
        .collect()
}

/// `4:40pm` today, or `Tue 3:45pm` on another day, as Claude Code writes a reset time.
pub fn clock_time(at: OffsetDateTime, clock: &Clock) -> String {
    let local = at.to_offset(clock.utc_offset);
    let (hour, half) = match local.hour() {
        0 => (12, "am"),
        hour @ 1..=11 => (hour, "am"),
        12 => (12, "pm"),
        hour => (hour - 12, "pm"),
    };
    let time = format!("{hour}:{:02}{half}", local.minute());
    if local.date() == clock.now.to_offset(clock.utc_offset).date() {
        time
    } else {
        let day = local.weekday().to_string();
        format!("{} {time}", &day[..3.min(day.len())])
    }
}

/// `2d`, `3h`: how long ago a reading was taken, once that's long enough to matter.
fn age(observed_at: OffsetDateTime, now: OffsetDateTime) -> Option<String> {
    let age = Duration::try_from(now - observed_at).ok()?;
    if age <= AGE_SHOWN_AFTER {
        return None;
    }
    let hours = age.as_secs() / 3600;
    Some(if hours >= 48 {
        format!("{}d", hours / 24)
    } else {
        format!("{hours}h")
    })
}

/// `five_hour` → `5h`, `seven_day` → `7d`, and other windows in plain words.
fn window_name(window: &str) -> String {
    match window.trim() {
        "five_hour" | "5h" => "5h".to_owned(),
        "seven_day" | "weekly" | "7d" => "7d".to_owned(),
        "seven_day_opus" => "7d Opus".to_owned(),
        "seven_day_sonnet" => "7d Sonnet".to_owned(),
        "daily" | "one_day" => "1d".to_owned(),
        other => other.replace('_', " "),
    }
}

/// How long a window lasts, for drawing the shorter one first. A window that names no length
/// goes last.
fn window_minutes(label: &str) -> (u64, usize) {
    let digits: String = label.chars().take_while(char::is_ascii_digit).collect();
    let unit = label[digits.len()..].chars().next();
    let minutes = match (digits.parse::<u64>(), unit) {
        (Ok(count), Some('h')) => count * 60,
        (Ok(count), Some('d')) => count * 60 * 24,
        _ => u64::MAX,
    };
    // `7d Opus` after `7d`.
    (minutes, label.len())
}

/// Subscription CLIs have a plan, so one without a reading has simply not been read yet.
fn has_plan(label: &str) -> bool {
    matches!(label, "Claude" | "Codex") || label == "agy" || label.starts_with("agy ")
}

/// `$0.21/$5.00`, `48.2k/2m tokens`, or the spend alone without a limit it can be read against.
fn spend_text(spent: &Spend, tokens: u64, limit: Option<&LimitAmount>) -> String {
    let spent = format::spend(spent);
    match limit {
        Some(LimitAmount {
            unit: LimitUnit::Usd,
            amount,
        }) => format!("{spent}/{}", format::usd(*amount)),
        Some(LimitAmount {
            unit: LimitUnit::Tokens,
            amount,
        }) => format!(
            "{}/{} tokens",
            format::tokens(tokens),
            format::tokens(amount.max(0.0) as u64)
        ),
        Some(LimitAmount {
            unit: LimitUnit::Minutes,
            amount,
        }) => format!("{spent} · limit {}m", amount.max(0.0).round() as u64),
        Some(LimitAmount {
            unit: LimitUnit::PlanUsage,
            amount,
        }) => format!(
            "{spent} · limit {}% of plan usage",
            (amount.clamp(0.0, 1.0) * 100.0).round() as u64
        ),
        None => spent,
    }
}

/// `0.50 usd` → `$0.50`; other caps as written.
fn cap_text(cap: &str) -> String {
    let mut words = cap.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some(amount), Some(unit), None) if unit.eq_ignore_ascii_case("usd") => amount
            .parse::<f64>()
            .map_or_else(|_| cap.trim().to_owned(), format::usd),
        _ => cap.trim().to_owned(),
    }
}

#[cfg(test)]
mod tests;
