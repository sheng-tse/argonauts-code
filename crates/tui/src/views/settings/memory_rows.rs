//! A level whose CLI lists its models by provider, as OpenCode does: one table down the whole
//! list, each group's heading naming the columns its rows fill. When the controller knows this
//! computer's memory, a fixed line under the title says the most models can use, what is free
//! now, and what holds the rest, and a gauge under it what the model under the pointer would
//! take. A hosted model's group says where it runs. A model that won't fit says which limit it
//! meets, and suggests one that does.

use std::time::Instant;

use argonauts_core::app::ModelChoice;
use argonauts_core::app::model_picker::{Fit, ListedFacts, MachineMemory, ModelMemory};
use argonauts_core::look::window_size::window_words;
use argonauts_core::models::limits::CostEstimate;

use crate::base::theme::Role;
use crate::bottom::{Gauge, PickerCell, PickerColumn};

const GIB: f64 = (1u64 << 30) as f64;
const TOO_BIG: &str = "too big here";

/// The table's columns, in the order its cells come.
pub(super) fn columns() -> Vec<PickerColumn> {
    vec![
        PickerColumn::numbers("context"),
        PickerColumn::numbers("memory"),
        PickerColumn::words("abilities"),
        PickerColumn::words("cost"),
        PickerColumn::numbers("download"),
        PickerColumn::words("fit"),
    ]
}

/// Bytes as the line and the rows say them: whole gigabytes.
pub(super) fn gb(bytes: u64) -> String {
    format!("{} GB", whole_gb(bytes))
}

/// The number alone, before "of 52 GB".
fn whole_gb(bytes: u64) -> f64 {
    (bytes as f64 / GIB).round()
}

/// `Models can use` and `up to 52 GB of this computer's 64 GB · 18 GB free now · models hold
/// 25 GB · other apps 21 GB`: the ceiling, what is free, and who holds the rest.
pub(super) fn banner(memory: &MachineMemory) -> (String, String) {
    let models = if memory.in_use == 0 {
        "none".to_owned()
    } else {
        gb(memory.in_use)
    };
    let others = memory
        .total
        .saturating_sub(memory.free)
        .saturating_sub(memory.in_use);
    (
        "Models can use".to_owned(),
        format!(
            "up to {} of this computer's {} · {} free now · models hold {models} · other apps {}",
            gb(memory.limit),
            gb(memory.total),
            gb(memory.free),
            gb(others)
        ),
    )
}

/// A group's heading: where its models come from, and for the ones not installed, why they
/// are offered.
pub(super) fn heading(group: &str, listed: &ListedFacts) -> String {
    if listed.hosted_by.is_some() {
        format!("Hosted by {group}")
    } else if listed
        .memory
        .as_ref()
        .is_some_and(|memory| memory.download.is_some())
    {
        group.to_owned()
    } else {
        format!("Installed through {group}")
    }
}

fn tools(listed: &ListedFacts) -> &'static str {
    match (listed.no_tools, listed.thinking) {
        (true, _) => "no tools",
        (false, true) => "tools, thinking",
        (false, false) => "tools",
    }
}

fn cost(choice: &ModelChoice) -> Option<String> {
    match choice.cost {
        CostEstimate::Free => Some("free".to_owned()),
        CostEstimate::Usd(usd) => Some(format!("~{} a task", crate::base::format::usd(usd))),
        CostEstimate::PlanUsage(share) => Some(format!(
            "~{} of plan usage a task",
            crate::base::format::percent(share)
        )),
        CostEstimate::Unknown => None,
    }
}

/// `Every model here is free`, when every model on the level costs the same, so the rows
/// needn't say it each.
pub(super) fn same_cost<'a>(models: impl Iterator<Item = &'a ModelChoice>) -> Option<String> {
    let costs: Vec<Option<String>> = models
        .filter(|choice| choice.worker.model.is_some())
        .map(cost)
        .collect();
    let first = costs.first()?.clone()?;
    if costs.len() < 2 || costs.iter().any(|cost| cost.as_ref() != Some(&first)) {
        return None;
    }
    Some(if first == "free" {
        "Every model here is free".to_owned()
    } else {
        format!("Every model here costs {first}")
    })
}

