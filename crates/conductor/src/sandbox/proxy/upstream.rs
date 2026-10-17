//! The proxy Argonauts Code itself goes out through, when its own environment names one: on a
//! network that reaches the internet only through a proxy, a tunnel dialled straight to the
//! host never arrives. Only an `http://` proxy is followed, by a CONNECT of its own, and
//! `NO_PROXY`'s hosts are dialled straight.

use std::io;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::{HEAD_LIMIT, Resolver, base64, connect};

/// An `http://` proxy from `HTTPS_PROXY` or `ALL_PROXY`, with the hosts `NO_PROXY` keeps from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    host: String,
    port: u16,
    /// `Proxy-Authorization`'s `Basic` credentials, encoded, when the URL carries a user.
    credentials: Option<String>,
    /// Lowercase, without a leading dot. `*` keeps every host from it.
    no_proxy: Vec<String>,
}

impl Upstream {
    /// The proxy `var` names, the uppercase spelling first: `HTTPS_PROXY`, else `ALL_PROXY`.
    /// `None` when neither names an `http://` proxy.
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let named = |name: &str| {
            var(name)
                .or_else(|| var(&name.to_ascii_lowercase()))
                .filter(|value| !value.trim().is_empty())
        };
        let url = named("HTTPS_PROXY").or_else(|| named("ALL_PROXY"))?;
        let mut upstream = Self::parse(&url)?;
        upstream.no_proxy = named("NO_PROXY")
            .unwrap_or_default()
            .split(',')
            .map(|entry| entry.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|entry| !entry.is_empty())
            .collect();
        Some(upstream)
    }

    fn parse(url: &str) -> Option<Self> {
        let rest = url.trim();
        let rest = match rest.split_once("://") {
            Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => rest,
            Some(_) => return None,
            None => rest,
        };
        let authority = rest.split('/').next().unwrap_or_default();
        let (userinfo, hostport) = match authority.rsplit_once('@') {
            Some((userinfo, hostport)) => (Some(userinfo), hostport),
            None => (None, authority),
        };
        // An IPv6 address comes in brackets, its own colons inside them.
        let (host, port) = match hostport.strip_prefix('[') {
            Some(bracketed) => {
                let (host, after) = bracketed.split_once(']')?;
                (host, after.strip_prefix(':'))
            }
            None => match hostport.rsplit_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (hostport, None),
            },
        };
        let port = port.map_or(Some(80), |port| port.parse().ok())?;
        if host.is_empty() {
            return None;
        }
        Some(Self {
            host: host.to_owned(),
            port,
            credentials: userinfo.map(|userinfo| base64(decoded(userinfo).as_bytes())),
            no_proxy: Vec::new(),
        })
    }

    /// Whether a tunnel to `host` goes through this proxy: every host but those `NO_PROXY`
    /// names, itself or a name under it, with or without its port.
    pub fn takes(&self, host: &str, port: u16) -> bool {
        let host = host.to_ascii_lowercase();
        !self.no_proxy.iter().any(|entry| {
            let (name, only_port) = match entry.rsplit_once(':') {
                Some((name, only)) => (name, only.parse::<u16>().ok()),
                None => (entry.as_str(), None),
            };
            entry == "*"
                || (only_port.is_none_or(|only| only == port)
                    && (host == name || host.ends_with(&format!(".{name}"))))
        })
    }

    /// A tunnel to `host` through this proxy, open once it said yes. An error when it can't be
    /// reached or said anything else.
    pub async fn open(&self, resolver: &Resolver, host: &str, port: u16) -> io::Result<TcpStream> {
        let mut stream = connect(resolver, &self.host, self.port).await?;
        let auth = self
            .credentials
            .as_ref()
            .map(|credentials| format!("Proxy-Authorization: Basic {credentials}\r\n"))
            .unwrap_or_default();
        let request =
            format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n{auth}\r\n");
        stream.write_all(request.as_bytes()).await?;
        let status = read_status(&mut stream).await?;
        if status != 200 {
            return Err(io::Error::other(format!("the proxy answered {status}")));
        }
        Ok(stream)
    }
}

/// The status of the reply's head, read up to its end and no further, so nothing of the
/// tunnel's own bytes is taken.
async fn read_status(stream: &mut TcpStream) -> io::Result<u16> {
    let mut head = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > HEAD_LIMIT || stream.read(&mut byte).await? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        head.push(byte[0]);
    }
    let line = String::from_utf8_lossy(&head);
    line.split(' ')
        .nth(1)
        .and_then(|status| status.parse().ok())
        .ok_or_else(|| io::Error::other("the proxy's reply had no status"))
}

/// A URL's user and password as they're meant, `%40` back to `@`.
fn decoded(userinfo: &str) -> String {
    let bytes = userinfo.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = bytes
            .get(at + 1..at + 3)
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (bytes[at], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                at += 3;
            }
            (byte, _) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn https_proxy_comes_first_then_all_proxy_in_either_spelling() {
        let upstream = Upstream::from_env(env(&[
            ("ALL_PROXY", "http://all.example.test:8080"),
            ("https_proxy", "http://corp.example.test:3128"),
        ]))
        .unwrap();
        assert_eq!(
            (upstream.host.as_str(), upstream.port),
            ("corp.example.test", 3128)
        );
        let all = Upstream::from_env(env(&[("all_proxy", "all.example.test")])).unwrap();
        assert_eq!((all.host.as_str(), all.port), ("all.example.test", 80));
    }

    #[test]
    fn only_an_http_proxy_is_followed() {
        assert_eq!(
            Upstream::from_env(env(&[("ALL_PROXY", "socks5://127.0.0.1:1080")])),
            None
        );
        assert_eq!(Upstream::from_env(env(&[("HTTPS_PROXY", " ")])), None);
        assert_eq!(Upstream::from_env(env(&[])), None);
    }

    #[test]
    fn a_proxy_user_goes_as_basic_credentials() {
        let upstream =
            Upstream::from_env(env(&[("HTTPS_PROXY", "http://me:p%40ss@[::1]:3128/")])).unwrap();
        assert_eq!((upstream.host.as_str(), upstream.port), ("::1", 3128));
        assert_eq!(upstream.credentials, Some(base64(b"me:p@ss")));
    }

    #[test]
    fn no_proxy_keeps_its_hosts_and_the_names_under_them_out() {
        let upstream = Upstream::from_env(env(&[
            ("HTTPS_PROXY", "http://corp.example.test:3128"),
            ("NO_PROXY", "internal.test, .lan,api.example.test:8443"),
        ]))
        .unwrap();
        assert!(upstream.takes("api.anthropic.com", 443));
        assert!(!upstream.takes("internal.test", 443));
        assert!(!upstream.takes("git.internal.test", 443));
        assert!(!upstream.takes("printer.LAN", 443));
        assert!(upstream.takes("notinternal.test", 443));
        assert!(!upstream.takes("api.example.test", 8443));
        assert!(upstream.takes("api.example.test", 443));
        let none = Upstream::from_env(env(&[
            ("HTTPS_PROXY", "http://corp.example.test:3128"),
            ("NO_PROXY", "*"),
        ]))
        .unwrap();
        assert!(!none.takes("api.anthropic.com", 443));
    }
}
