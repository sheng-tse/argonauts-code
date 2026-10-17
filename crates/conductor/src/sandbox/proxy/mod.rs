//! The localhost proxy that is a sandboxed session's only way out. macOS won't start a CLI's
//! own sandbox inside ours, so the CLI runs with its own turned off, and our profile lets the
//! session connect to one loopback port: this proxy. The proxy opens CONNECT tunnels to the hosts
//! on the session's list and refuses everything else. Your local model servers are the one
//! exception: they speak plain HTTP, often on this machine's loopback, so requests to exactly
//! their host and port are passed on as they are, minus the calls that manage models.
//!
//! It never terminates TLS and never reads what goes through a tunnel. Its log lines carry a
//! host name and a port, and nothing else from the request.
//!
//! One proxy serves the whole process on one random port, on its own runtime thread so it
//! outlives whichever runtime asked for it. Each list of hosts gets a random key, which its
//! sessions send as the proxy URL's password. The key picks the list, and a process without one,
//! such as a check with loopback open for its test servers, gets no tunnel at all.
//!
//! Where Argonauts Code itself reaches the internet only through a proxy, its environment's
//! `HTTPS_PROXY` or `ALL_PROXY`, tunnels go on through that one.

mod allow;
mod upstream;

pub use allow::{Allowlist, Rule, cli_hosts};
pub use upstream::Upstream;

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tracing::instrument::WithSubscriber;

/// The user name in the proxy URL. The key is the password.
const USER: &str = "argonauts";
/// A CONNECT request's head is a line and a few headers.
const HEAD_LIMIT: usize = 8 * 1024;
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const REFUSALS_KEPT: usize = 64;

/// Finds the addresses for a host and port.
pub type Resolver =
    Arc<dyn Fn(String, u16) -> BoxFuture<'static, io::Result<Vec<SocketAddr>>> + Send + Sync>;

/// A tunnel the proxy wouldn't open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub host: String,
    pub port: u16,
    pub why: Why,
    pub at: Instant,
    /// The list of the key it came with, so a session finds its own.
    list: Option<Arc<Allowlist>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// No key, or one this proxy didn't give out.
    NoKey,
    /// The host isn't on the key's list.
    NotListed,
    /// A call to a local endpoint that would pull, push, copy, or delete a model.
    ManagesModels,
    /// An allowed host it couldn't reach, itself or through the proxy it goes out by.
    Unreachable,
}

impl Refusal {
    /// What the proxy said to the session, as a line: `couldn't reach api.anthropic.com`.
    pub fn words(&self) -> String {
        match self.why {
            Why::NoKey => format!("refused {} without a session's key", self.host),
            Why::NotListed => format!("refused {}, which isn't on the session's list", self.host),
            Why::ManagesModels => format!("refused a call that manages {}'s models", self.host),
            Why::Unreachable => format!("couldn't reach {}", self.host),
        }
    }
}

#[derive(Clone)]
pub struct Proxy {
    inner: Arc<Inner>,
}

struct Inner {
    /// Taken on drop, to shut down without blocking.
    runtime: Option<tokio::runtime::Runtime>,
    port: u16,
    state: Arc<State>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

struct State {
    /// For the hosts on a list: only addresses outside this machine.
    resolver: Resolver,
    /// For your local endpoints, which may be on loopback.
    local_resolver: Resolver,
    keys: Mutex<Keys>,
    refusals: Mutex<VecDeque<Refusal>>,
    /// The proxy this process goes out through, when its environment names one.
    upstream: Option<Upstream>,
}

#[derive(Default)]
struct Keys {
    by_list: HashMap<Allowlist, String>,
    /// `Proxy-Authorization` credentials, as clients encode them, to the list they open.
    by_credentials: HashMap<String, Arc<Allowlist>>,
}

static SHARED: Mutex<Option<Proxy>> = Mutex::new(None);

/// The process's proxy, started the first time a session needs it.
pub fn shared() -> io::Result<Proxy> {
    let mut shared = lock(&SHARED);
    if let Some(proxy) = &*shared {
        return Ok(proxy.clone());
    }
    let proxy = Proxy::start()?;
    *shared = Some(proxy.clone());
    Ok(proxy)
}

impl Proxy {
    /// A proxy that looks hosts up in DNS.
    pub fn start() -> io::Result<Self> {
        let upstream = Upstream::from_env(|name| std::env::var(name).ok());
        Self::with_resolvers(
            system_resolver(Reach::Outside),
            system_resolver(Reach::Any),
            upstream,
        )
    }