/// Whether it fits now or when, in the colour that says so: `fits now`, `fits once apps free
/// 6 GB`, `fits once loaded models unload`, `too big here` or `loaded`, with the smaller window
/// it would fit at.
fn fit_cell(memory: &ModelMemory, machine: &MachineMemory) -> PickerCell {
    let at = |at: Option<(u64, u64)>| {
        at.map(|(window, ram)| format!("; fits at {} ({})", window_words(window), gb(ram)))
            .unwrap_or_default()
    };
    match memory.fit {
        Fit::Loaded => PickerCell::in_role("loaded", Role::Success),
        Fit::Fits => PickerCell::in_role("fits now", Role::Success),
        Fit::Tight { at: smaller } => PickerCell::in_role(
            format!(
                "fits once apps free {}{}",
                gb(memory.ram.saturating_sub(machine.free)),
                at(smaller)
            ),
            Role::Warn,
        ),
        Fit::FitsAlone => PickerCell::in_role("fits once loaded models unload", Role::Warn),
        Fit::TooBig { at: smaller } => {
            PickerCell::in_role(format!("{TOO_BIG}{}", at(smaller)), Role::Danger)
        }
    }
}

/// A model's row in the table's columns, its cost left out where the level says it once for
/// every model. Default, which stands for another model, says what it runs in its description
/// instead.
pub(super) fn cells(
    choice: &ModelChoice,
    listed: &ListedFacts,
    machine: Option<&MachineMemory>,
    cost_said: bool,
) -> Vec<PickerCell> {
    if choice.worker.model.is_none() {
        return Vec::new();
    }
    let memory = listed
        .memory
        .as_ref()
        .filter(|_| listed.hosted_by.is_none());
    let text = |text: Option<String>| text.map(PickerCell::new).unwrap_or_default();
    let mut cells = vec![
        text(listed.window.map(window_words)),
        text(memory.map(|memory| gb(memory.ram))),
        PickerCell::new(tools(listed)),
        text(cost(choice).filter(|_| !cost_said)),
        text(memory.and_then(|memory| memory.download).map(gb)),
        memory
            .zip(machine)
            .map(|(memory, machine)| fit_cell(memory, machine))
            .unwrap_or_default(),
    ];
    // What sets one row apart closes it, after whatever it says last.
    let notes: Vec<&str> = [(choice.recommended, "recommended"), (choice.new, "new")]
        .into_iter()
        .filter_map(|(set, word)| set.then_some(word))
        .collect();
    if let Some(last) = cells.iter_mut().rev().find(|cell| !cell.is_empty())
        && !notes.is_empty()
    {
        last.text = format!("{} · {}", last.text, notes.join(" · "));
    }
    cells
}

/// Default's description: the window and host of the model it runs, and that model.
pub(super) fn default_description(choice: &ModelChoice, listed: &ListedFacts) -> String {
    let mut fields = Vec::new();
    fields.extend(
        listed
            .window
            .map(|window| format!("{} context", window_words(window))),
    );
    fields.extend(
        listed
            .hosted_by
            .as_ref()
            .map(|host| format!("hosted by {host}")),
    );
    fields.extend(choice.runs.as_ref().map(|runs| format!("runs {runs}")));
    fields.join(" · ")
}

/// The gauge for the row under the pointer: what it takes of the most models can use here,
/// solid as far as what is free now covers it, and the rest hatched with what is short.
pub(super) fn gauge(choice: Option<&ModelChoice>, machine: &MachineMemory) -> Gauge {
    let limit = machine.limit.max(1);
    let share = |bytes: u64| (bytes as f64 / limit as f64).min(1.0);
    let listed = choice.and_then(|choice| choice.listed.as_ref());
    let memory = listed
        .filter(|listed| listed.hosted_by.is_none())
        .and_then(|listed| listed.memory.as_ref());
    let Some(memory) = memory else {
        let elsewhere = listed
            .and_then(|listed| listed.hosted_by.as_ref())
            .map(|host| format!(" · hosted by {host}"))
            .unwrap_or_default();
        return Gauge {
            label: "this pick".to_owned(),
            words: format!("none of {}{elsewhere}", gb(machine.limit)),
            ..Gauge::default()
        };
    };
    let need = memory.ram;
    let mut words = format!("{} of {}", whole_gb(need), gb(machine.limit));
    if matches!(memory.fit, Fit::Loaded) {
        words.push_str(" · loaded now");
    }
    let (fits, short) = match memory.fit {
        Fit::Loaded | Fit::Fits => (need, None),
        Fit::Tight { .. } => (
            machine.free,
            Some(format!(
                "{} more than is free now",
                gb(need.saturating_sub(machine.free))
            )),
        ),
        Fit::FitsAlone => (
            need.min(machine.free),
            Some(format!(
                "{} more than the loaded models leave",
                gb(need.saturating_sub(machine.limit.saturating_sub(machine.in_use)))
            )),
        ),
        Fit::TooBig { .. } => (
            need.min(machine.free),
            Some(format!(
                "{} more than models may use here",
                gb(need.saturating_sub(machine.limit))
            )),
        ),
    };
    Gauge {
        label: "this pick".to_owned(),
        fits: share(fits),
        needs: share(need),
        words,
        short,
        moved: None,
    }
}

