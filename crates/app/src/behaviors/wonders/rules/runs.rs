//! The wonders one run raises on its own: the first run whose tally holds.

use argonauts_core::behaviors::Label;
use argonauts_core::behaviors::behavior_world::Wonder;
use argonauts_core::models::registry::provider::is_known;
use time::OffsetDateTime;

use super::super::tally::{Member, Tally};
use super::{About, Found, MODELS_NAMED, Records};

const ARGO_MAKERS: usize = 3;
const FLEECE_TASKS: u32 = 5;
const CLASHING_ROCKS_FAILURES: u32 = 5;
const DRAGON_NIGHT_HOURS: f32 = 6.0;
const HARPIES_FALLS: u32 = 3;
const DODONA_MAKERS: usize = 3;
const DODONA_ROUNDS: u32 = 2;
const COLOSSUS_AGENTS: u32 = 10;

/// The earliest run that `holds`, with what it found there.
fn first_run<'a, T>(
    records: &'a Records<'_>,
    holds: impl Fn(&'a Tally) -> Option<(OffsetDateTime, T)>,
) -> Option<(OffsetDateTime, T)> {
    records
        .tallies
        .values()
        .filter_map(|run| holds(&run.tally))
        .min_by_key(|(at, _)| *at)
}

/// The busiest model of each maker known, busiest first.
fn one_per_maker(crew: &[Member]) -> Vec<String> {
    let mut makers: Vec<&str> = Vec::new();
    let mut models = Vec::new();
    for member in crew {
        if is_known(&member.maker) && !makers.contains(&member.maker.as_str()) {
            makers.push(&member.maker);
            models.push(member.model.clone());
        }
    }
    models
}

fn busiest_models(crew: &[Member], most: usize) -> Vec<String> {
    crew.iter()
        .take(most)
        .map(|member| member.model.clone())
        .collect()
}

/// The Argo: your first workflow with models of three makers aboard merges its work.
pub(super) fn argo(records: &Records<'_>) -> Option<Found> {
    let (at, aboard) = first_run(records, |tally| {
        let aboard = one_per_maker(&tally.crew);
        (aboard.len() >= ARGO_MAKERS).then_some((tally.merged_at?, aboard))
    })?;
    Some(Found::new(
        Wonder::Argo,
        at,
        format!("{} makers aboard, merged", aboard.len()),
        aboard,
        About::Labels(&[&[Label::Workflow], &[Label::Subagent]]),
    ))
}

/// The Golden Fleece: a run of five tasks or more merges, every task merged on its first try.
pub(super) fn fleece(records: &Records<'_>) -> Option<Found> {
    let (at, tasks) = first_run(records, |tally| {
        let tasks = tally.first_try_tasks;
        (tasks >= FLEECE_TASKS).then_some((tally.merged_at?, tasks))
    })?;
    Some(Found::new(
        Wonder::Fleece,
        at,
        format!("{tasks} tasks in one run, each merged on its first try"),
        Vec::new(),
        About::Labels(&[&[Label::RunTests], &[Label::Build]]),
    ))
}

/// The Clashing Rocks: a task's checks fail five times in a row, and it still merges.
pub(super) fn clashing_rocks(records: &Records<'_>) -> Option<Found> {
    let (at, feat) = first_run(records, |tally| {
        let feat = tally.failing_rounds.as_ref()?;
        (feat.times >= CLASHING_ROCKS_FAILURES).then_some((feat.at, feat))
    })?;
    Some(Found::new(
        Wonder::Symplegades,
        at,
        format!(
            "checks failed {} times in a row, then it merged",
            feat.times
        ),
        feat.models.clone(),
        About::Labels(&[&[Label::RunTestsFailed]]),
    ))
}

/// The Sirens: an agent tries to remove or skip a test, the test check catches it, and the
/// task merges with every test kept.
pub(super) fn sirens(records: &Records<'_>) -> Option<Found> {
    let (at, feat) = first_run(records, |tally| {
        let feat = tally.tamper_caught.as_ref()?;
        Some((feat.at, feat))
    })?;
    Some(Found::new(
        Wonder::Sirens,
        at,
        "the test check caught it, then the task merged with every test kept".to_owned(),
        feat.models.clone(),
        About::Labels(&[&[Label::EditTestRemoved], &[Label::EditTest]]),
    ))
}