    /// One resolver for every host, as tests use to send them all to a local server.
    pub fn with_resolver(resolver: Resolver) -> io::Result<Self> {
        Self::with_resolvers(resolver.clone(), resolver, None)
    }

    /// [`Proxy::with_resolver`] for the hosts it tunnels to, going out through `upstream`,
    /// which is looked up in DNS as your local endpoints are.
    pub fn through(resolver: Resolver, upstream: Upstream) -> io::Result<Self> {
        Self::with_resolvers(resolver, system_resolver(Reach::Any), Some(upstream))
    }

    /// Listens on a random loopback port from now on. Its log lines go wherever the caller's go.
    fn with_resolvers(
        resolver: Resolver,
        local_resolver: Resolver,
        upstream: Option<Upstream>,
    ) -> io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("argonauts-proxy")
            .enable_io()
            .enable_time()
            .build()?;
        let std_listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        std_listener.set_nonblocking(true)?;
        let port = std_listener.local_addr()?.port();
        let listener = {
            let _entered = runtime.enter();
            TcpListener::from_std(std_listener)?
        };
        let state = Arc::new(State {
            resolver,
            local_resolver,
            keys: Mutex::new(Keys::default()),
            refusals: Mutex::new(VecDeque::new()),
            upstream,
        });
        runtime.spawn(serve(listener, state.clone()).with_current_subscriber());
        Ok(Self {
            inner: Arc::new(Inner {
                runtime: Some(runtime),
                port,
                state,
            }),
        })
    }

    /// The loopback port it listens on.
    pub fn port(&self) -> u16 {
        self.inner.port
    }

    /// The proxy URL for sessions that may reach `allow`'s hosts, with their list's key in it.
    pub fn session_url(&self, allow: &Allowlist) -> String {
        let mut keys = lock(&self.inner.state.keys);
        let key = match keys.by_list.get(allow) {
            Some(key) => key.clone(),
            None => {
                let key = uuid::Uuid::new_v4().simple().to_string();
                let credentials = base64(format!("{USER}:{key}").as_bytes());
                keys.by_credentials
                    .insert(credentials, Arc::new(allow.clone()));
                keys.by_list.insert(allow.clone(), key.clone());
                key
            }
        };
        format!("http://{USER}:{key}@127.0.0.1:{}", self.inner.port)
    }

    /// The latest refused tunnels, oldest first.
    pub fn refusals(&self) -> Vec<Refusal> {
        lock(&self.inner.state.refusals).iter().cloned().collect()
    }

    /// The tunnels refused since `since` to sessions that may reach `allow`'s hosts, oldest
    /// first.
    pub fn refused_since(&self, allow: &Allowlist, since: Instant) -> Vec<Refusal> {
        lock(&self.inner.state.refusals)
            .iter()
            .filter(|refusal| refusal.at >= since && refusal.list.as_deref() == Some(allow))
            .cloned()
            .collect()
    }
}

/// The variables that send a session's traffic to the proxy at `url`. Both spellings, since
/// curl reads only the lowercase `http_proxy`, and `NO_PROXY` empty so nothing tries to go
/// around it.
pub fn session_env(url: &str) -> Vec<(String, String)> {
    let mut env = Vec::new();
    for name in ["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY"] {
        env.push((name.to_owned(), url.to_owned()));
        env.push((name.to_ascii_lowercase(), url.to_owned()));
    }
    env.push(("NO_PROXY".to_owned(), String::new()));
    env.push(("no_proxy".to_owned(), String::new()));
    env
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Copy)]
enum Reach {
    Outside,
    Any,
}

