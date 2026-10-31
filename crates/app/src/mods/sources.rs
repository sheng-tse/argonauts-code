//! The built-in sources: numbers Argonauts Code already keeps, shaped as a mod's reading. Each is
//! built from plain facts gathered once, so a source never reads a file or asks a CLI itself.

use argonauts_core::Spend;
use argonauts_core::app::PlanUsageChip;
use argonauts_core::app::views::ContextRow;
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::look::window_size;
use argonauts_core::models::limits::PlanUsageStatus;
use argonauts_core::runs::api::{RunState, RunSummary};
use argonauts_core::runs::events::context::ContextKind;
use argonauts_core::settings::mods::reading::plain_number;
use argonauts_core::settings::mods::{BuiltinSource, ModColor, ModLevel, ModPart, ModReading};
use time::OffsetDateTime;

/// The most workflows the list shows, newest first.
const MOST_RUNS: usize = 8;

/// What the sources draw from.
#[derive(Debug, Clone, Default)]
pub(crate) struct Numbers {
    /// The main session's context, once it has one.
    pub context: Option<ContextRow>,
    /// One per CLI in the crew.
    pub plan_chips: Vec<PlanUsageChip>,
    /// Today's spend, by the CLI's id, in the crew's order.
    pub today: Vec<(String, Spend)>,
    /// The CLIs' names, which name the spend card's parts.
    pub names: DisplayNames,
    /// Newest first.
    pub runs: Vec<RunSummary>,
    pub now: Option<OffsetDateTime>,
}

/// The reading, or why there's none yet in plain words.
pub(crate) fn reading(source: BuiltinSource, numbers: &Numbers) -> Result<ModReading, String> {
    match source {
        BuiltinSource::Context => context(numbers),
        BuiltinSource::PlanUsage => plan_usage(numbers),
        BuiltinSource::Spend => spend(numbers),
        BuiltinSource::Tokens => tokens(numbers),
        BuiltinSource::Runs => Ok(runs(numbers)),
        BuiltinSource::Waiting => Ok(waiting(numbers)),
    }
}

/// The main session's context by its CLI's categories, each measured against the window, and
/// the free space in grey, which leaves out the room held back for compacting. A CLI that says
/// only how full the session is has one part for all of it.
fn context(numbers: &Numbers) -> Result<ModReading, String> {
    let row = numbers
        .context
        .as_ref()
        .ok_or("the main session hasn't said how full its context is yet")?;
    let window = row
        .window
        .ok_or("the main session's CLI doesn't say how large its context is")?;
    let part = |label: &str, tokens: u64| {
        ModPart::new(label, tokens as f64)
            .of(window as f64)
            .reading(window_size::tokens_beside(tokens, window))
    };
    let of_kind = |kind: ContextKind| {
        row.categories
            .iter()
            .filter(move |category| category.kind == kind && category.tokens > 0)
    };
    let mut parts: Vec<ModPart> = of_kind(ContextKind::Used)
        .map(|category| part(&category.name, category.tokens))
        .collect();
    if parts.is_empty() {
        parts.push(part("in context", row.used));
    }
    let used: u64 = parts.iter().map(|part| part.value as u64).sum();
    let held: u64 = of_kind(ContextKind::Buffer)
        .map(|category| category.tokens)
        .sum();
    let free = window.saturating_sub(used).saturating_sub(held);
    parts.push(part("Free space", free).colored(ModColor::Grey));
    let mut summary = format!(
        "{} of {}",
        window_size::tokens_beside(used, window),
        window_size::window_words(window)
    );
    if held > 0 && held < window {
        summary.push_str(&format!(
            " · compacts at {}",
            window_size::tokens_beside(window - held, window)
        ));
    }
    Ok(ModReading {
        summary,
        parts,
        ..ModReading::default()
    })
}

