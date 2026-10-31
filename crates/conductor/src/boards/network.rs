//! Whether the internet is there at all: one state for the process, which the health board
//! keeps. A network error makes the board ask, at most once every 10 s: a 3 s TCP connect to
//! the providers' API hosts, with nothing sent and never to this machine, or, where
//! `HTTPS_PROXY` names a proxy, an HTTP `CONNECT` to those hosts through it, wherever the
//! proxy is. An error that comes sooner gets one more check once the 10 s are up, so
//! every error has a check after it. When none of them answers the network is down, and the
//! board asks again after 5 s, then less and less often, up to every 30 s. Any answer, or any
//! session that finished its work on a provider, says it's up.

use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use argonauts_core::base::clock::{Clock, SystemClock};
use argonauts_core::base::future::BoxFuture;
use argonauts_core::models::limits::NetState;
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::Instant;

/// The providers' API hosts the crew's CLIs call, whose answer says the internet is there.
pub const PROVIDER_HOSTS: [&str; 3] = [
    "api.anthropic.com:443",
    "api.openai.com:443",
    "generativelanguage.googleapis.com:443",
];

/// The hosts to connect to in place of the providers' and the proxy, as `host:port` with commas
/// between, such as a host your network lets through, or `off` for no check, so every network
/// error counts against its agent, as before there was one. Tests turn it off, so nothing they
/// start reaches out.
pub const CHECK_ENV: &str = "ARGONAUTS_NETWORK_CHECK";

/// Set for the rest of a process that must never reach past the machine, whatever its
/// environment says.
static OFF_IN_THIS_PROCESS: AtomicBool = AtomicBool::new(false);

/// Turns the check off for the rest of this process. The test kit does, so code a test runs in
/// its own process never checks the internet.
pub fn turn_off_in_this_process() {
    OFF_IN_THIS_PROCESS.store(true, Ordering::Relaxed);
}

/// Whether [`CHECK_ENV`], or [`turn_off_in_this_process`], turns the check off.
pub fn check_is_off() -> bool {
    OFF_IN_THIS_PROCESS.load(Ordering::Relaxed)
        || std::env::var(CHECK_ENV).is_ok_and(|value| turns_it_off(&value))
}

fn turns_it_off(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("off")
}

const CONNECT_WITHIN: Duration = Duration::from_secs(3);

/// How often the network is asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pace {
    /// While the internet is up, a burst of network errors asks once in this long.
    pub at_most_every: Duration,
    /// While it's down, the first wait before asking again, doubled each time.
    pub first_again: Duration,
    pub longest_again: Duration,
}

impl Default for Pace {
    fn default() -> Self {
        Self {
            at_most_every: Duration::from_secs(10),
            first_again: Duration::from_secs(5),
            longest_again: Duration::from_secs(30),
        }
    }
}

impl Pace {
    /// The waits between asks while the network is down.
    pub fn waits(self) -> impl Iterator<Item = Duration> {
        std::iter::successors(Some(self.first_again), move |wait| {
            Some((*wait * 2).min(self.longest_again))
        })
    }
}

/// Asks whether anything out there answers.
pub trait Probe: Send + Sync {
    /// Whether anything answered. With nothing it may ask, it can't say the internet is gone,
    /// so that is `true` too.
    fn answers(&self) -> BoxFuture<'_, bool>;
}

/// Where the check connects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A TCP connect to `host:port`, never to this machine, with nothing sent.
    Direct(String),
    /// An HTTP `CONNECT` to `host` through the proxy at `proxy`. A proxy on this machine, as
    /// cntlm is, may be the only way out, so it's asked too: what it answers is about `host`.
    Through { proxy: String, host: String },
}

/// Connects to each target at once.
pub struct TcpProbe {
    targets: Vec<Target>,
    within: Duration,
}

impl TcpProbe {
    /// The hosts [`CHECK_ENV`] names, else the providers' hosts, through the proxy your
    /// environment names where it names one.
    pub fn of_providers() -> Self {
        let set = |key: &str| {
            std::env::var(key)
                .ok()
                .filter(|value| !value.trim().is_empty())
        };
        if let Some(named) = set(CHECK_ENV) {
            return Self::to(named_hosts(&named));
        }
        let proxy = ["HTTPS_PROXY", "https_proxy"].into_iter().find_map(set);
        Self::at(targets(proxy.as_deref()))
    }

