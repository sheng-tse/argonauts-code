//! "Crew for this request": when your words set the crew for a request, the
//! crew Argonauts understood is shown before any work starts, each role with where it came
//! from: your words, your settings, the main session, or the role it follows. One question
//! then asks where to keep it.

use argonauts_core::names::WorkerRef;
use argonauts_core::settings::Settings;
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::steps::intake::InstructionKind;
use argonauts_crew::roles::{candidates, display_name};

use crate::controller::instructions::Applied;

/// The four roles a request's words can set, in the order the lines show them.
const SHOWN: [CrewRole; 4] = [
    CrewRole::Orchestrator,
    CrewRole::Workers,
    CrewRole::Reviewers,
    CrewRole::Judge,
];

/// One role as understood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CrewLine {
    pub role: CrewRole,
    pub who: String,
    pub from: String,
    /// Its seats, when your words set them.
    pub asked: Option<Vec<WorkerRef>>,
}

/// The crew a request runs with when your followed words changed who fills a role; `None`
/// when they didn't, so nothing is shown. Settings a request gets any other way, such as
/// ultracode's, aren't your words, so they show nothing.
///
/// `run` is the run's whole request layer as `key=value`, in the order it applies, when the run
/// adds seats of its own around your words, as the orchestrator's workflow a crew from solo
/// takes does. The lines then show the seats the run really has: a seat your words set counts
/// as asked only while the run keeps it, and one the run gave your main session says so.
pub(crate) fn understood(
    settings: &Settings,
    applied: &Applied,
    run: Option<&[String]>,
) -> Option<Vec<CrewLine>> {
    let followed = |kind: InstructionKind| {
        applied
            .instructions
            .iter()
            .any(|instruction| instruction.kind == kind && instruction.followed())
    };
    if !SEAT_KINDS.into_iter().any(followed) {
        return None;
    }
    let words = with_request(settings, &request_settings(applied))?;
    let asked = match run {
        Some(pairs) => with_request(settings, pairs)?,
        None => words.clone(),
    };
    let seats = |settings: &Settings, role: CrewRole| candidates(settings, role.fine_role());
    let by_words = |role: CrewRole| {
        seats(settings, role) != seats(&asked, role) && seats(&words, role) == seats(&asked, role)
    };
    if !SHOWN.into_iter().any(by_words) {
        return None;
    }
    // The orchestrator is your main session whenever it runs there; any other role only when
    // the run, not your settings, put it there.
    let main = [settings.main_agent.clone()];
    let given_to_main = |role: CrewRole| {
        seats(&asked, role) == main
            && (role == CrewRole::Orchestrator || seats(settings, role) != main)
    };
    // "All the other agents" plainly means the workers; any other role it reaches without a
    // word of its own is shown with those words, since that is the part you might not expect.
    let others = applied
        .instructions
        .iter()
        .find(|instruction| instruction.kind == InstructionKind::Others && instruction.followed());
    let lines = SHOWN
        .into_iter()
        .map(|role| {
            let follows = follows(&asked, role);
            let set_by_words = by_words(role) && follows.is_none();
            let from = if set_by_words {
                match others.filter(|_| reached_by_others(role, &followed)) {
                    Some(others) => format!("as you asked: \"{}\"", phrase(&others.says)),
                    None => "as you asked".to_owned(),
                }
            } else if let Some(followed) = follows {
                format!("follows the {}", followed.as_str())
            } else if given_to_main(role) {
                "your main session".to_owned()
            } else {
                "your settings".to_owned()
            };
            CrewLine {
                role,
                who: names(&seats(&asked, role)),
                from,
                asked: set_by_words.then(|| seats(&asked, role)),
            }
        })
        .collect();
    Some(lines)
}

/// The lines as the chat shows them, the names and sources in columns.
pub(crate) fn text(lines: &[CrewLine]) -> String {
    let role_width = lines
        .iter()
        .map(|line| line.role.as_str().len())
        .max()
        .unwrap_or(0);
    let who_width = lines
        .iter()
        .map(|line| line.who.chars().count())
        .max()
        .unwrap_or(0);
    let mut text = "Crew for this request".to_owned();
    for line in lines {
        text.push_str(&format!(
            "\n{:role_width$}   {:who_width$}   {}",
            line.role.as_str(),
            line.who,
            line.from
        ));
    }
    text
}