/// `5h` and `7d` for the windows the CLIs name, and the CLI's own word for any other.
fn window_words(window: Option<&str>) -> String {
    match window {
        Some(name) if name.contains("five") || name == "5h" => "5h".to_owned(),
        Some(name) if name.contains("week") || name.contains("seven") || name == "7d" => {
            "7d".to_owned()
        }
        Some(name) => name.replace('_', " "),
        None => "plan".to_owned(),
    }
}

/// `resets in 2h 10m`, or nothing once the time has passed.
fn resets_words(resets_at: Option<OffsetDateTime>, now: Option<OffsetDateTime>) -> Option<String> {
    let minutes = (resets_at? - now?).whole_minutes();
    match minutes {
        m if m <= 0 => None,
        m if m < 60 => Some(format!("resets in {m}m")),
        m if m < 48 * 60 => Some(format!("resets in {}h {}m", m / 60, m % 60)),
        m => Some(format!("resets in {}d", m / (24 * 60))),
    }
}

fn plan_usage(numbers: &Numbers) -> Result<ModReading, String> {
    let mut parts = Vec::new();
    let mut level = ModLevel::Ok;
    let mut fullest: Option<(f64, String)> = None;
    for chip in &numbers.plan_chips {
        let Some(reading) = &chip.reading else {
            continue;
        };
        let Some(used) = reading.used else {
            continue;
        };
        let label = format!("{} {}", chip.label, window_words(reading.window.as_deref()));
        level = level.max(match reading.status {
            PlanUsageStatus::Rejected => ModLevel::Danger,
            PlanUsageStatus::Warning => ModLevel::Warn,
            PlanUsageStatus::Allowed | PlanUsageStatus::Unknown => ModLevel::Ok,
        });
        if fullest.as_ref().is_none_or(|(most, _)| used > *most) {
            fullest = Some((used, label.clone()));
        }
        let mut part = ModPart::new(&label, (used * 100.0).round())
            .of(100.0)
            .on_cli(reading.agent_id.0.clone());
        if let Some(words) = resets_words(reading.resets_at, numbers.now) {
            part = part.noted(words);
        }
        parts.push(part);
    }
    let (used, label) = fullest.ok_or("no CLI has reported its plan usage yet")?;
    Ok(ModReading {
        summary: format!("{:.0}% {label}", used * 100.0),
        level,
        badge: None,
        parts,
    })
}

fn usd(spend: &Spend) -> String {
    let mark = if spend.cost_estimated { "~" } else { "" };
    format!("{mark}${:.2}", spend.cost_usd)
}

fn total(numbers: &Numbers) -> Spend {
    let mut all = Spend::default();
    for (_, spent) in &numbers.today {
        crate::views::sessions::add_spend(&mut all, spent);
    }
    all
}

fn spend(numbers: &Numbers) -> Result<ModReading, String> {
    if numbers.today.is_empty() {
        return Err("nothing has been spent today".to_owned());
    }
    Ok(ModReading {
        summary: format!("{} today", usd(&total(numbers))),
        parts: numbers
            .today
            .iter()
            .map(|(cli, spent)| {
                ModPart::new(&numbers.names.cli(cli), spent.cost_usd)
                    .reading(usd(spent))
                    .on_cli(cli)
            })
            .collect(),
        ..ModReading::default()
    })
}

fn tokens(numbers: &Numbers) -> Result<ModReading, String> {
    let all = total(numbers);
    if all.total_tokens() == 0 {
        return Err("no tokens have been used today".to_owned());
    }
    let part = |label: &str, count: u64, color| {
        ModPart::new(label, count as f64)
            .colored(color)
            .reading(plain_number(count as f64))
    };
    Ok(ModReading {
        summary: format!("{} tokens today", plain_number(all.total_tokens() as f64)),
        parts: vec![
            part("from cache", all.cached_tokens, ModColor::Blue),
            part(
                "new input",
                all.input_tokens.saturating_sub(all.cached_tokens),
                ModColor::Purple,
            ),
            part("output", all.output_tokens, ModColor::Cyan),
        ],
        ..ModReading::default()
    })
}