    /// `host:port` targets, each connected to directly.
    pub fn to(targets: Vec<String>) -> Self {
        Self::at(targets.into_iter().map(Target::Direct).collect())
    }

    pub fn at(targets: Vec<Target>) -> Self {
        Self {
            targets,
            within: CONNECT_WITHIN,
        }
    }
}

impl Probe for TcpProbe {
    fn answers(&self) -> BoxFuture<'_, bool> {
        Box::pin(async move {
            let mut tries = JoinSet::new();
            for target in &self.targets {
                tries.spawn(reach(target.clone()));
            }
            let heard = tokio::time::timeout(self.within, async {
                let mut asked = false;
                while let Some(reached) = tries.join_next().await {
                    match reached {
                        Ok(Reached::Answered) => return true,
                        Ok(Reached::OnlyThisMachine) => {}
                        Ok(Reached::NoAnswer) | Err(_) => asked = true,
                    }
                }
                !asked
            })
            .await;
            heard.unwrap_or(false)
        })
    }
}

enum Reached {
    Answered,
    NoAnswer,
    /// It names this machine, whose answer says nothing of the internet.
    OnlyThisMachine,
}

async fn reach(target: Target) -> Reached {
    match target {
        Target::Direct(target) => reach_directly(target).await,
        Target::Through { proxy, host } => reach_through(&proxy, &host).await,
    }
}

async fn reach_directly(target: String) -> Reached {
    if host_of(&target).is_some_and(names_this_machine) {
        return Reached::OnlyThisMachine;
    }
    let Ok(addresses) = tokio::net::lookup_host(target.as_str()).await else {
        return Reached::NoAnswer;
    };
    let elsewhere: Vec<_> = addresses
        .filter(|address| !address.ip().is_loopback() && !address.ip().is_unspecified())
        .collect();
    if elsewhere.is_empty() {
        return Reached::OnlyThisMachine;
    }
    for address in elsewhere {
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            return Reached::Answered;
        }
    }
    Reached::NoAnswer
}

/// Asks the proxy to open a tunnel to `host`, and reads only its answer's status. A tunnel it
/// opened, or a proxy that wants its sign-in first, answered: the proxy is there to carry the
/// CLIs' calls. Its own failure to reach `host`, such as a 502, is no answer.
async fn reach_through(proxy: &str, host: &str) -> Reached {
    let Ok(mut stream) = tokio::net::TcpStream::connect(proxy).await else {
        return Reached::NoAnswer;
    };
    let ask = format!("CONNECT {host} HTTP/1.1\r\nHost: {host}\r\n\r\n");
    if stream.write_all(ask.as_bytes()).await.is_err() {
        return Reached::NoAnswer;
    }
    let mut heard = Vec::new();
    let mut buffer = [0u8; 256];
    while !heard.windows(2).any(|pair| pair == b"\r\n") && heard.len() < 1_024 {
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => heard.extend_from_slice(&buffer[..read]),
        }
    }
    match status_of(&heard) {
        Some(200..=299 | 407) => Reached::Answered,
        _ => Reached::NoAnswer,
    }
}

/// The status code on an HTTP answer's first line, such as 200 in `HTTP/1.1 200 OK`.
fn status_of(answer: &[u8]) -> Option<u16> {
    let line = answer.split(|byte| *byte == b'\n').next()?;
    let line = std::str::from_utf8(line).ok()?;
    let mut words = line.split_whitespace();
    words
        .next()
        .filter(|version| version.starts_with("HTTP/"))?;
    words.next()?.parse().ok()
}

/// The `host:port` targets a [`CHECK_ENV`] value names.
fn named_hosts(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(str::to_owned)
        .collect()
}

/// What to connect to: the providers' hosts, through the proxy when one is set.
pub fn targets(proxy: Option<&str>) -> Vec<Target> {
    let proxy = proxy.and_then(proxy_target);
    PROVIDER_HOSTS
        .iter()
        .map(|host| match &proxy {
            Some(proxy) => Target::Through {
                proxy: proxy.clone(),
                host: (*host).to_owned(),
            },
            None => Target::Direct((*host).to_owned()),
        })
        .collect()
}

