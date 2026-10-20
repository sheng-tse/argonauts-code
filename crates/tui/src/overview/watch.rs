//! Who draws usage numbers says so each time it draws, and the runtime asks the controller
//! for them again while someone does: about once a second while work runs or the Usage tab
//! is open, and now and then while only a pinned card sits idle.
//!
//! The Status and Context tabs are kept fresh the same way: the open tab says what it shows
//! as it draws, and that is asked for again while it stays on screen.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use argonauts_core::api::UsagePeriod;
use argonauts_core::app::{ViewData, ViewRequest};

/// How often live numbers are asked for again.
const LIVE: Duration = Duration::from_secs(1);
/// How often an idle screen asks, so a reset or another window's spend still shows.
const IDLE: Duration = Duration::from_secs(30);
/// Drawn longer ago than this, nobody is watching.
const WATCHED_FOR: Duration = Duration::from_secs(3);
/// An answer that hasn't come after this long isn't coming.
const ANSWER_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, Default)]
struct Watched {
    period: UsagePeriod,
    drawn: Option<Instant>,
    live: bool,
    asked: Option<Instant>,
    tab: Option<TabWatch>,
    /// Tab refreshes sent whose answers haven't come back, with when each was sent.
    owed: Vec<(ViewRequest, Instant)>,
}

/// What the open tab shows beside the usage numbers.
#[derive(Debug)]
struct TabWatch {
    request: ViewRequest,
    every: Duration,
    drawn: Instant,
    asked: Instant,
}

/// Shared by what draws the numbers and the runtime that asks for them.
#[derive(Debug, Clone, Default)]
pub struct UsageWatch(Arc<Mutex<Watched>>);

impl UsageWatch {
    fn lock(&self) -> MutexGuard<'_, Watched> {
        // Plain data, so a panic elsewhere can't leave it half written.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Called as the numbers are drawn. `live` asks for them every second.
    pub fn drawn(&self, period: UsagePeriod, live: bool, now: Instant) {
        let mut watched = self.lock();
        // A new period's numbers are owed at once.
        if watched.period != period {
            watched.asked = None;
        }
        watched.period = period;
        watched.live = live;
        watched.drawn = Some(now);
    }

    /// Called as a tab draws what `request` answered, to have it asked again `every` so
    /// often while the tab stays on screen. The tab opened with a fresh answer, so the
    /// first ask waits a turn.
    pub fn drawn_tab(&self, request: ViewRequest, every: Duration, now: Instant) {
        let mut watched = self.lock();
        match &mut watched.tab {
            Some(tab) if tab.request == request => tab.drawn = now,
            tab => {
                *tab = Some(TabWatch {
                    request,
                    every,
                    drawn: now,
                    asked: now,
                })
            }
        }
    }

    /// The request to send now, if someone is watching and the last answer has aged.
    pub fn due(&self, now: Instant) -> Option<ViewRequest> {
        let mut watched = self.lock();
        if Self::next_usage(&watched, now).is_some_and(|at| at <= now) {
            watched.asked = Some(now);
            return Some(ViewRequest::Usage {
                period: watched.period,
            });
        }
        if Self::next_tab(&watched, now).is_none_or(|at| at > now) {
            return None;
        }
        let tab = watched.tab.as_mut()?;
        tab.asked = now;
        let request = tab.request.clone();
        watched.owed.push((request.clone(), now));
        Some(request)
    }