fn system_resolver(reach: Reach) -> Resolver {
    Arc::new(move |host, port| {
        Box::pin(async move {
            let found = tokio::net::lookup_host((host.as_str(), port)).await?;
            Ok(found
                .filter(|addr| matches!(reach, Reach::Any) || outside_this_machine(addr.ip()))
                .collect())
        })
    })
}

/// Whatever a name resolves to, a tunnel never reaches back into this machine or a cloud
/// metadata address, which would undo the profile's loopback lock.
fn outside_this_machine(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(ip) => !(ip.is_loopback() || ip.is_unspecified() || ip.is_link_local()),
        IpAddr::V6(ip) => !(ip.is_loopback() || ip.is_unspecified() || ip.is_unicast_link_local()),
    }
}

async fn serve(listener: TcpListener, state: Arc<State>) {
    loop {
        let client = match listener.accept().await {
            Ok((client, _)) => client,
            Err(err) => {
                tracing::warn!("the proxy couldn't accept a connection: {err}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        tokio::spawn(tunnel(client, state.clone()).with_current_subscriber());
    }
}

async fn tunnel(mut client: TcpStream, state: Arc<State>) {
    let Ok(Ok(Some((head, early)))) = timeout(HEAD_TIMEOUT, read_head(&mut client)).await else {
        return;
    };
    let request = match parse_request(&head) {
        Ok(request) => request,
        Err(Bad::NotConnect) => return not_connect(&mut client).await,
        Err(Bad::Malformed) => {
            return respond(
                &mut client,
                "400 Bad Request",
                "a malformed CONNECT request",
            )
            .await;
        }
    };
    let (host, port) = (request.host, request.port);
    let allow = request
        .credentials
        .and_then(|credentials| lock(&state.keys).by_credentials.get(&credentials).cloned());
    let Some(allow) = allow else {
        tracing::warn!(%host, port, "the proxy refused a tunnel without a session's key");
        state.refused(&host, port, Why::NoKey, None);
        let reply = "HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic \
                     realm=\"argonauts\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let _ = client.write_all(reply.as_bytes()).await;
        return;
    };
    let local = allow.is_local(&host, port);
    if request.forward.is_some() && !local {
        return not_connect(&mut client).await;
    }
    if !allow.allows(&host, port) {
        tracing::warn!(%host, port, "the proxy refused a tunnel to a host not on the session's list");
        state.refused(&host, port, Why::NotListed, Some(&allow));
        let why = format!(
            "{host}:{port} isn't on this session's list. Add it to sandbox.proxy_allow if it \
             should be."
        );
        return respond(&mut client, "403 Forbidden", &why).await;
    }
    if request
        .forward
        .as_ref()
        .is_some_and(|forward| manages_models(&forward.method, &forward.path))
    {
        tracing::warn!(%host, port, "the proxy refused a call that manages a local endpoint's models");
        state.refused(&host, port, Why::ManagesModels, Some(&allow));
        let why = "a session may use your local models, not pull, push, copy, or delete them";
        return respond(&mut client, "403 Forbidden", why).await;
    }
    let resolver = if local {
        &state.local_resolver
    } else {
        &state.resolver
    };
    // Your own proxy may be on this machine, so it's looked up as a local endpoint is.
    let through = state
        .upstream
        .as_ref()
        .filter(|upstream| !local && upstream.takes(&host, port));
    let reached = match through {
        // A name that points only into this machine stays out of reach, as it does dialled
        // straight. One that can't be looked up here may still be out there, past the proxy.
        Some(through) => match resolver(host.clone(), port).await {
            Ok(addrs) if addrs.is_empty() => Err(io::ErrorKind::NotFound.into()),
            _ => through.open(&state.local_resolver, &host, port).await,
        },
        None => connect(resolver, &host, port).await,
    };
    let mut upstream = match reached {
        Ok(upstream) => upstream,
        Err(_) => {
            tracing::warn!(%host, port, "the proxy couldn't reach an allowed host");
            state.refused(&host, port, Why::Unreachable, Some(&allow));
            let why = format!("couldn't reach {host}:{port}");
            return respond(&mut client, "502 Bad Gateway", &why).await;
        }
    };
    let opened = match &request.forward {
        Some(forward) => upstream.write_all(&forward.head).await.is_ok(),
        None => client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .is_ok(),
    };
    tracing::debug!(%host, port, "the proxy opened a tunnel");
    if !opened || upstream.write_all(&early).await.is_err() {
        return;
    }
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    tracing::debug!(%host, port, "the proxy closed a tunnel");
}

async fn not_connect(client: &mut TcpStream) {
    tracing::warn!("the proxy refused a request that wasn't CONNECT");
    let why = "only CONNECT tunnels go through, so use an https:// URL";
    respond(client, "405 Method Not Allowed", why).await;
}

/// Ollama's API also pulls, pushes, copies, and deletes models, and a session only needs to
/// run one. Paths compare after decoding and cleaning, as the server will see them.
fn manages_models(method: &str, path: &str) -> bool {
    const MANAGING: &[&str] = &[
        "/api/pull",
        "/api/push",
        "/api/create",
        "/api/copy",
        "/api/delete",
        "/api/blobs",
    ];
    if ["DELETE", "PUT", "PATCH"]
        .iter()
        .any(|managing| method.eq_ignore_ascii_case(managing))
    {
        return true;
    }
    let Some(path) = clean_path(path) else {
        return true;
    };
    MANAGING.iter().any(|managing| {
        path.strip_prefix(managing)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// The path without its query, percent-decoded, lowercased, with empty and `.` segments
/// dropped and `..` applied. `None` when it doesn't decode.
fn clean_path(path: &str) -> Option<String> {
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = std::str::from_utf8(bytes.get(at + 1..at + 3)?).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            decoded.push(bytes[at]);
            at += 1;
        }
    }
    let decoded = String::from_utf8(decoded).ok()?.to_ascii_lowercase();
    let mut segments: Vec<&str> = Vec::new();
    for segment in decoded.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            segment => segments.push(segment),
        }
    }
    Some(format!("/{}", segments.join("/")))
}

impl State {
    fn refused(&self, host: &str, port: u16, why: Why, list: Option<&Arc<Allowlist>>) {
        let mut kept = lock(&self.refusals);
        if kept.len() == REFUSALS_KEPT {
            kept.pop_front();
        }
        kept.push_back(Refusal {
            host: host.to_owned(),
            port,
            why,
            at: Instant::now(),
            list: list.cloned(),
        });
    }
}

/// The request head, and any bytes the client sent after it without waiting for the reply.
/// `None` when the client closed first or the head is too long.
async fn read_head(client: &mut TcpStream) -> io::Result<Option<(Vec<u8>, Vec<u8>)>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(end) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            let early = buf.split_off(end + 4);
            return Ok(Some((buf, early)));
        }
        if buf.len() > HEAD_LIMIT {
            return Ok(None);
        }
        let read = client.read(&mut chunk).await?;
        if read == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..read]);
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Request {
    host: String,
    port: u16,
    /// The `Basic` credentials of `Proxy-Authorization`, still encoded.
    credentials: Option<String>,
    /// A plain HTTP request to pass on, `None` for a CONNECT tunnel.
    forward: Option<Forward>,
}

