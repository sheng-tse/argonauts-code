//! OpenCode's level when the controller knows this machine's memory: a fixed line under the
//! title with the most models may use, what loaded models hold and what is free now, and
//! every row in one field order, context · RAM · tools · cost · whether it fits, so each
//! field sits in the same place down the list. A hosted model says where it runs in the RAM
//! field's place. A model that won't fit says which limit it meets, and suggests one that does.

use argonauts_core::app::ModelChoice;
use argonauts_core::limits::CostEstimate;
use argonauts_core::model_picker::{Fit, ListedFacts, MachineMemory, ModelMemory};
use argonauts_core::window_size::window_words;

use crate::text::str_width;

const GIB: f64 = (1u64 << 30) as f64;
const TOO_BIG: &str = "too big";

/// Bytes as the line and the rows say them: whole gigabytes.
pub(super) fn gb(bytes: u64) -> String {
    format!("{} GB", (bytes as f64 / GIB).round())
}

/// `RAM for models:` and `up to 52 GB of this Mac's 64 GB · 25 GB in use · 18 GB free now`.
pub(super) fn banner(memory: &MachineMemory) -> (String, String) {
    let machine = if memory.mac {
        "this Mac's"
    } else {
        "this machine's"
    };
    let in_use = if memory.in_use == 0 {
        "none in use".to_owned()
    } else {
        format!("{} in use", gb(memory.in_use))
    };
    (
        "RAM for models:".to_owned(),
        format!(
            "up to {} of {machine} {} · {in_use} · {} free now",
            gb(memory.limit),
            gb(memory.total),
            gb(memory.free)
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

/// `too big`, `fits alone`, `tight`, `fits` or `loaded`, with the smaller window it would
/// fit at, and the download size before it for one not installed.
fn fit_words(memory: &ModelMemory) -> String {
    let at = |at: Option<(u64, u64)>| {
        at.map(|(window, ram)| format!("; fits at {} ({} RAM)", window_words(window), gb(ram)))
            .unwrap_or_default()
    };
    let fit = match memory.fit {
        Fit::Loaded => "loaded".to_owned(),
        Fit::Fits => "fits".to_owned(),
        Fit::Tight { at: smaller } => format!("tight{}", at(smaller)),
        Fit::FitsAlone => "fits alone".to_owned(),
        Fit::TooBig { at: smaller } => format!("{TOO_BIG}{}", at(smaller)),
    };
    match memory.download {
        Some(download) => format!("{} download · {fit}", gb(download)),
        None => fit,
    }
}

/// A row's description in the one field order, with the words to draw in the danger colour.
pub(super) fn description(
    choice: &ModelChoice,
    listed: &ListedFacts,
    width: usize,
) -> (String, Option<String>) {
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
        return (fields.join(" · "), None);
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
    fields.extend(memory.map(fit_words));
    if choice.recommended {
        fields.push("recommended".to_owned());
    }
    let danger = memory
        .filter(|memory| matches!(memory.fit, Fit::TooBig { .. }))
        .map(|_| TOO_BIG.to_owned());
    (fields.join(" · "), danger)
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