/// A run's state in plain words, and the color its row's mark takes.
fn state_words(run: &RunSummary) -> (&'static str, ModColor) {
    if run.needs_you() {
        return ("needs you", ModColor::Amber);
    }
    match run.state {
        RunState::Running | RunState::Pausing => ("working", ModColor::Blue),
        RunState::Waiting => ("waiting", ModColor::Grey),
        RunState::Merged => ("merged", ModColor::Green),
        RunState::PullRequest => ("pull request", ModColor::Green),
        RunState::BranchLeft => ("branch left", ModColor::Green),
        RunState::Stopped | RunState::Abandoned | RunState::Crashed => ("stopped", ModColor::Grey),
        RunState::NeverStarted => ("never started", ModColor::Grey),
        RunState::FromAnotherBuild => ("from an older Argonauts", ModColor::Grey),
        RunState::Failed => ("failed", ModColor::Red),
    }
}

fn run_name(run: &RunSummary) -> String {
    match &run.name {
        Some(name) if !name.is_empty() => name.clone(),
        _ if !run.title.is_empty() => run.title.clone(),
        _ => run.run.to_string(),
    }
}

fn runs(numbers: &Numbers) -> ModReading {
    let going = numbers.runs.iter().filter(|run| run.is_open()).count();
    let summary = match (numbers.runs.len(), going) {
        (0, _) => "no workflows yet".to_owned(),
        (count, 0) => format!("{count} workflows"),
        (_, going) => format!("{going} working"),
    };
    ModReading {
        summary,
        parts: numbers
            .runs
            .iter()
            .take(MOST_RUNS)
            .map(|run| {
                let (words, color) = state_words(run);
                ModPart::new(&run_name(run), 1.0)
                    .colored(color)
                    .reading(words)
            })
            .collect(),
        ..ModReading::default()
    }
}