#[derive(Debug, PartialEq, Eq)]
struct Forward {
    method: String,
    /// As the client sent it, with any query.
    path: String,
    /// The head to send on: the request line in origin form, without the proxy's own headers,
    /// and closing after one response so a kept-alive connection can't reach another host.
    head: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
enum Bad {
    NotConnect,
    Malformed,
}

/// The headers that are for the proxy, or for one hop, and never go on.
const HOP_HEADERS: &[&str] = &[
    "proxy-authorization",
    "proxy-connection",
    "connection",
    "keep-alive",
];

/// `CONNECT host:port HTTP/1.x`, or a plain request to an `http://` URL, and its credentials. A
/// host that isn't a plain name or IP is malformed, so a log line never carries anything else.
fn parse_request(head: &[u8]) -> Result<Request, Bad> {
    let head = std::str::from_utf8(head).map_err(|_| Bad::Malformed)?;
    let mut lines = head.split("\r\n");
    let mut parts = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(Bad::Malformed);
    };
    if !version.starts_with("HTTP/1.") {
        return Err(Bad::Malformed);
    }
    let headers: Vec<&str> = lines.filter(|line| !line.is_empty()).collect();
    let (host, port, forward) = if method == "CONNECT" {
        let (host, port) = host_and_port(target, None)?;
        (host, port, None)
    } else {
        let rest = target
            .get(..7)
            .filter(|scheme| scheme.eq_ignore_ascii_case("http://"))
            .map(|_| &target[7..])
            .ok_or(Bad::NotConnect)?;
        let split = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, path) = rest.split_at(split);
        if authority.contains('@') {
            return Err(Bad::Malformed);
        }
        let (host, port) = host_and_port(authority, Some(80))?;
        let path = match path {
            "" => "/".to_owned(),
            query if query.starts_with('?') => format!("/{query}"),
            path => path.to_owned(),
        };
        let mut sent = format!("{method} {path} {version}\r\n");
        for line in &headers {
            let name = line.split(':').next().unwrap_or_default().trim();
            if !HOP_HEADERS.iter().any(|hop| name.eq_ignore_ascii_case(hop)) {
                sent.push_str(line);
                sent.push_str("\r\n");
            }
        }
        sent.push_str("Connection: close\r\n\r\n");
        let forward = Forward {
            method: method.to_owned(),
            path,
            head: sent.into_bytes(),
        };
        (host, port, Some(forward))
    };
    let credentials = headers.iter().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if !name.trim().eq_ignore_ascii_case("proxy-authorization") {
            return None;
        }
        let (scheme, credentials) = value.trim().split_once(' ')?;
        scheme
            .eq_ignore_ascii_case("basic")
            .then(|| credentials.trim().to_owned())
    });
    Ok(Request {
        host,
        port,
        credentials,
        forward,
    })
}

