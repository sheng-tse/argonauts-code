//! The known endpoints, and your `[endpoints.<id>]` tables merged over them.

use super::Settings;
use super::agents::EndpointSettings;

/// (id, base URL, key variable, local) for each known endpoint.
const KNOWN_ENDPOINTS: &[(&str, &str, Option<&str>, bool)] = &[
    (
        "openrouter",
        "https://openrouter.ai/api/v1",
        Some("OPENROUTER_API_KEY"),
        false,
    ),
    ("ollama", "http://localhost:11434/v1", None, true),
    ("lmstudio", "http://127.0.0.1:1234/v1", None, true),
];

/// The known endpoints `openrouter`, `ollama`, and `lmstudio`, as settings with their base
/// URL, key variable, and whether they're local.
pub fn builtin_endpoint(id: &str) -> Option<EndpointSettings> {
    let &(_, base_url, api_key_env, local) = KNOWN_ENDPOINTS.iter().find(|known| known.0 == id)?;
    Some(EndpointSettings {
        base_url: Some(base_url.to_owned()),
        api_key_env: api_key_env.map(str::to_owned),
        local,
        ..EndpointSettings::default()
    })
}

/// `[endpoints.<id>]` as the endpoint runner needs it: your table over the known endpoint of the
/// same name, key by key. `None` when neither exists.
///
/// Settings arrive resolved, so a key still at its default counts as unset. The one exception
/// is `local`: pointing a known endpoint at another `base_url` drops the known endpoint's `local =
/// true`, because a server elsewhere isn't on your machine unless you say so.
pub fn endpoint_settings(settings: &Settings, id: &str) -> Option<EndpointSettings> {
    let known = builtin_endpoint(id);
    let Some(yours) = settings.endpoints.get(id) else {
        return known;
    };
    let Some(known) = known else {
        return Some(yours.clone());
    };
    let default = EndpointSettings::default();
    let moved = yours
        .base_url
        .as_ref()
        .is_some_and(|url| Some(url) != known.base_url.as_ref());
    Some(EndpointSettings {
        base_url: yours.base_url.clone().or(known.base_url),
        api_key_env: yours.api_key_env.clone().or(known.api_key_env),
        api: either(yours.api, known.api, default.api),
        structured_output: either(
            yours.structured_output,
            known.structured_output,
            default.structured_output,
        ),
        local: yours.local || (!moved && known.local),
        allow_api_billing: yours.allow_api_billing || known.allow_api_billing,
        timeout: either(yours.timeout.clone(), known.timeout, default.timeout),
        max_concurrency: either(
            yours.max_concurrency,
            known.max_concurrency,
            default.max_concurrency,
        ),
        header_env: if yours.header_env.is_empty() {
            known.header_env
        } else {
            yours.header_env.clone()
        },
    })
}

fn either<T: PartialEq>(yours: T, known: T, default: T) -> T {
    if yours == default { known } else { yours }
}

/// Where each local endpoint listens, as (host, port) from its `base_url`: the known local
/// endpoints unless you moved them, and every table of yours marked `local`. A session's network
/// proxy lets it reach exactly these, since they speak plain HTTP and often sit on loopback.
pub fn local_endpoint_addresses(settings: &Settings) -> Vec<(String, u16)> {
    let ids = KNOWN_ENDPOINTS
        .iter()
        .map(|known| known.0)
        .chain(settings.endpoints.keys().map(String::as_str));
    let mut addresses: Vec<(String, u16)> = ids
        .filter_map(|id| endpoint_settings(settings, id))
        .filter(|endpoint| endpoint.local)
        .filter_map(|endpoint| host_and_port(endpoint.base_url.as_deref()?))
        .collect();
    addresses.sort();
    addresses.dedup();
    addresses
}

/// `http://host:port/...` as (host, port), the port from the scheme when the URL names none.
/// IPv6 hosts lose their brackets.
pub fn host_and_port(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.trim().split_once("://")?;
    let default = match scheme.to_ascii_lowercase().as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let (host, port) = match authority.strip_prefix('[') {
        Some(bracketed) => {
            let (host, rest) = bracketed.split_once(']')?;
            (host, rest.strip_prefix(':'))
        }
        None => match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        },
    };
    let port = match port {
        Some(port) => port.parse().ok().filter(|port| *port != 0)?,
        None => default,
    };
    (!host.is_empty()).then(|| (host.to_ascii_lowercase(), port))
}

