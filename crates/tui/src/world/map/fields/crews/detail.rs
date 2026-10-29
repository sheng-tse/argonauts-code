//! The made-up detail of each action, as the app keeps it: its day and time, its session and
//! its place in it, whether it failed, how its work and its session ended. Drawn from the crew's
//! seed: sessions are strung from each day's actions, mostly in the order agents usually work
//! (a read before an edit, an edit before a test run), with loops, waits and busy hours where
//! the crew's habits say.

use std::collections::BTreeMap;

use regex::Regex;

use super::Habits;
use crate::world::raster::noise::Rng;

/// What usually comes after what: an action matching the first pattern is mostly followed by
/// one matching the second.
const NEXT: &[(&str, &str)] = &[
    (
        "^read a (source|test|config)|^printed",
        "edited a source|searched code|edited a test|^read a",
    ),
    (
        "searched code",
        "^read a source|edited a source|^read a test|^printed",
    ),
    ("listed files", "^read a|searched code"),
    (
        "^ran the test suite, which failed",
        "edited a source|^read a source|edited a test|tried an edit",
    ),
    (
        "^ran the test suite",
        "checked git status|returned its|ran the test suite",
    ),
    ("edited a test|wrote a test", "^ran the test suite"),
    (
        "edited a source|wrote a source|tried an edit",
        "^ran the test suite|built the project|edited a source|linter",
    ),
    (
        "built the project|linter",
        "^ran the test suite|edited a source",
    ),
    (
        "checked git status",
        "changed git state|read git history|returned its",
    ),
    (
        "changed git state|git history",
        "returned its|checked git status",
    ),
    (
        "shell command|Python|moved, copied",
        "ran a shell|^read a|edited a",
    ),
    ("doc", "edited a doc|wrote a doc|read a doc"),
    (
        "web",
        "fetched from the web|searched the web|read a doc|wrote a doc",
    ),
    (
        "to-do|deferred tool|MCP",
        "^read a source|edited a source|searched code",
    ),
    ("review|subagent|workflow", "returned its|^read a"),
    (
        "run records|run's status",
        "read Argonauts|checked the run's|returned its",
    ),
];
const STARTER: &str = "^read a |searched code|listed files|checked git status|updated its to-do|run records|run's status|searched the web";
const LOOPY: &str = "^ran the test suite|tried an edit|ran a shell command|edited a source|ran a Python|searched code";
const ENDER: &str = "returned its structured answer|asked another agent to review";
const FAILED: &str = "which failed|didn't apply|didn't fit|failed to";

/// How a session's work ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A conversation's own turn, with no work to settle.
    None,
    Merged,
    SentBack,
    Dropped,
}

/// How a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Completed,
    Stopped,
    /// Its CLI failed.
    Failed,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub day: u32,
    /// Minutes into its day.
    pub start: f64,
    /// Its actions in order, by index.
    pub actions: Vec<usize>,
    pub outcome: Outcome,
    pub ended: Ended,
}

/// Every action's detail, by index.
#[derive(Debug, Clone)]
pub struct Detail {
    /// Days in the period: the idle days and the span.
    pub period: u32,
    /// Days back from the period's last day.
    pub day: Vec<u32>,
    /// Minutes into its day, as a 32-bit float as the record keeps it.
    pub minute: Vec<f32>,
    pub session: Vec<usize>,
    pub nth: Vec<usize>,
    pub ok: Vec<bool>,
    pub outcome: Vec<Outcome>,
    /// Minutes from the start of the period's first day.
    pub time: Vec<f32>,
    pub sessions: Vec<Session>,
    pub quiet_days: u32,
    /// Each stretch of 2 or more quiet days in a row: days back to its latest, and its length.
    pub quiet_stretches: Vec<(u32, u32)>,
}

/// Labels left to string into sessions on one day, in the order each first came, each with its
/// actions; an action is taken from the end.
struct Pool {
    labels: Vec<(usize, Vec<usize>)>,
    left: usize,
}

