use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use super::*;

/// An upstream that echoes what it gets until the client is done, counting connections.
fn echo_server() -> (u16, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let contacted = Arc::new(AtomicUsize::new(0));
    let count = contacted.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            count.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || {
                let mut buf = [0u8; 1024];
                while let Ok(read) = stream.read(&mut buf) {
                    if read == 0 || stream.write_all(&buf[..read]).is_err() {
                        break;
                    }
                }
            });
        }
    });
    (port, contacted)
}

/// Sends every host to `port` on loopback, counting lookups.
pub(crate) fn to_loopback(port: u16, lookups: Arc<AtomicUsize>) -> Resolver {
    Arc::new(move |_host, _port| {
        lookups.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(vec![SocketAddr::from((Ipv4Addr::LOCALHOST, port))]) })
    })
}

/// The `Proxy-Authorization` header for a session URL's user and key.
fn auth_header(url: &str) -> String {
    let userinfo = url
        .strip_prefix("http://")
        .and_then(|rest| rest.split_once('@'))
        .map(|(userinfo, _)| userinfo)
        .unwrap();
    format!(
        "Proxy-Authorization: Basic {}\r\n",
        base64(userinfo.as_bytes())
    )
}

fn connect_request(target: &str, auth: &str) -> String {
    format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n{auth}\r\n")
}

/// Sends `request` and reads the reply's head.
fn send(proxy: &Proxy, request: &str) -> (TcpStream, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", proxy.port())).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
        head.push(byte[0]);
    }
    (stream, String::from_utf8(head).unwrap())
}

fn body(mut stream: TcpStream) -> String {
    let mut body = String::new();
    let _ = stream.read_to_string(&mut body);
    body
}

#[test]
fn an_allowed_host_tunnels() {
    let (upstream, contacted) = echo_server();
    let proxy = Proxy::with_resolver(to_loopback(upstream, Arc::default())).unwrap();
    let allow = Allowlist::parse([format!("api.example.test:{upstream}").as_str()]).unwrap();
    let url = proxy.session_url(&allow);
    assert!(
        url.ends_with(&format!("@127.0.0.1:{}", proxy.port())),
        "{url}"
    );
    assert_eq!(proxy.session_url(&allow), url, "one key per list");

    // Bytes sent before the reply, as some clients do, reach the host too.
    let target = format!("api.example.test:{upstream}");
    let request = connect_request(&target, &auth_header(&url)) + "hello ";
    let (mut stream, head) = send(&proxy, &request);
    assert!(head.starts_with("HTTP/1.1 200 "), "{head}");
    stream.write_all(b"world").unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    assert_eq!(body(stream), "hello world");
    assert_eq!(contacted.load(Ordering::SeqCst), 1);
    assert!(proxy.refusals().is_empty());
}

#[test]
fn a_host_not_on_the_list_is_refused() {
    let (upstream, contacted) = echo_server();
    let lookups = Arc::new(AtomicUsize::new(0));
    let proxy = Proxy::with_resolver(to_loopback(upstream, lookups.clone())).unwrap();
    let url = proxy.session_url(&Allowlist::for_cli("claude-code", &[]).unwrap());
    let auth = auth_header(&url);

    for target in [
        "evil.example.test:443",
        "api.anthropic.com:8443",
        "api.openai.com:443",
    ] {
        let (stream, head) = send(&proxy, &connect_request(target, &auth));
        assert!(head.starts_with("HTTP/1.1 403 "), "{target}: {head}");
        assert!(body(stream).contains("sandbox.proxy_allow"), "{target}");
    }
    assert_eq!(
        lookups.load(Ordering::SeqCst),
        0,
        "a refused host is never looked up"
    );
    assert_eq!(contacted.load(Ordering::SeqCst), 0);
    let refused: Vec<(String, u16, Why)> = proxy
        .refusals()
        .into_iter()
        .map(|refusal| (refusal.host, refusal.port, refusal.why))
        .collect();
    assert_eq!(
        refused,
        [
            ("evil.example.test".to_owned(), 443, Why::NotListed),
            ("api.anthropic.com".to_owned(), 8443, Why::NotListed),
            ("api.openai.com".to_owned(), 443, Why::NotListed),
        ]
    );
}