#[cfg(test)]
mod tests {
    use super::super::agents::EndpointApi;
    use super::*;

    #[test]
    fn known_endpoints_merge_under_your_table() {
        let openrouter = builtin_endpoint("openrouter").unwrap();
        assert_eq!(
            openrouter.base_url.as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
        assert_eq!(
            openrouter.api_key_env.as_deref(),
            Some("OPENROUTER_API_KEY")
        );
        assert!(!openrouter.local);
        assert!(builtin_endpoint("ollama").unwrap().local);
        assert_eq!(
            builtin_endpoint("lmstudio").unwrap().base_url.as_deref(),
            Some("http://127.0.0.1:1234/v1")
        );
        assert!(builtin_endpoint("vllm").is_none());

        let mut settings = Settings::default();
        assert!(endpoint_settings(&settings, "ollama").unwrap().local);
        assert!(endpoint_settings(&settings, "vllm").is_none());

        settings.endpoints.insert(
            "openrouter".to_owned(),
            EndpointSettings {
                api: EndpointApi::Responses,
                max_concurrency: 2,
                ..EndpointSettings::default()
            },
        );
        let merged = endpoint_settings(&settings, "openrouter").unwrap();
        assert_eq!(merged.base_url, openrouter.base_url);
        assert_eq!(merged.api_key_env, openrouter.api_key_env);
        assert_eq!(merged.api, EndpointApi::Responses);
        assert_eq!(merged.max_concurrency, 2);

        settings.endpoints.insert(
            "ollama".to_owned(),
            EndpointSettings {
                base_url: Some("http://gpu-box:11434/v1".to_owned()),
                ..EndpointSettings::default()
            },
        );
        let remote = endpoint_settings(&settings, "ollama").unwrap();
        assert_eq!(remote.base_url.as_deref(), Some("http://gpu-box:11434/v1"));
        assert!(!remote.local);

        let vllm = EndpointSettings {
            base_url: Some("http://localhost:8000/v1".to_owned()),
            local: true,
            ..EndpointSettings::default()
        };
        settings.endpoints.insert("vllm".to_owned(), vllm.clone());
        assert_eq!(endpoint_settings(&settings, "vllm"), Some(vllm));
    }

    #[test]
    fn local_endpoints_are_the_known_local_ones_and_yours() {
        let mut settings = Settings::default();
        assert_eq!(
            local_endpoint_addresses(&settings),
            [
                ("127.0.0.1".to_owned(), 1234),
                ("localhost".to_owned(), 11434)
            ]
        );

        // Moving Ollama elsewhere drops it unless you say it's local; a table of yours marked
        // local joins, and one that isn't stays out.
        settings.endpoints.insert(
            "ollama".to_owned(),
            EndpointSettings {
                base_url: Some("http://gpu-box:11434/v1".to_owned()),
                ..EndpointSettings::default()
            },
        );
        for (id, url, local) in [
            ("vllm", "http://[::1]:8000/v1", true),
            ("llamacpp", "http://Localhost:8080", true),
            ("hosted", "https://llm.example.com/v1", false),
        ] {
            settings.endpoints.insert(
                id.to_owned(),
                EndpointSettings {
                    base_url: Some(url.to_owned()),
                    local,
                    ..EndpointSettings::default()
                },
            );
        }
        assert_eq!(
            local_endpoint_addresses(&settings),
            [
                ("127.0.0.1".to_owned(), 1234),
                ("::1".to_owned(), 8000),
                ("localhost".to_owned(), 8080),
            ]
        );
    }

    #[test]
    fn urls_give_their_host_and_port() {
        for (url, expected) in [
            ("http://localhost:11434/v1", Some(("localhost", 11434))),
            ("http://gpu-box/v1", Some(("gpu-box", 80))),
            ("https://llm.example.com", Some(("llm.example.com", 443))),
            ("http://user:pw@127.0.0.1:8080/x", Some(("127.0.0.1", 8080))),
            ("http://[::1]:8000", Some(("::1", 8000))),
            ("http://[::1]", Some(("::1", 80))),
            ("ftp://host:21", None),
            ("localhost:11434", None),
            ("http://:8080", None),
            ("http://host:0", None),
        ] {
            let found = host_and_port(url);
            assert_eq!(
                found.as_ref().map(|(host, port)| (host.as_str(), *port)),
                expected,
                "{url}"
            );
        }
    }
}