    /// When the next request is due, while someone is watching.
    pub fn next(&self, now: Instant) -> Option<Instant> {
        let watched = self.lock();
        [
            Self::next_usage(&watched, now),
            Self::next_tab(&watched, now),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// True when `data` answers a tab refresh sent from here, which is then settled. The tab
    /// that takes its answer says so, and an answer no view took is for a panel that has
    /// closed, so it must open nothing and print nothing.
    pub fn answered(&self, data: &ViewData, now: Instant) -> bool {
        let mut watched = self.lock();
        watched
            .owed
            .retain(|(_, sent)| now.saturating_duration_since(*sent) < ANSWER_WAIT);
        let answers = |request: &ViewRequest| {
            matches!(
                (request, data),
                (ViewRequest::Status, ViewData::Status { .. })
                    | (ViewRequest::StatusTab, ViewData::StatusTab { .. })
                    | (ViewRequest::Context, ViewData::Context { .. })
            )
        };
        match watched
            .owed
            .iter()
            .position(|(request, _)| answers(request))
        {
            Some(at) => {
                watched.owed.remove(at);
                true
            }
            None => false,
        }
    }

    fn next_usage(watched: &Watched, now: Instant) -> Option<Instant> {
        let drawn = watched.drawn?;
        if now.saturating_duration_since(drawn) > WATCHED_FOR {
            return None;
        }
        let every = if watched.live { LIVE } else { IDLE };
        Some(watched.asked.map_or(now, |asked| asked + every))
    }

    /// When the open tab is asked for again: never while it isn't drawn, or while the last
    /// ask still waits for its answer.
    fn next_tab(watched: &Watched, now: Instant) -> Option<Instant> {
        let tab = watched.tab.as_ref()?;
        let waits = |(request, sent): &(ViewRequest, Instant)| {
            *request == tab.request && now.saturating_duration_since(*sent) < ANSWER_WAIT
        };
        if now.saturating_duration_since(tab.drawn) > WATCHED_FOR || watched.owed.iter().any(waits)
        {
            return None;
        }
        Some(tab.asked + tab.every)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_asks_at_once_then_every_second_while_live_and_never_unwatched() {
        let watch = UsageWatch::default();
        let t0 = Instant::now();
        assert_eq!(watch.due(t0), None, "nobody draws them");

        watch.drawn(UsagePeriod::Today, true, t0);
        let today = ViewRequest::Usage {
            period: UsagePeriod::Today,
        };
        assert_eq!(watch.due(t0), Some(today.clone()));
        assert_eq!(watch.due(t0 + Duration::from_millis(500)), None);
        assert_eq!(watch.next(t0), Some(t0 + LIVE));
        watch.drawn(UsagePeriod::Today, true, t0 + LIVE);
        assert_eq!(watch.due(t0 + LIVE), Some(today));

        // Idle, it asks far less often.
        watch.drawn(UsagePeriod::Today, false, t0 + LIVE);
        assert_eq!(watch.due(t0 + LIVE * 2), None);

        // Another period is owed at once.
        watch.drawn(UsagePeriod::Week, false, t0 + LIVE * 2);
        assert_eq!(
            watch.due(t0 + LIVE * 2),
            Some(ViewRequest::Usage {
                period: UsagePeriod::Week
            })
        );

        // Not drawn for a while, nobody is watching.
        assert_eq!(watch.due(t0 + Duration::from_secs(60)), None);
        assert_eq!(watch.next(t0 + Duration::from_secs(60)), None);
    }

    #[test]
    fn an_open_tab_is_asked_again_while_drawn_and_one_ask_at_a_time() {
        let watch = UsageWatch::default();
        let t0 = Instant::now();
        let status = || ViewData::Status {
            summary: Default::default(),
        };
        watch.drawn_tab(ViewRequest::Status, LIVE, t0);
        assert_eq!(watch.due(t0), None, "it opened with a fresh answer");
        assert_eq!(watch.next(t0), Some(t0 + LIVE));
        watch.drawn_tab(ViewRequest::Status, LIVE, t0 + LIVE);
        assert_eq!(watch.due(t0 + LIVE), Some(ViewRequest::Status));

        // Until that answer comes nothing more is asked, however long it stays drawn.
        watch.drawn_tab(ViewRequest::Status, LIVE, t0 + LIVE * 3);
        assert_eq!(watch.due(t0 + LIVE * 3), None);
        assert_eq!(watch.next(t0 + LIVE * 3), None);
        assert!(watch.answered(&status(), t0 + LIVE * 3));
        assert!(
            !watch.answered(&status(), t0 + LIVE * 3),
            "one ask, one answer"
        );
        assert_eq!(watch.due(t0 + LIVE * 3), Some(ViewRequest::Status));

        // The panel closed: the answer still on its way is known as a refresh's.
        assert_eq!(watch.due(t0 + LIVE * 9), None);
        assert!(watch.answered(&status(), t0 + LIVE * 9));

        // An answer nobody asked for from here is somebody else's.
        let context = ViewData::Context {
            view: argonauts_core::views::ContextView::default(),
        };
        assert!(!watch.answered(&context, t0));
    }
}