impl Pool {
    fn has(&self, label: usize) -> bool {
        self.labels.iter().any(|(l, _)| *l == label)
    }

    fn size(&self, label: usize) -> usize {
        self.labels
            .iter()
            .find(|(l, _)| *l == label)
            .map_or(0, |(_, actions)| actions.len())
    }

    fn keys(&self) -> Vec<usize> {
        self.labels.iter().map(|(l, _)| *l).collect()
    }

    fn take(&mut self, label: usize) -> usize {
        let at = self
            .labels
            .iter()
            .position(|(l, _)| *l == label)
            .expect("a label in the pool");
        let action = self.labels[at].1.pop().expect("a label with actions");
        if self.labels[at].1.is_empty() {
            self.labels.remove(at);
        }
        self.left -= 1;
        action
    }

    /// One of `labels`, each as likely as it has actions left.
    fn weighted(&self, labels: &[usize], rng: &mut Rng) -> usize {
        let total: usize = labels.iter().map(|l| self.size(*l)).sum();
        let mut x = rng.next_f64() * total as f64;
        for label in labels {
            x -= self.size(*label) as f64;
            if x <= 0.0 {
                return *label;
            }
        }
        *labels.last().expect("labels to choose from")
    }
}

impl Detail {
    pub fn draw(labels: &[String], l: &[usize], habits: &Habits) -> Detail {
        let n = l.len();
        let period = habits.idle + habits.span;
        let seed = habits.seed;
        let mut r = Rng::new(seed.wrapping_mul(977).wrapping_add(13));
        let mut waits = Rng::new(seed.wrapping_mul(31).wrapping_add(7));
        let quiet: Vec<u32> = habits
            .quiet
            .iter()
            .copied()
            .filter(|day| *day >= habits.idle && *day < period)
            .collect();
        let active: Vec<u32> = (habits.idle..period)
            .filter(|day| !quiet.contains(day))
            .collect();
        let pick_day = |from: u32, to: u32, r: &mut Rng| {
            let within: Vec<u32> = active
                .iter()
                .copied()
                .filter(|day| *day >= from && *day <= to)
                .collect();
            let list = if within.is_empty() { &active } else { &within };
            let weight = |day: u32| 1.0 / (1.0 + f64::from(day - habits.idle) / 10.0);
            let total: f64 = list.iter().map(|day| weight(*day)).sum();
            let mut x = r.next_f64() * total;
            for day in list {
                x -= weight(*day);
                if x <= 0.0 {
                    return *day;
                }
            }
            *list.last().expect("an active day")
        };
        let crunch = habits
            .crunch
            .as_ref()
            .filter(|crunch| active.contains(&crunch.day));
        let mut day = vec![0_u32; n];
        for (at, label) in l.iter().enumerate() {
            let crowded = crunch.and_then(|crunch| {
                crunch
                    .labels
                    .iter()
                    .find(|(l, _)| l == label)
                    .map(|(_, share)| (crunch.day, *share))
            });
            let era = habits.era.iter().find(|(l, _, _)| l == label);
            day[at] = match crowded {
                Some((busy, share)) if r.next_f64() < share => busy,
                _ => match era {
                    Some(&(_, from, to)) => pick_day(from, to, &mut r),
                    None => pick_day(habits.idle, period - 1, &mut r),
                },
            };
        }

        let matcher = |pattern: &str| Regex::new(pattern).expect("a pattern");
        let starter = matcher(STARTER);
        let loopy = matcher(LOOPY);
        let ender = matcher(ENDER);
        let next: Vec<Vec<usize>> = labels
            .iter()
            .map(|words| {
                NEXT.iter()
                    .find(|(from, _)| matcher(from).is_match(words))
                    .map(|(_, to)| {
                        let to = matcher(to);
                        (0..labels.len())
                            .filter(|k| to.is_match(&labels[*k]))
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .collect();
        let bog = habits
            .bog
            .map(|(pattern, chance)| (matcher(pattern), chance));
        let sent = habits.sent.map(matcher);
        let dropped = habits.dropped.map(matcher);

        let mut session = vec![0_usize; n];
        let mut nth = vec![0_usize; n];
        let mut minute = vec![0.0_f32; n];
        let mut sessions: Vec<Session> = Vec::new();
        let mut by_day: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        for (at, d) in day.iter().enumerate() {
            by_day.entry(*d).or_default().push(at);
        }
        for (&d, actions) in by_day.iter().rev() {
            let mut pool = Pool {
                labels: Vec::new(),
                left: actions.len(),
            };
            for &at in actions {
                match pool.labels.iter_mut().find(|(label, _)| *label == l[at]) {
                    Some((_, list)) => list.push(at),
                    None => pool.labels.push((l[at], vec![at])),
                }
            }
            let mut strung = 0;
            while pool.left > 0 {
                let busy = crunch.filter(|crunch| crunch.day == d);
                let fan = busy.filter(|crunch| strung < crunch.fan);
                let len = 6 + (r.next_f64() * 20.0) as usize;
                let mut seq: Vec<usize> = Vec::new();
                let mut cur = match fan {
                    Some(crunch) if pool.has(crunch.fan_label) => crunch.fan_label,
                    _ => {
                        let starters: Vec<usize> = pool
                            .keys()
                            .into_iter()
                            .filter(|label| starter.is_match(&labels[*label]))
                            .collect();
                        if !starters.is_empty() && r.next_f64() < 0.8 {
                            pool.weighted(&starters, &mut r)
                        } else {
                            pool.weighted(&pool.keys(), &mut r)
                        }
                    }
                };
                while seq.len() < len && pool.left > 0 {
                    if !pool.has(cur) {
                        cur = pool.weighted(&pool.keys(), &mut r);
                    }
                    seq.push(pool.take(cur));
                    if loopy.is_match(&labels[cur])
                        && pool.size(cur) >= 4
                        && r.next_f64() < habits.loops * 0.3
                    {
                        let run = 4 + (r.next_f64() * 4.0) as usize;
                        for _ in 0..run {
                            if !pool.has(cur) {
                                break;
                            }
                            seq.push(pool.take(cur));
                        }
                    }
                    if pool.left == 0 {
                        break;
                    }
                    if seq.len() + 1 >= len && r.next_f64() < 0.6 {
                        let enders: Vec<usize> = pool
                            .keys()
                            .into_iter()
                            .filter(|label| ender.is_match(&labels[*label]))
                            .collect();
                        if !enders.is_empty() {
                            let last = pool.weighted(&enders, &mut r);
                            seq.push(pool.take(last));
                            break;
                        }
                    }
                    let onward: Vec<usize> = next[cur]
                        .iter()
                        .copied()
                        .filter(|label| pool.has(*label) && *label != cur)
                        .collect();
                    cur = if !onward.is_empty() && r.next_f64() < 0.78 {
                        pool.weighted(&onward, &mut r)
                    } else {
                        pool.weighted(&pool.keys(), &mut r)
                    };
                }
                let start = match (busy, fan) {
                    (Some(_), Some(_)) => 14.0 * 60.0 + r.next_f64() * 4.0,
                    (Some(_), None) => 14.0 * 60.0 + r.next_f64() * 50.0,
                    _ if r.next_f64() < habits.night => 22.0 * 60.0 + r.next_f64() * 6.5 * 60.0,
                    _ => 9.0 * 60.0 + r.next_f64() * 10.0 * 60.0,
                };
                let mut m = start;
                for (q, &at) in seq.iter().enumerate() {
                    if q > 0 {
                        m += if l[seq[q - 1]] == l[at] {
                            0.15 + r.next_f64() * 0.3
                        } else {
                            0.25 + r.next_f64() * 1.3
                        };
                        // A wait before an action draws from its own stream, so the rest of
                        // the record stays as it was.
                        if let Some((bogged, chance)) = &bog
                            && bogged.is_match(&labels[l[at]])
                            && waits.next_f64() < *chance
                        {
                            m += 10.0 + waits.next_f64() * 30.0;
                        }
                    }
                    minute[at] = m as f32;
                    session[at] = sessions.len();
                    nth[at] = q;
                }
                let share = |pattern: &Option<Regex>| match pattern {
                    Some(pattern) => {
                        seq.iter()
                            .filter(|at| pattern.is_match(&labels[l[**at]]))
                            .count() as f64
                            / seq.len() as f64
                    }
                    None => 0.0,
                };
                let p_sent = (0.05 + habits.sent_k * share(&sent)).min(0.85);
                let p_dropped = (0.04 + habits.dropped_k * share(&dropped)).min(0.85 - p_sent);
                let conversation = r.next_f64() < 0.2;
                let x = r.next_f64();
                let outcome = if conversation {
                    Outcome::None
                } else if x < p_sent {
                    Outcome::SentBack
                } else if x < p_sent + p_dropped {
                    Outcome::Dropped
                } else {
                    Outcome::Merged
                };
                let y = r.next_f64();
                let ended = if y < habits.crash {
                    Ended::Failed
                } else if y < habits.crash + habits.stop {
                    Ended::Stopped
                } else {
                    Ended::Completed
                };
                sessions.push(Session {
                    day: d,
                    start,
                    actions: seq,
                    outcome,
                    ended,
                });
                strung += 1;
            }
        }

        let failed = matcher(FAILED);
        let extra = habits
            .fail
            .map(|(pattern, chance)| (matcher(pattern), chance));
        let mut ok = vec![true; n];
        let mut outcome = vec![Outcome::None; n];
        let mut time = vec![0.0_f32; n];
        for at in 0..n {
            let words = &labels[l[at]];
            let chance = match &extra {
                Some((pattern, chance)) if pattern.is_match(words) => *chance,
                _ => 0.012,
            };
            ok[at] = !(failed.is_match(words) || r.next_f64() < chance);
            outcome[at] = sessions[session[at]].outcome;
            time[at] = (f64::from(period - 1 - day[at]) * 1440.0 + f64::from(minute[at])) as f32;
        }
        let mut quiet_stretches = Vec::new();
        let mut i = 0;
        while i < quiet.len() {
            let mut j = i;
            while j + 1 < quiet.len() && quiet[j + 1] == quiet[j] + 1 {
                j += 1;
            }
            if j > i {
                quiet_stretches.push((quiet[i], (j - i + 1) as u32));
            }
            i = j + 1;
        }
        Detail {
            period,
            day,
            minute,
            session,
            nth,
            ok,
            outcome,
            time,
            sessions,
            quiet_days: quiet.len() as u32,
            quiet_stretches,
        }
    }

    /// A label's actions that came 10 minutes or more after the one before in its session.
    pub fn waited(&self, l: &[usize], label: usize) -> u32 {
        let mut waited = 0;
        for session in &self.sessions {
            for pair in session.actions.windows(2) {
                if l[pair[1]] == label && self.minute[pair[1]] - self.minute[pair[0]] >= 10.0 {
                    waited += 1;
                }
            }
        }
        waited
    }

    /// How often agents went from one label straight to another, a run of one label counting
    /// once.
    pub fn moves(&self, l: &[usize]) -> Vec<((usize, usize), u32)> {
        let mut moves: Vec<((usize, usize), u32)> = Vec::new();
        for session in &self.sessions {
            let actions = &session.actions;
            let mut q = 0;
            while q < actions.len() {
                let mut e = q;
                while e + 1 < actions.len() && l[actions[e + 1]] == l[actions[q]] {
                    e += 1;
                }
                if e + 1 < actions.len() {
                    let key = (l[actions[q]], l[actions[e + 1]]);
                    match moves.iter_mut().find(|(k, _)| *k == key) {
                        Some((_, count)) => *count += 1,
                        None => moves.push((key, 1)),
                    }
                }
                q = e + 1;
            }
        }
        moves
    }
}