/// The gauge for the row the pointer moved to, springing from where `before` stood.
pub(super) fn gauge_after(gauge: Gauge, before: Option<&Gauge>, now: Instant) -> Gauge {
    match before {
        Some(before) => gauge.moving_from(before, now),
        None => gauge,
    }
}

/// Where the effort line goes, while the pointer is on a model that won't fit as it is: which
/// limit it meets, and what to do instead.
pub(super) fn warning(
    choice: &ModelChoice,
    machine: &MachineMemory,
    suggested: Option<&ModelChoice>,
) -> Option<String> {
    let listed = choice.listed.as_ref()?;
    let memory = listed
        .memory
        .as_ref()
        .filter(|_| listed.hosted_by.is_none())?;
    let name = choice.display_name();
    let needs = format!("{name} needs {} of RAM", gb(memory.ram));
    let smaller = |at: Option<(u64, u64)>| {
        at.map(|(window, ram)| format!("try it at {} ({} of RAM)", window_words(window), gb(ram)))
    };
    match memory.fit {
        Fit::TooBig { at } => {
            let instead = smaller(at)
                .map(|try_it| format!(" Or {try_it}."))
                .or_else(|| {
                    let other = suggested.filter(|other| other.worker != choice.worker)?;
                    let ram = other.listed.as_ref()?.memory.as_ref()?.ram;
                    Some(format!(
                        " Try {}, which needs {}.",
                        other.display_name(),
                        gb(ram)
                    ))
                });
            Some(format!(
                "{needs}; models may use at most {} here.{}",
                gb(machine.limit),
                instead.unwrap_or_default()
            ))
        }
        Fit::FitsAlone => Some(format!(
            "{needs}, more than the {} beside the loaded models; it fits once they're freed.",
            gb(machine.limit.saturating_sub(machine.in_use))
        )),
        Fit::Tight { at } => Some(format!(
            "{needs} and {} is free now; close some apps{}.",
            gb(machine.free),
            smaller(at)
                .map(|try_it| format!(", or {try_it}"))
                .unwrap_or_else(|| " first".to_owned())
        )),
        Fit::Fits | Fit::Loaded => None,
    }
}

