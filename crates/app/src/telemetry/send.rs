//! The one request: the day's event to PostHog's capture address, marked so PostHog keeps no
//! profile of the sender.

use std::time::Duration;

use anyhow::Context as _;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::event::{DailyUsage, EVENT};

/// PostHog Cloud's capture address in its EU region.
pub(crate) const EU_CAPTURE: &str = "https://eu.i.posthog.com/i/v0/e/";

/// A slow network gives up rather than keep a connection open; the next day tries again.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Where the event goes, and the project key it goes with.
#[derive(Debug, Clone)]
pub(crate) struct Sender {
    pub address: String,
    pub key: String,
}

impl Sender {
    /// The project key comes from `ARGONAUTS_POSTHOG_KEY` when the program is built, and never
    /// from the repository. A build without it sends nothing. Only a release build carries it,
    /// so no test binary can send, this crate's or any that builds it as a dependency.
    pub fn built_in() -> Option<Self> {
        let key = if cfg!(test) || cfg!(debug_assertions) {
            None
        } else {
            option_env!("ARGONAUTS_POSTHOG_KEY")
        };
        key.filter(|key| !key.trim().is_empty()).map(|key| Self {
            address: EU_CAPTURE.to_owned(),
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

/// PostHog's capture body. `$process_person_profile: false` makes the event anonymous: PostHog
/// builds no person from it and bills it at its lower rate. The install id is the only id.
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
        "timestamp": now.format(&Rfc3339).unwrap_or_default(),
        "properties": properties,
    })
}