/// What "Keep this crew?" writes, as `(key, TOML value)`: each role your words set, under
/// `[crew]`, or under the role's own workers where your settings set those, since they beat
/// the crew's.
pub(crate) fn to_keep(settings: &Settings, lines: &[CrewLine]) -> Vec<(String, String)> {
    lines
        .iter()
        .filter_map(|line| {
            let seats = line.asked.as_ref()?;
            let fine = line.role.fine_role();
            let own = settings
                .roles
                .get(&fine)
                .is_some_and(|own| !own.workers.is_empty());
            let key = if own {
                format!("roles.{}.workers", fine.as_str())
            } else {
                format!("crew.{}", line.role.as_str())
            };
            Some((key, toml_list(seats)))
        })
        .collect()
}

/// The kinds of instruction that give agents seats.
const SEAT_KINDS: [InstructionKind; 8] = [
    InstructionKind::Lead,
    InstructionKind::Coordinate,
    InstructionKind::Build,
    InstructionKind::Review,
    InstructionKind::Judge,
    InstructionKind::Crew,
    InstructionKind::Others,
    InstructionKind::Exclude,
];

/// Whether "all the other agents" is what set `role`: no word of yours names it, and it isn't
/// the workers, which those words plainly mean.
fn reached_by_others(role: CrewRole, followed: &dyn Fn(InstructionKind) -> bool) -> bool {
    let own = match role {
        CrewRole::Orchestrator => InstructionKind::Coordinate,
        CrewRole::Reviewers => InstructionKind::Review,
        CrewRole::Judge => InstructionKind::Judge,
        CrewRole::Main | CrewRole::Workers => return false,
    };
    !followed(own)
}

/// The words that reach every other role, such as "all the other agents" out of "all the
/// other agents need to be opus".
fn phrase(says: &str) -> String {
    let words: Vec<&str> = says.split_whitespace().collect();
    let other = words
        .iter()
        .position(|word| word.to_lowercase().starts_with("other"));
    let end = match other {
        Some(at) if words[at].to_lowercase().starts_with("others") => at,
        Some(at) => (at + 1).min(words.len() - 1),
        None => return says.trim().to_owned(),
    };
    words[..=end]
        .join(" ")
        .trim_end_matches([',', '.', ';', ':'])
        .to_owned()
}

/// The role whose seats `role` takes, while it has none of its own.
fn follows(settings: &Settings, role: CrewRole) -> Option<CrewRole> {
    settings.crew.followed(role).filter(|_| {
        settings
            .roles
            .get(&role.fine_role())
            .is_none_or(|own| own.workers.is_empty())
    })
}

fn names(seats: &[WorkerRef]) -> String {
    match seats {
        [] => "nobody".to_owned(),
        seats => seats
            .iter()
            .map(display_name)
            .collect::<Vec<_>>()
            .join(", "),
    }
}

fn toml_list(seats: &[WorkerRef]) -> String {
    let items: Vec<String> = seats
        .iter()
        .map(|seat| toml::Value::String(seat.to_string()).to_string())
        .collect();
    format!("[{}]", items.join(", "))
}

/// The request layer as `key=value`, the way [`Applied::apply_to`] gives it to the run.
fn request_settings(applied: &Applied) -> Vec<String> {
    let mut pairs: Vec<String> = applied
        .settings
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    if let Some(lead) = &applied.lead {
        let lead = toml::Value::String(lead.to_string());
        pairs.push(format!("main_agent={lead}"));
    }
    pairs
}

/// `settings` with the request's own `key=value` settings on top, as the run resolves them.
/// `None` when they don't read, so nothing wrong is shown.
fn with_request(settings: &Settings, pairs: &[String]) -> Option<Settings> {
    if pairs.is_empty() {
        return Some(settings.clone());
    }
    let mut table = toml::Value::try_from(settings).ok()?;
    for pair in pairs {
        let (key, value) = pair.split_once('=')?;
        let value: toml::Value = toml::from_str::<toml::Table>(&format!("v = {}", value.trim()))
            .ok()?
            .remove("v")?;
        let mut at = &mut table;
        let keys: Vec<&str> = key.trim().split('.').collect();
        let (last, path) = keys.split_last()?;
        for key in path {
            let toml::Value::Table(inner) = at else {
                return None;
            };
            at = inner
                .entry((*key).to_owned())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        }
        let toml::Value::Table(inner) = at else {
            return None;
        };
        inner.insert((*last).to_owned(), value);
    }
    table.try_into().ok()
}

#[cfg(test)]
mod tests;
