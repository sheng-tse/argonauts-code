//! The one request: the day's event to PostHog's capture address, marked so PostHog keeps no
//! profile of the sender.

use std::time::Duration;

use anyhow::Context as _;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::event::{DailyUsage, EVENT};
use super::show::Held;

/// PostHog Cloud's capture address in its US region, where reports go unless the build names
/// another host.
pub(crate) const US_CAPTURE: &str = "https://us.i.posthog.com/i/v0/e/";

/// Argonauts Code's PostHog project key. It can add events to the project and never read them,
/// so it's no secret, and every optimized build carries it: the installer's, Homebrew's, npm's,
/// `cargo install argonauts-code` and a clone built with `--release`.
const PROJECT_KEY: &str = "phc_xDqZhz6envcZv9tVL6s7j7upbCrPFWzf8HJR5uXvHDrg";

/// The capture path on a PostHog host.
const CAPTURE_PATH: &str = "/i/v0/e/";

/// A slow network gives up rather than keep a connection open; the next day tries again.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Where the event goes, and the project key it goes with.
#[derive(Debug, Clone)]
pub(crate) struct Sender {
    pub address: String,
    pub key: String,
}

impl Sender {
    /// Where this build sends, or why it never does. Only an optimized build sends, so no debug
    /// or test binary can, this crate's or any that builds it as a dependency. A fork points its
    /// reports at its own project with `ARGONAUTS_POSTHOG_KEY` and `ARGONAUTS_POSTHOG_HOST` when
    /// it builds, or turns them off for good with the key set empty.
    pub fn built_in() -> Result<Self, Held> {
        if cfg!(test) || cfg!(debug_assertions) {
            return Err(Held::DebugBuild);
        }
        Self::chosen(
            option_env!("ARGONAUTS_POSTHOG_KEY"),
            option_env!("ARGONAUTS_POSTHOG_HOST"),
        )
        .ok_or(Held::NoKey)
    }

    /// The project's key unless the build named one, and none when it named an empty one. The
    /// project's key goes only to the project's host, so a build that named a host of its own
    /// and no key sends nothing.
    pub(crate) fn chosen(key: Option<&str>, host: Option<&str>) -> Option<Self> {
        let host = host.map(str::trim).filter(|host| !host.is_empty());
        let key = match (key, host) {
            (Some(key), _) => key.trim(),
            (None, Some(_)) => return None,
            (None, None) => PROJECT_KEY,
        };
        (!key.is_empty()).then(|| Self {
            address: capture_address(host),
            key: key.to_owned(),
        })
    }

    pub async fn send(&self, usage: &DailyUsage, now: OffsetDateTime) -> anyhow::Result<()> {
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .context("making the HTTP client")?;
        client
            .post(&self.address)
            .json(&body(&self.key, usage, now))
            .send()
            .await
            .context("sending the day's event")?
            .error_for_status()
            .context("PostHog refused the day's event")?;
        Ok(())
    }
}

/// The capture address on `host`, or the US one without a host.
pub(crate) fn capture_address(host: Option<&str>) -> String {
    let Some(host) = host.map(str::trim).filter(|host| !host.is_empty()) else {
        return US_CAPTURE.to_owned();
    };
    let host = host.trim_end_matches('/');
    let host = host
        .strip_suffix(CAPTURE_PATH.trim_end_matches('/'))
        .unwrap_or(host);
    format!("{host}{CAPTURE_PATH}")
}

/// PostHog's capture body. `$process_person_profile: false` makes the event anonymous: PostHog
/// builds no person from it and bills it at its lower rate. The install id is the only id, and
/// the time is the day alone, since nothing is timed finer than a day.
pub(crate) fn body(key: &str, usage: &DailyUsage, now: OffsetDateTime) -> Value {
    let mut properties = serde_json::to_value(usage).unwrap_or_else(|_| json!({}));
    if let Value::Object(properties) = &mut properties {
        properties.remove("install");
        properties.insert("$process_person_profile".to_owned(), Value::Bool(false));
    }
    json!({
        "api_key": key,
        "event": EVENT,
        "distinct_id": usage.install,
        "timestamp": now
            .replace_time(time::Time::MIDNIGHT)
            .format(&Rfc3339)
            .unwrap_or_default(),
        "properties": properties,
    })
}