/// `http://user:secret@proxy.example.com:3128/` as `proxy.example.com:3128`, with the scheme's
/// port when it names none.
fn proxy_target(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://").unwrap_or(("http", url));
    let authority = rest.split(['/', '?', '#']).next()?;
    let address = authority.rsplit_once('@').map_or(authority, |(_, at)| at);
    if address.is_empty() {
        return None;
    }
    let has_port = match address.rsplit_once(':') {
        Some((host, port)) => !port.is_empty() && (!host.contains(':') || host.ends_with(']')),
        None => false,
    };
    if has_port {
        return Some(address.to_owned());
    }
    let port = if scheme.eq_ignore_ascii_case("https") {
        443
    } else {
        80
    };
    Some(format!("{address}:{port}"))
}

fn host_of(target: &str) -> Option<&str> {
    let (host, _) = target.rsplit_once(':')?;
    Some(host.trim_start_matches('[').trim_end_matches(']'))
}

fn names_this_machine(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified())
}

/// What the network check knows, for the health board's rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Internet {
    /// Nothing checks, so every network error counts against its agent, as it always did.
    #[default]
    Unchecked,
    /// Up, last found so at `found_at`, or never yet.
    Up {
        found_at: Option<OffsetDateTime>,
    },
    Down,
}

impl Internet {
    pub fn is_down(self) -> bool {
        self == Self::Down
    }

    /// Whether a network error seen at `at` says something about its agent: only once the
    /// internet was found up after it, so an outage of the internet itself blames nobody.
    pub fn blames(self, at: OffsetDateTime) -> bool {
        match self {
            Self::Unchecked => true,
            Self::Up { found_at } => found_at.is_some_and(|found| found >= at),
            Self::Down => false,
        }
    }
}

/// The process's one network state.
pub struct Network {
    probe: Option<Arc<dyn Probe>>,
    pace: Pace,
    clock: Arc<dyn Clock>,
    state: watch::Sender<NetState>,
    asking: Mutex<Asking>,
    /// Counts finished checks, for whoever waits on the one under way.
    checked: watch::Sender<u64>,
}

#[derive(Default)]
struct Asking {
    began: Option<Instant>,
    running: bool,
    /// The loop that asks again while it's down runs.
    again: bool,
    /// A network error came too soon after the last check to ask at once, so one more check
    /// waits for its turn.
    due: bool,
    found_up_at: Option<OffsetDateTime>,
}

/// When a network error makes the board ask.
enum Turn {
    Now,
    /// Once this has passed, since a check began lately.
    After(Duration),
    /// A check under way or waiting for its turn, or the loop that asks while it's down,
    /// answers for this error too.
    Covered,
}

impl Network {
    /// Never checks and always says up.
    pub fn unchecked() -> Self {
        Self::with_probe(None)
    }

    pub fn probing(probe: Arc<dyn Probe>) -> Self {
        Self::with_probe(Some(probe))
    }

    fn with_probe(probe: Option<Arc<dyn Probe>>) -> Self {
        Self {
            probe,
            pace: Pace::default(),
            clock: Arc::new(SystemClock),
            state: watch::Sender::new(NetState::Up),
            asking: Mutex::default(),
            checked: watch::Sender::new(0),
        }
    }