/// `host:port`, or `[ipv6]:port`. Without a port, `default`, which a CONNECT doesn't have.
fn host_and_port(target: &str, default: Option<u16>) -> Result<(String, u16), Bad> {
    let (host, port) = match target.strip_prefix('[') {
        Some(bracketed) => {
            let (ip, rest) = bracketed.split_once(']').ok_or(Bad::Malformed)?;
            ip.parse::<Ipv6Addr>().map_err(|_| Bad::Malformed)?;
            match rest.strip_prefix(':') {
                Some(port) => (ip, Some(port)),
                None if rest.is_empty() => (ip, None),
                None => return Err(Bad::Malformed),
            }
        }
        None => match target.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (target, None),
        },
    };
    let port = match port {
        Some(port) => port.parse::<u16>().ok().filter(|port| *port != 0),
        None => default,
    }
    .ok_or(Bad::Malformed)?;
    let host = host.to_ascii_lowercase();
    if !(allow::is_host_name(host.trim_end_matches('.')) || host.parse::<IpAddr>().is_ok()) {
        return Err(Bad::Malformed);
    }
    Ok((host, port))
}

async fn connect(resolver: &Resolver, host: &str, port: u16) -> io::Result<TcpStream> {
    let connecting = async {
        let addrs = resolver(host.to_owned(), port).await?;
        let mut last = io::Error::new(io::ErrorKind::NotFound, "no usable address");
        for addr in addrs {
            match TcpStream::connect(addr).await {
                Ok(stream) => return Ok(stream),
                Err(err) => last = err,
            }
        }
        Err(last)
    };
    timeout(CONNECT_TIMEOUT, connecting)
        .await
        .unwrap_or_else(|_| Err(io::ErrorKind::TimedOut.into()))
}

async fn respond(client: &mut TcpStream, status: &str, why: &str) {
    let body = format!("argonauts proxy: {why}\n");
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: \
         close\r\n\r\n{body}",
        body.len()
    );
    let _ = client.write_all(reply.as_bytes()).await;
    let _ = client.shutdown().await;
}

/// Standard base64 with padding, as clients encode `Proxy-Authorization`.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, byte)| n | u32::from(*byte) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests;