#[test]
fn a_tunnel_needs_its_lists_key() {
    let (upstream, contacted) = echo_server();
    let proxy = Proxy::with_resolver(to_loopback(upstream, Arc::default())).unwrap();
    let target = format!("api.example.test:{upstream}");
    let allow = Allowlist::parse([target.as_str()]).unwrap();
    let url = proxy.session_url(&allow);
    let other = proxy.session_url(&Allowlist::for_cli("codex", &[]).unwrap());
    assert_ne!(url, other);

    let wrong_key = "Proxy-Authorization: Basic YXJnb25hdXRzOm5vcGU=\r\n";
    for auth in ["", wrong_key, "Proxy-Authorization: Bearer x\r\n"] {
        let (_, head) = send(&proxy, &connect_request(&target, auth));
        assert!(head.starts_with("HTTP/1.1 407 "), "{auth:?}: {head}");
        assert!(head.contains("Proxy-Authenticate: Basic"), "{head}");
    }
    // Another list's key opens only that list's hosts.
    let (_, head) = send(&proxy, &connect_request(&target, &auth_header(&other)));
    assert!(head.starts_with("HTTP/1.1 403 "), "{head}");
    assert_eq!(contacted.load(Ordering::SeqCst), 0);
    assert_eq!(
        proxy
            .refusals()
            .iter()
            .map(|refusal| refusal.why)
            .collect::<Vec<_>>(),
        [Why::NoKey, Why::NoKey, Why::NoKey, Why::NotListed]
    );
}

#[test]
fn only_connect_goes_through() {
    let proxy = Proxy::with_resolver(to_loopback(9, Arc::default())).unwrap();
    let url = proxy.session_url(&Allowlist::parse(["api.example.test"]).unwrap());
    let auth = auth_header(&url);
    let plain =
        format!("GET http://api.example.test/ HTTP/1.1\r\nHost: api.example.test\r\n{auth}\r\n");
    let (_, head) = send(&proxy, &plain);
    assert!(head.starts_with("HTTP/1.1 405 "), "{head}");
    for request in [
        "HELLO\r\n\r\n".to_owned(),
        connect_request("api.example.test", &auth),
        connect_request("api.example.test:0", &auth),
        connect_request("api.example.test/x:443", &auth),
        connect_request("[not-ip]:443", &auth),
        "CONNECT api.example.test:443 SPDY/3\r\n\r\n".to_owned(),
    ] {
        let (_, head) = send(&proxy, &request);
        assert!(head.starts_with("HTTP/1.1 400 "), "{request:?}: {head}");
    }
    assert!(proxy.refusals().is_empty());
}

#[test]
fn tunnels_never_reach_this_machine() {
    let (local, contacted) = echo_server();
    let proxy = Proxy::start().unwrap();
    // Even listed, loopback is out of reach.
    let url =
        proxy.session_url(&Allowlist::parse([format!("localhost:{local}").as_str()]).unwrap());
    let (_, head) = send(
        &proxy,
        &connect_request(&format!("localhost:{local}"), &auth_header(&url)),
    );
    assert!(head.starts_with("HTTP/1.1 502 "), "{head}");
    assert_eq!(contacted.load(Ordering::SeqCst), 0);

    for ip in [
        "127.0.0.1",
        "::1",
        "::ffff:127.0.0.1",
        "0.0.0.0",
        "169.254.169.254",
        "fe80::1",
    ] {
        assert!(!outside_this_machine(ip.parse().unwrap()), "{ip}");
    }
    for ip in ["160.79.104.10", "2607:6bc0::10", "10.0.0.8"] {
        assert!(outside_this_machine(ip.parse().unwrap()), "{ip}");
    }
}

/// Collects what a `fmt` subscriber writes.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        lock(&self.0).extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn lines(&self) -> Vec<String> {
        let text = String::from_utf8(lock(&self.0).clone()).unwrap();
        let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
        lines.sort();
        lines
    }
}

