//! Which hosts a session's tunnels may reach: its harness's own hosts, plus
//! `sandbox.proxy_allow`, plus the local model servers your settings name.

use std::fmt;

/// The port a rule means when it names none.
const HTTPS: u16 = 443;

/// What each harness needs to run: its model API, the endpoint that refreshes its login, and the
/// vendor's own telemetry and feature flags. Found in each CLI's binary and checked against its
/// docs. Third-party intakes such as Datadog and Sentry are left out: they take anyone's key, so
/// they would carry a session's files out, and the CLIs run without them.
pub fn harness_hosts(vendor: &str) -> &'static [&'static str] {
    match vendor {
        "claude-code" => CLAUDE_CODE,
        "codex" => CODEX,
        "antigravity-cli" => ANTIGRAVITY_CLI,
        "opencode" => OPENCODE,
        _ => &[],
    }
}

const CLAUDE_CODE: &[&str] = &[
    // Messages, plus a subscription login's account, usage, and event logging.
    "api.anthropic.com",
    // Refreshes a subscription login (`/v1/oauth/token`).
    "platform.claude.com",
];

const CODEX: &[&str] = &[
    // Model traffic and usage on a ChatGPT login (`/backend-api`).
    "chatgpt.com",
    // Model traffic on an API key.
    "api.openai.com",
    // Refreshes a ChatGPT login (`/oauth/token`).
    "auth.openai.com",
    // Feature flags and metrics.
    "ab.chatgpt.com",
];

const ANTIGRAVITY_CLI: &[&str] = &[
    // Model traffic on a Google login.
    "cloudcode-pa.googleapis.com",
    "daily-cloudcode-pa.googleapis.com",
    // Model traffic on a Gemini API key.
    "generativelanguage.googleapis.com",
    // Refreshes a Google login, then reads whose it is (`/oauth2/v2/userinfo`).
    "oauth2.googleapis.com",
    "www.googleapis.com",
    // Usage logging and feature flags.
    "play.googleapis.com",
    "antigravity-unleash.goog",
    // Your account's picture: agy 1.2.14's eligibility check fetches it, and the session stops
    // when it can't.
    "lh3.googleusercontent.com",
];

/// Its providers are your choice, so their hosts go in `sandbox.proxy_allow`.
const OPENCODE: &[&str] = &[
    // The model catalog it loads at start.
    "models.dev",
];

/// One allowed host and port.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Rule {
    Host {
        host: String,
        port: u16,
    },
    /// Any subdomain of `domain`, though not `domain` itself.
    Subdomains {
        domain: String,
        port: u16,
    },
}

impl Rule {
    /// Parses `host`, `host:port`, or `*.domain` with an optional port.
    pub fn parse(entry: &str) -> Result<Self, String> {
        let bad = |why: &str| format!("sandbox.proxy_allow entry {entry:?} {why}");
        let (name, port) = match entry.rsplit_once(':') {
            Some((name, port)) => {
                let port = port
                    .parse::<u16>()
                    .ok()
                    .filter(|port| *port != 0)
                    .ok_or_else(|| bad("has a port that isn't a number from 1 to 65535"))?;
                (name, port)
            }
            None => (entry, HTTPS),
        };
        let (domain, subdomains) = match name.strip_prefix("*.") {
            Some(domain) => (domain, true),
            None => (name, false),
        };
        let domain = normalize(domain);
        if !is_host_name(&domain) {
            return Err(bad(
                "isn't a host name: use host, host:port, or *.domain, with no scheme or path",
            ));
        }
        Ok(if subdomains {
            Rule::Subdomains { domain, port }
        } else {
            Rule::Host { host: domain, port }
        })
    }

    fn allows(&self, host: &str, port: u16) -> bool {
        match self {
            Rule::Host {
                host: allowed,
                port: allowed_port,
            } => *allowed_port == port && allowed == host,
            Rule::Subdomains {
                domain,
                port: allowed_port,
            } => {
                *allowed_port == port
                    && host
                        .strip_suffix(domain.as_str())
                        .is_some_and(|label| label.len() > 1 && label.ends_with('.'))
            }
        }
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (prefix, name, port) = match self {
            Rule::Host { host, port } => ("", host, port),
            Rule::Subdomains { domain, port } => ("*.", domain, port),
        };
        if *port == HTTPS {
            write!(f, "{prefix}{name}")
        } else {
            write!(f, "{prefix}{name}:{port}")
        }
    }
}

/// The hosts one session may tunnel to. Sorted, so equal lists share a listener.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Allowlist {
    rules: Vec<Rule>,
    /// Your local endpoints, each exactly its host and port. They speak plain HTTP and may sit
    /// on this machine's loopback, which no other rule reaches.
    local: Vec<(String, u16)>,
}

impl Allowlist {
    /// `vendor`'s own hosts plus `extra`, which is `sandbox.proxy_allow`.
    pub fn for_harness(vendor: &str, extra: &[String]) -> Result<Self, String> {
        let own = harness_hosts(vendor).iter().copied();
        Self::parse(own.chain(extra.iter().map(String::as_str)))
    }