fn waiting(numbers: &Numbers) -> ModReading {
    let parts: Vec<ModPart> = numbers
        .runs
        .iter()
        .filter(|run| run.needs_you())
        .map(|run| {
            let count = run.waiting.len();
            let words = if count == 1 {
                "1 approval".to_owned()
            } else {
                format!("{count} approvals")
            };
            ModPart::new(&run_name(run), count as f64)
                .colored(ModColor::Amber)
                .reading(words)
        })
        .collect();
    let count: usize = parts.iter().map(|part| part.value as usize).sum();
    ModReading {
        summary: if count == 0 {
            "nothing waits for you".to_owned()
        } else {
            format!("{count} waiting for you")
        },
        level: if count == 0 {
            ModLevel::Ok
        } else {
            ModLevel::Warn
        },
        badge: None,
        parts,
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::RunId;
    use argonauts_core::base::names::{AgentId, ApprovalId, ModelRef, SessionKey};
    use argonauts_core::models::limits::{HealthState, PlanUsageReading, PlanUsageSource};
    use argonauts_core::runs::events::context::ContextCategory;
    use argonauts_core::runs::events::run::RunMode;
    use time::Duration;

    use super::*;

    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap()
    }

    fn chip(cli: &str, used: f64, window: &str, status: PlanUsageStatus) -> PlanUsageChip {
        PlanUsageChip {
            label: DisplayNames::default().cli(cli),
            reading: Some(PlanUsageReading {
                agent_id: AgentId(cli.to_owned()),
                status,
                used: Some(used),
                resets_at: Some(now() + Duration::minutes(130)),
                window: Some(window.to_owned()),
                source: PlanUsageSource::UsageProbe,
                observed_at: now(),
                detail: None,
                wrap_up: false,
            }),
            health: HealthState::Healthy,
            windows: Vec::new(),
        }
    }

    fn run(id: &str, state: RunState, waiting: usize) -> RunSummary {
        RunSummary {
            run: RunId::new(id).unwrap(),
            title: format!("{id} title"),
            name: None,
            mode: RunMode::Crew,
            workflow: "council".to_owned(),
            state,
            stopped_at: String::new(),
            updated: now(),
            spent: Spend::default(),
            sessions_to_resume: 0,
            waiting: (0..waiting)
                .map(|n| ApprovalId::numbered(n as u32))
                .collect(),
            live: true,
            remind: false,
            shape_known: true,
        }
    }

    fn spent(cost: f64, input: u64, cached: u64, output: u64) -> Spend {
        Spend {
            input_tokens: input,
            output_tokens: output,
            cached_tokens: cached,
            cost_usd: cost,
            cost_partial: false,
            cost_estimated: true,
        }
    }

    #[test]
    fn plan_usage_is_a_meter_a_window_and_names_the_fullest() {
        let numbers = Numbers {
            plan_chips: vec![
                chip("claude-code", 0.56, "weekly", PlanUsageStatus::Allowed),
                chip("codex", 0.11, "five_hour", PlanUsageStatus::Warning),
                PlanUsageChip {
                    label: "agy".to_owned(),
                    reading: None,
                    health: HealthState::Healthy,
                    windows: Vec::new(),
                },
            ],
            now: Some(now()),
            ..Numbers::default()
        };
        let reading = reading(BuiltinSource::PlanUsage, &numbers).unwrap();
        assert_eq!(reading.summary, "56% Claude Code 7d");
        assert_eq!(reading.level, ModLevel::Warn);
        assert_eq!(reading.parts.len(), 2);
        assert_eq!(reading.parts[0].label, "Claude Code 7d");
        assert_eq!(reading.parts[0].cli.as_deref(), Some("claude-code"));
        assert_eq!(reading.parts[0].value, 56.0);
        assert_eq!(reading.parts[0].max, Some(100.0));
        assert_eq!(reading.parts[0].note.as_deref(), Some("resets in 2h 10m"));
        assert_eq!(reading.parts[1].label, "Codex 5h");
    }

    #[test]
    fn a_source_with_nothing_to_show_says_why() {
        let empty = Numbers::default();
        assert_eq!(
            reading(BuiltinSource::PlanUsage, &empty).unwrap_err(),
            "no CLI has reported its plan usage yet"
        );
        assert_eq!(
            reading(BuiltinSource::Spend, &empty).unwrap_err(),
            "nothing has been spent today"
        );
        assert_eq!(
            reading(BuiltinSource::Tokens, &empty).unwrap_err(),
            "no tokens have been used today"
        );
        assert!(reading(BuiltinSource::Context, &empty).is_err());
        assert_eq!(
            reading(BuiltinSource::Runs, &empty).unwrap().summary,
            "no workflows yet"
        );
        assert_eq!(
            reading(BuiltinSource::Waiting, &empty).unwrap().summary,
            "nothing waits for you"
        );
    }

    #[test]
    fn spend_and_tokens_sum_today_by_cli() {
        let numbers = Numbers {
            today: vec![
                (
                    "claude-code".to_owned(),
                    spent(1.5, 1_000_000, 800_000, 20_000),
                ),
                ("codex".to_owned(), spent(0.36, 300_000, 100_000, 80_000)),
            ],
            ..Numbers::default()
        };
        let spend = reading(BuiltinSource::Spend, &numbers).unwrap();
        assert_eq!(spend.summary, "~$1.86 today");
        assert_eq!(spend.parts[0].amount(), "~$1.50");
        assert_eq!(spend.parts[0].label, "Claude Code");
        assert_eq!(spend.parts[1].label, "Codex");
        assert_eq!(spend.parts[1].cli.as_deref(), Some("codex"));
        let tokens = reading(BuiltinSource::Tokens, &numbers).unwrap();
        assert_eq!(tokens.summary, "1.4M tokens today");
        let amounts: Vec<String> = tokens.parts.iter().map(ModPart::amount).collect();
        assert_eq!(amounts, ["900k", "400k", "100k"]);
    }

    fn main_session(used: u64, window: u64) -> Numbers {
        Numbers {
            context: Some(ContextRow {
                run: None,
                session: SessionKey::numbered(1),
                label: "main".to_owned(),
                worker: ModelRef::new("claude-code", None),
                used,
                window: Some(window),
                cap: None,
                tool_calls: 0,
                spent: Spend::default(),
                rotations: 0,
                estimated: false,
                counts: false,
                categories: Vec::new(),
                recorded: None,
                ended: None,
                answered_for: None,
            }),
            ..Numbers::default()
        }
    }

    /// A CLI that says only how full the session is has one part for all of it, then the
    /// free space.
    #[test]
    fn context_says_how_full_where_its_cli_gives_no_categories() {
        let read = reading(BuiltinSource::Context, &main_session(212_000, 1_000_000)).unwrap();
        assert_eq!(read.summary, "212k of 1M");
        assert_eq!(read.parts[0].label, "in context");
        assert_eq!(read.parts[1].value, 788_000.0);
        assert_eq!(read.parts[1].color, Some(ModColor::Grey));
    }

    /// A CLI that counts by category gives a part for each, in its order and by its names,
    /// and the free space leaves out the room held back for compacting.
    #[test]
    fn context_reads_by_the_clis_categories() {
        let category = |name: &str, tokens: u64, kind| ContextCategory {
            name: name.to_owned(),
            tokens,
            color: "promptBorder".to_owned(),
            kind,
        };
        let mut numbers = main_session(28_810, 1_000_000);
        if let Some(row) = numbers.context.as_mut() {
            row.categories = vec![
                category("System prompt", 4_380, ContextKind::Used),
                category("System tools", 24_430, ContextKind::Used),
                category("Autocompact buffer", 33_000, ContextKind::Buffer),
                category("Free space", 938_190, ContextKind::Free),
            ];
        }
        let read = reading(BuiltinSource::Context, &numbers).unwrap();
        assert_eq!(read.summary, "28.8k of 1M · compacts at 967k");
        let labels: Vec<&str> = read.parts.iter().map(|part| part.label.as_str()).collect();
        assert_eq!(labels, ["System prompt", "System tools", "Free space"]);
        assert_eq!(read.parts[2].value, 938_190.0);
        assert!(read.parts.iter().all(|part| part.max == Some(1_000_000.0)));
    }

    /// A 64k window reads 64k, and what is used and free of it reads in the same divisor, so
    /// the two parts add up to the window.
    #[test]
    fn a_window_of_1024s_reads_by_1024() {
        let read = reading(BuiltinSource::Context, &main_session(9_551, 65_536)).unwrap();
        assert_eq!(read.summary, "9.3k of 64k");
        let amounts: Vec<String> = read.parts.iter().map(ModPart::amount).collect();
        assert_eq!(amounts, ["9.3k", "54.7k"]);
        assert_eq!(read.parts[0].label, "in context");
        let full = reading(BuiltinSource::Context, &main_session(65_536, 65_536)).unwrap();
        assert_eq!(full.summary, "64k of 64k");
    }

    #[test]
    fn workflows_list_their_state_in_plain_words_and_waiting_counts_approvals() {
        let numbers = Numbers {
            runs: vec![
                run("run-0003", RunState::Running, 0),
                run("run-0002", RunState::Waiting, 2),
                run("run-0001", RunState::Merged, 0),
            ],
            ..Numbers::default()
        };
        let list = reading(BuiltinSource::Runs, &numbers).unwrap();
        assert_eq!(list.summary, "2 working");
        let words: Vec<String> = list.parts.iter().map(ModPart::amount).collect();
        assert_eq!(words, ["working", "needs you", "merged"]);
        assert_eq!(list.parts[0].label, "run-0003 title");
        let waiting = reading(BuiltinSource::Waiting, &numbers).unwrap();
        assert_eq!(waiting.summary, "2 waiting for you");
        assert_eq!(waiting.level, ModLevel::Warn);
        assert_eq!(waiting.parts[0].amount(), "2 approvals");
    }
}