    /// Dates its changes by `clock`, as the health board dates its marks.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.set_clock(clock);
        self
    }

    pub fn set_clock(&mut self, clock: Arc<dyn Clock>) {
        self.clock = clock;
    }

    pub fn with_pace(mut self, pace: Pace) -> Self {
        self.pace = pace;
        self
    }

    pub fn state(&self) -> NetState {
        *self.state.borrow()
    }

    pub fn changes(&self) -> watch::Receiver<NetState> {
        self.state.subscribe()
    }

    pub fn internet(&self) -> Internet {
        if self.probe.is_none() {
            return Internet::Unchecked;
        }
        match self.state() {
            NetState::Down { .. } => Internet::Down,
            NetState::Up => Internet::Up {
                found_at: self.asking().found_up_at,
            },
        }
    }

    /// A session met a network error: asks whether the internet is there, unless a check
    /// under way or waiting for its turn, or the loop that asks while it's down, answers for
    /// it. Within 10 s of the last check, the next one waits for the 10 s to pass, so a burst
    /// of errors still has a check after its last one.
    pub fn suspect(self: &Arc<Self>) {
        let Some(probe) = self.probe.clone() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let wait = match self.take_turn() {
            Turn::Covered => return,
            Turn::Now => None,
            Turn::After(wait) => Some(wait),
        };
        let network = Arc::clone(self);
        runtime.spawn(async move {
            if let Some(wait) = wait {
                tokio::time::sleep(wait).await;
                if !network.take_the_due_turn() {
                    return;
                }
            }
            let answered = probe.answers().await;
            network.found(answered);
        });
    }

    /// A session finished its work on a provider, which only the internet makes possible.
    pub fn answered(&self) {
        self.asking().found_up_at = Some(self.clock.now());
        self.state.send_if_modified(|state| {
            let changed = state.is_down();
            *state = NetState::Up;
            changed
        });
    }

    /// The network as the latest check found it, once the check under way and the one
    /// waiting for its turn, if any, have said. Only a network error asks, so a session that
    /// ended some other way, such as a crash, never makes this reach out.
    pub async fn settled(&self) -> NetState {
        let mut checked = self.checked.subscribe();
        while self.unsettled() {
            if checked.changed().await.is_err() {
                break;
            }
        }
        self.state()
    }

    fn unsettled(&self) -> bool {
        let asking = self.asking();
        asking.running || asking.due
    }

    /// When this caller asks, and takes the turn if it's now: nobody is asking or waiting to,
    /// the loop that asks while it's down isn't running, and no check began lately.
    fn take_turn(&self) -> Turn {
        let mut asking = self.asking();
        if asking.running || asking.due || asking.again {
            return Turn::Covered;
        }
        match asking.began.map(|began| began.elapsed()) {
            Some(since) if since < self.pace.at_most_every => {
                asking.due = true;
                Turn::After(self.pace.at_most_every - since)
            }
            _ => {
                asking.running = true;
                asking.began = Some(Instant::now());
                Turn::Now
            }
        }
    }

    /// The check that waited for its turn begins, unless one that began since, or the loop
    /// that asks while it's down, answers for it.
    fn take_the_due_turn(&self) -> bool {
        let mut asking = self.asking();
        asking.due = false;
        if asking.running || asking.again {
            drop(asking);
            // Whoever waits for the checks to settle looks again.
            self.checked.send_modify(|_| {});
            return false;
        }
        asking.running = true;
        asking.began = Some(Instant::now());
        true
    }

    fn found(self: &Arc<Self>, answered: bool) {
        let now = self.clock.now();
        let ask_again = {
            let mut asking = self.asking();
            asking.running = false;
            if answered {
                asking.found_up_at = Some(now);
            }
            let ask_again = !answered && !asking.again;
            if ask_again {
                asking.again = true;
            }
            ask_again
        };
        self.state
            .send_if_modified(|state| match (answered, *state) {
                (true, NetState::Down { .. }) => {
                    *state = NetState::Up;
                    true
                }
                (false, NetState::Up) => {
                    *state = NetState::Down { since: now };
                    true
                }
                _ => false,
            });
        self.checked.send_modify(|count| *count += 1);
        if ask_again && let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(ask_again_while_down(Arc::downgrade(self), self.pace));
        }
    }

    fn asking(&self) -> MutexGuard<'_, Asking> {
        self.asking.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Asks again while the network is down, waiting longer each time, until it's back or the