    pub fn parse<'a>(entries: impl IntoIterator<Item = &'a str>) -> Result<Self, String> {
        let mut rules = entries
            .into_iter()
            .map(Rule::parse)
            .collect::<Result<Vec<_>, _>>()?;
        rules.sort();
        rules.dedup();
        Ok(Self {
            rules,
            local: Vec::new(),
        })
    }

    /// With your local endpoints, as (host, port) from each one's `base_url`.
    pub fn with_local_endpoints(
        mut self,
        endpoints: impl IntoIterator<Item = (String, u16)>,
    ) -> Self {
        self.local.extend(
            endpoints
                .into_iter()
                .map(|(host, port)| (normalize(&host), port)),
        );
        self.local.sort();
        self.local.dedup();
        self
    }

    /// Whether a tunnel to `host:port` may open. `host` comes as the client sent it.
    pub fn allows(&self, host: &str, port: u16) -> bool {
        let host = normalize(host);
        self.rules.iter().any(|rule| rule.allows(&host, port)) || self.is_local_at(&host, port)
    }

    /// Whether `host:port` is one of your local endpoints, which plain HTTP may reach too.
    pub fn is_local(&self, host: &str, port: u16) -> bool {
        self.is_local_at(&normalize(host), port)
    }

    fn is_local_at(&self, host: &str, port: u16) -> bool {
        self.local
            .iter()
            .any(|(local, local_port)| *local_port == port && local == host)
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// Lowercase, with a trailing dot and IPv6 brackets dropped, so `API.Anthropic.com.` matches
/// `api.anthropic.com`.
fn normalize(host: &str) -> String {
    let host = host
        .strip_prefix('[')
        .and_then(|ip| ip.strip_suffix(']'))
        .unwrap_or(host);
    host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase()
}

/// Letters, digits, and hyphens in dot-separated labels. IP literals pass too, which is fine:
/// a tunnel to an IP opens only when you list that IP.
pub(super) fn is_host_name(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_host_parses() {
        for vendor in ["claude-code", "codex", "antigravity-cli", "opencode"] {
            let list = Allowlist::for_harness(vendor, &[]).unwrap();
            assert_eq!(list.len(), harness_hosts(vendor).len(), "{vendor}");
            for host in harness_hosts(vendor) {
                assert!(list.allows(host, 443), "{vendor} {host}");
            }
        }
        assert!(
            Allowlist::for_harness("someone-else", &[])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn harnesses_get_only_their_own_hosts() {
        let claude = Allowlist::for_harness("claude-code", &[]).unwrap();
        assert!(claude.allows("api.anthropic.com", 443));
        assert!(!claude.allows("api.openai.com", 443));
        assert!(!claude.allows("cloudcode-pa.googleapis.com", 443));
        let codex = Allowlist::for_harness("codex", &[]).unwrap();
        assert!(codex.allows("chatgpt.com", 443));
        assert!(!codex.allows("api.anthropic.com", 443));
    }

    #[test]
    fn rules_match_host_port_and_subdomains() {
        let extra = [
            "*.corp.example:8443".to_owned(),
            "Mirror.Example.".to_owned(),
        ];
        let list = Allowlist::for_harness("claude-code", &extra).unwrap();
        // Names compare without case or a trailing dot; the port must match too.
        assert!(list.allows("API.anthropic.com.", 443));
        assert!(!list.allows("api.anthropic.com", 80));
        assert!(list.allows("mirror.example", 443));
        // A wildcard covers subdomains at any depth, but not the domain itself or look-alikes.
        assert!(list.allows("llm.corp.example", 8443));
        assert!(list.allows("a.b.corp.example", 8443));
        assert!(!list.allows("corp.example", 8443));
        assert!(!list.allows("evilcorp.example", 8443));
        assert!(!list.allows("llm.corp.example", 443));
        // Suffix and prefix tricks don't pass.
        assert!(!list.allows("api.anthropic.com.evil.example", 443));
        assert!(!list.allows("evil-api.anthropic.com", 443));
    }

    #[test]
    fn bad_entries_are_refused_by_name() {
        for entry in [
            "",
            "*",
            "https://api.example.com",
            "api.example.com/v1",
            "api.example.com:0",
            "api.example.com:99999",
            "api.example.com:https",
            "a..b",
            "-bad.example",
            "*.*.example",
            "api example.com",
        ] {
            let err = Rule::parse(entry).unwrap_err();
            assert!(err.contains("sandbox.proxy_allow"), "{entry}: {err}");
        }
        let err = Allowlist::for_harness("codex", &["bad/".to_owned()]).unwrap_err();
        assert!(err.contains("\"bad/\""), "{err}");
    }

    #[test]
    fn rules_print_as_written() {
        for entry in ["api.example.com", "api.example.com:8443", "*.example.com"] {
            assert_eq!(Rule::parse(entry).unwrap().to_string(), entry);
        }
        assert_eq!(
            Rule::parse("API.example.com:443").unwrap().to_string(),
            "api.example.com"
        );
    }

    #[test]
    fn equal_lists_compare_equal_whatever_the_order() {
        let a = Allowlist::parse(["b.example", "a.example", "a.example"]).unwrap();
        let b = Allowlist::parse(["a.example", "b.example"]).unwrap();
        assert_eq!(a, b);
    }
}
