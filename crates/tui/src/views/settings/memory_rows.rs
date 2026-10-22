//! OpenCode's level when the controller knows this machine's memory: a fixed line under the
//! title with the most models can use, what is free now, and what holds the rest, models and
//! everything else, and every row in one field order, context · RAM · tools · cost · whether
//! it fits now or when, so each field sits in the same place down the list. A hosted model
//! says where it runs in the RAM field's place. A model that won't fit says which limit it
//! meets, and suggests one that does.

use argonauts_core::app::ModelChoice;
use argonauts_core::limits::CostEstimate;
use argonauts_core::model_picker::{Fit, ListedFacts, MachineMemory, ModelMemory};
use argonauts_core::window_size::window_words;

use crate::text::str_width;

const GIB: f64 = (1u64 << 30) as f64;
const TOO_BIG: &str = "too big here";

/// Bytes as the line and the rows say them: whole gigabytes.
pub(super) fn gb(bytes: u64) -> String {
    format!("{} GB", (bytes as f64 / GIB).round())
}

/// `Models can use` and `up to 52 GB of this Mac's 64 GB · 18 GB free now · models hold 25 GB
/// · macOS and your apps 21 GB`: the ceiling, what is free, and who holds the rest.
pub(super) fn banner(memory: &MachineMemory) -> (String, String) {
    let (machine, system) = if memory.mac {
        ("this Mac's", "macOS")
    } else {
        ("this machine's", "the system")
    };
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
            "up to {} of {machine} {} · {} free now · models hold {models} · {system} and your \
             apps {}",
            gb(memory.limit),
            gb(memory.total),
            gb(memory.free),
            gb(others)
        ),
    )
}

/// `64k context`, padded to the widest on the level.
pub(super) fn context_words(listed: &ListedFacts) -> String {
    listed
        .window
        .map(|window| format!("{} context", window_words(window)))
        .unwrap_or_default()
}

/// The widest context field on a level, so the RAM field starts in one column.
pub(super) fn context_width<'a>(listed: impl Iterator<Item = &'a ListedFacts>) -> usize {
    listed
        .map(|listed| str_width(&context_words(listed)))
        .max()
        .unwrap_or(0)
}

fn padded(listed: &ListedFacts, width: usize) -> String {
    let words = context_words(listed);
    let pad = width.saturating_sub(str_width(&words));
    format!("{words}{}", " ".repeat(pad))
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
        CostEstimate::Usd(usd) => Some(format!("~{} a task", crate::format::usd(usd))),
        CostEstimate::PlanUsage(share) => Some(format!(
            "~{} of plan usage a task",
            crate::format::percent(share)
        )),
        CostEstimate::Unknown => None,
    }
}

/// Whether it fits now or when: `fits now`, `fits once apps free 6 GB`, `fits once loaded
/// models unload`, `too big here` or `loaded`, with the smaller window it would fit at.
fn fit_words(memory: &ModelMemory, machine: &MachineMemory) -> String {
    let at = |at: Option<(u64, u64)>| {
        at.map(|(window, ram)| format!("; fits at {} ({} RAM)", window_words(window), gb(ram)))
            .unwrap_or_default()
    };
    match memory.fit {
        Fit::Loaded => "loaded".to_owned(),
        Fit::Fits => "fits now".to_owned(),
        Fit::Tight { at: smaller } => format!(
            "fits once apps free {}{}",
            gb(memory.ram.saturating_sub(machine.free)),
            at(smaller)
        ),
        Fit::FitsAlone => "fits once loaded models unload".to_owned(),
        Fit::TooBig { at: smaller } => format!("{TOO_BIG}{}", at(smaller)),
    }
}

/// A row's description in the one field order, then whether it fits, which the picker puts on
/// a line of its own where the row can't hold it, and the words to draw in the danger colour.
pub(super) fn description(
    choice: &ModelChoice,
    listed: &ListedFacts,
    width: usize,
    machine: &MachineMemory,
) -> Described {
    let place = match (&listed.hosted_by, &listed.memory) {
        (Some(host), _) => Some(format!("hosted by {host}")),
        (None, Some(memory)) => Some(format!("{} RAM", gb(memory.ram))),
        (None, None) => None,
    };
    let mut fields = vec![padded(listed, width)];
    fields.extend(place);
    if choice.worker.model.is_none() {
        if let Some(runs) = &choice.runs {
            fields.push(format!("runs {runs}"));
        }
        return Described {
            description: fields.join(" · "),
            ..Described::default()
        };
    }
    if choice.new {
        fields.insert(0, "new".to_owned());
    }
    fields.push(tools(listed).to_owned());
    fields.extend(cost(choice));
    let memory = listed
        .memory
        .as_ref()
        .filter(|_| listed.hosted_by.is_none());
    fields.extend(
        memory
            .and_then(|memory| memory.download)
            .map(|download| format!("{} download", gb(download))),
    );
    let mut tail: Vec<String> = memory
        .map(|memory| fit_words(memory, machine))
        .into_iter()
        .collect();
    if choice.recommended {
        tail.push("recommended".to_owned());
    }
    let danger = memory
        .filter(|memory| matches!(memory.fit, Fit::TooBig { .. }))
        .map(|_| TOO_BIG.to_owned());
    Described {
        description: fields.join(" · "),
        tail: (!tail.is_empty()).then(|| tail.join(" · ")),
        danger,
    }
}

/// A model row's words, as the picker draws them.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Described {
    pub description: String,
    /// Whether it fits, last, on a line of its own where the row can't hold it.
    pub tail: Option<String>,
    /// The words drawn in the danger colour.
    pub danger: Option<String>,
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

    fn mac(limit: u64, total: u64, in_use: u64, free: u64) -> MachineMemory {
        MachineMemory {
            limit: limit * GB,
            total: total * GB,
            in_use: in_use * GB,
            free: free * GB,
            mac: true,
        }
    }

    /// The line names the ceiling, what is free now, and both holders of the rest.
    #[test]
    fn the_line_says_what_models_can_use_and_who_holds_the_rest() {
        assert_eq!(
            banner(&mac(52, 64, 0, 37)),
            (
                "Models can use".to_owned(),
                "up to 52 GB of this Mac's 64 GB · 37 GB free now · models hold none · macOS \
                 and your apps 27 GB"
                    .to_owned()
            )
        );
        let pc = MachineMemory {
            mac: false,
            ..mac(24, 32, 25, 18)
        };
        assert_eq!(
            banner(&pc).1,
            "up to 24 GB of this machine's 32 GB · 18 GB free now · models hold 25 GB · the \
             system and your apps 0 GB"
        );
    }

    /// Each row's last words say whether it fits now, or when it would.
    #[test]
    fn each_fit_says_now_or_when() {
        let machine = mac(52, 64, 25, 18);
        let words = |fit, ram| {
            fit_words(
                &ModelMemory {
                    ram: ram * GB,
                    fit,
                    download: None,
                },
                &machine,
            )
        };
        assert_eq!(words(Fit::Fits, 17), "fits now");
        assert_eq!(
            words(Fit::Tight { at: None }, 24),
            "fits once apps free 6 GB"
        );
        assert_eq!(words(Fit::FitsAlone, 45), "fits once loaded models unload");
        assert_eq!(
            words(
                Fit::TooBig {
                    at: Some((32_768, 4 * GB))
                },
                60
            ),
            "too big here; fits at 32k (4 GB RAM)"
        );
        assert_eq!(words(Fit::Loaded, 25), "loaded");
    }
}