#[test]
fn the_log_carries_hosts_and_ports_and_nothing_else() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_max_level(tracing::Level::TRACE)
        .without_time()
        .with_target(false)
        .with_level(false)
        .with_ansi(false)
        .finish();
    // tracing caches whether each log line is wanted. With one subscriber alive it asks only the
    // current thread's, so a line first reached on another test's thread would stay unwanted.
    // A second one makes it ask every subscriber.
    let _second = tracing::Dispatch::new(tracing_subscriber::registry());
    let _logging = tracing::subscriber::set_default(subscriber);

    let (upstream, _) = echo_server();
    let proxy = Proxy::with_resolver(to_loopback(upstream, Arc::default())).unwrap();
    let target = format!("api.example.test:{upstream}");
    let url = proxy.session_url(&Allowlist::parse([target.as_str()]).unwrap());
    let auth = auth_header(&url);
    let secrets = "User-Agent: secret-agent\r\nCookie: secret-cookie\r\n";

    let tunnel = format!("CONNECT {target} HTTP/1.1\r\n{secrets}{auth}\r\n");
    let (mut stream, head) = send(&proxy, &tunnel);
    assert!(head.starts_with("HTTP/1.1 200 "), "{head}");
    stream.write_all(b"secret-payload").unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    assert_eq!(body(stream), "secret-payload");

    send(
        &proxy,
        &format!("CONNECT evil.example.test:443 HTTP/1.1\r\n{secrets}{auth}\r\n"),
    );
    send(
        &proxy,
        &format!("CONNECT {target} HTTP/1.1\r\n{secrets}\r\n"),
    );
    send(
        &proxy,
        &format!("GET http://{target}/secret-path HTTP/1.1\r\n{secrets}{auth}\r\n"),
    );

    let expected = {
        let mut lines = vec![
            format!("the proxy closed a tunnel host=api.example.test port={upstream}"),
            format!("the proxy opened a tunnel host=api.example.test port={upstream}"),
            "the proxy refused a request that wasn't CONNECT".to_owned(),
            format!("the proxy refused a tunnel without a session's key host=api.example.test port={upstream}"),
            "the proxy refused a tunnel to a host not on the session's list host=evil.example.test port=443"
                .to_owned(),
        ];
        lines.sort();
        lines
    };
    // The tunnel's last line comes once both sides have closed.
    let deadline = Instant::now() + Duration::from_secs(10);
    while captured.lines().len() < expected.len() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let lines = captured.lines();
    assert_eq!(lines, expected);
    let key = url.split_once('@').unwrap().0.rsplit_once(':').unwrap().1;
    for line in &lines {
        assert!(!line.contains("secret") && !line.contains(key), "{line}");
    }
}

#[test]
fn requests_parse_with_their_credentials() {
    let request = "CONNECT API.Example.test.:8443 HTTP/1.1\r\nhost: x\r\nproxy-authorization: basic  abc= \r\n\r\n";
    assert_eq!(
        parse_request(request.as_bytes()),
        Ok(Request {
            host: "api.example.test.".to_owned(),
            port: 8443,
            credentials: Some("abc=".to_owned()),
            forward: None,
        })
    );
    let ipv6 = parse_request(b"CONNECT [2001:db8::1]:443 HTTP/1.1\r\n\r\n").unwrap();
    assert_eq!(
        (ipv6.host.as_str(), ipv6.port, ipv6.credentials),
        ("2001:db8::1", 443, None)
    );
    assert_eq!(
        parse_request(b"GET / HTTP/1.1\r\n\r\n"),
        Err(Bad::NotConnect)
    );
    assert_eq!(
        parse_request(b"GET https://api.example.test/ HTTP/1.1\r\n\r\n"),
        Err(Bad::NotConnect)
    );
    assert_eq!(
        parse_request(b"GET http://user@localhost:11434/ HTTP/1.1\r\n\r\n"),
        Err(Bad::Malformed)
    );
}

#[test]
fn a_plain_request_goes_on_in_origin_form_without_the_proxys_headers() {
    let head = "POST http://LocalHost:11434/v1/chat/completions?x=1 HTTP/1.1\r\nHost: localhost:11434\r\n\
                Proxy-Authorization: Basic abc=\r\nProxy-Connection: keep-alive\r\n\
                Connection: keep-alive\r\nContent-Length: 2\r\n\r\n";
    let request = parse_request(head.as_bytes()).unwrap();
    assert_eq!((request.host.as_str(), request.port), ("localhost", 11434));
    assert_eq!(request.credentials.as_deref(), Some("abc="));
    let forward = request.forward.unwrap();
    assert_eq!(forward.method, "POST");
    assert_eq!(forward.path, "/v1/chat/completions?x=1");
    assert_eq!(
        String::from_utf8(forward.head).unwrap(),
        "POST /v1/chat/completions?x=1 HTTP/1.1\r\nHost: localhost:11434\r\n\
         Content-Length: 2\r\nConnection: close\r\n\r\n"
    );

    let bare = parse_request(b"GET http://[::1] HTTP/1.1\r\n\r\n").unwrap();
    assert_eq!((bare.host.as_str(), bare.port), ("::1", 80));
    assert_eq!(bare.forward.unwrap().path, "/");
}