/// board is gone.
async fn ask_again_while_down(network: Weak<Network>, pace: Pace) {
    for wait in pace.waits() {
        tokio::time::sleep(wait).await;
        let Some(network) = network.upgrade() else {
            return;
        };
        let probe = match (&network.probe, network.state()) {
            (Some(probe), NetState::Down { .. }) => probe.clone(),
            _ => {
                network.asking().again = false;
                return;
            }
        };
        {
            let mut asking = network.asking();
            asking.running = true;
            asking.began = Some(Instant::now());
        }
        let answered = probe.answers().await;
        if answered {
            network.asking().again = false;
        }
        network.found(answered);
        if answered {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Answers from a script, then the last one again, and counts how often it was asked.
    #[derive(Default)]
    struct Scripted {
        answers: Mutex<VecDeque<bool>>,
        asked: AtomicUsize,
    }

    impl Scripted {
        fn answering(answers: &[bool]) -> Arc<Self> {
            Arc::new(Self {
                answers: Mutex::new(answers.iter().copied().collect()),
                asked: AtomicUsize::new(0),
            })
        }

        fn asked(&self) -> usize {
            self.asked.load(Ordering::SeqCst)
        }
    }

    impl Probe for Scripted {
        fn answers(&self) -> BoxFuture<'_, bool> {
            self.asked.fetch_add(1, Ordering::SeqCst);
            let mut answers = self.answers.lock().unwrap();
            let answer = if answers.len() > 1 {
                answers.pop_front().unwrap()
            } else {
                answers.front().copied().unwrap_or(true)
            };
            Box::pin(async move { answer })
        }
    }

    /// Asks again within milliseconds, so a test waits for real.
    fn quick() -> Pace {
        Pace {
            at_most_every: Duration::from_secs(60),
            first_again: Duration::from_millis(20),
            longest_again: Duration::from_millis(40),
        }
    }

    async fn next_change(changes: &mut watch::Receiver<NetState>) -> NetState {
        tokio::time::timeout(Duration::from_secs(10), changes.changed())
            .await
            .expect("the network state changed")
            .unwrap();
        *changes.borrow_and_update()
    }

    #[tokio::test]
    async fn a_failed_probe_marks_the_network_down() {
        let probe = Scripted::answering(&[false, false, true]);
        let network = Arc::new(Network::probing(probe.clone()).with_pace(quick()));
        let mut changes = network.changes();
        network.suspect();
        assert!(next_change(&mut changes).await.is_down());
        assert_eq!(network.internet(), Internet::Down);
        // A burst of errors asks once; the loop that asks again takes it from here.
        network.suspect();
        network.suspect();
        assert_eq!(next_change(&mut changes).await, NetState::Up);
        assert_eq!(probe.asked(), 3);
        assert!(matches!(
            network.internet(),
            Internet::Up { found_at: Some(_) }
        ));
    }

    #[test]
    fn it_asks_again_less_and_less_often() {
        let waits: Vec<u64> = Pace::default()
            .waits()
            .take(5)
            .map(|wait| wait.as_secs())
            .collect();
        assert_eq!(waits, [5, 10, 20, 30, 30]);
    }

    #[tokio::test]
    async fn any_success_brings_it_back() {
        let probe = Scripted::answering(&[false]);
        let network = Arc::new(Network::probing(probe.clone()).with_pace(quick()));
        let mut changes = network.changes();
        network.suspect();
        assert!(next_change(&mut changes).await.is_down());
        network.answered();
        assert_eq!(network.state(), NetState::Up);
        // Back up, nothing asks again.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(probe.asked(), 1);
        assert_eq!(network.state(), NetState::Up);
    }

    #[tokio::test]
    async fn a_check_waits_for_the_one_under_way() {
        let probe = Scripted::answering(&[false]);
        let network = Arc::new(Network::probing(probe.clone()));
        network.suspect();
        assert!(network.settled().await.is_down());
        assert_eq!(probe.asked(), 1);
        // Down already: the loop that asks again says when it's back.
        assert!(network.settled().await.is_down());
        assert_eq!(probe.asked(), 1);
    }

    #[tokio::test]
    async fn an_error_soon_after_a_check_gets_one_more_once_the_pace_allows() {
        let probe = Scripted::answering(&[true]);
        let pace = Pace {
            at_most_every: Duration::from_millis(300),
            ..quick()
        };
        let network = Arc::new(Network::probing(probe.clone()).with_pace(pace));
        network.suspect();
        assert_eq!(network.settled().await, NetState::Up);
        assert_eq!(probe.asked(), 1);
        // Two more within the pace: one more check, after them both.
        network.suspect();
        network.suspect();
        assert_eq!(network.settled().await, NetState::Up);
        assert_eq!(probe.asked(), 2);
    }

    #[tokio::test]
    async fn an_unchecked_network_never_asks_and_is_up() {
        let network = Arc::new(Network::unchecked());
        network.suspect();
        assert_eq!(network.settled().await, NetState::Up);
        assert_eq!(network.internet(), Internet::Unchecked);
    }

    #[tokio::test]
    async fn nothing_asks_without_a_network_error() {
        let probe = Scripted::answering(&[false]);
        let network = Arc::new(Network::probing(probe.clone()));
        assert_eq!(network.settled().await, NetState::Up);
        assert_eq!(probe.asked(), 0);
    }

    #[tokio::test]
    async fn loopback_is_never_probed() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let probe = TcpProbe::to(vec![
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            format!("[::1]:{port}"),
        ]);
        // Nothing it may ask, so it can't say the internet is gone.
        assert!(probe.answers().await);
        let accepted = tokio::time::timeout(Duration::from_millis(200), listener.accept()).await;
        assert!(accepted.is_err(), "the probe connected to this machine");
    }

    /// Where the providers' hosts are asked through a proxy, by its address.
    fn proxies(proxy: &str) -> Vec<String> {
        targets(Some(proxy))
            .into_iter()
            .map(|target| match target {
                Target::Through { proxy, host } => {
                    assert!(PROVIDER_HOSTS.contains(&host.as_str()), "{host}");
                    proxy
                }
                Target::Direct(host) => panic!("{host} went around the proxy"),
            })
            .collect()
    }

    #[test]
    fn the_providers_are_asked_through_the_proxy_wherever_it_is() {
        let three = |proxy: &str| vec![proxy.to_owned(); PROVIDER_HOSTS.len()];
        assert_eq!(
            proxies("http://user:secret@proxy.example.com:3128/"),
            three("proxy.example.com:3128")
        );
        assert_eq!(
            proxies("https://proxy.example.com"),
            three("proxy.example.com:443")
        );
        assert_eq!(proxies("proxy.example.com"), three("proxy.example.com:80"));
        assert_eq!(
            proxies("http://[2001:db8::1]:8080"),
            three("[2001:db8::1]:8080")
        );
        // A proxy on this machine may be the only way out.
        assert_eq!(proxies("http://127.0.0.1:3128"), three("127.0.0.1:3128"));
        let direct: Vec<Target> = PROVIDER_HOSTS
            .iter()
            .map(|host| Target::Direct((*host).to_owned()))
            .collect();
        assert_eq!(targets(None), direct);
    }

    /// A proxy on this machine that says `answer` to a tunnel's request.
    async fn proxy_saying(answer: &'static str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let asked = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 512];
            let read = stream.read(&mut request).await.unwrap();
            stream.write_all(answer.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&request[..read]).into_owned()
        });
        (address, asked)
    }

    #[tokio::test]
    async fn a_tunnel_through_the_proxy_says_the_internet_is_there() {
        let (proxy, asked) = proxy_saying("HTTP/1.1 200 Connection established\r\n\r\n").await;
        let probe = TcpProbe::at(vec![Target::Through {
            proxy,
            host: "api.anthropic.com:443".to_owned(),
        }]);
        assert!(probe.answers().await);
        let request = asked.await.unwrap();
        assert!(
            request.starts_with("CONNECT api.anthropic.com:443 HTTP/1.1\r\n"),
            "{request}"
        );
    }

    #[tokio::test]
    async fn a_proxy_that_cant_reach_the_host_or_isnt_there_says_it_is_gone() {
        let (proxy, _asked) = proxy_saying("HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
        let through = |proxy: String| {
            TcpProbe::at(vec![Target::Through {
                proxy,
                host: "api.openai.com:443".to_owned(),
            }])
        };
        assert!(!through(proxy).answers().await);
        // Nothing listens there.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = closed.local_addr().unwrap().to_string();
        drop(closed);
        assert!(!through(address).answers().await);
    }

    #[test]
    fn an_answers_status_is_read_from_its_first_line() {
        assert_eq!(
            status_of(b"HTTP/1.1 200 Connection established\r\n"),
            Some(200)
        );
        assert_eq!(
            status_of(b"HTTP/1.0 407 Proxy Authentication Required\r\n"),
            Some(407)
        );
        assert_eq!(status_of(b"SSH-2.0-OpenSSH\r\n"), None);
        assert_eq!(status_of(b""), None);
    }

    /// The hosts the environment names take the providers' place, and `off` turns the check
    /// off.
    #[test]
    fn named_hosts_take_the_providers_place() {
        assert_eq!(
            named_hosts(" proxy.example.com:3128, ,localhost:9 "),
            ["proxy.example.com:3128", "localhost:9"]
        );
        assert!(turns_it_off(" Off "));
        assert!(!turns_it_off("proxy.example.com:3128"));
    }

    #[test]
    fn a_network_error_blames_its_agent_only_once_the_internet_was_found_up_after_it() {
        let at = OffsetDateTime::UNIX_EPOCH + time::Duration::minutes(1);
        assert!(Internet::Unchecked.blames(at));
        assert!(!Internet::Down.blames(at));
        assert!(!Internet::Up { found_at: None }.blames(at));
        let before = at - time::Duration::seconds(1);
        assert!(
            !Internet::Up {
                found_at: Some(before)
            }
            .blames(at)
        );
        assert!(Internet::Up { found_at: Some(at) }.blames(at));
    }
}