/// The Sleepless Dragon: a run works six hours of your night with nothing from you, and
/// merges.
pub(super) fn dragon(records: &Records<'_>) -> Option<Found> {
    let (at, tally) = first_run(records, |tally| {
        (tally.night_hours >= DRAGON_NIGHT_HOURS).then_some((tally.merged_at?, tally))
    })?;
    Some(Found::new(
        Wonder::Dragon,
        at,
        format!(
            "worked {} hours of your night without you, and merged",
            tally.night_hours.floor()
        ),
        busiest_models(&tally.crew, 3),
        About::SecondBusiest,
    ))
}

/// The Harpies: usage limits take one task's model three times, it falls to another model each
/// time, and it still merges.
pub(super) fn harpies(records: &Records<'_>) -> Option<Found> {
    let (at, feat) = first_run(records, |tally| {
        let feat = tally.limit_falls.as_ref()?;
        (feat.times >= HARPIES_FALLS).then_some((feat.at, feat))
    })?;
    Some(Found::new(
        Wonder::Harpies,
        at,
        format!(
            "usage limits took its model {} times, and it still merged",
            feat.times
        ),
        feat.models.iter().take(MODELS_NAMED).cloned().collect(),
        About::Labels(&[&[Label::Shell], &[Label::McpTool]]),
    ))
}

/// The Oak of Dodona: a council of three makers or more talks two rounds or more, agrees, and
/// the work it planned merges.
pub(super) fn dodona(records: &Records<'_>) -> Option<Found> {
    let (at, seats) = first_run(records, |tally| {
        let merged = tally.merged_at?;
        let council = tally.councils.iter().find(|council| {
            council.makers.len() >= DODONA_MAKERS && council.rounds >= DODONA_ROUNDS
        })?;
        Some((merged, &council.makers))
    })?;
    Some(Found::new(
        Wonder::Dodona,
        at,
        format!("a council of {} makers agreed", seats.len()),
        seats.iter().map(|seat| seat.model.clone()).collect(),
        About::Labels(&[&[Label::AskReview]]),
    ))
}

/// The Colossus of Rhodes: one workflow runs ten agents or more and merges its work.
pub(super) fn colossus(records: &Records<'_>) -> Option<Found> {
    let (at, tally) = first_run(records, |tally| {
        (tally.agents >= COLOSSUS_AGENTS).then_some((tally.merged_at?, tally))
    })?;
    Some(Found::new(
        Wonder::Colossus,
        at,
        format!("{} agents in one workflow, merged", tally.agents),
        busiest_models(&tally.crew, MODELS_NAMED),
        About::Labels(&[&[Label::Subagent], &[Label::Workflow]]),
    ))
}

/// The Lighthouse of Alexandria: work built by a model on your own machine merges after another
/// maker's model reviewed it.
pub(super) fn pharos(records: &Records<'_>) -> Option<Found> {
    let (at, feat) = first_run(records, |tally| {
        let feat = tally.local_reviewed.as_ref()?;
        Some((feat.at, feat))
    })?;
    let built = feat.models.first()?.clone();
    Some(Found::new(
        Wonder::Pharos,
        at,
        "built on your own machine, merged after another maker's review".to_owned(),
        feat.models.clone(),
        About::Model(built),
    ))
}

/// The Temple of Artemis: a run that stopped is carried on with `/resume` and merges.
pub(super) fn artemis(records: &Records<'_>) -> Option<Found> {
    let (at, ()) = first_run(records, |tally| {
        tally.resumed_and_merged.then_some((tally.merged_at?, ()))
    })?;
    Some(Found::new(
        Wonder::Artemis,
        at,
        "stopped, then /resume carried it on and it merged".to_owned(),
        Vec::new(),
        About::Labels(&[
            &[Label::RunStatus],
            &[Label::ReadRecords, Label::ListRecords],
        ]),
    ))
}