/// What Enter asks before anything downloads.
pub(super) fn download_question(choice: &ModelChoice) -> Option<String> {
    let download = choice.listed.as_ref()?.memory.as_ref()?.download?;
    Some(format!(
        "Download {} with Ollama ({} download)?  Enter to download · Esc to keep looking",
        choice.display_name(),
        gb(download)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1 << 30;

    fn machine(limit: u64, total: u64, in_use: u64, free: u64) -> MachineMemory {
        MachineMemory {
            limit: limit * GB,
            total: total * GB,
            in_use: in_use * GB,
            free: free * GB,
        }
    }

    fn choice(worker: &str) -> ModelChoice {
        ModelChoice {
            worker: worker.parse().unwrap(),
            provider: String::new(),
            access: argonauts_core::models::registry::ModelAccess::Local,
            cost: CostEstimate::Free,
            plan_usage: None,
            efforts: Vec::new(),
            strengths: Vec::new(),
            current: false,
            note: None,
            older: false,
            name: String::new(),
            description: None,
            recommended: false,
            new: false,
            default_effort: None,
            effort: None,
            alias: false,
            runs: None,
            listed: None,
        }
    }

    fn local(ram: u64, fit: Fit) -> ModelChoice {
        ModelChoice {
            listed: Some(ListedFacts {
                memory: Some(ModelMemory {
                    ram: ram * GB,
                    fit,
                    download: None,
                }),
                ..ListedFacts::default()
            }),
            ..choice("opencode/ollama/sample:8b")
        }
    }

    /// The line names the ceiling, what is free now, and both holders of the rest, in the same
    /// words on every system.
    #[test]
    fn the_line_says_what_models_can_use_and_who_holds_the_rest() {
        assert_eq!(
            banner(&machine(52, 64, 0, 37)),
            (
                "Models can use".to_owned(),
                "up to 52 GB of this computer's 64 GB · 37 GB free now · models hold none · \
                 other apps 27 GB"
                    .to_owned()
            )
        );
        assert_eq!(
            banner(&machine(24, 32, 25, 18)).1,
            "up to 24 GB of this computer's 32 GB · 18 GB free now · models hold 25 GB · other \
             apps 0 GB"
        );
    }

    /// Each row's fit says now or when, in the colour that says whether it can run.
    #[test]
    fn each_fit_says_now_or_when_in_its_colour() {
        let machine = machine(52, 64, 25, 18);
        let cell = |fit, ram| {
            let cell = fit_cell(
                &ModelMemory {
                    ram: ram * GB,
                    fit,
                    download: None,
                },
                &machine,
            );
            (cell.text, cell.role)
        };
        let fits = Some(Role::Success);
        assert_eq!(cell(Fit::Fits, 17), ("fits now".to_owned(), fits));
        assert_eq!(
            cell(Fit::Tight { at: None }, 24),
            ("fits once apps free 6 GB".to_owned(), Some(Role::Warn))
        );
        assert_eq!(
            cell(Fit::FitsAlone, 45),
            (
                "fits once loaded models unload".to_owned(),
                Some(Role::Warn)
            )
        );
        assert_eq!(
            cell(
                Fit::TooBig {
                    at: Some((32_768, 4 * GB))
                },
                60
            ),
            (
                "too big here; fits at 32k (4 GB)".to_owned(),
                Some(Role::Danger)
            )
        );
        assert_eq!(cell(Fit::Loaded, 25), ("loaded".to_owned(), fits));
    }

    /// The gauge fills what is free now and hatches the rest, saying how much is short.
    #[test]
    fn the_gauge_fills_what_fits_now_and_names_the_shortfall() {
        let here = machine(52, 64, 25, 18);
        let tight = gauge(Some(&local(21, Fit::Tight { at: None })), &here);
        assert_eq!(tight.words, "21 of 52 GB");
        let loaded = gauge(Some(&local(25, Fit::Loaded)), &here);
        assert_eq!(loaded.words, "25 of 52 GB · loaded now");
        assert_eq!(tight.short.as_deref(), Some("3 GB more than is free now"));
        assert!((tight.fits - 18.0 / 52.0).abs() < 1e-9);
        assert!((tight.needs - 21.0 / 52.0).abs() < 1e-9);

        let fits = gauge(Some(&local(11, Fit::Fits)), &here);
        assert_eq!((fits.fits, fits.short), (fits.needs, None));

        let too_big = gauge(Some(&local(60, Fit::TooBig { at: None })), &here);
        assert_eq!(
            too_big.short.as_deref(),
            Some("8 GB more than models may use here")
        );
        assert!((too_big.needs - 1.0).abs() < 1e-9);

        let nothing = gauge(None, &here);
        assert_eq!(
            (nothing.words.as_str(), nothing.needs),
            ("none of 52 GB", 0.0)
        );
    }

    /// Every row costing the same says so once, above the rows.
    #[test]
    fn a_cost_every_model_shares_is_said_once() {
        let free = local(8, Fit::Fits);
        let default = choice("opencode");
        let rows = [default.clone(), free.clone(), free.clone()];
        assert_eq!(
            same_cost(rows.iter()).as_deref(),
            Some("Every model here is free")
        );
        let paid = ModelChoice {
            cost: CostEstimate::Usd(0.04),
            ..free.clone()
        };
        assert_eq!(same_cost([free.clone(), paid].iter()), None);
        assert_eq!(same_cost([default, free].iter()), None);
    }
}