#[test]
fn calls_that_manage_models_are_spotted_however_spelled() {
    for (method, path) in [
        ("DELETE", "/api/delete"),
        ("delete", "/v1/models/x"),
        ("PUT", "/anything"),
        ("POST", "/api/pull"),
        ("POST", "/api/push?insecure=true"),
        ("POST", "/api/create"),
        ("POST", "/api/copy"),
        ("HEAD", "/api/blobs/sha256:abc"),
        ("POST", "/api/%70ull"),
        ("POST", "//api//pull"),
        ("POST", "/v1/../api/pull"),
        ("POST", "/API/Pull"),
        ("POST", "/api/pull/"),
        ("POST", "/api/%zz"),
    ] {
        assert!(manages_models(method, path), "{method} {path}");
    }
    for (method, path) in [
        ("POST", "/v1/chat/completions"),
        ("POST", "/api/chat"),
        ("POST", "/api/generate"),
        ("GET", "/api/tags"),
        ("POST", "/api/show"),
        ("GET", "/v1/models"),
        ("POST", "/api/pulley"),
    ] {
        assert!(!manages_models(method, path), "{method} {path}");
    }
}

#[test]
fn a_local_endpoint_is_reached_on_loopback_at_exactly_its_port() {
    let (local, contacted) = echo_server();
    let (other, other_contacted) = echo_server();
    let proxy = Proxy::start().unwrap();
    let allow = Allowlist::for_cli("opencode", &[])
        .unwrap()
        .with_local_endpoints([("LocalHost".to_owned(), local)]);
    let url = proxy.session_url(&allow);
    let auth = auth_header(&url);

    // A plain request goes on, rewritten for the server. The echo sends the head back.
    let plain = format!(
        "POST http://localhost:{local}/v1/chat/completions HTTP/1.1\r\nHost: localhost:{local}\r\n{auth}\r\n"
    );
    let (stream, echoed) = send(&proxy, &plain);
    assert_eq!(
        echoed,
        format!(
            "POST /v1/chat/completions HTTP/1.1\r\nHost: localhost:{local}\r\nConnection: close\r\n\r\n"
        )
    );
    stream.shutdown(Shutdown::Write).unwrap();
    drop(stream);

    // So does a tunnel.
    let (mut stream, head) = send(
        &proxy,
        &connect_request(&format!("localhost:{local}"), &auth),
    );
    assert!(head.starts_with("HTTP/1.1 200 "), "{head}");
    stream.write_all(b"ping").unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    assert_eq!(body(stream), "ping");
    assert_eq!(contacted.load(Ordering::SeqCst), 2);

    // Nothing else on loopback: not another port, not another name for the same server, and
    // not without the session's key.
    let (_, head) = send(
        &proxy,
        &format!("GET http://localhost:{other}/ HTTP/1.1\r\n{auth}\r\n"),
    );
    assert!(head.starts_with("HTTP/1.1 405 "), "{head}");
    let (_, head) = send(
        &proxy,
        &connect_request(&format!("localhost:{other}"), &auth),
    );
    assert!(head.starts_with("HTTP/1.1 403 "), "{head}");
    let (_, head) = send(
        &proxy,
        &format!("GET http://127.0.0.1:{local}/ HTTP/1.1\r\n{auth}\r\n"),
    );
    assert!(head.starts_with("HTTP/1.1 405 "), "{head}");
    let (_, head) = send(
        &proxy,
        &format!("GET http://localhost:{local}/ HTTP/1.1\r\n\r\n"),
    );
    assert!(head.starts_with("HTTP/1.1 407 "), "{head}");

    // Nor the calls that manage its models.
    let (stream, head) = send(
        &proxy,
        &format!("POST http://localhost:{local}/api/delete HTTP/1.1\r\n{auth}\r\n"),
    );
    assert!(head.starts_with("HTTP/1.1 403 "), "{head}");
    assert!(body(stream).contains("not pull, push, copy, or delete"));

    assert_eq!(contacted.load(Ordering::SeqCst), 2);
    assert_eq!(other_contacted.load(Ordering::SeqCst), 0);
    let refused: Vec<Why> = proxy.refusals().iter().map(|refusal| refusal.why).collect();
    assert_eq!(refused, [Why::NotListed, Why::NoKey, Why::ManagesModels]);
}

#[test]
fn credentials_encode_as_clients_send_them() {
    for (plain, encoded) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foobar", "Zm9vYmFy"),
        ("argonauts:k", "YXJnb25hdXRzOms="),
    ] {
        assert_eq!(base64(plain.as_bytes()), encoded, "{plain}");
    }
}

#[test]
fn sessions_get_every_proxy_variable_and_no_bypass() {
    let env: HashMap<String, String> = session_env("http://argonauts:k@127.0.0.1:4128")
        .into_iter()
        .collect();
    for name in [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        assert_eq!(env[name], "http://argonauts:k@127.0.0.1:4128", "{name}");
    }
    assert_eq!(env["NO_PROXY"], "");
    assert_eq!(env["no_proxy"], "");
}
