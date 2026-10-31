//! The known endpoints, and your `[endpoints.<id>]` tables merged over them.

use super::Settings;
use super::agents::EndpointSettings;
use crate::agents::runtimes::{RUNTIMES, Requests, Runtime, runtime};

const OPENROUTER: &str = "openrouter";

/// How many requests an endpoint that isn't a local runtime takes at once, unless you say.
pub const DEFAULT_REQUESTS: u32 = 4;

/// The ids of the known endpoints, in the order they're listed: OpenRouter, then the local
/// runtimes.
pub fn known_endpoint_ids() -> impl Iterator<Item = &'static str> {
    std::iter::once(OPENROUTER).chain(RUNTIMES.iter().map(|runtime| runtime.id))
}

/// The known endpoints: OpenRouter and the local runtimes, as settings with their base URL, key
/// variable, and whether they're local.
pub fn builtin_endpoint(id: &str) -> Option<EndpointSettings> {
    if id == OPENROUTER {
        return Some(EndpointSettings {
            base_url: Some("https://openrouter.ai/api/v1".to_owned()),
            api_key_env: Some("OPENROUTER_API_KEY".to_owned()),
            ..EndpointSettings::default()
        });
    }
    let runtime = runtime(id)?;
    Some(EndpointSettings {
        base_url: Some(runtime.address().to_owned()),
        local: true,
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
        // A number you set wins even when it's the usual one.
        max_concurrency: yours.max_concurrency.or(known.max_concurrency),
        free_memory: either(yours.free_memory, known.free_memory, default.free_memory),
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

/// How many requests endpoint `id` takes at once: your `max_concurrency`, else its runtime's
/// own number, else [`DEFAULT_REQUESTS`]. `reported` is what llama-server says it has slots for,
/// once asked; until then it counts as one.
pub fn requests_at_once(id: &str, endpoint: &EndpointSettings, reported: Option<u32>) -> u32 {
    let number = endpoint.max_concurrency.unwrap_or_else(|| {
        match runtime(id).map(|runtime| runtime.requests) {
            Some(Requests::At(number)) => number,
            Some(Requests::TheServerSlots) => reported.unwrap_or(1),
            None => DEFAULT_REQUESTS,
        }
    });
    number.max(1)
}

/// Where each local endpoint listens, as (host, port) from its `base_url`: the known local
/// runtimes unless you moved them, and every table of yours marked `local`. A session's network
/// proxy lets it reach exactly these, since they speak plain HTTP and often sit on loopback.
///
/// llama-server's and vLLM's ports are common to other servers, so they count only once you
/// have a table for them.
pub fn local_endpoint_addresses(settings: &Settings) -> Vec<(String, u16)> {
    let known = RUNTIMES
        .iter()
        .filter(|runtime| !runtime.common_ports || settings.endpoints.contains_key(runtime.id))
        .map(|runtime: &Runtime| runtime.id);
    let ids = known.chain(settings.endpoints.keys().map(String::as_str));
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
        assert_eq!(
            builtin_endpoint("llamacpp").unwrap().base_url.as_deref(),
            Some("http://127.0.0.1:9931/v1")
        );
        assert!(builtin_endpoint("vllm").unwrap().local);
        assert!(builtin_endpoint("gpu-box").is_none());

        let mut settings = Settings::default();
        assert!(endpoint_settings(&settings, "ollama").unwrap().local);
        assert!(endpoint_settings(&settings, "gpu-box").is_none());

        settings.endpoints.insert(
            "openrouter".to_owned(),
            EndpointSettings {
                api: EndpointApi::Responses,
                max_concurrency: Some(2),
                ..EndpointSettings::default()
            },
        );
        let merged = endpoint_settings(&settings, "openrouter").unwrap();
        assert_eq!(merged.base_url, openrouter.base_url);
        assert_eq!(merged.api_key_env, openrouter.api_key_env);
        assert_eq!(merged.api, EndpointApi::Responses);
        assert_eq!(merged.max_concurrency, Some(2));

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

        let mine = EndpointSettings {
            base_url: Some("http://gpu-box:8000/v1".to_owned()),
            local: true,
            ..EndpointSettings::default()
        };
        settings
            .endpoints
            .insert("gpu-box".to_owned(), mine.clone());
        assert_eq!(endpoint_settings(&settings, "gpu-box"), Some(mine));
    }

    #[test]
    fn a_number_you_set_wins_over_the_servers_own() {
        let mut settings = Settings::default();
        let count = |settings: &Settings, id: &str, reported: Option<u32>| {
            let endpoint = endpoint_settings(settings, id).unwrap_or_default();
            requests_at_once(id, &endpoint, reported)
        };
        assert_eq!(count(&settings, "ollama", None), 1);
        assert_eq!(count(&settings, "lmstudio", None), 1);
        assert_eq!(count(&settings, "vllm", None), 4);
        assert_eq!(count(&settings, "llamacpp", None), 1);
        assert_eq!(count(&settings, "llamacpp", Some(3)), 3);
        assert_eq!(count(&settings, "openrouter", None), DEFAULT_REQUESTS);

        // 4 is the usual number elsewhere, and still wins over Ollama's 1 once you write it.
        for (id, number) in [("ollama", 4), ("llamacpp", 2), ("openrouter", 9)] {
            settings.endpoints.insert(
                id.to_owned(),
                EndpointSettings {
                    max_concurrency: Some(number),
                    ..EndpointSettings::default()
                },
            );
        }
        assert_eq!(count(&settings, "ollama", None), 4);
        assert_eq!(count(&settings, "llamacpp", Some(8)), 2);
        assert_eq!(count(&settings, "openrouter", None), 9);

        let zero = EndpointSettings {
            max_concurrency: Some(0),
            ..EndpointSettings::default()
        };
        assert_eq!(requests_at_once("ollama", &zero, None), 1);
    }

    #[test]
    fn freeing_memory_is_on_until_you_turn_it_off() {
        let mut settings = Settings::default();
        assert!(endpoint_settings(&settings, "ollama").unwrap().free_memory);
        settings.endpoints.insert(
            "ollama".to_owned(),
            EndpointSettings {
                free_memory: false,
                ..EndpointSettings::default()
            },
        );
        assert!(!endpoint_settings(&settings, "ollama").unwrap().free_memory);
    }

    #[test]
    fn local_endpoints_are_the_known_local_ones_and_yours() {
        // llama-server and vLLM listen on ports other servers use, so they wait for a table.
        let mut settings = Settings::default();
        assert_eq!(
            local_endpoint_addresses(&settings),
            [
                ("127.0.0.1".to_owned(), 1234),
                ("localhost".to_owned(), 11434)
            ]
        );
        settings
            .endpoints
            .insert("llamacpp".to_owned(), EndpointSettings::default());
        assert_eq!(
            local_endpoint_addresses(&settings),
            [
                ("127.0.0.1".to_owned(), 1234),
                ("127.0.0.1".to_owned(), 9931),
                ("localhost".to_owned(), 11434)
            ]
        );
        settings.endpoints.remove("llamacpp");

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
